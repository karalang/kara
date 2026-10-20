//! Core semantics §6.2's run-time borrow flags, as the borrow-flag pass
//! (`src/mir/flags.rs`) placed them: the runtime keeps each flagged field's
//! flag by address (`runtime/src/mir_flags.rs`), and a frame keeps, per
//! loan site and place, the address it took a flag at, so `Release` and the
//! frame's `return` give back what it holds.
//!
//! A coroutine's resume body takes none: its frame outlives one resume,
//! and the slots here do not.

use inkwell::values::PointerValue;
use inkwell::IntPredicate;

use super::{Cx, R};
use crate::mir::coroutine;
use crate::mir::syntax::{BorrowKind, FlagOp, Place, StatementKind};

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

const CONFLICT: &str =
    "already borrowed: this `shared` field is borrowed through another handle (core semantics §6.2)";

/// A flag a frame can hold: the loan site, the flagged place, the kind,
/// and the slot holding the address it was taken at (null when not held).
pub(super) struct FlagSlot<'ctx> {
    loan: u32,
    place: Place,
    mutable: bool,
    slot: PointerValue<'ctx>,
}

impl<'ctx> Cx<'ctx, '_> {
    /// One slot per flag the body can take, null, in the entry block.
    pub(super) fn flag_slots(&mut self) -> R<()> {
        self.flags.clear();
        let body = self.cur()?;
        if !self.flags_on || coroutine::is_resume(&body.instance.name) {
            return Ok(());
        }
        for block in &body.blocks {
            for s in &block.statements {
                let StatementKind::BorrowFlag(FlagOp::Acquire { place, kind, loan }) = &s.kind
                else {
                    continue;
                };
                if self
                    .flags
                    .iter()
                    .any(|f| f.loan == *loan && f.place == *place)
                {
                    continue;
                }
                let slot = self
                    .builder
                    .build_alloca(self.ptr_ty(), "flag")
                    .map_err(e)?;
                self.builder
                    .build_store(slot, self.ptr_ty().const_null())
                    .map_err(e)?;
                self.flags.push(FlagSlot {
                    loan: *loan,
                    place: place.clone(),
                    mutable: *kind == BorrowKind::Mut,
                    slot,
                });
            }
        }
        Ok(())
    }

    pub(super) fn borrow_flag(&mut self, op: &FlagOp) -> R<()> {
        if !self.flags_on || coroutine::is_resume(&self.cur()?.instance.name) {
            return Ok(());
        }
        match op {
            FlagOp::Acquire { place, kind, loan } => {
                let i = self
                    .flags
                    .iter()
                    .position(|f| f.loan == *loan && f.place == *place)
                    .ok_or("a flag acquired with no slot")?;
                // A loan made again while held, as in a loop, gives back
                // what it held first.
                self.give_back(i)?;
                let mutable = *kind == BorrowKind::Mut;
                let addr = self.place_ptr(place)?;
                let r = self.flag_call("karac_mir_flag_acquire", addr, mutable)?;
                self.abort_unless(r, CONFLICT)?;
                self.builder
                    .build_store(self.flags[i].slot, addr)
                    .map_err(e)?;
            }
            FlagOp::Release { loan } => {
                for i in 0..self.flags.len() {
                    if self.flags[i].loan == *loan {
                        self.give_back(i)?;
                    }
                }
            }
            FlagOp::Check { place, kind } => {
                let addr = self.place_ptr(place)?;
                let r = self.flag_call("karac_mir_flag_check", addr, *kind == BorrowKind::Mut)?;
                self.abort_unless(r, CONFLICT)?;
            }
        }
        Ok(())
    }

    /// Gives back every flag the frame still holds, as it returns.
    pub(super) fn give_back_all(&mut self) -> R<()> {
        for i in 0..self.flags.len() {
            self.give_back(i)?;
        }
        Ok(())
    }

    /// `if slot { release(slot); slot = null }`.
    fn give_back(&mut self, i: usize) -> R<()> {
        let (slot, mutable) = (self.flags[i].slot, self.flags[i].mutable);
        let pt = self.ptr_ty();
        let held = self
            .builder
            .build_load(pt, slot, "")
            .map_err(e)?
            .into_pointer_value();
        let is_held = self.builder.build_is_not_null(held, "").map_err(e)?;
        let f = self.cur_fn()?;
        let yes = self.llcx.append_basic_block(f, "flag.held");
        let done = self.llcx.append_basic_block(f, "flag.done");
        self.builder
            .build_conditional_branch(is_held, yes, done)
            .map_err(e)?;
        self.builder.position_at_end(yes);
        let i8t = self.llcx.i8_type();
        let rel = self.extern_fn(
            "karac_mir_flag_release",
            self.llcx
                .void_type()
                .fn_type(&[pt.into(), i8t.into()], false),
        );
        let m = i8t.const_int(u64::from(mutable), false);
        self.builder
            .build_call(rel, &[held.into(), m.into()], "")
            .map_err(e)?;
        self.builder.build_store(slot, pt.const_null()).map_err(e)?;
        self.builder.build_unconditional_branch(done).map_err(e)?;
        self.builder.position_at_end(done);
        Ok(())
    }

    /// `name(addr, mutable) == 0`: the runtime found no conflict.
    fn flag_call(
        &mut self,
        name: &str,
        addr: PointerValue<'ctx>,
        mutable: bool,
    ) -> R<inkwell::values::IntValue<'ctx>> {
        let i8t = self.llcx.i8_type();
        let f = self.extern_fn(
            name,
            i8t.fn_type(&[self.ptr_ty().into(), i8t.into()], false),
        );
        let m = i8t.const_int(u64::from(mutable), false);
        let r = self
            .builder
            .build_call(f, &[addr.into(), m.into()], "")
            .map_err(e)?
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_int_compare(IntPredicate::EQ, r, i8t.const_zero(), "")
            .map_err(e)
    }
}
