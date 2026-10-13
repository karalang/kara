//! The text and sequence natives: `String` methods over the runtime's
//! `karac_mir_str_*`, and the methods of a view (a slice, an array or a
//! `Vec` seen as one) and of a `Vec` itself. Their reference semantics
//! are the interpreter's `string_method`, `view_method` and
//! `vec_more_method` (`src/mir/interp.rs`).

use inkwell::values::{BasicMetadataValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::key::KeyOp;
use super::{Cx, R};
use crate::mir::ty::{IntTy, IntrinsicTy};
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

    /// Appends the `et` at `x` to the `Vec` at `v`: in line while there
    /// is room (a typed copy and a length bump), through the runtime only
    /// to grow.
    pub(super) fn vec_push(
        &mut self,
        v: PointerValue<'ctx>,
        x: PointerValue<'ctx>,
        et: Ty,
    ) -> R<()> {
        let size = self.layout(et)?.size;
        if size == 0 {
            return self.rt("vec_push", &[v.into(), x.into(), self.i64(0).into()]);
        }
        let lenp = self.offset(v, 8)?;
        let len = self.load_i64(lenp)?;
        let cap = self.load_i64(self.offset(v, 16)?)?;
        let full = self
            .builder
            .build_int_compare(IntPredicate::UGE, len, cap, "full")
            .map_err(e)?;
        self.if_then(full, |cx| {
            cx.rt(
                "vec_reserve",
                &[v.into(), cx.i64(1).into(), cx.i64(size).into()],
            )
        })?;
        let buf = self.load_ptr(v)?;
        let at = self.elem_ptr(buf, len, size)?;
        self.copy(at, x, et)?;
        let len = self
            .builder
            .build_int_add(len, self.i64(1), "")
            .map_err(e)?;
        self.store(lenp, len.into())
    }

    /// The char starting at byte `i` of the text `p[..n]`, or -1 when
    /// there is none: an ASCII byte in line, anything else through the
    /// runtime.
    fn char_at_byte(
        &mut self,
        p: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        i: IntValue<'ctx>,
    ) -> R<IntValue<'ctx>> {
        let f = self.cur_fn()?;
        let slot = self.entry_slot(8, "ch")?;
        let (look, fast, slow, done) = (
            self.llcx.append_basic_block(f, "ch.look"),
            self.llcx.append_basic_block(f, "ch.ascii"),
            self.llcx.append_basic_block(f, "ch.slow"),
            self.llcx.append_basic_block(f, "ch.done"),
        );
        // Unsigned, so a negative index is out of range too.
        let inside = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, n, "")
            .map_err(e)?;
        self.builder
            .build_conditional_branch(inside, look, slow)
            .map_err(e)?;
        self.builder.position_at_end(look);
        let bp = self.elem_ptr(p, i, 1)?;
        let b = self
            .builder
            .build_load(self.llcx.i8_type(), bp, "")
            .map_err(e)?
            .into_int_value();
        let ascii = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                b,
                self.llcx.i8_type().const_int(0x80, false),
                "",
            )
            .map_err(e)?;
        self.builder
            .build_conditional_branch(ascii, fast, slow)
            .map_err(e)?;
        self.builder.position_at_end(fast);
        let c = self
            .builder
            .build_int_z_extend(b, self.llcx.i64_type(), "")
            .map_err(e)?;
        self.store(slot, c.into())?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(slow);
        let c = self.rt_int("char_at_byte", &[p.into(), n.into(), i.into()], 64)?;
        self.store(slot, c.into())?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(done);
        self.load_i64(slot)
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
            ("sorted_by", [s, f]) => {
                // The chars, sorted by the comparator, as a new String.
                let (p, n) = self.str_parts(s)?;
                let malloc = self.libc_fn("malloc");
                let room = self.builder.build_int_mul(n, self.i64(4), "").map_err(e)?;
                let room = self
                    .builder
                    .build_int_add(room, self.i64(4), "")
                    .map_err(e)?;
                let buf = self
                    .builder
                    .build_call(malloc, &[room.into()], "chars")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let k = self.mrt_i64("str_decode", &[buf.into(), p.into(), n.into()])?;
                let ch = self.tys.intern(TyKind::Char);
                let (cmp, ctx) = self.sort_by_args(f, false, ch)?;
                self.rt(
                    "sort_ctx",
                    &[
                        buf.into(),
                        k.into(),
                        self.i64(4).into(),
                        cmp.into(),
                        ctx.into(),
                    ],
                )?;
                let d = self.place_ptr(dest)?;
                self.rt("string_from_chars", &[d.into(), buf.into(), k.into()])?;
                self.free(buf)?;
                self.drop_moved(&args[1..])?;
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
            ("reserve" | "reserve_exact" | "shrink_to_fit", _) => {}
            ("slice", [s, lo, hi]) => {
                // Saturating, as the interpreter's: a start out of range is
                // empty and the end clamps; a cut inside a char is lossy.
                let (p, n) = self.str_parts(s)?;
                let lo = self.int_arg(lo)?;
                let hi = self.int_arg(hi)?;
                let mode = self.llcx.i32_type().const_int(2, false);
                let d = self.place_ptr(dest)?;
                self.rt_int(
                    self.str_cut(dty),
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
                    self.str_cut(dty),
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
                let c = self.char_at_byte(p, n, i)?;
                let ok = self.non_negative(c)?;
                self.abort_unless(ok, "explicit panic")?;
                let c = self
                    .builder
                    .build_int_truncate(c, self.llcx.i32_type(), "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, c.into())?;
            }
            ("bytes" | "as_str", [s]) => {
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
            ("find", [s, needle]) => {
                let (sp, sn) = self.str_parts(s)?;
                let nt = self.operand_type(needle)?;
                let (np, nn, c) = if matches!(self.tys.kind(nt), TyKind::Char) {
                    let c = self.operand_scalar(needle)?.into_int_value();
                    let c = self
                        .builder
                        .build_int_z_extend(c, self.llcx.i64_type(), "")
                        .map_err(e)?;
                    (sp, self.i64(0), c)
                } else if self.is_text(needle)? {
                    let (np, nn) = self.str_parts(needle)?;
                    (np, nn, self.i64(u64::MAX))
                } else {
                    return Ok(None);
                };
                let at = self.rt_int(
                    "str_find",
                    &[sp.into(), sn.into(), np.into(), nn.into(), c.into()],
                    64,
                )?;
                self.store_opt_int(dest, at)?;
                self.drop_moved(&args[1..])?;
            }
            ("char_at" | "char_count", [s, rest @ ..]) => {
                let (sp, sn) = self.str_parts(s)?;
                let (op, i) = match rest {
                    [i] => (0, self.int_arg(i)?),
                    _ => (1, self.i64(0)),
                };
                let op = self.llcx.i32_type().const_int(op, false);
                let v = self.rt_int(
                    "str_chars",
                    &[op.into(), sp.into(), sn.into(), i.into()],
                    64,
                )?;
                if method == "char_at" {
                    self.store_opt_int(dest, v)?;
                } else {
                    self.store_int_as(dest, v, true)?;
                }
            }
            ("strip_prefix" | "strip_suffix", [s, p]) => {
                if !self.is_text(p)? {
                    return Ok(None);
                }
                let (sp, sn) = self.str_parts(s)?;
                let (pp, pn) = self.str_parts(p)?;
                let d = self.place_ptr(dest)?;
                let (some, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
                let op = self
                    .llcx
                    .i32_type()
                    .const_int(u64::from(method == "strip_suffix"), false);
                let r = self.rt_int(
                    "str_strip",
                    &[
                        q.into(),
                        op.into(),
                        sp.into(),
                        sn.into(),
                        pp.into(),
                        pn.into(),
                    ],
                    8,
                )?;
                let found = self
                    .builder
                    .build_int_compare(IntPredicate::NE, r, r.get_type().const_zero(), "")
                    .map_err(e)?;
                self.if_then(found, |cx| cx.store_tag(d, dty, some))?;
                self.drop_moved(&args[1..])?;
            }
            ("replacen", [s, a, b, n]) => {
                let (sp, sn) = self.str_parts(s)?;
                let (ap, an) = self.str_parts(a)?;
                let (bp, bn) = self.str_parts(b)?;
                let n = self.int_arg(n)?;
                let d = self.place_ptr(dest)?;
                self.rt(
                    "str_replacen",
                    &[
                        d.into(),
                        sp.into(),
                        sn.into(),
                        ap.into(),
                        an.into(),
                        bp.into(),
                        bn.into(),
                        n.into(),
                    ],
                )?;
                self.drop_moved(&args[1..3])?;
            }
            ("repeat", [s, n]) => {
                let (sp, sn) = self.str_parts(s)?;
                let n = self.int_arg(n)?;
                let d = self.place_ptr(dest)?;
                self.rt("str_repeat", &[d.into(), sp.into(), sn.into(), n.into()])?;
            }
            ("split" | "split_whitespace" | "lines", [s, rest @ ..]) => {
                let (sp, sn) = self.str_parts(s)?;
                let mut sep_buf = None;
                let (op, (pp, pn)) = match (method, rest) {
                    ("split", [sep]) if self.is_char(sep)? => {
                        // A `char` separator, encoded into a scratch
                        // `String` that drops after the split.
                        let string = self.tys.intern(TyKind::Intrinsic(IntrinsicTy::String));
                        let tmp = self.temp(string, "sep")?;
                        self.store(tmp, self.llvm_ty(string)?.const_zero())?;
                        let c = self.operand_scalar(sep)?;
                        self.rt("string_push_char", &[tmp.into(), c.into()])?;
                        sep_buf = Some((tmp, string));
                        (
                            0,
                            (self.load_ptr(tmp)?, self.load_i64(self.offset(tmp, 8)?)?),
                        )
                    }
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
                if let Some((tmp, string)) = sep_buf {
                    self.call_glue(tmp, string)?;
                }
                self.drop_moved(rest)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// The runtime cut of a text whose result has type `dty`: a `str`
    /// result (a `StringSlice`) is a view into the text, which nothing
    /// drops; a `String` result is a copy of its own.
    fn str_cut(&self, dty: Ty) -> &'static str {
        if matches!(self.tys.kind(dty), TyKind::Str) {
            "str_view"
        } else {
            "str_slice"
        }
    }

    /// Whether an operand is a `char`.
    fn is_char(&self, a: &Operand) -> R<bool> {
        let t = self.operand_type(a)?;
        Ok(matches!(self.tys.kind(t), TyKind::Char))
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
    pub(super) fn store_tag_dyn(
        &mut self,
        p: PointerValue<'ctx>,
        ty: Ty,
        k: IntValue<'ctx>,
    ) -> R<()> {
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
    pub(super) fn checked_elem(
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
        el: Ty,
    ) -> R<()> {
        let dty = self.place_type(dest)?;
        let ok = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, n, "")
            .map_err(e)?;
        let d = self.place_ptr(dest)?;
        let (some, none) = self.option_variants(dty)?;
        self.store_tag(d, dty, none)?;
        self.if_then(ok, |cx| {
            let at = cx.elem_ptr(p, i, size)?;
            cx.store_tag(d, dty, some)?;
            cx.store_found(d, dty, some, at, el)
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

    /// Sorts the `n` elements of `el` (`size` bytes each) at `buf` in
    /// their own order: integers, `char`s and `bool`s straight through
    /// the runtime's slice sort, anything else by [`Self::sort_cmp`].
    fn sort_own_order(
        &mut self,
        buf: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        el: Ty,
        size: u64,
    ) -> R<()> {
        let signed = match self.tys.kind(el) {
            TyKind::Int(IntTy::I128 | IntTy::U128) => None,
            TyKind::Int(i) => Some(i.signed()),
            TyKind::Char | TyKind::Bool => Some(false),
            _ => None,
        };
        match signed {
            Some(signed) if self.user_cmp_body(el).is_none() && matches!(size, 1 | 2 | 4 | 8) => {
                let kind = size * 8 + if signed { 256 } else { 0 };
                self.rt("sort_int", &[buf.into(), n.into(), self.i64(kind).into()])
            }
            _ => {
                let cmp = self.sort_cmp(el);
                self.rt(
                    "sort",
                    &[buf.into(), n.into(), self.i64(size).into(), cmp.into()],
                )
            }
        }
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
                self.store_ref(d, at, el)?;
            }
            ("get", [i]) => {
                let i = self.int_arg(i)?;
                self.some_elem(dest, p, n, i, size, el)?;
            }
            ("first", []) => self.some_elem(dest, p, n, self.i64(0), size, el)?,
            ("last", rest) => {
                let k = match rest {
                    [k] => self.int_arg(k)?,
                    _ => self.i64(0),
                };
                let last = self.builder.build_int_sub(n, self.i64(1), "").map_err(e)?;
                let i = self.builder.build_int_sub(last, k, "").map_err(e)?;
                self.some_elem(dest, p, n, i, size, el)?;
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
            ("split_at" | "split_at_mut", [m]) => {
                let m = self.int_arg(m)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, m, n, "")
                    .map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                let d = self.place_ptr(dest)?;
                let rest = self.builder.build_int_sub(n, m, "").map_err(e)?;
                let mid = self.elem_ptr(p, m, size)?;
                for (f, (at, len)) in [(p, m), (mid, rest)].into_iter().enumerate() {
                    let q = self.offset(d, self.field_offset(dty, None, f as u32)?)?;
                    self.store(q, at.into())?;
                    self.store(self.offset(q, 8)?, len.into())?;
                }
            }
            ("fill", [v]) => {
                // Each element drops and becomes a clone of the value, which
                // drops once every slot holds its own.
                let mut vt = self.operand_type(v)?;
                let mut vp = self.operand_ptr(v)?;
                while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(vt) {
                    vp = self.load_ptr(vp)?;
                    vt = i;
                }
                let drops = self.tys.needs_drop(el);
                self.count_loop(n, |cx, i| {
                    let x = cx.elem_ptr(p, i, size)?;
                    if drops {
                        cx.call_glue(x, el)?;
                    }
                    cx.clone_into(x, vp, el)
                })?;
                self.drop_moved(&args[1..])?;
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
            ("sort" | "sort_unstable", []) => self.sort_own_order(p, n, el, size)?,
            (
                "sort_by"
                | "sort_unstable_by"
                | "sort_by_key"
                | "sort_unstable_by_key"
                | "sort_by_cached_key",
                [f],
            ) => {
                let by_key = method.contains("key");
                let (cmp, ctx) = self.sort_by_args(f, by_key, el)?;
                self.rt(
                    "sort_ctx",
                    &[
                        p.into(),
                        n.into(),
                        self.i64(size).into(),
                        cmp.into(),
                        ctx.into(),
                    ],
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
            ("extend" | "append", [other]) if is_vec => {
                // The other `Vec`'s elements move over and it is left empty;
                // passed by value it is then dropped (freeing its buffer).
                let mut ot = self.operand_type(other)?;
                let mut q = self.operand_ptr(other)?;
                while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ot) {
                    q = self.load_ptr(q)?;
                    ot = i;
                }
                if !matches!(self.tys.kind(ot), TyKind::Intrinsic(IntrinsicTy::Vec(_))) {
                    return Ok(None);
                }
                let op = self.load_ptr(q)?;
                let on = self.load_i64(self.offset(q, 8)?)?;
                let v = self.ref_arg(recv)?;
                self.rt("vec_reserve", &[v.into(), on.into(), self.i64(size).into()])?;
                let buf = self.load_ptr(v)?;
                let end = self.elem_ptr(buf, n, size)?;
                let bytes = self
                    .builder
                    .build_int_mul(on, self.i64(size), "")
                    .map_err(e)?;
                self.builder.build_memcpy(end, 1, op, 1, bytes).map_err(e)?;
                let m = self.builder.build_int_add(n, on, "").map_err(e)?;
                self.store(self.offset(v, 8)?, m.into())?;
                self.store(self.offset(q, 8)?, self.i64(0).into())?;
                self.drop_moved(&args[1..])?;
            }
            ("chunks" | "windows", [k]) => {
                // Views into the receiver: back to back with the last one
                // shorter, or every run of `k` in a row.
                let k = self.int_arg(k)?;
                let pos = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, k, self.i64(0), "")
                    .map_err(e)?;
                self.abort_unless(pos, "explicit panic")?;
                let b = &self.builder;
                let count = if method == "chunks" {
                    let m = b.build_int_add(n, k, "").map_err(e)?;
                    let m = b.build_int_sub(m, self.i64(1), "").map_err(e)?;
                    b.build_int_unsigned_div(m, k, "").map_err(e)?
                } else {
                    let enough = b
                        .build_int_compare(IntPredicate::UGE, n, k, "")
                        .map_err(e)?;
                    let c = b.build_int_sub(n, k, "").map_err(e)?;
                    let c = b.build_int_add(c, self.i64(1), "").map_err(e)?;
                    b.build_select(enough, c, self.i64(0), "")
                        .map_err(e)?
                        .into_int_value()
                };
                let d = self.place_ptr(dest)?;
                self.rt("vec_alloc", &[d.into(), count.into(), self.i64(16).into()])?;
                let buf = self.load_ptr(d)?;
                let chunks = method == "chunks";
                self.count_loop(count, |cx, i| {
                    let b = &cx.builder;
                    let at = if chunks {
                        b.build_int_mul(i, k, "").map_err(e)?
                    } else {
                        i
                    };
                    let left = b.build_int_sub(n, at, "").map_err(e)?;
                    let short = b
                        .build_int_compare(IntPredicate::ULT, left, k, "")
                        .map_err(e)?;
                    let len = b
                        .build_select(short, left, k, "")
                        .map_err(e)?
                        .into_int_value();
                    let start = cx.elem_ptr(p, at, size)?;
                    let slot = cx.elem_ptr(buf, i, 16)?;
                    cx.store(slot, start.into())?;
                    cx.store(cx.offset(slot, 8)?, len.into())
                })?;
            }
            ("binary_search", [x]) => {
                let kp = self.key_arg_of(x, el)?;
                let cmp = self.key_fn(KeyOp::Cmp, el);
                let i = self.rt_int(
                    "bsearch",
                    &[
                        p.into(),
                        n.into(),
                        self.i64(size).into(),
                        cmp.as_global_value().as_pointer_value().into(),
                        kp.into(),
                    ],
                    64,
                )?;
                self.store_opt_int(dest, i)?;
                self.drop_moved(&args[1..])?;
            }
            ("sorted" | "sorted_by" | "sorted_by_key", rest) if is_vec => {
                // A sorted copy: a receiver moved in is sorted where it
                // lands; a borrowed one is cloned first.
                let d = self.place_ptr(dest)?;
                if matches!(recv, Operand::Move(_))
                    && !matches!(self.tys.kind(rt), TyKind::Ref(_) | TyKind::MutRef(_))
                {
                    let q = self.operand_ptr(recv)?;
                    self.copy(d, q, rt)?;
                } else {
                    self.vec_of_clones(dest, p, n, el)?;
                }
                let buf = self.load_ptr(d)?;
                match rest {
                    [] => self.sort_own_order(buf, n, el, size)?,
                    [f] => {
                        let (cmp, ctx) = self.sort_by_args(f, method == "sorted_by_key", el)?;
                        self.rt(
                            "sort_ctx",
                            &[
                                buf.into(),
                                n.into(),
                                self.i64(size).into(),
                                cmp.into(),
                                ctx.into(),
                            ],
                        )?;
                    }
                    _ => return Ok(None),
                }
                self.drop_moved(rest)?;
            }
            ("resize", [k, val]) if is_vec => {
                // Cut-off elements drop in order; each new slot holds a
                // clone of the value but the last, which takes it.
                let k = self.int_arg(k)?;
                let neg = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, k, self.i64(0), "")
                    .map_err(e)?;
                let want = self
                    .builder
                    .build_select(neg, self.i64(0), k, "")
                    .map_err(e)?
                    .into_int_value();
                let less = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, want, n, "")
                    .map_err(e)?;
                let from = self
                    .builder
                    .build_select(less, want, n, "")
                    .map_err(e)?
                    .into_int_value();
                if self.tys.needs_drop(el) {
                    let cut = self.builder.build_int_sub(n, from, "").map_err(e)?;
                    self.count_loop(cut, |cx, i| {
                        let j = cx.builder.build_int_add(from, i, "").map_err(e)?;
                        let x = cx.elem_ptr(p, j, size)?;
                        cx.call_glue(x, el)
                    })?;
                }
                let grow = self.builder.build_int_sub(want, from, "").map_err(e)?;
                let v = self.ref_arg(recv)?;
                self.rt(
                    "vec_reserve",
                    &[v.into(), grow.into(), self.i64(size).into()],
                )?;
                let buf = self.load_ptr(v)?;
                let vp = self.operand_ptr(val)?;
                let moved = matches!(val, Operand::Move(_)) && self.tys.needs_drop(el);
                let clones = if moved {
                    let some = self
                        .builder
                        .build_int_compare(IntPredicate::SGT, grow, self.i64(0), "")
                        .map_err(e)?;
                    let less1 = self
                        .builder
                        .build_int_sub(grow, self.i64(1), "")
                        .map_err(e)?;
                    self.builder
                        .build_select(some, less1, self.i64(0), "")
                        .map_err(e)?
                        .into_int_value()
                } else {
                    grow
                };
                self.count_loop(clones, |cx, i| {
                    let j = cx.builder.build_int_add(from, i, "").map_err(e)?;
                    let x = cx.elem_ptr(buf, j, size)?;
                    cx.clone_into(x, vp, el)
                })?;
                if moved {
                    let none = self.is_zero(grow)?;
                    let last = self.builder.build_int_add(from, clones, "").map_err(e)?;
                    let at = self.elem_ptr(buf, last, size)?;
                    let f = self.cur_fn()?;
                    let (bt, bf, done) = (
                        self.llcx.append_basic_block(f, "resize.drop"),
                        self.llcx.append_basic_block(f, "resize.take"),
                        self.llcx.append_basic_block(f, "resize.done"),
                    );
                    self.builder
                        .build_conditional_branch(none, bt, bf)
                        .map_err(e)?;
                    self.builder.position_at_end(bt);
                    self.call_glue(vp, el)?;
                    self.builder.build_unconditional_branch(done).map_err(e)?;
                    self.builder.position_at_end(bf);
                    self.copy(at, vp, el)?;
                    self.builder.build_unconditional_branch(done).map_err(e)?;
                    self.builder.position_at_end(done);
                }
                self.store(self.offset(v, 8)?, want.into())?;
            }
            ("split_off", [at]) if is_vec => {
                let at = self.int_arg(at)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULE, at, n, "")
                    .map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                let tail = self.builder.build_int_sub(n, at, "").map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.rt("vec_alloc", &[d.into(), tail.into(), self.i64(size).into()])?;
                let nb = self.load_ptr(d)?;
                let from = self.elem_ptr(p, at, size)?;
                let bytes = self
                    .builder
                    .build_int_mul(tail, self.i64(size), "")
                    .map_err(e)?;
                self.builder
                    .build_memcpy(nb, 1, from, 1, bytes)
                    .map_err(e)?;
                let v = self.ref_arg(recv)?;
                self.store(self.offset(v, 8)?, at.into())?;
            }
            ("retain", [f]) if is_vec => {
                // Each rejected element drops right away, in order; the
                // kept ones close up behind them.
                let c = self.callee_by_ref(f)?;
                let w = self.entry_slot(8, "retain.w")?;
                self.store(w, self.i64(0).into())?;
                let el_ref = self.tys.intern(TyKind::Ref(el));
                let arg = self.entry_slot(8, "retain.arg")?;
                let keep = self.entry_slot(1, "retain.keep")?;
                self.count_loop(n, |cx, i| {
                    let x = cx.elem_ptr(p, i, size)?;
                    cx.store(arg, x.into())?;
                    cx.call_callee(&c, keep, &[(arg, el_ref)])?;
                    let k = cx
                        .builder
                        .build_load(cx.llcx.i8_type(), keep, "")
                        .map_err(e)?
                        .into_int_value();
                    let k = cx
                        .builder
                        .build_int_compare(IntPredicate::NE, k, k.get_type().const_zero(), "")
                        .map_err(e)?;
                    let f = cx.cur_fn()?;
                    let (kb, db, next) = (
                        cx.llcx.append_basic_block(f, "retain.keep"),
                        cx.llcx.append_basic_block(f, "retain.drop"),
                        cx.llcx.append_basic_block(f, "retain.next"),
                    );
                    cx.builder.build_conditional_branch(k, kb, db).map_err(e)?;
                    cx.builder.position_at_end(kb);
                    let wi = cx.load_i64(w)?;
                    let y = cx.elem_ptr(p, wi, size)?;
                    let bytes = cx.i64(size);
                    cx.builder.build_memmove(y, 1, x, 1, bytes).map_err(e)?;
                    let wn = cx.builder.build_int_add(wi, cx.i64(1), "").map_err(e)?;
                    cx.store(w, wn.into())?;
                    cx.builder.build_unconditional_branch(next).map_err(e)?;
                    cx.builder.position_at_end(db);
                    if cx.tys.needs_drop(el) {
                        cx.call_glue(x, el)?;
                    }
                    cx.builder.build_unconditional_branch(next).map_err(e)?;
                    cx.builder.position_at_end(next);
                    Ok(())
                })?;
                let v = self.ref_arg(recv)?;
                let wn = self.load_i64(w)?;
                self.store(self.offset(v, 8)?, wn.into())?;
                if !Self::consumes(&c) {
                    self.drop_moved(&args[1..])?;
                }
            }
            ("truncate", [k]) if is_vec => {
                // The tail drops first to last, as a dying Vec's does.
                let k = self.int_arg(k)?;
                let neg = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, k, self.i64(0), "")
                    .map_err(e)?;
                let k = self
                    .builder
                    .build_select(neg, self.i64(0), k, "")
                    .map_err(e)?
                    .into_int_value();
                let over = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, k, n, "")
                    .map_err(e)?;
                let keep = self
                    .builder
                    .build_select(over, n, k, "")
                    .map_err(e)?
                    .into_int_value();
                if self.tys.needs_drop(el) {
                    let cut = self.builder.build_int_sub(n, keep, "").map_err(e)?;
                    self.count_loop(cut, |cx, i| {
                        let j = cx.builder.build_int_add(keep, i, "").map_err(e)?;
                        let x = cx.elem_ptr(p, j, size)?;
                        cx.call_glue(x, el)
                    })?;
                }
                let v = self.ref_arg(recv)?;
                self.store(self.offset(v, 8)?, keep.into())?;
            }
            ("reserve" | "reserve_exact", [k]) if is_vec => {
                let k = self.int_arg(k)?;
                let pos = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, k, self.i64(0), "")
                    .map_err(e)?;
                let v = self.ref_arg(recv)?;
                self.if_then(pos, |cx| {
                    cx.rt("vec_reserve", &[v.into(), k.into(), cx.i64(size).into()])
                })?;
            }
            ("try_reserve" | "try_reserve_exact", [k]) if is_vec => {
                // Allocation failure aborts, so the request always succeeds.
                let k = self.int_arg(k)?;
                let pos = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, k, self.i64(0), "")
                    .map_err(e)?;
                let v = self.ref_arg(recv)?;
                self.if_then(pos, |cx| {
                    cx.rt("vec_reserve", &[v.into(), k.into(), cx.i64(size).into()])
                })?;
                let ok = self.variant_of(dty, "Ok")?;
                let d = self.place_ptr(dest)?;
                self.store_tag(d, dty, ok)?;
            }
            ("capacity", []) if is_vec => {
                // At least the length: a borrowed literal has capacity 0.
                let v = self.ref_arg(recv)?;
                let cap = self.load_i64(self.offset(v, 16)?)?;
                let more = self
                    .builder
                    .build_int_compare(IntPredicate::UGT, cap, n, "")
                    .map_err(e)?;
                let c = self
                    .builder
                    .build_select(more, cap, n, "")
                    .map_err(e)?
                    .into_int_value();
                self.store_int_as(dest, c, false)?;
            }
            ("shrink_to_fit", []) if is_vec => {}
            ("is_sorted", []) => {
                let cmp = self.key_fn(KeyOp::Cmp, el);
                let res = self.entry_slot(1, "sorted")?;
                self.store(res, self.llcx.i8_type().const_int(1, false).into())?;
                let pairs = self.builder.build_int_sub(n, self.i64(1), "").map_err(e)?;
                let some = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, pairs, self.i64(0), "")
                    .map_err(e)?;
                let pairs = self
                    .builder
                    .build_select(some, pairs, self.i64(0), "")
                    .map_err(e)?
                    .into_int_value();
                self.count_loop(pairs, |cx, i| {
                    let x = cx.elem_ptr(p, i, size)?;
                    let y = cx.offset(x, size)?;
                    let o = cx
                        .builder
                        .build_call(cmp, &[x.into(), y.into()], "")
                        .map_err(e)?
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    let gt = cx
                        .builder
                        .build_int_compare(IntPredicate::SGT, o, o.get_type().const_zero(), "")
                        .map_err(e)?;
                    cx.if_then(gt, |cx| {
                        cx.store(res, cx.llcx.i8_type().const_zero().into())
                    })
                })?;
                let d = self.place_ptr(dest)?;
                let v = self
                    .builder
                    .build_load(self.llcx.i8_type(), res, "")
                    .map_err(e)?;
                self.store(d, v)?;
            }
            ("dedup", []) if is_vec => {
                // A run of equal elements keeps its first; the rest drop in
                // order.
                let eq = self.key_fn(KeyOp::Eq, el);
                let w = self.entry_slot(8, "kept")?;
                self.store(w, self.i64(0).into())?;
                self.count_loop(n, |cx, i| {
                    let k = cx.load_i64(w)?;
                    let x = cx.elem_ptr(p, i, size)?;
                    let first = cx.is_zero(k)?;
                    let dup = cx.entry_slot(1, "dup")?;
                    cx.store(dup, cx.llcx.i8_type().const_zero().into())?;
                    let not_first = cx.builder.build_not(first, "").map_err(e)?;
                    cx.if_then(not_first, |cx| {
                        let km = cx.builder.build_int_sub(k, cx.i64(1), "").map_err(e)?;
                        let last = cx.elem_ptr(p, km, size)?;
                        let same = cx
                            .builder
                            .build_call(eq, &[last.into(), x.into()], "")
                            .map_err(e)?
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value();
                        let same = cx
                            .builder
                            .build_int_z_extend(same, cx.llcx.i8_type(), "")
                            .map_err(e)?;
                        cx.store(dup, same.into())
                    })?;
                    let d = cx
                        .builder
                        .build_load(cx.llcx.i8_type(), dup, "")
                        .map_err(e)?
                        .into_int_value();
                    let is_dup = cx
                        .builder
                        .build_int_compare(IntPredicate::NE, d, d.get_type().const_zero(), "")
                        .map_err(e)?;
                    let f = cx.cur_fn()?;
                    let drop_bb = cx.llcx.append_basic_block(f, "dedup.drop");
                    let keep_bb = cx.llcx.append_basic_block(f, "dedup.keep");
                    let next = cx.llcx.append_basic_block(f, "dedup.next");
                    cx.builder
                        .build_conditional_branch(is_dup, drop_bb, keep_bb)
                        .map_err(e)?;
                    cx.builder.position_at_end(drop_bb);
                    if cx.tys.needs_drop(el) {
                        cx.call_glue(x, el)?;
                    }
                    cx.builder.build_unconditional_branch(next).map_err(e)?;
                    cx.builder.position_at_end(keep_bb);
                    let at = cx.elem_ptr(p, k, size)?;
                    cx.copy(at, x, el)?;
                    let k1 = cx.builder.build_int_add(k, cx.i64(1), "").map_err(e)?;
                    cx.store(w, k1.into())?;
                    cx.builder.build_unconditional_branch(next).map_err(e)?;
                    cx.builder.position_at_end(next);
                    Ok(())
                })?;
                let v = self.ref_arg(recv)?;
                let k = self.load_i64(w)?;
                self.store(self.offset(v, 8)?, k.into())?;
            }
            ("get_unchecked", [i]) => {
                let i = self.int_arg(i)?;
                let at = self.checked_elem(p, n, i, size)?;
                let d = self.place_ptr(dest)?;
                if matches!(self.tys.kind(dty), TyKind::Ref(_) | TyKind::MutRef(_)) {
                    self.store_ref(d, at, el)?;
                } else {
                    self.clone_into(d, at, dty)?;
                }
            }
            _ => return Ok(None),
        }
        let _ = base;
        Ok(Some(()))
    }

    /// `Vec.from_fn(n, f)`: a `Vec` of `f(0)`, ..., `f(n - 1)`.
    pub(super) fn vec_from_fn(&mut self, args: &[Operand], dest: &Place) -> R<()> {
        let dty = self.place_type(dest)?;
        let TyKind::Intrinsic(IntrinsicTy::Vec(el)) = self.tys.kind(dty) else {
            return Err("Vec.from_fn builds a non-Vec".into());
        };
        let size = self.layout(el)?.size;
        let k = self.int_arg(&args[0])?;
        let neg = self
            .builder
            .build_int_compare(IntPredicate::SLT, k, self.i64(0), "")
            .map_err(e)?;
        let n = self
            .builder
            .build_select(neg, self.i64(0), k, "")
            .map_err(e)?
            .into_int_value();
        let c = self.callee_by_ref(&args[1])?;
        let d = self.place_ptr(dest)?;
        self.rt("vec_alloc", &[d.into(), n.into(), self.i64(size).into()])?;
        let buf = self.load_ptr(d)?;
        let i64t = self.tys.intern(TyKind::Int(crate::mir::IntTy::I64));
        let arg = self.entry_slot(8, "from_fn.i")?;
        // The closure's result as it returns it: an integer literal body
        // (`|i| 7`) returns an `i64` for a `Vec[i32]`, cast into the slot.
        let rt = self.callee_ret(&c)?;
        let cast = rt != el && self.widens(rt, el);
        let tmp = if cast {
            Some(self.temp(rt, "from_fn.x")?)
        } else {
            None
        };
        self.count_loop(n, |cx, i| {
            cx.store(arg, i.into())?;
            let x = cx.elem_ptr(buf, i, size)?;
            match tmp {
                Some(t) => {
                    cx.call_callee(&c, t, &[(arg, i64t)])?;
                    cx.widen_into(x, t, rt, el)
                }
                None => cx.call_callee(&c, x, &[(arg, i64t)]),
            }
        })?;
        if !Self::consumes(&c) {
            self.drop_moved(&args[1..])?;
        }
        Ok(())
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
    pub(super) fn float_intrinsic(
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
        if self.narrow_float(t).is_some() {
            return Ok(None);
        }
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            let at = self.operand_type(a)?;
            if at == t {
                vals.push(self.operand_scalar(a)?);
                continue;
            }
            // An unsuffixed literal argument typed `i64` or `f64`
            // (`x.wrapping_add(1)` on an `i32`) is the receiver's number.
            match (self.tys.kind(at), self.tys.kind(t), a) {
                (TyKind::Int(from), TyKind::Int(_), Operand::Const(_)) => {
                    let v = self.operand_scalar(a)?.into_int_value();
                    let to = vals[0].into_int_value().get_type();
                    let v = self
                        .builder
                        .build_int_cast_sign_flag(v, to, from.signed(), "")
                        .map_err(e)?;
                    vals.push(v.into());
                }
                (TyKind::Float(_), TyKind::Float(_), Operand::Const(_)) => {
                    let v = self.operand_scalar(a)?.into_float_value();
                    let to = vals[0].into_float_value().get_type();
                    let v = self.builder.build_float_cast(v, to, "").map_err(e)?;
                    vals.push(v.into());
                }
                (TyKind::Int(from), TyKind::Float(_), Operand::Const(_)) => {
                    let v = self.operand_scalar(a)?.into_int_value();
                    let to = vals[0].into_float_value().get_type();
                    let v = if from.signed() {
                        self.builder.build_signed_int_to_float(v, to, "")
                    } else {
                        self.builder.build_unsigned_int_to_float(v, to, "")
                    }
                    .map_err(e)?;
                    vals.push(v.into());
                }
                _ => return Ok(None),
            }
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
                b.build_int_cast_sign_flag(n, it, false, "")
                    .map_err(e)?
                    .into()
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
