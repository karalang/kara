//! Key glue, for the tables and the orders: `hash.<T>(p) -> u64`,
//! `eq.<T>(a, b) -> bool` and `cmp.<T>(a, b) -> i32` (-1, 0 or 1).
//!
//! All three are structural, as the interpreter's `key_form` and
//! `key_order` are (`src/mir/interp.rs`): numbers, text and chars by
//! value, `false < true`, tuples, arrays, sequences and structs part by
//! part and then by length, enums by variant and then payload, and a
//! `shared` handle by what it holds. A reference is its referent. `eq`
//! compares floats by their bits, so it agrees with `hash` and is an
//! equivalence (every NaN equal to itself); `cmp` orders them by value.

use std::collections::HashMap;

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Linkage;
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::layout;
use crate::mir::ty::IntrinsicTy;
use crate::mir::{Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum KeyOp {
    Hash,
    Eq,
    /// The order by value.
    Cmp,
    /// The order with floats by their bits, whose zero is `eq`.
    Total,
    /// The order of the type's own `cmp` body.
    User,
}

#[derive(Default)]
pub(super) struct Keys<'ctx> {
    fns: HashMap<(KeyOp, Ty), FunctionValue<'ctx>>,
    todo: Vec<(KeyOp, Ty, FunctionValue<'ctx>)>,
}

/// One part of a value being walked: where it is in each operand, and its
/// type.
type Part<'ctx> = (PointerValue<'ctx>, PointerValue<'ctx>, Ty);

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn keys_pending(&self) -> bool {
        !self.keys.todo.is_empty()
    }

    /// The key glue `op` of `ty`, declared now and built by
    /// [`Cx::finish_keys`].
    pub(super) fn key_fn(&mut self, op: KeyOp, ty: Ty) -> FunctionValue<'ctx> {
        if let Some(&f) = self.keys.fns.get(&(op, ty)) {
            return f;
        }
        let p = self.ptr_ty();
        let c = self.llcx;
        let fn_ty = match op {
            KeyOp::Hash => c.i64_type().fn_type(&[p.into()], false),
            KeyOp::Eq => c.bool_type().fn_type(&[p.into(), p.into()], false),
            KeyOp::Cmp | KeyOp::Total | KeyOp::User => {
                c.i32_type().fn_type(&[p.into(), p.into()], false)
            }
        };
        let prefix = match op {
            KeyOp::Hash => "hash",
            KeyOp::Eq => "eq",
            KeyOp::Cmp => "cmp",
            KeyOp::Total => "cmp.total",
            KeyOp::User => "cmp.user",
        };
        let name = format!("{prefix}.{}", self.tys.display(ty));
        let f = self
            .module
            .add_function(&name, fn_ty, Some(Linkage::Internal));
        // The runtime calls `eq` glue as a Rust `bool` function.
        self.bool_abi(f, false);
        self.keys.fns.insert((op, ty), f);
        self.keys.todo.push((op, ty, f));
        f
    }

    pub(super) fn finish_keys(&mut self) -> R<()> {
        while let Some((op, ty, f)) = self.keys.todo.pop() {
            let entry = self.llcx.append_basic_block(f, "entry");
            self.builder.position_at_end(entry);
            let a = f
                .get_nth_param(0)
                .ok_or("key glue has no parameter")?
                .into_pointer_value();
            match op {
                // A key with a user `eq` is found by calling it on every
                // entry, as the interpreter's `find_key` does: one bucket.
                KeyOp::Hash if self.user_eq_body(ty).is_some() => {
                    let z = self.llcx.i64_type().const_zero();
                    self.builder.build_return(Some(&z)).map_err(e)?;
                }
                KeyOp::Eq if self.user_eq_body(ty).is_some() => {
                    let b = f
                        .get_nth_param(1)
                        .ok_or("eq glue has one parameter")?
                        .into_pointer_value();
                    let name = self.user_eq_body(ty).unwrap_or_default();
                    let program = self.program;
                    let bt = program.bodies[&name].return_ty();
                    let func = self.function(&name)?;
                    let ret = self.temp(bt, "eq")?;
                    let (sa, sb) = (self.entry_slot(8, "a")?, self.entry_slot(8, "b")?);
                    self.store(sa, a.into())?;
                    self.store(sb, b.into())?;
                    self.builder
                        .build_call(func, &[ret.into(), sa.into(), sb.into()], "")
                        .map_err(e)?;
                    let v = self.load(ret, bt)?.into_int_value();
                    let t = self
                        .builder
                        .build_int_compare(IntPredicate::NE, v, v.get_type().const_zero(), "")
                        .map_err(e)?;
                    self.builder.build_return(Some(&t)).map_err(e)?;
                }
                KeyOp::Hash => {
                    let h = self.hash_value(a, ty)?;
                    self.builder.build_return(Some(&h)).map_err(e)?;
                }
                KeyOp::Eq => {
                    let b = f
                        .get_nth_param(1)
                        .ok_or("eq glue has one parameter")?
                        .into_pointer_value();
                    let c = self.cmp_call(KeyOp::Total, a, b, ty)?;
                    let z = self.is_zero(c)?;
                    self.builder.build_return(Some(&z)).map_err(e)?;
                }
                KeyOp::User => {
                    let b = f
                        .get_nth_param(1)
                        .ok_or("cmp glue has one parameter")?
                        .into_pointer_value();
                    let c = self.user_cmp_value(a, b, ty)?;
                    self.builder.build_return(Some(&c)).map_err(e)?;
                }
                KeyOp::Cmp | KeyOp::Total => {
                    let b = f
                        .get_nth_param(1)
                        .ok_or("cmp glue has one parameter")?
                        .into_pointer_value();
                    let c = self.cmp_value(op, a, b, ty)?;
                    self.builder.build_return(Some(&c)).map_err(e)?;
                }
            }
        }
        Ok(())
    }

    /// `cmp.<ty>(a, b)` (or its total form), inline for a scalar.
    pub(super) fn cmp_call(
        &mut self,
        op: KeyOp,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        ty: Ty,
    ) -> R<IntValue<'ctx>> {
        if self.is_scalar_key(ty) {
            return self.cmp_value(op, a, b, ty);
        }
        let f = self.key_fn(op, ty);
        let v = self
            .builder
            .build_call(f, &[a.into(), b.into()], "")
            .map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }

    /// The order `ty` is sorted, searched and kept in: its own `cmp` body
    /// when it has one, else the structural order (B-2026-10-06-109).
    pub(super) fn own_order(&self, ty: Ty) -> KeyOp {
        if self.user_cmp_body(ty).is_some() {
            KeyOp::User
        } else {
            KeyOp::Cmp
        }
    }

    /// The name of the `cmp` body of `ty` (through references), if it has
    /// one.
    pub(super) fn user_cmp_body(&self, ty: Ty) -> Option<String> {
        let mut ty = ty;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ty) {
            ty = i;
        }
        if !matches!(self.tys.kind(ty), TyKind::Adt(_) | TyKind::Shared(_)) {
            return None;
        }
        let name = format!("{}.cmp", self.tys.adt_name(ty));
        self.program.bodies.contains_key(&name).then_some(name)
    }

    /// The lowered body of `ty`'s user `PartialEq.eq`, if it has one.
    fn user_eq_body(&self, ty: Ty) -> Option<String> {
        let mut ty = ty;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(ty) {
            ty = i;
        }
        if !matches!(self.tys.kind(ty), TyKind::Adt(_) | TyKind::Shared(_)) {
            return None;
        }
        let name = format!("{}.eq", self.tys.adt_name(ty));
        self.program.bodies.contains_key(&name).then_some(name)
    }

    /// -1, 0 or 1 from the `Ordering` the `cmp` body of `ty` returns for
    /// `a` and `b`, each passed by reference.
    fn user_cmp_value(
        &mut self,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        ty: Ty,
    ) -> R<IntValue<'ctx>> {
        let name = self
            .user_cmp_body(ty)
            .ok_or_else(|| format!("`{}` has no cmp body", self.tys.display(ty)))?;
        // A key that is itself a reference (`sort_by_key(|s| s)`) points
        // at the value the body compares.
        let (mut a, mut b, mut t) = (a, b, ty);
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            a = self.load_ptr(a)?;
            b = self.load_ptr(b)?;
            t = i;
        }
        let program = self.program;
        let ord_ty = program.bodies[&name].return_ty();
        let f = self.function(&name)?;
        let ret = self.temp(ord_ty, "ordering")?;
        let (sa, sb) = (self.entry_slot(8, "a")?, self.entry_slot(8, "b")?);
        self.store(sa, a.into())?;
        self.store(sb, b.into())?;
        self.builder
            .build_call(f, &[ret.into(), sa.into(), sb.into()], "")
            .map_err(e)?;
        self.ordering_value(ret, ord_ty)
    }

    /// -1, 0 or 1 for the `Ordering` of type `ord_ty` at `p`.
    pub(super) fn ordering_value(
        &mut self,
        p: PointerValue<'ctx>,
        ord_ty: Ty,
    ) -> R<IntValue<'ctx>> {
        let t = self.load_tag(p, ord_ty)?;
        let i32t = self.llcx.i32_type();
        let t = self
            .builder
            .build_int_cast_sign_flag(t, i32t, false, "")
            .map_err(e)?;
        let less = self.variant_of(ord_ty, "Less")?;
        let greater = self.variant_of(ord_ty, "Greater")?;
        let is_less = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                t,
                i32t.const_int(u64::from(less), false),
                "",
            )
            .map_err(e)?;
        let is_greater = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                t,
                i32t.const_int(u64::from(greater), false),
                "",
            )
            .map_err(e)?;
        self.ord_of(is_less, is_greater)
    }

    pub(super) fn hash_call(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<IntValue<'ctx>> {
        if self.is_scalar_key(ty) {
            return self.hash_value(p, ty);
        }
        let f = self.key_fn(KeyOp::Hash, ty);
        let v = self.builder.build_call(f, &[p.into()], "").map_err(e)?;
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }

    /// Whether `ty`'s key glue only reads: a scalar or text, whose
    /// comparisons run no user code.
    pub(super) fn plain_key(&self, ty: Ty) -> bool {
        self.is_scalar_key(ty) || self.seq_elem(ty) == Some(None)
    }

    fn is_scalar_key(&self, ty: Ty) -> bool {
        matches!(
            self.tys.kind(ty),
            TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Float(_) | TyKind::Unit
        )
    }

    /// A scalar at `p` as an `i64` of its bits (an `i128` folded).
    fn scalar_bits(&self, p: PointerValue<'ctx>, ty: Ty) -> R<IntValue<'ctx>> {
        let v = self.load(p, ty)?;
        let i64t = self.llcx.i64_type();
        let v = if v.is_float_value() {
            // Every NaN as one, as the interpreter's total form keys it.
            let f = v.into_float_value();
            let bits = f.get_type().get_bit_width();
            let it = self.int_ty(bits);
            let raw = self
                .builder
                .build_bit_cast(f, it, "")
                .map_err(e)?
                .into_int_value();
            let nan = self
                .builder
                .build_float_compare(inkwell::FloatPredicate::UNO, f, f, "")
                .map_err(e)?;
            let canon = match bits {
                32 => it.const_int(u64::from(f32::NAN.to_bits()), false),
                _ => it.const_int(f64::NAN.to_bits(), false),
            };
            self.builder
                .build_select(nan, canon, raw, "")
                .map_err(e)?
                .into_int_value()
        } else {
            v.into_int_value()
        };
        let w = v.get_type().get_bit_width();
        Ok(if w == 128 {
            let hi = self
                .builder
                .build_right_shift(v, v.get_type().const_int(64, false), false, "")
                .map_err(e)?;
            let lo = self.builder.build_int_truncate(v, i64t, "").map_err(e)?;
            let hi = self.builder.build_int_truncate(hi, i64t, "").map_err(e)?;
            self.builder.build_xor(lo, hi, "").map_err(e)?
        } else if w < 64 {
            self.builder.build_int_z_extend(v, i64t, "").map_err(e)?
        } else {
            v
        })
    }

    /// `h` mixed with `x`.
    fn mix(&self, h: IntValue<'ctx>, x: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        let r = self
            .builder
            .build_left_shift(h, self.i64(5), "")
            .map_err(e)?;
        let l = self
            .builder
            .build_right_shift(h, self.i64(59), false, "")
            .map_err(e)?;
        let r = self.builder.build_or(r, l, "").map_err(e)?;
        let r = self.builder.build_xor(r, x, "").map_err(e)?;
        self.builder
            .build_int_mul(r, self.i64(0x0100_0000_01b3), "")
            .map_err(e)
    }

    /// The runtime's keyed hash of a word.
    fn hash_word(&self, v: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        let i64t = self.llcx.i64_type();
        let f = self.extern_fn("karac_hash_word", i64t.fn_type(&[i64t.into()], false));
        let v = self.builder.build_call(f, &[v.into()], "").map_err(e)?;
        // The seed is fixed for the process, so the hash is a pure function
        // of `v` to compiled code, and a `get` and an `insert` of one key
        // share it (kata 219).
        for name in ["memory", "nounwind", "willreturn"] {
            v.add_attribute(
                AttributeLoc::Function,
                self.llcx
                    .create_enum_attribute(Attribute::get_named_enum_kind_id(name), 0),
            );
        }
        Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
    }

    /// Where the `{ptr, len}` of the sequence of type `ty` at `p` is: the
    /// value itself for a `str`, a slice or a reference to one, the
    /// `String` or `Vec` header otherwise.
    fn seq_parts(&self, p: PointerValue<'ctx>) -> R<(PointerValue<'ctx>, IntValue<'ctx>)> {
        Ok((self.load_ptr(p)?, self.load_i64(self.offset(p, 8)?)?))
    }

    /// The element type of a sequence type, or `None` for text.
    fn seq_elem(&self, ty: Ty) -> Option<Option<Ty>> {
        match self.tys.kind(ty) {
            TyKind::Str | TyKind::Intrinsic(IntrinsicTy::String) => Some(None),
            TyKind::Slice(el) | TyKind::Intrinsic(IntrinsicTy::Vec(el)) => Some(Some(el)),
            _ => None,
        }
    }

    /// What a key of type `ty` at `p` is compared and hashed as: through a
    /// reference to its referent and through a `shared` handle to its
    /// body.
    fn key_place(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<(PointerValue<'ctx>, Ty)> {
        match self.tys.kind(ty) {
            TyKind::Ref(i) | TyKind::MutRef(i) => {
                if matches!(self.tys.kind(i), TyKind::Str | TyKind::Slice(_)) {
                    Ok((p, i))
                } else {
                    let q = self.load_ptr(p)?;
                    self.key_place(q, i)
                }
            }
            TyKind::Shared(a) => {
                let b = self.load_ptr(p)?;
                let off =
                    layout::shared_body_offset(self.tys, ty).ok_or("a shared type has no body")?;
                Ok((self.offset(b, off)?, self.tys.intern(TyKind::Adt(a))))
            }
            _ => Ok((p, ty)),
        }
    }

    /// The fields of the struct or tuple `ty`, or of variant `v` of the
    /// enum `ty`, as offsets, types and whether the part is boxed.
    fn parts_of(&self, ty: Ty, v: Option<u32>) -> R<Vec<(u64, Ty, bool)>> {
        let n = match self.tys.kind(ty) {
            TyKind::Tuple(ts) => ts.len(),
            _ => {
                let (adt, _) = self
                    .tys
                    .tcx()
                    .adt_of(ty)
                    .ok_or_else(|| format!("`{}` is not an ADT", self.tys.display(ty)))?;
                adt.variants[v.unwrap_or(0) as usize].fields.len()
            }
        };
        (0..n as u32)
            .map(|i| {
                let t = self
                    .tys
                    .field_ty(ty, v, i)
                    .ok_or_else(|| format!("`{}` has no field {i}", self.tys.display(ty)))?;
                let boxed = layout::boxed_field(self.tys, ty, v, i);
                Ok((self.field_offset(ty, v, i)?, t, boxed))
            })
            .collect()
    }

    /// Where a part [`Self::parts_of`] found lives in the value at `p`.
    fn part_ptr(&mut self, p: PointerValue<'ctx>, off: u64, boxed: bool) -> R<PointerValue<'ctx>> {
        let q = self.offset(p, off)?;
        if boxed {
            self.load_ptr(q)
        } else {
            Ok(q)
        }
    }

    fn hash_value(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<IntValue<'ctx>> {
        let (p, ty) = self.key_place(p, ty)?;
        if matches!(self.tys.kind(ty), TyKind::Unit) || self.layout(ty)?.size == 0 {
            return Ok(self.i64(0));
        }
        if self.is_scalar_key(ty) {
            let v = self.scalar_bits(p, ty)?;
            return self.hash_word(v);
        }
        if let Some(el) = self.seq_elem(ty) {
            let (buf, n) = self.seq_parts(p)?;
            return match el {
                None => {
                    let pt = self.ptr_ty();
                    let i64t = self.llcx.i64_type();
                    let f = self.extern_fn(
                        "karac_hash_bytes",
                        i64t.fn_type(&[pt.into(), i64t.into()], false),
                    );
                    let v = self
                        .builder
                        .build_call(f, &[buf.into(), n.into()], "")
                        .map_err(e)?;
                    Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
                }
                Some(el) => self.hash_elems(buf, n, el),
            };
        }
        match self.tys.kind(ty) {
            TyKind::Array(el, n) => self.hash_elems(p, self.i64(n), el),
            TyKind::Tuple(_) => self.hash_parts(p, ty, None, self.i64(0)),
            TyKind::Adt(_) => match layout::enum_tag(self.tys, ty) {
                None => self.hash_parts(p, ty, None, self.i64(0)),
                Some(_) => {
                    let acc = self.entry_slot(8, "hash")?;
                    let t = self.tag_at(p, ty)?;
                    let t64 = self
                        .builder
                        .build_int_z_extend_or_bit_cast(t, self.llcx.i64_type(), "")
                        .map_err(e)?;
                    let h0 = self.hash_word(t64)?;
                    self.store(acc, h0.into())?;
                    self.each_variant(t, ty, |cx, v| {
                        let h = cx.hash_parts(p, ty, Some(v), h0)?;
                        cx.store(acc, h.into())
                    })?;
                    self.load_i64(acc)
                }
            },
            _ => Err(format!(
                "MIR to LLVM: hashing a `{}` is not lowered yet",
                self.tys.display(ty)
            )),
        }
    }

    fn hash_parts(
        &mut self,
        p: PointerValue<'ctx>,
        ty: Ty,
        v: Option<u32>,
        mut h: IntValue<'ctx>,
    ) -> R<IntValue<'ctx>> {
        for (off, t, boxed) in self.parts_of(ty, v)? {
            let q = self.part_ptr(p, off, boxed)?;
            let x = self.hash_call(q, t)?;
            h = self.mix(h, x)?;
        }
        Ok(h)
    }

    fn hash_elems(
        &mut self,
        buf: PointerValue<'ctx>,
        n: IntValue<'ctx>,
        el: Ty,
    ) -> R<IntValue<'ctx>> {
        let size = self.layout(el)?.size;
        let acc = self.entry_slot(8, "hash")?;
        self.store(acc, n.into())?;
        self.count_loop(n, |cx, i| {
            let q = cx.elem_ptr(buf, i, size)?;
            let x = cx.hash_call(q, el)?;
            let h = cx.load_i64(acc)?;
            let h = cx.mix(h, x)?;
            cx.store(acc, h.into())
        })?;
        self.load_i64(acc)
    }

    /// For each variant of the enum `ty` that has fields, `body(v)` in a
    /// block reached when the tag `t` is `v`; the builder ends after them.
    fn each_variant(
        &mut self,
        t: IntValue<'ctx>,
        ty: Ty,
        mut body: impl FnMut(&mut Self, u32) -> R<()>,
    ) -> R<()> {
        let (adt, _) = self
            .tys
            .tcx()
            .adt_of(ty)
            .ok_or_else(|| format!("`{}` is not an ADT", self.tys.display(ty)))?;
        let f = self.cur_fn()?;
        let done = self.llcx.append_basic_block(f, "variants.done");
        let mut cases = Vec::new();
        for (v, var) in adt.variants.iter().enumerate() {
            if !var.fields.is_empty() {
                let bb = self.llcx.append_basic_block(f, &format!("variant{v}"));
                cases.push((t.get_type().const_int(v as u64, false), bb, v as u32));
            }
        }
        let arms: Vec<_> = cases.iter().map(|(k, bb, _)| (*k, *bb)).collect();
        self.builder.build_switch(t, done, &arms).map_err(e)?;
        for (_, bb, v) in cases {
            self.builder.position_at_end(bb);
            body(self, v)?;
            self.builder.build_unconditional_branch(done).map_err(e)?;
        }
        self.builder.position_at_end(done);
        Ok(())
    }

    /// -1, 0 or 1 as `a` is less than, equal to or greater than `b`.
    fn cmp_value(
        &mut self,
        op: KeyOp,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        ty: Ty,
    ) -> R<IntValue<'ctx>> {
        let (a, ta) = self.key_place(a, ty)?;
        let (b, _) = self.key_place(b, ty)?;
        let ty = ta;
        let i32t = self.llcx.i32_type();
        if matches!(self.tys.kind(ty), TyKind::Unit) || self.layout(ty)?.size == 0 {
            return Ok(i32t.const_zero());
        }
        match self.tys.kind(ty) {
            TyKind::Float(_) if op == KeyOp::Cmp => {
                let x = self.load(a, ty)?;
                let x = self.float_of(x, ty)?;
                let y = self.load(b, ty)?;
                let y = self.float_of(y, ty)?;
                let lt = self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::OLT, x, y, "")
                    .map_err(e)?;
                let gt = self
                    .builder
                    .build_float_compare(inkwell::FloatPredicate::OGT, x, y, "")
                    .map_err(e)?;
                return self.ord_of(lt, gt);
            }
            TyKind::Float(_) => {
                let x = self.scalar_bits(a, ty)?;
                let y = self.scalar_bits(b, ty)?;
                return self.ord_ints(x, y, false);
            }
            TyKind::Int(it) => {
                let x = self.load(a, ty)?.into_int_value();
                let y = self.load(b, ty)?.into_int_value();
                return self.ord_ints(x, y, it.signed());
            }
            TyKind::Bool | TyKind::Char => {
                let x = self.load(a, ty)?.into_int_value();
                let y = self.load(b, ty)?.into_int_value();
                return self.ord_ints(x, y, false);
            }
            _ => {}
        }
        if let Some(el) = self.seq_elem(ty) {
            let (pa, na) = self.seq_parts(a)?;
            let (pb, nb) = self.seq_parts(b)?;
            return match el {
                None => {
                    let pt = self.ptr_ty();
                    let i64t = self.llcx.i64_type();
                    let f = self.extern_fn(
                        "karac_mir_bytes_cmp",
                        i32t.fn_type(&[pt.into(), i64t.into(), pt.into(), i64t.into()], false),
                    );
                    let v = self
                        .builder
                        .build_call(f, &[pa.into(), na.into(), pb.into(), nb.into()], "")
                        .map_err(e)?;
                    Ok(v.try_as_basic_value().unwrap_basic().into_int_value())
                }
                Some(el) => self.cmp_elems(op, pa, na, pb, nb, el),
            };
        }
        match self.tys.kind(ty) {
            TyKind::Array(el, n) => {
                let n = self.i64(n);
                self.cmp_elems(op, a, n, b, n, el)
            }
            TyKind::Tuple(_) => {
                let parts = self.parts_of(ty, None)?;
                self.cmp_parts(op, a, b, parts)
            }
            TyKind::Adt(_) => match layout::enum_tag(self.tys, ty) {
                None => {
                    let parts = self.parts_of(ty, None)?;
                    self.cmp_parts(op, a, b, parts)
                }
                Some(_) => {
                    let ta = self.tag_at(a, ty)?;
                    let tb = self.tag_at(b, ty)?;
                    let res = self.entry_slot(4, "ord")?;
                    let o = self.ord_ints(ta, tb, false)?;
                    self.store(res, o.into())?;
                    let same = self.is_zero(o)?;
                    self.if_then(same, |cx| {
                        cx.each_variant(ta, ty, |cx, v| {
                            let parts = cx.parts_of(ty, Some(v))?;
                            let o = cx.cmp_parts(op, a, b, parts)?;
                            cx.store(res, o.into())
                        })
                    })?;
                    Ok(self
                        .builder
                        .build_load(i32t, res, "")
                        .map_err(e)?
                        .into_int_value())
                }
            },
            _ => Err(format!(
                "MIR to LLVM: ordering a `{}` is not lowered yet",
                self.tys.display(ty)
            )),
        }
    }

    /// -1 / 0 / 1 from `lt` and `gt`.
    fn ord_of(&self, lt: IntValue<'ctx>, gt: IntValue<'ctx>) -> R<IntValue<'ctx>> {
        let i32t = self.llcx.i32_type();
        let g = self.builder.build_int_z_extend(gt, i32t, "").map_err(e)?;
        let l = self.builder.build_int_z_extend(lt, i32t, "").map_err(e)?;
        self.builder.build_int_sub(g, l, "").map_err(e)
    }

    fn ord_ints(&self, x: IntValue<'ctx>, y: IntValue<'ctx>, signed: bool) -> R<IntValue<'ctx>> {
        let (lp, gp) = if signed {
            (IntPredicate::SLT, IntPredicate::SGT)
        } else {
            (IntPredicate::ULT, IntPredicate::UGT)
        };
        let lt = self.builder.build_int_compare(lp, x, y, "").map_err(e)?;
        let gt = self.builder.build_int_compare(gp, x, y, "").map_err(e)?;
        self.ord_of(lt, gt)
    }

    /// The first nonzero order among `parts`, else 0.
    fn cmp_parts(
        &mut self,
        op: KeyOp,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        parts: Vec<(u64, Ty, bool)>,
    ) -> R<IntValue<'ctx>> {
        let i32t = self.llcx.i32_type();
        let res = self.entry_slot(4, "ord")?;
        self.store(res, i32t.const_zero().into())?;
        for (off, t, boxed) in parts {
            let part: Part = (
                self.part_ptr(a, off, boxed)?,
                self.part_ptr(b, off, boxed)?,
                t,
            );
            let cur = self
                .builder
                .build_load(i32t, res, "")
                .map_err(e)?
                .into_int_value();
            let open = self.is_zero(cur)?;
            self.if_then(open, |cx| {
                let o = cx.cmp_call(op, part.0, part.1, part.2)?;
                cx.store(res, o.into())
            })?;
        }
        Ok(self
            .builder
            .build_load(i32t, res, "")
            .map_err(e)?
            .into_int_value())
    }

    /// Lexicographic order of `na` and `nb` elements, then by length.
    fn cmp_elems(
        &mut self,
        op: KeyOp,
        pa: PointerValue<'ctx>,
        na: IntValue<'ctx>,
        pb: PointerValue<'ctx>,
        nb: IntValue<'ctx>,
        el: Ty,
    ) -> R<IntValue<'ctx>> {
        let i32t = self.llcx.i32_type();
        let size = self.layout(el)?.size;
        let res = self.entry_slot(4, "ord")?;
        self.store(res, i32t.const_zero().into())?;
        let shorter = self
            .builder
            .build_int_compare(IntPredicate::ULT, na, nb, "")
            .map_err(e)?;
        let n = self
            .builder
            .build_select(shorter, na, nb, "")
            .map_err(e)?
            .into_int_value();
        self.count_loop(n, |cx, i| {
            let cur = cx
                .builder
                .build_load(i32t, res, "")
                .map_err(e)?
                .into_int_value();
            let open = cx.is_zero(cur)?;
            cx.if_then(open, |cx| {
                let x = cx.elem_ptr(pa, i, size)?;
                let y = cx.elem_ptr(pb, i, size)?;
                let o = cx.cmp_call(op, x, y, el)?;
                cx.store(res, o.into())
            })
        })?;
        let cur = self
            .builder
            .build_load(i32t, res, "")
            .map_err(e)?
            .into_int_value();
        let open = self.is_zero(cur)?;
        self.if_then(open, |cx| {
            let o = cx.ord_ints(na, nb, false)?;
            cx.store(res, o.into())
        })?;
        Ok(self
            .builder
            .build_load(i32t, res, "")
            .map_err(e)?
            .into_int_value())
    }
}
