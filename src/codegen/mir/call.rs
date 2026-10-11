//! `Call` terminators: a call to a body, and the natives (a `FnDef` with
//! no body, named `{Type}.{method}` or a bare free function), whose
//! reference semantics are the interpreter's `native()`.
//!
//! Every argument is copied into a temporary the callee owns, and the
//! result comes back through a temporary that is then copied to the
//! destination, so neither can alias a place the callee reads.

use inkwell::values::{IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::interp::split_method;
use crate::mir::ty::{IntTy, IntrinsicTy};
use crate::mir::{BasicBlock, ConstKind, Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn call(
        &mut self,
        func: &Operand,
        args: &[Operand],
        dest: &Place,
        target: Option<BasicBlock>,
    ) -> R<()> {
        let program = self.program;
        match func {
            Operand::Const(c) => {
                let ConstKind::FnDef(inst) = &c.kind else {
                    return Err("MIR to LLVM: a call to a constant that is not a function".into());
                };
                if program.bodies.contains_key(&inst.name) {
                    self.call_body(&inst.name, args, dest)?;
                } else {
                    self.native(&inst.name, args, dest)?;
                }
            }
            _ => self.call_value(func, args, dest)?,
        }
        match target {
            Some(t) => self
                .builder
                .build_unconditional_branch(self.blocks[t.index()])
                .map_err(e)?,
            None => self.builder.build_unreachable().map_err(e)?,
        };
        Ok(())
    }

    /// A temporary in the entry block that can hold a `ty`.
    pub(super) fn temp(&self, ty: Ty, name: &str) -> R<PointerValue<'ctx>> {
        let size = self.layout(ty)?.size;
        self.entry_slot(size.max(1) as u32, name)
    }

    fn call_body(&mut self, name: &str, args: &[Operand], dest: &Place) -> R<()> {
        let f = self.function(name)?;
        let dty = self.place_type(dest)?;
        let ret = self.temp(dty, "ret")?;
        let mut vals = vec![ret.into()];
        let program = self.program;
        let params = program.bodies.get(name).map(|b| &b.locals);
        for (i, a) in args.iter().enumerate() {
            let t = self.operand_type(a)?;
            let want = params.and_then(|ls| ls.get(1 + i)).map(|l| l.ty);
            let slot = match want {
                // An argument whose integers are narrower than the
                // parameter's (`(u8, u32)` for a `(i64, i64)`) is widened.
                Some(w) if w != t && self.widens(t, w) => {
                    let src = self.operand_ptr(a)?;
                    let slot = self.temp(w, "arg")?;
                    self.widen_into(slot, src, t, w)?;
                    slot
                }
                _ => {
                    let slot = self.temp(t, "arg")?;
                    self.store_operand(slot, a)?;
                    slot
                }
            };
            vals.push(slot.into());
        }
        self.builder.build_call(f, &vals, "").map_err(e)?;
        let d = self.place_ptr(dest)?;
        self.copy(d, ret, dty)
    }

    /// Whether a `from` value converts to `to` by widening its integers
    /// in place: integers, and tuples of them of the same arity.
    pub(super) fn widens(&self, from: Ty, to: Ty) -> bool {
        match (self.tys.kind(from), self.tys.kind(to)) {
            (TyKind::Int(_), TyKind::Int(_)) | (TyKind::Float(_), TyKind::Float(_)) => true,
            (TyKind::Tuple(a), TyKind::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| x == y || self.widens(*x, *y))
            }
            _ => false,
        }
    }

    /// Writes the `from` at `src` as a `to` at `dst` (see [`Self::widens`]).
    pub(super) fn widen_into(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        from: Ty,
        to: Ty,
    ) -> R<()> {
        if from == to {
            return self.copy(dst, src, to);
        }
        match (self.tys.kind(from), self.tys.kind(to)) {
            (TyKind::Int(it), TyKind::Int(_)) => {
                let signed = matches!(
                    it,
                    IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Isize
                );
                let v = self.load(src, from)?.into_int_value();
                let wt = self.llvm_ty(to)?.into_int_type();
                let w = self
                    .builder
                    .build_int_cast_sign_flag(v, wt, signed, "")
                    .map_err(e)?;
                self.store(dst, w.into())
            }
            (TyKind::Float(_), TyKind::Float(_)) => {
                let v = self.load(src, from)?;
                let f = self.float_of(v, from)?;
                let w: inkwell::values::BasicValueEnum = match self.narrow_float(to) {
                    Some(ft) => self.narrow_bits(f, ft)?.into(),
                    None => {
                        let ft = self.llvm_ty(to)?.into_float_type();
                        self.builder.build_float_cast(f, ft, "").map_err(e)?.into()
                    }
                };
                self.store(dst, w)
            }
            (TyKind::Tuple(a), TyKind::Tuple(b)) => {
                for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                    let so = self.field_offset(from, None, i as u32)?;
                    let d = self.field_offset(to, None, i as u32)?;
                    let (sp, dp) = (self.offset(src, so)?, self.offset(dst, d)?);
                    self.widen_into(dp, sp, *x, *y)?;
                }
                Ok(())
            }
            _ => self.copy(dst, src, to),
        }
    }

    /// Drops each argument the native was handed by move and did not keep.
    pub(super) fn drop_moved(&mut self, args: &[Operand]) -> R<()> {
        for a in args {
            if let Operand::Move(p) = a {
                let t = self.place_type(p)?;
                if self.tys.needs_drop(t) {
                    let q = self.place_ptr(p)?;
                    self.call_glue(q, t)?;
                }
            }
        }
        Ok(())
    }

    /// Appends each argument's display to the format buffer.
    fn fmt_args(&mut self, args: &[Operand]) -> R<()> {
        for a in args {
            let t = self.operand_type(a)?;
            let p = self.operand_ptr(a)?;
            self.fmt_call(p, t)?;
        }
        Ok(())
    }

    /// The `{ptr, len}` of a `str`, `ref str` or `String` operand.
    pub(super) fn str_parts(
        &mut self,
        a: &Operand,
    ) -> R<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>)> {
        let t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        if let TyKind::Ref(inner) | TyKind::MutRef(inner) = self.tys.kind(t) {
            if !matches!(self.tys.kind(inner), TyKind::Str) {
                p = self.load_ptr(p)?;
            }
        }
        let s = self.load_ptr(p)?;
        let n = self.load_i64(self.offset(p, 8)?)?;
        Ok((s, n))
    }

    /// The pointer a `ref T` / `mut ref T` operand holds.
    /// Fills the `Some` payload at `q` of the option type `dty` with the
    /// element at `at`: the reference itself when the payload is one, else
    /// a clone of the element, as the interpreter's `option_of_place` has
    /// it (`Array[i64, 4].get` is an `Option[i64]`).
    ///
    /// `d` is the option, already tagged `Some`; `el` is the element's
    /// type. A `weak` element read as the `shared` type it points at is
    /// upgraded, so a dead target reads as `None`.
    pub(super) fn store_found(
        &mut self,
        d: PointerValue<'ctx>,
        dty: Ty,
        some: u32,
        at: PointerValue<'ctx>,
        el: Ty,
    ) -> R<()> {
        let pt = self
            .tys
            .field_ty(dty, Some(some), 0)
            .ok_or("an option without a payload")?;
        let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
        match (self.tys.kind(pt), self.tys.kind(el)) {
            (TyKind::Ref(_) | TyKind::MutRef(_), _) => self.store(q, at.into()),
            (TyKind::Shared(_), TyKind::Weak(_)) => {
                let (_, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                let h = self.load_ptr(at)?;
                let set = self.builder.build_is_not_null(h, "").map_err(e)?;
                self.if_then(set, |cx| {
                    let n = cx.load_i64(h)?;
                    let alive = cx
                        .builder
                        .build_int_compare(IntPredicate::NE, n, cx.i64(0), "")
                        .map_err(e)?;
                    cx.if_then(alive, |cx| {
                        cx.retain(h)?;
                        cx.store_tag(d, dty, some)?;
                        cx.store(q, h.into())
                    })
                })
            }
            _ => self.clone_into(q, at, pt),
        }
    }

    pub(super) fn ref_arg(&mut self, a: &Operand) -> R<PointerValue<'ctx>> {
        let p = self.operand_ptr(a)?;
        self.load_ptr(p)
    }

    fn native(&mut self, name: &str, args: &[Operand], dest: &Place) -> R<()> {
        let (ty_name, method) = split_method(name);
        let base = ty_name.split('[').next().unwrap_or(ty_name);
        let dty = self.place_type(dest)?;
        if matches!(base, "Map" | "Set" | "SortedMap" | "SortedSet")
            && self.table_native(method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if base == "String" && self.string_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if matches!(
            base,
            "i8" | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "usize"
                | "f32"
                | "f64"
                | "char"
        ) && self.scalar_native(method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if self.num_native(base, method, args, dest)?.is_some() {
            return Ok(());
        }
        if base == "FileSystem" && self.fs_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if base == "Atomic" && self.atomic_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if matches!(base, "OnceLock" | "OnceCell")
            && self.once_native(method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if base == "Vector" && self.vector_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if base == "Interner" && self.interner_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if base == "Arena" && self.arena_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if base == "VecDeque" && self.deque_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if (base, method) == ("Vec", "from_slice") {
            let (p, n, el) = self.view_parts(&args[0])?;
            return self.vec_of_clones(dest, p, n, el);
        }
        if matches!(base, "Vec" | "Slice" | "Array")
            && self.seq_native(base, method, args, dest)?.is_some()
        {
            return Ok(());
        }
        match (base, method) {
            ("println" | "print" | "eprintln" | "eprint", "") => {
                self.rt("fmt_begin", &[])?;
                self.fmt_args(args)?;
                let flags =
                    u64::from(base.ends_with("ln")) | (u64::from(base.starts_with('e')) << 1);
                let f = self.llcx.i32_type().const_int(flags, false);
                self.rt("print_end", &[f.into()])?;
                self.drop_moved(args)
            }
            ("format", "") | (_, "to_string") if method != "to_string" || args.len() == 1 => {
                self.rt("fmt_begin", &[])?;
                self.fmt_args(args)?;
                let d = self.place_ptr(dest)?;
                self.rt("fmt_end_string", &[d.into()])?;
                self.drop_moved(args)
            }
            ("format_spec", "") => self.format_spec(args, dest),
            ("dbg", "") => self.dbg(args),
            ("panic_message", "") => {
                self.rt("fmt_begin", &[])?;
                self.fmt_args(args)?;
                self.rt("panic_set", &[])?;
                self.drop_moved(args)
            }
            ("process", "exit") => {
                let code = self.operand_scalar(&args[0])?.into_int_value();
                let code = self
                    .builder
                    .build_int_truncate_or_bit_cast(code, self.llcx.i32_type(), "")
                    .map_err(e)?;
                self.rt("exit", &[code.into()])
            }
            ("String", "from") => {
                let (s, n) = self.str_parts(&args[0])?;
                let d = self.place_ptr(dest)?;
                // A static `StringSlice` re-reads its literal as a view.
                if matches!(self.tys.kind(dty), TyKind::Str) {
                    self.store(d, s.into())?;
                    return self.store(self.offset(d, 8)?, n.into());
                }
                self.rt("string_from", &[d.into(), s.into(), n.into()])?;
                self.drop_moved(args)
            }
            ("String" | "Vec", "new") => {
                let d = self.place_ptr(dest)?;
                self.builder
                    .build_memset(d, 1, self.llcx.i8_type().const_zero(), self.i64(24))
                    .map_err(e)?;
                Ok(())
            }
            ("String" | "Vec", "len") => {
                let (_, n) = self.view_of(&args[0])?;
                let d = self.place_ptr(dest)?;
                let it = self.llvm_ty(dty)?.into_int_type();
                let n = self
                    .builder
                    .build_int_truncate_or_bit_cast(n, it, "")
                    .map_err(e)?;
                self.store(d, n.into())
            }
            ("String" | "Vec", "is_empty") => {
                let (_, n) = self.view_of(&args[0])?;
                let z = self.is_zero(n)?;
                let z = self
                    .builder
                    .build_int_z_extend(z, self.llcx.i8_type(), "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, z.into())
            }
            ("Vec", "push") => {
                let v = self.ref_arg(&args[0])?;
                let et = self.vec_elem(name, &args[0])?;
                let size = self.layout(et)?.size;
                let x = self.operand_ptr(&args[1])?;
                self.rt("vec_push", &[v.into(), x.into(), self.i64(size).into()])
            }
            ("String", "push_str") => {
                let v = self.ref_arg(&args[0])?;
                let (s, n) = self.str_parts(&args[1])?;
                self.rt("string_push_str", &[v.into(), s.into(), n.into()])?;
                self.drop_moved(&args[1..])
            }
            ("String", "push") => {
                let v = self.ref_arg(&args[0])?;
                let c = self.operand_scalar(&args[1])?;
                self.rt("string_push_char", &[v.into(), c.into()])
            }
            ("String", "add") => {
                // Takes the left, borrows the right.
                let d = self.place_ptr(dest)?;
                let lt = self.operand_type(&args[0])?;
                match (&args[0], self.tys.kind(lt)) {
                    (Operand::Move(p), TyKind::Intrinsic(IntrinsicTy::String)) => {
                        let src = self.place_ptr(p)?;
                        self.copy(d, src, lt)?;
                    }
                    _ => {
                        let (s, n) = self.str_parts(&args[0])?;
                        self.rt("string_from", &[d.into(), s.into(), n.into()])?;
                    }
                }
                let (s, n) = self.str_parts(&args[1])?;
                self.rt("string_push_str", &[d.into(), s.into(), n.into()])?;
                self.drop_moved(&args[1..])
            }
            ("String", "eq") => {
                let (a, an) = self.str_parts(&args[0])?;
                let (b, bn) = self.str_parts(&args[1])?;
                let r = self.rt_u8("bytes_eq", &[a.into(), an.into(), b.into(), bn.into()])?;
                let d = self.place_ptr(dest)?;
                self.store(d, r.into())?;
                self.drop_moved(args)
            }
            ("String", "with_capacity") => {
                let d = self.place_ptr(dest)?;
                let n = self.layout(dty)?.size;
                self.builder
                    .build_memset(d, 1, self.llcx.i8_type().const_zero(), self.i64(n))
                    .map_err(e)?;
                Ok(())
            }
            ("Vec", "with_capacity") => {
                let n = self.int_arg(&args[0])?;
                let ok = self.non_negative(n)?;
                self.abort_unless(ok, "explicit panic")?;
                let d = self.place_ptr(dest)?;
                self.builder
                    .build_memset(d, 1, self.llcx.i8_type().const_zero(), self.i64(24))
                    .map_err(e)?;
                Ok(())
            }
            ("Vec", "from_array") => {
                let at = self.operand_type(&args[0])?;
                let TyKind::Array(el, n) = self.tys.kind(at) else {
                    return Err(format!("{name} of a `{}`", self.tys.display(at)));
                };
                let size = self.layout(el)?.size;
                let src = self.operand_ptr(&args[0])?;
                let d = self.place_ptr(dest)?;
                self.rt(
                    "vec_alloc",
                    &[d.into(), self.i64(n).into(), self.i64(size).into()],
                )?;
                if n * size != 0 {
                    let buf = self.load_ptr(d)?;
                    self.builder
                        .build_memmove(buf, 1, src, 1, self.i64(n * size))
                        .map_err(e)?;
                }
                Ok(())
            }
            ("Vec", "filled") => {
                let TyKind::Intrinsic(IntrinsicTy::Vec(el)) = self.tys.kind(dty) else {
                    return Err(format!("{name} into a `{}`", self.tys.display(dty)));
                };
                let size = self.layout(el)?.size;
                let n = self.int_arg(&args[0])?;
                let ok = self.non_negative(n)?;
                self.abort_unless(ok, "explicit panic")?;
                let val = self.operand_ptr(&args[1])?;
                let d = self.place_ptr(dest)?;
                self.rt("vec_alloc", &[d.into(), n.into(), self.i64(size).into()])?;
                let buf = self.load_ptr(d)?;
                let empty = self.is_zero(n)?;
                let f = self.cur_fn()?;
                let (none, some, done) = (
                    self.llcx.append_basic_block(f, "filled.none"),
                    self.llcx.append_basic_block(f, "filled.some"),
                    self.llcx.append_basic_block(f, "filled"),
                );
                self.builder
                    .build_conditional_branch(empty, none, some)
                    .map_err(e)?;
                self.builder.position_at_end(none);
                self.drop_moved(&args[1..])?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(some);
                // Clones into the first `n - 1`, and moves into the last.
                let last = self.builder.build_int_sub(n, self.i64(1), "").map_err(e)?;
                self.count_loop(last, |cx, i| {
                    let at = cx.elem_ptr(buf, i, size)?;
                    cx.clone_into(at, val, el)
                })?;
                let at = self.elem_ptr(buf, last, size)?;
                self.copy(at, val, el)?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
                Ok(())
            }
            ("Vec", "index" | "index_mut") => {
                let v = self.ref_arg(&args[0])?;
                let el = self.vec_elem(name, &args[0])?;
                let size = self.layout(el)?.size;
                let i = self.int_arg(&args[1])?;
                let len = self.load_i64(self.offset(v, 8)?)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, i, len, "")
                    .map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                let buf = self.load_ptr(v)?;
                let at = self.elem_ptr(buf, i, size)?;
                let d = self.place_ptr(dest)?;
                self.store(d, at.into())
            }
            ("Vec", "get") => {
                let v = self.ref_arg(&args[0])?;
                let el = self.vec_elem(name, &args[0])?;
                let size = self.layout(el)?.size;
                let i = self.int_arg(&args[1])?;
                let len = self.load_i64(self.offset(v, 8)?)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, i, len, "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                let (some, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                self.if_then(ok, |cx| {
                    let buf = cx.load_ptr(v)?;
                    let at = cx.elem_ptr(buf, i, size)?;
                    cx.store_tag(d, dty, some)?;
                    cx.store_found(d, dty, some, at, el)
                })
            }
            ("Vec", "pop") => {
                let v = self.ref_arg(&args[0])?;
                let el = self.vec_elem(name, &args[0])?;
                let size = self.layout(el)?.size;
                let lp = self.offset(v, 8)?;
                let len = self.load_i64(lp)?;
                let d = self.place_ptr(dest)?;
                let (some, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                let empty = self.is_zero(len)?;
                let full = self.builder.build_not(empty, "").map_err(e)?;
                self.if_then(full, |cx| {
                    let n = cx.builder.build_int_sub(len, cx.i64(1), "").map_err(e)?;
                    cx.store(lp, n.into())?;
                    let buf = cx.load_ptr(v)?;
                    let at = cx.elem_ptr(buf, n, size)?;
                    cx.store_tag(d, dty, some)?;
                    let q = cx.offset(d, cx.field_offset(dty, Some(some), 0)?)?;
                    cx.copy(q, at, el)
                })
            }
            (_, "as_slice" | "as_mut_slice") if args.len() == 1 => {
                let (p, n) = self.view_of(&args[0])?;
                let d = self.place_ptr(dest)?;
                self.store(d, p.into())?;
                self.store(self.offset(d, 8)?, n.into())
            }
            ("Env", "args") => {
                let d = self.place_ptr(dest)?;
                self.rt(
                    "vec_alloc",
                    &[d.into(), self.i64(1).into(), self.i64(24).into()],
                )?;
                let buf = self.load_ptr(d)?;
                let (s, n) = self.str_const("main")?;
                self.rt("string_from", &[buf.into(), s.into(), n.into()])
            }
            (_, "clone") if args.len() == 1 => {
                let t = self.operand_type(&args[0])?;
                let (src, t) = match self.tys.kind(t) {
                    TyKind::Ref(i) | TyKind::MutRef(i) => (self.ref_arg(&args[0])?, i),
                    _ => (self.operand_ptr(&args[0])?, t),
                };
                let d = self.place_ptr(dest)?;
                self.clone_into(d, src, t)
            }
            _ => Err(format!("MIR to LLVM: native `{name}` is not lowered yet")),
        }
    }

    /// An integer operand, widened (or narrowed) to `i64` as a signed value.
    pub(super) fn int_arg(&mut self, a: &Operand) -> R<IntValue<'ctx>> {
        let v = self.operand_scalar(a)?.into_int_value();
        let w = v.get_type().get_bit_width();
        let t = self.llcx.i64_type();
        Ok(match w.cmp(&64) {
            std::cmp::Ordering::Less => self.builder.build_int_s_extend(v, t, "").map_err(e)?,
            std::cmp::Ordering::Greater => self.builder.build_int_truncate(v, t, "").map_err(e)?,
            std::cmp::Ordering::Equal => v,
        })
    }

    pub(super) fn non_negative(&self, n: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        self.builder
            .build_int_compare(IntPredicate::SGE, n, self.i64(0), "")
            .map_err(e)
    }

    /// `if !ok { abort(msg) }`, leaving the builder after it.
    pub(super) fn abort_unless(&mut self, ok: IntValue<'ctx>, msg: &str) -> R<()> {
        let f = self.cur_fn()?;
        let bad = self.llcx.append_basic_block(f, "abort");
        let good = self.llcx.append_basic_block(f, "ok");
        self.builder
            .build_conditional_branch(ok, good, bad)
            .map_err(e)?;
        self.builder.position_at_end(bad);
        self.abort_with(msg)?;
        self.builder.position_at_end(good);
        Ok(())
    }

    /// The variant indices of `Some` and `None` in the `Option` type `ty`.
    pub(super) fn option_variants(&self, ty: Ty) -> R<(u32, u32)> {
        let (adt, _) = self
            .tys
            .tcx()
            .adt_of(ty)
            .ok_or_else(|| format!("`{}` is not an Option", self.tys.display(ty)))?;
        let some = adt
            .variants
            .iter()
            .position(|v| v.name == "Some")
            .ok_or("an Option has no Some")? as u32;
        Ok((some, 1 - some))
    }

    /// The `{ptr, len}` of an array, `Vec` or slice operand, through a
    /// reference.
    pub(super) fn view_of(&mut self, a: &Operand) -> R<(PointerValue<'ctx>, IntValue<'ctx>)> {
        let mut t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            if matches!(self.tys.kind(i), TyKind::Str | TyKind::Slice(_)) {
                t = i;
                break;
            }
            p = self.load_ptr(p)?;
            t = i;
        }
        match self.tys.kind(t) {
            TyKind::Array(_, n) => Ok((p, self.i64(n))),
            TyKind::Intrinsic(IntrinsicTy::Vec(_) | IntrinsicTy::String)
            | TyKind::Str
            | TyKind::Slice(_) => Ok((self.load_ptr(p)?, self.load_i64(self.offset(p, 8)?)?)),
            _ => Err(format!("a view of a `{}`", self.tys.display(t))),
        }
    }

    /// The element type of the `Vec` a `ref Vec[T]` operand points at.
    fn vec_elem(&self, name: &str, a: &Operand) -> R<Ty> {
        let t = self.operand_type(a)?;
        let inner = match self.tys.kind(t) {
            TyKind::Ref(i) | TyKind::MutRef(i) => i,
            _ => t,
        };
        match self.tys.kind(inner) {
            TyKind::Intrinsic(IntrinsicTy::Vec(el)) => Ok(el),
            _ => Err(format!("{name}: not a Vec receiver")),
        }
    }
}
