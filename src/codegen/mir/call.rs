//! `Call` terminators: a call to a body, and the natives (a `FnDef` with
//! no body, named `{Type}.{method}` or a bare free function), whose
//! reference semantics are the interpreter's `native()`.
//!
//! Every argument is copied into a temporary the callee owns, and the
//! result comes back through a temporary that is then copied to the
//! destination, so neither can alias a place the callee reads.

use inkwell::values::{BasicMetadataValueEnum, IntValue, PointerValue};
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
        let f = self.direct_function(name)?;
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
            (TyKind::Adt(_), TyKind::Adt(_)) => self.unit_instance(from, to),
            _ => false,
        }
    }

    /// Where the operand `a` is as a `want`: in place, or widened into a
    /// temporary when its type is narrower (see [`Self::widens`]).
    pub(super) fn operand_as(&mut self, a: &Operand, want: Ty) -> R<PointerValue<'ctx>> {
        let t = self.operand_type(a)?;
        let src = self.operand_ptr(a)?;
        if t == want || !self.widens(t, want) {
            return Ok(src);
        }
        let slot = self.temp(want, "wide")?;
        self.widen_into(slot, src, t, want)?;
        Ok(slot)
    }

    /// Whether `from` is the enum `to` with some type arguments left at
    /// `()` (or `!`), as the builder types a payload-less variant built
    /// with nothing to infer its arguments from (`v.push(Ho.Empty)` gives
    /// `Ho[()]` for a `Vec[Ho[String]]`). Its tag sits where `to`'s does
    /// and its payloads hold nothing, so it widens by zero-filling.
    fn unit_instance(&self, from: Ty, to: Ty) -> bool {
        let tcx = self.tys.tcx();
        let (Some((fa, fargs)), Some((ta, targs))) = (tcx.adt_of(from), tcx.adt_of(to)) else {
            return false;
        };
        fa.def == ta.def
            && fa.is_enum
            && fargs.len() == targs.len()
            && fargs
                .iter()
                .zip(targs.iter())
                .all(|(f, t)| f == t || matches!(self.tys.kind(*f), TyKind::Unit | TyKind::Never))
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
            (TyKind::Adt(_), TyKind::Adt(_)) => {
                let n = self.layout(to)?.size;
                self.builder
                    .build_memset(dst, 1, self.llcx.i8_type().const_zero(), self.i64(n))
                    .map_err(e)?;
                self.copy(dst, src, from)
            }
            _ => self.copy(dst, src, to),
        }
    }

    /// Writes a reference to the `el` at `at` into `d`: its address, or
    /// for a `str` or slice the two words a reference to one is, or for an
    /// `el` that is a reference, that reference.
    pub(super) fn store_ref(
        &mut self,
        d: PointerValue<'ctx>,
        at: PointerValue<'ctx>,
        el: Ty,
    ) -> R<()> {
        // An element that is itself a reference is the result (a borrow of
        // a borrow is the borrow), as is a view, which is its own header.
        if matches!(
            self.tys.kind(el),
            TyKind::Str | TyKind::Slice(_) | TyKind::Ref(_) | TyKind::MutRef(_)
        ) {
            self.copy(d, at, el)
        } else {
            self.store(d, at.into())
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

    /// `format` without the runtime's format buffer, when its pieces
    /// allow: text pieces alone (literal text and `str` or `String` holes)
    /// as one exact allocation and a copy of each piece, and one integer
    /// as its digits. False, with nothing emitted, for any other pieces.
    fn format_direct(&mut self, args: &[Operand], dest: &Place) -> R<bool> {
        let text = |cx: &Self, t: Ty| {
            matches!(
                cx.tys.kind(t),
                TyKind::Str | TyKind::Intrinsic(IntrinsicTy::String)
            )
        };
        let mut all_text = true;
        for a in args {
            let t = self.operand_type(a)?;
            all_text &= match self.tys.kind(t) {
                TyKind::Ref(i) | TyKind::MutRef(i) => text(self, i),
                _ => text(self, t),
            };
        }
        if !all_text {
            let [a] = args else { return Ok(false) };
            let t = self.operand_type(a)?;
            let TyKind::Int(i) = self.tys.kind(t) else {
                return Ok(false);
            };
            if matches!(i, IntTy::I128 | IntTy::U128) {
                return Ok(false);
            }
            let v = self.operand_scalar(a)?.into_int_value();
            let w = self
                .builder
                .build_int_cast_sign_flag(v, self.llcx.i64_type(), i.signed(), "")
                .map_err(e)?;
            let d = self.place_ptr(dest)?;
            let f = if i.signed() {
                "string_of_i64"
            } else {
                "string_of_u64"
            };
            self.rt(f, &[d.into(), w.into()])?;
            return Ok(true);
        }
        let mut parts = Vec::with_capacity(args.len());
        let mut total = self.i64(0);
        for a in args {
            let (p, n) = self.str_parts(a)?;
            total = self.builder.build_int_add(total, n, "").map_err(e)?;
            parts.push((p, n));
        }
        // At least one byte, so the capacity is never 0 and the block is
        // freed with the `String`.
        let one = self.i64(1);
        let big = self
            .builder
            .build_int_compare(IntPredicate::UGT, total, one, "")
            .map_err(e)?;
        let room = self
            .builder
            .build_select(big, total, one, "")
            .map_err(e)?
            .into_int_value();
        let malloc = self.libc_fn("malloc");
        let buf = self
            .builder
            .build_call(malloc, &[room.into()], "text")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let mut at = buf;
        for (p, n) in parts {
            self.builder.build_memcpy(at, 1, p, 1, n).map_err(e)?;
            at = self.elem_ptr(at, n, 1)?;
        }
        let d = self.place_ptr(dest)?;
        self.store(d, buf.into())?;
        self.store(self.offset(d, 8)?, total.into())?;
        self.store(self.offset(d, 16)?, room.into())?;
        Ok(true)
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
            (TyKind::Ref(_) | TyKind::MutRef(_), _) => self.store_ref(q, at, el),
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

    /// The `shared` handle an operand holds, or holds a reference to.
    fn handle_arg(&mut self, a: &Operand) -> R<PointerValue<'ctx>> {
        let mut t = self.operand_type(a)?;
        let p = self.operand_ptr(a)?;
        let mut v = self.load_ptr(p)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            v = self.load_ptr(v)?;
            t = i;
        }
        Ok(v)
    }

    fn native(&mut self, name: &str, args: &[Operand], dest: &Place) -> R<()> {
        let (ty_name, method) = split_method(name);
        let base = ty_name.split('[').next().unwrap_or(ty_name);
        let dty = self.place_type(dest)?;
        // `a.ref_eq(b)` on a `shared` type, or the free `ref_eq(a, b)`: the
        // two handles are one pointer (B-2026-10-13-17).
        if (method == "ref_eq" && ty_name.starts_with("shared ")) || name == "ref_eq" {
            let [a, b] = args else {
                return Err(format!("MIR to LLVM: `{name}` takes two handles"));
            };
            let i64t = self.llcx.i64_type();
            let (a, b) = (self.handle_arg(a)?, self.handle_arg(b)?);
            let bld = &self.builder;
            let a = bld.build_ptr_to_int(a, i64t, "").map_err(e)?;
            let b = bld.build_ptr_to_int(b, i64t, "").map_err(e)?;
            let same = bld
                .build_int_compare(IntPredicate::EQ, a, b, "")
                .map_err(e)?;
            self.store_bool(dest, same)?;
            return self.drop_moved(args);
        }
        if matches!(base, "Map" | "Set" | "SortedMap" | "SortedSet")
            && self.table_native(method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if (matches!(base, "CStr" | "CString") || (base, method) == ("String", "to_cstring"))
            && self.cstr_native(base, method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if matches!(base, "Semaphore" | "RateLimiter")
            && self.limit_native(base, method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if base == "Pool" && self.pool_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if matches!(base, "TaskGroup" | "TaskHandle")
            && self.task_native(base, method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if matches!(base, "Channel" | "Sender" | "Receiver" | "BoundedChannel")
            && self.chan_native(base, method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if base == "Entry" && self.entry_native(method, args, dest)?.is_some() {
            return Ok(());
        }
        if matches!(base, "String" | "str") && self.string_native(method, args, dest)?.is_some() {
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
        if self.io_native(base, method, args, dest)?.is_some() {
            return Ok(());
        }
        if (base == "Atomic"
            || (base == "Mutex" && matches!(method, "new" | "get_mut" | "into_inner")))
            && self.atomic_native(method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if matches!(base, "OnceLock" | "OnceCell")
            && self.once_native(method, args, dest)?.is_some()
        {
            return Ok(());
        }
        if matches!(
            base,
            "tracing_active_span"
                | "tracing_level_enabled"
                | "tracing_set_min_level"
                | "tracing_reset"
                | "tracing_emit_event"
                | "with_span"
                | "Log"
        ) && self.tracing_native(base, method, args, dest)?.is_some()
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
        if matches!(
            base,
            "Vec" | "VecDeque" | "String" | "Map" | "SortedMap" | "Set" | "SortedSet"
        ) && method.starts_with("try_")
            && !matches!(method, "try_into" | "try_from")
        {
            // The fallible companions: the allocator aborts rather than
            // fails, so each is its base method, in `Ok`.
            let ok = self.variant_of(dty, "Ok")?;
            let pt = self
                .tys
                .field_ty(dty, Some(ok), 0)
                .ok_or("a try_ result without an Ok payload")?;
            let payload = dest
                .project(crate::mir::ProjElem::Downcast(crate::mir::VariantIdx(ok)))
                .field(0, pt);
            self.native(&name.replacen(".try_", ".", 1), args, &payload)?;
            let d = self.place_ptr(dest)?;
            return self.store_tag(d, dty, ok);
        }
        if (base, method) == ("Vec", "from_fn") && args.len() == 2 {
            return self.vec_from_fn(args, dest);
        }
        if (base, method) == ("Vec", "from_slice") {
            // The source may come moved (a `Vec` handed over whole): its
            // elements are cloned, then it drops.
            let (p, n, el) = self.view_parts(&args[0])?;
            self.vec_of_clones(dest, p, n, el)?;
            return self.drop_moved(args);
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
                if base == "format" && self.format_direct(args, dest)? {
                    return self.drop_moved(args);
                }
                self.rt("fmt_begin", &[])?;
                self.fmt_args(args)?;
                let d = self.place_ptr(dest)?;
                self.rt("fmt_end_string", &[d.into()])?;
                self.drop_moved(args)
            }
            ("format_spec", "") => self.format_spec(args, dest),
            ("dbg", "") => self.dbg(args),
            // One thread runs the program; a fence orders nothing.
            ("fence" | "compiler_fence", "") => Ok(()),
            ("__ready", "") => self.ready_native(args, dest),
            ("sleep_ms", "") if args.len() == 1 => {
                let ms = self.int_arg(&args[0])?;
                self.rt("sleep_ms", &[ms.into()])
            }
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
            ("F64" | "F32" | "F16" | "Bf16", "from") if args.len() == 1 => {
                // The total-order wrapper around its one float field.
                // A literal argument can reach it at another float width
                // (`F32.from(2.5)` passes an `f64`), so it is cast.
                let d = self.place_ptr(dest)?;
                let q = self.offset(d, self.field_offset(dty, None, 0)?)?;
                let ft = self
                    .tys
                    .field_ty(dty, None, 0)
                    .ok_or("a float wrapper without its field")?;
                let at = self.operand_type(&args[0])?;
                if at != ft
                    && matches!(self.tys.kind(at), TyKind::Float(_))
                    && matches!(self.tys.kind(ft), TyKind::Float(_))
                {
                    let v = self.operand_scalar(&args[0])?.into_float_value();
                    let want = self.llvm_ty(ft)?.into_float_type();
                    let v = self.builder.build_float_cast(v, want, "").map_err(e)?;
                    return self.store(q, v.into());
                }
                self.store_operand(q, &args[0])
            }
            ("Cpu", "supports") if args.len() == 1 => {
                let (p, n) = self.str_parts(&args[0])?;
                let i32t = self.llcx.i32_type();
                let f = self.extern_fn(
                    "karac_cpu_supports",
                    i32t.fn_type(&[self.ptr_ty().into(), self.llcx.i64_type().into()], false),
                );
                let v = self
                    .builder
                    .build_call(f, &[p.into(), n.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let yes = self
                    .builder
                    .build_int_compare(IntPredicate::NE, v, i32t.const_zero(), "")
                    .map_err(e)?;
                self.store_bool(dest, yes)?;
                self.drop_moved(args)
            }
            ("Env", "var") if args.len() == 1 => {
                // `Ok(value)`, or `Err(NotPresent)` for an unset variable.
                let (p, n) = self.str_parts(&args[0])?;
                let d = self.place_ptr(dest)?;
                let ok = self.variant_of(dty, "Ok")?;
                let q = self.offset(d, self.field_offset(dty, Some(ok), 0)?)?;
                let found = self.rt_flag("env_var", &[q.into(), p.into(), n.into()])?;
                self.store_result_flag(d, dty, found, Some("NotPresent"))?;
                self.drop_moved(args)
            }
            ("Env", "set") if args.len() == 2 => {
                let (p, n) = self.str_parts(&args[0])?;
                let (v, vn) = self.str_parts(&args[1])?;
                self.rt("env_set", &[p.into(), n.into(), v.into(), vn.into()])?;
                self.drop_moved(args)
            }
            ("f64" | "f32", "total_cmp") if args.len() == 2 => {
                // `Less`, `Equal` or `Greater` by the IEEE total order.
                let f64t = self.llcx.f64_type();
                let mut x = Vec::with_capacity(2);
                for a in args {
                    let mut t = self.operand_type(a)?;
                    let mut p = self.operand_ptr(a)?;
                    while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
                        p = self.load_ptr(p)?;
                        t = i;
                    }
                    let v = self.load(p, t)?.into_float_value();
                    x.push(if v.get_type() == f64t {
                        v
                    } else {
                        self.builder.build_float_ext(v, f64t, "").map_err(e)?
                    });
                }
                let narrow = self
                    .llcx
                    .i8_type()
                    .const_int(u64::from(base == "f32"), false);
                let i32t = self.llcx.i32_type();
                let f = self.extern_fn(
                    "karac_mir_total_cmp",
                    i32t.fn_type(
                        &[f64t.into(), f64t.into(), self.llcx.i8_type().into()],
                        false,
                    ),
                );
                let o = self
                    .builder
                    .build_call(f, &[x[0].into(), x[1].into(), narrow.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let d = self.place_ptr(dest)?;
                let tag_of = |name: &str| self.variant_of(dty, name);
                let (lt, eq, gt) = (tag_of("Less")?, tag_of("Equal")?, tag_of("Greater")?);
                let tag = |v: u32| i32t.const_int(u64::from(v), false);
                let is_lt = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, o, i32t.const_zero(), "")
                    .map_err(e)?;
                let is_gt = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, o, i32t.const_zero(), "")
                    .map_err(e)?;
                let k = self
                    .builder
                    .build_select(is_gt, tag(gt), tag(eq), "")
                    .map_err(e)?;
                let k = self
                    .builder
                    .build_select(is_lt, tag(lt).into(), k, "")
                    .map_err(e)?
                    .into_int_value();
                self.store_tag_dyn(d, dty, k)
            }
            ("Stdout" | "Stderr", "print" | "println" | "flush") => {
                if let [s] = args {
                    self.rt("fmt_begin", &[])?;
                    self.fmt_args(std::slice::from_ref(s))?;
                    let flags = u64::from(method == "println") | (u64::from(base == "Stderr") << 1);
                    let f = self.llcx.i32_type().const_int(flags, false);
                    self.rt("print_end", &[f.into()])?;
                }
                self.drop_moved(args)
            }
            ("Stdin", "read_line" | "read_to_string") => {
                // `Ok(text)`; an I/O error is an `Err` whose payload is
                // zeroed, not the interpreter's `IoError` for the error.
                let d = self.place_ptr(dest)?;
                let ok = self.variant_of(dty, "Ok")?;
                let q = self.offset(d, self.field_offset(dty, Some(ok), 0)?)?;
                let op = self
                    .llcx
                    .i32_type()
                    .const_int(u64::from(method == "read_to_string"), false);
                let found = self.rt_flag("stdin_read", &[q.into(), op.into()])?;
                self.store_result_flag(d, dty, found, None)?;
                self.drop_moved(args)
            }
            ("RandomSource", "next_u64") => {
                let v = self.mrt_i64("rand_next", &[])?;
                self.store_int_as(dest, v, false)?;
                self.drop_moved(args)
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
            ("String" | "Vec" | "str", "len") => {
                let (_, n) = self.view_of(&args[0])?;
                let d = self.place_ptr(dest)?;
                let it = self.llvm_ty(dty)?.into_int_type();
                let n = self
                    .builder
                    .build_int_truncate_or_bit_cast(n, it, "")
                    .map_err(e)?;
                self.store(d, n.into())
            }
            ("String" | "Vec" | "str", "is_empty") => {
                let (_, n) = self.view_of(&args[0])?;
                let z = self.is_zero(n)?;
                let z = self
                    .builder
                    .build_int_z_extend(z, self.llcx.i8_type(), "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, z.into())
            }
            // A counting loop's pushes, reserved before it
            // (`mir::presize`): `bound + delta` elements, clamped.
            ("Vec", "presize") => {
                let v = self.ref_arg(&args[0])?;
                let et = self.vec_elem(name, &args[0])?;
                let size = self.layout(et)?.size;
                if size == 0 {
                    return Ok(());
                }
                let b = self.int_arg(&args[1])?;
                let d = self.int_arg(&args[2])?;
                let most = self.i64(crate::mir::presize::room(i128::MAX, 0, size) as u64);
                // `bound` is clamped first, so adding `delta` (small) cannot
                // overflow.
                let b = self.smin(b, most)?;
                let b = self.smax(b, self.i64((-(1i64 << 40)) as u64))?;
                let n = self.builder.build_int_add(b, d, "").map_err(e)?;
                let n = self.smin(n, most)?;
                let pos = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, n, self.i64(0), "")
                    .map_err(e)?;
                // The reserve leaves the length as it was; storing it back
                // says so to LLVM, which otherwise loses what it knew of
                // the length (a fill to a constant count) at the call.
                let lenp = self.offset(v, 8)?;
                let len = self.load_i64(lenp)?;
                self.if_then(pos, |cx| cx.reserve_via_copy(v, n, cx.i64(size)))?;
                self.store(lenp, len.into())
            }
            ("Vec", "push") => {
                let v = self.ref_arg(&args[0])?;
                let et = self.vec_elem(name, &args[0])?;
                let x = self.operand_as(&args[1], et)?;
                self.vec_push(v, x, et)
            }
            ("String", "push_str") => {
                let v = self.ref_arg(&args[0])?;
                let (s, n) = self.str_parts(&args[1])?;
                self.rt("string_push_str", &[v.into(), s.into(), n.into()])?;
                self.drop_moved(&args[1..])
            }
            ("String", "push") => {
                // An ASCII char in line, as a byte push; any other through
                // the runtime.
                let v = self.ref_arg(&args[0])?;
                let c = self.operand_scalar(&args[1])?.into_int_value();
                let ascii = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, c, c.get_type().const_int(128, false), "")
                    .map_err(e)?;
                let f = self.cur_fn()?;
                let byte = self.llcx.append_basic_block(f, "push.ascii");
                let wide = self.llcx.append_basic_block(f, "push.wide");
                let done = self.llcx.append_basic_block(f, "push.done");
                self.builder
                    .build_conditional_branch(ascii, byte, wide)
                    .map_err(e)?;
                self.builder.position_at_end(byte);
                let b = self
                    .builder
                    .build_int_truncate(c, self.llcx.i8_type(), "")
                    .map_err(e)?;
                let lenp = self.offset(v, 8)?;
                let len = self.load_i64(lenp)?;
                let cap = self.load_i64(self.offset(v, 16)?)?;
                let full = self
                    .builder
                    .build_int_compare(IntPredicate::UGE, len, cap, "full")
                    .map_err(e)?;
                self.if_then(full, |cx| cx.grow_by_one(v, len, cap, 1))?;
                let buf = self.load_ptr(v)?;
                let at = self.elem_ptr(buf, len, 1)?;
                self.store(at, b.into())?;
                let len = self
                    .builder
                    .build_int_add(len, self.i64(1), "")
                    .map_err(e)?;
                self.store(lenp, len.into())?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(wide);
                self.rt("string_push_char", &[v.into(), c.into()])?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
                Ok(())
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
            // Exactly `n` slots up front, and no elements (B-2026-10-14-14).
            ("String" | "Vec", "with_capacity") => {
                let n = self.int_arg(&args[0])?;
                let ok = self.non_negative(n)?;
                self.abort_unless(ok, "explicit panic")?;
                let size = match self.tys.kind(dty) {
                    TyKind::Intrinsic(IntrinsicTy::Vec(el)) => self.layout(el)?.size,
                    _ => 1,
                };
                let d = self.place_ptr(dest)?;
                self.vec_alloc(d, n, size)?;
                self.store(self.offset(d, 8)?, self.i64(0).into())
            }
            ("String", "clear") => {
                // Bytes need no drop; a literal's borrowed bytes are left
                // where they are, and the next push copies nothing.
                let v = self.ref_arg(&args[0])?;
                self.store(self.offset(v, 8)?, self.i64(0).into())
            }
            // B-2026-10-19-2: the last char, and `len` lowered by its UTF-8
            // width; like `clear`, only `len` moves.
            ("String", "pop") => {
                let v = self.ref_arg(&args[0])?;
                let lenp = self.offset(v, 8)?;
                let n = self.load_i64(lenp)?;
                let p = self.load_ptr(v)?;
                let c = self.char_before_byte(p, n, n)?;
                self.store_opt_int(dest, c)?;
                let found = self
                    .builder
                    .build_int_compare(IntPredicate::SGE, c, self.i64(0), "")
                    .map_err(e)?;
                self.if_then(found, |cx| {
                    let mut left = n;
                    for at in [0, 0x80, 0x800, 0x10000] {
                        let wider = cx
                            .builder
                            .build_int_compare(IntPredicate::SGE, c, cx.i64(at), "")
                            .map_err(e)?;
                        let step = cx
                            .builder
                            .build_int_z_extend(wider, cx.llcx.i64_type(), "")
                            .map_err(e)?;
                        left = cx.builder.build_int_sub(left, step, "").map_err(e)?;
                    }
                    cx.store(lenp, left.into())
                })
            }
            // Keeps the first `k` bytes: none below 0, a no-op past the end
            // (as `Vec.truncate`), and a cut inside a char panics.
            ("String", "truncate") => {
                let v = self.ref_arg(&args[0])?;
                let k = self.int_arg(&args[1])?;
                let lenp = self.offset(v, 8)?;
                let n = self.load_i64(lenp)?;
                let neg = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, k, self.i64(0), "")
                    .map_err(e)?;
                let k = self
                    .builder
                    .build_select(neg, self.i64(0), k, "")
                    .map_err(e)?
                    .into_int_value();
                let shorter = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, k, n, "")
                    .map_err(e)?;
                self.if_then(shorter, |cx| {
                    let p = cx.load_ptr(v)?;
                    let bp = cx.elem_ptr(p, k, 1)?;
                    let b = cx
                        .builder
                        .build_load(cx.llcx.i8_type(), bp, "")
                        .map_err(e)?
                        .into_int_value();
                    let i8t = cx.llcx.i8_type();
                    let top = cx
                        .builder
                        .build_and(b, i8t.const_int(0xC0, false), "")
                        .map_err(e)?;
                    let starts = cx
                        .builder
                        .build_int_compare(IntPredicate::NE, top, i8t.const_int(0x80, false), "")
                        .map_err(e)?;
                    cx.abort_unless(starts, crate::mir::interp::TRUNCATE_OFF_A_CHAR)?;
                    cx.store(lenp, k.into())
                })
            }
            ("Vec", "from_array") => {
                let at = self.operand_type(&args[0])?;
                let TyKind::Array(el, n) = self.tys.kind(at) else {
                    return Err(format!("{name} of a `{}`", self.tys.display(at)));
                };
                let size = self.layout(el)?.size;
                let src = self.operand_ptr(&args[0])?;
                let d = self.place_ptr(dest)?;
                self.vec_alloc(d, self.i64(n), size)?;
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
                self.vec_alloc(d, n, size)?;
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
                self.store_ref(d, at, el)
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
            (_, "as_slice" | "as_mut_slice" | "as_slice_mut") if args.len() == 1 => {
                let (p, n) = self.view_of(&args[0])?;
                let d = self.place_ptr(dest)?;
                self.store(d, p.into())?;
                self.store(self.offset(d, 8)?, n.into())
            }
            ("Env", "args") => {
                let d = self.place_ptr(dest)?;
                self.vec_alloc(d, self.i64(1), 24)?;
                let buf = self.load_ptr(d)?;
                let (s, n) = self.str_const("main")?;
                self.rt("string_from", &[buf.into(), s.into(), n.into()])
            }
            (
                "Map" | "Set" | "SortedMap" | "SortedSet" | "VecDeque",
                "reserve" | "reserve_exact" | "shrink_to_fit",
            ) => {
                // A hint the tables size themselves without.
                Ok(())
            }
            ("Secret", "expose" | "expose_mut") if args.len() == 1 => {
                // A reference to the secret's one field.
                let t = self.operand_type(&args[0])?;
                let (p, sty) = match self.tys.kind(t) {
                    TyKind::Ref(i) | TyKind::MutRef(i) => (self.ref_arg(&args[0])?, i),
                    _ => (self.operand_ptr(&args[0])?, t),
                };
                let q = self.offset(p, self.field_offset(sty, None, 0)?)?;
                let d = self.place_ptr(dest)?;
                let inner = match self.tys.kind(dty) {
                    TyKind::Ref(i) | TyKind::MutRef(i) => i,
                    _ => dty,
                };
                if matches!(self.tys.kind(inner), TyKind::Str | TyKind::Slice(_)) {
                    self.copy(d, q, inner)
                } else {
                    self.store(d, q.into())
                }
            }
            ("Secret", "ct_eq") if args.len() == 2 => {
                // The two secrets' one field, compared by the field type's
                // equality (no timing to keep constant, as the interpreter).
                let mut at = Vec::with_capacity(2);
                let mut sty = None;
                for a in args {
                    let t = self.operand_type(a)?;
                    at.push(match self.tys.kind(t) {
                        TyKind::Ref(i) | TyKind::MutRef(i) => {
                            sty = Some(i);
                            self.ref_arg(a)?
                        }
                        _ => {
                            sty = Some(t);
                            self.operand_ptr(a)?
                        }
                    });
                }
                let sty = sty.ok_or("Secret.ct_eq has no receiver")?;
                let ft = self
                    .tys
                    .field_ty(sty, None, 0)
                    .ok_or("a Secret has no field")?;
                let off = self.field_offset(sty, None, 0)?;
                let (x, y) = (self.offset(at[0], off)?, self.offset(at[1], off)?);
                let eq = self.key_fn(super::key::KeyOp::Eq, ft);
                let r = self
                    .builder
                    .build_call(eq, &[x.into(), y.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("eq glue returns nothing")?
                    .into_int_value();
                self.store_bool(dest, r)?;
                self.drop_moved(&args[1..])
            }
            (_, "clone") if args.len() == 1 => {
                let t = self.operand_type(&args[0])?;
                // A `str` clones to a `String` of its own; a `ref str` is
                // the two-word view itself, not a pointer to one.
                let dty = self.place_type(dest)?;
                if matches!(self.tys.kind(dty), TyKind::Intrinsic(IntrinsicTy::String)) {
                    let inner = match self.tys.kind(t) {
                        TyKind::Ref(i) | TyKind::MutRef(i) => i,
                        _ => t,
                    };
                    if matches!(self.tys.kind(inner), TyKind::Str) {
                        let (p, n) = self.str_parts(&args[0])?;
                        let d = self.place_ptr(dest)?;
                        return self.rt("string_from", &[d.into(), p.into(), n.into()]);
                    }
                }
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

    fn smin(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        let lt = self
            .builder
            .build_int_compare(IntPredicate::SLT, a, b, "")
            .map_err(e)?;
        Ok(self
            .builder
            .build_select(lt, a, b, "")
            .map_err(e)?
            .into_int_value())
    }

    fn smax(&self, a: IntValue<'ctx>, b: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        let gt = self
            .builder
            .build_int_compare(IntPredicate::SGT, a, b, "")
            .map_err(e)?;
        Ok(self
            .builder
            .build_select(gt, a, b, "")
            .map_err(e)?
            .into_int_value())
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

    /// A fresh `Vec` of `n` slots of `size` bytes at `d`, with `len = n`
    /// (no buffer and `cap = 0` when that is no bytes), as
    /// `karac_mir_vec_alloc` builds it but from `malloc` in line, so LLVM
    /// sees a new allocation and its length. A count past the
    /// allocation ceiling goes to the runtime, which panics as before, and
    /// a failed `malloc` aborts, so no path joins with an unknown length.
    pub(super) fn vec_alloc(
        &mut self,
        d: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        size: u64,
    ) -> R<()> {
        let null = self.ptr_ty().const_null();
        if size == 0 {
            self.store(d, null.into())?;
            self.store(self.offset(d, 8)?, n.into())?;
            return self.store(self.offset(d, 16)?, self.i64(0).into());
        }
        let f = self.cur_fn()?;
        let heap = self.llcx.append_basic_block(f, "alloc.heap");
        let empty = self.llcx.append_basic_block(f, "alloc.empty");
        let got = self.llcx.append_basic_block(f, "alloc.got");
        let slow = self.llcx.append_basic_block(f, "alloc.slow");
        let done = self.llcx.append_basic_block(f, "alloc.done");
        // The runtime's ceiling, `KARAC_MAX_ALLOC_BYTES`.
        let limit = self.i64(((1u64 << 61) - 1) / size);
        let fits = self
            .builder
            .build_int_compare(IntPredicate::ULE, n, limit, "")
            .map_err(e)?;
        let nonzero = self
            .builder
            .build_int_compare(IntPredicate::NE, n, self.i64(0), "")
            .map_err(e)?;
        let small = self.llcx.append_basic_block(f, "alloc.fits");
        self.builder
            .build_conditional_branch(fits, small, slow)
            .map_err(e)?;
        self.builder.position_at_end(small);
        self.builder
            .build_conditional_branch(nonzero, heap, empty)
            .map_err(e)?;
        self.builder.position_at_end(empty);
        self.store(d, null.into())?;
        self.store(self.offset(d, 8)?, n.into())?;
        self.store(self.offset(d, 16)?, self.i64(0).into())?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(heap);
        let bytes = self
            .builder
            .build_int_nuw_mul(n, self.i64(size), "")
            .map_err(e)?;
        let malloc = self.libc_fn("malloc");
        let p = self
            .builder
            .build_call(malloc, &[bytes.into()], "buf")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let failed = self.builder.build_is_null(p, "").map_err(e)?;
        let oom = self.llcx.append_basic_block(f, "alloc.oom");
        self.builder
            .build_conditional_branch(failed, oom, got)
            .map_err(e)?;
        self.builder.position_at_end(oom);
        self.abort_with("out of memory")?;
        self.builder.position_at_end(got);
        self.store(d, p.into())?;
        self.store(self.offset(d, 8)?, n.into())?;
        self.store(self.offset(d, 16)?, n.into())?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(slow);
        // Past the ceiling the runtime panics and does not come back.
        self.rt("vec_alloc", &[d.into(), n.into(), self.i64(size).into()])?;
        self.builder.build_unreachable().map_err(e)?;
        self.builder.position_at_end(done);
        Ok(())
    }

    /// Calls the runtime's `karac_mir_{name}`, which returns a `u8` flag,
    /// as an `i1`.
    pub(super) fn rt_flag(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> R<IntValue<'ctx>> {
        let tys = self.arg_tys(args);
        let i8t = self.llcx.i8_type();
        let f = self.extern_fn(&format!("karac_mir_{name}"), i8t.fn_type(&tys, false));
        let v = self
            .builder
            .build_call(f, args, "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_int_compare(IntPredicate::NE, v, i8t.const_zero(), "")
            .map_err(e)
    }

    /// Tags the `Result` at `d` (of type `dty`) `Ok` when `ok`, whose
    /// payload the runtime wrote, else `Err`: its payload zeroed, then the
    /// variant `err_variant` of the error enum when named.
    pub(super) fn store_result_flag(
        &mut self,
        d: PointerValue<'ctx>,
        dty: Ty,
        ok: IntValue<'ctx>,
        err_variant: Option<&str>,
    ) -> R<()> {
        let okv = self.variant_of(dty, "Ok")?;
        let errv = self.variant_of(dty, "Err")?;
        let f = self.cur_fn()?;
        let (yes, no, done) = (
            self.llcx.append_basic_block(f, "result.ok"),
            self.llcx.append_basic_block(f, "result.err"),
            self.llcx.append_basic_block(f, "result"),
        );
        self.builder
            .build_conditional_branch(ok, yes, no)
            .map_err(e)?;
        self.builder.position_at_end(yes);
        self.store_tag(d, dty, okv)?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(no);
        let et = self
            .tys
            .field_ty(dty, Some(errv), 0)
            .ok_or("an `Err` without a payload")?;
        let q = self.offset(d, self.field_offset(dty, Some(errv), 0)?)?;
        let size = self.layout(et)?.size;
        if size > 0 {
            self.builder
                .build_memset(q, 1, self.llcx.i8_type().const_zero(), self.i64(size))
                .map_err(e)?;
        }
        if let Some(name) = err_variant {
            let ev = self.variant_of(et, name)?;
            self.store_tag(q, et, ev)?;
        }
        self.store_tag(d, dty, errv)?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(done);
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
