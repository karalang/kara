//! Compile-time elision procedure for refinement narrowing (design.md
//! § Refinement Types > "Compile-time elision procedure (v1)";
//! phase-9-verification line 37).
//!
//! When a value flows into a refined target slot — a `let` binding with a
//! refined annotation, a function-call argument whose parameter is refined,
//! or a function body returned into a refined return type — the narrowing
//! base→refined (or refined→refined) is governed by exactly **two elision
//! rules** plus an explicit-coercion rejection. There is no SMT solver, no
//! interval arithmetic, no flow-sensitive narrowing; anything the two rules
//! do not admit must go through `Refined.try_from(x)?` or `x as Refined`.
//!
//! The two rules and the rejection, in evaluation order:
//!
//!  1. **Const-evaluable narrowing.** If the initializer reduces to a
//!     concrete value via the existing const-evaluator, evaluate the
//!     refinement predicate against that value at compile time. Success →
//!     admit with no runtime check. Failure → a deterministic, catchable
//!     **build-time** error (`E_REFINEMENT_PREDICATE_VIOLATION`), not a
//!     runtime fault.
//!  2. **Type-identity narrowing.** If the static type of the initializer
//!     is *exactly* the target refinement (same name), admit with no check.
//!     (`check_assignable` already admits this via the `a == b` fast path,
//!     so the arm here is defensive — it keeps the procedure total.)
//!  3. **Reject implicit runtime-value narrowing** (rules 4 + 5 of the
//!     checklist): a runtime base value, or a *different* refinement over
//!     the same base, cannot narrow implicitly — even when two refinements'
//!     predicates are textually identical (v1 has no implication-based
//!     elision). Emit `E_REFINEMENT_IMPLICIT_NARROWING` suggesting the
//!     explicit form, and note that flow-sensitive narrowing is unsupported.
//!
//! The whole procedure lives in the typechecker: a rule-1 admit needs no
//! runtime check (there is no cast node to enforce, and the value was
//! verified at build time), a rule-2 admit is a no-op, and every rejection
//! is a compile error that stops the program before it runs. The interpreter
//! and codegen paths are unchanged.
//!
//! **Generic-code guard (checklist step 7).** Inside `fn f[T](v: T)` the
//! expected type is `Type::TypeParam(T)`, never a `Type::Refinement`, so the
//! single `Type::Refinement` gate at the top of [`try_refinement_narrowing`]
//! structurally skips opaque `T`. Elision applies only at the caller's
//! call/return boundaries against the concrete instantiated type, which is
//! exactly where `check_expr` re-runs against the substituted (refined) slot.

use super::types::{strip_refinement, type_display, types_compatible, Type};
use super::{TypeChecker, TypeErrorKind};
use crate::ast::{Expr, ExprKind};
use crate::prelude::ConstValue;

impl TypeChecker<'_> {
    /// Apply the compile-time elision procedure for a value `expr` (static
    /// type `actual`) flowing into a refined target slot `expected`.
    ///
    /// Returns `Some(ty)` when the slot is a refinement and this is a
    /// narrowing the procedure resolved — either an admit (`Some(expected)`)
    /// or a rejection (`Some(Type::Error)`, after emitting the diagnostic).
    /// Returns `None` to fall through to the generic `check_assignable`
    /// when the slot is not a refinement, or `actual` is a genuinely wrong
    /// base type (so the caller surfaces the ordinary "expected X, found Y").
    ///
    /// Callers gate this on `!is_subtype_with_projections(expected, actual)`
    /// so the exact-match and refined→base widening directions never reach
    /// here — only base→refined / refined→refined narrowings do.
    pub(super) fn try_refinement_narrowing(
        &mut self,
        expr: &Expr,
        expected: &Type,
        actual: &Type,
    ) -> Option<Type> {
        let Type::Refinement { name, base } = expected else {
            return None;
        };

        // ── Source is itself a refinement ───────────────────────────
        if let Type::Refinement {
            name: actual_name, ..
        } = actual
        {
            // Rule 2 — type-identity. (Defensive: an exact match already
            // passed `check_assignable`'s fast path before we were called.)
            if actual_name == name {
                return Some(expected.clone());
            }
            // Rule 5 — cross-refinement. Two distinct refinements over the
            // same base have no implicit relationship, even with identical
            // predicates. Only reject when the bases actually match; an
            // unrelated refinement falls through to the generic mismatch.
            if types_compatible(base, strip_refinement(actual)) {
                self.emit_cross_refinement_rejection(name, actual_name, base, expr);
                return Some(Type::Error);
            }
            return None;
        }

        // The source must be base-compatible for any narrowing to be in
        // play. Otherwise this is a genuine type error — let the generic
        // "expected X, found Y" mismatch handle it (e.g. `i32` into an
        // `i64`-based refinement, which is a base mismatch, not a narrowing).
        if !types_compatible(base, actual) {
            return None;
        }

        // Rule 1 — const-evaluable narrowing. Reduce the initializer
        // against the base type; a successful reduction means the value
        // fits the base and we can check the predicate at compile time.
        if let Ok(value) = self.eval_const_expr(expr, base) {
            match self.eval_refinement_predicate_const(name, expr) {
                Some(true) => {
                    // Admitted at build time — no runtime check is emitted.
                    self.record_expr_type(&expr.span, expected);
                    return Some(expected.clone());
                }
                Some(false) => {
                    self.emit_refinement_predicate_violation(name, &value, expr);
                    return Some(Type::Error);
                }
                // Predicate not const-evaluable against this value (e.g. a
                // `self.len()` predicate paired with a numeric const — a
                // malformed combination the grammar can't pre-empt). Fall
                // through to the explicit-form rejection.
                None => {}
            }
        }

        // Rule 4 — reject implicit runtime-value narrowing.
        self.emit_implicit_narrowing_rejection(name, actual, expr);
        Some(Type::Error)
    }

    /// Compile-time predicate check for a combined distinct-type constructor
    /// `T(value)` where `T = distinct type … = Base where pred` (design.md
    /// § Distinct Types — "Construction semantics", rule 1). When `value` is
    /// const-evaluable and the predicate fails against it, emit a build-time
    /// `E_REFINEMENT_PREDICATE_VIOLATION`. A non-const argument is a no-op
    /// here — its predicate is enforced by the runtime assertion the
    /// interpreter / codegen constructor emits. Reuses the elision helpers.
    pub(super) fn check_distinct_constructor_predicate(
        &mut self,
        name: &str,
        base: &Type,
        arg: &Expr,
    ) {
        if let Ok(value) = self.eval_const_expr(arg, base) {
            if self.eval_refinement_predicate_const(name, arg) == Some(false) {
                self.emit_refinement_predicate_violation(name, &value, arg);
            }
        }
    }

    /// Evaluate the refinement `rname`'s `where` predicate against the
    /// const-evaluable initializer `init` at compile time. The predicate's
    /// `self` references are replaced by `init`, and the whole expression is
    /// handed to the existing const-evaluator. Returns `Some(true/false)`
    /// when the predicate reduces to a boolean, `None` when it is not
    /// const-evaluable (e.g. it calls `self.len()` on a value the evaluator
    /// has no length for).
    fn eval_refinement_predicate_const(&mut self, rname: &str, init: &Expr) -> Option<bool> {
        let mut pred = self.env.refinement_predicates.get(rname)?.expr.clone();
        subst_self_with_expr(&mut pred, init);
        match self.eval_const_expr(&pred, &Type::Bool) {
            Ok(ConstValue::Bool(b)) => Some(b),
            _ => None,
        }
    }

    /// Rule 1 build-time failure: a constant initializer reduced to a value
    /// that does not satisfy the refinement's predicate.
    fn emit_refinement_predicate_violation(&mut self, name: &str, value: &ConstValue, expr: &Expr) {
        let rendered = super::const_eval::format_const_value(value);
        self.type_error(
            format!(
                "error[E_REFINEMENT_PREDICATE_VIOLATION]: constant value `{rendered}` does not \
                 satisfy refinement `{name}`'s `where` predicate; this is a compile-time-detected \
                 contract violation — supply a value the predicate admits, or construct it through \
                 `{name}.try_from(x)?` to handle the failure at runtime"
            ),
            expr.span.clone(),
            TypeErrorKind::TypeMismatch,
        );
    }

    /// Rule 4 rejection: a runtime (non-const) base value cannot narrow
    /// implicitly into a refined slot.
    fn emit_implicit_narrowing_rejection(&mut self, name: &str, actual: &Type, expr: &Expr) {
        self.type_error(
            format!(
                "error[E_REFINEMENT_IMPLICIT_NARROWING]: cannot narrow `{}` to refinement `{name}` \
                 implicitly — the value is not a compile-time constant, so its predicate cannot be \
                 checked at build time. Use `{name}.try_from(x)?` (recoverable) or `x as {name}` \
                 (asserting) to narrow explicitly. Note: flow-sensitive narrowing is not supported, \
                 so a surrounding `if` guard does not refine the value's type",
                type_display(actual),
            ),
            expr.span.clone(),
            TypeErrorKind::TypeMismatch,
        );
    }

    /// Rule 5 rejection: a *different* refinement over the same base cannot
    /// coerce implicitly, even when the two predicates are textually equal.
    fn emit_cross_refinement_rejection(
        &mut self,
        name: &str,
        actual_name: &str,
        base: &Type,
        expr: &Expr,
    ) {
        self.type_error(
            format!(
                "error[E_REFINEMENT_IMPLICIT_NARROWING]: cannot coerce refinement `{actual_name}` \
                 to refinement `{name}` implicitly — distinct refinements over the same base `{}` \
                 have no implicit relationship, even when their predicates are textually identical \
                 (v1 has no implication-based elision). Use `{name}.try_from(x)?` or `x as {name}`",
                type_display(base),
            ),
            expr.span.clone(),
            TypeErrorKind::TypeMismatch,
        );
    }
}

/// Replace every `self` (`ExprKind::SelfValue`) reference in a refinement
/// predicate with a clone of `replacement` (the const-evaluable initializer),
/// walking the constant/operator/`self`-rooted forms the predicate grammar
/// permits (design.md § Refinement Types > "Refinement constraint
/// language"). Only the node's `kind` is overwritten so the predicate's own
/// spans survive — irrelevant here since predicate-eval errors are swallowed,
/// but it keeps the rewrite minimal.
fn subst_self_with_expr(e: &mut Expr, replacement: &Expr) {
    match &mut e.kind {
        ExprKind::SelfValue => {
            e.kind = replacement.kind.clone();
        }
        ExprKind::Binary { left, right, .. } => {
            subst_self_with_expr(left, replacement);
            subst_self_with_expr(right, replacement);
        }
        ExprKind::Unary { operand, .. } => subst_self_with_expr(operand, replacement),
        ExprKind::FieldAccess { object, .. } => subst_self_with_expr(object, replacement),
        ExprKind::MethodCall { object, args, .. } => {
            subst_self_with_expr(object, replacement);
            for a in args {
                subst_self_with_expr(&mut a.value, replacement);
            }
        }
        ExprKind::Call { callee, args } => {
            subst_self_with_expr(callee, replacement);
            for a in args {
                subst_self_with_expr(&mut a.value, replacement);
            }
        }
        _ => {}
    }
}
