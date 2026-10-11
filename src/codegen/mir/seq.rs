//! The text and sequence natives: `String` methods over the runtime's
//! `karac_mir_str_*`, and the methods of a view (a slice, an array or a
//! `Vec` seen as one) and of a `Vec` itself. Their reference semantics
//! are the interpreter's `string_method`, `view_method` and
//! `vec_more_method` (`src/mir/interp.rs`).

use inkwell::values::{BasicMetadataValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::key::KeyOp;
use super::{Cx, R};
use crate::mir::ty::IntrinsicTy;
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// `karac_mir_<name>(args)` returning an integer of `bits` bits.
    pub(super) fn rt_int(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
        bits: u32,
    ) -> R<IntValue<'ctx>> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(
            &format!("karac_mir_{name}"),
            self.int_ty(bits).fn_type(&tys, false),
        );
        let v = self.builder.build_call(f, args, "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }

    /// The index of the variant named `name` of the enum `ty`.
    pub(super) fn variant_of(&self, ty: Ty, name: &str) -> R<u32> {
        let (adt, _) = self
            .tys
            .tcx()
            .adt_of(ty)
            .ok_or_else(|| format!("`{}` is not an enum", self.tys.display(ty)))?;
        adt.variants
            .iter()
            .position(|v| v.name == name)
            .map(|v| v as u32)
            .ok_or_else(|| format!("`{}` has no variant {name}", adt.name))
    }

    /// Writes the fieldless variant `name` of the enum `ty` at `p`.
    fn store_variant(&mut self, p: PointerValue<'ctx>, ty: Ty, name: &str) -> R<()> {
        let v = self.variant_of(ty, name)?;
        self.store_tag(p, ty, v)
    }

    /// Writes the `Ordering` for -1 / 0 / 1 at `p`, of type `ty`.
    fn store_ordering(&mut self, p: PointerValue<'ctx>, ty: Ty, o: IntValue<'ctx>) -> R<()> {
        let f = self.cur_fn()?;
        let done = self.llcx.append_basic_block(f, "ordered");
        let mut arms = Vec::new();
        for (k, name) in [(-1i64, "Less"), (0, "Equal"), (1, "Greater")] {
            let bb = self.llcx.append_basic_block(f, name);
            arms.push((o.get_type().const_int(k as u64, true), bb, name));
        }
        let cases: Vec<_> = arms.iter().map(|(k, bb, _)| (*k, *bb)).collect();
        self.builder.build_switch(o, done, &cases).map_err(e)?;
        for (_, bb, name) in arms {
            self.builder.position_at_end(bb);
            self.store_variant(p, ty, name)?;
            self.builder.build_unconditional_branch(done).map_err(e)?;
        }
        self.builder.position_at_end(done);
        Ok(())
    }

    /// `if code != 0 { abort(msg) }`.
    fn abort_on(&mut self, code: IntValue<'ctx>, msg: &str) -> R<()> {
        let ok = self.is_zero(code)?;
        self.abort_unless(ok, msg)
    }

    /// The natives of `String`, or `None` for one that is not lowered here.
    pub(super) fn string_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        let test = match method {
            "contains" => Some(0),
            "starts_with" => Some(1),
            "ends_with" => Some(2),
            "lt" => Some(3),
            _ => None,
        };
        let map = match method {
            "trim" => Some(0),
            "to_uppercase" => Some(1),
            "to_lowercase" => Some(2),
            "trim_start" => Some(3),
            "trim_end" => Some(4),
            "sorted" => Some(5),
            _ => None,
        };
        match (method, args) {
            (_, [a, b]) if test.is_some() => {
                if !self.is_text(b)? {
                    return Ok(None);
                }
                let (ap, an) = self.str_parts(a)?;
                let (bp, bn) = self.str_parts(b)?;
                let op = self.llcx.i32_type().const_int(test.unwrap_or(0), false);
                let r = self.rt_int(
                    "str_test",
                    &[op.into(), ap.into(), an.into(), bp.into(), bn.into()],
                    8,
                )?;
                let d = self.place_ptr(dest)?;
                self.store(d, r.into())?;
            }
            (_, [s]) if map.is_some() => {
                let (p, n) = self.str_parts(s)?;
                let op = self.llcx.i32_type().const_int(map.unwrap_or(0), false);
                let d = self.place_ptr(dest)?;
                self.rt("str_map", &[d.into(), op.into(), p.into(), n.into()])?;
            }
            ("cmp", [a, b]) => {
                let (ap, an) = self.str_parts(a)?;
                let (bp, bn) = self.str_parts(b)?;
                let o = self.rt_int(
                    "bytes_cmp",
                    &[ap.into(), an.into(), bp.into(), bn.into()],
                    32,
                )?;
                let d = self.place_ptr(dest)?;
                self.store_ordering(d, dty, o)?;
            }
            ("substring" | "index_range", [s, lo, rest @ ..]) => {
                let (p, n) = self.str_parts(s)?;
                let lo = self.int_arg(lo)?;
                let hi = match rest {
                    [hi] => self.int_arg(hi)?,
                    _ => self.i64(i64::MAX as u64),
                };
                let mode = self
                    .llcx
                    .i32_type()
                    .const_int(u64::from(method == "index_range"), false);
                let d = self.place_ptr(dest)?;
                let r = self.rt_int(
                    "str_slice",
                    &[
                        d.into(),
                        p.into(),
                        n.into(),
                        lo.into(),
                        hi.into(),
                        mode.into(),
                    ],
                    8,
                )?;
                self.abort_on(r, "explicit panic")?;
            }
            ("char_at_byte", [s, i]) => {
                let (p, n) = self.str_parts(s)?;
                let i = self.int_arg(i)?;
                let c = self.rt_int("char_at_byte", &[p.into(), n.into(), i.into()], 64)?;
                let ok = self.non_negative(c)?;
                self.abort_unless(ok, "explicit panic")?;
                let c = self
                    .builder
                    .build_int_truncate(c, self.llcx.i32_type(), "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, c.into())?;
            }
            ("bytes", [s]) => {
                // A view of the text's own bytes; the borrow keeps it alive
                // and unchanged for as long as the view is used.
                let (p, n) = self.str_parts(s)?;
                let d = self.place_ptr(dest)?;
                self.store(d, p.into())?;
                self.store(self.offset(d, 8)?, n.into())?;
            }
            ("from_utf8", [b]) => {
                let (p, n, _) = self.view_parts(b)?;
                let ok = self.variant_of(dty, "Ok")?;
                let errv = self.variant_of(dty, "Err")?;
                let d = self.place_ptr(dest)?;
                let payload = self.offset(d, self.field_offset(dty, Some(ok), 0)?)?;
                let r = self.rt_int("str_from_utf8", &[payload.into(), p.into(), n.into()], 32)?;
                self.store_tag(d, dty, ok)?;
                let bad = self
                    .builder
                    .build_int_compare(IntPredicate::NE, r, r.get_type().const_zero(), "")
                    .map_err(e)?;
                let ety = self
                    .tys
                    .field_ty(dty, Some(errv), 0)
                    .ok_or("a from_utf8 Err has no payload")?;
                let eoff = self.field_offset(dty, Some(errv), 0)?;
                self.if_then(bad, |cx| {
                    cx.store_tag(d, dty, errv)?;
                    let q = cx.offset(d, eoff)?;
                    let incomplete = cx
                        .builder
                        .build_int_compare(
                            IntPredicate::EQ,
                            r,
                            r.get_type().const_int(1, false),
                            "",
                        )
                        .map_err(e)?;
                    let (iv, bv) = (
                        cx.variant_of(ety, "IncompleteSequence")?,
                        cx.variant_of(ety, "InvalidByte")?,
                    );
                    let tag_ty = cx.llcx.i32_type();
                    let k = cx
                        .builder
                        .build_select(
                            incomplete,
                            tag_ty.const_int(u64::from(iv), false),
                            tag_ty.const_int(u64::from(bv), false),
                            "",
                        )
                        .map_err(e)?
                        .into_int_value();
                    cx.store_tag_dyn(q, ety, k)
                })?;
                self.drop_moved(args)?;
            }
            ("replace", [s, a, b]) => {
                let (sp, sn) = self.str_parts(s)?;
                let (ap, an) = self.str_parts(a)?;
                let (bp, bn) = self.str_parts(b)?;
                let d = self.place_ptr(dest)?;
                self.rt(
                    "str_replace",
                    &[
                        d.into(),
                        sp.into(),
                        sn.into(),
                        ap.into(),
                        an.into(),
                        bp.into(),
                        bn.into(),
                    ],
                )?;
                self.drop_moved(&args[1..])?;
            }
            ("repeat", [s, n]) => {
                let (sp, sn) = self.str_parts(s)?;
                let n = self.int_arg(n)?;
                let d = self.place_ptr(dest)?;
                self.rt("str_repeat", &[d.into(), sp.into(), sn.into(), n.into()])?;
            }
            ("split" | "split_whitespace" | "lines", [s, rest @ ..]) => {
                let (sp, sn) = self.str_parts(s)?;
                let (op, (pp, pn)) = match (method, rest) {
                    ("split", [sep]) => {
                        if !self.is_text(sep)? {
                            return Ok(None);
                        }
                        (0, self.str_parts(sep)?)
                    }
                    ("split_whitespace", []) => (1, (sp, self.i64(0))),
                    ("lines", []) => (2, (sp, self.i64(0))),
                    _ => return Ok(None),
                };
                let op = self.llcx.i32_type().const_int(op, false);
                let d = self.place_ptr(dest)?;
                self.rt(
                    "str_split",
                    &[
                        d.into(),
                        op.into(),
                        sp.into(),
                        sn.into(),
                        pp.into(),
                        pn.into(),
                    ],
                )?;
                self.drop_moved(rest)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// Whether an operand is text (`str`, `String`, or a reference to one).
    fn is_text(&self, a: &Operand) -> R<bool> {
        let mut t = self.operand_type(a)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            t = i;
        }
        Ok(matches!(
            self.tys.kind(t),
            TyKind::Str | TyKind::Intrinsic(IntrinsicTy::String)
        ))
    }

    /// Writes the tag `k` (an `i32`) of the enum `ty` at `p`.
    fn store_tag_dyn(&mut self, p: PointerValue<'ctx>, ty: Ty, k: IntValue<'ctx>) -> R<()> {
        let Some(tag) = crate::mir::layout::enum_tag(self.tys, ty) else {
            return Ok(());
        };
        let tt = self.int_ty(tag.size as u32 * 8);
        let k = self
            .builder
            .build_int_cast_sign_flag(k, tt, false, "")
            .map_err(e)?;
        self.store(p, k.into())
    }

    /// The `{ptr, len}` and element type of a view operand: a slice, an
    /// array place or a `Vec`, through references.
    pub(super) fn view_parts(
        &mut self,
        a: &Operand,
    ) -> R<(PointerValue<'ctx>, IntValue<'ctx>, Ty)> {
        let mut t = self.operand_type(a)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            t = i;
        }
        let el = match self.tys.kind(t) {
            TyKind::Slice(el) | TyKind::Array(el, _) | TyKind::Intrinsic(IntrinsicTy::Vec(el)) => {
                el
            }
            _ => return Err(format!("a view of a `{}`", self.tys.display(t))),
        };
        let (p, n) = self.view_of(a)?;
        Ok((p, n, el))
    }

    /// The pointer to element `i` of a view, aborting out of bounds.
    fn checked_elem(
        &mut self,
        p: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        i: IntValue<'ctx>,
        size: u64,
    ) -> R<PointerValue<'ctx>> {
        let ok = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, n, "")
            .map_err(e)?;
        self.abort_unless(ok, "index out of bounds")?;
        self.elem_ptr(p, i, size)
    }

    /// Writes `Some(ref elem i)` or `None` at `dest` (an `Option[ref T]`).
    fn some_elem(
        &mut self,
        dest: &Place,
        p: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        i: IntValue<'ctx>,
        size: u64,
    ) -> R<()> {
        let dty = self.place_type(dest)?;
        let ok = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, n, "")
            .map_err(e)?;
        let d = self.place_ptr(dest)?;
        let (some, none) = self.option_variants(dty)?;
        self.store_tag(d, dty, none)?;
        let off = self.field_offset(dty, Some(some), 0)?;
        self.if_then(ok, |cx| {
            let at = cx.elem_ptr(p, i, size)?;
            cx.store_tag(d, dty, some)?;
            cx.store(cx.offset(d, off)?, at.into())
        })
    }

    /// The comparison a sort of `el` uses: the type's own `cmp` body when
    /// it has one, else the structural order.
    fn sort_cmp(&mut self, el: Ty) -> PointerValue<'ctx> {
        let op = if self.user_cmp_body(el).is_some() {
            KeyOp::User
        } else {
            KeyOp::Cmp
        };
        self.key_fn(op, el).as_global_value().as_pointer_value()
    }

    /// The methods of a view or a `Vec`, or `None` for one not lowered here.
    pub(super) fn seq_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let rt = self.operand_type(recv)?;
        let mut inner = rt;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(inner) {
            inner = i;
        }
        let is_vec = matches!(self.tys.kind(inner), TyKind::Intrinsic(IntrinsicTy::Vec(_)));
        if !(is_vec || matches!(self.tys.kind(inner), TyKind::Slice(_) | TyKind::Array(..))) {
            return Ok(None);
        }
        let (p, n, el) = self.view_parts(recv)?;
        let size = self.layout(el)?.size;
        let dty = self.place_type(dest)?;
        match (method, &args[1..]) {
            ("len", []) => {
                let d = self.place_ptr(dest)?;
                let it = self.llvm_ty(dty)?.into_int_type();
                let n = self
                    .builder
                    .build_int_truncate_or_bit_cast(n, it, "")
                    .map_err(e)?;
                self.store(d, n.into())?;
            }
            ("is_empty", []) => {
                let z = self.is_zero(n)?;
                self.store_bool(dest, z)?;
            }
            ("index" | "index_mut", [i]) => {
                let i = self.int_arg(i)?;
                let at = self.checked_elem(p, n, i, size)?;
                let d = self.place_ptr(dest)?;
                self.store(d, at.into())?;
            }
            ("get", [i]) => {
                let i = self.int_arg(i)?;
                self.some_elem(dest, p, n, i, size)?;
            }
            ("first", []) => self.some_elem(dest, p, n, self.i64(0), size)?,
            ("last", rest) => {
                let k = match rest {
                    [k] => self.int_arg(k)?,
                    _ => self.i64(0),
                };
                let last = self.builder.build_int_sub(n, self.i64(1), "").map_err(e)?;
                let i = self.builder.build_int_sub(last, k, "").map_err(e)?;
                self.some_elem(dest, p, n, i, size)?;
            }
            ("slice" | "slice_mut", [a, b]) => {
                let (a, b) = (self.int_arg(a)?, self.int_arg(b)?);
                let a_ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, a, b, "")
                    .map_err(e)?;
                let b_ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, b, n, "")
                    .map_err(e)?;
                let ok = self.builder.build_and(a_ok, b_ok, "").map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                let start = self.elem_ptr(p, a, size)?;
                let len = self.builder.build_int_sub(b, a, "").map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, start.into())?;
                self.store(self.offset(d, 8)?, len.into())?;
            }
            ("swap", [i, j]) => {
                let (i, j) = (self.int_arg(i)?, self.int_arg(j)?);
                let x = self.checked_elem(p, n, i, size)?;
                let y = self.checked_elem(p, n, j, size)?;
                let tmp = self.temp(el, "swap")?;
                self.copy(tmp, x, el)?;
                self.copy(x, y, el)?;
                self.copy(y, tmp, el)?;
            }
            ("reverse", []) => {
                let tmp = self.temp(el, "swap")?;
                let half = self
                    .builder
                    .build_int_unsigned_div(n, self.i64(2), "")
                    .map_err(e)?;
                let last = self.builder.build_int_sub(n, self.i64(1), "").map_err(e)?;
                self.count_loop(half, |cx, i| {
                    let j = cx.builder.build_int_sub(last, i, "").map_err(e)?;
                    let x = cx.elem_ptr(p, i, size)?;
                    let y = cx.elem_ptr(p, j, size)?;
                    cx.copy(tmp, x, el)?;
                    cx.copy(x, y, el)?;
                    cx.copy(y, tmp, el)
                })?;
            }
            ("contains", [needle]) => {
                let np = self.key_arg_of(needle, el)?;
                let res = self.entry_slot(1, "found")?;
                self.store(res, self.llcx.i8_type().const_zero().into())?;
                let eq = self.key_fn(KeyOp::Eq, el);
                self.count_loop(n, |cx, i| {
                    let x = cx.elem_ptr(p, i, size)?;
                    let v = cx
                        .builder
                        .build_call(eq, &[x.into(), np.into()], "")
                        .map_err(e)?
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    cx.if_then(v, |cx| {
                        cx.store(res, cx.llcx.i8_type().const_int(1, false).into())
                    })
                })?;
                let d = self.place_ptr(dest)?;
                let v = self
                    .builder
                    .build_load(self.llcx.i8_type(), res, "")
                    .map_err(e)?;
                self.store(d, v)?;
                self.drop_moved(&args[1..])?;
            }
            ("sort" | "sort_unstable", []) => {
                let cmp = self.sort_cmp(el);
                self.rt(
                    "sort",
                    &[p.into(), n.into(), self.i64(size).into(), cmp.into()],
                )?;
            }
            ("sort_by" | "sort_unstable_by" | "sort_by_key" | "sort_unstable_by_key"
            | "sort_by_cached_key", [f]) => {
                let by_key = method.contains("key");
                let (cmp, ctx) = self.sort_by_args(f, by_key, el)?;
                self.rt(
                    "sort_ctx",
                    &[p.into(), n.into(), self.i64(size).into(), cmp.into(), ctx.into()],
                )?;
                self.drop_moved(&args[1..])?;
            }
            ("to_vec", []) => self.vec_of_clones(dest, p, n, el)?,
            ("join", [sep]) if is_vec => {
                let (sp, sn) = self.str_parts(sep)?;
                let d = self.place_ptr(dest)?;
                self.rt(
                    "str_join",
                    &[
                        d.into(),
                        p.into(),
                        n.into(),
                        self.i64(size).into(),
                        sp.into(),
                        sn.into(),
                    ],
                )?;
                self.drop_moved(&args[1..])?;
            }
            ("remove", [i]) if is_vec => {
                let i = self.int_arg(i)?;
                let at = self.checked_elem(p, n, i, size)?;
                let d = self.place_ptr(dest)?;
                self.copy(d, at, el)?;
                let rest = self.builder.build_int_sub(n, i, "").map_err(e)?;
                let rest = self
                    .builder
                    .build_int_sub(rest, self.i64(1), "")
                    .map_err(e)?;
                let bytes = self
                    .builder
                    .build_int_mul(rest, self.i64(size), "")
                    .map_err(e)?;
                let next = self.offset(at, size)?;
                self.builder
                    .build_memmove(at, 1, next, 1, bytes)
                    .map_err(e)?;
                let v = self.ref_arg(recv)?;
                let m = self.builder.build_int_sub(n, self.i64(1), "").map_err(e)?;
                self.store(self.offset(v, 8)?, m.into())?;
            }
            ("insert", [i, x]) if is_vec => {
                let i = self.int_arg(i)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, i, n, "")
                    .map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                let v = self.ref_arg(recv)?;
                self.rt(
                    "vec_reserve",
                    &[v.into(), self.i64(1).into(), self.i64(size).into()],
                )?;
                let buf = self.load_ptr(v)?;
                let at = self.elem_ptr(buf, i, size)?;
                let rest = self.builder.build_int_sub(n, i, "").map_err(e)?;
                let bytes = self
                    .builder
                    .build_int_mul(rest, self.i64(size), "")
                    .map_err(e)?;
                let next = self.offset(at, size)?;
                self.builder
                    .build_memmove(next, 1, at, 1, bytes)
                    .map_err(e)?;
                self.store_operand(at, x)?;
                let m = self.builder.build_int_add(n, self.i64(1), "").map_err(e)?;
                self.store(self.offset(v, 8)?, m.into())?;
            }
            ("clear", []) if is_vec => {
                if self.tys.needs_drop(el) {
                    self.count_loop(n, |cx, i| {
                        let x = cx.elem_ptr(p, i, size)?;
                        cx.call_glue(x, el)
                    })?;
                }
                let v = self.ref_arg(recv)?;
                self.store(self.offset(v, 8)?, self.i64(0).into())?;
            }
            ("extend_from_slice", [other]) if is_vec => {
                let (op, on, _) = self.view_parts(other)?;
                let v = self.ref_arg(recv)?;
                self.rt("vec_reserve", &[v.into(), on.into(), self.i64(size).into()])?;
                let buf = self.load_ptr(v)?;
                let end = self.elem_ptr(buf, n, size)?;
                self.clone_seq(end, op, on, el)?;
                let m = self.builder.build_int_add(n, on, "").map_err(e)?;
                self.store(self.offset(v, 8)?, m.into())?;
            }
            _ => return Ok(None),
        }
        let _ = base;
        Ok(Some(()))
    }

    /// A new `Vec` at `dest` of clones of `n` elements of `el` from `p`.
    pub(super) fn vec_of_clones(
        &mut self,
        dest: &Place,
        p: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        el: Ty,
    ) -> R<()> {
        let size = self.layout(el)?.size;
        let d = self.place_ptr(dest)?;
        self.rt("vec_alloc", &[d.into(), n.into(), self.i64(size).into()])?;
        let buf = self.load_ptr(d)?;
        self.clone_seq(buf, p, n, el)
    }

    /// Clones `n` elements of `el` from `src` into `dst`.
    fn clone_seq(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        el: Ty,
    ) -> R<()> {
        let size = self.layout(el)?.size;
        self.count_loop(n, |cx, i| {
            let d = cx.elem_ptr(dst, i, size)?;
            let s = cx.elem_ptr(src, i, size)?;
            cx.clone_into(d, s, el)
        })
    }

    /// Where a needle's bytes are, in the layout of `el`.
    fn key_arg_of(&mut self, a: &Operand, el: Ty) -> R<PointerValue<'ctx>> {
        let mut t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        while t != el {
            match self.tys.kind(t) {
                TyKind::Ref(i) | TyKind::MutRef(i) => {
                    if matches!(self.tys.kind(i), TyKind::Str | TyKind::Slice(_)) {
                        break;
                    }
                    p = self.load_ptr(p)?;
                    t = i;
                }
                _ => break,
            }
        }
        Ok(p)
    }
}

impl<'ctx> Cx<'ctx, '_> {
    /// `llvm.<name>` over the float type of `v`.
    fn float_intrinsic(
        &self,
        name: &str,
        args: &[inkwell::values::FloatValue<'ctx>],
    ) -> R<inkwell::values::FloatValue<'ctx>> {
        let ft = args[0].get_type();
        let i = inkwell::intrinsics::Intrinsic::find(&format!("llvm.{name}"))
            .ok_or_else(|| format!("no intrinsic llvm.{name}"))?;
        let f = i
            .get_declaration(&self.module, &[ft.into()])
            .ok_or_else(|| format!("llvm.{name} has no declaration"))?;
        let vals: Vec<BasicMetadataValueEnum> = args.iter().map(|a| (*a).into()).collect();
        Ok(self
            .builder
            .build_call(f, &vals, "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value())
    }

    /// The methods of a number or a char by value (`abs`, `max`, `min`,
    /// `signum`, the wrapping operations, the float roundings, and
    /// `len_utf8`), or `None` for one not lowered here.
    pub(super) fn scalar_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let Some(first) = args.first() else {
            return Ok(None);
        };
        let t = self.operand_type(first)?;
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            if self.operand_type(a)? != t {
                return Ok(None);
            }
            vals.push(self.operand_scalar(a)?);
        }
        let b = &self.builder;
        let v: inkwell::values::BasicValueEnum = match (self.tys.kind(t), method, vals.as_slice()) {
            (TyKind::Char, "len_utf8", [c]) => {
                let c = c.into_int_value();
                let ct = c.get_type();
                let mut n = self.i64(4);
                for (lim, k) in [(0x10000u64, 3u64), (0x800, 2), (0x80, 1)] {
                    let lt = b
                        .build_int_compare(IntPredicate::ULT, c, ct.const_int(lim, false), "")
                        .map_err(e)?;
                    n = b
                        .build_select(lt, self.i64(k), n, "")
                        .map_err(e)?
                        .into_int_value();
                }
                let it = self.llvm_ty(self.place_type(dest)?)?.into_int_type();
                b.build_int_cast_sign_flag(n, it, false, "").map_err(e)?.into()
            }
            (TyKind::Int(it), "abs", [x]) => {
                let x = x.into_int_value();
                if it.signed() {
                    let min = x.get_type().const_int(1u64 << 63, false);
                    let min = if it.bits() == 64 {
                        min
                    } else {
                        b.build_left_shift(
                            x.get_type().const_int(1, false),
                            x.get_type().const_int(u64::from(it.bits() - 1), false),
                            "",
                        )
                        .map_err(e)?
                    };
                    let ok = b
                        .build_int_compare(IntPredicate::NE, x, min, "")
                        .map_err(e)?;
                    self.abort_unless(ok, "integer overflow")?;
                    let b = &self.builder;
                    let neg = b
                        .build_int_compare(IntPredicate::SLT, x, x.get_type().const_zero(), "")
                        .map_err(e)?;
                    let nx = b.build_int_neg(x, "").map_err(e)?;
                    b.build_select(neg, nx, x, "").map_err(e)?
                } else {
                    x.into()
                }
            }
            (TyKind::Int(it), "signum", [x]) => {
                let x = x.into_int_value();
                let z = x.get_type().const_zero();
                let (lp, gp) = if it.signed() {
                    (IntPredicate::SLT, IntPredicate::SGT)
                } else {
                    (IntPredicate::ULT, IntPredicate::UGT)
                };
                let lt = b.build_int_compare(lp, x, z, "").map_err(e)?;
                let gt = b.build_int_compare(gp, x, z, "").map_err(e)?;
                let g = b.build_int_z_extend(gt, x.get_type(), "").map_err(e)?;
                let l = b.build_int_z_extend(lt, x.get_type(), "").map_err(e)?;
                b.build_int_sub(g, l, "").map_err(e)?.into()
            }
            (TyKind::Int(it), "max" | "min", [x, y]) => {
                let (x, y) = (x.into_int_value(), y.into_int_value());
                let p = match (method, it.signed()) {
                    ("max", true) => IntPredicate::SGT,
                    ("max", false) => IntPredicate::UGT,
                    (_, true) => IntPredicate::SLT,
                    _ => IntPredicate::ULT,
                };
                let c = b.build_int_compare(p, x, y, "").map_err(e)?;
                b.build_select(c, x, y, "").map_err(e)?
            }
            (TyKind::Int(_), "wrapping_add" | "wrapping_sub" | "wrapping_mul", [x, y]) => {
                let (x, y) = (x.into_int_value(), y.into_int_value());
                match method {
                    "wrapping_add" => b.build_int_add(x, y, ""),
                    "wrapping_sub" => b.build_int_sub(x, y, ""),
                    _ => b.build_int_mul(x, y, ""),
                }
                .map_err(e)?
                .into()
            }
            (TyKind::Float(_), "abs" | "sqrt" | "floor" | "ceil" | "round", [x]) => {
                let name = if method == "abs" { "fabs" } else { method };
                self.float_intrinsic(name, &[x.into_float_value()])?.into()
            }
            (TyKind::Float(_), "max" | "min", [x, y]) => {
                let name = if method == "max" { "maxnum" } else { "minnum" };
                self.float_intrinsic(name, &[x.into_float_value(), y.into_float_value()])?
                    .into()
            }
            _ => return Ok(None),
        };
        let d = self.place_ptr(dest)?;
        self.store(d, v)?;
        Ok(Some(()))
    }
}
