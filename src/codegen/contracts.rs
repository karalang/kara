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

/// The per-function half of [`super::contract_state::ContractState`], lifted out
/// so a nested (monomorphized) body compile can swap it. See
/// [`super::Codegen::take_contract_frame`].
pub(crate) struct SavedContractFrame<'ctx> {
    ensures: Vec<crate::ast::EnsuresClause>,
    result_type: Option<crate::ast::TypeExpr>,
    old_snapshots: rustc_hash::FxHashMap<SpanKey, BasicValueEnum<'ctx>>,
    invariants: Vec<Expr>,
    ctor_self_type: Option<String>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Install the contract frame for `func`: emit its `requires` asserts,
    /// capture `old(...)` pre-state, and stash the `ensures` clauses, the
    /// result type and the receiver-type invariants for the return sites.
    ///
    /// Called at the same point by BOTH body-emitting paths — `compile_function`
    /// and `compile_mono_function` — after the parameters are bound and before
    /// the body runs. It is one function rather than two copies because the
    /// second copy is precisely what was missing: the monomorphized path had no
    /// contract handling at all, so `requires` and `ensures` on a GENERIC
    /// function were silently dropped by both compiled backends while the
    /// interpreter enforced them (B-2026-08-21-21). design.md § Contracts
    /// presents a generic `binary_search[T: Ord]` as the feature's worked
    /// example, so the one shape the documentation leads with was the one shape
    /// neither backend checked.
    pub(super) fn install_contract_frame(
        &mut self,
        func: &crate::ast::Function,
    ) -> Result<(), String> {
        // Contract emission setup (design.md § Contracts). Gated on
        // `!strip_contracts` so a release build (design: "stripped in
        // release") emits none of it — zero runtime cost, including the
        // `old(...)` pre-state clone. Suppressing the three setup statements
        // here is sufficient: `emit_ensures_checks` / `emit_invariant_checks`
        // both no-op on their now-empty state vectors at the return sites, no
        // `requires` assert is built, and `old(...)` (which lives only inside
        // `ensures` bodies) is never reached because those bodies aren't
        // compiled. The gate is a single decision point for the whole feature.
        if !self.contract_state.strip_contracts {
            // `requires` preconditions: emit the entry-time predicate checks
            // now that parameters are bound and before the body runs. A false
            // predicate aborts with `contract violated`.
            self.emit_requires_checks(&func.requires)?;

            // `ensures` setup: capture `old(...)` pre-state now (entry
            // dominates every return point) and stash the clauses so
            // `emit_ensures_checks` can fire them inline before each `ret`
            // (the tail return below + every explicit `return`).
            self.capture_contract_old_snapshots(&func.ensures)?;
            self.contract_state.current_contract_ensures = func.ensures.clone();
            // Return type for the `result` binding in `emit_ensures_checks`
            // (so `result.field` resolves its struct field index).
            self.contract_state.current_contract_result_type = func.return_type.clone();

            // Struct/impl `invariant` setup (rule 3): resolve the receiver
            // type's invariants for this method and stash them so
            // `emit_invariant_checks` can fire them inline before each `ret`
            // (same exit points as `ensures`), with `self` bound. The synthetic
            // method function carries `Type.method` as its name and the
            // method's `is_pub` flag — both consumed by `method_invariants_for`.
            // Free functions and invariant-free structs yield an empty list.
            self.contract_state.current_method_invariants =
                self.method_invariants_for(&func.name, func.is_pub);
            self.contract_state.constructor_invariant_self_type = None;
            // `method_invariants_for` keys purely off the `Type.method` name, so
            // it also matches associated functions (which `make_impl_method_function`
            // names `Type.method` but gives no `self` parameter). For those:
            //   - A *constructor* — returns `Self`/the type — checks the invariants
            //     against its RETURN value (the construction boundary). Record the
            //     type so `emit_invariant_checks` binds the return value as `self`.
            //   - Any other associated function (e.g. `Type.parse() -> i64`) is NOT
            //     a constructor: clear the invariants so we don't try to evaluate
            //     `self.field` against a non-receiver (which previously aborted
            //     codegen with `Undefined variable 'self'`).
            if !self.contract_state.current_method_invariants.is_empty() {
                let has_self_param = func.params.first().is_some_and(|p| {
                    matches!(&p.pattern.kind, crate::ast::PatternKind::Binding(n) if n == "self")
                });
                if !has_self_param {
                    match func.name.split_once('.') {
                        // Constructor (returns `Self`/the type): bind the return
                        // value as `self` and enforce the invariants against it.
                        // Works for owned and shared (RC) structs alike — for a
                        // shared struct the return value is the heap pointer, and
                        // `self.field` resolves through the shared heap-GEP path
                        // because `shared_type_for_expr` accepts the constructor's
                        // `SelfValue` binding (gated to non-`ref`-param `self`).
                        Some((type_name, _))
                            if super::functions::returns_self_or_type(
                                func.return_type.as_ref(),
                                type_name,
                            ) =>
                        {
                            self.contract_state.constructor_invariant_self_type =
                                Some(type_name.to_string());
                        }
                        // Any other associated function (e.g. `Type.parse() -> i64`)
                        // is NOT a constructor: clear the name-resolved invariants
                        // so we don't evaluate `self.field` against a non-receiver
                        // (which would abort codegen with `Undefined variable 'self'`).
                        _ => self.contract_state.current_method_invariants.clear(),
                    }
                }
            }
        }
        Ok(())
    }

    /// Snapshot of the per-function contract frame, taken so a nested body
    /// compile can install its own without destroying the enclosing one.
    ///
    /// A monomorphized body is emitted INLINE while its caller is mid-compile
    /// (`compile_generic_call` reaches `compile_mono_function` from inside the
    /// caller's body), so the caller's `ensures` clauses and `old(...)`
    /// snapshots are live across that call. The mono path saves them the same
    /// way it already saves `variables`, `ref_params` and the layout carriers.
    pub(super) fn take_contract_frame(&mut self) -> SavedContractFrame<'ctx> {
        SavedContractFrame {
            ensures: std::mem::take(&mut self.contract_state.current_contract_ensures),
            result_type: self.contract_state.current_contract_result_type.take(),
            old_snapshots: std::mem::take(&mut self.contract_state.contract_old_snapshots),
            invariants: std::mem::take(&mut self.contract_state.current_method_invariants),
            ctor_self_type: self.contract_state.constructor_invariant_self_type.take(),
        }
    }

    /// Put back what [`Self::take_contract_frame`] removed.
    pub(super) fn restore_contract_frame(&mut self, saved: SavedContractFrame<'ctx>) {
        self.contract_state.current_contract_ensures = saved.ensures;
        self.contract_state.current_contract_result_type = saved.result_type;
        self.contract_state.contract_old_snapshots = saved.old_snapshots;
        self.contract_state.current_method_invariants = saved.invariants;
        self.contract_state.constructor_invariant_self_type = saved.ctor_self_type;
    }

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
            .build_call(self.runtime_fns.karac_runtime_enter_predicate_fn, &[], "")
            .unwrap();
        let cond = self.compile_expr(pred).map(|v| v.into_int_value());
        // Exit on the common post-evaluation path, before the branch below.
        self.builder
            .build_call(self.runtime_fns.karac_runtime_exit_predicate_fn, &[], "")
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
            self.contract_state
                .contract_old_snapshots
                .insert(SpanKey::from_span(&arg.span), val);
        }
        Ok(())
    }

    /// Look up a captured `old(arg)` snapshot by the arg's span. Returns
    /// `None` when no snapshot is active (the caller falls back to compiling
    /// the arg directly — defensive; the typechecker restricts `old(...)` to
    /// `ensures`).
    pub(super) fn contract_old_lookup(&self, arg: &Expr) -> Option<BasicValueEnum<'ctx>> {
        self.contract_state
            .contract_old_snapshots
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
        let ensures = self.contract_state.current_contract_ensures.clone();
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
                    // Record the binding's static type NAME so a `result.field`
                    // access inside the predicate resolves the struct field
                    // index. Without this the field access can't find the
                    // struct name and reads the wrong slot. Only the type name
                    // is recorded (via `record_var_type_name`, which normalizes
                    // a refinement return to its base) — NOT the full
                    // collection/heap-tracking registration, which would mark
                    // the borrowed `result` for a drop and double-free its heap
                    // fields. The saved/removed entry is restored below
                    // alongside the `variables` slot.
                    let prev_type_name = self.var_types.var_type_names.remove(param);
                    if let Some(crate::ast::TypeKind::Path(p)) = self
                        .contract_state
                        .current_contract_result_type
                        .as_ref()
                        .map(|te| &te.kind)
                    {
                        if let Some(seg) = p.segments.first().cloned() {
                            self.record_var_type_name(param.clone(), seg);
                        }
                    }
                    Some((param.clone(), prev, prev_type_name))
                }
                _ => None,
            };
            self.emit_contract_assert(&ens.body, "contract violated: ensures clause")?;
            if let Some((param, prev, prev_type_name)) = saved {
                match prev {
                    Some(p) => {
                        self.variables.insert(param.clone(), p);
                    }
                    None => {
                        self.variables.remove(&param);
                    }
                }
                match prev_type_name {
                    Some(tn) => {
                        self.var_types.var_type_names.insert(param, tn);
                    }
                    None => {
                        self.var_types.var_type_names.remove(&param);
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
        let invariants = self.contract_state.current_method_invariants.clone();
        if invariants.is_empty() {
            return Ok(());
        }
        // Constructor: bind the return value as `self` so `self.field` in each
        // invariant resolves to the freshly-constructed instance. Saved/restored
        // around the checks (defensive — a constructor has no real `self`
        // binding to shadow, but this keeps the table clean).
        let bound_self = match (
            &self.contract_state.constructor_invariant_self_type,
            result_value,
        ) {
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
                let prev_ty = self
                    .var_types
                    .var_type_names
                    .insert("self".to_string(), type_name);
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
                    self.var_types.var_type_names.insert("self".to_string(), t);
                }
                None => {
                    self.var_types.var_type_names.remove("self");
                }
            }
        }
        Ok(())
    }
}

/// Whether `program` declares any contract whose predicate evaluation
/// `emit_contract_assert` would bracket with the runtime
/// enter/exit-predicate calls: `requires` / `ensures` on free functions,
/// impl methods, or trait methods (trait-method contracts are scanned
/// defensively — propagation to impls lands them on `Function` nodes, but
/// over-approximation here only keeps the always-correct runtime read), and
/// `invariant` / `impl invariant` on structs. Refinement predicates
/// (`type T = U where ...`) are deliberately NOT counted —
/// `emit_refinement_assert` never brackets enter/exit, so they cannot move
/// the runtime depth counter. Consumed by `compile_program` to decide
/// `runtime_panic_prefix_needed` (see that field's doc for the costs a
/// `false` answer avoids).
pub(super) fn program_declares_contracts(program: &crate::ast::Program) -> bool {
    program.items.iter().any(|item| match item {
        Item::Function(f) => !f.requires.is_empty() || !f.ensures.is_empty(),
        Item::StructDef(s) => !s.invariants.is_empty() || !s.impl_invariants.is_empty(),
        Item::ImplBlock(ib) => ib.items.iter().any(|ii| match ii {
            crate::ast::ImplItem::Method(m) => !m.requires.is_empty() || !m.ensures.is_empty(),
            crate::ast::ImplItem::AssocType(_) => false,
        }),
        Item::TraitDef(t) => t.items.iter().any(|ti| match ti {
            crate::ast::TraitItem::Method(m) => !m.requires.is_empty() || !m.ensures.is_empty(),
            crate::ast::TraitItem::AssocType(_) => false,
        }),
        _ => false,
    })
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
