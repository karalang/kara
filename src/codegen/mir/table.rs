//! `Map`, `Set`, `SortedMap` and `SortedSet`: their natives and their
//! drop, over the runtime's insertion-ordered tables
//! (`runtime/src/mir_map.rs`), whose order is the interpreter's: entries
//! in insertion order, or in key order for the sorted kinds.

use inkwell::values::{BasicMetadataValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::key::KeyOp;
use super::{Cx, R};
use crate::mir::ty::{IntTy, IntrinsicTy};
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

/// A table type's parts.
#[derive(Clone, Copy)]
pub(super) struct Table {
    pub key: Ty,
    /// The value type of a map; `None` for a set.
    pub val: Option<Ty>,
    pub sorted: bool,
    /// The entry type: `(K, V)` for a map, `K` for a set.
    pub entry: Ty,
}

impl<'ctx> Cx<'ctx, '_> {
    /// The parts of the table type `ty` (through references), if it is one.
    pub(super) fn table_of(&self, mut ty: Ty) -> Option<Table> {
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ty) {
            ty = i;
        }
        let (key, val, sorted) = match self.tys.kind(ty) {
            TyKind::Intrinsic(IntrinsicTy::Map(k, v)) => (k, Some(v), false),
            TyKind::Intrinsic(IntrinsicTy::SortedMap(k, v)) => (k, Some(v), true),
            TyKind::Intrinsic(IntrinsicTy::Set(k)) => (k, None, false),
            TyKind::Intrinsic(IntrinsicTy::SortedSet(k)) => (k, None, true),
            _ => return None,
        };
        let entry = match val {
            Some(v) => self.tys.intern(TyKind::Tuple(vec![key, v])),
            None => key,
        };
        Some(Table {
            key,
            val,
            sorted,
            entry,
        })
    }

    fn val_off(&self, t: Table) -> R<u64> {
        match t.val {
            Some(_) => self.field_offset(t.entry, None, 1),
            None => Ok(0),
        }
    }

    pub(super) fn mrt_ptr(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> R<PointerValue<'ctx>> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(
            &format!("karac_mir_{name}"),
            self.ptr_ty().fn_type(&tys, false),
        );
        let v = self.builder.build_call(f, args, "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_pointer_value())
    }

    pub(super) fn mrt_i64(
        &self,
        name: &str,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> R<IntValue<'ctx>> {
        let tys = self.arg_tys(args);
        let f = self.extern_fn(
            &format!("karac_mir_{name}"),
            self.llcx.i64_type().fn_type(&tys, false),
        );
        let v = self.builder.build_call(f, args, "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }

    /// The `hash`, `eq` and `cmp` glue pointers a table of `t` searches
    /// with: `cmp` alone for a sorted one, `hash` and `eq` otherwise.
    fn glue_ptrs(&mut self, t: Table) -> [PointerValue<'ctx>; 3] {
        let null = self.ptr_ty().const_null();
        let ptr = |f: inkwell::values::FunctionValue<'ctx>| f.as_global_value().as_pointer_value();
        if t.sorted {
            [null, null, ptr(self.key_fn(self.own_order(t.key), t.key))]
        } else {
            [
                ptr(self.key_fn(KeyOp::Hash, t.key)),
                ptr(self.key_fn(KeyOp::Eq, t.key)),
                null,
            ]
        }
    }

    /// The index of `key` in the table `h`, or -1.
    fn find(
        &mut self,
        h: PointerValue<'ctx>,
        key: PointerValue<'ctx>,
        t: Table,
    ) -> R<IntValue<'ctx>> {
        let [hf, ef, cf] = self.glue_ptrs(t);
        self.mrt_i64(
            "map_find",
            &[h.into(), key.into(), hf.into(), ef.into(), cf.into()],
        )
    }

    /// Adds the entry at `entry` to the table whose handle is at `hp`.
    fn insert_entry(
        &mut self,
        hp: PointerValue<'ctx>,
        entry: PointerValue<'ctx>,
        t: Table,
    ) -> R<()> {
        let [hf, _, cf] = self.glue_ptrs(t);
        let size = self.layout(t.entry)?.size.max(1);
        self.mrt_i64(
            "map_insert",
            &[
                hp.into(),
                self.i64(size).into(),
                entry.into(),
                hf.into(),
                cf.into(),
            ],
        )?;
        Ok(())
    }

    fn entry_at(&self, h: PointerValue<'ctx>, i: IntValue<'ctx>) -> R<PointerValue<'ctx>> {
        self.mrt_ptr("map_at", &[h.into(), i.into()])
    }

    /// Where a key argument's bytes are, in the key type's layout: a `str`
    /// probe of a `String` key shares its first two words.
    fn key_arg(&mut self, a: &Operand, key: Ty) -> R<PointerValue<'ctx>> {
        let mut t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        while t != key {
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
        // An integer of another width (a `u8` byte looked up in a
        // `Set[i64]`) is widened to the key type, as the interpreter's
        // untyped integers compare.
        if let (TyKind::Int(from), TyKind::Int(_)) = (self.tys.kind(t), self.tys.kind(key)) {
            if t != key {
                let signed = matches!(
                    from,
                    IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Isize
                );
                let v = self.load(p, t)?.into_int_value();
                let kt = self.llvm_ty(key)?.into_int_type();
                let w = self
                    .builder
                    .build_int_cast_sign_flag(v, kt, signed, "")
                    .map_err(e)?;
                let tmp = self.temp(key, "key")?;
                self.store(tmp, w.into())?;
                return Ok(tmp);
            }
        }
        Ok(p)
    }

    /// Drops the table at `p`: each entry in its order, its key before its
    /// value, then the table.
    pub(super) fn drop_table(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<()> {
        let t = self
            .table_of(ty)
            .ok_or_else(|| format!("`{}` is not a table", self.tys.display(ty)))?;
        let h = self.load_ptr(p)?;
        let live = self.builder.build_is_not_null(h, "").map_err(e)?;
        self.if_then(live, |cx| {
            cx.drop_entries(h, t)?;
            cx.rt("map_free", &[h.into()])
        })
    }

    fn drop_entries(&mut self, h: PointerValue<'ctx>, t: Table) -> R<()> {
        let kd = self.tys.needs_drop(t.key);
        let vd = t.val.is_some_and(|v| self.tys.needs_drop(v));
        if !(kd || vd) {
            return Ok(());
        }
        let voff = self.val_off(t)?;
        let n = self.mrt_i64("map_len", &[h.into()])?;
        self.count_loop(n, |cx, i| {
            let at = cx.entry_at(h, i)?;
            if kd {
                cx.call_glue(at, t.key)?;
            }
            if let (true, Some(v)) = (vd, t.val) {
                let q = cx.offset(at, voff)?;
                cx.call_glue(q, v)?;
            }
            Ok(())
        })
    }

    /// Writes a clone of the table at `src` to `dst`.
    pub(super) fn clone_table(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        ty: Ty,
    ) -> R<()> {
        let t = self
            .table_of(ty)
            .ok_or_else(|| format!("`{}` is not a table", self.tys.display(ty)))?;
        self.store(dst, self.ptr_ty().const_null().into())?;
        let h = self.load_ptr(src)?;
        let n = self.mrt_i64("map_len", &[h.into()])?;
        let tmp = self.temp(t.entry, "entry")?;
        self.count_loop(n, |cx, i| {
            let at = cx.entry_at(h, i)?;
            cx.clone_into(tmp, at, t.entry)?;
            cx.insert_entry(dst, tmp, t)
        })
    }

    /// The natives of the table types, or `None` for a method that is not
    /// one of them.
    pub(super) fn table_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        if method == "new" || method == "with_capacity" {
            let d = self.place_ptr(dest)?;
            self.store(d, self.ptr_ty().const_null().into())?;
            return Ok(Some(()));
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let rt_ty = self.operand_type(recv)?;
        let Some(t) = self.table_of(rt_ty) else {
            return Ok(None);
        };
        let hp = self.ref_arg(recv)?;
        let is_map = t.val.is_some();
        let unit = matches!(self.tys.kind(dty), TyKind::Unit);
        match (method, &args[1..]) {
            ("len", []) => {
                let h = self.load_ptr(hp)?;
                let n = self.mrt_i64("map_len", &[h.into()])?;
                let d = self.place_ptr(dest)?;
                let it = self.llvm_ty(dty)?.into_int_type();
                let n = self
                    .builder
                    .build_int_truncate_or_bit_cast(n, it, "")
                    .map_err(e)?;
                self.store(d, n.into())?;
            }
            ("is_empty", []) => {
                let h = self.load_ptr(hp)?;
                let n = self.mrt_i64("map_len", &[h.into()])?;
                let z = self.is_zero(n)?;
                self.store_bool(dest, z)?;
            }
            ("contains_key" | "contains", [k]) => {
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.find(h, kp, t)?;
                let found = self.found(i)?;
                self.store_bool(dest, found)?;
                self.drop_moved(&args[1..])?;
            }
            ("get" | "get_mut_by_view", [k]) if is_map => {
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.find(h, kp, t)?;
                let found = self.found(i)?;
                let voff = self.val_off(t)?;
                let d = self.place_ptr(dest)?;
                let (some, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                self.if_then(found, |cx| {
                    let at = cx.entry_at(h, i)?;
                    let v = cx.offset(at, voff)?;
                    cx.store_tag(d, dty, some)?;
                    let vt = t.val.unwrap_or(t.key);
                    cx.store_found(d, dty, some, v, vt)
                })?;
                self.drop_moved(&args[1..])?;
            }
            ("index" | "index_mut", [k]) if is_map => {
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.find(h, kp, t)?;
                let found = self.found(i)?;
                self.abort_unless(found, "explicit panic")?;
                let at = self.entry_at(h, i)?;
                let v = self.offset(at, self.val_off(t)?)?;
                let d = self.place_ptr(dest)?;
                self.store(d, v.into())?;
                self.drop_moved(&args[1..])?;
            }
            ("entry_at", [i]) => {
                let h = self.load_ptr(hp)?;
                let i = self.int_arg(i)?;
                let n = self.mrt_i64("map_len", &[h.into()])?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, i, n, "")
                    .map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                let at = self.entry_at(h, i)?;
                let d = self.place_ptr(dest)?;
                self.store(d, at.into())?;
            }
            ("insert", [k, rest @ ..]) => self.table_insert(hp, t, k, rest, dest, dty, unit)?,
            ("entries", []) if is_map => {
                let h = self.load_ptr(hp)?;
                let n = self.mrt_i64("map_len", &[h.into()])?;
                let size = self.layout(t.entry)?.size;
                let d = self.place_ptr(dest)?;
                self.vec_alloc(d, n, size)?;
                let buf = self.load_ptr(d)?;
                self.count_loop(n, |cx, i| {
                    let at = cx.entry_at(h, i)?;
                    let out = cx.elem_ptr(buf, i, size)?;
                    cx.clone_into(out, at, t.entry)
                })?;
            }
            ("entry_or_insert" | "entry_or_insert_with", [k, val]) if is_map => {
                // A reference to the stored value, inserting `val` (or what
                // `val()` returns) when the key is new; a found key drops
                // the new key and the unused value.
                let v = t.val.unwrap_or(t.key);
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.find(h, kp, t)?;
                let found = self.found(i)?;
                let slot = self.entry_slot(8, "stored")?;
                let f = self.cur_fn()?;
                let (old, new, done) = (
                    self.llcx.append_basic_block(f, "entry.found"),
                    self.llcx.append_basic_block(f, "entry.new"),
                    self.llcx.append_basic_block(f, "entry"),
                );
                self.builder
                    .build_conditional_branch(found, old, new)
                    .map_err(e)?;
                self.builder.position_at_end(old);
                self.drop_moved(&args[1..])?;
                let at = self.entry_at(h, i)?;
                self.store(slot, at.into())?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(new);
                let tmp = self.temp(t.entry, "entry")?;
                self.copy(tmp, kp, t.key)?;
                let vslot = self.offset(tmp, self.val_off(t)?)?;
                if method == "entry_or_insert_with" {
                    let c = self.callee_of(val)?;
                    self.call_callee(&c, vslot, &[])?;
                    if !Self::consumes(&c) {
                        self.drop_moved(std::slice::from_ref(val))?;
                    }
                } else {
                    self.store_operand(vslot, val)?;
                }
                self.insert_entry(hp, tmp, t)?;
                let h2 = self.load_ptr(hp)?;
                let i2 = self.find(h2, tmp, t)?;
                let at = self.entry_at(h2, i2)?;
                self.store(slot, at.into())?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
                let at = self.load_ptr(slot)?;
                let vp = self.offset(at, self.val_off(t)?)?;
                let d = self.place_ptr(dest)?;
                self.store(d, vp.into())?;
                let _ = v;
            }
            ("remove", [k]) => {
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.find(h, kp, t)?;
                let found = self.found(i)?;
                let d = self.place_ptr(dest)?;
                let tmp = self.temp(t.entry, "removed")?;
                if is_map {
                    let (some, none) = self.option_variants(dty)?;
                    self.store_tag(d, dty, none)?;
                    let voff = self.val_off(t)?;
                    let v = t.val.unwrap_or(t.key);
                    self.if_then(found, |cx| {
                        cx.rt("map_remove", &[h.into(), i.into(), tmp.into()])?;
                        if cx.tys.needs_drop(t.key) {
                            cx.call_glue(tmp, t.key)?;
                        }
                        cx.store_tag(d, dty, some)?;
                        let q = cx.offset(d, cx.field_offset(dty, Some(some), 0)?)?;
                        let src = cx.offset(tmp, voff)?;
                        cx.copy(q, src, v)
                    })?;
                } else {
                    self.store_bool(dest, found)?;
                    self.if_then(found, |cx| {
                        cx.rt("map_remove", &[h.into(), i.into(), tmp.into()])?;
                        if cx.tys.needs_drop(t.key) {
                            cx.call_glue(tmp, t.key)?;
                        }
                        Ok(())
                    })?;
                }
                self.drop_moved(&args[1..])?;
            }
            ("clear", []) => {
                let h = self.load_ptr(hp)?;
                let live = self.builder.build_is_not_null(h, "").map_err(e)?;
                self.if_then(live, |cx| {
                    cx.drop_entries(h, t)?;
                    cx.rt("map_clear", &[h.into()])
                })?;
            }
            ("drain_entries", []) => {
                // Every entry moved out into a `Vec`, in iteration order;
                // the table is left empty, with nothing of its own to drop.
                let h = self.load_ptr(hp)?;
                let n = self.mrt_i64("map_len", &[h.into()])?;
                let size = self.layout(t.entry)?.size;
                let d = self.place_ptr(dest)?;
                self.vec_alloc(d, n, size)?;
                let buf = self.load_ptr(d)?;
                self.count_loop(n, |cx, i| {
                    let at = cx.entry_at(h, i)?;
                    let out = cx.elem_ptr(buf, i, size)?;
                    cx.copy(out, at, t.entry)
                })?;
                let live = self.builder.build_is_not_null(h, "").map_err(e)?;
                self.if_then(live, |cx| cx.rt("map_clear", &[h.into()]))?;
            }
            ("get_or", [k, default]) if is_map => {
                let v = t.val.unwrap_or(t.key);
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.find(h, kp, t)?;
                let found = self.found(i)?;
                let d = self.place_ptr(dest)?;
                let voff = self.val_off(t)?;
                let dt = self.operand_type(default)?;
                let by_ref = matches!(self.tys.kind(dt), TyKind::Ref(_) | TyKind::MutRef(_));
                let dp = if by_ref {
                    self.ref_arg(default)?
                } else {
                    self.operand_ptr(default)?
                };
                let f = self.cur_fn()?;
                let (yes, no, done) = (
                    self.llcx.append_basic_block(f, "get_or.found"),
                    self.llcx.append_basic_block(f, "get_or.default"),
                    self.llcx.append_basic_block(f, "get_or"),
                );
                self.builder
                    .build_conditional_branch(found, yes, no)
                    .map_err(e)?;
                self.builder.position_at_end(yes);
                let at = self.entry_at(h, i)?;
                let src = self.offset(at, voff)?;
                self.clone_into(d, src, v)?;
                if !by_ref {
                    self.drop_moved(std::slice::from_ref(default))?;
                }
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(no);
                if by_ref {
                    self.clone_into(d, dp, v)?;
                } else {
                    self.copy(d, dp, v)?;
                }
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
                self.drop_moved(std::slice::from_ref(k))?;
            }
            ("keys" | "values", []) if is_map => {
                let TyKind::Intrinsic(IntrinsicTy::Vec(el)) = self.tys.kind(dty) else {
                    return Err(format!("{method} into a `{}`", self.tys.display(dty)));
                };
                let (part_ty, off) = if method == "values" {
                    (t.val.unwrap_or(t.key), self.val_off(t)?)
                } else {
                    (t.key, 0)
                };
                let by_ref = matches!(self.tys.kind(el), TyKind::Ref(_));
                let size = self.layout(el)?.size;
                let h = self.load_ptr(hp)?;
                let n = self.mrt_i64("map_len", &[h.into()])?;
                let d = self.place_ptr(dest)?;
                self.vec_alloc(d, n, size)?;
                let buf = self.load_ptr(d)?;
                self.count_loop(n, |cx, i| {
                    let at = cx.entry_at(h, i)?;
                    let src = cx.offset(at, off)?;
                    let out = cx.elem_ptr(buf, i, size)?;
                    if by_ref {
                        cx.store(out, src.into())
                    } else {
                        cx.clone_into(out, src, part_ty)
                    }
                })?;
            }
            ("entry", [k]) if is_map => {
                // `Occupied` lends the stored value; `Vacant` keeps the key
                // and the map handle's address, its low bit set for a
                // sorted map (an `Entry` names a `Map` either way).
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.find(h, kp, t)?;
                let found = self.found(i)?;
                let d = self.place_ptr(dest)?;
                let occ = self.variant_of(dty, "Occupied")?;
                let vac = self.variant_of(dty, "Vacant")?;
                let f = self.cur_fn()?;
                let (old, new, done) = (
                    self.llcx.append_basic_block(f, "entry.occupied"),
                    self.llcx.append_basic_block(f, "entry.vacant"),
                    self.llcx.append_basic_block(f, "entry.made"),
                );
                self.builder
                    .build_conditional_branch(found, old, new)
                    .map_err(e)?;
                self.builder.position_at_end(old);
                self.drop_moved(&args[1..])?;
                self.store_tag(d, dty, occ)?;
                let at = self.entry_at(h, i)?;
                let v = self.offset(at, self.val_off(t)?)?;
                let q = self.offset(d, self.field_offset(dty, Some(occ), 0)?)?;
                self.store(q, v.into())?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(new);
                self.store_tag(d, dty, vac)?;
                let q = self.offset(d, self.field_offset(dty, Some(vac), 0)?)?;
                self.copy(q, kp, t.key)?;
                let q = self.offset(d, self.field_offset(dty, Some(vac), 1)?)?;
                let i64t = self.llcx.i64_type();
                let bits = self.builder.build_ptr_to_int(hp, i64t, "").map_err(e)?;
                let bits = self
                    .builder
                    .build_or(bits, i64t.const_int(u64::from(t.sorted), false), "")
                    .map_err(e)?;
                let tagged = self
                    .builder
                    .build_int_to_ptr(bits, self.ptr_ty(), "")
                    .map_err(e)?;
                self.store(q, tagged.into())?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
            }
            ("min" | "max" | "first" | "last", []) => {
                let h = self.load_ptr(hp)?;
                let op = if matches!(method, "min" | "first") {
                    0
                } else {
                    1
                };
                let null = self.ptr_ty().const_null();
                let i = self.pick(h, null, t, op)?;
                self.store_picked(dest, dty, h, i, t)?;
            }
            ("floor" | "ceiling", [k]) if is_map => {
                let h = self.load_ptr(hp)?;
                let kp = self.key_arg(k, t.key)?;
                let i = self.pick(h, kp, t, if method == "floor" { 2 } else { 3 })?;
                self.store_picked(dest, dty, h, i, t)?;
                self.drop_moved(&args[1..])?;
            }
            ("range" | "range_inclusive", [lo, hi]) if t.sorted => {
                // The entries with keys in `[lo, hi)` (`[lo, hi]`
                // inclusive), in key order: references when the `Vec`
                // holds them, else clones.
                let TyKind::Intrinsic(IntrinsicTy::Vec(el)) = self.tys.kind(dty) else {
                    return Err(format!("{method} into a `{}`", self.tys.display(dty)));
                };
                let by_ref = matches!(self.tys.kind(el), TyKind::Ref(_));
                let h = self.load_ptr(hp)?;
                let lp = self.key_arg(lo, t.key)?;
                let start = self.pick(h, lp, t, 4)?;
                let up = self.key_arg(hi, t.key)?;
                let end = self.pick(h, up, t, if method == "range" { 4 } else { 5 })?;
                let n = self.builder.build_int_sub(end, start, "").map_err(e)?;
                let neg = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, n, self.i64(0), "")
                    .map_err(e)?;
                let n = self
                    .builder
                    .build_select(neg, self.i64(0), n, "")
                    .map_err(e)?
                    .into_int_value();
                let size = self.layout(el)?.size;
                let d = self.place_ptr(dest)?;
                self.vec_alloc(d, n, size)?;
                let buf = self.load_ptr(d)?;
                self.count_loop(n, |cx, i| {
                    let j = cx.builder.build_int_add(start, i, "").map_err(e)?;
                    let at = cx.entry_at(h, j)?;
                    let out = cx.elem_ptr(buf, i, size)?;
                    if by_ref {
                        cx.store_ref(out, at, t.entry)
                    } else {
                        cx.clone_into(out, at, t.entry)
                    }
                })?;
                self.drop_moved(&args[1..])?;
            }
            ("union" | "intersection" | "difference" | "symmetric_difference", [other])
                if !is_map =>
            {
                // A new set of clones, the receiver's elements first (key
                // order for a sorted one, which its inserts keep).
                let h = self.load_ptr(hp)?;
                let ot = self.operand_type(other)?;
                let op = if matches!(self.tys.kind(ot), TyKind::Ref(_) | TyKind::MutRef(_)) {
                    self.ref_arg(other)?
                } else {
                    self.operand_ptr(other)?
                };
                let oh = self.load_ptr(op)?;
                let d = self.place_ptr(dest)?;
                self.store(d, self.ptr_ty().const_null().into())?;
                let tmp = self.temp(t.entry, "elem")?;
                let mine = self.mrt_i64("map_len", &[h.into()])?;
                self.count_loop(mine, |cx, i| {
                    let at = cx.entry_at(h, i)?;
                    let j = cx.find(oh, at, t)?;
                    let keep = match method {
                        "union" => cx.llcx.bool_type().const_all_ones(),
                        "intersection" => cx.found(j)?,
                        _ => cx
                            .builder
                            .build_int_compare(IntPredicate::SLT, j, cx.i64(0), "")
                            .map_err(e)?,
                    };
                    cx.if_then(keep, |cx| {
                        cx.clone_into(tmp, at, t.entry)?;
                        cx.insert_entry(d, tmp, t)
                    })
                })?;
                if matches!(method, "union" | "symmetric_difference") {
                    let theirs = self.mrt_i64("map_len", &[oh.into()])?;
                    self.count_loop(theirs, |cx, i| {
                        let at = cx.entry_at(oh, i)?;
                        let j = cx.find(h, at, t)?;
                        let new = cx
                            .builder
                            .build_int_compare(IntPredicate::SLT, j, cx.i64(0), "")
                            .map_err(e)?;
                        cx.if_then(new, |cx| {
                            cx.clone_into(tmp, at, t.entry)?;
                            cx.insert_entry(d, tmp, t)
                        })
                    })?;
                }
                self.drop_moved(&args[1..])?;
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    #[allow(clippy::too_many_arguments)]
    fn table_insert(
        &mut self,
        hp: PointerValue<'ctx>,
        t: Table,
        k: &Operand,
        rest: &[Operand],
        dest: &Place,
        dty: Ty,
        unit: bool,
    ) -> R<()> {
        let h = self.load_ptr(hp)?;
        let kp = self.key_arg(k, t.key)?;
        let i = self.find(h, kp, t)?;
        let found = self.found(i)?;
        let f = self.cur_fn()?;
        let (old, new, done) = (
            self.llcx.append_basic_block(f, "insert.old"),
            self.llcx.append_basic_block(f, "insert.new"),
            self.llcx.append_basic_block(f, "inserted"),
        );
        // The destination's address is taken before the branch: a
        // projected one (the `Ok` payload of `try_insert`) emits code.
        let d = if unit {
            None
        } else {
            Some(self.place_ptr(dest)?)
        };
        self.builder
            .build_conditional_branch(found, old, new)
            .map_err(e)?;
        // The key is already there: the new key drops, and a map's value
        // is replaced, handing the old one back.
        self.builder.position_at_end(old);
        if self.tys.needs_drop(t.key) && matches!(k, Operand::Move(_)) {
            self.call_glue(kp, t.key)?;
        }
        match (t.val, rest) {
            (Some(v), [val]) => {
                let at = self.entry_at(h, i)?;
                let slot = self.offset(at, self.val_off(t)?)?;
                match d {
                    Some(d) => {
                        let (some, _) = self.option_variants(dty)?;
                        self.store_tag(d, dty, some)?;
                        let q = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
                        self.copy(q, slot, v)?;
                    }
                    None if self.tys.needs_drop(v) => self.call_glue(slot, v)?,
                    None => {}
                }
                self.store_operand(slot, val)?;
            }
            (None, []) => {
                if let Some(d) = d {
                    let z = self.llcx.bool_type().const_zero();
                    self.store_i1(d, z)?;
                }
            }
            _ => return Err("a table insert with the wrong arguments".into()),
        }
        self.builder.build_unconditional_branch(done).map_err(e)?;
        // A new key: the entry is built in a temporary and added.
        self.builder.position_at_end(new);
        let tmp = self.temp(t.entry, "entry")?;
        self.copy(tmp, kp, t.key)?;
        if let [val] = rest {
            let slot = self.offset(tmp, self.val_off(t)?)?;
            self.store_operand(slot, val)?;
        }
        self.insert_entry(hp, tmp, t)?;
        match (d, t.val) {
            (Some(d), Some(_)) => {
                let (_, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
            }
            (Some(d), None) => {
                let one = self.llcx.bool_type().const_all_ones();
                self.store_i1(d, one)?;
            }
            (None, _) => {}
        }
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(done);
        Ok(())
    }

    /// The natives of `Entry[K, V]`, the view `Map.entry` returns:
    /// `and_modify`, `or_insert` and `or_insert_with`.
    pub(super) fn entry_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let mut ety = self.operand_type(recv)?;
        let mut ep = self.operand_ptr(recv)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ety) {
            ep = self.load_ptr(ep)?;
            ety = i;
        }
        // A user type named `Entry` keeps its own methods.
        let (Ok(occ), Ok(vac)) = (
            self.variant_of(ety, "Occupied"),
            self.variant_of(ety, "Vacant"),
        ) else {
            return Ok(None);
        };
        let vref = self
            .tys
            .field_ty(ety, Some(occ), 0)
            .ok_or("an `Occupied` without its value")?;
        let mty = self
            .tys
            .field_ty(ety, Some(vac), 1)
            .ok_or("a `Vacant` without its map")?;
        let t = self
            .table_of(mty)
            .ok_or("a `Vacant` whose map is not a table")?;
        let tag = self.load_tag(ep, ety)?;
        let occupied = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                tag.get_type().const_int(u64::from(occ), false),
                "",
            )
            .map_err(e)?;
        let at_occ = self.offset(ep, self.field_offset(ety, Some(occ), 0)?)?;
        match (method, &args[1..]) {
            ("and_modify", [f]) => {
                let d = self.place_ptr(dest)?;
                self.copy(d, ep, ety)?;
                let c = self.callee_of(f)?;
                let fd = self.cur_fn()?;
                let (yes, no, done) = (
                    self.llcx.append_basic_block(fd, "modify"),
                    self.llcx.append_basic_block(fd, "modify.skip"),
                    self.llcx.append_basic_block(fd, "modified"),
                );
                self.builder
                    .build_conditional_branch(occupied, yes, no)
                    .map_err(e)?;
                self.builder.position_at_end(yes);
                let ret = self.entry_slot(16, "modify.ret")?;
                self.call_callee(&c, ret, &[(at_occ, vref)])?;
                if !Self::consumes(&c) {
                    self.drop_moved(std::slice::from_ref(f))?;
                }
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(no);
                self.drop_moved(std::slice::from_ref(f))?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
            }
            ("or_insert" | "or_insert_with", [val]) => {
                let d = self.place_ptr(dest)?;
                let fd = self.cur_fn()?;
                let (yes, no, done) = (
                    self.llcx.append_basic_block(fd, "or_insert.found"),
                    self.llcx.append_basic_block(fd, "or_insert.new"),
                    self.llcx.append_basic_block(fd, "or_insert"),
                );
                self.builder
                    .build_conditional_branch(occupied, yes, no)
                    .map_err(e)?;
                self.builder.position_at_end(yes);
                let v = self.load_ptr(at_occ)?;
                self.store(d, v.into())?;
                self.drop_moved(std::slice::from_ref(val))?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(no);
                let kq = self.offset(ep, self.field_offset(ety, Some(vac), 0)?)?;
                let mq = self.offset(ep, self.field_offset(ety, Some(vac), 1)?)?;
                let tagged = self.load_ptr(mq)?;
                let i64t = self.llcx.i64_type();
                let bits = self.builder.build_ptr_to_int(tagged, i64t, "").map_err(e)?;
                let low = self
                    .builder
                    .build_and(bits, i64t.const_int(1, false), "")
                    .map_err(e)?;
                let sorted = self.is_zero(low)?;
                let sorted = self.builder.build_not(sorted, "").map_err(e)?;
                let bits = self
                    .builder
                    .build_and(bits, i64t.const_int(!1, false), "")
                    .map_err(e)?;
                let hp = self
                    .builder
                    .build_int_to_ptr(bits, self.ptr_ty(), "")
                    .map_err(e)?;
                let tmp = self.temp(t.entry, "entry")?;
                self.copy(tmp, kq, t.key)?;
                let vslot = self.offset(tmp, self.val_off(t)?)?;
                if method == "or_insert_with" {
                    let c = self.callee_of(val)?;
                    self.call_callee(&c, vslot, &[])?;
                    if !Self::consumes(&c) {
                        self.drop_moved(std::slice::from_ref(val))?;
                    }
                } else {
                    self.store_operand(vslot, val)?;
                }
                let hashed = Table { sorted: false, ..t };
                let sorted_t = Table { sorted: true, ..t };
                let [hf, _, _] = self.glue_ptrs(hashed);
                let [_, _, cf] = self.glue_ptrs(sorted_t);
                let null = self.ptr_ty().const_null();
                let hf = self.builder.build_select(sorted, null, hf, "").map_err(e)?;
                let cf = self.builder.build_select(sorted, cf, null, "").map_err(e)?;
                let size = self.layout(t.entry)?.size.max(1);
                let i = self.mrt_i64(
                    "map_insert",
                    &[
                        hp.into(),
                        self.i64(size).into(),
                        tmp.into(),
                        hf.into(),
                        cf.into(),
                    ],
                )?;
                let h = self.load_ptr(hp)?;
                let at = self.entry_at(h, i)?;
                let v = self.offset(at, self.val_off(t)?)?;
                self.store(d, v.into())?;
                self.builder.build_unconditional_branch(done).map_err(e)?;
                self.builder.position_at_end(done);
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    /// [`karac_mir_map_pick`]'s index into `h` for `op` (see the runtime).
    fn pick(
        &mut self,
        h: PointerValue<'ctx>,
        key: PointerValue<'ctx>,
        t: Table,
        op: u64,
    ) -> R<IntValue<'ctx>> {
        let cmp = self.key_fn(self.own_order(t.key), t.key);
        let cmp = cmp.as_global_value().as_pointer_value();
        let sorted = self.llcx.bool_type().const_int(u64::from(t.sorted), false);
        self.mrt_i64(
            "map_pick",
            &[
                h.into(),
                key.into(),
                cmp.into(),
                sorted.into(),
                self.i64(op).into(),
            ],
        )
    }

    /// Writes `Some` of the entry at index `i` of `h` to the option
    /// `dest` (a reference or a clone, as its payload type has it), or
    /// `None` when `i` is -1.
    fn store_picked(
        &mut self,
        dest: &Place,
        dty: Ty,
        h: PointerValue<'ctx>,
        i: IntValue<'ctx>,
        t: Table,
    ) -> R<()> {
        let d = self.place_ptr(dest)?;
        let (some, none) = self.option_variants(dty)?;
        self.store_tag(d, dty, none)?;
        let found = self.found(i)?;
        self.if_then(found, |cx| {
            let at = cx.entry_at(h, i)?;
            cx.store_tag(d, dty, some)?;
            cx.store_found(d, dty, some, at, t.entry)
        })
    }

    fn found(&self, i: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        self.builder
            .build_int_compare(IntPredicate::SGE, i, self.i64(0), "")
            .map_err(e)
    }

    /// Stores the `i1` `v` as a `bool` byte at `p`.
    pub(super) fn store_i1(&self, p: PointerValue<'ctx>, v: IntValue<'ctx>) -> R<()> {
        let b = self
            .builder
            .build_int_z_extend(v, self.llcx.i8_type(), "")
            .map_err(e)?;
        self.store(p, b.into())
    }

    pub(super) fn store_bool(&mut self, dest: &Place, v: IntValue<'ctx>) -> R<()> {
        let d = self.place_ptr(dest)?;
        self.store_i1(d, v)
    }
}
