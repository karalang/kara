//! The library cells whose methods are lowered here: `Atomic[T]`, the
//! `T` held in place (`src/mir/layout.rs`), and `OnceLock` / `OnceCell`,
//! a handle on the runtime's write-once cell (`karac_runtime_once_*`).
//! Their reference semantics are the interpreter's `atomic_method` and
//! `once_method` (`src/mir/interp.rs`). One task runs at a time on this
//! backend, so an atomic's operations are plain loads and stores.

use inkwell::values::{IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::layout::{library_cell, Cell};
use crate::mir::ty::IntTy;
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// The cell an operand is or points at, and where it is.
    fn cell_arg(&mut self, a: &Operand) -> R<(PointerValue<'ctx>, Ty)> {
        let mut t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            p = self.load_ptr(p)?;
            t = i;
        }
        Ok((p, t))
    }

    /// The methods of `Atomic[T]`, or `None` for one not lowered here.
    pub(super) fn atomic_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        if method == "new" {
            let d = self.place_ptr(dest)?;
            self.store_operand(d, &args[0])?;
            return Ok(Some(()));
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let (cell, ct) = self.cell_arg(recv)?;
        let Some(Cell::Inline(t)) = library_cell(self.tys, ct) else {
            return Ok(None);
        };
        match (method, &args[1..]) {
            ("load" | "into_inner", [..]) => {
                let d = self.place_ptr(dest)?;
                self.copy(d, cell, t)?;
            }
            ("get_mut", []) => {
                let d = self.place_ptr(dest)?;
                self.store(d, cell.into())?;
            }
            ("store", [v, ..]) => self.store_operand(cell, v)?,
            ("swap", [v, ..]) => {
                let d = self.place_ptr(dest)?;
                self.copy(d, cell, t)?;
                self.store_operand(cell, v)?;
            }
            ("compare_exchange" | "compare_exchange_weak", [x, v, ..]) => {
                let old = self.load(cell, t)?.into_int_value();
                let want = self.operand_scalar(x)?.into_int_value();
                let same = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, old, want, "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                let (ok, er) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
                self.store_tag(d, dty, er)?;
                let off = self.field_offset(dty, Some(er), 0)?;
                self.store(self.offset(d, off)?, old.into())?;
                let ok_off = self.field_offset(dty, Some(ok), 0)?;
                self.if_then(same, |cx| {
                    cx.store_tag(d, dty, ok)?;
                    cx.store(cx.offset(d, ok_off)?, old.into())?;
                    cx.store_operand(cell, v)
                })?;
            }
            (m, [v, ..]) if m.starts_with("fetch_") => {
                let old = self.load(cell, t)?.into_int_value();
                let rhs = self.operand_scalar(v)?.into_int_value();
                let signed = matches!(
                    self.tys.kind(t),
                    TyKind::Int(
                        IntTy::I8
                            | IntTy::I16
                            | IntTy::I32
                            | IntTy::I64
                            | IntTy::I128
                            | IntTy::Isize
                    )
                );
                let Some(new) = self.fetch_op(&m["fetch_".len()..], old, rhs, signed)? else {
                    return Ok(None);
                };
                let new = if matches!(self.tys.kind(t), TyKind::Bool) {
                    let one = new.get_type().const_int(1, false);
                    self.builder.build_and(new, one, "").map_err(e)?
                } else {
                    new
                };
                let d = self.place_ptr(dest)?;
                self.store(d, old.into())?;
                self.store(cell, new.into())?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    fn fetch_op(
        &self,
        op: &str,
        a: IntValue<'ctx>,
        b: IntValue<'ctx>,
        signed: bool,
    ) -> R<Option<IntValue<'ctx>>> {
        let bl = &self.builder;
        let v = match op {
            "add" => bl.build_int_add(a, b, ""),
            "sub" => bl.build_int_sub(a, b, ""),
            "and" => bl.build_and(a, b, ""),
            "or" => bl.build_or(a, b, ""),
            "xor" => bl.build_xor(a, b, ""),
            "nand" => {
                let x = bl.build_and(a, b, "").map_err(e)?;
                bl.build_not(x, "")
            }
            "max" | "min" => {
                let pred = match (op == "max", signed) {
                    (true, true) => IntPredicate::SGT,
                    (true, false) => IntPredicate::UGT,
                    (false, true) => IntPredicate::SLT,
                    (false, false) => IntPredicate::ULT,
                };
                let c = bl.build_int_compare(pred, a, b, "").map_err(e)?;
                return Ok(Some(
                    bl.build_select(c, a, b, "").map_err(e)?.into_int_value(),
                ));
            }
            _ => return Ok(None),
        };
        Ok(Some(v.map_err(e)?))
    }

    /// The methods of `OnceLock[T]` / `OnceCell[T]`, or `None` for one not
    /// lowered here.
    pub(super) fn once_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        if method == "new" {
            let h = self.mrt_ptr_named("karac_runtime_once_new", &[])?;
            let d = self.place_ptr(dest)?;
            self.store(d, h.into())?;
            return Ok(Some(()));
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let (cell, ct) = self.cell_arg(recv)?;
        let Some(t) = self
            .tys
            .tcx()
            .adt_of(ct)
            .and_then(|(_, a)| a.first().copied())
        else {
            return Ok(None);
        };
        let size = self.layout(t)?.size;
        let h = self.load_ptr(cell)?;
        match (method, &args[1..]) {
            ("is_set", []) => {
                let s = self.rt_int_named("karac_runtime_once_is_set", &[h.into()], 8)?;
                let s = self
                    .builder
                    .build_int_compare(IntPredicate::NE, s, s.get_type().const_zero(), "")
                    .map_err(e)?;
                self.store_bool(dest, s)?;
            }
            ("get", []) => {
                let p = self.mrt_ptr_named("karac_runtime_once_get", &[h.into()])?;
                let d = self.place_ptr(dest)?;
                let (some, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                let set = self.builder.build_is_not_null(p, "").map_err(e)?;
                self.if_then(set, |cx| {
                    cx.store_tag(d, dty, some)?;
                    cx.store_found(d, dty, some, p, t)
                })?;
            }
            ("set", [v]) => {
                let vp = self.operand_ptr(v)?;
                let won = self.rt_int_named(
                    "karac_runtime_once_set",
                    &[h.into(), vp.into(), self.i64(size).into()],
                    8,
                )?;
                let won = self
                    .builder
                    .build_int_compare(IntPredicate::NE, won, won.get_type().const_zero(), "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                let (ok, er) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
                self.store_tag(d, dty, ok)?;
                let off = self.field_offset(dty, Some(er), 0)?;
                let lost = self.builder.build_not(won, "").map_err(e)?;
                self.if_then(lost, |cx| {
                    cx.store_tag(d, dty, er)?;
                    cx.copy(cx.offset(d, off)?, vp, t)
                })?;
            }
            ("get_or_init", [f]) => {
                let p = self.mrt_ptr_named("karac_runtime_once_get", &[h.into()])?;
                let unset = self.builder.build_is_null(p, "").map_err(e)?;
                let c = self.callee_of(f)?;
                self.if_then(unset, |cx| {
                    let tmp = cx.temp(t, "once")?;
                    cx.call_callee(&c, tmp, &[])?;
                    cx.rt_int_named(
                        "karac_runtime_once_set",
                        &[h.into(), tmp.into(), cx.i64(size).into()],
                        8,
                    )?;
                    Ok(())
                })?;
                // The call disposes of a consumed closure; otherwise, or
                // when the cell was already set, the moved closure drops here.
                let f1 = std::slice::from_ref(f);
                if Self::consumes(&c) {
                    let set = self.builder.build_not(unset, "").map_err(e)?;
                    self.if_then(set, |cx| cx.drop_moved(f1))?;
                } else {
                    self.drop_moved(f1)?;
                }
                let p = self.mrt_ptr_named("karac_runtime_once_get", &[h.into()])?;
                let d = self.place_ptr(dest)?;
                self.store(d, p.into())?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// `FileSystem.write` / `read_to_string` / `read_lines` through
    /// `karac_mir_fs`, as the interpreter's `fs_method`: `Ok` of nothing,
    /// the text, or its lines; `Err` of the failure's `IoError`.
    pub(super) fn fs_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let op = match method {
            "write" => 0,
            "read_to_string" => 1,
            "read_lines" => 2,
            _ => return Ok(None),
        };
        let dty = self.place_type(dest)?;
        let (pp, pn) = self.str_parts(&args[0])?;
        let (tp, tn) = match args.get(1) {
            Some(t) => self.str_parts(t)?,
            None => (self.ptr_ty().const_null(), self.i64(0)),
        };
        let out = self.temp(
            self.tys
                .intern(TyKind::Intrinsic(crate::mir::ty::IntrinsicTy::String)),
            "fs",
        )?;
        let i32t = self.llcx.i32_type();
        let code = self.rt_int(
            "fs",
            &[
                i32t.const_int(op, false).into(),
                pp.into(),
                pn.into(),
                tp.into(),
                tn.into(),
                out.into(),
            ],
            32,
        )?;
        self.drop_moved(args)?;
        let d = self.place_ptr(dest)?;
        let (ok, er) = (self.variant_of(dty, "Ok")?, self.variant_of(dty, "Err")?);
        let okt = self
            .tys
            .field_ty(dty, Some(ok), 0)
            .ok_or("Result.Ok has no payload")?;
        let et = self
            .tys
            .field_ty(dty, Some(er), 0)
            .ok_or("Result.Err has no payload")?;
        let ok_off = self.field_offset(dty, Some(ok), 0)?;
        let ep = self.offset(d, self.field_offset(dty, Some(er), 0)?)?;
        let done = self
            .builder
            .build_int_compare(IntPredicate::SLT, code, i32t.const_zero(), "")
            .map_err(e)?;
        self.store_tag(d, dty, er)?;
        self.if_then(done, |cx| {
            cx.store_tag(d, dty, ok)?;
            if op != 0 {
                cx.copy(cx.offset(d, ok_off)?, out, okt)?;
            }
            Ok(())
        })?;
        let names = [
            "NotFound",
            "PermissionDenied",
            "AlreadyExists",
            "UnexpectedEof",
            "InvalidUtf8",
            "Interrupted",
            "Other",
        ];
        for (k, name) in names.iter().enumerate() {
            let v = self.variant_of(et, name)?;
            let is = self
                .builder
                .build_int_compare(IntPredicate::EQ, code, i32t.const_int(k as u64, false), "")
                .map_err(e)?;
            let other = *name == "Other";
            let off = if other {
                self.field_offset(et, Some(v), 0)?
            } else {
                0
            };
            let st = out;
            self.if_then(is, |cx| {
                cx.store_tag(ep, et, v)?;
                if other {
                    let s = cx
                        .tys
                        .intern(TyKind::Intrinsic(crate::mir::ty::IntrinsicTy::String));
                    cx.copy(cx.offset(ep, off)?, st, s)?;
                }
                Ok(())
            })?;
        }
        Ok(Some(()))
    }

    /// The place an operand of a (possibly borrowed) value points at, and
    /// the value's type.
    pub(super) fn behind_refs(&mut self, a: &Operand) -> R<(PointerValue<'ctx>, Ty)> {
        let mut t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            p = self.load_ptr(p)?;
            t = i;
        }
        Ok((p, t))
    }

    /// `Arena[T]`: a handle to a `RawArena` (`runtime/src/mir.rs`), its
    /// items in a vector, and `ArenaRef` / `ArenaCheckpoint` values of the
    /// arena's id and an index, as the interpreter's `arena_method`.
    pub(super) fn arena_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        if method == "new" {
            let h = self.mrt_ptr("arena_new", &[])?;
            let d = self.place_ptr(dest)?;
            self.store(d, h.into())?;
            return Ok(Some(()));
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let (ap, at) = self.behind_refs(recv)?;
        let Some(el) = self
            .tys
            .tcx()
            .adt_of(at)
            .and_then(|(_, a)| a.first().copied())
        else {
            return Ok(None);
        };
        let size = self.layout(el)?.size;
        let h = self.load_ptr(ap)?;
        let len = self.load_i64(self.offset(h, 8)?)?;
        let id = self.load_i64(self.offset(h, 24)?)?;
        // The arena id and index of a handle or checkpoint argument.
        let pair = |cx: &mut Self, a: &Operand| -> R<(IntValue<'ctx>, IntValue<'ctx>)> {
            let (p, t) = cx.behind_refs(a)?;
            let o = cx.load_i64(cx.offset(p, cx.field_offset(t, None, 0)?)?)?;
            let i = cx.load_i64(cx.offset(p, cx.field_offset(t, None, 1)?)?)?;
            Ok((o, i))
        };
        let owned_in = |cx: &mut Self, o: IntValue<'ctx>, i: IntValue<'ctx>| -> R<IntValue<'ctx>> {
            let b = &cx.builder;
            let mine = b
                .build_int_compare(IntPredicate::EQ, o, id, "")
                .map_err(e)?;
            let inb = b
                .build_int_compare(IntPredicate::ULT, i, len, "")
                .map_err(e)?;
            b.build_and(mine, inb, "").map_err(e)
        };
        let store_pair = |cx: &mut Self, a: IntValue<'ctx>, b: IntValue<'ctx>| -> R<()> {
            let d = cx.place_ptr(dest)?;
            cx.store(cx.offset(d, cx.field_offset(dty, None, 0)?)?, a.into())?;
            cx.store(cx.offset(d, cx.field_offset(dty, None, 1)?)?, b.into())
        };
        match (method, &args[1..]) {
            ("len", []) => {
                let d = self.place_ptr(dest)?;
                let it = self.llvm_ty(dty)?.into_int_type();
                let n = self
                    .builder
                    .build_int_truncate_or_bit_cast(len, it, "")
                    .map_err(e)?;
                self.store(d, n.into())?;
            }
            ("push", [v]) => {
                let x = self.operand_ptr(v)?;
                self.rt("vec_push", &[h.into(), x.into(), self.i64(size).into()])?;
                store_pair(self, id, len)?;
            }
            ("get" | "get_mut", [r]) => {
                let (o, i) = pair(self, r)?;
                let ok = owned_in(self, o, i)?;
                self.abort_unless(ok, "index out of bounds")?;
                let buf = self.load_ptr(h)?;
                let at = self.elem_ptr(buf, i, size)?;
                let d = self.place_ptr(dest)?;
                self.store(d, at.into())?;
            }
            ("high_water_mark", []) => store_pair(self, id, len)?,
            ("rewind_to", [cp]) => {
                // A checkpoint of another arena is ignored; the items past
                // the mark drop, last pushed first.
                let (o, m) = pair(self, cp)?;
                let ok = owned_in(self, o, m)?;
                self.if_then(ok, |cx| {
                    cx.drop_back_to(h, m, len, el)?;
                    cx.store(cx.offset(h, 8)?, m.into())
                })?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// `Interner`: a handle to the runtime's `MirInterner`; a `Symbol` is
    /// its string's id.
    pub(super) fn interner_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        if method == "new" {
            let h = self.mrt_ptr("interner_new", &[])?;
            let d = self.place_ptr(dest)?;
            self.store(d, h.into())?;
            return Ok(Some(()));
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let (ip, _) = self.behind_refs(recv)?;
        let h = self.load_ptr(ip)?;
        match (method, &args[1..]) {
            ("intern", [s]) => {
                let (p, n) = self.str_parts(s)?;
                let id = self.mrt_i64("interner_intern", &[h.into(), p.into(), n.into()])?;
                self.drop_moved(&args[1..])?;
                let d = self.place_ptr(dest)?;
                self.store(d, id.into())?;
            }
            ("resolve", [sym]) => {
                let (sp, _) = self.behind_refs(sym)?;
                let id = self.load_i64(sp)?;
                let s = self.mrt_ptr("interner_resolve", &[h.into(), id.into()])?;
                let d = self.place_ptr(dest)?;
                self.store(d, s.into())?;
            }
            ("len", []) => {
                let n = self.mrt_i64("interner_len", &[h.into()])?;
                let it = self.llvm_ty(dty)?.into_int_type();
                let n = self
                    .builder
                    .build_int_truncate_or_bit_cast(n, it, "")
                    .map_err(e)?;
                let d = self.place_ptr(dest)?;
                self.store(d, n.into())?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    fn mrt_ptr_named(
        &self,
        name: &str,
        args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
    ) -> R<PointerValue<'ctx>> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(name, self.ptr_ty().fn_type(&tys, false));
        let v = self.builder.build_call(f, args, "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_pointer_value())
    }

    fn rt_int_named(
        &self,
        name: &str,
        args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
        bits: u32,
    ) -> R<IntValue<'ctx>> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(name, self.int_ty(bits).fn_type(&tys, false));
        let v = self.builder.build_call(f, args, "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }
}
