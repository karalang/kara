//! Runtime predicate emission for refinement types (phase-9 step 5c).
//!
//! Step 4 lowered a refinement to its base *layout*; step 5a made codegen
//! *dispatch* a refined value as its base. This module closes the
//! value-safety gap: the predicate a refinement carries
//! (`type Even = i64 where self % 2 == 0`) is actually *enforced* at the
//! two construction sites.
//!
//! - `x as Refined` (asserting form) → `emit_refinement_assert`: a false
//!   predicate aborts with a `contract violated` fault.
//! - `Refined.try_from(x)` (recoverable form) → `compile_refinement_try_from`:
//!   `Ok(x)` when the predicate holds, `Err(<message>)` when it fails.
//!
//! Both bind the candidate value to a synthetic `self` local and compile
//! the predicate against it, reusing the normal expression-compilation and
//! method-dispatch machinery (so method-form predicates such as
//! `self.len() > 0` work). The predicate's `self` references are rewritten
//! to that local via `subst_self`, avoiding any clobber of a real method
//! receiver at the cast site.

use crate::ast::{CallArg, Expr, ExprKind};
use crate::token::Span;

use inkwell::values::{BasicValueEnum, IntValue};

use super::state::VarSlot;

/// Synthetic local the candidate value is bound to while the predicate is
/// evaluated. The `__karac_`-prefix keeps it clear of any user identifier.
const REFINE_SELF: &str = "__karac_refine_self";

/// Rewrite every `self` reference in a predicate expression to reference
/// `name` instead. The typechecker restricts refinement predicates to
/// `self`-rooted forms over constants and a fixed operator set
/// (`validate_refinement_predicate`), so this targeted walk covers every
/// shape a valid predicate can take; any other form is left untouched (it
/// would have been rejected upstream).
fn subst_self(e: &mut Expr, name: &str) {
    match &mut e.kind {
        ExprKind::SelfValue => {
            e.kind = ExprKind::Identifier(name.to_string());
        }
        ExprKind::Binary { left, right, .. } => {
            subst_self(left, name);
            subst_self(right, name);
        }
        ExprKind::Unary { operand, .. } => subst_self(operand, name),
        ExprKind::FieldAccess { object, .. } => subst_self(object, name),
        ExprKind::MethodCall { object, args, .. } => {
            subst_self(object, name);
            for a in args.iter_mut() {
                subst_self(&mut a.value, name);
            }
        }
        ExprKind::Call { callee, args } => {
            subst_self(callee, name);
            for a in args.iter_mut() {
                subst_self(&mut a.value, name);
            }
        }
        ExprKind::Question(inner) => subst_self(inner, name),
        _ => {}
    }
}

impl<'ctx> super::Codegen<'ctx> {
    /// Bind `value` to the synthetic `self` local, registering the base
    /// type's side-tables so method-form predicates dispatch correctly.
    fn bind_refine_self(&mut self, value: BasicValueEnum<'ctx>, rname: &str) {
        let fn_val = self.current_fn.expect("refinement check inside a function");
        let alloca = self.create_entry_alloca(fn_val, REFINE_SELF, value.get_type());
        self.builder.build_store(alloca, value).unwrap();
        self.variables.insert(
            REFINE_SELF.to_string(),
            VarSlot {
                ptr: alloca,
                ty: value.get_type(),
            },
        );
        // The base `TypeExpr` lives in `refinement_bases` for a plain
        // refinement and in `distinct_bases` for a combined `distinct type T
        // = Base where pred`; consult both so a method-form predicate
        // (`self.len()`) gets the base side-tables in either case.
        if let Some(base_te) = self
            .refinement_bases
            .get(rname)
            .or_else(|| self.distinct_bases.get(rname))
            .cloned()
        {
            self.register_var_from_type_expr(REFINE_SELF, &base_te);
        }
    }

    /// Drop the synthetic `self` binding's side-table entries so a stale
    /// registration can't leak into later code in the same function.
    fn unbind_refine_self(&mut self) {
        self.variables.remove(REFINE_SELF);
        self.var_type_names.remove(REFINE_SELF);
        self.string_vars.remove(REFINE_SELF);
        self.vec_elem_types.remove(REFINE_SELF);
        self.var_elem_type_exprs.remove(REFINE_SELF);
    }

    /// Compile the refinement's predicate to an `i1`, with `self` already
    /// bound (caller must `bind_refine_self` first and `unbind_refine_self`
    /// after the binding is no longer needed).
    fn compile_bound_predicate(&mut self, rname: &str) -> Result<IntValue<'ctx>, String> {
        let mut pred = self
            .refinement_predicates
            .get(rname)
            .cloned()
            .ok_or_else(|| format!("no predicate registered for refinement `{rname}`"))?;
        subst_self(&mut pred, REFINE_SELF);
        Ok(self.compile_expr(&pred)?.into_int_value())
    }

    /// `x as Refined`: enforce the predicate at runtime, aborting with a
    /// `contract violated` fault when it fails. A no-op when `rname` is not
    /// a refinement. On success the builder is left in a block where the
    /// predicate held; the caller's (layout-identical) value stays valid.
    pub(super) fn emit_refinement_assert(
        &mut self,
        rname: &str,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        if !self.refinement_predicates.contains_key(rname) {
            return Ok(());
        }
        self.bind_refine_self(value, rname);
        let cond = self.compile_bound_predicate(rname)?;
        self.unbind_refine_self();

        let fn_val = self
            .current_fn
            .ok_or_else(|| "refinement check emitted outside a function".to_string())?;
        let fail_bb = self.context.append_basic_block(fn_val, "refine.fail");
        let ok_bb = self.context.append_basic_block(fn_val, "refine.ok");
        self.builder
            .build_conditional_branch(cond, ok_bb, fail_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.emit_panic(&format!(
            "contract violated: value does not satisfy refinement `{rname}`"
        ));
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    /// `Refined.try_from(x)`: lower to a runtime predicate check producing a
    /// `Result[Refined, String]` — `Ok(x)` when the predicate holds,
    /// `Err(<message>)` otherwise. Returns `Ok(None)` when `rname` is not a
    /// refinement so the caller falls through to normal dispatch.
    pub(super) fn compile_refinement_try_from(
        &mut self,
        rname: &str,
        arg: &Expr,
        span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        if !self.refinement_predicates.contains_key(rname) {
            return Ok(None);
        }
        let value = self.compile_expr(arg)?;
        self.bind_refine_self(value, rname);
        let cond = self.compile_bound_predicate(rname)?;

        let fn_val = self
            .current_fn
            .ok_or_else(|| "refinement try_from emitted outside a function".to_string())?;
        let ok_bb = self.context.append_basic_block(fn_val, "tryfrom.ok");
        let err_bb = self.context.append_basic_block(fn_val, "tryfrom.err");
        let cont_bb = self.context.append_basic_block(fn_val, "tryfrom.cont");
        self.builder
            .build_conditional_branch(cond, ok_bb, err_bb)
            .unwrap();

        // Ok(value) — reference the stored value via the synthetic local so
        // `arg` is not re-evaluated (it may carry side effects).
        self.builder.position_at_end(ok_bb);
        let ok_arg = CallArg {
            label: None,
            mut_marker: false,
            value: Expr {
                kind: ExprKind::Identifier(REFINE_SELF.to_string()),
                span: span.clone(),
            },
            span: span.clone(),
        };
        let ok_val = self
            .try_compile_enum_variant("Ok", std::slice::from_ref(&ok_arg))?
            .ok_or_else(|| "failed to build Ok(...) for refinement try_from".to_string())?;
        // `try_from` CONSUMES its argument: on the Ok path the heap buffer
        // (`Vec`/`String`) now lives in the `Ok` payload, so the source
        // binding (`enriched` / `v`) must NOT free it again at scope exit —
        // else a double-free against the `Ok` payload's drop (the Weave
        // dogfood's `NonEmpty.try_from(enriched)`). The suppression emits a
        // `store cap = 0` at the current insert point, so placing it in the OK
        // block makes it branch-local: on the Err path the value is discarded
        // and the source's own cleanup (cap intact) correctly frees it.
        self.suppress_source_vec_cleanup_for_arg(arg);
        let ok_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Err(message)
        self.builder.position_at_end(err_bb);
        let err_arg = CallArg {
            label: None,
            mut_marker: false,
            value: Expr {
                kind: ExprKind::StringLit(format!("value does not satisfy refinement `{rname}`")),
                span: span.clone(),
            },
            span: span.clone(),
        };
        let err_val = self
            .try_compile_enum_variant("Err", std::slice::from_ref(&err_arg))?
            .ok_or_else(|| "failed to build Err(...) for refinement try_from".to_string())?;
        let err_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // Merge the two arms.
        self.builder.position_at_end(cont_bb);
        self.unbind_refine_self();
        let phi = self
            .builder
            .build_phi(ok_val.get_type(), "tryfrom.result")
            .unwrap();
        phi.add_incoming(&[(&ok_val, ok_end), (&err_val, err_end)]);
        Ok(Some(phi.as_basic_value()))
    }
}
