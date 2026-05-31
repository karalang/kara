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

use crate::ast::{Expr, ExprKind};
use crate::resolver::SpanKey;
use inkwell::values::BasicValueEnum;

use super::state::VarSlot;

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

    /// Capture `old(arg)` pre-state for the current function's `ensures`
    /// clauses at entry: compile each `old(arg)` arg and stash the SSA value
    /// keyed by the arg's span (entry dominates every return point, so the
    /// value is valid wherever the postcondition reads it). Call once, after
    /// params are bound, before the body runs.
    pub(super) fn capture_contract_old_snapshots(
        &mut self,
        ensures: &[crate::ast::EnsuresClause],
    ) -> Result<(), String> {
        // Collect arg expressions first so the immutable walk doesn't overlap
        // the mutable compile.
        let mut args: Vec<Expr> = Vec::new();
        for ens in ensures {
            collect_old_args(&ens.body, &mut args);
        }
        for arg in &args {
            let val = self.compile_expr(arg)?;
            self.contract_old_snapshots
                .insert(SpanKey::from_span(&arg.span), val);
        }
        Ok(())
    }

    /// Look up a captured `old(arg)` snapshot by the arg's span. Returns
    /// `None` when no snapshot is active (the caller falls back to compiling
    /// the arg directly — defensive; the typechecker restricts `old(...)` to
    /// `ensures`).
    pub(super) fn contract_old_lookup(&self, arg: &Expr) -> Option<BasicValueEnum<'ctx>> {
        self.contract_old_snapshots
            .get(&SpanKey::from_span(&arg.span))
            .copied()
    }

    /// Emit the `ensures` postcondition checks for the function currently
    /// being compiled, with `result` bound to `result_value`. Called inline
    /// before each `ret`. A false predicate aborts with
    /// `contract violated: ensures clause`.
    pub(super) fn emit_ensures_checks(
        &mut self,
        result_value: Option<BasicValueEnum<'ctx>>,
    ) -> Result<(), String> {
        let ensures = self.current_contract_ensures.clone();
        if ensures.is_empty() {
            return Ok(());
        }
        let fn_val = self
            .current_fn
            .ok_or_else(|| "ensures emitted outside a function".to_string())?;
        for ens in &ensures {
            // Bind `result` to the return value for the duration of this
            // predicate, saving/restoring any shadowed binding.
            let saved = match (&ens.param, result_value) {
                (Some(param), Some(rv)) => {
                    let alloca = self.create_entry_alloca(fn_val, param, rv.get_type());
                    self.builder.build_store(alloca, rv).unwrap();
                    let prev = self.variables.insert(
                        param.clone(),
                        VarSlot {
                            ptr: alloca,
                            ty: rv.get_type(),
                        },
                    );
                    Some((param.clone(), prev))
                }
                _ => None,
            };
            self.emit_contract_assert(&ens.body, "contract violated: ensures clause")?;
            if let Some((param, prev)) = saved {
                match prev {
                    Some(p) => {
                        self.variables.insert(param, p);
                    }
                    None => {
                        self.variables.remove(&param);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Collect the arg expressions of every `old(arg)` occurrence in a contract
/// expression (mirrors the interpreter / typechecker walkers).
fn collect_old_args(expr: &Expr, out: &mut Vec<Expr>) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Identifier(n) = &callee.kind {
                if n == "old" && args.len() == 1 {
                    out.push(args[0].value.clone());
                    return;
                }
            }
            collect_old_args(callee, out);
            for a in args {
                collect_old_args(&a.value, out);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_old_args(left, out);
            collect_old_args(right, out);
        }
        ExprKind::Unary { operand, .. } => collect_old_args(operand, out),
        ExprKind::FieldAccess { object, .. } => collect_old_args(object, out),
        ExprKind::MethodCall { object, args, .. } => {
            collect_old_args(object, out);
            for a in args {
                collect_old_args(&a.value, out);
            }
        }
        ExprKind::Index { object, index } => {
            collect_old_args(object, out);
            collect_old_args(index, out);
        }
        _ => {}
    }
}
