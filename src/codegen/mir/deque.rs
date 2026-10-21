//! `VecDeque[T]`: the ring `{ptr, head, len, cap}` (`src/mir/layout.rs`),
//! pushed and popped at both ends through the runtime
//! (`karac_mir_deque_*`), whose element `i` is slot `(head + i) % cap`.

use inkwell::values::{IntValue, PointerValue};
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::ty::IntrinsicTy;
use crate::mir::{Operand, Place, Ty, TyKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

impl<'ctx> Cx<'ctx, '_> {
    /// The methods of a `VecDeque`, or `None` for one not lowered here.
    pub(super) fn deque_native(
        &mut self,
        method: &str,
        args: &[Operand],
        dest: &Place,
    ) -> R<Option<()>> {
        let dty = self.place_type(dest)?;
        if matches!(method, "new" | "with_capacity") {
            if let [n] = args {
                let n = self.int_arg(n)?;
                let ok = self.non_negative(n)?;
                self.abort_unless(ok, "explicit panic")?;
            }
            let d = self.place_ptr(dest)?;
            self.builder
                .build_memset(d, 1, self.llcx.i8_type().const_zero(), self.i64(32))
                .map_err(e)?;
            return Ok(Some(()));
        }
        let Some(recv) = args.first() else {
            return Ok(None);
        };
        let mut t = self.operand_type(recv)?;
        let by_ref = matches!(self.tys.kind(t), TyKind::Ref(_) | TyKind::MutRef(_));
        while let TyKind::Ref(i) | TyKind::MutRef(i) = self.tys.kind(t) {
            t = i;
        }
        let TyKind::Intrinsic(IntrinsicTy::VecDeque(el)) = self.tys.kind(t) else {
            return Ok(None);
        };
        let q = if by_ref {
            self.ref_arg(recv)?
        } else {
            self.operand_ptr(recv)?
        };
        let size = self.layout(el)?.size;
        let len = self.load_i64(self.offset(q, 16)?)?;
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
            ("is_empty", []) => {
                let z = self.is_zero(len)?;
                self.store_bool(dest, z)?;
            }
            ("push_back" | "push_front", [x]) => {
                let x = self.operand_ptr(x)?;
                let front = self
                    .llcx
                    .i8_type()
                    .const_int(u64::from(method == "push_front"), false);
                self.rt(
                    "deque_push",
                    &[q.into(), x.into(), self.i64(size).into(), front.into()],
                )?;
            }
            ("pop_back" | "pop_front", []) => {
                let d = self.place_ptr(dest)?;
                let (some, none) = self.option_variants(dty)?;
                self.store_tag(d, dty, none)?;
                let out = self.offset(d, self.field_offset(dty, Some(some), 0)?)?;
                let front = self
                    .llcx
                    .i8_type()
                    .const_int(u64::from(method == "pop_front"), false);
                let got = self.rt_int(
                    "deque_pop",
                    &[q.into(), out.into(), self.i64(size).into(), front.into()],
                    8,
                )?;
                let got = self
                    .builder
                    .build_int_compare(IntPredicate::NE, got, got.get_type().const_zero(), "")
                    .map_err(e)?;
                self.if_then(got, |cx| cx.store_tag(d, dty, some))?;
            }
            ("front" | "front_mut", []) => self.some_slot(dest, q, len, self.i64(0), size, el)?,
            ("back" | "back_mut", []) => {
                let i = self
                    .builder
                    .build_int_sub(len, self.i64(1), "")
                    .map_err(e)?;
                self.some_slot(dest, q, len, i, size, el)?;
            }
            ("get" | "get_mut", [i]) => {
                let i = self.int_arg(i)?;
                self.some_slot(dest, q, len, i, size, el)?;
            }
            ("entry_at" | "index" | "index_mut", [i]) => {
                let i = self.int_arg(i)?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, i, len, "")
                    .map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                let at = self.slot(q, i, size)?;
                let d = self.place_ptr(dest)?;
                self.store_ref(d, at, el)?;
            }
            ("clear", []) => {
                if self.tys.needs_drop(el) {
                    self.drop_ring(q, el)?;
                }
                self.store(self.offset(q, 8)?, self.i64(0).into())?;
                self.store(self.offset(q, 16)?, self.i64(0).into())?;
            }
            // Element `i` becomes `x`; `i` may be the length. An allocation
            // here does not fail, so `try_insert` is always `Ok`.
            ("insert" | "try_insert", [i, x]) => {
                let i = self.int_arg(i)?;
                let x = self.operand_ptr(x)?;
                let ok = self.rt_int(
                    "deque_insert",
                    &[q.into(), i.into(), x.into(), self.i64(size).into()],
                    8,
                )?;
                let ok = self
                    .builder
                    .build_int_compare(IntPredicate::NE, ok, ok.get_type().const_zero(), "")
                    .map_err(e)?;
                self.abort_unless(ok, "index out of bounds")?;
                if method == "try_insert" {
                    let tag = self.variant_of(dty, "Ok")?;
                    let d = self.place_ptr(dest)?;
                    self.store_tag(d, dty, tag)?;
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(()))
    }

    fn slot(&self, q: PointerValue<'ctx>, i: IntValue<'ctx>, size: u64) -> R<PointerValue<'ctx>> {
        self.mrt_ptr("deque_slot", &[q.into(), i.into(), self.i64(size).into()])
    }

    /// Writes `Some(ref elem i)` or `None` at `dest` (an `Option[ref T]`).
    fn some_slot(
        &mut self,
        dest: &Place,
        q: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        i: IntValue<'ctx>,
        size: u64,
        el: Ty,
    ) -> R<()> {
        let dty = self.place_type(dest)?;
        let ok = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "")
            .map_err(e)?;
        let d = self.place_ptr(dest)?;
        let (some, none) = self.option_variants(dty)?;
        self.store_tag(d, dty, none)?;
        self.if_then(ok, |cx| {
            let at = cx.slot(q, i, size)?;
            cx.store_tag(d, dty, some)?;
            cx.store_found(d, dty, some, at, el)
        })
    }
}
