//! Runtime contract emission (design.md § Contracts).
//!
//! Emits the AOT-binary counterpart of the interpreter's contract checks.
//! This first slice covers `requires` preconditions: at function entry —
//! after parameters are bound, before the body runs — each `requires`
//! predicate is compiled and a false result aborts with a
//! `contract violated` fault. The predicate references the function's
//! parameters, which are already compiled into scope at the injection
//! point, so the predicate compiles through the normal expression path (no
//! synthetic-`self` rebinding, unlike the refinement asserts).
//!
//! `ensures` (return-point interception), struct `invariant`s, and
//! `old(...)` capture are follow-on slices — the interpreter path already
//! enforces all of them.

use crate::ast::Expr;

impl<'ctx> super::Codegen<'ctx> {
    /// Compile `pred` to an `i1` and branch: on `true` execution continues;
    /// on `false` the program aborts via `emit_panic(fault_msg)`. The builder
    /// is left positioned in a block where the predicate held. Reuses the
    /// same shape as `emit_refinement_assert`.
    pub(super) fn emit_contract_assert(
        &mut self,
        pred: &Expr,
        fault_msg: &str,
    ) -> Result<(), String> {
        let cond = self.compile_expr(pred)?.into_int_value();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "contract assertion emitted outside a function".to_string())?;
        let fail_bb = self.context.append_basic_block(fn_val, "contract.fail");
        let ok_bb = self.context.append_basic_block(fn_val, "contract.ok");
        self.builder
            .build_conditional_branch(cond, ok_bb, fail_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.emit_panic(fault_msg);
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// Emit the `requires` precondition checks for a function at entry.
    /// Each predicate is evaluated with the parameters in scope; a false
    /// predicate aborts with `contract violated: requires clause`.
    pub(super) fn emit_requires_checks(&mut self, requires: &[Expr]) -> Result<(), String> {
        for req in requires {
            self.emit_contract_assert(req, "contract violated: requires clause")?;
        }
        Ok(())
    }
}
