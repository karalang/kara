//! Places, operands, rvalues, and every terminator but `Drop`.
//!
//! Thread B's half of M2. Until it lands, each entry point refuses with
//! the construct it does not lower yet, so a program that reaches one
//! fails to compile rather than miscompiling.

use super::{Cx, R};
use crate::mir::{Place, Statement, TerminatorKind};
use inkwell::values::PointerValue;

impl<'ctx> Cx<'ctx, '_> {
    pub(super) fn statement(&mut self, s: &Statement) -> R<()> {
        Err(format!(
            "MIR to LLVM: statement not lowered yet: {:?}",
            s.kind
        ))
    }

    pub(super) fn terminator(&mut self, kind: &TerminatorKind) -> R<()> {
        Err(format!("MIR to LLVM: terminator not lowered yet: {kind:?}"))
    }

    pub(super) fn projected_place_ptr(&mut self, place: &Place) -> R<PointerValue<'ctx>> {
        Err(format!(
            "MIR to LLVM: place projection not lowered yet: {place:?}"
        ))
    }
}
