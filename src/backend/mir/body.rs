//! Places, operands, rvalues, and every terminator but `Drop`.
//!
//! Every value lives in memory: a place is a pointer, an operand is copied
//! from its place (or a constant materialized into a temporary), and an
//! assignment copies `size` bytes. A scalar is loaded only to compute with
//! it. The calls themselves are `call.rs`'s.

use inkwell::module::Linkage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValue, BasicValueEnum, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use super::{Cx, R};
use crate::mir::layout::{self, SHARED_COUNTS};
use crate::mir::place_ty::place_ty;
use crate::mir::ty::{FloatTy, IntTy};
use crate::mir::{
    AbortReason, AggregateKind, BinOp, CastKind, Const, ConstKind, NullOp, Operand, Place,
    ProjElem, Rvalue, Statement, StatementKind, TerminatorKind, Ty, TyKind, UnOp,
};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn statement(&mut self, s: &Statement) -> R<()> {
        match &s.kind {
            StatementKind::Assign(place, rv) => {
                let ty = self.place_type(place)?;
                let dst = self.place_ptr(place)?;
                self.rvalue(dst, ty, rv)
            }
            StatementKind::SetDiscriminant(place, v) => {
                let ty = self.place_type(place)?;
                let dst = self.place_ptr(place)?;
                self.store_tag(dst, ty, v.0)
            }
            // Storage is the function's for its whole life, so it emits
            // nothing.
            StatementKind::FreeBox(place) => self.free_box_of(place),
            StatementKind::BorrowFlag(op) => self.borrow_flag(op),
            StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {
                Ok(())
            }
        }
    }

    pub(super) fn terminator(&mut self, kind: &TerminatorKind) -> R<()> {
        match kind {
            TerminatorKind::Goto { target } => {
                self.builder
                    .build_unconditional_branch(self.blocks[target.index()])
                    .map_err(e)?;
            }
            TerminatorKind::SwitchInt { discr, targets } => {
                let ty = self.operand_type(discr)?;
                let v = self.operand_scalar(discr)?.into_int_value();
                let it = v.get_type();
                let cases: Vec<_> = targets
                    .values
                    .iter()
                    .map(|&(val, bb)| {
                        let lo = val as u64;
                        let c = if it.get_bit_width() > 64 {
                            it.const_int_arbitrary_precision(&[lo, (val >> 64) as u64])
                        } else {
                            it.const_int(lo, false)
                        };
                        (c, self.blocks[bb.index()])
                    })
                    .collect();
                let _ = ty;
                self.builder
                    .build_switch(v, self.blocks[targets.otherwise.index()], &cases)
                    .map_err(e)?;
            }
            TerminatorKind::Return => {
                self.give_back_all()?;
                match self.ret_by_value {
                    Some(Some(t)) => {
                        let v = self.builder.build_load(t, self.locals[0], "").map_err(e)?;
                        if let Some(i) = v.as_instruction_value() {
                            let _ = i.set_alignment(1);
                        }
                        self.builder.build_return(Some(&v)).map_err(e)?;
                    }
                    _ => {
                        self.builder.build_return(None).map_err(e)?;
                    }
                }
            }
            TerminatorKind::Call {
                func,
                args,
                destination,
                target,
                ..
            } => self.call(func, args, destination, *target)?,
            TerminatorKind::Abort { reason } => {
                let msg = match reason {
                    AbortReason::Panic => "explicit panic",
                    AbortReason::Overflow => "integer overflow",
                    AbortReason::DivByZero => "division by zero",
                    AbortReason::BoundsCheck => "index out of bounds",
                    AbortReason::UnreachableArm => "entered unreachable code",
                };
                self.abort_with(msg)?;
            }
            TerminatorKind::Unreachable => {
                self.builder.build_unreachable().map_err(e)?;
            }
            TerminatorKind::Drop { .. } => return Err("`Drop` is drop.rs's".into()),
        }
        Ok(())
    }

    /// `karac_mir_abort_at(msg, at)`, which does not return: `msg` (or a
    /// `panic`'s own message), at the place `debug.rs` names. The function
    /// has one such call per message, which each site jumps to with its
    /// place, so an overflow check costs a branch where it is rather than
    /// a call (and a function full of them stays small enough to inline).
    pub(super) fn abort_with(&mut self, msg: &str) -> R<()> {
        let (at, at_n) = self.abort_site()?;
        let here = self.builder.get_insert_block().ok_or("no block")?;
        // Shims and helpers are built outside `lower_body`, so a block is
        // shared only within the function it was made in.
        let func = here.get_parent();
        let i = match self
            .abort_blocks
            .iter()
            .position(|a| a.0 == msg && a.1.get_parent() == func)
        {
            Some(i) => i,
            None => {
                let (s, n) = self.str_const(msg)?;
                let pt = self.ptr_ty();
                let i64t = self.llcx.i64_type();
                let f = self.extern_fn(
                    "karac_mir_abort_at",
                    self.llcx
                        .void_type()
                        .fn_type(&[pt.into(), i64t.into(), pt.into(), i64t.into()], false),
                );
                let block = self.llcx.append_basic_block(self.cur_fn()?, "abort");
                self.builder.position_at_end(block);
                let p = self.builder.build_phi(pt, "at").map_err(e)?;
                let l = self.builder.build_phi(i64t, "at.len").map_err(e)?;
                let (pv, lv) = (p.as_basic_value(), l.as_basic_value());
                self.builder
                    .build_call(f, &[s.into(), n.into(), pv.into(), lv.into()], "")
                    .map_err(e)?;
                self.builder.build_unreachable().map_err(e)?;
                self.builder.position_at_end(here);
                self.abort_blocks.push((msg.to_string(), block, p, l));
                self.abort_blocks.len() - 1
            }
        };
        let (_, block, p, l) = &self.abort_blocks[i];
        p.add_incoming(&[(&at, here)]);
        l.add_incoming(&[(&at_n, here)]);
        let block = *block;
        self.builder.build_unconditional_branch(block).map_err(e)?;
        Ok(())
    }

    // ── types ──────────────────────────────────────────────────────

    pub(super) fn place_type(&self, place: &Place) -> R<Ty> {
        Ok(place_ty(self.cur()?, self.tys, place)?.ty)
    }

    pub(super) fn operand_type(&self, op: &Operand) -> R<Ty> {
        match op {
            Operand::Copy(p) | Operand::Move(p) => self.place_type(p),
            Operand::Const(c) => Ok(c.ty),
        }
    }

    fn int_of(&self, ty: Ty) -> Option<IntTy> {
        match self.tys.kind(ty) {
            TyKind::Int(i) => Some(i),
            _ => None,
        }
    }

    // ── places ─────────────────────────────────────────────────────

    pub(super) fn projected_place_ptr(&mut self, place: &Place) -> R<PointerValue<'ctx>> {
        Ok(self.place_box(place)?.0)
    }

    /// A pointer to `place`'s storage, and the slot holding its box when
    /// the place is a boxed field ([`layout::boxed_field`]). An empty box
    /// slot on the way is filled, for a write after a move.
    pub(super) fn place_box(
        &mut self,
        place: &Place,
    ) -> R<(PointerValue<'ctx>, Option<PointerValue<'ctx>>)> {
        let body = self.cur()?;
        let mut slot = None;
        let mut ptr = self
            .locals
            .get(place.local.index())
            .copied()
            .ok_or_else(|| format!("no local {:?}", place.local))?;
        let mut ty = body.local(place.local).ty;
        let mut variant = None;
        for elem in &place.projection {
            match *elem {
                ProjElem::Field(f, fty) => {
                    let off =
                        layout::field_offset(self.tys, ty, variant, f.0).ok_or_else(|| {
                            format!("`{}` has no field {}", self.tys.display(ty), f.0)
                        })?;
                    // A field of a `shared` value is in the body its handle
                    // points at.
                    if let TyKind::Shared(_) = self.tys.kind(ty) {
                        let b = self.load_ptr(ptr)?;
                        let at = layout::shared_body_offset(self.tys, ty).unwrap_or(SHARED_COUNTS);
                        ptr = self.offset(b, at)?;
                    }
                    let boxed = layout::boxed_field(self.tys, ty, variant, f.0);
                    ptr = self.offset(ptr, off)?;
                    slot = None;
                    if boxed {
                        slot = Some(ptr);
                        ptr = self.box_at(ptr, fty)?;
                    }
                    ty = fty;
                    variant = None;
                }
                ProjElem::Downcast(v) => variant = Some(v.0),
                ProjElem::Deref => {
                    slot = None;
                    let (p, t) = self.deref(ptr, ty)?;
                    ptr = p;
                    ty = t;
                    variant = None;
                }
                ProjElem::Index(l) => {
                    let lp = self.locals[l.index()];
                    let i = self.load_i64(lp)?;
                    let (p, t) = self.index(ptr, ty, i)?;
                    slot = None;
                    ptr = p;
                    ty = t;
                    variant = None;
                }
                ProjElem::ConstIndex(k) => {
                    let (p, t) = self.index(ptr, ty, self.i64(k))?;
                    slot = None;
                    ptr = p;
                    ty = t;
                    variant = None;
                }
            }
        }
        Ok((ptr, slot))
    }

    /// `free_box(place)`: frees the box of the boxed field `place`, if it
    /// still holds one. Unlike [`Self::place_box`] it never makes one.
    fn free_box_of(&mut self, place: &Place) -> R<()> {
        let n = place.projection.len();
        let Some(&ProjElem::Field(f, _)) = place.projection.last() else {
            return Err("free_box of a place that is not a field".into());
        };
        let parent = Place {
            local: place.local,
            projection: place.projection[..n - 1].to_vec(),
        };
        let pt = place_ty(self.cur()?, self.tys, &parent)?;
        if !layout::boxed_field(self.tys, pt.ty, pt.variant, f.0) {
            return Ok(());
        }
        let p = self.place_ptr(&parent)?;
        let slot = self.offset(p, self.field_offset(pt.ty, pt.variant, f.0)?)?;
        self.free_box(slot)
    }

    /// The place `*p` for a `p` of type `ty` stored at `ptr`, and its type.
    fn deref(&mut self, ptr: PointerValue<'ctx>, ty: Ty) -> R<(PointerValue<'ctx>, Ty)> {
        Ok(match self.tys.kind(ty) {
            // A reference to a `str` or slice is the two words themselves.
            TyKind::Ref(p) | TyKind::MutRef(p)
                if matches!(self.tys.kind(p), TyKind::Str | TyKind::Slice(_)) =>
            {
                (ptr, p)
            }
            TyKind::Ref(p) | TyKind::MutRef(p) | TyKind::RawPtr { pointee: p, .. } => {
                (self.load_ptr(ptr)?, p)
            }
            TyKind::Shared(a) => {
                let b = self.load_ptr(ptr)?;
                let body = layout::shared_body_offset(self.tys, ty).unwrap_or(SHARED_COUNTS);
                (self.offset(b, body)?, self.tys.intern(TyKind::Adt(a)))
            }
            _ => {
                return Err(format!(
                    "MIR to LLVM: cannot dereference a `{}`",
                    self.tys.display(ty)
                ))
            }
        })
    }

    /// Element `i` of the sequence of type `ty` at `ptr`, and its type.
    fn index(
        &mut self,
        ptr: PointerValue<'ctx>,
        ty: Ty,
        i: IntValue<'ctx>,
    ) -> R<(PointerValue<'ctx>, Ty)> {
        let (base, elem) = match self.tys.kind(ty) {
            TyKind::Array(el, _) => (ptr, el),
            TyKind::Slice(el) | TyKind::Intrinsic(crate::mir::ty::IntrinsicTy::Vec(el)) => {
                (self.load_ptr(ptr)?, el)
            }
            _ => {
                return Err(format!(
                    "MIR to LLVM: cannot index a `{}`",
                    self.tys.display(ty)
                ))
            }
        };
        let size = self.layout(elem)?.size;
        let at = self
            .builder
            .build_int_mul(i, self.i64(size), "")
            .map_err(e)?;
        // SAFETY: the MIR bounds-checked `i` against the length.
        let p = unsafe {
            self.builder
                .build_in_bounds_gep(self.llcx.i8_type(), base, &[at], "elem")
                .map_err(e)?
        };
        Ok((p, elem))
    }

    // ── operands ───────────────────────────────────────────────────

    /// A pointer to the operand's value: its place, or a temporary holding
    /// the constant.
    pub(super) fn operand_ptr(&mut self, op: &Operand) -> R<PointerValue<'ctx>> {
        match op {
            Operand::Move(p) if !p.projection.is_empty() => {
                // Moving a boxed field out frees its box: the value goes
                // to a temporary first.
                let (q, slot) = self.place_box(p)?;
                let Some(slot) = slot else {
                    return Ok(q);
                };
                let ty = self.place_type(p)?;
                let size = self.layout(ty)?.size;
                let t = self.entry_slot(size.max(1) as u32, "unboxed")?;
                self.copy(t, q, ty)?;
                self.free_box(slot)?;
                Ok(t)
            }
            Operand::Copy(p) | Operand::Move(p) => self.place_ptr(p),
            Operand::Const(c) => {
                let size = self.layout(c.ty)?.size;
                let t = self.entry_slot(size.max(1) as u32, "const")?;
                self.store_const(t, c)?;
                Ok(t)
            }
        }
    }

    /// Copies the operand's value to `dst`.
    pub(super) fn store_operand(&mut self, dst: PointerValue<'ctx>, op: &Operand) -> R<()> {
        match op {
            Operand::Const(c) => self.store_const(dst, c),
            Operand::Copy(p) | Operand::Move(p) => {
                let ty = self.place_type(p)?;
                let (src, slot) = self.place_box(p)?;
                self.copy(dst, src, ty)?;
                match (op, slot) {
                    (Operand::Move(_), Some(slot)) => self.free_box(slot),
                    _ => Ok(()),
                }
            }
        }
    }

    /// Copies a value of type `ty` from `src` to `dst`.
    pub(super) fn copy(&self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, ty: Ty) -> R<()> {
        let size = self.layout(ty)?.size;
        if size == 0 || dst == src {
            return Ok(());
        }
        self.builder
            .build_memmove(dst, 1, src, 1, self.i64(size))
            .map_err(e)?;
        Ok(())
    }

    /// The scalar value of the operand, loaded as `llvm_ty` says.
    pub(super) fn operand_scalar(&mut self, op: &Operand) -> R<BasicValueEnum<'ctx>> {
        let ty = self.operand_type(op)?;
        if let Operand::Const(c) = op {
            if let Some(v) = self.scalar_const(c)? {
                return Ok(v);
            }
        }
        let p = self.operand_ptr(op)?;
        self.load(p, ty)
    }

    pub(super) fn load(&self, p: PointerValue<'ctx>, ty: Ty) -> R<BasicValueEnum<'ctx>> {
        let lt = self.llvm_ty(ty)?;
        let v = self.builder.build_load(lt, p, "").map_err(e)?;
        if let Some(i) = v.as_instruction_value() {
            let _ = i.set_alignment(1);
        }
        Ok(v)
    }

    pub(super) fn store(&self, p: PointerValue<'ctx>, v: BasicValueEnum<'ctx>) -> R<()> {
        let i = self.builder.build_store(p, v).map_err(e)?;
        let _ = i.set_alignment(1);
        Ok(())
    }

    // ── constants ──────────────────────────────────────────────────

    /// A scalar constant as an LLVM value, or `None` for one that is not a
    /// scalar.
    fn scalar_const(&self, c: &Const) -> R<Option<BasicValueEnum<'ctx>>> {
        Ok(Some(match (&c.kind, self.tys.kind(c.ty)) {
            (ConstKind::Scalar(v), TyKind::Int(_) | TyKind::Bool | TyKind::Char) => {
                let it = self.llvm_ty(c.ty)?.into_int_type();
                if it.get_bit_width() > 64 {
                    it.const_int_arbitrary_precision(&[*v as u64, (*v >> 64) as u64])
                        .into()
                } else {
                    it.const_int(*v as u64, false).into()
                }
            }
            (ConstKind::Float(bits), TyKind::Float(FloatTy::F64)) => self
                .llcx
                .f64_type()
                .const_float(f64::from_bits(*bits))
                .into(),
            (ConstKind::Float(bits), TyKind::Float(ft @ (FloatTy::F16 | FloatTy::BF16))) => {
                let v = ft.round(f64::from_bits(*bits));
                let h = match ft {
                    FloatTy::F16 => crate::mir::ty::f32_to_f16_bits(v as f32),
                    _ => ((v as f32).to_bits() >> 16) as u16,
                };
                self.llcx.i16_type().const_int(u64::from(h), false).into()
            }
            (ConstKind::Float(bits), TyKind::Float(FloatTy::F32)) => {
                // The builder keeps a float constant's `f64` bits.
                self.llcx
                    .f32_type()
                    .const_float(f64::from_bits(*bits))
                    .into()
            }
            _ => return Ok(None),
        }))
    }

    pub(super) fn store_const(&mut self, dst: PointerValue<'ctx>, c: &Const) -> R<()> {
        if let Some(v) = self.scalar_const(c)? {
            return self.store(dst, v);
        }
        match &c.kind {
            ConstKind::Unit | ConstKind::ZeroSized | ConstKind::FnDef(_) => Ok(()),
            ConstKind::Str(s) => {
                let (p, n) = self.str_const(s)?;
                self.store(dst, p.into())?;
                let at = self.offset(dst, 8)?;
                self.store(at, n.into())
            }
            ConstKind::Static(i) => {
                let g = self.static_ptr(*i)?;
                self.store(dst, g.into())
            }
            ConstKind::Scalar(v) => Err(format!(
                "MIR to LLVM: a scalar constant {v} of type `{}`",
                self.tys.display(c.ty)
            )),
            ConstKind::Float(_) => Err(format!(
                "MIR to LLVM: a float constant of type `{}`",
                self.tys.display(c.ty)
            )),
        }
    }

    /// A `str` constant: a pointer to its bytes in a private global, and
    /// its length.
    pub(super) fn str_const(&mut self, s: &str) -> R<(PointerValue<'ctx>, IntValue<'ctx>)> {
        let n = self.i64(s.len() as u64);
        if s.is_empty() {
            return Ok((self.ptr_ty().const_null(), n));
        }
        // Byte by byte: `build_global_string_ptr` goes through a C string
        // and so stops at an interior NUL (`"x\u{0}y"`).
        let arr = self.llcx.const_string(s.as_bytes(), true);
        let g = self.module.add_global(arr.get_type(), None, "str");
        g.set_initializer(&arr);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.set_unnamed_addr(true);
        Ok((g.as_pointer_value(), n))
    }

    // ── rvalues ────────────────────────────────────────────────────

    fn rvalue(&mut self, dst: PointerValue<'ctx>, ty: Ty, rv: &Rvalue) -> R<()> {
        match rv {
            Rvalue::Use(op) => self.store_operand(dst, op),
            Rvalue::Ref(_, place) => {
                let pty = self.place_type(place)?;
                let p = self.place_ptr(place)?;
                if matches!(self.tys.kind(pty), TyKind::Str | TyKind::Slice(_)) {
                    // A reference to a `str` or slice is its two words.
                    self.copy(dst, p, pty)
                } else {
                    self.store(dst, p.into())
                }
            }
            Rvalue::Retain(place) => {
                let pty = self.place_type(place)?;
                let p = self.place_ptr(place)?;
                let h = self.load_ptr(p)?;
                self.retain(h)?;
                self.copy(dst, p, pty)
            }
            Rvalue::BinaryOp(op, a, b) => {
                let at = self.operand_type(a)?;
                if let Some((et, n)) = self.vector_of(at) {
                    // Lane by lane; a comparison's lanes are bools.
                    let (dt, _) = self
                        .vector_of(ty)
                        .ok_or("a lane operation into no vector")?;
                    let (pa, pb) = (self.operand_ptr(a)?, self.operand_ptr(b)?);
                    let bt = self.operand_type(b)?;
                    let (bet, _) = self.vector_of(bt).unwrap_or((bt, 0));
                    let (es, bs, ds) = (
                        self.layout(et)?.size,
                        self.layout(bet)?.size,
                        self.layout(dt)?.size,
                    );
                    for i in 0..n {
                        let x = self.load(self.offset(pa, i * es)?, et)?;
                        let y = self.load(self.offset(pb, i * bs)?, bet)?;
                        let v = self.binop(*op, et, x, y)?;
                        self.store(self.offset(dst, i * ds)?, v)?;
                    }
                    return Ok(());
                }
                let x = self.operand_scalar(a)?;
                let y = self.operand_scalar(b)?;
                let v = self.binop(*op, at, x, y)?;
                self.store(dst, v)
            }
            Rvalue::CheckedBinaryOp(op, a, b)
                if self.vector_of(self.operand_type(a)?).is_some() =>
            {
                let at = self.operand_type(a)?;
                let (et, n) = self.vector_of(at).ok_or("no vector")?;
                let (pa, pb) = (self.operand_ptr(a)?, self.operand_ptr(b)?);
                let es = self.layout(et)?.size;
                let mut any = self.llcx.bool_type().const_zero();
                for i in 0..n {
                    let x = self.load(self.offset(pa, i * es)?, et)?.into_int_value();
                    let y = self.load(self.offset(pb, i * es)?, et)?.into_int_value();
                    let (v, o) = self.checked(*op, et, x, y)?;
                    self.store(self.offset(dst, i * es)?, v.into())?;
                    any = self.builder.build_or(any, o, "").map_err(e)?;
                }
                let off1 = layout::field_offset(self.tys, ty, None, 1)
                    .ok_or("a checked operation's result is not a pair")?;
                let o = self
                    .builder
                    .build_int_z_extend(any, self.llcx.i8_type(), "")
                    .map_err(e)?;
                let at = self.offset(dst, off1)?;
                self.store(at, o.into())
            }
            Rvalue::CheckedBinaryOp(op, a, b) => {
                let at = self.operand_type(a)?;
                let x = self.operand_scalar(a)?.into_int_value();
                let y = self.operand_scalar(b)?.into_int_value();
                let (v, o) = self.checked(*op, at, x, y)?;
                let off1 = layout::field_offset(self.tys, ty, None, 1)
                    .ok_or("a checked operation's result is not a pair")?;
                self.store(dst, v.into())?;
                let o = self
                    .builder
                    .build_int_z_extend(o, self.llcx.i8_type(), "")
                    .map_err(e)?;
                let at = self.offset(dst, off1)?;
                self.store(at, o.into())
            }
            Rvalue::UnaryOp(op, a) => {
                let at = self.operand_type(a)?;
                if let Some((et, n)) = self.vector_of(at) {
                    let (src, es) = (self.operand_ptr(a)?, self.layout(et)?.size);
                    for i in 0..n {
                        let x = self.load(self.offset(src, i * es)?, et)?;
                        let v = self.unop_value(*op, et, x)?;
                        self.store(self.offset(dst, i * es)?, v)?;
                    }
                    return Ok(());
                }
                let x = self.operand_scalar(a)?;
                let v = self.unop_value(*op, at, x)?;
                self.store(dst, v)
            }
            Rvalue::Cast(kind, a, to) => self.cast(dst, *kind, a, *to),
            Rvalue::Discriminant(place) => {
                let pty = self.place_type(place)?;
                let p = self.place_ptr(place)?;
                let tag = self.load_tag(p, pty)?;
                let it = self.llvm_ty(ty)?.into_int_type();
                let v = self
                    .builder
                    .build_int_z_extend_or_bit_cast(tag, it, "")
                    .map_err(e)?;
                self.store(dst, v.into())
            }
            Rvalue::Len(place) => {
                let pty = self.place_type(place)?;
                let n = match self.tys.kind(pty) {
                    TyKind::Array(_, n) => self.i64(n),
                    TyKind::Slice(_)
                    | TyKind::Str
                    | TyKind::Intrinsic(crate::mir::ty::IntrinsicTy::Vec(_))
                    | TyKind::Intrinsic(crate::mir::ty::IntrinsicTy::String) => {
                        let p = self.place_ptr(place)?;
                        self.load_i64(self.offset(p, 8)?)?
                    }
                    _ => {
                        return Err(format!(
                            "MIR to LLVM: the length of a `{}`",
                            self.tys.display(pty)
                        ))
                    }
                };
                let it = self.llvm_ty(ty)?.into_int_type();
                let n = self
                    .builder
                    .build_int_truncate_or_bit_cast(n, it, "")
                    .map_err(e)?;
                self.store(dst, n.into())
            }
            Rvalue::Aggregate(kind, ops) => self.aggregate(dst, ty, kind, ops),
            Rvalue::NullaryOp(op, t) => {
                let l = self.layout(*t)?;
                let v = match op {
                    NullOp::SizeOf => l.size,
                    NullOp::AlignOf => l.align,
                };
                let it = self.llvm_ty(ty)?.into_int_type();
                self.store(dst, it.const_int(v, false).into())
            }
        }
    }

    /// `strong += 1` on the `shared` box `h`.
    pub(super) fn retain(&self, h: PointerValue<'ctx>) -> R<()> {
        let n = self.load_i64(h)?;
        let n = self.builder.build_int_add(n, self.i64(1), "").map_err(e)?;
        self.builder.build_store(h, n).map_err(e)?;
        Ok(())
    }

    /// `op` on the scalar `x` of type `at`.
    fn unop_value(&mut self, op: UnOp, at: Ty, x: BasicValueEnum<'ctx>) -> R<BasicValueEnum<'ctx>> {
        // `-MIN` overflows (and negating a non-zero unsigned value
        // leaves its range): C8 traps it, as the interpreter does.
        if let (UnOp::Neg, TyKind::Int(it)) = (op, self.tys.kind(at)) {
            let x = x.into_int_value();
            let signed = matches!(
                it,
                IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Isize
            );
            let bad = if signed {
                let w = x.get_type().get_bit_width();
                let min = x
                    .get_type()
                    .const_int(1, false)
                    .const_shl(x.get_type().const_int(u64::from(w - 1), false));
                self.builder.build_int_compare(IntPredicate::EQ, x, min, "")
            } else {
                self.builder
                    .build_int_compare(IntPredicate::NE, x, x.get_type().const_zero(), "")
            }
            .map_err(e)?;
            let ok = self.builder.build_not(bad, "").map_err(e)?;
            self.abort_unless(ok, "integer overflow")?;
        }
        let b = &self.builder;
        let v: BasicValueEnum = match (op, self.tys.kind(at)) {
            (UnOp::Not, TyKind::Bool) => b
                .build_xor(
                    x.into_int_value(),
                    self.llcx.i8_type().const_int(1, false),
                    "",
                )
                .map_err(e)?
                .into(),
            (UnOp::Not, _) => b.build_not(x.into_int_value(), "").map_err(e)?.into(),
            (UnOp::Neg, TyKind::Float(FloatTy::F16 | FloatTy::BF16)) => b
                .build_xor(
                    x.into_int_value(),
                    self.llcx.i16_type().const_int(0x8000, false),
                    "",
                )
                .map_err(e)?
                .into(),
            (UnOp::Neg, TyKind::Float(_)) => b
                .build_float_neg(x.into_float_value(), "")
                .map_err(e)?
                .into(),
            (UnOp::Neg, _) => b.build_int_neg(x.into_int_value(), "").map_err(e)?.into(),
        };
        Ok(v)
    }

    /// The lane type and count of a lane vector type.
    pub(super) fn vector_of(&self, t: Ty) -> Option<(Ty, u64)> {
        match self.tys.kind(t) {
            TyKind::Array(et, n) if self.tys.is_vector(t) => Some((et, n)),
            _ => None,
        }
    }

    pub(super) fn binop(
        &self,
        op: BinOp,
        ty: Ty,
        x: BasicValueEnum<'ctx>,
        y: BasicValueEnum<'ctx>,
    ) -> R<BasicValueEnum<'ctx>> {
        let b = &self.builder;
        let bool8 = |v: IntValue<'ctx>| -> R<BasicValueEnum<'ctx>> {
            Ok(b.build_int_z_extend(v, self.llcx.i8_type(), "")
                .map_err(e)?
                .into())
        };
        if let TyKind::Float(_) = self.tys.kind(ty) {
            let narrow = self.narrow_float(ty);
            let (x, y) = (self.float_of(x, ty)?, self.float_of(y, ty)?);
            if let (Some(ft), BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem) =
                (narrow, op)
            {
                let r = match op {
                    BinOp::Add => b.build_float_add(x, y, ""),
                    BinOp::Sub => b.build_float_sub(x, y, ""),
                    BinOp::Mul => b.build_float_mul(x, y, ""),
                    BinOp::Div => b.build_float_div(x, y, ""),
                    _ => b.build_float_rem(x, y, ""),
                }
                .map_err(e)?;
                return Ok(self.narrow_bits(r, ft)?.into());
            }
            let cmp = |p| -> R<BasicValueEnum<'ctx>> {
                bool8(b.build_float_compare(p, x, y, "").map_err(e)?)
            };
            return match op {
                BinOp::Add => Ok(b.build_float_add(x, y, "").map_err(e)?.into()),
                BinOp::Sub => Ok(b.build_float_sub(x, y, "").map_err(e)?.into()),
                BinOp::Mul => Ok(b.build_float_mul(x, y, "").map_err(e)?.into()),
                BinOp::Div => Ok(b.build_float_div(x, y, "").map_err(e)?.into()),
                BinOp::Rem => Ok(b.build_float_rem(x, y, "").map_err(e)?.into()),
                BinOp::Eq => cmp(FloatPredicate::OEQ),
                BinOp::Ne => cmp(FloatPredicate::UNE),
                BinOp::Lt => cmp(FloatPredicate::OLT),
                BinOp::Le => cmp(FloatPredicate::OLE),
                BinOp::Gt => cmp(FloatPredicate::OGT),
                BinOp::Ge => cmp(FloatPredicate::OGE),
                _ => Err(format!("MIR to LLVM: {} on floats", op.name())),
            };
        }
        let signed = self.int_of(ty).is_some_and(IntTy::signed);
        if !x.is_int_value() || !y.is_int_value() {
            return Err(format!(
                "MIR to LLVM: {} on a `{}`",
                op.name(),
                self.tys.display(ty)
            ));
        }
        let (x, mut y) = (x.into_int_value(), y.into_int_value());
        if matches!(op, BinOp::Shl | BinOp::Shr) && y.get_type() != x.get_type() {
            y = b
                .build_int_cast_sign_flag(y, x.get_type(), false, "")
                .map_err(e)?;
        }
        let cmp = |s: IntPredicate, u: IntPredicate| -> R<BasicValueEnum<'ctx>> {
            bool8(
                b.build_int_compare(if signed { s } else { u }, x, y, "")
                    .map_err(e)?,
            )
        };
        Ok(match op {
            BinOp::Add => b.build_int_add(x, y, "").map_err(e)?.into(),
            BinOp::Sub => b.build_int_sub(x, y, "").map_err(e)?.into(),
            BinOp::Mul => b.build_int_mul(x, y, "").map_err(e)?.into(),
            BinOp::Div if signed => b.build_int_signed_div(x, y, "").map_err(e)?.into(),
            BinOp::Div => b.build_int_unsigned_div(x, y, "").map_err(e)?.into(),
            BinOp::Rem if signed => b.build_int_signed_rem(x, y, "").map_err(e)?.into(),
            BinOp::Rem => b.build_int_unsigned_rem(x, y, "").map_err(e)?.into(),
            BinOp::BitAnd => b.build_and(x, y, "").map_err(e)?.into(),
            BinOp::BitOr => b.build_or(x, y, "").map_err(e)?.into(),
            BinOp::BitXor => b.build_xor(x, y, "").map_err(e)?.into(),
            BinOp::Shl => b.build_left_shift(x, y, "").map_err(e)?.into(),
            BinOp::Shr => b.build_right_shift(x, y, signed, "").map_err(e)?.into(),
            BinOp::Eq => cmp(IntPredicate::EQ, IntPredicate::EQ)?,
            BinOp::Ne => cmp(IntPredicate::NE, IntPredicate::NE)?,
            BinOp::Lt => cmp(IntPredicate::SLT, IntPredicate::ULT)?,
            BinOp::Le => cmp(IntPredicate::SLE, IntPredicate::ULE)?,
            BinOp::Gt => cmp(IntPredicate::SGT, IntPredicate::UGT)?,
            BinOp::Ge => cmp(IntPredicate::SGE, IntPredicate::UGE)?,
        })
    }

    /// `x op y` and whether it overflowed `ty`.
    pub(super) fn checked(
        &self,
        op: BinOp,
        ty: Ty,
        x: IntValue<'ctx>,
        y: IntValue<'ctx>,
    ) -> R<(IntValue<'ctx>, IntValue<'ctx>)> {
        let signed = self.int_of(ty).is_some_and(IntTy::signed);
        let it = x.get_type();
        let bits = it.get_bit_width();
        let b = &self.builder;
        let intrinsic = |name: &str| -> R<(IntValue<'ctx>, IntValue<'ctx>)> {
            let i = inkwell::intrinsics::Intrinsic::find(name)
                .ok_or_else(|| format!("no intrinsic {name}"))?;
            let f = i
                .get_declaration(&self.module, &[it.into()])
                .ok_or_else(|| format!("no declaration of {name}"))?;
            let r = b
                .build_call(f, &[x.into(), y.into()], "")
                .map_err(e)?
                .try_as_basic_value()
                .basic()
                .ok_or("an overflow intrinsic returns nothing")?
                .into_struct_value();
            let v = b.build_extract_value(r, 0, "").map_err(e)?.into_int_value();
            let o = b.build_extract_value(r, 1, "").map_err(e)?.into_int_value();
            Ok((v, o))
        };
        // `x ± c` for a constant `c` overflows exactly when `x` is past a
        // bound: a compare and a `nsw`/`nuw` operation, which LLVM's loop
        // and range analyses see through where they do not see through
        // the overflow intrinsics (a bit-count loop becomes `popcnt`).
        let (cx, cy) = (x.is_const(), y.is_const());
        if matches!(op, BinOp::Add | BinOp::Sub) && bits <= 64 && (cy || (cx && op == BinOp::Add)) {
            let (v, c) = if cy { (x, y) } else { (y, x) };
            let width = |n: i128| -> i128 {
                if signed {
                    n
                } else {
                    n & ((1i128 << bits) - 1)
                }
            };
            let c = if signed {
                c.get_sign_extended_constant().map(i128::from)
            } else {
                c.get_zero_extended_constant().map(i128::from)
            };
            if let Some(c) = c.map(width) {
                let (min, max) = if signed {
                    (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
                } else {
                    (0, (1i128 << bits) - 1)
                };
                // The overflow test as `v pred bound`, or none when `c` is 0.
                let test = match (op, c.signum()) {
                    (_, 0) => None,
                    (BinOp::Add, 1) | (BinOp::Sub, -1) => {
                        let bound = if op == BinOp::Add { max - c } else { max + c };
                        Some((
                            if signed {
                                IntPredicate::SGT
                            } else {
                                IntPredicate::UGT
                            },
                            bound,
                        ))
                    }
                    _ => {
                        let bound = if op == BinOp::Add { min - c } else { min + c };
                        Some((
                            if signed {
                                IntPredicate::SLT
                            } else {
                                IntPredicate::ULT
                            },
                            bound,
                        ))
                    }
                };
                let o = match test {
                    None => self.llcx.bool_type().const_zero(),
                    Some((pred, bound)) => {
                        let bound = it.const_int(bound as u64, signed);
                        b.build_int_compare(pred, v, bound, "").map_err(e)?
                    }
                };
                let r = match (op, signed) {
                    (BinOp::Add, true) => b.build_int_nsw_add(x, y, ""),
                    (BinOp::Add, false) => b.build_int_nuw_add(x, y, ""),
                    (_, true) => b.build_int_nsw_sub(x, y, ""),
                    (_, false) => b.build_int_nuw_sub(x, y, ""),
                }
                .map_err(e)?;
                return Ok((r, o));
            }
        }
        let s = if signed { "s" } else { "u" };
        match op {
            BinOp::Add => intrinsic(&format!("llvm.{s}add.with.overflow")),
            BinOp::Sub => intrinsic(&format!("llvm.{s}sub.with.overflow")),
            BinOp::Mul => intrinsic(&format!("llvm.{s}mul.with.overflow")),
            BinOp::Shl | BinOp::Shr => {
                let y = b.build_int_cast_sign_flag(y, it, false, "").map_err(e)?;
                let o = b
                    .build_int_compare(
                        IntPredicate::UGE,
                        y,
                        it.const_int(u64::from(bits), false),
                        "",
                    )
                    .map_err(e)?;
                let masked = b
                    .build_and(y, it.const_int(u64::from(bits - 1), false), "")
                    .map_err(e)?;
                let v = match op {
                    BinOp::Shl => b.build_left_shift(x, masked, "").map_err(e)?,
                    _ => b.build_right_shift(x, masked, signed, "").map_err(e)?,
                };
                Ok((v, o))
            }
            BinOp::Div | BinOp::Rem => {
                // `MIN / -1` overflows; division by zero is its own abort,
                // checked before this.
                let o = if signed {
                    let min = it.const_int(1u64 << ((bits - 1).min(63)), false);
                    let min = if bits > 64 {
                        b.build_left_shift(
                            it.const_int(1, false),
                            it.const_int(u64::from(bits - 1), false),
                            "",
                        )
                        .map_err(e)?
                    } else {
                        min
                    };
                    let is_min = b
                        .build_int_compare(IntPredicate::EQ, x, min, "")
                        .map_err(e)?;
                    let neg1 = b
                        .build_int_compare(IntPredicate::EQ, y, it.const_all_ones(), "")
                        .map_err(e)?;
                    b.build_and(is_min, neg1, "").map_err(e)?
                } else {
                    self.llcx.bool_type().const_zero()
                };
                // Divide by 1 where it would overflow, so the instruction
                // itself never traps.
                let safe_y = b
                    .build_select(o, it.const_int(1, false), y, "")
                    .map_err(e)?
                    .into_int_value();
                let v = self
                    .binop(op, ty, x.into(), safe_y.into())?
                    .into_int_value();
                Ok((v, o))
            }
            _ => Err(format!("MIR to LLVM: checked {}", op.name())),
        }
    }

    fn cast(&mut self, dst: PointerValue<'ctx>, kind: CastKind, a: &Operand, to: Ty) -> R<()> {
        let from = self.operand_type(a)?;
        if self.narrow_float(from).is_some() || self.narrow_float(to).is_some() {
            return self.narrow_cast(dst, kind, a, from, to);
        }
        match kind {
            CastKind::RefToPtr | CastKind::PtrToPtr => return self.store_operand(dst, a),
            CastKind::Downgrade => {
                // A new weak handle to what a `ref shared T`, `ref weak T`
                // or `ref Option[shared T]` reaches: one more weak count;
                // nothing gives the empty (null) weak handle.
                let h = self.handle_behind(a)?;
                let live = self.builder.build_is_not_null(h, "").map_err(e)?;
                self.if_then(live, |cx| {
                    let w = cx.offset(h, 8)?;
                    let n = cx.load_i64(w)?;
                    let n = cx.builder.build_int_add(n, cx.i64(1), "").map_err(e)?;
                    cx.builder.build_store(w, n).map_err(e)?;
                    Ok(())
                })?;
                return self.store(dst, h.into());
            }
            CastKind::Upgrade => return self.upgrade(dst, a, to),
            CastKind::Erase => return self.erase(dst, a, from, to),
            _ => {}
        }
        let x = self.operand_scalar(a)?;
        let tt: BasicTypeEnum = self.llvm_ty(to)?;
        let b = &self.builder;
        let src_signed = self.int_of(from).is_some_and(IntTy::signed);
        let v: BasicValueEnum = match kind {
            CastKind::IntToInt
            | CastKind::IntToChar
            | CastKind::CharToInt
            | CastKind::BoolToInt => b
                .build_int_cast_sign_flag(x.into_int_value(), tt.into_int_type(), src_signed, "")
                .map_err(e)?
                .into(),
            CastKind::IntToFloat if src_signed => b
                .build_signed_int_to_float(x.into_int_value(), tt.into_float_type(), "")
                .map_err(e)?
                .into(),
            CastKind::IntToFloat => b
                .build_unsigned_int_to_float(x.into_int_value(), tt.into_float_type(), "")
                .map_err(e)?
                .into(),
            CastKind::FloatToInt => {
                // Saturating, NaN to 0, as the interpreter's `as` is.
                let dst_signed = self.int_of(to).is_some_and(IntTy::signed);
                let name = if dst_signed {
                    "llvm.fptosi.sat"
                } else {
                    "llvm.fptoui.sat"
                };
                let i = inkwell::intrinsics::Intrinsic::find(name).ok_or("no fpto*i.sat")?;
                let f = i
                    .get_declaration(&self.module, &[tt, x.get_type()])
                    .ok_or("no fpto*i.sat declaration")?;
                b.build_call(f, &[x.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("fpto*i.sat returns nothing")?
            }
            CastKind::FloatToFloat => b
                .build_float_cast(x.into_float_value(), tt.into_float_type(), "")
                .map_err(e)?
                .into(),
            _ => unreachable!("handled above"),
        };
        self.store(dst, v)
    }

    /// A cast from or to `f16` / `bf16`, through `f32` as the interpreter
    /// rounds: an integer or an `f64` reaches `f32` first.
    fn narrow_cast(
        &mut self,
        dst: PointerValue<'ctx>,
        kind: CastKind,
        a: &Operand,
        from: Ty,
        to: Ty,
    ) -> R<()> {
        let x = self.operand_scalar(a)?;
        let f32t = self.llcx.f32_type();
        let f64t = self.llcx.f64_type();
        // The source as an `f32`, or an integer as it is.
        let v: BasicValueEnum = match kind {
            CastKind::IntToFloat => {
                let signed = self.int_of(from).is_some_and(IntTy::signed);
                let b = &self.builder;
                let w = if signed {
                    b.build_signed_int_to_float(x.into_int_value(), f64t, "")
                } else {
                    b.build_unsigned_int_to_float(x.into_int_value(), f64t, "")
                }
                .map_err(e)?;
                b.build_float_trunc(w, f32t, "").map_err(e)?.into()
            }
            CastKind::FloatToFloat | CastKind::FloatToInt => {
                let f = self.float_of(x, from)?;
                if self.narrow_float(from).is_some() {
                    f.into()
                } else {
                    self.builder
                        .build_float_cast(f, f32t, "")
                        .map_err(e)?
                        .into()
                }
            }
            _ => {
                return Err(format!(
                    "MIR to LLVM: a cast from `{}` to `{}` is not lowered yet",
                    self.tys.display(from),
                    self.tys.display(to)
                ))
            }
        };
        if let Some(ft) = self.narrow_float(to) {
            let h = self.narrow_bits(v.into_float_value(), ft)?;
            return self.store(dst, h.into());
        }
        // From a narrow float to a wider float or an integer: the widened
        // value, cast on.
        let f = v.into_float_value();
        let tt = self.llvm_ty(to)?;
        let out: BasicValueEnum = match kind {
            CastKind::FloatToInt => {
                let signed = self.int_of(to).is_some_and(IntTy::signed);
                let name = if signed {
                    "llvm.fptosi.sat"
                } else {
                    "llvm.fptoui.sat"
                };
                let i = inkwell::intrinsics::Intrinsic::find(name).ok_or("no fpto*i.sat")?;
                let fd = i
                    .get_declaration(&self.module, &[tt, f32t.into()])
                    .ok_or("no fpto*i.sat declaration")?;
                self.builder
                    .build_call(fd, &[f.into()], "")
                    .map_err(e)?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("fpto*i.sat returns nothing")?
            }
            _ => self
                .builder
                .build_float_cast(f, tt.into_float_type(), "")
                .map_err(e)?
                .into(),
        };
        self.store(dst, out)
    }

    /// A closure or function item as an erased `Fn` value.
    fn erase(&mut self, dst: PointerValue<'ctx>, a: &Operand, from: Ty, to: Ty) -> R<()> {
        self.erase_into(dst, a, from, to)
    }

    /// `weak T` to `Option[shared T]`: `Some` with one more strong count
    /// while the object lives, else `None`.
    fn upgrade(&mut self, dst: PointerValue<'ctx>, a: &Operand, to: Ty) -> R<()> {
        let (adt, _) = self
            .tys
            .tcx()
            .adt_of(to)
            .ok_or("an upgrade does not make an Option")?;
        let some = adt
            .variants
            .iter()
            .position(|v| v.name == "Some")
            .ok_or("an upgrade's Option has no Some")? as u32;
        let none = 1 - some;
        let h = self.handle_behind(a)?;
        self.store_tag(dst, to, none)?;
        let live = self.builder.build_is_not_null(h, "").map_err(e)?;
        self.if_then(live, |cx| {
            let n = cx.load_i64(h)?;
            let alive = cx
                .builder
                .build_int_compare(IntPredicate::NE, n, cx.i64(0), "")
                .map_err(e)?;
            cx.if_then(alive, |cx| {
                cx.retain(h)?;
                cx.store_tag(dst, to, some)?;
                let off =
                    layout::field_offset(cx.tys, to, Some(some), 0).ok_or("Some has no field")?;
                let at = cx.offset(dst, off)?;
                cx.store(at, h.into())
            })
        })
    }

    /// The handle (null for none) that the operand reaches through its
    /// references: a `shared T` or `weak T` slot holds it, and an
    /// `Option[shared T]` holds it in `Some`.
    fn handle_behind(&mut self, a: &Operand) -> R<PointerValue<'ctx>> {
        let mut t = self.operand_type(a)?;
        let mut p = self.operand_ptr(a)?;
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            p = self.load_ptr(p)?;
            t = i;
        }
        match self.tys.kind(t) {
            TyKind::Shared(_) | TyKind::Weak(_) => self.load_ptr(p),
            TyKind::Adt(_) => {
                let (adt, _) = self.tys.tcx().adt_of(t).ok_or("not an Option")?;
                let some = adt
                    .variants
                    .iter()
                    .position(|v| v.name == "Some")
                    .ok_or("a handle behind a non-Option")? as u32;
                let off = self.field_offset(t, Some(some), 0)?;
                let tag = self.load_tag(p, t)?;
                let is_some = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        tag.get_type().const_int(u64::from(some), false),
                        "",
                    )
                    .map_err(e)?;
                let h = self.load_ptr(self.offset(p, off)?)?;
                let null = self.ptr_ty().const_null();
                Ok(self
                    .builder
                    .build_select(is_some, h, null, "")
                    .map_err(e)?
                    .into_pointer_value())
            }
            _ => Err(format!(
                "MIR to LLVM: a handle behind a `{}`",
                self.tys.display(t)
            )),
        }
    }

    // ── aggregates and enums ───────────────────────────────────────

    fn aggregate(
        &mut self,
        dst: PointerValue<'ctx>,
        ty: Ty,
        kind: &AggregateKind,
        ops: &[Operand],
    ) -> R<()> {
        match kind {
            AggregateKind::Tuple | AggregateKind::Closure { .. } => {
                for (i, op) in ops.iter().enumerate() {
                    let off = self.field_offset(ty, None, i as u32)?;
                    let at = self.offset(dst, off)?;
                    self.store_operand(at, op)?;
                }
                Ok(())
            }
            AggregateKind::Array(el) => {
                let size = self.layout(*el)?.size;
                // A repeat literal (`[0; 65536]`) comes as N copies of one
                // place: a loop, not N stores.
                if ops.len() > 16
                    && matches!(&ops[0], Operand::Copy(_))
                    && ops.iter().all(|o| o == &ops[0])
                {
                    let src = self.operand_ptr(&ops[0])?;
                    let n = self.i64(ops.len() as u64);
                    return self.count_loop(n, |cx, i| {
                        let at = cx.elem_ptr(dst, i, size)?;
                        cx.copy(at, src, *el)
                    });
                }
                for (i, op) in ops.iter().enumerate() {
                    let at = self.offset(dst, size * i as u64)?;
                    self.store_operand(at, op)?;
                }
                Ok(())
            }
            AggregateKind::Adt { ty: aty, variant } => self.fill_adt(dst, *aty, variant.0, ops),
            AggregateKind::Shared { ty: sty, variant } => {
                let body = layout::body_layout(self.tys, *sty)
                    .ok_or("a shared type has no body layout")?;
                let off = layout::shared_body_offset(self.tys, *sty).unwrap_or(SHARED_COUNTS);
                let malloc = self.libc_fn("malloc");
                let h = self
                    .builder
                    .build_call(malloc, &[self.i64(off + body.size.max(1)).into()], "box")
                    .map_err(e)?
                    .try_as_basic_value()
                    .basic()
                    .ok_or("malloc returns nothing")?
                    .into_pointer_value();
                self.builder.build_store(h, self.i64(1)).map_err(e)?;
                let w = self.offset(h, 8)?;
                self.builder.build_store(w, self.i64(0)).map_err(e)?;
                let b = self.offset(h, off)?;
                self.fill_adt(b, *sty, variant.0, ops)?;
                self.store(dst, h.into())
            }
        }
    }

    /// Writes variant `v` of the ADT `ty` (plain, or the body of a
    /// `shared` one) with `ops` as its fields at `dst`.
    fn fill_adt(&mut self, dst: PointerValue<'ctx>, ty: Ty, v: u32, ops: &[Operand]) -> R<()> {
        let enum_ = layout::enum_tag(self.tys, ty).is_some();
        let var = enum_.then_some(v);
        for (i, op) in ops.iter().enumerate() {
            let off = self.field_offset(ty, var, i as u32)?;
            let mut at = self.offset(dst, off)?;
            if layout::boxed_field(self.tys, ty, var, i as u32) {
                let fty = self
                    .tys
                    .field_ty(ty, var, i as u32)
                    .ok_or("a boxed field has no type")?;
                at = self.new_box(at, fty)?;
            }
            self.store_operand(at, op)?;
        }
        if enum_ {
            // `dst` is the body itself, also for a `shared` type.
            let plain = match self.tys.kind(ty) {
                TyKind::Shared(a) => self.tys.intern(TyKind::Adt(a)),
                _ => ty,
            };
            self.store_tag(dst, plain, v)?;
        }
        Ok(())
    }

    /// The tag of the enum of type `ty` at `p` (through the handle for a
    /// `shared` one), as an integer of its own width.
    pub(super) fn load_tag(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<IntValue<'ctx>> {
        let (p, ty) = self.through_shared(p, ty)?;
        self.tag_at(p, ty)
    }

    /// The tag of the enum body of type `ty` at `p` (no `shared` handle to
    /// go through), as an integer of [`layout::enum_tag`]'s width. An
    /// `Option` held as a bare pointer ([`layout::null_niche`]) is `None`
    /// when the pointer is null.
    pub(super) fn tag_at(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<IntValue<'ctx>> {
        let Some(tag) = layout::enum_tag(self.tys, ty) else {
            // A struct (or an enum with no variants) has discriminant 0.
            return Ok(self.llcx.i8_type().const_zero());
        };
        let it = self.int_ty(tag.size as u32 * 8);
        if let Some((none, some)) = layout::null_niche(self.tys, ty) {
            let q = self.load_ptr(p)?;
            let null = self.builder.build_is_null(q, "").map_err(e)?;
            let v = self
                .builder
                .build_select(
                    null,
                    it.const_int(u64::from(none), false),
                    it.const_int(u64::from(some), false),
                    "tag",
                )
                .map_err(e)?;
            return Ok(v.into_int_value());
        }
        let v = self.builder.build_load(it, p, "tag").map_err(e)?;
        Ok(v.into_int_value())
    }

    pub(super) fn store_tag(&mut self, p: PointerValue<'ctx>, ty: Ty, v: u32) -> R<()> {
        let (p, ty) = self.through_shared(p, ty)?;
        let Some(tag) = layout::enum_tag(self.tys, ty) else {
            return Ok(());
        };
        if let Some((none, _)) = layout::null_niche(self.tys, ty) {
            // `Some` is its payload, written on its own; `None` is null.
            if v == none {
                let null = self.ptr_ty().const_null();
                self.store(p, null.into())?;
            }
            return Ok(());
        }
        let it = self.int_ty(tag.size as u32 * 8);
        self.store(p, it.const_int(u64::from(v), false).into())
    }

    /// For a `shared` handle at `p`, its body and the ADT type; otherwise
    /// `p` and `ty` unchanged.
    fn through_shared(&mut self, p: PointerValue<'ctx>, ty: Ty) -> R<(PointerValue<'ctx>, Ty)> {
        match self.tys.kind(ty) {
            TyKind::Shared(_) => self.deref(p, ty),
            _ => Ok((p, ty)),
        }
    }
}
