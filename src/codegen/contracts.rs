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
//! `ensures` (return-point interception), `old(...)` capture, and struct/impl
//! `invariant`s are all emitted now — the AOT binary enforces the same
//! contract surface as the interpreter path.

use crate::ast::{Expr, ExprKind, Item};
use crate::resolver::SpanKey;
use inkwell::values::BasicValueEnum;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    /// Compile `pred` to an `i1` and branch: on `true` execution continues;
    /// on `false` the program aborts via `emit_panic(fault_msg)`. The builder
    /// is left positioned in a block where the predicate held. Reuses the
    /// same shape as `emit_refinement_assert`.
    ///
    /// The predicate's *runtime* evaluation is bracketed by
    /// `karac_runtime_enter_predicate()` / `karac_runtime_exit_predicate()`
    /// (design.md § Contracts rule 2), which bump a thread-local depth counter
    /// in the runtime. Any panic that fires while the depth is non-zero — an
    /// inline bounds check in `v[i]`, a divide-by-zero guard, an `unwrap`
    /// None-check, OR a panic inside a function the predicate transitively
    /// calls — aborts as the distinct `contract predicate panicked: <msg>`
    /// fault rather than `contract violated`. The exit call is emitted on the
    /// common path right after the predicate value is produced (before the
    /// conditional branch), so it runs whether the predicate holds or fails;
    /// the explicit false-branch panic below therefore reports `contract
    /// violated` (depth back to 0). A panic *during* evaluation aborts the
    /// process before reaching the exit call, which is correct — the prefix is
    /// already set. The counter (not a bool) keeps a predicate that calls a
    /// contracted function nesting correctly.
    pub(super) fn emit_contract_assert(
        &mut self,
        pred: &Expr,
        fault_msg: &str,
    ) -> Result<(), String> {
        self.builder
            .build_call(self.karac_runtime_enter_predicate_fn, &[], "")
            .unwrap();
        let cond = self.compile_expr(pred).map(|v| v.into_int_value());
        // Exit on the common post-evaluation path, before the branch below.
        self.builder
            .build_call(self.karac_runtime_exit_predicate_fn, &[], "")
            .unwrap();
        let cond = cond?;
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

    /// Compute the struct/impl `invariant` predicates that must hold at each
    /// exit of the impl method whose synthetic name is `fn_name` (the
    /// `Type.method` shape minted by `make_impl_method_function`). `impl
    /// invariant`s fire at every method exit; plain `invariant`s only when the
    /// method is `pub` (`is_pub`). Free functions (no `.` in `fn_name`) and
    /// structs without invariants yield an empty list. Mirrors the
    /// interpreter's `method_invariants_to_check`, but the receiver type and
    /// pub-ness are already recoverable from the synthetic function — `self`'s
    /// pub flag is preserved through the method clone, and the type name is the
    /// `Type` segment of `Type.method`.
    pub(super) fn method_invariants_for(&self, fn_name: &str, is_pub: bool) -> Vec<Expr> {
        let Some((type_name, _method)) = fn_name.rsplit_once('.') else {
            return Vec::new();
        };
        let Some(program) = self.program_snapshot.clone() else {
            return Vec::new();
        };
        let Some((invariants, impl_invariants)) =
            program.items.iter().find_map(|item| match item {
                Item::StructDef(s) if s.name == type_name => {
                    Some((s.invariants.clone(), s.impl_invariants.clone()))
                }
                _ => None,
            })
        else {
            return Vec::new();
        };
        // `impl invariant` — every method exit; plain `invariant` — pub only.
        let mut result = impl_invariants;
        if is_pub {
            result.extend(invariants);
        }
        result
    }

    /// Emit the struct/impl `invariant` checks for the method currently being
    /// compiled. Called inline before each `ret` (same exit points as
    /// `ensures`). For a method, `self` is already bound as the first parameter
    /// so each predicate's `self.field` access resolves through the normal
    /// expression path. For a *constructor* (`constructor_invariant_self_type`
    /// is set — a `pub` associated function returning `Self`/the type, which has
    /// no receiver), the `result_value` is bound as `self` for the duration of
    /// the checks, mirroring how `emit_ensures_checks` binds `result`. A false
    /// predicate aborts with `contract violated: invariant`.
    pub(super) fn emit_invariant_checks(
        &mut self,
        result_value: Option<BasicValueEnum<'ctx>>,
    ) -> Result<(), String> {
        let invariants = self.current_method_invariants.clone();
        if invariants.is_empty() {
            return Ok(());
        }
        // Constructor: bind the return value as `self` so `self.field` in each
        // invariant resolves to the freshly-constructed instance. Saved/restored
        // around the checks (defensive — a constructor has no real `self`
        // binding to shadow, but this keeps the table clean).
        let bound_self = match (&self.constructor_invariant_self_type, result_value) {
            (Some(type_name), Some(rv)) => {
                let type_name = type_name.clone();
                let fn_val = self
                    .current_fn
                    .ok_or_else(|| "invariant emitted outside a function".to_string())?;
                let alloca = self.create_entry_alloca(fn_val, "self", rv.get_type());
                self.builder.build_store(alloca, rv).unwrap();
                let prev_var = self.variables.insert(
                    "self".to_string(),
                    VarSlot {
                        ptr: alloca,
                        ty: rv.get_type(),
                    },
                );
                let prev_ty = self.var_type_names.insert("self".to_string(), type_name);
                Some((prev_var, prev_ty))
            }
            _ => None,
        };
        for inv in &invariants {
            self.emit_contract_assert(inv, "contract violated: invariant")?;
        }
        if let Some((prev_var, prev_ty)) = bound_self {
            match prev_var {
                Some(p) => {
                    self.variables.insert("self".to_string(), p);
                }
                None => {
                    self.variables.remove("self");
                }
            }
            match prev_ty {
                Some(t) => {
                    self.var_type_names.insert("self".to_string(), t);
                }
                None => {
                    self.var_type_names.remove("self");
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
