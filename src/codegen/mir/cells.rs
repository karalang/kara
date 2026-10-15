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

    /// `Semaphore` and `RateLimiter`, one-field handles naming the
    /// runtime's registries; `None` for a method not lowered.
    pub(super) fn limit_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        match (base, method, args) {
            ("Semaphore", "new", [n]) => {
                let n = self.int_arg(n)?;
                let id = self.mrt_i64("sem_new", &[n.into()])?;
                let d = self.place_ptr(dest)?;
                self.store(d, id.into())?;
            }
            ("RateLimiter", "new_token_bucket", [r, c]) => {
                let (r, c) = (self.int_arg(r)?, self.int_arg(c)?);
                let id = self.mrt_i64("limiter_new", &[r.into(), c.into()])?;
                let d = self.place_ptr(dest)?;
                self.store(d, id.into())?;
            }
            ("Semaphore" | "RateLimiter", "drop", _) => {}
            ("Semaphore", "acquire", [recv, ..]) => {
                let (cell, _) = self.cell_arg(recv)?;
                let id = self.load_i64(cell)?;
                let d = self.place_ptr(dest)?;
                let ok = self.rt_flag("sem_acquire", &[id.into()])?;
                self.store_result_flag(d, dty, ok, Some("Timeout"))?;
            }
            ("Semaphore", "release", [recv]) => {
                let (cell, _) = self.cell_arg(recv)?;
                let id = self.load_i64(cell)?;
                self.rt("sem_release", &[id.into()])?;
            }
            ("RateLimiter", "try_acquire", [recv, key]) => {
                let (cell, _) = self.cell_arg(recv)?;
                let id = self.load_i64(cell)?;
                let (p, n) = self.str_parts(key)?;
                let ok = self.rt_flag("limiter_try", &[id.into(), p.into(), n.into()])?;
                self.store_bool(dest, ok)?;
                self.drop_moved(&args[1..])?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// `CStr` and `CString` (design.md § C-String Literals), each a pointer
    /// to NUL-terminated bytes: a literal's in a private constant, a
    /// `CString`'s in a buffer it owns. `None` for a method not lowered.
    pub(super) fn cstr_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        match (base, method) {
            ("CStr", "from_bytes") => {
                let mut bytes = Vec::with_capacity(args.len() + 1);
                for a in args {
                    let v = self.operand_scalar(a)?.into_int_value();
                    let Some(b) = v.get_zero_extended_constant() else {
                        return Err("MIR to LLVM: a `CStr` of non-constant bytes".into());
                    };
                    bytes.push(b as u8);
                }
                bytes.push(0);
                let i8t = self.llcx.i8_type();
                let vals: Vec<_> = bytes
                    .iter()
                    .map(|b| i8t.const_int(u64::from(*b), false))
                    .collect();
                let arr = i8t.const_array(&vals);
                let g = self.module.add_global(arr.get_type(), None, "karac.cstr");
                g.set_initializer(&arr);
                g.set_constant(true);
                g.set_linkage(inkwell::module::Linkage::Private);
                let d = self.place_ptr(dest)?;
                self.store(d, g.as_pointer_value().into())?;
                return Ok(Some(()));
            }
            ("CStr", "from_ptr") => {
                // A `ref CStr`: a slot of this call's frame holding the
                // pointer.
                let p = self.operand_scalar(&args[0])?;
                let slot = self.entry_slot(8, "cstr")?;
                self.store(slot, p)?;
                let d = self.place_ptr(dest)?;
                self.store(d, slot.into())?;
                return Ok(Some(()));
            }
            ("String", "to_cstring") => {
                let (p, n) = self.str_parts(&args[0])?;
                let d = self.place_ptr(dest)?;
                let ok = self.variant_of(dty, "Ok")?;
                let q = self.offset(d, self.field_offset(dty, Some(ok), 0)?)?;
                let made = self.rt_flag("to_cstring", &[q.into(), p.into(), n.into()])?;
                self.store_result_flag(d, dty, made, Some("InteriorNul"))?;
                self.drop_moved(args)?;
                return Ok(Some(()));
            }
            ("CStr" | "CString", _) => {}
            _ => return Ok(None),
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let (cell, _) = self.cell_arg(recv)?;
        let h = self.load_ptr(cell)?;
        match method {
            "len" | "is_empty" => {
                let n = self.mrt_i64("cstr_len", &[h.into()])?;
                if method == "len" {
                    self.store_int_as(dest, n, false)?;
                } else {
                    let z = self.is_zero(n)?;
                    self.store_bool(dest, z)?;
                }
            }
            "as_ptr" => {
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
            }
            "as_bytes" => {
                let n = self.mrt_i64("cstr_len", &[h.into()])?;
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
                self.store(self.offset(d, 8)?, n.into())?;
            }
            "to_string" | "to_string_slice" if base == "CStr" => {
                let d = self.place_ptr(dest)?;
                let ok = self.variant_of(dty, "Ok")?;
                let q = self.offset(d, self.field_offset(dty, Some(ok), 0)?)?;
                let view = self
                    .llcx
                    .i8_type()
                    .const_int(u64::from(method == "to_string_slice"), false);
                let i32t = self.llcx.i32_type();
                let f = self.extern_fn(
                    "karac_mir_cstr_to_string",
                    i32t.fn_type(
                        &[
                            self.ptr_ty().into(),
                            self.ptr_ty().into(),
                            self.llcx.i8_type().into(),
                        ],
                        false,
                    ),
                );
                let r = self
                    .builder
                    .build_call(f, &[q.into(), h.into(), view.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let good = self.is_zero(r)?;
                let incomplete = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, r, i32t.const_int(1, false), "")
                    .map_err(e)?;
                let errv = self.variant_of(dty, "Err")?;
                let et = self
                    .tys
                    .field_ty(dty, Some(errv), 0)
                    .ok_or("an `Err` without a payload")?;
                let eq = self.offset(d, self.field_offset(dty, Some(errv), 0)?)?;
                let (inc, bad) = (
                    self.variant_of(et, "IncompleteSequence")?,
                    self.variant_of(et, "InvalidByte")?,
                );
                self.store_result_flag(d, dty, good, None)?;
                let not_good = self.builder.build_not(good, "").map_err(e)?;
                self.if_then(not_good, |cx| {
                    let k = cx
                        .builder
                        .build_select(
                            incomplete,
                            cx.i64(u64::from(inc)),
                            cx.i64(u64::from(bad)),
                            "",
                        )
                        .map_err(e)?
                        .into_int_value();
                    cx.store_tag_dyn(eq, et, k)
                })?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
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

    /// The `fs` functions through `karac_mir_fs`, as the interpreter's
    /// `fs_method`: `exists`' bool, or `Ok` of nothing, the text, its lines
    /// or a directory's names, `copy`'s count or `metadata`'s `Metadata`;
    /// `Err` of the failure's `IoError`.
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
            "exists" => 3,
            "metadata" => 4,
            "read_dir" => 5,
            "create_dir" => 6,
            "create_dir_all" => 7,
            "remove_file" => 8,
            "remove_dir" => 9,
            "remove_dir_all" => 10,
            "rename" => 11,
            "copy" => 12,
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
        let value = self.entry_slot(32, "fs.value")?;
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
                value.into(),
            ],
            32,
        )?;
        self.drop_moved(args)?;
        let d = self.place_ptr(dest)?;
        if op == 3 {
            let b = self
                .builder
                .build_int_truncate(code, self.llcx.i8_type(), "")
                .map_err(e)?;
            self.store(d, b.into())?;
            return Ok(Some(()));
        }
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
            let at = cx.offset(d, ok_off)?;
            match op {
                1 | 2 | 5 => cx.copy(at, out, okt)?,
                12 => {
                    let n = cx.load_i64(value)?;
                    cx.store(at, n.into())?;
                }
                4 => {
                    // `Metadata { len, is_file, is_dir, modified_ms }`.
                    for i in 0..4u32 {
                        let w = cx.offset(value, 8 * i as u64)?;
                        let w = cx.load_i64(w)?;
                        let f = cx.offset(at, cx.field_offset(okt, None, i)?)?;
                        let v: inkwell::values::BasicValueEnum = if i == 1 || i == 2 {
                            cx.builder
                                .build_int_truncate(w, cx.llcx.i8_type(), "")
                                .map_err(e)?
                                .into()
                        } else {
                            w.into()
                        };
                        cx.store(f, v)?;
                    }
                }
                _ => {}
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

impl<'ctx> Cx<'ctx, '_> {
    /// The channels (`Channel`, its `Sender` and `Receiver` ends, and
    /// `BoundedChannel`), a handle on the runtime's queue
    /// (`karac_mir_chan_*`), with the interpreter's `channel_method`
    /// semantics; `None` for a method not lowered.
    pub(super) fn chan_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        let elem_of = |me: &Self, t: Ty| -> R<Ty> {
            me.tys
                .tcx()
                .adt_of(t)
                .and_then(|(_, a)| a.first().copied())
                .ok_or_else(|| format!("`{}` needs its element type", me.tys.display(t)))
        };
        match (base, method, args) {
            ("Channel", "new" | "bounded", _) => {
                let cap = match args {
                    [] => self.i64(0),
                    [n] => self.int_arg(n)?,
                    _ => return Ok(None),
                };
                let end = self.tys.field_ty(dty, None, 0).ok_or("a channel pair")?;
                let el = elem_of(self, end)?;
                let h = self.chan_new(el, cap, false)?;
                let d = self.place_ptr(dest)?;
                for f in 0..2 {
                    let q = self.offset(d, self.field_offset(dty, None, f)?)?;
                    self.store(q, h.into())?;
                }
            }
            ("BoundedChannel", "new", [n, _]) => {
                let n = self.int_arg(n)?;
                let el = elem_of(self, dty)?;
                let h = self.chan_new(el, n, true)?;
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
                self.drop_moved(&args[1..])?;
            }
            (_, "drop", [recv]) => {
                let h = self.chan_handle(recv)?;
                let i8t = self.llcx.i8_type();
                for s in [1, 0] {
                    self.rt("chan_end", &[h.into(), i8t.const_int(s, false).into()])?;
                }
            }
            ("Sender" | "Receiver", "clone", [recv]) => {
                let h = self.chan_handle(recv)?;
                let s = self
                    .llcx
                    .i8_type()
                    .const_int(u64::from(base == "Sender"), false);
                let h = self.mrt_ptr("chan_clone", &[h.into(), s.into()])?;
                let d = self.place_ptr(dest)?;
                self.store(d, h.into())?;
            }
            (_, "send" | "try_send", [recv, v]) => {
                let h = self.chan_handle(recv)?;
                let (_, t) = self.cell_arg(recv)?;
                let el = elem_of(self, t)?;
                let vp = self.operand_ptr(v)?;
                let r = self.rt_int("chan_send", &[h.into(), vp.into()], 32)?;
                self.chan_send_result(base, method, dty, dest, r, vp, el)?;
            }
            (_, "recv" | "recv_blocking" | "try_recv", [recv]) => {
                let h = self.chan_handle(recv)?;
                let d = self.place_ptr(dest)?;
                if base == "BoundedChannel" || method == "try_recv" {
                    let (some, none) = self.option_variants(dty)?;
                    let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
                    let got = self.rt_flag("chan_recv", &[h.into(), q.into()])?;
                    let i32t = self.llcx.i32_type();
                    let k = self
                        .builder
                        .build_select(
                            got,
                            i32t.const_int(u64::from(some), false),
                            i32t.const_int(u64::from(none), false),
                            "",
                        )
                        .map_err(e)?
                        .into_int_value();
                    self.store_tag_dyn(d, dty, k)?;
                } else {
                    let got = self.rt_flag("chan_recv", &[h.into(), d.into()])?;
                    self.abort_unless(got, "explicit panic")?;
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// A new runtime channel of `el` values holding at most `cap`.
    fn chan_new(&mut self, el: Ty, cap: IntValue<'ctx>, bounded: bool) -> R<PointerValue<'ctx>> {
        let size = self.i64(self.layout(el)?.size);
        let glue = if self.tys.needs_drop(el) {
            self.glue_fn(el).as_global_value().as_pointer_value()
        } else {
            self.ptr_ty().const_null()
        };
        let cap = self
            .builder
            .build_int_cast_sign_flag(cap, self.llcx.i64_type(), true, "")
            .map_err(e)?;
        let b = self.llcx.i8_type().const_int(u64::from(bounded), false);
        self.mrt_ptr(
            "chan_new",
            &[size.into(), cap.into(), b.into(), glue.into()],
        )
    }

    /// The runtime channel a channel end or `BoundedChannel` operand holds
    /// (its first word), through references.
    fn chan_handle(&mut self, a: &Operand) -> R<PointerValue<'ctx>> {
        let (cell, _) = self.cell_arg(a)?;
        self.load_ptr(cell)
    }

    /// What a send returns, given the runtime's answer `r` (0 queued, 1
    /// full, 2 closed): `try_send`'s `Result` carrying the value back, a
    /// `BoundedChannel`'s `Result` dropping it when full, or a `Sender`'s
    /// panic when closed.
    #[allow(clippy::too_many_arguments)]
    fn chan_send_result(
        &mut self,
        base: &str,
        method: &str,
        dty: Ty,
        dest: &Place,
        r: IntValue<'ctx>,
        vp: PointerValue<'ctx>,
        el: Ty,
    ) -> R<()> {
        let f = self.cur_fn()?;
        let i32t = self.llcx.i32_type();
        if method == "try_send" || base == "BoundedChannel" {
            let d = self.place_ptr(dest)?;
            let okv = self.variant_of(dty, "Ok")?;
            let errv = self.variant_of(dty, "Err")?;
            let et = self
                .tys
                .field_ty(dty, Some(errv), 0)
                .ok_or("an `Err` without a payload")?;
            let q = self.offset(d, self.field_offset(dty, Some(errv), 0)?)?;
            let (ok_b, full_b, closed_b, done) = (
                self.llcx.append_basic_block(f, "send.ok"),
                self.llcx.append_basic_block(f, "send.full"),
                self.llcx.append_basic_block(f, "send.closed"),
                self.llcx.append_basic_block(f, "send"),
            );
            self.builder
                .build_switch(
                    r,
                    ok_b,
                    &[
                        (i32t.const_int(1, false), full_b),
                        (i32t.const_int(2, false), closed_b),
                    ],
                )
                .map_err(e)?;
            self.builder.position_at_end(ok_b);
            self.store_tag(d, dty, okv)?;
            self.builder.build_unconditional_branch(done).map_err(e)?;
            for (b, name) in [(full_b, "Full"), (closed_b, "Closed")] {
                self.builder.position_at_end(b);
                if base == "BoundedChannel" && name == "Closed" {
                    // Never answered: a `BoundedChannel` has no ends.
                    self.builder.build_unreachable().map_err(e)?;
                    continue;
                }
                let ev = self.variant_of(et, name)?;
                if method == "try_send" {
                    let at = self.offset(q, self.field_offset(et, Some(ev), 0)?)?;
                    self.copy(at, vp, el)?;
                } else if self.tys.needs_drop(el) {
                    self.call_glue(vp, el)?;
                }
                self.store_tag(q, et, ev)?;
                self.store_tag(d, dty, errv)?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
            }
            self.builder.position_at_end(done);
            return Ok(());
        }
        let closed = self
            .builder
            .build_int_compare(IntPredicate::EQ, r, i32t.const_int(2, false), "")
            .map_err(e)?;
        let (bad, good) = (
            self.llcx.append_basic_block(f, "send.closed"),
            self.llcx.append_basic_block(f, "send"),
        );
        self.builder
            .build_conditional_branch(closed, bad, good)
            .map_err(e)?;
        self.builder.position_at_end(bad);
        if self.tys.needs_drop(el) {
            self.call_glue(vp, el)?;
        }
        self.abort_with("explicit panic")?;
        self.builder.position_at_end(good);
        Ok(())
    }
}

impl<'ctx> Cx<'ctx, '_> {
    /// `TaskGroup` and `TaskHandle`, as the interpreter's `task_method`:
    /// one task runs at a time, so `spawn` runs the closure at once and
    /// its handle points at a malloc'd block holding the result, which
    /// `join` moves out (leaving a handle lent to it null) and the
    /// handle's glue drops otherwise. `None` for a method not lowered.
    pub(super) fn task_native(
        &mut self,
        base: &str,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        match (base, method, args) {
            ("TaskGroup", "new", []) => {
                let d = self.place_ptr(dest)?;
                self.store(d, self.i64(0).into())?;
            }
            ("TaskGroup", "cancel" | "drop", _) => {}
            ("TaskGroup", "spawn", [_, f]) => {
                let held = self
                    .tys
                    .tcx()
                    .adt_of(dty)
                    .and_then(|(_, a)| a.first().copied())
                    .ok_or("a TaskHandle without its result type")?;
                let c = self.callee_of(f)?;
                let size = self.layout(held)?.size;
                let malloc = self.libc_fn("malloc");
                let block = self
                    .builder
                    .build_call(malloc, &[self.i64(size.max(1)).into()], "task")
                    .map_err(e)?
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let rt = self.callee_ret(&c)?;
                if rt != held && self.widens(rt, held) {
                    let t = self.temp(rt, "task.ret")?;
                    self.call_callee(&c, t, &[])?;
                    self.widen_into(block, t, rt, held)?;
                } else {
                    self.call_callee(&c, block, &[])?;
                }
                if !Self::consumes(&c) {
                    self.drop_moved(&args[1..])?;
                }
                let d = self.place_ptr(dest)?;
                self.store(d, block.into())?;
            }
            ("TaskHandle", "join", [h]) => {
                let (cell, _) = self.cell_arg(h)?;
                let block = self.load_ptr(cell)?;
                let d = self.place_ptr(dest)?;
                self.copy(d, block, dty)?;
                self.free(block)?;
                self.store(cell, self.ptr_ty().const_null().into())?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }
}
