//! Expression inference — the largest single submodule.
//!
//! Houses the central `check_expr` / `infer_expr` / `infer_expr_inner`
//! dispatch alongside every per-shape inference rule: binary / unary
//! operators, identifier / path resolution, the `offset_of` intrinsic,
//! the layout-query intrinsic, call inference (`infer_call`,
//! `check_call_site_marker`, explicit-generic-args, `infer_pipe`,
//! `infer_question`), method-call inference (`infer_method_call`),
//! and the `Into` / `TryInto` coercion arms. Bound-discharge and
//! call-site type substitution recording live here too because they
//! fire as part of call-site inference.

use crate::ast::*;
use crate::cross_task_safe::is_cross_task_safe_with;
use crate::resolver::SpanKey;
use crate::token::Span;
use rustc_hash::FxHashMap;
use std::collections::HashMap;

use super::env::{FunctionSig, ImplInfo};
use super::inference::{
    const_value_from_literal, instantiate_signature_with_fresh_vars, resolve_const_arg,
    resolve_type_var_top, resolve_type_vars, substitute_const_idents_in_expr,
    substitute_type_params, unify_types, InstantiatedSignature,
};
use super::types::{
    contains_type_param, float_width_rank, impl_args_match, impl_table_key,
    int_coercion_is_widening, int_signed_width, is_integer, lub_block_type, type_display,
    type_is_fully_concrete, type_to_concrete_or_param_name, type_to_mono_mangle_token,
    types_compatible, ConstArg, DimArg, IntSize, ScrutineeMode, SubstValue, Type, UIntSize,
};
use super::BreakFrame;
use super::TypeErrorKind;

/// Validate an f-string format specifier `{expr:spec}` against the hole's
/// inferred type (Phase 8 format specifiers). Runs at typecheck so `karac run`
/// and `karac build` reject the same programs at compile time. v1 supports
/// specifiers on int / float / string values only; the rules mirror what the
/// interpreter's `crate::format_spec::apply_*` and codegen's printf mapping can
/// render identically.
fn check_format_spec_for_type(spec_raw: &str, ty: &Type) -> Result<(), String> {
    let fs = crate::format_spec::FormatSpec::parse(spec_raw)?;
    let is_int = matches!(ty, Type::Int(_) | Type::UInt(_));
    let is_float = matches!(ty, Type::Float(_));
    let is_str = matches!(ty, Type::Str);
    if is_int {
        if fs.precision.is_some() {
            return Err(format!(
                "format spec `{spec_raw}`: precision (`.N`) is not valid for an integer"
            ));
        }
        return Ok(());
    }
    if is_float {
        if fs.radix != crate::format_spec::Radix::Dec {
            return Err(format!(
                "format spec `{spec_raw}`: a radix type (x/X/o) is not valid for a float"
            ));
        }
        if fs.precision.is_none() {
            return Err(format!(
                "format spec `{spec_raw}`: a float format spec needs a precision \
                 (e.g. `{{x:.2}}` or `{{x:8.2}}`)"
            ));
        }
        return Ok(());
    }
    if is_str {
        if fs.radix != crate::format_spec::Radix::Dec {
            return Err(format!(
                "format spec `{spec_raw}`: a radix type (x/X/o) is not valid for a string"
            ));
        }
        if fs.precision.is_some() {
            return Err(format!(
                "format spec `{spec_raw}`: precision (`.N`) on a string is not yet supported \
                 (planned follow-up); width and alignment are available"
            ));
        }
        if fs.zero_pad {
            return Err(format!(
                "format spec `{spec_raw}`: zero-pad (`0`) is not valid for a string"
            ));
        }
        return Ok(());
    }
    Err(format!(
        "format spec `{spec_raw}`: format specifiers apply to int, float, and string values \
         only (got `{}`)",
        type_display(ty)
    ))
}

/// The component exprs of a (possibly tuple-desugared) index expression —
/// `t[i, j]` arrives as `Tuple([i, j])`; a single index yields one slot.
fn tuple_index_parts(index: &Expr) -> Vec<Option<&Expr>> {
    match &index.kind {
        ExprKind::Tuple(parts) => parts.iter().map(Some).collect(),
        _ => vec![Some(index)],
    }
}

/// Is this expectation solid enough to seed a generic call from? It must carry
/// no inference metavar AND no un-instantiated type parameter.
///
/// The `TypeParam` half is what `contains_type_var` alone misses: an
/// unannotated generic struct literal checks its field against the DECLARED
/// slot (`Option[T]` with a bare `T`), not against a metavar, so a metavar-only
/// guard let seeding fire and bind the constructor's payload to the parameter's
/// own name — after which `T` could not be inferred at all
/// (`let b = Boxed { v: Some("x".to_string()) }`). Reuses the existing
/// `contains_type_param` from the types module. B-2026-08-05-25.
fn expectation_is_concrete(t: &Type) -> bool {
    !contains_type_var(t) && !contains_type_param(t)
}

/// Does this type still carry an unsolved inference metavar? Gates
/// expected-return seeding: a fully concrete return has nothing to seed, and
/// unifying it against a mismatched expectation could bind ids inside the
/// EXPECTATION instead. B-2026-08-05-19.
fn contains_type_var(t: &Type) -> bool {
    match t {
        Type::TypeVar(_) => true,
        Type::Named { args, .. } => args.iter().any(contains_type_var),
        Type::Ref(i) | Type::MutRef(i) => contains_type_var(i),
        Type::Tuple(ts) => ts.iter().any(contains_type_var),
        Type::Array { element, .. } => contains_type_var(element),
        Type::Slice { element, .. } => contains_type_var(element),
        Type::Function {
            params,
            return_type,
        }
        | Type::OnceFunction {
            params,
            return_type,
        } => params.iter().any(contains_type_var) || contains_type_var(return_type),
        _ => false,
    }
}

impl<'a> super::TypeChecker<'a> {
    /// design.md § `loop` type inference: "All `break` values must agree
    /// (or unify); a type mismatch across `break` sites is a compile
    /// error." Reported here rather than left to `lub_block_type`, which
    /// silently picks a candidate — silence is what let the whole
    /// break-value surface rot unnoticed (B-2026-08-24-10).
    ///
    /// `Never` contributes nothing to the join (rule 0) and `Error` means a
    /// diagnostic already fired, so both are skipped.
    fn check_break_values_agree(&mut self, values: &[Type], span: Span) {
        let mut first: Option<&Type> = None;
        for v in values {
            if *v == Type::Never || *v == Type::Error {
                continue;
            }
            match first {
                None => first = Some(v),
                Some(f) => {
                    if !types_compatible(f, v) {
                        self.type_error(
                            format!(
                                "`break` value types disagree: '{}' and '{}'. \
                                 Every `break` out of the same loop or labeled \
                                 block must carry the same type",
                                type_display(f),
                                type_display(v)
                            ),
                            span,
                            TypeErrorKind::TypeMismatch,
                        );
                        return;
                    }
                }
            }
        }
    }
    /// `true` when `expr` is a `Coll.try_with_capacity(n)` path call — the
    /// fallible constructor whose `?`-form needs check-mode element pinning
    /// (phase-8-stdlib-floor item 8).
    fn is_try_with_capacity_call(expr: &Expr) -> bool {
        let ExprKind::Call { callee, args } = &expr.kind else {
            return false;
        };
        args.len() == 1
            && matches!(&callee.kind, ExprKind::Path { segments, .. }
                if segments.len() == 2 && segments[1] == "try_with_capacity")
    }

    /// `true` when `expected` is `Result[<ok>, _]` whose `ok` payload matches
    /// the collection a `coll.try_with_capacity` produces — `Vec`/`VecDeque`
    /// map to a same-named `Named` Ok payload, `String` to `Type::Str`.
    fn try_with_capacity_result_matches(coll: &str, expected: &Type) -> bool {
        let Type::Named { name, args } = expected else {
            return false;
        };
        if name != "Result" || args.len() != 2 {
            return false;
        }
        match coll {
            "Vec" => matches!(&args[0], Type::Named { name, .. } if name == "Vec"),
            "VecDeque" => matches!(&args[0], Type::Named { name, .. } if name == "VecDeque"),
            "String" => matches!(&args[0], Type::Str),
            _ => false,
        }
    }

    /// Whether a weak-store referent and the field's declared inner type name
    /// the SAME shared type. A strong handle surfaces as either `Type::Shared(n)`
    /// (a pattern/constructor-bound handle) or `Type::Named { name: n, .. }` (a
    /// let-bound value), and the field's inner is likewise one of those, so a
    /// plain `==` / `types_compatible` misses the `Shared("Node")` vs
    /// `Named{"Node"}` cross-form. Compare the extracted base names.
    fn weak_referent_names_match(&self, referent: &Type, inner: &Type) -> bool {
        fn base_name(t: &Type) -> Option<&str> {
            match t {
                Type::Shared(n) => Some(n.as_str()),
                Type::Named { name, .. } => Some(name.as_str()),
                Type::Rc(i) | Type::Arc(i) | Type::Ref(i) | Type::MutRef(i) => base_name(i),
                _ => None,
            }
        }
        match (base_name(referent), base_name(inner)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// The CONTEXTUAL type a scalar-numeric collection literal should adopt from
    /// its expected type (`let v: Vec[u16] = [1, 2, 3]` -> `Vec[u16]`), or
    /// `None` when the expression is not such a literal or the context does not
    /// name a scalar element.
    ///
    /// Factored out of the post-`check_assignable` re-record block so the same
    /// decision can be made BEFORE the assignability check. That block's comment
    /// asserted "acceptance semantics are unchanged — `check_assignable` above
    /// already ruled", which held only while the check was permissive about
    /// numeric generic arguments. B-2026-08-05-19 makes two concrete numeric
    /// generic args invariant, so an unsuffixed literal defaulting to `i64` now
    /// has to adopt `u16` before acceptance is judged, not after. B-2026-08-05-19.
    fn contextual_scalar_collection_type(expr: &Expr, expected: &Type) -> Option<Type> {
        if !matches!(
            &expr.kind,
            ExprKind::ArrayLiteral(_)
                | ExprKind::PrefixCollectionLiteral { .. }
                | ExprKind::RepeatLiteral { .. }
        ) {
            return None;
        }
        fn is_scalar_numeric(t: &Type) -> bool {
            matches!(t, Type::Int(_) | Type::UInt(_) | Type::Float(_))
        }
        let ctx = match expected {
            Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
            other => other,
        };
        match ctx {
            Type::Named { name, args }
                if (name == "Vec" || name == "VecDeque")
                    && args.len() == 1
                    && is_scalar_numeric(&args[0]) =>
            {
                Some(ctx.clone())
            }
            Type::Slice { element, .. } if is_scalar_numeric(element) => Some(Type::Named {
                name: "Vec".to_string(),
                args: vec![(**element).clone()],
            }),
            // `Array[T, N]`: the dedicated check-mode arm further up only
            // recognises an `ArrayLiteral`, so a PREFIX literal
            // (`first(Array[10, 20, 30])` against `Array[i32, 3]`) reached the
            // assignability check still typed `Array[i64, 3]`.
            Type::Array { element, .. } if is_scalar_numeric(element) => Some(ctx.clone()),
            _ => None,
        }
    }

    /// B-2026-08-14-11 — record an UNSUFFIXED FLOAT literal at the width its
    /// DESTINATION declares, so `let a: f32 = 0.1` is the same value as
    /// `let a: f32 = 0.1f32`.
    ///
    /// Synthesis types a bare literal `f64` (`type_from_float_suffix`'s `None`
    /// arm) and nothing moved it, so the literal's own span said `f64` while it
    /// sat in a narrow-float slot. The interpreter reads that span, so it kept
    /// the full double at every such position; codegen narrowed at all but the
    /// annotated `let`. `a == b` against the suffixed spelling then answered
    /// `false` under `--interp` and `true` compiled, on a program with no
    /// arithmetic in it.
    ///
    /// Narrow floats ONLY — an `f64` context leaves the recording exactly as it
    /// was. A SUFFIXED literal is untouched: the suffix is the author naming
    /// the width. Tuples recurse per slot, because a tuple literal whose
    /// elements are all plain values never checks its elements against their
    /// slots (the element-wise arm above fires only for an inferred
    /// constructor), so the recursion is where `let t: (f32, f32) = (0.1, 0.2)`
    /// is reached.
    pub(super) fn record_narrow_float_literal(&mut self, expr: &Expr, expected: &Type) {
        /// The element type of a SEQUENCE context (B-2026-08-31-20). The same
        /// argument the tuple arm rests on: a collection literal whose elements
        /// are plain values does not check them against their slots — the
        /// Vec-context arm in `check_expr` SYNTHESIZES its elements — so
        /// nothing ever reaches the literal with the element type in hand.
        ///
        /// `Vector[T, N]` is deliberately absent: nothing was measured through
        /// it, and its lane literal is a call rather than a collection literal.
        fn seq_elem(t: &Type) -> Option<&Type> {
            match t {
                Type::Array { element, .. } | Type::Slice { element, .. } => Some(element),
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(&args[0])
                }
                _ => None,
            }
        }
        /// Which type argument a value-carrying prelude constructor fills:
        /// `Some(x)` / `Ok(x)` take the FIRST, `Err(e)` the SECOND. Both
        /// spellings the parser produces are accepted — a bare `Some` is an
        /// identifier, `Option.Some` a two-segment path.
        fn payload_arg_index(callee: &Expr, expected_name: &str) -> Option<usize> {
            let variant = match &callee.kind {
                ExprKind::Identifier(n) => n.as_str(),
                ExprKind::Path { segments, .. } if segments.len() == 2 => segments[1].as_str(),
                _ => return None,
            };
            match (expected_name, variant) {
                ("Option", "Some") => Some(0),
                ("Result", "Ok") => Some(0),
                ("Result", "Err") => Some(1),
                _ => None,
            }
        }
        let ctx = match expected {
            Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
            other => other,
        };
        match (&expr.kind, ctx) {
            (ExprKind::Float(_, None), Type::Float(size))
                if !matches!(size, crate::typechecker::types::FloatSize::F64) =>
            {
                self.record_expr_type(&expr.span, ctx);
            }
            (ExprKind::Tuple(elems), Type::Tuple(slots)) if elems.len() == slots.len() => {
                for (e, slot) in elems.iter().zip(slots.iter()) {
                    self.record_narrow_float_literal(e, slot);
                }
            }
            // B-2026-08-31-20 — a collection literal's elements. `let v:
            // Vec[f16] = [0.1]` and `Array[0.1]` both kept f64 precision under
            // `--interp` against every compiled backend's narrowed value. The
            // row's own control used `v.push(0.1)`, which has its OWN recording
            // site (`method_vec_mutation.rs`) and therefore agreed — so the
            // literal spelling of the same store was the one nothing covered.
            (
                ExprKind::ArrayLiteral(items) | ExprKind::PrefixCollectionLiteral { items, .. },
                _,
            ) => {
                if let Some(elem) = seq_elem(ctx).cloned() {
                    for it in items {
                        self.record_narrow_float_literal(it, &elem);
                    }
                }
            }
            (ExprKind::RepeatLiteral { value, .. }, _) => {
                if let Some(elem) = seq_elem(ctx).cloned() {
                    self.record_narrow_float_literal(value, &elem);
                }
            }
            // B-2026-08-31-20 — an `Option` / `Result` PAYLOAD. The width comes
            // from the binding's annotation and has to travel through the
            // constructor call to reach the literal; nothing carried it, so
            // `Option.Some(0.1)` stayed f64 while `Option.Some(c)` with an
            // already-narrowed `c` was right, which is what made this look like
            // an Option-specific defect rather than one missing recursion.
            (ExprKind::Call { callee, args }, Type::Named { name, args: targs }) => {
                if let Some(idx) = payload_arg_index(callee, name) {
                    if let (Some(a), Some(t)) = (args.first(), targs.get(idx).cloned()) {
                        self.record_narrow_float_literal(&a.value, &t);
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn check_expr(&mut self, expr: &Expr, expected: &Type) -> Type {
        // B-2026-08-19-24 — type-directed bare unit-variant resolution. When
        // the context names an enum, a bare variant name means THAT enum's
        // variant: `let x: Second = A;`, `fn f() -> Second { A }`,
        // `want_second(A)`.
        //
        // This is the checking-position counterpart to B-2026-08-19-17 (b),
        // which rejects an ambiguous bare name in SYNTHESIS position (`let x =
        // A;`) where nothing can disambiguate it. Together they make the rule
        // uniform: the context decides when it can, and you are asked to
        // qualify only when it genuinely cannot. Before this, the losing enum
        // of a collision was unreachable by bare name in every position — even
        // `let x: Second = A;` was rejected with "expected 'Second', found
        // 'First'", so the annotation the author reached for did not help.
        //
        // Runs FIRST so it beats the fallthrough to `infer_expr`, which would
        // resolve the name context-free and emit (b)'s ambiguity error for a
        // name the context has in fact just resolved. `record_expr_type` is
        // what carries the decision to the backends: the interpreter reads it
        // back for a bare variant (B-2026-08-19-17 (a)) and codegen types the
        // expression from it, so both follow the context for free.
        //
        // Running FIRST is also why `Some`/`None`/`Ok`/`Err` are excluded (in
        // `bare_variant_from_expected`) rather than merely deprioritised.
        // MEASURED: with them included this arm sat above the weak-slot
        // coercion below — which lowers a `None` into a `weak T` field through
        // `karac_weak_downgrade` — and above the `Some(..)`/`Ok(..)`/`Err(..)`
        // constructor checks. Intercepting `None` skipped that coercion, and a
        // weak pointer and a strong `Option` differ in representation and
        // refcount semantics, so the self-host parser miscompiled into `double
        // free or corruption` at run time rather than into any type error.
        if let ExprKind::Identifier(name) = &expr.kind {
            if let Some(ty) = self.bare_variant_from_expected(name, expected) {
                self.record_expr_type(&expr.span, &ty);
                return ty;
            }
        }
        // B-2026-07-02-7: an UNSUFFIXED integer literal (bare or negated) at
        // a narrow-int-typed position must fit that type's range — `let x:
        // i8 = 200`, `f(70000)` against `i16`, `S { b: 300 }` against `u8`,
        // and return/match-arm positions alike were silently admitted (the
        // wide value flowed to the interpreter while codegen truncated at
        // the honest width — a silent run-vs-build divergence). `ref T`
        // scalar borrows peel to the inner type. Non-literal expressions
        // and non-narrow contexts fall through untouched.
        if let Some(value) = Self::unsuffixed_int_literal_value(expr) {
            let ctx = match expected {
                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                other => other,
            };
            if !self.check_int_literal_fits(value, ctx, &expr.span, None) {
                self.record_expr_type(&expr.span, &Type::Error);
                return Type::Error;
            }
        }
        // B-2026-07-09-7: a SUFFIXED integer literal at a differently-typed
        // boundary (`let x: u64 = -5i64`, `let x: u32 = 5_000_000_000i64`, and
        // the same at arg/return/field/match-arm positions) must still fit the
        // CONTEXTUAL type — its own-suffix validation at synthesis does not see
        // the coercion target, so a negative-into-unsigned or out-of-range value
        // silently changed sign / stayed untruncated. `check_int_literal_fits`
        // emits ONLY when the value does not fit, so an in-range coercion
        // (`5i64` into `u64`) is left untouched — the broader question of
        // whether in-range implicit integer widening at boundaries should
        // require `as` at all is a separate design decision (see the ledger
        // entry). Returning early keeps this the single diagnostic (the
        // synthesis-time own-suffix check is skipped for the error case).
        if let Some((value, sfx)) = Self::suffixed_int_literal_value(expr) {
            let ctx = match expected {
                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                other => other,
            };
            if !self.check_int_literal_fits(value, ctx, &expr.span, sfx) {
                self.record_expr_type(&expr.span, &Type::Error);
                return Type::Error;
            }
        }
        // Fallible-allocation constructor `?`-form at check-mode
        // (phase-8-stdlib-floor item 8): `let v: Vec[T] =
        // Vec.try_with_capacity(n)?`. The `?` unwraps `Result[Vec[?T],
        // AllocError]` to `Vec[?T]`, whose fresh element typevar then can't
        // unify against the declared `Vec[i64]` (the unannotated form pins
        // `?T` from a downstream op instead). Push the `Result`-wrapped
        // expected into the inner constructor so its check-mode adopt arm
        // (below) binds the element, then run the normal `?` error-
        // propagation check on the pinned operand.
        if let ExprKind::Question(inner) = &expr.kind {
            if Self::is_try_with_capacity_call(inner) {
                if self.in_defer {
                    self.type_error(
                        "'?' operator is not allowed inside defer/errdefer blocks".to_string(),
                        expr.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                let wrapped = self.result_alloc_error_type(expected.clone());
                let inner_ty = self.check_expr(inner, &wrapped);
                if inner_ty == Type::Error {
                    return Type::Error;
                }
                let result = self.resolve_question(inner_ty, &expr.span);
                self.record_expr_type(&expr.span, &result);
                return result;
            }
        }
        // B-2026-08-08-7 — the `.unwrap()` sibling of the `?` arm above.
        // `let v: Vec[i64] = Vec.try_with_capacity(0)?` pinned its element and
        // `let v: Vec[i64] = Vec.try_with_capacity(0).unwrap()` did not, purely
        // because an expectation reaches a `?` OPERAND but not a method
        // RECEIVER: the receiver is inferred, so the constructor synth-returned
        // `Result[Vec[?T], _]`, `.unwrap()` handed back `Vec[?T]`, and the
        // fresh typevar was then rejected against the declared `Vec[i64]`.
        // Naming the intermediate (`let r: Result[Vec[i64], _] = …;
        // r.unwrap()`) always worked, which is the tell that this is
        // expectation plumbing and not the constructor's own typing.
        //
        // Deliberately narrow: only a zero-arg `.unwrap()` directly on a
        // `try_with_capacity` call, which is the one receiver shape whose
        // element type has no other source. Widening this to "push the
        // Result-wrapped expectation through any `.unwrap()`" would guess at
        // the error arm for receivers that do have their own typing.
        if let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &expr.kind
        {
            if method == "unwrap" && args.is_empty() && Self::is_try_with_capacity_call(object) {
                // Published for `infer_method_call` to consume at its receiver
                // (`pending_unwrap_receiver_expectation`), NOT returned from
                // here: short-circuiting the method call skips the side-table
                // recording codegen's `unwrap` dispatcher depends on. Taken by
                // the very next `infer_method_call`, which is this one.
                self.pending_unwrap_receiver_expectation =
                    Some(self.result_alloc_error_type(expected.clone()));
                let actual = self.infer_expr(expr);
                self.pending_unwrap_receiver_expectation = None;
                self.check_assignable(expected, &actual, expr.span);
                return actual;
            }
        }
        // Weak-field STORE coercion (the downgrade). A `weak T` field slot
        // accepts a bare strong `T` / `shared T`, an `Option[T]`, or `None` —
        // codegen lowers all of them to the single nullable weak pointer via
        // `karac_weak_downgrade` (`docs/spikes/weak-refs.md`, B-2026-07-19-8).
        // Intercept before the generic `types_compatible` path, which would
        // reject `Node` / `Option[Node]` against the raw `weak Node` slot.
        if let Type::Weak(inner) = expected {
            let actual = self.infer_expr(expr);
            // The value the weak slot references, after peeling an `Option`
            // wrapper (`Some(x)` / `None` / `Option[T]`): the inner strong type.
            let referent = match &actual {
                Type::Named { name, args } if name == "Option" && args.len() == 1 => &args[0],
                other => other,
            };
            let ok = matches!(actual, Type::Error | Type::Never)
                // `None` leaves the referent unbound (`Option[?]`) — accept it.
                || matches!(referent, Type::TypeVar(_) | Type::TypeParam(_) | Type::Error)
                || super::types::types_compatible(referent, inner)
                || self.weak_referent_names_match(referent, inner);
            if !ok {
                self.type_error(
                    format!(
                        "cannot store value of type '{}' into a `weak {}` field; \
                         expected `{}`, `Option[{}]`, or `None`",
                        type_display(&actual),
                        type_display(inner),
                        type_display(inner),
                        type_display(inner),
                    ),
                    expr.span,
                    TypeErrorKind::TypeMismatch,
                );
                self.record_expr_type(&expr.span, &Type::Error);
                return Type::Error;
            }
            self.record_expr_type(&expr.span, expected);
            return expected.clone();
        }
        // Enum-payload constructors at check-mode: `Ok(x)` / `Err(x)` against
        // an expected `Result[T, E]`, and `Some(x)` against `Option[T]`.
        // Push the PAYLOAD slot inward so a type-inferred constructor in the
        // payload resolves against it.
        //
        // Without this, `fn f() -> Result[Vec[i64], String] { Ok(Vec.new()) }`
        // was rejected: `Ok`'s argument was inferred in synthesis mode, minting
        // `Vec[?T]`, and the enclosing `Result[Vec[?T], E]` could not unify
        // against the declared `Result[Vec[i64], String]` — reported as
        // `expected 'Result<Vec<i64>, String>', found 'Result<Vec<?T2>, E>'`.
        // Every payload whose type was already known worked (`Ok(0)`,
        // `Ok(Vec[1, 2])`, `Ok(v)` on an annotated binding, `Ok(None)`), which
        // is why this survived: it needs a payload that is itself
        // inference-driven, i.e. `Vec.new()` / `Map.new()`.
        //
        // Placed BEFORE the collection-constructor short-circuit below so the
        // payload lands there with a concrete expectation already in hand.
        if let ExprKind::Call { callee, args } = &expr.kind {
            if args.len() == 1 {
                // B-2026-08-05-24 — accept the QUALIFIED spelling too. This
                // block used to match only an `Identifier` callee, i.e. bare
                // `Some(..)` / `Ok(..)` / `Err(..)`, so `Option.Some(1)` and
                // `Result.Err(-1)` never reached the adoption below and their
                // literal stayed `i64`. While generic arguments were permissive
                // about numeric width that mismatch was simply tolerated, so the
                // gap was invisible; B-2026-08-05-19 closed the hole and turned
                // it into a hard error on a spelling the language treats as
                // equivalent everywhere else.
                //
                // The `(ctor, expected)` match below already pairs `Some` with
                // `Option` and `Ok`/`Err` with `Result`, so a mismatched
                // qualifier (`Result.Some`) still falls through to `None`.
                let ctor_name: Option<&str> = match &callee.kind {
                    ExprKind::Identifier(c) => Some(c.as_str()),
                    ExprKind::Path { segments, .. }
                        if segments.len() == 2
                            && matches!(segments[0].as_str(), "Option" | "Result") =>
                    {
                        Some(segments[1].as_str())
                    }
                    _ => None,
                };
                if let Some(ctor) = ctor_name {
                    let payload_slot = match (ctor, expected) {
                        ("Ok", Type::Named { name, args: ta })
                            if name == "Result" && ta.len() == 2 =>
                        {
                            Some(ta[0].clone())
                        }
                        ("Err", Type::Named { name, args: ta })
                            if name == "Result" && ta.len() == 2 =>
                        {
                            Some(ta[1].clone())
                        }
                        ("Some", Type::Named { name, args: ta })
                            if name == "Option" && ta.len() == 1 =>
                        {
                            Some(ta[0].clone())
                        }
                        _ => None,
                    };
                    // Narrowly scoped to a payload that is itself a
                    // type-INFERRED collection constructor (`Vec.new()`,
                    // `Map.new()`, ...), which is the shape that cannot resolve
                    // without the expectation. Pushing the slot at EVERY
                    // payload changes integer handling: `return Some(i)` with
                    // `i: i64` into an `Option[u64]` then check-mode-coerces
                    // and trips the i64->u64 narrowing diagnostic, where
                    // synthesis mode had accepted it (caught by
                    // `book_snippets_compile` on ch08's `find[T]`). Every
                    // neighbouring short-circuit here is targeted the same way.
                    let payload_is_inferred_ctor = matches!(
                        &args[0].value.kind,
                        ExprKind::Call { callee: inner_callee, args: inner_args }
                            if inner_args.is_empty()
                                && matches!(
                                    &inner_callee.kind,
                                    ExprKind::Path { segments, .. }
                                        if segments.len() == 2 && segments[1] == "new"
                                )
                    );
                    // B-2026-08-05-19 — also push the slot for an UNSUFFIXED
                    // INTEGER LITERAL payload. `let o: Option[u16] = Some(3)`
                    // used to be accepted only because generic arguments were
                    // permissive about numeric width: the literal inferred `i64`
                    // and `Option[i64]` was let through against `Option[u16]`.
                    // With that hole closed the literal has to actually adopt
                    // `u16`, which is what pushing the slot does.
                    //
                    // A literal is the safe extension precisely where the
                    // comment above says a blanket push is not: the warned case
                    // is `Some(i)` with `i: i64` into an `Option[u64]`, an
                    // IDENTIFIER, which stays in synthesis mode here. Range
                    // checking still applies through the normal literal path, so
                    // `Some(300)` into `Option[u8]` is still caught.
                    let payload_is_unsuffixed_int =
                        Self::unsuffixed_int_literal_value(&args[0].value).is_some();
                    if let Some(slot) = payload_slot {
                        if payload_is_inferred_ctor || payload_is_unsuffixed_int {
                            self.check_expr(&args[0].value, &slot);
                            self.record_expr_type(&expr.span, expected);
                            return expected.clone();
                        }
                    }
                }
            }
        }

        // B-2026-08-02-11 — tuple LITERALS at check-mode: push each expected
        // element type into elements that are themselves type-INFERRED
        // collection constructors (`(Vec.new(), 3)` against
        // `(Vec[i64], i64)`, at a let annotation or a struct-literal field),
        // which cannot resolve without the expectation — the tuple sibling
        // of the Ok/Some payload and bare-constructor short-circuits here,
        // with the same narrow scoping: a non-constructor element keeps
        // synthesis mode so integer handling is unchanged, and the returned
        // Tuple of per-element types still flows through the caller's
        // compatibility check so a genuine mismatch elsewhere reports.
        if let (ExprKind::Tuple(elems), Type::Tuple(exp_elems)) = (&expr.kind, expected) {
            if !elems.is_empty() && elems.len() == exp_elems.len() {
                let elem_is_inferred_ctor = |e: &Expr| -> bool {
                    matches!(
                        &e.kind,
                        ExprKind::Call { callee, args }
                            if args.is_empty()
                                && matches!(
                                    &callee.kind,
                                    ExprKind::Path { segments, .. }
                                        if segments.len() == 2 && segments[1] == "new"
                                )
                    )
                };
                if elems.iter().any(elem_is_inferred_ctor) {
                    let types: Vec<Type> = elems
                        .iter()
                        .zip(exp_elems.iter())
                        .map(|(e, slot)| {
                            if elem_is_inferred_ctor(e) {
                                self.check_expr(e, slot)
                            } else {
                                self.infer_expr(e)
                            }
                        })
                        .collect();
                    let result = Type::Tuple(types);
                    // Early-return ONLY when the assembled tuple satisfies the
                    // expectation — the Let/field callers trust check_expr's
                    // return, so an unconditional return would swallow a
                    // genuine mismatch in a non-constructor element
                    // (`(Vec.new(), "x")` against `(Vec[i64], i64)`). On
                    // mismatch, fall through to the generic path, which
                    // re-infers and reports the standard expected/found error.
                    if super::types::types_compatible(&result, expected) {
                        self.record_expr_type(&expr.span, &result);
                        return result;
                    }
                }
            }
        }

        // Built-in collection constructors at check-mode: `Vec.new()` /
        // `VecDeque.new()` / `Set.new()` / `SortedSet.new()` / `Map.new()`
        // resolve to the expected type directly when the surface names
        // line up. Without this short-circuit the constructor's synth-
        // mode return (`Vec[?T]` minted by `infer_call`) flows through
        // `types_compatible`, which can't unify the fresh typevar
        // against `Vec<Fn()>` etc. (the existing legacy callers' shape).
        if let ExprKind::Call { callee, args } = &expr.kind {
            if args.is_empty() {
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 && segments[1] == "new" {
                        let collection = segments[0].as_str();
                        let matches_expected = match (collection, expected) {
                            ("Vec", Type::Named { name, .. }) => name == "Vec",
                            ("VecDeque", Type::Named { name, .. }) => name == "VecDeque",
                            ("Set", Type::Named { name, .. }) => name == "Set",
                            ("SortedSet", Type::Named { name, .. }) => name == "SortedSet",
                            ("SortedMap", Type::Named { name, .. }) => name == "SortedMap",
                            ("Map", Type::Named { name, .. }) => name == "Map",
                            _ => false,
                        };
                        if matches_expected {
                            self.record_expr_type(&expr.span, expected);
                            return expected.clone();
                        }
                        // `Channel.new()` at an annotated check-mode position
                        // (`let (tx, rx): (Sender[i64], Receiver[i64]) =
                        // Channel.new();`). Its synth-mode return is
                        // `(Sender[?T], Receiver[?T])`; the fresh typevar
                        // nested inside the tuple's `Named` args doesn't unify
                        // against the declared element type through
                        // `types_compatible`, which rejects with "expected
                        // (Sender<i64>, Receiver<i64>), found (Sender<?T0>,
                        // Receiver<?T0>)". Adopt the expected tuple directly
                        // when it is the `(Sender[T], Receiver[T])` shape —
                        // the same recovery the collection constructors above
                        // get. (Unannotated `let (tx, rx) = Channel.new();`
                        // takes the synth path and pins `?T` from a downstream
                        // `tx.send(x)` / `rx.recv()` instead.)
                        if collection == "Channel" {
                            if let Type::Tuple(elems) = expected {
                                let is_channel_pair = elems.len() == 2
                                    && matches!(&elems[0], Type::Named { name, .. } if name == "Sender")
                                    && matches!(&elems[1], Type::Named { name, .. } if name == "Receiver");
                                if is_channel_pair {
                                    self.record_expr_type(&expr.span, expected);
                                    return expected.clone();
                                }
                            }
                        }
                    }
                }
            }
            // Same check-mode short-circuit for `Vec.with_capacity(n)` /
            // `VecDeque.with_capacity(n)`. The synth-mode arm in
            // `expr_call.rs` returns `Vec[?T]` so an untyped
            // `let mut v = Vec.with_capacity(8); v.push(x);` can pin from
            // the downstream push; but at an annotated check-mode position
            // (`let mut v: Vec[char] = Vec.with_capacity(8);`) the fresh
            // typevar doesn't unify against the declared element type and
            // `types_compatible` rejects with "expected Vec<char>, found
            // Vec<?T0>". Adopt the expected type directly here, then
            // typecheck the capacity arg as i64. Latent since the
            // `with_capacity` arm landed; surfaced by the CLI typecheck-
            // error gate added at db573a4 (the in-tree codegen tests don't
            // gate on typecheck errors so they pass past this).
            if args.len() == 1 {
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 && segments[1] == "with_capacity" {
                        let collection = segments[0].as_str();
                        let matches_expected = match (collection, expected) {
                            ("Vec", Type::Named { name, .. }) => name == "Vec",
                            ("VecDeque", Type::Named { name, .. }) => name == "VecDeque",
                            _ => false,
                        };
                        if matches_expected {
                            let cap_ty = self.infer_expr(&args[0].value);
                            self.check_assignable(
                                &Type::Int(IntSize::I64),
                                &cap_ty,
                                args[0].value.span,
                            );
                            self.record_expr_type(&expr.span, expected);
                            return expected.clone();
                        }
                    }
                }
            }
            // Fallible-allocation constructor companion at check-mode
            // (phase-8-stdlib-floor item 8): a `let r: Result[Vec[T],
            // AllocError] = Vec.try_with_capacity(n)` binds the `Result`
            // directly. The zero-arg `try_with_capacity` synth-returns
            // `Result[Vec[?T], _]`, whose nested fresh element typevar
            // `types_compatible` can't unify against the declared
            // `Result[Vec[i64], _]` — the same hazard as `with_capacity`
            // above, one `Result` layer deeper. Adopt the expected `Result`
            // type, then typecheck the capacity arg as i64. (VecDeque/String
            // type-check here too; their codegen is gated separately and
            // still rejects with the item-8 message under `karac build`.)
            if args.len() == 1 {
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 && segments[1] == "try_with_capacity" {
                        let coll = segments[0].as_str();
                        if Self::try_with_capacity_result_matches(coll, expected) {
                            let cap_ty = self.infer_expr(&args[0].value);
                            self.check_assignable(
                                &Type::Int(IntSize::I64),
                                &cap_ty,
                                args[0].value.span,
                            );
                            self.record_expr_type(&expr.span, expected);
                            return expected.clone();
                        }
                    }
                }
            }
            // Same check-mode short-circuit for `Vec.filled(n, fill)` so
            // an annotated `let mut v: Vec[Vec[i64]] = Vec.filled(N,
            // Vec.new())` propagates `Vec[i64]` into the fill arg, which
            // then hits the `Vec.new()` short-circuit above and gets
            // pinned cleanly. Without this arm, `Vec.filled(n, Vec.new())`
            // synth-mode returns `Vec[Vec[?T0]]`, the fresh typevar never
            // unifies against the declared `Vec[i64]`, and
            // `types_compatible` rejects with "expected Vec<Vec<i64>>,
            // found Vec<Vec<?T0>>" — surfaced 2026-05-25 by kata 3629's
            // `bench/bfs_sieve.kara::build_factors`.
            // `Vec.from_fn(n, f)` in check position — push the destination's
            // element type through as the function's RETURN type, the same
            // pushdown the `Vec.filled` arm below does for its value argument
            // (B-2026-08-21-10). Without it a `let v: Vec[i32] =
            // Vec.from_fn(3, |i| 7)` synths `Vec[i64]` from the literal body
            // and is refused against the annotation.
            if args.len() == 2 {
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 && segments[0] == "Vec" && segments[1] == "from_fn" {
                        if let Type::Named {
                            name,
                            args: type_args,
                        } = expected
                        {
                            if name == "Vec" && type_args.len() == 1 {
                                let n_ty = self.infer_expr(&args[0].value);
                                self.check_assignable(
                                    &Type::Int(IntSize::I64),
                                    &n_ty,
                                    args[0].value.span,
                                );
                                let f_ty = Type::Function {
                                    params: vec![Type::Int(IntSize::I64)],
                                    return_type: Box::new(type_args[0].clone()),
                                };
                                self.check_expr(&args[1].value, &f_ty);
                                self.record_expr_type(&expr.span, expected);
                                return expected.clone();
                            }
                        }
                    }
                }
            }
            if args.len() == 2 {
                if let ExprKind::Path { segments, .. } = &callee.kind {
                    if segments.len() == 2 && segments[0] == "Vec" && segments[1] == "filled" {
                        if let Type::Named {
                            name,
                            args: type_args,
                        } = expected
                        {
                            if name == "Vec" && type_args.len() == 1 {
                                let n_ty = self.infer_expr(&args[0].value);
                                self.check_assignable(
                                    &Type::Int(IntSize::I64),
                                    &n_ty,
                                    args[0].value.span,
                                );
                                // Push the inner element type into the
                                // fill arg so a nested `Vec.new()` /
                                // `Vec.with_capacity(n)` constructor at
                                // that position can short-circuit on it.
                                self.check_expr(&args[1].value, &type_args[0]);
                                self.record_expr_type(&expr.span, expected);
                                return expected.clone();
                            }
                        }
                    }
                }
            }
        }

        // DataFrame `column(name)` at a check-mode position
        // (`let c: Column[T] = df.column("x");`). Its synth-mode return is
        // `Column[?fresh]` — the element type can't be bound from a
        // non-generic `DataFrame` receiver, so the fresh typevar doesn't
        // unify against the declared `Column[T]` through `check_assignable`
        // (the same hazard `Vec.with_capacity` hits above). Adopt the
        // expected `Column` type directly once the receiver is confirmed a
        // DataFrame and the name arg is a String. Unannotated
        // `let c = df.column("x");` takes the synth path in
        // `expr_method_call.rs` and pins `?fresh` from a downstream use.
        if let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &expr.kind
        {
            if method == "column" && args.len() == 1 {
                if let Type::Named { name, .. } = expected {
                    if name == "Column" {
                        let recv = self.infer_expr(object);
                        let is_df = match &recv {
                            Type::Named { name, .. } => name == "DataFrame",
                            Type::Ref(i) | Type::MutRef(i) => {
                                matches!(i.as_ref(), Type::Named { name, .. } if name == "DataFrame")
                            }
                            _ => false,
                        };
                        if is_df {
                            let name_ty = self.infer_expr(&args[0].value);
                            self.check_assignable(&Type::Str, &name_ty, args[0].value.span);
                            self.record_expr_type(&expr.span, expected);
                            return expected.clone();
                        }
                    }
                }
            }
        }

        // Empty prefix-literal (`Vec[]` / `Array[]` / `Set[]` / `Map[]`) at
        // a check-mode position: recover via the expected type. Synthesis-
        // mode use (no annotation, no expected-type carrier) hits the
        // matching arm in `infer_expr_inner` and emits
        // `E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION`. Per design.md
        // § Collection Literals: an empty prefix-literal has no element
        // type to infer.
        if let ExprKind::PrefixCollectionLiteral { type_name, items } = &expr.kind {
            if items.is_empty() {
                let matches_expected = match (type_name.as_str(), expected) {
                    ("Vec", Type::Named { name, .. }) => name == "Vec",
                    ("Set", Type::Named { name, .. }) => name == "Set",
                    ("Map", Type::Named { name, .. }) => name == "Map" || name == "HashMap",
                    ("Array", Type::Array { .. }) => true,
                    _ => false,
                };
                if matches_expected {
                    self.record_expr_type(&expr.span, expected);
                    return expected.clone();
                }
            }
        }
        // Bare-identifier call at an expected-type position: `default()` where
        // expected is `T: Default` or a concrete type with an `impl Default`.
        // Intercepts before normal inference so the typechecker can substitute
        // the missing receiver (`T.default()` / `Wrapper.default()`).
        if let ExprKind::Call { callee, args } = &expr.kind {
            if let ExprKind::Identifier(name) = &callee.kind {
                if let Some(ty) =
                    self.try_apply_expected_assoc_fn_inference(name, args, expected, &expr.span)
                {
                    return ty;
                }
            }
        }

        // Check-mode coercion: bare `[...]` literal → `Array[T, N]` when the
        // expected type is a fixed-size array. This overrides the synthesis-mode
        // default of Vec[T] so annotated lets and typed call arguments work.
        if let (ExprKind::ArrayLiteral(elements), Type::Array { element, size }) =
            (&expr.kind, expected)
        {
            // Length-mismatch check skipped for non-literal sizes (slice 3
            // `ConstParam` / `ConstVar` resolve at mono-emission time).
            if let Some(n) = size.as_usize() {
                if elements.len() != n {
                    self.type_error(
                        format!(
                            "array literal has {} element(s), expected {}",
                            elements.len(),
                            n
                        ),
                        expr.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            for elem in elements {
                self.check_expr(elem, element);
                // B-2026-09-03-20 — an array-literal element is consumed by
                // value, the same value position as a tuple element.
                self.warn_partial_move_of_drop_struct(elem, element);
            }
            self.record_expr_type(&expr.span, expected);
            return expected.clone();
        }
        // Same coercion for bare `[v; n]` against an `Array[T, N]` expected:
        // the literal's count must equal N, and the value's type must match T.
        if let (
            ExprKind::RepeatLiteral {
                type_name: None,
                value,
                count,
            },
            Type::Array { element, size },
        ) = (&expr.kind, expected)
        {
            if let ExprKind::Integer(n, _) = &count.kind {
                // Length-mismatch check skipped for non-literal sizes
                // (slice 3 `ConstParam` / `ConstVar` resolve at mono-
                // emission time).
                if let Some(expected_size) = size.as_usize() {
                    if *n < 0 || *n as usize != expected_size {
                        self.type_error(
                            format!(
                                "repeat-literal count {} does not match expected array length {}",
                                n, expected_size
                            ),
                            count.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
            } else {
                self.type_error(
                    "Array[T, N] repeat-literal requires a non-negative integer literal count"
                        .to_string(),
                    count.span,
                    TypeErrorKind::TypeMismatch,
                );
                self.infer_expr(count);
            }
            self.check_expr(value, element);
            self.record_expr_type(&expr.span, expected);
            return expected.clone();
        }
        if let Some(coerced) = self.try_apply_into_coercion(expr, expected) {
            return coerced;
        }
        if let Some(coerced) = self.try_apply_tryinto_coercion(expr, expected) {
            return coerced;
        }
        if let Some(coerced) = self.try_apply_parse_coercion(expr, expected) {
            return coerced;
        }
        // Closure pushdown: when expected is `Type::Function { params, return }`
        // (or `Type::OnceFunction { ... }`, item 131 sub-step 3) and `expr` is
        // a closure literal, seed each closure param's type from the expected
        // param type instead of letting the synth path fall back to
        // `fresh_type_var()`. Required for compound type+effect polymorphism
        // (round 10.1 step 2): once the call site has solved `T = Iter[i32]`
        // and substituted `T.Item -> &i32` into the param's `Fn(T.Item) -> ...`,
        // the closure body must be type-checked against that concrete shape.
        // Explicit param annotations on the closure still take priority.
        // OnceFunction slots use the same pushdown — the slot's signature
        // describes call arity/types regardless of repeat-callability, and
        // sub-step 3's `is_subtype` then admits a Function-typed closure
        // into an OnceFunction slot via the cross-arm subsumption rule.
        let expected_fn_shape = match expected {
            Type::Function {
                params,
                return_type,
            }
            | Type::OnceFunction {
                params,
                return_type,
            } => Some((params.as_slice(), return_type.as_ref())),
            _ => None,
        };
        if let (
            ExprKind::Closure {
                params,
                capture_mode,
                prefix_span: _,
                body,
            },
            Some((expected_params, expected_ret)),
        ) = (&expr.kind, expected_fn_shape)
        {
            if params.len() == expected_params.len() {
                // Round 12.44 (Step 2) — once-callability inference must run
                // here too so the closure's actual type reflects whether it
                // consumes a captured outer non-Copy binding. When `expected`
                // is `Type::Function` and the body promotes the closure to
                // `OnceFunction`, the trailing `check_assignable` correctly
                // rejects the cross-pair (Step 1's identity-only subtyping).
                let outer_bindings = self.flatten_local_scope_snapshot();
                let closure_param_names: Vec<String> = params
                    .iter()
                    .flat_map(|p| p.pattern.binding_names())
                    .collect();
                self.local_scope.push();
                let param_types: Vec<Type> = params
                    .iter()
                    .zip(expected_params.iter())
                    .map(|(p, expected_pty)| {
                        let ty = p
                            .ty
                            .as_ref()
                            .map(|t| self.lower_type_expr(t, &[]))
                            .unwrap_or_else(|| expected_pty.clone());
                        if !self.is_irrefutable_pattern(&p.pattern, &ty) {
                            self.type_error(
                                "refutable pattern in closure parameter; use `if let` or `match` for patterns that may not match".to_string(),
                                p.pattern.span,
                                TypeErrorKind::RefutablePattern,
                            );
                        }
                        self.bind_pattern_types(&p.pattern, &ty);
                        ty
                    })
                    .collect();
                // B-2026-08-10-17 — push the closure-scoped `return` collector
                // here too. B-2026-07-31-18 added it to the INFER-direction
                // arm only, so a closure whose type is known from context (an
                // annotated `let`, an argument to a fn with a declared `Fn`
                // param, a comparator) checked every `return` in its body
                // against `current_return_type` — the ENCLOSING FN's return
                // type. `let f: Fn(i64) -> i64 = |n| { … return 1i64; }` in a
                // `fn main()` reported "expected '()', found 'i64'", and the
                // error vanished if the enclosing fn happened to return i64,
                // which is the tell.
                //
                // Here the expected return type is already known, so the
                // collected returns are CHECKED against it rather than unified
                // with the tail the way the infer arm has to do.
                self.closure_return_types
                    .push(super::ClosureReturnFrame::default());
                let mut body_ty = self.check_expr(body, expected_ret);
                // B-2026-08-26-19 — the scalar `ref` peel that `check_assignable`
                // performs (B-2026-07-15-3) has ALREADY accepted this body
                // against `expected_ret` by the line above; record that it
                // happened, or the closure's own type keeps the borrow and the
                // trailing whole-type check compares `Fn() -> ref i64` against
                // `Fn() -> i64` and rejects a body it just approved.
                //
                // That mismatch is why the failure looked so odd: `ref i64` in
                // a plain value, return, or argument position coerced fine, and
                // only the closure tail refused it. The asymmetry is exactly
                // this — the coercion was permitted but never written back, so
                // it survived the inner check and died at the outer one. A
                // non-scalar (`ref String`) fails the inner check instead and
                // reports there, which is the correct answer and stays put.
                //
                // Condition mirrors `check_assignable`'s peel verbatim so the
                // two cannot drift into disagreeing about what coerces.
                if let Type::Ref(inner) | Type::MutRef(inner) = &body_ty {
                    let inner = (**inner).clone();
                    if Self::scalar_reads_as_value(&inner)
                        && self.is_subtype_with_projections(expected_ret, &inner)
                    {
                        body_ty = inner;
                    }
                }
                let collected = self
                    .closure_return_types
                    .pop()
                    .expect("closure return collector pushed above");
                for (t, span) in collected.returns {
                    let t = resolve_type_var_top(&t, &self.env.substitutions);
                    if matches!(t, Type::Never | Type::Error) {
                        continue;
                    }
                    self.check_assignable(expected_ret, &t, span);
                }
                self.local_scope.pop();
                let actual = self.closure_type_with_capture_inference(
                    &expr.span,
                    *capture_mode,
                    &closure_param_names,
                    body,
                    &outer_bindings,
                    param_types,
                    body_ty,
                );
                self.check_assignable(expected, &actual, expr.span);
                // B-2026-07-02-12: record the closure literal's resolved
                // `Fn` type at its own span. The lowering pass folds
                // Function-typed `expr_types` entries into
                // `Program.fn_value_typed_exprs`, which codegen's
                // `compile_closure` reads to type UN-ANNOTATED params
                // (`|a| f"{a}!"` against a `Fn(String) -> String` slot).
                // Without the record, codegen fell back to i64 params and
                // the closure's actual signature mismatched the declared-Fn
                // indirect-call ABI at every call site — String/Vec args
                // read as integers, silently.
                self.record_expr_type(&expr.span, &actual);
                return actual;
            }
            // Arity mismatch: fall through to the synth path so the existing
            // `check_assignable` produces a normal `Fn` arity diagnostic.
        }

        // Block at check position: thread `expected` through to the
        // trailing expression so closures inside `let x: T = { ...; |a| body }`
        // see `T`'s shape. `check_block_against` already routes the final
        // expression through `check_expr`.
        if let ExprKind::Block(block) = &expr.kind {
            let ty = self.check_block_against(block, expected);
            self.record_expr_type(&expr.span, &ty);
            return ty;
        }

        // If/IfLet at check position: push `expected` into both branches.
        // Each branch's `check_expr` enforces assignability against the
        // expected type independently, so divergent branches surface a
        // per-branch TypeMismatch rather than the synth-mode aggregate
        // BranchTypeMismatch (more specific, points at the offending
        // branch). Condition typing is unchanged.
        if let ExprKind::If {
            condition,
            then_block,
            else_branch,
        } = &expr.kind
        {
            let ty = self.check_if_against(
                condition,
                then_block,
                else_branch.as_deref(),
                expected,
                &expr.span,
            );
            return ty;
        }
        if let ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } = &expr.kind
        {
            let ty = self.check_if_let_against(
                pattern,
                value,
                then_block,
                else_branch.as_deref(),
                expected,
                &expr.span,
            );
            return ty;
        }

        // Match at check position: each arm body is checked against
        // `expected` so closures in arm bodies (and other check-mode-
        // sensitive shapes) see the target type.
        if let ExprKind::Match { scrutinee, arms } = &expr.kind {
            let ty = self.check_match_against(scrutinee, arms, expected, &expr.span);
            return ty;
        }

        // Generic struct literal at check position (B-2026-07-18-17): seed the
        // struct's type params from the expected type's args so a field whose
        // declared type is a struct type param (`Box[T] { value: Vec.new() }` at
        // `Box[Vec[i64]]`) gets the concrete slot pushed into its value's check,
        // resolving a type-inferred constructor arg the field values alone leave
        // as `?T`. Gated to a PLAIN generic struct whose name and arity match the
        // expectation and with no spread — enum-variant `StructLiteral` paths
        // (whose head names an enum, not a struct) and shared/`par` structs
        // (non-generic at v1) fall through to the unseeded inference below.
        if let ExprKind::StructLiteral {
            path,
            fields,
            spread: None,
            ..
        } = &expr.kind
        {
            if let Type::Named { name, args } = expected {
                let sname = path.last().map(String::as_str).unwrap_or("");
                if name == sname
                    && self.env.structs.get(sname).is_some_and(|si| {
                        !si.is_shared
                            && !si.is_par
                            && !si.generic_params.is_empty()
                            && si.generic_params.len() == args.len()
                    })
                {
                    let ty = self.infer_struct_literal_expected(
                        path,
                        fields,
                        &expr.span,
                        Some(args),
                        false,
                    );
                    self.record_expr_type(&expr.span, &ty);
                    return ty;
                }
            }
        }

        // B-2026-08-05-19 — publish the expectation for a generic CALL so its
        // type params can be seeded from it before being solved from arguments.
        // `let b: Box[i32] = Box.new(5)` infers `T = i64` from the literal
        // otherwise, and with numeric generic args now layout-invariant that
        // became a rejection of working code. Restricted to a call, and taken by
        // the first generic call that runs, so a nested call inside an argument
        // never sees it.
        // Restricted to a PATH-callee call (`Box.new(..)`, `Type.assoc(..)`) —
        // the associated-function shape this seeding exists for. A broader gate
        // leaks the expectation into a nested call inside an argument: with a
        // bare `ExprKind::Call` gate, `let b = Bag { items: [..] }` (unannotated,
        // so the expectation is an unsolved metavar) had that metavar bound by
        // the first inner call instead, and `T` then failed to infer.
        // Which callees may seed their generic params from the expectation.
        //
        // A PATH callee (`Box.new(..)`, `Tensor.from(..)`, `Option.Some(..)`) —
        // the associated-function shape the seeding was written for
        // (B-2026-08-05-19). A broader gate leaks the expectation into a nested
        // call inside an argument, which is why this is not simply "any call".
        //
        // B-2026-08-05-25 — plus the BARE enum constructors. Seeding fired for
        // `Option.Some(0 - 1)` and not for `Some(0 - 1)`, so two spellings of
        // the same construction disagreed: the qualified form adopted `i32`, the
        // bare form kept the literal's `i64` and was rejected. `Some(-1)` worked
        // in both because the payload-adoption block below admits an unsuffixed
        // literal directly; `0 - 1` is arithmetic over literals and only seeding
        // reaches it. These three names are constructors rather than arbitrary
        // calls, so admitting them does not reopen the nested-call leak, and the
        // `!contains_type_var` guard still keeps an unsolved expectation from
        // being bound by an inner call. The case the payload-adoption comment
        // warns about — `return Some(i)` with `i: i64` into an `Option[u64]` —
        // is unaffected, because seeding binds the RETURN type and the payload
        // is then an ordinary scalar assignment where numeric coercion stays
        // permissive; verified against both spellings.
        //
        // B-2026-08-08-8 — plus a BARE IDENTIFIER callee, i.e. an ordinary
        // generic free function. `let h: Holder[u32] = wrap([30, 10, 20])`
        // reported `expected Holder<u32>, found Holder<i64>` while the
        // qualified `Holder.wrap(..)` spelling of the same body adopted `u32`,
        // so which of two identical signatures a caller could use depended on
        // where it was declared. The nested-call leak the path-only gate was
        // guarding against is now blocked by the `expectation_is_concrete`
        // condition below, which post-dates that restriction: the `Bag { items:
        // [..] }` case leaked precisely because an UNANNOTATED `let` publishes
        // an unsolved metavar as its expectation, and such an expectation no
        // longer gets published at all. Verified by re-running that shape.
        let seedable_call = match &expr.kind {
            ExprKind::Call { callee, .. } => {
                matches!(
                    &callee.kind,
                    ExprKind::Path { .. } | ExprKind::Identifier(_)
                )
            }
            _ => false,
        };
        if seedable_call && *expected != Type::Error && expectation_is_concrete(expected) {
            self.pending_expected_call_return = Some(expected.clone());
        }
        let actual = self.infer_expr(expr);
        self.pending_expected_call_return = None;
        // Expected-type-driven generic resolution: when a generic call's
        // return type came back as `TypeParam(T)` (the solver had no arg
        // information to fix `T`), `expected` lets us bind `T` to a concrete
        // name for the interpreter's runtime dispatch stack. Only fires for
        // `Call` expressions — other shapes don't introduce per-call generic
        // bindings.
        if matches!(expr.kind, ExprKind::Call { .. }) {
            if let Type::TypeParam(t_name) = &actual {
                if let Some(target) = type_to_concrete_or_param_name(expected) {
                    if target != *t_name {
                        self.call_type_subs
                            .entry(SpanKey::from_span(&expr.span))
                            .or_default()
                            .insert(t_name.clone(), target);
                    }
                }
            }
        }
        // Refinement narrowing elision (design.md § Refinement Types >
        // "Compile-time elision procedure (v1)"; phase-9 line 37). When the
        // slot is a refinement that `actual` does not already satisfy, run
        // the two elision rules + the explicit-coercion rejection *before*
        // the generic `check_assignable`, since the procedure needs the
        // initializer expression (for const-eval), not just its type. This
        // single site covers every check-mode position uniformly — `let`
        // initializers, function-call arguments, struct-field inits, and
        // function-body returns all flow through `check_expr`.
        if !self.is_subtype_with_projections(expected, &actual) {
            if let Some(narrowed) = self.try_refinement_narrowing(expr, expected, &actual) {
                return narrowed;
            }
        }
        // B-2026-07-09-7 (design decision (B)): a NON-literal integer value
        // flowing into a differently-typed integer slot must widen — a
        // narrowing or sign-changing coercion (`let x: u32 = some_i64`,
        // `let x: u8 = wide_val`, signed→unsigned) requires an explicit `as`.
        // The static permissiveness that let these through is deliberate for
        // *literals* (value-checked above, so `let a: u64 = 5i64` stays fine)
        // but unsound for variables, whose value is unknown at compile time.
        // This is the variable half of B-2026-07-09-7; the literal half is
        // the two `*_int_literal_value` blocks at the top of check_expr.
        // B-2026-08-05-19: a scalar-numeric collection literal adopts its
        // contextual element type HERE, before acceptance is judged. Unsuffixed
        // integer literals infer as `i64`, so `let v: Vec[u16] = [1, 2, 3]`
        // otherwise reads as `Vec[i64]` vs `Vec[u16]` — which the new
        // generic-argument invariance correctly rejects. Adopting first means
        // the check compares `Vec[u16]` with `Vec[u16]`; the block further down
        // still re-records and still range-checks each element, so a genuinely
        // out-of-range literal (`let v: Vec[i8] = [200]`) is unaffected.
        // B-2026-08-08-7 — adopt only from a NUMERIC source. The adoption keys
        // on the expression being a collection literal and the context naming a
        // scalar element; it never looked at what the literal actually holds, so
        // `let v: Vec[u32] = ["a", "b"]` had `Vec[String]` overwritten with
        // `Vec[u32]` and sailed through the `check_assignable` on the next line.
        // That hole predates this row (it is reachable with no generics in
        // sight), but the seeded-generic-argument change above routes
        // `Column.from_vec(["a", "b"])` into check-mode too, which would have
        // widened it to a call shape that previously rejected correctly.
        // An unsolved element (the empty literal `[]`) still adopts — there is
        // nothing to disagree with.
        //
        // A FLOAT source may not adopt an INTEGER element either: `[1.5, 2.5]`
        // into a `Vec[u32]` slot is a silent truncation, not a width choice.
        // That direction was already rejected at a generic call (the argument
        // was inferred, so `Vec[f64]` met `Vec[u32]` head-on) and accepted at a
        // `let`; the change above would otherwise have resolved that
        // disagreement toward the permissive side. The reverse — an integer
        // literal adopting a float element, `let v: Vec[f32] = [1, 2, 3]` — is
        // ordinary and stays.
        let source_elem = match &actual {
            Type::Named { name, args }
                if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
            {
                Some(&args[0])
            }
            Type::Array { element, .. } | Type::Slice { element, .. } => Some(element.as_ref()),
            _ => None,
        };
        let adoptable = source_elem.is_some_and(|src| {
            let numeric = matches!(
                src,
                Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::TypeVar(_)
            );
            let truncating = matches!(src, Type::Float(_))
                && matches!(
                    Self::contextual_scalar_collection_type(expr, expected),
                    Some(Type::Named { ref args, .. }) if matches!(args.first(), Some(Type::Int(_) | Type::UInt(_)))
                );
            numeric && !truncating
        });
        let actual = match Self::contextual_scalar_collection_type(expr, expected) {
            Some(t) if actual != Type::Error && adoptable => t,
            _ => actual,
        };
        self.check_int_widening_coercion(expr, expected, &actual);
        // B-2026-08-14-12 — the float sibling of the gate above. Same call
        // site, same shape: design.md's implicit-widening table says a float
        // NARROWING needs an `as` exactly as an integer one does, and only the
        // integer half was enforced.
        self.check_float_narrowing_coercion(expr, expected, &actual);
        // B-2026-08-14-6 — the int-to-float sibling. Recording only; see the
        // helper. This is the boundary that covers an index-assign
        // (`v[i] = some_u8` on a `Vec[f64]`), a `let`, an argument and a
        // struct field in one place.
        self.record_float_coercion(expr, expected, &actual);
        // B-2026-08-14-1 — an ANNOTATED TUPLE checks element-wise, which the
        // whole-value gate above cannot do: it compares `(i64, i64)` against
        // `(u8, u8)`, and neither side is an integer, so the gate returns
        // immediately. `let t: (u8, u8) = (nsrc, nsrc)` with `nsrc: i64` was
        // therefore accepted with no diagnostic while every scalar position
        // rejected the same coercion.
        //
        // Additive by construction: it reports only narrowings, using element
        // types already inferred above, so no expression is re-inferred and no
        // previously-accepted program changes except the ones that were
        // silently truncating. The per-element span is the element's own, so
        // the diagnostic points at the offending component rather than the
        // whole tuple.
        if let (ExprKind::Tuple(elems), Type::Tuple(slots), Type::Tuple(actuals)) =
            (&expr.kind, expected, &actual)
        {
            if elems.len() == slots.len() && elems.len() == actuals.len() {
                for ((e, slot), got) in elems.iter().zip(slots.iter()).zip(actuals.iter()) {
                    self.check_int_widening_coercion(e, slot, got);
                    self.check_float_narrowing_coercion(e, slot, got);
                }
            }
        }
        self.check_assignable(expected, &actual, expr.span);
        // B-2026-07-02-6: a collection literal admitted against a
        // differently-widthed scalar element context (`total([10, 20, 30])`
        // with `v: Vec[i32]` — call args, method args, struct fields,
        // returns alike) kept its synth-mode default-width record
        // (`Vec[i64]`) in `expr_types`, so codegen packed the buffer at
        // the wrong stride and every read misindexed. Re-record the
        // literal at its CONTEXTUAL type — acceptance semantics are
        // unchanged (`check_assignable` above already ruled); only the
        // recorded width moves. Codegen's literal compilers read this
        // back through `enum_inst_type_exprs` (`literal_span_elem_hint`).
        // Scalar-element `Vec`/`VecDeque` only: wider element types have
        // no width to mispack, and `Array`-expected literals already
        // record `expected` in their dedicated check-mode arm above.
        // B-2026-08-14-11 — the SCALAR sibling of the collection re-record
        // below: an UNSUFFIXED FLOAT literal takes its width from the
        // DESTINATION, so `let a: f32 = 0.1` is the same value as
        // `let a: f32 = 0.1f32`.
        //
        // Synthesis types a bare literal `f64` (`type_from_float_suffix`'s
        // `None` arm) and nothing moved it, so the literal's own span said
        // `f64` while it sat in a narrow-float slot. The interpreter reads that
        // span and kept the full double; codegen narrowed at five of the six
        // positions but not at an annotated `let`. `a == b` against the
        // suffixed spelling then answered `false` under `--interp` and `true`
        // compiled, on a program with no arithmetic in it.
        //
        // Recorded HERE rather than at the top of `check_expr`, because the
        // fall-through above records the SYNTHESIZED type and would overwrite
        // an earlier entry — the same ordering the collection arm below
        // depends on. Narrow floats only: an `f64` context leaves the
        // recording exactly as it was, and a SUFFIXED literal is untouched
        // since the suffix is the author naming the width.
        if actual != Type::Error {
            self.record_narrow_float_literal(expr, expected);
        }
        if actual != Type::Error
            && matches!(
                &expr.kind,
                ExprKind::ArrayLiteral(_)
                    | ExprKind::PrefixCollectionLiteral { .. }
                    | ExprKind::RepeatLiteral { .. }
            )
        {
            fn is_scalar_numeric(t: &Type) -> bool {
                matches!(t, Type::Int(_) | Type::UInt(_) | Type::Float(_))
            }
            // `ref Vec[T]` / `mut ref Vec[T]` params carry the Vec inside a
            // Ref wrapper; `Slice[T]` params materialize the literal as a
            // Vec buffer first (the slice header is synthesized at the call
            // boundary), so record the literal as `Vec[T]` there too.
            let ctx = match expected {
                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                other => other,
            };
            let contextual = match ctx {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque")
                        && args.len() == 1
                        && is_scalar_numeric(&args[0]) =>
                {
                    Some(ctx.clone())
                }
                Type::Slice { element, .. } if is_scalar_numeric(element) => Some(Type::Named {
                    name: "Vec".to_string(),
                    args: vec![(**element).clone()],
                }),
                _ => None,
            };
            if let Some(t) = contextual {
                // B-2026-07-02-7: the Vec-context arm synthesizes elements
                // (unlike the Array arm, which `check_expr`s each element and
                // gets the literal-range validation for free), so validate
                // direct integer-literal elements against the contextual
                // element type here — `let v: Vec[i8] = [200]` silently
                // diverged (interp 200 vs build -56).
                if let Type::Named { args, .. } = &t {
                    let elem = &args[0];
                    let elem_exprs: Vec<&Expr> = match &expr.kind {
                        ExprKind::ArrayLiteral(items) => items.iter().collect(),
                        ExprKind::PrefixCollectionLiteral { items, .. } => items.iter().collect(),
                        ExprKind::RepeatLiteral { value, .. } => vec![value.as_ref()],
                        _ => Vec::new(),
                    };
                    let mut all_fit = true;
                    for e in elem_exprs {
                        if let Some(v) = Self::unsuffixed_int_literal_value(e) {
                            all_fit &= self.check_int_literal_fits(v, elem, &e.span, None);
                        }
                        // B-2026-08-14-12 — and the same argument one step
                        // further: a literal element is range-checked above,
                        // but a VARIABLE element is not checked at all here,
                        // and the adoption further up replaced the literal's
                        // `Vec[<lub>]` with the contextual `Vec[<elem>]` before
                        // either narrowing gate could compare them. So the one
                        // spelling that reached neither check was a wide
                        // variable inside a collection literal, and it split
                        // the backends: `let v: Vec[f32] = [c, 1.0f32]` with
                        // `c: f64` printed 0.1 under `--interp` and
                        // 0.10000000149011612 compiled; the integer twin
                        // (`let v: Vec[u8] = [n, 44u8]`, `n: i64 = 300`) read
                        // 300 and 44. Per-ELEMENT is what makes this safe —
                        // gating the adoption itself would reject
                        // `let v: Vec[u16] = [1, 2, 3]`, whose elements infer
                        // `i64` and are exactly what the adoption exists for.
                        // Both gates exempt literals internally, so only a
                        // typed element is ever reported.
                        if let Some(et) = self.expr_types.get(&SpanKey::from_span(&e.span)).cloned()
                        {
                            self.check_int_widening_coercion(e, elem, &et);
                            self.check_float_narrowing_coercion(e, elem, &et);
                        }
                    }
                    if !all_fit {
                        self.record_expr_type(&expr.span, &Type::Error);
                        return Type::Error;
                    }
                }
                self.record_expr_type(&expr.span, &t);
                return t;
            }
        }
        actual
    }

    /// Recognize `x.into()` at an expected-type position. When `expr` is a
    /// zero-argument method call named `into` and `expected` is a Named type
    /// `T` with a registered `impl From[S] for T` (where `S` is the receiver's
    /// inferred type), record the conversion and return `expected`. Returns
    /// `Some(Error)` when `.into()` matches shape but no suitable From impl
    /// exists (emits a diagnostic). Returns `None` when the expression is not
    /// a `.into()` call — caller falls back to regular inference.
    /// Bare-call expected-type inference: `name(args)` at an expected-type
    /// position resolves to `Target.name(args)` when the expected type narrows
    /// to a single trait (or impl) declaring an associated function called
    /// `name`. Returns `Some(return_type)` on dispatch, `None` to fall through
    /// to the existing inference path. Multiple matching traits → ambiguity
    /// error + `Type::Error`.
    ///
    /// `Type::TypeParam(t)` looks up `t`'s trait bounds via `enclosing_bounds`.
    /// `Type::Named { name }` looks up the type's `impl Trait for Name` blocks
    /// in `env.impls` and uses the registered impl method signature directly.
    fn try_apply_expected_assoc_fn_inference(
        &mut self,
        name: &str,
        args: &[CallArg],
        expected: &Type,
        span: &Span,
    ) -> Option<Type> {
        // If `name` is already a known function, builtin, or local, fall
        // through. Bare-call inference only applies to identifiers that
        // would otherwise be unresolvable at the value layer.
        if self.local_scope.lookup(name).is_some()
            || self.env.functions.contains_key(name)
            || self.env.constants.contains_key(name)
            || matches!(
                name,
                "todo" | "unreachable" | "println" | "print" | "eprintln" | "panic"
            )
        {
            return None;
        }

        match expected {
            Type::TypeParam(target) => {
                let bounds = self.enclosing_bounds.get(target).cloned()?;
                let candidates: Vec<String> = bounds
                    .iter()
                    .filter_map(|b| b.path.last().cloned())
                    .filter(|trait_name| self.find_trait_method(trait_name, name).is_some())
                    .collect();
                match candidates.len() {
                    0 => None,
                    1 => {
                        let trait_method = self.find_trait_method(&candidates[0], name)?.clone();
                        // Record the typeparam target so lowering rewrites
                        // the bare call to `T.name(args)`. At runtime the
                        // interpreter resolves `T` through its substitution
                        // stack to find the concrete impl.
                        self.bare_assoc_fn_targets
                            .insert(SpanKey::from_span(span), target.clone());
                        Some(self.dispatch_trait_assoc_fn(target, &trait_method, &[], args, span))
                    }
                    _ => {
                        let trait_list = candidates
                            .iter()
                            .map(|c| format!("`{}`", c))
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.type_error(
                            format!(
                                "ambiguous associated function '{}' on type parameter '{}': declared by {}. \
                                 Use UFCS `Trait.{}(...)` to disambiguate.",
                                name, target, trait_list, name,
                            ),
                            *span,
                            TypeErrorKind::AmbiguousAssocFn,
                        );
                        Some(Type::Error)
                    }
                }
            }
            Type::Named {
                name: target_name,
                args: target_args,
            } => {
                // Match against impl methods registered on this concrete type.
                // Trait impls and inherent impls share the same `env.impls`
                // table; we collect every impl whose target is `target_name`,
                // whose method set contains `name`, and whose impl-level
                // bounds discharge against the receiver's concrete generic
                // args (slice 1 of the method-resolution CR — see
                // `phase-4-interpreter.md`).
                let matching: Vec<&ImplInfo> = self
                    .env
                    .impls
                    .iter()
                    .filter(|imp| {
                        imp.target_type == *target_name
                            && impl_args_match(&imp.target_args, target_args)
                            && imp.methods.contains_key(name)
                            && self.env.impl_bounds_discharge(imp, target_args)
                    })
                    .collect();
                match matching.len() {
                    0 => None,
                    1 => {
                        let sig = matching[0].methods.get(name)?.clone();
                        // Record the resolved target so lowering can rewrite
                        // the bare call to `Target.name(args)` for the
                        // interpreter / codegen.
                        self.bare_assoc_fn_targets
                            .insert(SpanKey::from_span(span), target_name.clone());
                        Some(self.validate_args_against_sig(name, &sig, args, span))
                    }
                    _ => {
                        let trait_list = matching
                            .iter()
                            .filter_map(|imp| imp.trait_name.clone())
                            .map(|t| format!("`{}`", t))
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.type_error(
                            format!(
                                "ambiguous associated function '{}' on type '{}': declared by {}. \
                                 Use `Trait.{}(...)` to disambiguate.",
                                name, target_name, trait_list, name,
                            ),
                            *span,
                            TypeErrorKind::AmbiguousAssocFn,
                        );
                        Some(Type::Error)
                    }
                }
            }
            _ => None,
        }
    }

    /// Record per-call generic-param substitutions for use by the interpreter
    /// at runtime. Each entry maps a generic param name to a concrete type
    /// name — or to another generic param name when the caller is itself
    /// generic and propagates the binding (the interpreter resolves these
    /// transitively against its runtime substitution stack).
    fn record_call_type_subs(&mut self, span: &Span, solutions: &HashMap<String, Type>) {
        if solutions.is_empty() {
            return;
        }
        let mut frame: FxHashMap<String, String> = FxHashMap::default();
        let mut mangle_frame: FxHashMap<String, String> = FxHashMap::default();
        let mut te_frame: FxHashMap<String, crate::ast::TypeExpr> = FxHashMap::default();
        for (name, ty) in solutions {
            if let Some(resolved) = type_to_concrete_or_param_name(ty) {
                frame.insert(name.clone(), resolved);
            } else if let Type::Existential { origin, .. } = ty {
                // A generic param bound to a return-position `impl Trait`
                // VALUE — `fn total(c: impl Counter) -> i64` called as
                // `total(make(9))`. The existential names no concrete type, so
                // the mono frame got no entry and codegen had nothing to
                // instantiate `T` with; the call failed the build while the
                // interpreter ran it. Park it: the witness is resolved at
                // export, because this call site may well be checked before
                // the body that reveals it. B-2026-08-22-12.
                self.pending_existential_call_subs.push((
                    SpanKey::from_span(span),
                    name.clone(),
                    *origin,
                ));
            }
            // Element-aware mangle token (B-2026-07-11-35): the head-name `frame`
            // above erases `Vec[i64]` vs `Vec[String]` to `"Vec"`; this keeps the
            // full spelling so codegen can give each a distinct mono symbol.
            if let Some(tok) = type_to_mono_mangle_token(ty) {
                mangle_frame.insert(name.clone(), tok);
            }
            // FULL `TypeExpr` channel (B-2026-08-31-39): the head name above
            // drops a nested-generic's element (`Vec[i64]` → `"Vec"`) and
            // records nothing at all for a nameless type argument, so a
            // monomorph body could not resolve `T` to the type it was actually
            // instantiated at.
            //
            // Two solutions are deliberately skipped. A bare type PARAM names
            // no type — it is the self-referential / propagated binding the
            // name channel already flattens through the caller's active
            // substitution, and recording it here would let `T -> T` shadow
            // that flattening. An `Error` round-trip is a LOST type, not a
            // resolved one, and lowers to `i64`, so an entry would be worse
            // than the absence that makes consumers fall back.
            if !matches!(ty, Type::TypeParam(_)) {
                let te = Self::type_to_type_expr(ty);
                if !matches!(te.kind, crate::ast::TypeKind::Error) {
                    te_frame.insert(name.clone(), te);
                }
            }
        }
        if !frame.is_empty() {
            self.call_type_subs.insert(SpanKey::from_span(span), frame);
        }
        if !mangle_frame.is_empty() {
            self.call_type_subs_mangle
                .insert(SpanKey::from_span(span), mangle_frame);
        }
        if !te_frame.is_empty() {
            self.call_type_subs_te
                .insert(SpanKey::from_span(span), te_frame);
        }
    }

    // B-2026-08-22-3 — the thin `check_call_args_with_substitution` wrapper
    // that used to live here is GONE, not merely unused. Its whole body was a
    // call to `_full` with `None` for `explicit_generic_args`,
    // `formal_generic_params` and `where_clause`, and that last `None` is the
    // bug it caused: the method-call path reached for the convenient wrapper
    // and thereby discharged no bound of any class — plain `T: Trait`,
    // projection bounds, const predicates and assoc-type equalities alike.
    // Deleting it rather than keeping it behind an `#[allow(dead_code)]` is
    // the point: with no wrapper to reach for, a future call site has to pass
    // the where clause (or write `None` deliberately, where it is visible in
    // the diff) and cannot silently re-open the hole.

    /// Type-check call arguments against `(params, return_type)` with the
    /// round-10.1 closure-pushdown logic, returning the (possibly-substituted)
    /// return type. Shared by `infer_call` and the user-defined-method branch
    /// of `infer_method_call` so generic methods get the same inference fix as
    /// generic free functions.
    ///
    /// Behavior:
    /// - Non-generic signature: each arg checked against its slot via
    ///   `check_expr` (already does closure pushdown for monomorphic `Fn(...)`).
    /// - Generic signature: two-pass — non-closure args inferred eagerly to
    ///   solve `T`s, then closure args checked against the substituted slot
    ///   via `check_expr` (so a closure's params see the solved `T`, not a
    ///   fresh var). The substitution is recorded under
    ///   `record_subs_for_span` for downstream consumers (interpreter,
    ///   codegen).
    ///
    /// `apply_call_site_marker` controls the `mut` marker check; pass `false`
    /// for method calls (per design.md, the call-site marker rule applies only
    /// to free-function calls).
    ///
    /// Additionally accepts explicit call-site generic args + the function's
    /// declaration-order generic-param names (const generics slice 1c)
    /// and the callee's where-clause for bound discharge (slice 3c).
    /// When `explicit_generic_args` and `formal_generic_params` are
    /// both supplied, each (formal_name, explicit_arg) pair pre-binds
    /// the corresponding metavar so subsequent arg-position
    /// unification flows from the explicit binding. After the
    /// inference solver runs, each `WhereConstraint::ConstPredicate`
    /// in `where_clause` is evaluated against the resolved const-args;
    /// `Bool(false)` triggers a `"const constraint violated"`
    /// diagnostic at `discharge_span`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_call_args_with_substitution_full(
        &mut self,
        args: &[CallArg],
        params: &[Type],
        return_type: &Type,
        record_subs_for_span: &Span,
        apply_call_site_marker: bool,
        explicit_generic_args: Option<&[GenericArg]>,
        formal_generic_params: Option<&[String]>,
        where_clause: Option<&WhereClause>,
        discharge_span: &Span,
    ) -> Type {
        // Const generics slice 3c: when the callee declares a
        // where-clause with `ConstPredicate`s, force the full
        // instantiate+unify+resolve+discharge path even if neither
        // params nor return reference a generic — the predicate may
        // reference const-params that don't appear in the signature's
        // types (`fn f[const N: i64]() where N >= 0`). Without this
        // override the early-return below skips discharge entirely.
        //
        // GAT slice 8a: the same override applies to
        // `ProjectionBound` predicates. A function with no generic
        // params/return but a `where F.Mapped[i64]: Trait` clause
        // (where F is a type-param) needs the full discharge path so
        // `discharge_projection_bounds` runs against the call's
        // explicit type-args.
        let has_where_const_predicate = where_clause
            .map(|wc| {
                wc.constraints.iter().any(|c| {
                    matches!(
                        c,
                        WhereConstraint::ConstPredicate { .. }
                            | WhereConstraint::ProjectionBound { .. }
                    )
                })
            })
            .unwrap_or(false);
        let has_generic = params.iter().any(contains_type_param)
            || contains_type_param(return_type)
            || has_where_const_predicate;
        if !has_generic {
            for (arg, param_ty) in args.iter().zip(params.iter()) {
                // Line 549 slice 2b — set the union-borrow context for
                // the duration of this arg's check_expr so a top-level
                // `u.field` access lands in `infer_field_access` with
                // the borrow-flavored diagnostic active. Saved/restored
                // around the arg so sibling args don't inherit, and
                // `infer_field_access` takes() on the first union access
                // so nested non-borrow reads still fire slice 2a.
                let saved_borrow_ctx = self.borrow_context;
                let saved_mut_through = self.mut_through_param_arg;
                self.borrow_context = borrow_context_for_param(param_ty);
                self.mut_through_param_arg = param_mutates_through(param_ty);
                let arg_ty = self.check_expr(&arg.value, param_ty);
                self.borrow_context = saved_borrow_ctx;
                self.mut_through_param_arg = saved_mut_through;
                // B-2026-08-26-37 — a call ARGUMENT is a value position exactly
                // as a `let` initializer is, so the index-move rule applies here
                // too. Gated on a BY-VALUE parameter: handing `v[i]` to a
                // `ref T` / `mut ref T` / `Slice[T]` parameter is a borrow, and
                // a borrow is the remedy the rule points at, not the offence.
                if !matches!(
                    param_ty,
                    Type::Ref(_) | Type::MutRef(_) | Type::Slice { .. }
                ) {
                    self.reject_index_move_non_copy(&arg.value, param_ty);
                }
                // B-2026-09-03-20 — the same value position for the own-`Drop`
                // partial-move rule. The borrow gate above is now inside that
                // rule's predicate, so this call needs no gate of its own.
                self.warn_partial_move_of_drop_struct(&arg.value, param_ty);
                // B-2026-09-04-15 — the RC-owner spelling of the same shape,
                // and it inherits that rule's borrow gate identically.
                self.reject_shared_field_move(&arg.value, param_ty);
                if apply_call_site_marker {
                    self.check_call_site_marker(arg, param_ty, &arg_ty);
                }
            }
            return return_type.clone();
        }
        // Generic case: types-first / effects-second per design.md
        // § Monomorphization order for compound polymorphism. Item 131
        // sub-step 2b — replaces the per-call ad-hoc `solve_type_params`
        // with fresh-metavariable instantiation: each `TypeParam(T)` in
        // the callee's signature becomes a fresh `TypeVar(?M_n)` for
        // this call only, so cross-call collisions are impossible.
        // Pass 1 infers non-closure args and unifies them against the
        // instantiated slot types; pass 2 checks each arg (including
        // closures) against the resolved slot, with check_expr's
        // pushdown seeing concrete (i.e. solved) slot types when
        // available.
        let InstantiatedSignature {
            params: sub_params,
            return_type: sub_ret,
            name_to_id,
            id_to_name,
            name_to_const_id,
            const_id_to_name,
        } = instantiate_signature_with_fresh_vars(
            params,
            return_type,
            &mut self.env.next_type_var,
            &mut self.env.next_const_var,
        );

        // B-2026-08-05-19 — seed the metavars from the EXPECTED return type
        // BEFORE arguments are solved. Unifying the instantiated return
        // (`Box[?T]`) against the expectation (`Box[i32]`) binds `?T = i32`, so
        // the literal argument is then CHECKED against `i32` instead of minting
        // `i64` and forcing `Box[i64]`. The struct-literal path already does the
        // equivalent (`infer_struct_literal_expected`'s `seeded` arm,
        // B-2026-07-18-17); this is its call-side twin.
        //
        // Runs before the explicit-generic-args block below so an EXPLICIT
        // turbofish-equivalent still wins — it binds the same ids afterwards.
        // Only for a genuinely generic signature, and `unify_types` is
        // permissive on mismatch (it simply fails to bind), so an expectation
        // that does not match the return shape leaves inference exactly as it
        // was rather than forcing a wrong binding.
        //
        // B-2026-08-08-7 — recorded so pass 1 below can CHECK (rather than
        // infer) an argument whose slot this seeding just made concrete. See
        // the comment there for why the two halves have to move together.
        let mut seeded_from_expectation = false;
        if let Some(expected_ret) = self.pending_expected_call_return.take() {
            // Gate on the instantiated RETURN carrying an unsolved metavar, not
            // on `formal_generic_params`. An `impl[T] Box[T] { fn new(..) }`
            // declares no generics ON THE FN — `T` belongs to the impl — so the
            // formals list is `None` there while `sub_ret` is `Box[?0]`, which
            // is precisely the case this seeding exists for. Measured: gating on
            // the formals list left `Box.new(5)` unseeded.
            if contains_type_var(&sub_ret) {
                unify_types(
                    &sub_ret,
                    &expected_ret,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
                seeded_from_expectation = true;
            }
        }

        // Const generics slice 1c: pre-bind metavars from explicit
        // call-site generic args. Walk the formal-param names and the
        // user-supplied args in lockstep; each `GenericArg::Const`
        // literal binds the corresponding `ConstVar`, each
        // `GenericArg::Type` binds the corresponding `TypeVar`. The
        // subsequent arg-position unification flow runs against these
        // pre-bindings (so a mismatch between explicit and inferred
        // const-args surfaces at the per-position unify call).
        if let (Some(explicit), Some(formal_names)) = (explicit_generic_args, formal_generic_params)
        {
            for (formal_name, explicit_arg) in formal_names.iter().zip(explicit.iter()) {
                if let Some(&const_id) = name_to_const_id.get(formal_name) {
                    if let GenericArg::Const(expr) = explicit_arg {
                        if let Some(cv) = const_value_from_literal(expr) {
                            self.env
                                .const_substitutions
                                .insert(const_id, ConstArg::Literal(cv));
                        }
                    }
                } else if let Some(&type_id) = name_to_id.get(formal_name) {
                    match explicit_arg {
                        GenericArg::Type(te) => {
                            let ty = self.lower_type_expr(te, &[]);
                            self.env.substitutions.insert(type_id, ty);
                        }
                        // Phase 11 Q1: an explicit shape literal binds a
                        // shape-variadic param's metavar to the whole
                        // lowered `Type::Shape`.
                        GenericArg::Shape(lit) => {
                            let ty = self.lower_shape_literal(lit, &[]);
                            self.env.substitutions.insert(type_id, ty);
                        }
                        GenericArg::Const(_) => {}
                    }
                }
            }
        }

        let mut arg_tys: Vec<Option<Type>> = Vec::with_capacity(args.len());
        for (idx, (arg, formal_param_ty)) in args.iter().zip(params.iter()).enumerate() {
            if matches!(arg.value.kind, ExprKind::Closure { .. }) {
                arg_tys.push(None);
            } else {
                // Line 549 slice 2b — the `Type::Ref(_)` / `Type::MutRef(_)`
                // wrapper is visible on the *formal* param type before
                // metavar instantiation, so the borrow context can be
                // decided here without waiting for the pass-2 resolution.
                // This is what makes a generic `fn foo[T](x: ref T)`
                // called with a union-field arg fire slice 2b in pass 1.
                let saved_borrow_ctx = self.borrow_context;
                let saved_mut_through = self.mut_through_param_arg;
                self.borrow_context = borrow_context_for_param(formal_param_ty);
                self.mut_through_param_arg = param_mutates_through(formal_param_ty);
                // B-2026-08-08-7 — when the expected-return seeding above
                // already bound this slot to something fully concrete, CHECK
                // the argument against it instead of inferring it. Inferring
                // lets a context-adopting argument mint its default first —
                // `let h: Holder[u32] = wrap([30, 10, 20])` seeded `?T = u32`,
                // then pass 1 inferred the array literal as `Vec[i64]` anyway,
                // and pass 2 rejected it. The non-generic arm above has always
                // used `check_expr` for exactly this reason; this makes the
                // seeded generic arm agree with it. The literal spelling
                // (`[30u32, …]`) always worked, which is the tell that this is
                // adoption and not a real type disagreement.
                //
                // THREE gates, each paid for by a measured regression:
                //
                //  1. The seeding fired. Otherwise there is no expectation and
                //     nothing to check against.
                //  2. The resolved slot is free of metavars/type params. A
                //     partially-solved slot would have `check_expr` report a
                //     mismatch pass 1 is not entitled to judge — arguments
                //     still to come may bind it.
                //  3. The argument is a COLLECTION LITERAL. `check_expr` is not
                //     just a narrower `infer_expr`: it also runs
                //     `check_int_widening_coercion`, which is stricter than the
                //     `check_assignable` pass 2 applies. Running it on an
                //     arbitrary argument broke `return Some(i)` with `i: i64`
                //     into an `Option[u64]` — the exact shape the seeding's own
                //     comment promises is unaffected, "because seeding binds the
                //     RETURN type and the payload is then an ordinary scalar
                //     assignment where numeric coercion stays permissive". A
                //     collection literal has no such coercion to preserve: its
                //     element type is minted by literal defaulting and has no
                //     other source, which is the whole reason it needs the
                //     context. (`Column.from_vec(Vec.new())` — a constructor
                //     argument whose element is an unsolved var rather than a
                //     default — is left alone for now; widening gate 3 to reach
                //     it is a separate question with its own regression risk.)
                let arg_adopts_from_context = matches!(
                    &arg.value.kind,
                    ExprKind::ArrayLiteral(_)
                        | ExprKind::PrefixCollectionLiteral { .. }
                        | ExprKind::RepeatLiteral { .. }
                );
                let seeded_slot = if seeded_from_expectation && arg_adopts_from_context {
                    let resolved = resolve_type_vars(
                        &sub_params[idx],
                        &self.env.substitutions,
                        &id_to_name,
                        &self.env.const_substitutions,
                        &const_id_to_name,
                    );
                    let resolved = self.resolve_assoc_projections(&resolved);
                    expectation_is_concrete(&resolved).then_some(resolved)
                } else {
                    None
                };
                let inferred = match &seeded_slot {
                    Some(slot) => self.check_expr(&arg.value, slot),
                    None => self.infer_expr(&arg.value),
                };
                self.borrow_context = saved_borrow_ctx;
                self.mut_through_param_arg = saved_mut_through;
                arg_tys.push(Some(inferred));
            }
        }
        // Pass 1: unify non-closure arg types into the instantiated
        // slot types so the metavars get bound from arguments. Failure
        // is silent here — pass 2's `check_assignable` produces the
        // user-facing diagnostic, and unify already records partial
        // structural matches.
        for (sub_param_ty, arg_ty_opt) in sub_params.iter().zip(arg_tys.iter()) {
            if let Some(arg_ty) = arg_ty_opt {
                unify_types(
                    sub_param_ty,
                    arg_ty,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
            }
        }
        // Pass 2: check each arg against the resolved slot. For
        // closure args, the resolved slot may be a concrete
        // `Fn(i64) -> i64` (when T solved) and check_expr's pushdown
        // gives the closure params their types.
        for ((arg, sub_param_ty), arg_ty_opt) in
            args.iter().zip(sub_params.iter()).zip(arg_tys.iter())
        {
            let resolved = resolve_type_vars(
                sub_param_ty,
                &self.env.substitutions,
                &id_to_name,
                &self.env.const_substitutions,
                &const_id_to_name,
            );
            let resolved = self.resolve_assoc_projections(&resolved);
            match arg_ty_opt {
                Some(arg_ty) => {
                    // B-2026-08-08-7 — pass 2 still judges an argument that
                    // pass 1 checked. Skipping it here (the first cut did)
                    // silently LOST a rejection: `let c: Column[u32] =
                    // Column.from_vec(["a", "b"])` errored before the change and
                    // passed after it, because check-mode's collection-literal
                    // adoption stamps the expected type on the literal without
                    // validating the elements — a pre-existing hole that
                    // `check_assignable` was the thing catching. Pass 1's
                    // `check_expr` narrows the argument's type; it does not
                    // replace the verdict.
                    //
                    // B-2026-08-08-8 — resolve the ARGUMENT's type through the
                    // substitutions too, not just the slot's. `arg_ty` is the
                    // snapshot pass 1 took at inference time; pass 1's own
                    // `unify_types` may have bound its metavars afterwards, and
                    // comparing a stale snapshot against a freshly-resolved slot
                    // reported `expected 'Vec<u32>', found 'Vec<?T1>'` for
                    // `let c: Column[u32] = Column.from_vec(Vec.new())` — where
                    // `?T1` was, by then, bound to `u32` in that very map.
                    // Resolving cannot mask a real mismatch: a metavar in
                    // `arg_ty` is only ever bound by unifying against this
                    // signature, so resolving reproduces what unify accepted and
                    // leaves everything unify rejected (a shape or name clash
                    // binds nothing) exactly as it was.
                    let arg_ty = &self.resolve_assoc_projections(&resolve_type_vars(
                        arg_ty,
                        &self.env.substitutions,
                        &id_to_name,
                        &self.env.const_substitutions,
                        &const_id_to_name,
                    ));
                    self.check_assignable(&resolved, arg_ty, arg.value.span);
                    // B-2026-08-08-9 — a slot the EXPECTATION bound still owes
                    // the narrowing check. `check_assignable` is permissive
                    // between integer types, which is right when the slot was
                    // fixed by this very argument (there is nothing to narrow
                    // TO), but seeding fixes the slot from the OUTSIDE, so a
                    // wide variable then flows into a narrow slot unchallenged:
                    // `fn id[T](x: T) -> T` called as `let x: u8 = id(big)`
                    // with `big: i64 = 5000000000` typechecked and PRINTED
                    // 5000000000 — a value no `u8` can hold. The same spelling
                    // without the generic (`takeu8(big)`, or the plain
                    // `let x: u8 = big`) is rejected by B-2026-07-09-7, so the
                    // generic call was a hole in an existing rule rather than a
                    // separate policy. Literals are exempt inside the helper
                    // (they are value-checked, which is what keeps the
                    // `Box.new(5)` case seeding exists for working), so this
                    // only ever fires on a non-literal integer.
                    if seeded_from_expectation
                        && self.seeded_arg_narrows(&arg.value, &resolved, arg_ty)
                    {
                        self.check_int_widening_coercion(&arg.value, &resolved, arg_ty);
                    }
                    // B-2026-09-01-12 — the FLOAT sibling of the line above,
                    // which had no counterpart at all: `seeded_arg_narrows` is
                    // integer-only (it reasons in `int_signed_width`), so a
                    // float flowing into a slot the EXPECTATION fixed reached
                    // no gate. `Option[f32] = Option.Some(0.1f64)` therefore
                    // typechecked and then printed the f64 under `--interp` and
                    // the rounded f32 on every compiled backend — a run==build
                    // split on a program the plain spelling (`let d: f32 =
                    // 0.1f64`) already rejects. No `seeded_arg_narrows`
                    // pre-filter is needed here: the gate answers "nothing to
                    // do" itself for a non-float, an equal width, a widening,
                    // and for a constant that names no width of its own.
                    if seeded_from_expectation {
                        self.check_float_narrowing_coercion(&arg.value, &resolved, arg_ty);
                    }
                    // B-2026-09-01-19 — the contextual RANGE check for a
                    // suffixed integer literal, which this route also never
                    // ran. `check_int_widening_coercion` above exempts
                    // suffixed literals, and says why: they are "already
                    // range-checked against the contextual type at the top of
                    // `check_expr`". True of that route; false of this one,
                    // whose whole point is that the slot was fixed from the
                    // outside and the argument is checked here instead. So
                    // `Option[u8] = Option.Some(300i64)` reached the payload
                    // with nothing consulting `u8` at all: `Some(300)` under
                    // `--interp` against `Some(44)` on every compiled backend,
                    // and `Option.Some(-1i64)` `Some(-1)` against `Some(255)`
                    // — a sign change — while the bare `Option.Some(300)` and
                    // the plain `let a: u8 = 300i64` were both already
                    // rejected. This is the same check `check_expr` runs, on
                    // the route that does not reach it; like that one it emits
                    // ONLY when the value does not fit, so an in-range
                    // coercion (`Option.Some(5i64)` into `Option[u8]`) is left
                    // alone and the `Box.new(5)` shapes seeding exists for
                    // keep working.
                    if seeded_from_expectation {
                        if let Some((value, sfx)) = Self::suffixed_int_literal_value(&arg.value) {
                            let ctx = match &resolved {
                                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                                other => other,
                            };
                            self.check_int_literal_fits(value, ctx, &arg.value.span, sfx);
                        }
                    }
                    if apply_call_site_marker {
                        self.check_call_site_marker(arg, &resolved, arg_ty);
                    }
                }
                None => {
                    // Line 549 slice 2b — see the non-generic arm above
                    // for the contract. Closure args (the only branch
                    // that reaches this re-check, since pass 1 inferred
                    // non-closure args already) won't trip a union
                    // field read at their top level, but the context is
                    // set defensively so any synthesised cell-rewrap
                    // path that lowers into a non-closure here still
                    // routes through slice 2b correctly.
                    let saved_borrow_ctx = self.borrow_context;
                    let saved_mut_through = self.mut_through_param_arg;
                    self.borrow_context = borrow_context_for_param(&resolved);
                    self.mut_through_param_arg = param_mutates_through(&resolved);
                    let arg_ty = self.check_expr(&arg.value, &resolved);
                    self.borrow_context = saved_borrow_ctx;
                    self.mut_through_param_arg = saved_mut_through;
                    // B-2026-07-11-4: a type param that appears ONLY inside a
                    // closure param's type — e.g. `spawn[T](f: OnceFn() -> T)`,
                    // where T is fixed solely by the thunk's return — is still
                    // unsolved here: pass 1 skips closure args, so nothing bound
                    // the metavar. Unify the closure's now-inferred type back
                    // into the instantiated slot so the metavar binds from the
                    // closure body (the `Fn`→`OnceFn` cross arm in `unify_types`
                    // descends into the return type). Mirrors pass 1's
                    // non-closure unify; a no-op when the slot was already
                    // solved from another argument.
                    unify_types(
                        sub_param_ty,
                        &arg_ty,
                        &mut self.env.substitutions,
                        &mut self.env.const_substitutions,
                    );
                    if apply_call_site_marker {
                        self.check_call_site_marker(arg, &resolved, &arg_ty);
                    }
                }
            }
        }
        // Translate solved metavars back to the original `T → ConcreteType`
        // shape `record_call_type_subs` expects — this is what the
        // interpreter's runtime dispatch consumes for generic-method
        // resolution. Only entries that resolved to something other
        // than the originating TypeParam are recorded; unsolved ones
        // are skipped so the interpreter's resolution stack doesn't
        // see a self-referential `T → T` binding.
        let mut solutions: HashMap<String, Type> = HashMap::new();
        for (name, &id) in &name_to_id {
            let resolved = resolve_type_vars(
                &Type::TypeVar(id),
                &self.env.substitutions,
                &id_to_name,
                &self.env.const_substitutions,
                &const_id_to_name,
            );
            // B-2026-09-03-2 — the skip above used to be
            // `!matches!(&resolved, Type::TypeParam(n) if n == name)`, and that
            // NAME comparison cannot tell two different things apart:
            //
            //   * an UNSOLVED metavar, which `resolve_type_vars` hands back as
            //     `TypeParam(originating_name)` — the self-referential binding
            //     this skip exists to keep out of the resolution stack; and
            //   * a metavar genuinely SOLVED to the CALLER's own type param,
            //     when the caller happens to spell it with the same letter.
            //
            // One generic body calling another is the second case and is
            // extremely common — `fn fwd[T](x: Option[T]) { sink(x) }`, where
            // both functions call their parameter `T`. Dropping it meant the
            // inner call recorded NO frame at all, in any of the three
            // channels, so the inner monomorph resolved `T` to nothing and
            // lowered the payload at the erased one-word width. For a boxed
            // payload that word is the box POINTER: measured printing
            // `s:94482719980304`, a fresh number every run, on all three
            // compiled surfaces where `--interp` printed `[uv, wx]`. Renaming
            // the outer param to `U` made the same program correct on all four
            // — the whole defect was the name collision.
            //
            // Ask the SUBSTITUTION instead of the name. Follow the metavar
            // chain: it ends either at something bound (a real solution, record
            // it) or at an unsolved metavar (record it only if that metavar is
            // not this very param). That is the question the old test was
            // trying to ask, and it is immune to two scopes reusing a letter.
            let mut cur = id;
            let terminal_unsolved = loop {
                match self.env.substitutions.get(&cur) {
                    Some(Type::TypeVar(next)) => cur = *next,
                    Some(_) => break None,
                    None => break Some(cur),
                }
            };
            let self_referential = terminal_unsolved
                .and_then(|tid| id_to_name.get(&tid))
                .is_some_and(|n| n == name);
            if !self_referential {
                solutions.insert(name.clone(), resolved);
            }
        }
        self.record_call_type_subs(record_subs_for_span, &solutions);

        // Resolve the return type. Unsolved metavars come back as
        // `TypeParam(originating_name)` so the caller's
        // `find_unbound_type_param` (slice 2a) still surfaces the
        // unsolved-T diagnostic.
        let ret = resolve_type_vars(
            &sub_ret,
            &self.env.substitutions,
            &id_to_name,
            &self.env.const_substitutions,
            &const_id_to_name,
        );
        // GAT slice 8c — apply `substitute_type_params` against the
        // `solutions` map before `resolve_assoc_projections`.
        // `resolve_type_vars` walks `TypeVar` ids but doesn't touch
        // `AssocProjection.param` (which is a `String` carrying the
        // receiver's type-param name like `"F"`). Without this extra
        // pass, a return type `F.Mapped[i64]` keeps `param="F"`
        // after the TypeVar resolution, and the subsequent
        // `resolve_assoc_projections` lookup against `impl_assoc_types`
        // (keyed on concrete type names like `"V"`) misses — leaving
        // the call's return type as an unresolved projection at the
        // assignment site. `substitute_type_params` is the same
        // helper `discharge_projection_bounds` uses for the explicit-
        // where-clause projection path; routing the call's return
        // type through it keeps the projection resolution surface
        // consistent.
        //
        // GATE (B-2026-07-12-6): run this ONLY when `ret` actually
        // carries an `AssocProjection`. `ret` is already fully resolved
        // by `resolve_type_vars` above; re-running `substitute_type_params`
        // over a projection-free type re-substitutes bare `TypeParam`
        // nodes that resolution already handled. When a solution value
        // re-introduces the same param name — e.g. a generic method
        // `impl[T] Box[T]` calling `Some(self.items.pop())`, where the
        // constructor's own generic param and the enclosing method's are
        // BOTH literally `"T"`, so `solutions = {"T": Option[T]}` and the
        // already-resolved `ret = Option[Option[T]]` — the second pass
        // rewrites the inner `T` again, nesting a spurious extra layer
        // (`Option[Option[Option[T]]]`). The projection-param rewrite is
        // the only thing `resolve_type_vars` can't do, so gate on it.
        let ret = if solutions.is_empty() || !type_contains_assoc_projection(&ret) {
            ret
        } else {
            let solutions_as_subs: HashMap<String, SubstValue> = solutions
                .iter()
                .map(|(k, v)| (k.clone(), SubstValue::Type(v.clone())))
                .collect();
            substitute_type_params(&ret, &solutions_as_subs)
        };
        let ret = self.resolve_assoc_projections(&ret);

        // Const generics slice 3c: discharge `WhereConstraint::ConstPredicate`
        // entries against the resolved const-args. The substitution
        // map is built from two sources: inferred const-args (via
        // `name_to_const_id` + `env.const_substitutions` resolved
        // through `resolve_const_arg`), and explicit call-site args
        // (when supplied — formal-param names paired with
        // `explicit_generic_args` positions). Explicit args win on
        // collision (the user-supplied value pins the predicate
        // discharge directly without needing the inference solver to
        // have minted a ConstVar for the param). Slice 2's
        // `eval_const_expr` consumes the substituted predicate.
        if let Some(wc) = where_clause {
            let mut const_arg_subst: HashMap<String, i64> = HashMap::new();
            for (name, &id) in &name_to_const_id {
                let resolved = resolve_const_arg(
                    &ConstArg::ConstVar(id),
                    &self.env.const_substitutions,
                    &const_id_to_name,
                );
                if let ConstArg::Literal(n) = resolved {
                    const_arg_subst.insert(name.clone(), n);
                }
            }
            if let (Some(explicit), Some(formal_names)) =
                (explicit_generic_args, formal_generic_params)
            {
                for (formal_name, explicit_arg) in formal_names.iter().zip(explicit.iter()) {
                    if let GenericArg::Const(e) = explicit_arg {
                        if let Some(v) = const_value_from_literal(e) {
                            const_arg_subst.insert(formal_name.clone(), v);
                        }
                    }
                }
            }
            self.discharge_const_predicates(wc, &const_arg_subst, discharge_span);
            // Trait-bounds-at-codegen enforcement (slice 0.a, sub-step 1
            // of monomorphized collections prereq). Walks
            // `WhereConstraint::TypeBound` predicates in the same where-
            // clause and verifies each formal type-param's concrete
            // binding satisfies its declared bounds. Inline param bounds
            // (`fn f[T: Hash + Eq](...)`) were normalized into the
            // where-clause at FunctionSig construction
            // (`normalize_bounds_into_where_clause`) so this single
            // discharge call covers both inline and where-clause surfaces.
            let all_param_names: Vec<String> = name_to_id.keys().cloned().collect();
            self.discharge_type_bounds(wc, &solutions, &all_param_names, discharge_span);
        }

        // GAT slice 8c — implicit-trigger walker for
        // `discharge_gat_decl_constraints`. Scan the substituted
        // signature's param + return types for `AssocProjection`
        // nodes and discharge each one's GAT-decl per-param inline
        // bounds + where-clause. This is the sibling trigger to the
        // explicit `where F.Mapped[i64]: Trait` discharge inside
        // `discharge_projection_bounds` — slice 8b shipped that
        // explicit trigger but a function like
        // `fn f[F: Functor](x: F.Mapped[NoShow])` (with `type
        // Mapped[U: Show]`) never reaches the where-clause discharge,
        // so the inline bound on `U` was silently skipped. The
        // walker fires on the **substituted-but-not-yet-resolved**
        // projection (receiver string rewritten via
        // `substitute_type_params`, type-args resolved through
        // `resolve_type_vars`, but the impl-table lookup deferred).
        // This is the shape `discharge_gat_decl_constraints` expects
        // — its impl-table lookup is what proves the GAT-decl entry
        // exists and exposes the `param_bound_traits` /
        // `where_clause` fields to discharge. Calling
        // `resolve_assoc_projections` first would erase the
        // projection (replacing it with the substituted RHS), losing
        // the discharge opportunity entirely.
        let solutions_as_subs: HashMap<String, SubstValue> = solutions
            .iter()
            .map(|(k, v)| (k.clone(), SubstValue::Type(v.clone())))
            .collect();
        for sub_param_ty in &sub_params {
            let resolved = resolve_type_vars(
                sub_param_ty,
                &self.env.substitutions,
                &id_to_name,
                &self.env.const_substitutions,
                &const_id_to_name,
            );
            let substituted = if solutions_as_subs.is_empty() {
                resolved
            } else {
                substitute_type_params(&resolved, &solutions_as_subs)
            };
            self.discharge_gat_decl_constraints_in(&substituted, discharge_span);
        }
        // For the return type, fire the walker against the
        // substituted-but-not-yet-resolved shape so projections that
        // survive substitution can discharge their GAT-decl
        // constraints. `ret` above is the fully-resolved value (used
        // as the call's return). Rebuild the pre-resolution shape
        // for the walker so projections that get erased by
        // resolution still get their GAT-decl constraints checked.
        let pre_resolve_ret = resolve_type_vars(
            &sub_ret,
            &self.env.substitutions,
            &id_to_name,
            &self.env.const_substitutions,
            &const_id_to_name,
        );
        let pre_resolve_ret = if solutions_as_subs.is_empty() {
            pre_resolve_ret
        } else {
            substitute_type_params(&pre_resolve_ret, &solutions_as_subs)
        };
        self.discharge_gat_decl_constraints_in(&pre_resolve_ret, discharge_span);

        ret
    }

    /// Walk a where-clause and discharge each `TypeBound { T: Trait, ... }`
    /// predicate against the resolved type substitution. For each formal
    /// type-param T bound to a concrete type via `solutions`, check that
    /// the concrete type satisfies the trait. Emits a `TypeMismatch`
    /// diagnostic on miss.
    ///
    /// Built-in trait coverage (Hash / Eq / PartialEq / Ord / PartialOrd /
    /// Display on primitives, plus `#[derive(...)]` on named struct/enum
    /// types) flows through `type_satisfies_bound`, which consults the
    /// existing `type_supports_*` helpers before falling back to the
    /// `env.impls` table lookup.
    ///
    /// Slice 0.a, sub-step 1 of monomorphized collections prereq
    /// ([`phase-7-codegen.md`](../docs/implementation_checklist/phase-7-codegen.md)).
    /// Counterpart to `discharge_const_predicates` for ConstPredicate
    /// where-clauses (const generics slice 3c).
    fn discharge_type_bounds(
        &mut self,
        where_clause: &WhereClause,
        solutions: &HashMap<String, Type>,
        all_param_names: &[String],
        discharge_span: &Span,
    ) {
        // Solved fn type-params as a substitution map, for resolving a
        // parameterized bound's own args (`C: Reduce[T]` where `T` is another
        // solved param) before comparing them (B-2026-07-02-42).
        let solutions_subs: HashMap<String, SubstValue> = solutions
            .iter()
            .map(|(k, v)| (k.clone(), SubstValue::Type(v.clone())))
            .collect();
        for constraint in &where_clause.constraints {
            let WhereConstraint::TypeBound {
                type_name, bounds, ..
            } = constraint
            else {
                continue;
            };
            let Some(concrete_ty) = solutions.get(type_name) else {
                // Param unbound at this call site — the unsolved-T
                // diagnostic (slice 2a) handles this; don't double-report.
                continue;
            };
            if matches!(concrete_ty, Type::TypeVar(_) | Type::Error)
                || self.type_param_name(concrete_ty).is_some()
            {
                // Unresolved metavar / propagating-param / already-error —
                // upstream diagnostics handle. Avoid noise.
                //
                // `type_param_name` rather than a bare `TypeParam(_)` match:
                // the SAME parameter reaches here under two spellings, and a
                // bare match sees only one (B-2026-09-02-42). A signature
                // position lowers `T` to `TypeParam("T")`, while a function-
                // BODY annotation (`let w: T = v;`) deliberately leaves
                // `Named { name: "T", args: [] }` — type params are excluded
                // from `current_body_dim_scope` on purpose, the Named-vs-
                // TypeParam trap of B-2026-07-13-5 leg B — so a value whose
                // type flowed from an annotation arrived as `Named` and was
                // discharged against the impl table, where a type parameter
                // has no entry and so fails every bound. That reported a
                // still-generic `T` as not implementing its own declared
                // bound at a FREE generic function's call site (E0200), the
                // sibling of the method-resolution gate's E0236.
                //
                // Scoped by `enclosing_bounds`, so a `Named` naming a real
                // nominal type is untouched and still has its bounds checked.
                continue;
            }
            for bound in bounds {
                let Some(trait_name) = bound.path.last() else {
                    continue;
                };
                if !self.type_satisfies_bound(concrete_ty, trait_name) {
                    let message = self.render_unsatisfied_bound_message(
                        type_name,
                        trait_name,
                        concrete_ty,
                        bound,
                    );
                    self.type_error(message, *discharge_span, TypeErrorKind::TypeMismatch);
                    continue;
                }
                // B-2026-07-02-42: a PARAMETERIZED bound (`C: Reduce[i64]`) must
                // match the impl's trait ARGS. The name check above only proves
                // `Column` implements `Reduce`; `Column[f64]` implements
                // `Reduce[f64]`, NOT `Reduce[i64]`, so the mismatched arg must be
                // rejected (else `run` silently mis-types and `build` dies at LLVM
                // verification). Only fires when BOTH the impl's args and the
                // bound's requested args are fully concrete — an unsolved / still-
                // parametric arg on either side is left to the normal resolution.
                if let Some(bound_arg_asts) = &bound.generic_args {
                    if let Some(impl_args) =
                        self.env.impl_concrete_trait_args(concrete_ty, trait_name)
                    {
                        // Lower each requested arg with ALL the fn's type params
                        // in scope so a param-valued arg (`Reduce[T]`) becomes a
                        // `TypeParam` (not the bare `Named{"T"}` trap), then
                        // substitute the solved params. An arg that stays a
                        // `TypeParam` afterwards is an UNSOLVED param (e.g. `T`
                        // couldn't be pinned from a type-erased `Column`
                        // receiver) — skip the comparison so it isn't
                        // false-rejected.
                        let want: Vec<Type> = bound_arg_asts
                            .iter()
                            .filter_map(|a| match a {
                                crate::ast::GenericArg::Type(te) => {
                                    let t = self.lower_type_expr(te, all_param_names);
                                    Some(substitute_type_params(&t, &solutions_subs))
                                }
                                _ => None,
                            })
                            .collect();
                        let decidable = impl_args.len() == want.len()
                            && !impl_args.iter().chain(want.iter()).any(contains_type_param)
                            && impl_args
                                .iter()
                                .chain(want.iter())
                                .all(type_is_fully_concrete);
                        if decidable && impl_args != want {
                            let render = |v: &[Type]| {
                                v.iter().map(type_display).collect::<Vec<_>>().join(", ")
                            };
                            self.type_error(
                                format!(
                                    "trait bound `{}: {}[{}]` is not satisfied; `{}` implements \
                                     `{}[{}]`, not `{}[{}]`",
                                    type_name,
                                    trait_name,
                                    render(&want),
                                    type_display(concrete_ty),
                                    trait_name,
                                    render(&impl_args),
                                    trait_name,
                                    render(&want),
                                ),
                                *discharge_span,
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                }
            }
        }
        self.discharge_projection_bounds(where_clause, solutions, discharge_span);
        self.discharge_assoc_type_eq_bounds(
            where_clause,
            solutions,
            all_param_names,
            discharge_span,
        );
    }

    /// B-2026-08-22-3 — discharge `WhereConstraint::AssocTypeEq`
    /// (`where I.Item = i64`) at call sites.
    ///
    /// The declaration site (`bounds.rs`) only checks that the type
    /// PARAMETER exists and that the required type expression lowers. Nothing
    /// used to compare the call's solved `I` against the required associated
    /// type, so `fn take[I: Src](it: I) where I.Item = i64` accepted an `I`
    /// whose `Item` was `String`: the bound was accepted, documented, and
    /// silently unenforced, which is worse than a bound that is rejected —
    /// callers rely on it. design.md § Iterator and the stdlib method tables
    /// lean on this form (`fn extend[I: Iterator[Item = T]]`), so the
    /// constraint most likely to be written was the one not checked.
    ///
    /// The shape is `discharge_projection_bounds`'s, deliberately: build the
    /// projection the constraint names, substitute the call's solutions into
    /// it (which rewrites the receiver from a type-param name to the concrete
    /// type's bare name — see `substitute_type_params`'s `AssocProjection`
    /// arm), resolve it through `impl_assoc_types`, and compare. Only the last
    /// step differs — an equality constraint compares TYPES where a projection
    /// bound asks `type_satisfies_bound`.
    ///
    /// Anything not fully resolvable is skipped rather than reported, matching
    /// the sibling's "discharge only when fully resolvable" rule: an unsolved
    /// receiver is already covered by the unsolved-`T` diagnostic, and an
    /// impl-table miss means the `I: Src` bound itself failed and has its own
    /// error. Reporting here too would only cascade.
    fn discharge_assoc_type_eq_bounds(
        &mut self,
        where_clause: &WhereClause,
        solutions: &HashMap<String, Type>,
        all_param_names: &[String],
        discharge_span: &Span,
    ) {
        let subs: HashMap<String, SubstValue> = solutions
            .iter()
            .map(|(k, v)| (k.clone(), SubstValue::Type(v.clone())))
            .collect();
        for constraint in &where_clause.constraints {
            let WhereConstraint::AssocTypeEq {
                type_name,
                assoc_name,
                ty,
                ..
            } = constraint
            else {
                continue;
            };
            if !solutions.contains_key(type_name) {
                continue;
            }
            let projection = Type::AssocProjection {
                param: type_name.clone(),
                assoc: assoc_name.clone(),
                args: Vec::new(),
                receiver_args: Vec::new(),
            };
            let found = self.resolve_assoc_projections(&substitute_type_params(&projection, &subs));
            if !assoc_eq_bound_is_comparable(&found) {
                continue;
            }
            // Lower the REQUIRED type against the function's full type-param
            // list, not just the solved ones: a partial scope would make a
            // reference to an as-yet-unsolved sibling param read as an
            // undefined type and emit a spurious diagnostic here, on a line
            // the user did not write.
            let required = self.lower_type_expr(ty, all_param_names);
            let required =
                self.resolve_assoc_projections(&substitute_type_params(&required, &subs));
            if !assoc_eq_bound_is_comparable(&required) {
                continue;
            }
            if types_compatible(&found, &required) {
                continue;
            }
            self.type_error(
                format!(
                    "error[E_WHERE_CLAUSE_ASSOC_TYPE_EQ_NOT_SATISFIED]: associated-type \
                     bound `{}.{} = {}` is not satisfied; `{}` has `{} = {}`",
                    type_name,
                    assoc_name,
                    type_display(&required),
                    type_display(&solutions[type_name]),
                    assoc_name,
                    type_display(&found),
                ),
                *discharge_span,
                TypeErrorKind::TypeMismatch,
            );
        }
    }

    /// Render the message for an unsatisfied `type_name: trait_name`
    /// bound. Slice 6 of item 36 — consults the failing trait's
    /// `#[diagnostic::on_unimplemented(...)]` payload (if any) and
    /// substitutes `{Self}` against the concrete failing type plus
    /// `{T0}` / `{T1}` / ... against the bound's generic args; the
    /// result replaces the default phrasing entirely when `message` is
    /// present, with `label` and `note` appended as ` ; label: ...` /
    /// ` ; note: ...` clauses. Absent fields fall back to the default
    /// phrasing for that clause; an entirely absent payload reproduces
    /// the pre-slice-6 message verbatim.
    pub(super) fn render_unsatisfied_bound_message(
        &self,
        type_name: &str,
        trait_name: &str,
        concrete_ty: &Type,
        bound: &crate::ast::TraitBound,
    ) -> String {
        let default = format!(
            "trait bound `{}: {}` is not satisfied; `{}` does not implement `{}`",
            type_name,
            trait_name,
            type_display(concrete_ty),
            trait_name
        );
        let self_render = type_display(concrete_ty);
        let generic_arg_renders: Vec<Option<String>> = bound
            .generic_args
            .as_ref()
            .map(|args| {
                args.iter()
                    .map(|a| match a {
                        // Render the AST form rather than lowering +
                        // re-substituting — for simple traits like
                        // `T: Into[String]` this faithfully shows the
                        // user what `{T0}` resolves to; for traits
                        // whose generic args are themselves unsolved
                        // type-params, the source form (e.g. `U`)
                        // remains a useful readable token.
                        crate::ast::GenericArg::Type(ty) => {
                            Some(crate::parser::render_type_for_diagnostic(ty))
                        }
                        // Const args have no concise rendering and
                        // aren't part of the documented placeholder
                        // surface — leave the slot unsubstituted.
                        crate::ast::GenericArg::Const(_) => None,
                        // Shape args likewise — no placeholder rendering.
                        crate::ast::GenericArg::Shape(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let payload = self
            .env
            .traits
            .get(trait_name)
            .and_then(|t| t.on_unimplemented.as_ref());
        let headline = payload
            .and_then(|p| p.message.as_ref())
            .map(|m| {
                crate::diagnostic_attrs_lint::substitute_placeholders(
                    m,
                    &self_render,
                    &generic_arg_renders,
                )
            })
            .unwrap_or(default);
        let mut out = headline;
        if let Some(p) = payload {
            if let Some(label) = &p.label {
                out.push_str("; label: ");
                out.push_str(&crate::diagnostic_attrs_lint::substitute_placeholders(
                    label,
                    &self_render,
                    &generic_arg_renders,
                ));
            }
            if let Some(note) = &p.note {
                out.push_str("; note: ");
                out.push_str(&crate::diagnostic_attrs_lint::substitute_placeholders(
                    note,
                    &self_render,
                    &generic_arg_renders,
                ));
            }
        }
        // B-2026-08-26-10 — the failing type must never appear in its own
        // "is implemented by" list. It could, and did:
        //
        //   trait bound `T: Hash` is not satisfied; `Item` does not implement
        //   `Hash`; trait `Hash` is implemented by: Item
        //
        // Two tables disagreeing inside one sentence. The VERDICT comes from
        // `type_satisfies_bound`, which for the derive-backed traits (`Hash`,
        // `Eq`, `PartialEq`, `PartialOrd`, `Display`, `Clone`, `Copy`, `Debug`,
        // `Default`, …) answers from `derived_traits` and never consults the
        // impl table at all; the CANDIDATE LIST is built from the impl table,
        // where the author's `impl Hash for Item` is sitting. So a reader who
        // wrote the impl is told in the same breath that they did not and that
        // they did — and the list, whose job is to point at types that WOULD
        // work, points back at the one just refused.
        //
        // Splitting on the failing type is the whole fix, and it is keyed on
        // PRESENCE rather than on a hard-coded trait list on purpose: a trait
        // whose bound consults the impl table normally can never put the failing
        // type in this list, so the branch cannot misfire on one. `Ord` is the
        // live proof — `type_supports_ord` accepts a user `impl Ord`, so an
        // `Ord` bound that fails is one with no impl, and this never triggers.
        let (self_impls, other_impls): (Vec<String>, Vec<String>) = self
            .impl_candidates_for_trait(trait_name)
            .into_iter()
            .partition(|c| c == &self_render || c.starts_with(&format!("{self_render}[")));
        if !self_impls.is_empty() {
            // Lead with the reconciliation. Without it the sentence still reads
            // as a contradiction — the headline is the standard "does not
            // implement" phrasing and cannot be reworded per-trait from here —
            // so the clause has to say WHICH question the headline answered.
            //
            // The wording stays neutral about what the derive replaces
            // ("generated implementation", not "your body") because `Eq` and
            // `Copy` are MARKER impls with no body at all, and naming one would
            // be wrong for exactly the traits most likely to hit this.
            out.push_str(&format!(
                "; that verdict is about the derive, not your impl — you have \
                 written `impl {trait_name} for {self_render}`, but this bound is \
                 checked against `#[derive({trait_name})]`, which a hand-written \
                 impl does not satisfy. Adding the derive satisfies it, and the \
                 generated field-by-field implementation is what will be used"
            ));
        }
        if !other_impls.is_empty() {
            out.push_str("; trait `");
            out.push_str(trait_name);
            out.push_str("` is implemented by: ");
            out.push_str(&other_impls.join(", "));
        }
        // Float primitives deliberately do NOT implement the total-order /
        // total-equality / hashing traits — IEEE-754 NaN breaks reflexivity
        // and antisymmetry (env_build.rs "Floats deliberately excluded"). The
        // PascalCase `F32`/`F64` wrapper types (design.md § "total-order float
        // types") provide a total order (NaN sorts last) and DO implement these.
        // Without this note the built-in-impl list above ("… implemented by:
        // F32, F64, …") actively misleads: an `f64` user reads `F64` and
        // assumes their primitive qualifies. That exact confusion produced the
        // B-2026-07-04-15 ledger misdiagnosis (a correct `T: Ord` rejection on
        // `Column[f64]` was mis-read as a container/monomorphization bug).
        if matches!(concrete_ty, Type::Float(_)) && matches!(trait_name, "Ord" | "Eq" | "Hash") {
            let disp = type_display(concrete_ty);
            let wrapper = match disp.as_str() {
                "f32" => "F32",
                "f16" => "F16",
                "bf16" => "Bf16",
                _ => "F64",
            };
            out.push_str(&format!(
                "; note: `{disp}` is not totally ordered (IEEE-754 NaN), so it does not \
                 implement `{trait_name}` — use the total-order wrapper `{wrapper}` \
                 (`{wrapper}.from(x)`) in `Ord`/`Eq`/`Hash` contexts, or drop the \
                 `{trait_name}` bound if you only need arithmetic"
            ));
        }
        out
    }

    /// Slice 6 follow-up — produce a stable, deterministic list of
    /// impl-target renderings for the failed trait at a bound-not-
    /// satisfied site. Skips impls flagged `#[diagnostic::do_not_recommend]`
    /// (the spec's headline use case for the flag), dedupes by rendered
    /// string (a specialized impl + a generic-on-name impl on the same
    /// target collapse into one entry), and sorts alphabetically so the
    /// note's order does not leak registration order into user-visible
    /// diagnostics (and so snapshot tests stay stable across compiler
    /// changes that reorder env construction). Empty when the trait
    /// has no env entries (built-in traits like `Eq` / `Ord` / `Hash`
    /// where the impls are implicit rather than materialised in
    /// `env.impls`) — the caller suppresses the note in that case.
    fn impl_candidates_for_trait(&self, trait_name: &str) -> Vec<String> {
        let Some(indices) = self.env.impls_by_trait.get(trait_name) else {
            return Vec::new();
        };
        let mut renders: Vec<String> = indices
            .iter()
            .filter_map(|idx| {
                let imp = &self.env.impls[*idx];
                if imp.do_not_recommend {
                    return None;
                }
                Some(if imp.target_args.is_empty() {
                    imp.target_type.clone()
                } else {
                    let args = imp
                        .target_args
                        .iter()
                        .map(type_display)
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}[{}]", imp.target_type, args)
                })
            })
            .collect();
        renders.sort();
        renders.dedup();
        renders
    }

    /// GAT slice 8a — discharge `WhereConstraint::ProjectionBound`
    /// predicates at call sites. For each `<receiver>.Assoc[args]: Trait`
    /// constraint, the resolver lowers the projection type-expression
    /// against the function's generic scope, then substitutes the
    /// call's resolved `solutions` map into the projection (filling in
    /// the receiver's `TypeParam` head and any `TypeParam` args), then
    /// resolves it via `resolve_assoc_projections`. The resolved type
    /// is checked against each bound via `type_satisfies_bound`. On a
    /// miss, emits `E_WHERE_CLAUSE_PROJECTION_BOUND_NOT_SATISFIED`.
    ///
    /// Receiver-unsolved (no entry in `solutions`) and post-substitution
    /// projections that remain unresolved (the projection's `param`
    /// stays an unmatched `TypeParam` or the impl table has no entry)
    /// are skipped silently — those cases fall out of slice 8a's
    /// "discharge only when fully resolvable" rule. Tightening to
    /// reject unresolvable projections lands with the slice 8b
    /// `types_compatible` work or the slice-8c constraint solver.
    fn discharge_projection_bounds(
        &mut self,
        where_clause: &WhereClause,
        solutions: &HashMap<String, Type>,
        discharge_span: &Span,
    ) {
        // Build a substitution map for the call's solutions. Wrapped in
        // `SubstValue::Type` to feed `substitute_type_params`.
        let subs: HashMap<String, SubstValue> = solutions
            .iter()
            .map(|(k, v)| (k.clone(), SubstValue::Type(v.clone())))
            .collect();
        for constraint in &where_clause.constraints {
            let WhereConstraint::ProjectionBound {
                projection, bounds, ..
            } = constraint
            else {
                continue;
            };
            // Lower the projection type-expression against the
            // function's generic scope. The scope is the union of every
            // formal type-param name that appears in `solutions` (the
            // call-site discharge already has these in hand). Lowering
            // produces a `Type::AssocProjection { param, args, .. }`
            // with `param` as the receiver's type-param name.
            let scope: Vec<String> = solutions.keys().cloned().collect();
            let lowered = self.lower_type_expr(projection, &scope);
            // Substitute the call's solutions in for the receiver +
            // any type-param args inside the projection's `args` list.
            let substituted = substitute_type_params(&lowered, &subs);
            // GAT slice 8b: discharge the GAT decl's per-param inline
            // bounds + where-clause for the substituted projection
            // BEFORE checking the where-clause bound — a mismatch on
            // an arg's inline bound is a more focused diagnostic than
            // a downstream "bound not satisfied" cascade.
            self.discharge_gat_decl_constraints(&substituted, discharge_span);
            // Resolve the projection through `impl_assoc_types`. If the
            // receiver is now a concrete type registered with the GAT,
            // this yields the binding RHS substituted with the call's
            // args (e.g., `F.Mapped[i64]` with `F=Vec` and binding
            // `type Mapped[U] = Vec[U]` → `Vec[i64]`).
            let resolved = self.resolve_assoc_projections(&substituted);
            // Skip if the projection didn't resolve (receiver still a
            // TypeParam, impl table miss, or any unresolved metavar
            // shape). The unsolved-T diagnostic + upstream errors
            // surface those.
            if matches!(
                resolved,
                Type::AssocProjection { .. } | Type::TypeParam(_) | Type::TypeVar(_) | Type::Error
            ) {
                continue;
            }
            for bound in bounds {
                let Some(trait_name) = bound.path.last() else {
                    continue;
                };
                if self.type_satisfies_bound(&resolved, trait_name) {
                    continue;
                }
                self.type_error(
                    format!(
                        "error[E_WHERE_CLAUSE_PROJECTION_BOUND_NOT_SATISFIED]: \
                         projection bound `{}: {}` is not satisfied; \
                         resolved projection type `{}` does not implement `{}`",
                        type_display(&substituted),
                        trait_name,
                        type_display(&resolved),
                        trait_name
                    ),
                    *discharge_span,
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
    }

    /// GAT slice 8b carry-forwards (b) + (c): discharge the GAT
    /// declaration's per-param inline bounds and where-clause for a
    /// substituted projection. The projection must be in its
    /// post-substitution shape (`AssocProjection { param: <bare
    /// receiver name>, args: <concrete projection args>, .. }`) — the
    /// `param` field's bare name keys the impl-table lookup, and the
    /// `args` field carries the concrete types substituted for each
    /// `gat_param`. Anything else (still-`TypeParam` receiver,
    /// non-projection type, post-resolution non-projection) is a no-op.
    ///
    /// For each (gat_param, arg) position, checks the GAT decl's
    /// inline bounds (`type Mapped[U: Trait]`) via
    /// `type_satisfies_bound`. Emits `E_GAT_PARAM_BOUND_NOT_SATISFIED`
    /// on miss.
    ///
    /// For the GAT decl's `where`-clause (`type Mapped[U] where U:
    /// Trait`), substitutes `gat_params → args` and walks each
    /// `TypeBound` constraint — the substituted RHS type is checked
    /// via `type_satisfies_bound`. Emits
    /// `E_GAT_WHERE_CLAUSE_NOT_SATISFIED` on miss. Non-`TypeBound`
    /// constraints (AssocTypeEq / ConstPredicate / nested
    /// ProjectionBound) are out of scope for this slice — they're
    /// uncommon on GAT decls and the existing call-site discharge
    /// paths cover them when they appear.
    pub(super) fn discharge_gat_decl_constraints(
        &mut self,
        substituted: &Type,
        discharge_span: &Span,
    ) {
        let Type::AssocProjection {
            param, assoc, args, ..
        } = substituted
        else {
            return;
        };
        let key = (param.clone(), assoc.clone());
        let Some(entry) = self.env.impl_assoc_types.get(&key).cloned() else {
            return;
        };
        // (c) Per-param inline bounds — `type Mapped[U: Trait]` checks
        // each projection arg against its position-aligned bound trait
        // list.
        for ((gat_name, bound_traits), arg) in entry
            .gat_params
            .iter()
            .zip(entry.param_bound_traits.iter())
            .zip(args.iter())
        {
            if matches!(arg, Type::TypeParam(_) | Type::TypeVar(_) | Type::Error) {
                continue;
            }
            for trait_name in bound_traits {
                if self.type_satisfies_bound(arg, trait_name) {
                    continue;
                }
                self.type_error(
                    format!(
                        "error[E_GAT_PARAM_BOUND_NOT_SATISFIED]: \
                         GAT param `{}: {}` on `{}.{}` is not satisfied; \
                         arg `{}` does not implement `{}`",
                        gat_name,
                        trait_name,
                        param,
                        assoc,
                        type_display(arg),
                        trait_name,
                    ),
                    *discharge_span,
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        // (b) GAT decl's where-clause — substitute `gat_params → args`
        // into each `TypeBound` LHS and discharge via the same
        // `type_satisfies_bound` engine. Position-aligned with
        // `gat_params`.
        if let Some(ref wc) = entry.where_clause {
            let subs: HashMap<String, Type> = entry
                .gat_params
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect();
            for constraint in &wc.constraints {
                let WhereConstraint::TypeBound {
                    type_name, bounds, ..
                } = constraint
                else {
                    continue;
                };
                let Some(arg_ty) = subs.get(type_name) else {
                    continue;
                };
                if matches!(arg_ty, Type::TypeParam(_) | Type::TypeVar(_) | Type::Error) {
                    continue;
                }
                for bound in bounds {
                    let Some(trait_name) = bound.path.last() else {
                        continue;
                    };
                    if self.type_satisfies_bound(arg_ty, trait_name) {
                        continue;
                    }
                    self.type_error(
                        format!(
                            "error[E_GAT_WHERE_CLAUSE_NOT_SATISFIED]: \
                             GAT decl `where {}: {}` on `{}.{}` is not satisfied; \
                             arg `{}` does not implement `{}`",
                            type_name,
                            trait_name,
                            param,
                            assoc,
                            type_display(arg_ty),
                            trait_name,
                        ),
                        *discharge_span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
        }
    }

    /// GAT slice 8c — recursive walker that finds every
    /// `AssocProjection` node inside `ty` and dispatches each to
    /// `discharge_gat_decl_constraints`. The walker is the sibling
    /// trigger to the explicit-where-clause-bound discharge inside
    /// `discharge_projection_bounds`: signatures like
    /// `fn f[F: Functor](x: F.Mapped[NoShow])` (with `type Mapped[U:
    /// Show]`) reach the projection through the param-type position
    /// rather than a where-clause bound, so the implicit walk is
    /// what fires the GAT-decl per-param inline bound check.
    ///
    /// Walks every compound type shape (`Named.args`, `Tuple`,
    /// `Array.element`, `Slice.element`, `Ref` / `MutRef` / `Weak` /
    /// `Pointer.inner`, `Function.params` / `Function.return_type`,
    /// `OnceFunction.params` / `OnceFunction.return_type`) so a
    /// projection nested inside e.g. `Vec[F.Mapped[NoShow]]` or
    /// `(F.Mapped[NoShow], i64)` still gets discharged. The receiver
    /// `AssocProjection { receiver_args, args, .. }` walks both arg
    /// lists in case nested projections appear there too.
    ///
    /// Terminal types (`Int` / `UInt` / `Float` / `Bool` / `Char` /
    /// `String` / `Unit` / `Never` / `Error` / `TypeVar` / `TypeParam`
    /// / `Shared`) carry no projections and short-circuit. Idempotent:
    /// re-running on the same type re-issues the same diagnostics, so
    /// callers should call it exactly once per call-site discharge.
    pub(super) fn discharge_gat_decl_constraints_in(&mut self, ty: &Type, discharge_span: &Span) {
        match ty {
            Type::AssocProjection {
                args,
                receiver_args,
                ..
            } => {
                self.discharge_gat_decl_constraints(ty, discharge_span);
                for arg in args {
                    self.discharge_gat_decl_constraints_in(arg, discharge_span);
                }
                for arg in receiver_args {
                    self.discharge_gat_decl_constraints_in(arg, discharge_span);
                }
            }
            Type::Tuple(elems) => {
                for elem in elems {
                    self.discharge_gat_decl_constraints_in(elem, discharge_span);
                }
            }
            Type::Named { args, .. } => {
                for arg in args {
                    self.discharge_gat_decl_constraints_in(arg, discharge_span);
                }
            }
            Type::Array { element, .. } | Type::Slice { element, .. } => {
                self.discharge_gat_decl_constraints_in(element, discharge_span);
            }
            Type::Ref(inner)
            | Type::MutRef(inner)
            | Type::Weak(inner)
            | Type::Rc(inner)
            | Type::Arc(inner)
            | Type::Pointer { inner, .. } => {
                self.discharge_gat_decl_constraints_in(inner, discharge_span);
            }
            Type::Function {
                params,
                return_type,
            }
            | Type::OnceFunction {
                params,
                return_type,
            } => {
                for param in params {
                    self.discharge_gat_decl_constraints_in(param, discharge_span);
                }
                self.discharge_gat_decl_constraints_in(return_type, discharge_span);
            }
            _ => {}
        }
    }

    /// Check whether `ty` satisfies the named trait. Consults three
    /// sources in order:
    ///
    /// 1. **Built-in primitive coverage** for standard traits (Hash, Eq,
    ///    PartialEq, Ord, PartialOrd, Display) — primitives like `i64` /
    ///    `char` / `bool` satisfy these implicitly. The existing
    ///    `type_supports_*` helpers carry this knowledge, including
    ///    `#[derive(...)]`-driven satisfaction on named struct / enum types.
    /// 2. **Other named traits** via the impl table — direct impl lookup
    ///    plus supertrait closure walk via `env.type_satisfies_trait`.
    ///
    /// Returns `false` for types that can't satisfy nominal trait bounds
    /// (function types, raw pointers, type variables) — the discharge
    /// engine guards `TypeVar` / `TypeParam` / `Error` upstream so those
    /// don't reach here in practice.
    pub(super) fn type_satisfies_bound(&self, ty: &Type, trait_name: &str) -> bool {
        // `impl Trait` slice 3: an existential whose declared bound
        // matches the queried trait satisfies it by construction. The
        // existential's value type IS the trait surface — slice 3 does
        // not yet walk supertrait closures here (slice 5 + Phase 8 may
        // extend), so only an exact trait-name match qualifies.
        if let Type::Existential {
            trait_name: existential_trait,
            ..
        } = ty
        {
            if existential_trait == trait_name {
                return true;
            }
        }
        // Built-in coverage via the type_supports_* helpers — these
        // recognize primitives implicitly + named types via
        // `#[derive(Trait)]` registration.
        //
        // GAT slice 8b carry-forward (a): the derive-only builtins
        // Clone / Copy / Debug are recognized by the parser
        // (`DERIVE_ONLY_BUILTINS` in `bounds.rs`) but are not
        // registered as impl-table entries — so a bound `: Clone` on
        // a GAT (or a where-clause bound `T: Clone` reaching this
        // helper through `discharge_type_bounds`) would conservatively
        // reject every concrete RHS without this switch. The
        // type_supports_* / is_type_copy helpers consult
        // `derived_traits` directly, matching the pattern used for
        // Hash / Display / Eq above.
        match trait_name {
            "Hash" => return self.type_supports_hash(ty),
            "Eq" => return self.type_supports_eq(ty),
            "PartialEq" => return self.type_supports_partial_eq(ty),
            "Ord" => return self.type_supports_ord(ty),
            "PartialOrd" => return self.type_supports_partial_ord(ty),
            "Display" => return self.type_supports_display(ty),
            "Clone" => return self.type_supports_clone(ty),
            "Copy" => return self.is_type_copy(ty),
            "Debug" => return self.type_supports_debug(ty),
            // `Default` is a derive-only builtin (no `trait Default`) — a
            // `#[derive(Default)]` synthesizes a CONCRETE inherent `default`
            // impl, not a trait-table entry, so the impl-table fallthrough
            // below would reject every named type. `type_supports_default`
            // recognizes primitives implicitly and named types via that
            // synthesized `default` method — the analogue of the Clone/Debug
            // arms above. Without this a `T: Default` bound (std.mem `take`,
            // any user `fn f[T: Default]`) rejects every concrete arg.
            "Default" => return self.type_supports_default(ty),
            // Built-in marker trait for primitive numeric types (SIMD lane
            // elements + `fn f[T: Numeric]`). See `type_supports_numeric`.
            "Numeric" => return self.type_supports_numeric(ty),
            // Built-in structural marker for GPU-compatible types
            // (design.md § GpuSafe trait). Satisfied iff the FE-2 predicate
            // finds no offending heap / RC leaf — the same "all the way down"
            // walk the `#[gpu]` signature check uses, so the explicit
            // `T: GpuSafe` bound and the implicit `#[gpu]` constraint agree.
            "GpuSafe" => return self.is_gpu_safe_type(ty),
            _ => {}
        }
        // Other traits: explicit impl in the table, with supertrait closure.
        let Some((ty_name, ty_args)) = impl_table_key(ty) else {
            return false;
        };
        // An aliased imported trait (`import doer.{Doer as D}` + `T: D`) is
        // canonicalized inside `type_satisfies_trait` — see
        // `Env::trait_alias_canonical` (B-2026-07-29-10). Doing it there rather
        // than here is what also covers the impl-block bound gate, which
        // reaches the impl table through `Env::first_unsatisfied_bound` and
        // never passes through this function.
        self.env
            .type_satisfies_trait(&ty_name, &ty_args, trait_name)
    }

    /// Walk a where-clause and discharge each `ConstPredicate(expr)`
    /// against the resolved const-args (const generics slice 3c).
    /// Substitutes `Identifier(name)` references in the predicate with
    /// `Integer(value)` literals from `const_arg_subst`, then evaluates
    /// via `eval_const_expr` against `Type::Bool`. Emits a focused
    /// `"const constraint violated"` diagnostic on `Bool(false)`; other
    /// eval errors propagate via the existing `emit_const_eval_error`.
    fn discharge_const_predicates(
        &mut self,
        where_clause: &WhereClause,
        const_arg_subst: &HashMap<String, i64>,
        discharge_span: &Span,
    ) {
        for constraint in &where_clause.constraints {
            let WhereConstraint::ConstPredicate { expr, .. } = constraint else {
                continue;
            };
            let substituted = substitute_const_idents_in_expr(expr, const_arg_subst);
            match self.eval_const_expr(&substituted, &Type::Bool) {
                Ok(crate::prelude::ConstValue::Bool(true)) => {}
                Ok(crate::prelude::ConstValue::Bool(false)) => {
                    let bindings_summary: Vec<String> = const_arg_subst
                        .iter()
                        .map(|(n, v)| format!("{}={}", n, v))
                        .collect();
                    let bindings_str = if bindings_summary.is_empty() {
                        String::new()
                    } else {
                        format!(" with {}", bindings_summary.join(", "))
                    };
                    self.type_error(
                        format!(
                            "const constraint violated: predicate is false{}",
                            bindings_str
                        ),
                        *discharge_span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                Ok(_) => {
                    // Non-Bool result — the predicate expression isn't a
                    // boolean test. Slice 2's evaluator routes type
                    // mismatches through ConstEvalError, but the
                    // surface here is "predicate must return bool" —
                    // skip silently (slice 2's per-operator checks
                    // already surfaced any type errors).
                }
                Err(e) => self.emit_const_eval_error(e),
            }
        }
    }

    /// Validate `args` against a concrete `FunctionSig`. Used by the
    /// expected-type bare-call dispatch when the target is a concrete type and
    /// the impl's stored signature is the source of truth (no Self
    /// substitution needed).
    fn validate_args_against_sig(
        &mut self,
        name: &str,
        sig: &FunctionSig,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        if args.len() != sig.params.len() {
            self.type_error(
                format!(
                    "associated function '{}' expects {} argument(s), found {}",
                    name,
                    sig.params.len(),
                    args.len()
                ),
                *span,
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return sig.return_type.clone();
        }
        for (arg, param_ty) in args.iter().zip(sig.params.iter()) {
            let arg_ty = self.infer_expr(&arg.value);
            self.check_assignable(param_ty, &arg_ty, arg.value.span);
            self.warn_partial_move_of_drop_struct(&arg.value, param_ty);
        }
        sig.return_type.clone()
    }

    fn try_apply_into_coercion(&mut self, expr: &Expr, expected: &Type) -> Option<Type> {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &expr.kind
        else {
            return None;
        };
        if method != "into" || !args.is_empty() {
            return None;
        }
        // Wrapping conversions — `From[T] for Option[T]` (wrap in `Some`) and
        // `From[T] for Result[T, E]` (wrap in `Ok`), design.md § Conversion
        // Traits. Unlike the numeric / user-type `From` impls there is no
        // `.from()` method to dispatch: lowering rewrites the call straight to
        // `Some(x)` / `Ok(x)`, so this arm only verifies the source matches the
        // payload type (`args[0]`) and records the target enum name for
        // lowering. `E` in `Result[T, E]` is supplied entirely by `expected`
        // (the surrounding annotation), exactly as a hand-written `Ok(x)` at
        // the same position resolves it — nothing here constrains `args[1]`.
        // Checking the source against the payload via `check_expr` threads the
        // payload type down (so a bare literal types against it, e.g.
        // `let o: Option[i32] = 5.into()`) and reuses the shared check-mode
        // diagnostics on a mismatch. Previously every `.into()` at an
        // Option/Result position fell through to the "no impl From" error, so
        // this is purely additive.
        if let Type::Named { name, args: targs } = expected {
            let is_wrap =
                (name == "Option" && targs.len() == 1) || (name == "Result" && targs.len() == 2);
            if is_wrap {
                let payload = targs[0].clone();
                let before = self.errors.len();
                self.check_expr(object, &payload);
                if self.errors.len() == before {
                    self.into_conversions
                        .insert(SpanKey::from_span(&expr.span), name.clone());
                    self.record_expr_type(&expr.span, expected);
                    return Some(expected.clone());
                }
                self.record_expr_type(&expr.span, &Type::Error);
                return Some(Type::Error);
            }
        }
        let target_name = match expected {
            Type::Named { name, .. } => name.clone(),
            Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char | Type::Str => {
                type_display(expected)
            }
            _ => return None,
        };
        let src_ty = self.infer_expr(object);
        if src_ty == Type::Error {
            self.record_expr_type(&expr.span, &Type::Error);
            return Some(Type::Error);
        }
        // The resolved impl's dispatch segment, not the bare target — see
        // `resolved_impl_dispatch_segment` (B-2026-08-27-1).
        let from_impl_span = self
            .env
            .find_from_impl(&src_ty, &target_name, &[])
            .map(|imp| imp.target_span);
        if let Some(target_span) = from_impl_span {
            let seg = self.resolved_impl_dispatch_segment(target_span, &target_name);
            self.into_conversions
                .insert(SpanKey::from_span(&expr.span), seg);
            self.record_expr_type(&expr.span, expected);
            return Some(expected.clone());
        }
        self.type_error(
            format!(
                "no `impl From[{}] for {}` is in scope; cannot `.into()`",
                type_display(&src_ty),
                target_name
            ),
            expr.span,
            TypeErrorKind::TypeMismatch,
        );
        self.record_expr_type(&expr.span, &Type::Error);
        Some(Type::Error)
    }

    /// Recognize `x.try_into()` at an expected `Result[Target, _]` position.
    /// String-receiver `s.parse()` against an expected `Option[T]` (T a numeric
    /// primitive with a type-receiver `.parse`): record the target `T` in
    /// `parse_conversions` and return `Option[T]`. Lowering rewrites the call to
    /// the existing `T.parse(s)`, so no new interp/codegen surface is needed —
    /// this is purely the Rust-familiar string-receiver sugar for the annotated
    /// position (`let n: Option[i64] = s.parse()`, a `-> Option[i64]` return, an
    /// `Option[i64]` argument). Returns `None` (caller falls through to the
    /// normal "no method 'parse' on String" path) for any other shape — the
    /// unannotated / `.unwrap()`-chained forms use `i64.parse(s)` directly.
    fn try_apply_parse_coercion(&mut self, expr: &Expr, expected: &Type) -> Option<Type> {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &expr.kind
        else {
            return None;
        };
        if method != "parse" || !args.is_empty() {
            return None;
        }
        // Expected must be `Option[T]` with T a numeric primitive the
        // type-receiver `.parse` supports (i8..i64 / u8..u64 / usize / f64;
        // isize and f32 are not wired on the type-receiver side, so exclude them
        // here to keep the sugar and the underlying method in lockstep).
        let Type::Named { name, args: targs } = expected else {
            return None;
        };
        if name != "Option" || targs.len() != 1 {
            return None;
        }
        let t_name = match &targs[0] {
            Type::Int(_) | Type::UInt(_) | Type::Float(_) => type_display(&targs[0]),
            _ => return None,
        };
        if !matches!(
            t_name.as_str(),
            "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" | "f64"
        ) {
            return None;
        }
        // Receiver must be a `String` / `str`.
        let recv_ty = self.infer_expr(object);
        let is_string = matches!(&recv_ty, Type::Str)
            || matches!(&recv_ty, Type::Named { name, args } if name == "String" && args.is_empty());
        if !is_string {
            return None;
        }
        self.parse_conversions
            .insert(SpanKey::from_span(&expr.span), t_name);
        self.record_expr_type(&expr.span, expected);
        Some(expected.clone())
    }

    /// Mirrors `try_apply_into_coercion` with one twist: the target type is
    /// `Result.args[0]`, not the bare expected type. On a hit (matching
    /// `impl TryFrom[S] for Target`), records the rewrite span in
    /// `try_into_conversions` and returns the expected `Result[Target, E]`.
    /// On a miss, emits a "no `impl TryFrom[S] for T`" diagnostic and returns
    /// `Type::Error`. Returns `None` (caller falls through) when the
    /// expression isn't a zero-arg `.try_into()` call or when the expected
    /// type isn't `Result[_, _]`.
    fn try_apply_tryinto_coercion(&mut self, expr: &Expr, expected: &Type) -> Option<Type> {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &expr.kind
        else {
            return None;
        };
        if method != "try_into" || !args.is_empty() {
            return None;
        }
        // Expected must be `Result[Target, _]`. Extract Target.
        let target_ty = match expected {
            Type::Named { name, args } if name == "Result" && args.len() == 2 => &args[0],
            _ => return None,
        };
        let target_name = match target_ty {
            Type::Named { name, .. } => name.clone(),
            Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char | Type::Str => {
                type_display(target_ty)
            }
            _ => return None,
        };
        let src_ty = self.infer_expr(object);
        if src_ty == Type::Error {
            self.record_expr_type(&expr.span, &Type::Error);
            return Some(Type::Error);
        }
        // The resolved impl's dispatch segment, not the bare target — see
        // `resolved_impl_dispatch_segment` (B-2026-08-27-1).
        let tryfrom_impl_span = self
            .env
            .find_tryfrom_impl(&src_ty, &target_name, &[])
            .map(|imp| imp.target_span);
        if let Some(target_span) = tryfrom_impl_span {
            let seg = self.resolved_impl_dispatch_segment(target_span, &target_name);
            self.try_into_conversions
                .insert(SpanKey::from_span(&expr.span), seg);
            self.record_expr_type(&expr.span, expected);
            return Some(expected.clone());
        }
        self.type_error(
            format!(
                "no `impl TryFrom[{}] for {}` is in scope; cannot `.try_into()`",
                type_display(&src_ty),
                target_name
            ),
            expr.span,
            TypeErrorKind::TypeMismatch,
        );
        self.record_expr_type(&expr.span, &Type::Error);
        Some(Type::Error)
    }

    /// Solve a closure's return type against the `?` demands its body
    /// raised (B-2026-07-31-19). Result-form `?` sites demand
    /// `Result[_, E]` with a single unified `E`; Option-form sites demand
    /// `Option[_]`. An unbound Err slot left by a bare `Ok(v)` tail
    /// (`Result[T, ?E]`) is substituted with the demanded `E`; a concrete
    /// conflicting Err (or a non-Result/Option return with demands) is a
    /// hard error at the closure. Cross-error `From` conversions are not
    /// applied inside closures — the demand must match exactly.
    fn solve_closure_question_demands(
        &mut self,
        ret: Type,
        question_errs: Vec<Type>,
        question_option: bool,
        span: &Span,
    ) -> Type {
        if question_errs.is_empty() && !question_option {
            return ret;
        }
        if question_option && !question_errs.is_empty() {
            self.type_error(
                "closure mixes `?` on Result and `?` on Option; its return type \
                 cannot be both"
                    .to_string(),
                *span,
                TypeErrorKind::TypeMismatch,
            );
            return ret;
        }
        if question_option {
            match &ret {
                Type::Named { name, .. } if name == "Option" => return ret,
                Type::Never | Type::Error => return ret,
                other => {
                    self.type_error(
                        format!(
                            "`?` on an Option inside this closure requires the closure to \
                             return `Option`, found '{}'",
                            type_display(other)
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return ret;
                }
            }
        }
        // Result form: unify the demanded Err types.
        let mut e_unified: Option<Type> = None;
        for e in question_errs {
            let e = resolve_type_var_top(&e, &self.env.substitutions);
            if matches!(e, Type::Error) {
                continue;
            }
            match &e_unified {
                None => e_unified = Some(e),
                Some(cur) if *cur != e => {
                    self.type_error(
                        format!(
                            "`?` sites in this closure propagate conflicting error types: \
                             '{}' vs '{}'",
                            type_display(cur),
                            type_display(&e)
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return ret;
                }
                _ => {}
            }
        }
        let Some(e_final) = e_unified else { return ret };
        match &ret {
            Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                let e_slot = resolve_type_var_top(&args[1], &self.env.substitutions);
                match &e_slot {
                    _ if e_slot == e_final => ret,
                    Type::TypeVar(_) | Type::TypeParam(_) | Type::Error => Type::Named {
                        name: "Result".to_string(),
                        args: vec![args[0].clone(), e_final],
                    },
                    other => {
                        self.type_error(
                            format!(
                                "`?` inside this closure propagates '{}' but the closure \
                                 returns `Result[_, {}]`",
                                type_display(&e_final),
                                type_display(other)
                            ),
                            *span,
                            TypeErrorKind::TypeMismatch,
                        );
                        ret
                    }
                }
            }
            Type::Never | Type::Error => ret,
            other => {
                self.type_error(
                    format!(
                        "`?` on a Result inside this closure requires the closure to \
                         return `Result`, found '{}'",
                        type_display(other)
                    ),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                ret
            }
        }
    }

    /// `redundant_suffix` (B-2026-08-20-36). design.md § Numeric Semantics >
    /// Literal suffixes: "A suffix matching the default type (`42i64`,
    /// `3.14f64`) is valid but the compiler warns — suppressible with
    /// `#[allow(redundant_suffix)]`."
    ///
    /// Scoped to the DEFAULT type exactly as the spec words it, so `42i64` and
    /// `3.14f64` warn and `42i32` does not — including `let c: i32 = 42i32`,
    /// where the suffix is redundant against the ANNOTATION rather than
    /// against the default. That wider rule is a plausible lint but it is not
    /// the sentence design.md wrote, and a lint that fires beyond its
    /// documented trigger is its own defect.
    ///
    /// De-duplicated by span: `infer_expr` re-infers the same literal under
    /// bidirectional checking, and each visit would otherwise warn again.
    fn warn_redundant_suffix(&mut self, suffix: &str, span: Span) {
        if !self
            .redundant_suffix_reported
            .insert((span.offset, span.length))
        {
            return;
        }
        // B-2026-08-21-14 — the fix is a DELETION of the suffix, and the span
        // to delete is derivable here without re-lexing: `span` covers the
        // whole literal token (`42i64`, `1_000i64`, `0xFFi64`, `3.14f64`) and
        // the suffix is its tail, so the edit is the last `suffix.len()` bytes.
        // Every suffix this lint fires on is ASCII (`i64` / `f64`), so byte
        // length and character length agree and the arithmetic cannot land
        // mid-character.
        //
        // DELETING IT IS SAFE BY THE LINT'S OWN PREDICATE: it fires only when
        // the suffix names the type the literal would have had anyway, so the
        // unsuffixed spelling infers to the same type. `1i64.to_string()` was
        // the shape worth checking — `1.to_string()` could plausibly have
        // lexed `1.` as a float — and it parses correctly.
        //
        // Guarded rather than assumed: a span shorter than the suffix it is
        // supposed to end with would mean the token span is not what this
        // arithmetic takes it for, and a wrong deletion is worse than none, so
        // that case emits the warning with no fix instead.
        let fix_it = (span.length > suffix.len()).then(|| crate::typechecker::FixIt {
            span: Span {
                offset: span.offset + span.length - suffix.len(),
                length: suffix.len(),
                ..span
            },
            replacement: String::new(),
        });
        self.type_lint_warning_with_fix(
            format!("redundant `{suffix}` suffix: {suffix} is the default type for this literal"),
            span,
            TypeErrorKind::RedundantSuffix,
            "redundant_suffix",
            fix_it,
        );
    }

    pub(super) fn infer_expr(&mut self, expr: &Expr) -> Type {
        let ty = self.infer_expr_inner(expr);
        self.record_expr_type(&expr.span, &ty);
        ty
    }

    /// B-2026-07-02-7: the inclusive `i64`-literal range of a narrow scalar
    /// int type. `None` for types a decimal `i64` literal can never overflow
    /// (i128; u64/u128 above the negative check baked into the min of 0)
    /// and for every non-int type.
    fn int_literal_range(ty: &Type) -> Option<(i128, i128)> {
        Some(match ty {
            Type::Int(IntSize::I8) => (i8::MIN as i128, i8::MAX as i128),
            Type::Int(IntSize::I16) => (i16::MIN as i128, i16::MAX as i128),
            Type::Int(IntSize::I32) => (i32::MIN as i128, i32::MAX as i128),
            // i64 was absent here while it was VACUOUS — every literal was an
            // i64, so it fit by construction. B-2026-08-06-16 makes the row
            // load-bearing: an unsigned-suffixed literal now carries a value up
            // to u64::MAX, and `let x: i64 = 18446744073709551615u64` silently
            // bound -1 without it.
            Type::Int(IntSize::I64) => (i64::MIN as i128, i64::MAX as i128),
            Type::UInt(UIntSize::U8) => (0, u8::MAX as i128),
            Type::UInt(UIntSize::U16) => (0, u16::MAX as i128),
            Type::UInt(UIntSize::U32) => (0, u32::MAX as i128),
            Type::UInt(UIntSize::U64) => (0, u64::MAX as i128),
            // `u128` is the one width no `(min, max)` pair of `i128` can
            // express — its top half lives past `i128::MAX`. The ceiling here
            // is therefore only the bound for a literal that is NOT `u128`-
            // suffixed; a suffixed one is validated by `check_int_literal_fits`
            // before this range is consulted, because the lexer already proved
            // the magnitude fits `u128` and the parser stored the top half as a
            // wrapped (negative) bit pattern this comparison would misread
            // (B-2026-08-19-23). What is left for this row to catch is a
            // genuine negative, which is what `min` does.
            Type::UInt(UIntSize::U128) => (0, i128::MAX),
            _ => return None,
        })
    }

    /// A literal's `i64` payload widened to `i128` — read as an UNSIGNED bit
    /// pattern when the suffix is unsigned. B-2026-08-06-16.
    ///
    /// The upper half of `u64` lives past `i64::MAX`, so `18446744073709551615u64`
    /// cannot ride the i64 carrier as itself; the parser stores the wrapped bit
    /// pattern (`-1`), which is exactly how `u64.MAX` is already represented at
    /// runtime. Widening that with a plain `as i128` would sign-extend it back
    /// to `-1` and the range check would reject a legal literal with "negative
    /// integer literal -1 cannot initialize unsigned type 'u64'".
    ///
    /// Safe to apply to EVERY unsigned-suffixed literal, not just the wrapped
    /// ones: the lexer only ever produces a non-negative payload from digits, so
    /// a negative `n` under an unsigned suffix can only have come from the
    /// parser's out-of-range arm. A genuinely negative literal is
    /// `Neg(Integer(n))` with `n` positive — a different AST shape entirely,
    /// validated on its own path — so `-5u64` still reports correctly.
    fn literal_as_i128(n: i128, sfx: Option<crate::token::IntSuffix>) -> i128 {
        use crate::token::IntSuffix;
        match sfx {
            // `u128` is NOT in this list: a 128-bit-suffixed magnitude is
            // stored POSITIVELY by the parser (B-2026-08-19-8 stage 3a),
            // because `i128` has room for the whole `(i64::MAX, u64::MAX]`
            // band and beyond. Applying the u64 wrap read-back to it would
            // truncate — caught by this function's own debug assertion when a
            // `u128` literal past `u64::MAX` first reached here.
            Some(IntSuffix::U8) | Some(IntSuffix::U16) | Some(IntSuffix::U32)
            | Some(IntSuffix::U64) => {
                // The WRAPPED u64 bit pattern, unchanged by the i128 node
                // (B-2026-08-19-8 stage 2). Widening `ExprKind::Integer` gave
                // the literal room; it did NOT change how an unsigned magnitude
                // past i64::MAX is encoded, because the interpreter still
                // represents unsigned runtime values the same wrapped way. The
                // lexer's thresholds are likewise unchanged, so `n` always fits
                // i64 here and the round-trip through `u64` is exact.
                //
                // Retiring the wrap is stage 5's business, together with the
                // runtime representation it mirrors — doing it here would split
                // the encoding between literal and value.
                debug_assert!(
                    i64::try_from(n).is_ok(),
                    "unsigned literal payload {n} is wider than the wrap encoding assumes"
                );
                ((n as i64) as u64) as i128
            }
            _ => n,
        }
    }

    /// Emit the out-of-range diagnostic when `value` does not fit `ty`'s
    /// literal range. Returns whether the literal fits (true = no error).
    /// Pre-fix every out-of-range literal was silently admitted and the two
    /// surfaces DIVERGED (interp keeps the wide value, codegen truncates at
    /// its honest width): `let x: u8 = -1` printed -1 vs
    /// 18446744073709551615, `f(70000)` against `i16` printed 70000 vs 4464.
    ///
    /// `sfx` is the literal's OWN suffix when it has one (`None` for a bare
    /// literal, and `None` for a negated one — a unary minus makes the value
    /// genuinely negative regardless of what the operand was spelled). It is
    /// needed for exactly one width: `u128` cannot be range-checked as an
    /// `i128` pair, so a `u128`-suffixed literal is accepted on the strength of
    /// the lexer having parsed its magnitude as a `u128`, while everything else
    /// still has to clear `min` (B-2026-08-19-23).
    pub(super) fn check_int_literal_fits(
        &mut self,
        value: i128,
        ty: &Type,
        span: &Span,
        sfx: Option<crate::token::IntSuffix>,
    ) -> bool {
        // A `u128`-suffixed literal is valid by construction: the lexer parsed
        // the magnitude with `u128::from_str_radix`, so it fits by definition,
        // and the top half rides as a wrapped NEGATIVE bit pattern that the
        // signed comparison below would reject as "negative literal cannot
        // initialize unsigned type". The suffix also pins the literal's type,
        // so the contextual check this shares a body with is vacuous for it.
        if matches!(ty, Type::UInt(UIntSize::U128))
            && matches!(sfx, Some(crate::token::IntSuffix::U128))
        {
            return true;
        }
        let Some((min, max)) = Self::int_literal_range(ty) else {
            return true;
        };
        if value < min || value > max {
            let msg = if value < 0 && min == 0 {
                format!(
                    "negative integer literal {} cannot initialize unsigned type '{}'",
                    value,
                    type_display(ty)
                )
            } else {
                format!(
                    "integer literal {} out of range for '{}' (expected {}..={})",
                    value,
                    type_display(ty),
                    min,
                    max
                )
            };
            self.type_error(msg, *span, TypeErrorKind::TypeMismatch);
            return false;
        }
        true
    }

    /// The compile-time integer value of a bare `200` / negated `-200`
    /// UNSUFFIXED literal expression, in i128 (so `-(i64::MIN)` shapes can't
    /// wrap). Suffixed literals return `None` — their range is validated
    /// against their own suffix at synthesis.
    fn unsuffixed_int_literal_value(expr: &Expr) -> Option<i128> {
        match &expr.kind {
            ExprKind::Integer(n, None) => Some(*n),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match &operand.kind {
                ExprKind::Integer(n, None) => Some(-*n),
                _ => None,
            },
            _ => None,
        }
    }

    /// The compile-time value of a SUFFIXED integer literal (`5i64` / negated
    /// `-5i64`), in i128. Companion of [`unsuffixed_int_literal_value`] for the
    /// coercion-boundary range check (B-2026-07-09-7): a suffixed literal is
    /// validated against its own suffix at synthesis but was NOT re-checked
    /// against a *differing* contextual type at a `let`/arg/return boundary, so
    /// `let x: u64 = -5i64` (a negative into unsigned) and `let x: u32 =
    /// 5_000_000_000i64` (out of range) silently coerced — the exact holes the
    /// unsuffixed check at `check_expr` closes for bare literals.
    fn suffixed_int_literal_value(expr: &Expr) -> Option<(i128, Option<crate::token::IntSuffix>)> {
        match &expr.kind {
            // B-2026-08-06-16: read through `literal_as_i128` rather than a
            // bare `as i128`, so an unsigned-suffixed literal carrying a
            // wrapped bit pattern (the upper half of u64) widens back to its
            // unsigned value instead of sign-extending to a negative one.
            ExprKind::Integer(n, sfx @ Some(_)) => Some((Self::literal_as_i128(*n, *sfx), *sfx)),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match &operand.kind {
                // The negated form keeps the signed reading: the operand of a
                // unary minus is a positive magnitude by construction, so there
                // is no bit pattern to recover, and `-5u64` must still report
                // as negative. The suffix is deliberately dropped: `-1u128` is
                // a genuine negative, not a wrapped pattern, and must clear the
                // unsigned `min` like any other.
                ExprKind::Integer(n, Some(_)) => Some((-*n, None)),
                _ => None,
            },
            _ => None,
        }
    }

    /// B-2026-08-14-6 — note that `expr` is an INTEGER landing in a FLOAT
    /// slot, so the implicit int-to-float widening applies to it.
    ///
    /// Recording only; nothing here rejects or rewrites. The interpreter reads
    /// the set at the container store/probe sites, where it otherwise has no
    /// declared element type to convert against and would leave an `Int` in a
    /// `Vec[f64]`.
    pub(super) fn record_float_coercion(&mut self, expr: &Expr, expected: &Type, actual: &Type) {
        let peel = |t: &Type| -> Type {
            match t {
                Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                other => other.clone(),
            }
        };
        if let (Type::Float(size), true) = (peel(expected), is_integer(&peel(actual))) {
            // The declared WIDTH rides along, not just the fact of the
            // coercion. `4294967295u32` is not representable in f32 and rounds
            // to 4294967296 in every compiled backend, so an interpreter that
            // widens to f64 and stops leaves a value the slot's own type cannot
            // hold (B-2026-08-14-7's storage half).
            self.float_coerced_arg_sites
                .insert(crate::resolver::SpanKey::from_span(&expr.span), size);
        }
    }

    /// B-2026-07-09-7 variable half (design decision (B)): reject an implicit
    /// NARROWING or SIGN-CHANGING integer coercion at a check-mode boundary
    /// (`let`/arg/return/struct-field — every position funnels through
    /// `check_expr`). Only widening coercions (`i32`→`i64`, `u8`→`u32`,
    /// `u8`→`i16`) stay implicit; anything else demands an explicit `as`.
    ///
    /// Deliberately skipped:
    ///   - integer *literals* (bare or suffixed) — already range-checked against
    ///     the contextual type at the top of `check_expr`, and literal coercion
    ///     when the value fits is intentionally allowed (`let a: u64 = 5i64`);
    ///   - non-integer or non-concrete types (floats, generics, type vars,
    ///     `Error`) — the gate needs a concrete signed/unsigned width on both
    ///     sides, so those fall through untouched.
    pub(super) fn check_int_widening_coercion(
        &mut self,
        expr: &Expr,
        expected: &Type,
        actual: &Type,
    ) {
        if *actual == Type::Error {
            return;
        }
        // A literal was already validated by the two `*_int_literal_value`
        // blocks; re-flagging it here would be a spurious "needs `as`" on a
        // value that provably fits.
        if Self::unsuffixed_int_literal_value(expr).is_some()
            || Self::suffixed_int_literal_value(expr).is_some()
        {
            return;
        }
        let peel = |t: &Type| -> Type {
            match t {
                Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                other => other.clone(),
            }
        };
        let target = peel(expected);
        let source = peel(actual);
        // Both sides must be concrete integers and genuinely differ; a
        // widening coercion needs no `as`.
        if !is_integer(&target) || !is_integer(&source) || target == source {
            return;
        }
        if int_coercion_is_widening(&source, &target) {
            return;
        }
        self.type_error(
            format!(
                "implicit coercion from '{}' to '{}' would narrow or change sign; \
                 an out-of-range value is not caught at compile time. Write an \
                 explicit 'as {}' to acknowledge the truncation (widening \
                 coercions such as i32 -> i64 remain implicit)",
                type_display(&source),
                type_display(&target),
                type_display(&target),
            ),
            expr.span,
            TypeErrorKind::TypeMismatch,
        );
    }

    /// B-2026-08-14-12 — the FLOAT sibling of `check_int_widening_coercion`.
    ///
    /// design.md's implicit-widening table already rules on this: `f16`/`bf16`
    /// -> `f32` -> `f64` is implicit because it is lossless, and "Any
    /// narrowing" needs an `as`. Only the integer half of that table was
    /// enforced, so `let d: f32 = c` with `c: f64` silently left `d` holding a
    /// value its own declared type cannot represent.
    ///
    /// Deliberately skipped, mirroring the integer gate:
    ///   - COMPILE-TIME-CONSTANT float expressions — a bare literal (an
    ///     unsuffixed one is INFERRED at the destination's width by
    ///     B-2026-08-14-11, so it never narrows), and equally `-1.0`,
    ///     `0.0 - 1.0` or `0.0 * (0.0 - 1.0)`, which are the same literal one
    ///     `-` or one fold away. The integer gate exempts its literals for the
    ///     value-is-known reason and `seeded_arg_narrows` extends that to
    ///     const-evaluable arithmetic for exactly this case; the diagnostic's
    ///     own justification ("the rounded value is not checked at compile
    ///     time") does not apply when there is no runtime value to round.
    ///     Measured, not assumed: without this, `Tensor.from([-1.0, 2.0])` at a
    ///     `Tensor[f32]` and `Bf16 { value: 0.0 * (0.0 - 1.0) }` were both
    ///     rejected, and neither involves a typed variable at all;
    ///   - non-float or non-concrete types — the gate needs a concrete width on
    ///     both sides, so generics, type vars and `Error` fall through.
    ///
    /// `f16` and `bf16` rank EQUAL and are not interchangeable: neither is a
    /// subset of the other (`bf16` trades mantissa for `f32`'s exponent range),
    /// so a coercion between them is caught by the `source != target` arm
    /// rather than waved through as a same-rank move.
    pub(super) fn check_float_narrowing_coercion(
        &mut self,
        expr: &Expr,
        expected: &Type,
        actual: &Type,
    ) {
        if *actual == Type::Error {
            return;
        }
        let peel = |t: &Type| -> Type {
            match t {
                Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                other => other.clone(),
            }
        };
        let target = peel(expected);
        let source = peel(actual);
        if target == source {
            return;
        }
        let (Some(target_rank), Some(source_rank)) =
            (float_width_rank(&target), float_width_rank(&source))
        else {
            return;
        };
        // A strictly narrower source widens losslessly and needs no `as`.
        if source_rank < target_rank {
            return;
        }
        // B-2026-09-01-12 — the constant exemption above used to run FIRST and
        // unconditionally, which let a literal keep a width its destination
        // cannot hold. Its justification is that an unsuffixed literal is
        // inferred AT the destination's width (B-2026-08-14-11), so it never
        // narrows; a SUFFIXED one is the opposite — the width is pinned, and
        // pinned wider than the slot is exactly the narrowing this gate exists
        // to catch. Measured before the fix: `let d: f32 = 16777217.0f64` held
        // 16777217 on all four backends, a value no `f32` can represent, and
        // `Option[f32] = Option.Some(0.1f64)` printed the f64 under `--interp`
        // and the rounded f32 compiled. So the exemption now covers a constant
        // only while none of its float leaves names a width wider than the
        // destination.
        if Self::float_const_names_wider_width(expr, target_rank) {
            // RETRACT the `redundant_suffix` warning this literal already drew
            // (B-2026-09-01-12's second defect). That lint fires on a suffix
            // naming the DEFAULT type, without consulting the destination, so
            // on `let d: f32 = 0.1f64` it reported "redundant `f64` suffix:
            // f64 is the default type for this literal" — a statement about
            // the program that is false, since dropping the suffix changes the
            // literal's width and therefore its value. Its fix-it (delete the
            // suffix) is right and is what the error below recommends; only
            // the claim of redundancy is wrong, so the warning is withdrawn
            // rather than reworded and the reader gets one diagnostic instead
            // of a correct one beside a misleading one. Matched by
            // CONTAINMENT, not equality: on `-0.1f64` the gate's span is the
            // whole unary expression and the lint's is the literal inside it.
            let lo = expr.span.offset;
            let hi = expr.span.offset + expr.span.length;
            self.warnings.retain(|w| {
                !(matches!(w.kind, TypeErrorKind::RedundantSuffix)
                    && w.span.offset >= lo
                    && w.span.offset + w.span.length <= hi)
            });
            self.type_error(
                format!(
                    "literal suffix '{}' is wider than the '{}' it initializes, \
                     so the value would have to be rounded to reach the \
                     destination. Drop the suffix — '{}' already types the \
                     literal — or write an explicit 'as {}' if the rounding is \
                     intended",
                    type_display(&source),
                    type_display(&target),
                    type_display(&target),
                    type_display(&target),
                ),
                expr.span,
                TypeErrorKind::TypeMismatch,
            );
            return;
        }
        if Self::float_expr_is_compile_time_constant(expr) {
            return;
        }
        self.type_error(
            format!(
                "implicit coercion from '{}' to '{}' would lose precision; the \
                 rounded value is not checked at compile time. Write an \
                 explicit 'as {}' to acknowledge the rounding (widening \
                 coercions such as f32 -> f64 remain implicit)",
                type_display(&source),
                type_display(&target),
                type_display(&target),
            ),
            expr.span,
            TypeErrorKind::TypeMismatch,
        );
    }

    /// B-2026-09-01-12 — does this compile-time-constant float expression
    /// contain a literal whose SUFFIX names a width wider than `target_rank`?
    ///
    /// The companion of [`float_expr_is_compile_time_constant`], and it walks
    /// the same shapes for the same reason: a suffix on any leaf of
    /// `-0.1f64` or `0.1f64 * 2.0` pins that leaf's width just as firmly as it
    /// does on a bare literal, and the fold then carries the pinned width into
    /// the destination. An unsuffixed leaf answers `false` — it is inferred at
    /// the destination's width and cannot narrow.
    fn float_const_names_wider_width(expr: &Expr, target_rank: u8) -> bool {
        let suffix_rank = |sfx: &crate::token::FloatSuffix| -> u8 {
            match sfx {
                crate::token::FloatSuffix::F16 | crate::token::FloatSuffix::BF16 => 0,
                crate::token::FloatSuffix::F32 => 1,
                crate::token::FloatSuffix::F64 => 2,
            }
        };
        match &expr.kind {
            ExprKind::Float(_, Some(sfx)) => suffix_rank(sfx) > target_rank,
            ExprKind::Float(_, None) => false,
            ExprKind::Unary { op, operand } => {
                matches!(op, UnaryOp::Neg)
                    && Self::float_const_names_wider_width(operand, target_rank)
            }
            ExprKind::Binary { op, left, right } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && (Self::float_const_names_wider_width(left, target_rank)
                    || Self::float_const_names_wider_width(right, target_rank))
            }
            _ => false,
        }
    }

    /// B-2026-08-14-12 — is this expression a float value the compiler already
    /// knows, with no typed variable anywhere in it?
    ///
    /// Syntactic on purpose rather than a call into the const-evaluator. The
    /// evaluator wants a target type to evaluate AT, and its `ConstValue` has
    /// `F32`/`F64` arms but no `f16`/`bf16` ones — so asking it about a `bf16`
    /// destination answers "not constant" for a reason that has nothing to do
    /// with the question. What the exemption actually needs is the weaker,
    /// decidable property: every leaf is a float literal. A typed variable is a
    /// leaf that fails, which is precisely the narrowing this gate exists to
    /// catch, so the exemption cannot swallow the bug.
    fn float_expr_is_compile_time_constant(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Float(_, _) => true,
            ExprKind::Unary { op, operand } => {
                matches!(op, UnaryOp::Neg) && Self::float_expr_is_compile_time_constant(operand)
            }
            ExprKind::Binary { op, left, right } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && Self::float_expr_is_compile_time_constant(left)
                    && Self::float_expr_is_compile_time_constant(right)
            }
            _ => false,
        }
    }

    /// B-2026-08-08-9 — does this argument lose bytes flowing into a slot that
    /// expected-return SEEDING fixed from the outside?
    ///
    /// Deliberately much narrower than `check_int_widening_coercion`'s own rule,
    /// because a seeded slot is not an ordinary assignment target and two
    /// existing behaviours have to survive:
    ///
    ///  - SAME-WIDTH reinterpretation stays permissive. `return Some(i)` with
    ///    `i: i64` into an `Option[u64]` is a tested, deliberate shape
    ///    (`generic_numeric_arg_same_layout_and_literals_still_accepted`:
    ///    "same eight bytes, only the interpretation differs"). It disagrees
    ///    with the plain `return i` into a `-> u64` fn, which IS rejected —
    ///    but that disagreement predates this row and settling it is a language
    ///    decision, not a bug fix. Only a STRICT width reduction is caught.
    ///
    ///  - A COMPILE-TIME-CONSTANT argument stays permissive when its value
    ///    fits. `let a: Option[i32] = Some(0 - 1)` is arithmetic over literals,
    ///    so the bare-literal exemption inside `check_int_widening_coercion`
    ///    misses it, but the value is known and provably in range — the whole
    ///    point of B-2026-08-05-25. Const-evaluated against the TARGET, whose
    ///    range check is what "fits" means; a non-constant argument (the actual
    ///    bug: a variable of unknown value) fails the eval and is caught.
    fn seeded_arg_narrows(&mut self, expr: &Expr, expected: &Type, actual: &Type) -> bool {
        let peel = |t: &Type| -> Type {
            match t {
                Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                other => other.clone(),
            }
        };
        let target = peel(expected);
        let source = peel(actual);
        if int_coercion_is_widening(&source, &target) {
            return false;
        }
        let (Some((source_width, _)), Some((target_width, _))) =
            (int_signed_width(&source), int_signed_width(&target))
        else {
            return false;
        };
        if target_width >= source_width {
            return false;
        }
        self.eval_const_expr(expr, &target).is_err()
    }

    fn infer_expr_inner(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            // Literals
            ExprKind::Integer(n, sfx) => {
                let ty = self.type_from_int_suffix(*sfx, expr.span);
                if matches!(sfx, Some(crate::token::IntSuffix::I64)) {
                    self.warn_redundant_suffix("i64", expr.span);
                }
                // B-2026-07-02-7: a SUFFIXED literal's own suffix defines its
                // range — `300u8` was admitted and silently diverged (interp
                // printed 300, codegen truncated to 44). Unsuffixed literals
                // are validated against their CONTEXTUAL type in `check_expr`.
                let neg_validated = self
                    .neg_validated_suffixed_literal
                    .is_some_and(|k| k == (expr.span.offset, expr.span.length));
                if sfx.is_some() && !neg_validated {
                    self.check_int_literal_fits(
                        Self::literal_as_i128(*n, *sfx),
                        &ty,
                        &expr.span,
                        *sfx,
                    );
                }
                ty
            }
            ExprKind::Float(_, sfx) => {
                if matches!(sfx, Some(crate::token::FloatSuffix::F64)) {
                    self.warn_redundant_suffix("f64", expr.span);
                }
                Self::type_from_float_suffix(*sfx)
            }
            ExprKind::CharLit(_) => Type::Char,
            ExprKind::ByteLit(_) => Type::UInt(UIntSize::U8),
            // design.md § Byte and Byte-String Literals: "`b"..."` has type
            // `[u8; N]` where `N` is the byte count of the literal AFTER
            // ESCAPE RESOLUTION. Not `Slice[u8]`, not `&[u8; N]`." The length
            // is part of the type, which is the guarantee MMIO / protocol
            // code needs, so it is read off the resolved bytes rather than
            // inferred from context — an un-annotated `let b = b"hi"` is
            // `Array[u8, 2]`, not `Vec[u8]` (B-2026-08-20-37).
            ExprKind::ByteStringLit(bytes) => Type::Array {
                element: Box::new(Type::UInt(UIntSize::U8)),
                size: ConstArg::Literal(bytes.len() as i64),
            },
            ExprKind::StringLit(_) | ExprKind::MultiStringLit(_) => Type::Str,
            // `c"..."` C-string literal — typed `ref CStr` per
            // design.md § C-String Literals (v60 item 18). The
            // underlying `CStr` type itself is Phase 8 stdlib work
            // (methods `as_ptr`, `len`, etc.); slice 2 only commits
            // the literal-expression's type. The spec asks for a
            // `'static` lifetime annotation, which is aspirational —
            // Kāra v1 has no lifetime surface (no `'static` syntactic
            // form, no `Lifetime` carrier on `Type::Ref`), so `ref
            // CStr` is the v1 form. Method dispatch on the bare
            // `CStr` name will surface a NoMethodFound diagnostic
            // until Phase 8's stdlib registration lands.
            ExprKind::CStringLit { .. } => Type::Ref(Box::new(Type::Named {
                name: "CStr".to_string(),
                args: vec![],
            })),
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(inner_expr, spec) = part {
                        let ty = self.infer_expr(inner_expr);
                        if ty != Type::Error && !self.type_supports_display(&ty) {
                            self.type_error(
                                format!(
                                    "type '{}' does not implement Display; \
                                     cannot interpolate in f-string",
                                    type_display(&ty)
                                ),
                                inner_expr.span,
                                TypeErrorKind::TraitBoundNotSatisfied,
                            );
                        }
                        // Format specifier / value-type compatibility (Phase 8).
                        // Checked here so `karac run` and `karac build` reject the
                        // same programs at compile time.
                        if let Some(spec_raw) = spec {
                            if ty != Type::Error {
                                if let Err(msg) = check_format_spec_for_type(spec_raw, &ty) {
                                    self.type_error(
                                        msg,
                                        inner_expr.span,
                                        TypeErrorKind::TraitBoundNotSatisfied,
                                    );
                                }
                            }
                        }
                    }
                }
                Type::Str
            }
            ExprKind::Bool(_) => Type::Bool,

            // Identifiers
            ExprKind::Identifier(name) => {
                // B-2026-08-08-11 — note that this span read a `frozen`
                // parameter, so a mismatch on it can be rendered with the
                // spelling the author wrote instead of the `ref T` it lowers
                // to. Recording only; the type is unchanged.
                if self.current_fn_frozen_params.contains(name.as_str()) {
                    self.frozen_param_use_spans
                        .insert(SpanKey::from_span(&expr.span));
                }
                let ty = self.resolve_identifier_type(name, &expr.span);
                // B-2026-08-19-17 (b) — a bare variant name that two or more
                // USER enums declare is rejected rather than silently resolved
                // to whichever the scan's alphabetical tie-break happens to
                // reach. See `ambiguous_user_variant_owners` for why this is not
                // a real choice the author can override, and for the two
                // collisions that are deliberately still allowed.
                //
                // On THIS arm and not inside `resolve_identifier_type`, for the
                // same reason B-2026-08-11-6 put the type-name-in-value-position
                // diagnostic here: that helper is also the first-segment
                // fallback for `resolve_path_type`, which calls it
                // speculatively, so a hard error there fires on paths that go on
                // to resolve fine.
                //
                // The resolved `ty` is returned unchanged afterwards. The
                // program is already rejected, and letting inference continue
                // with the winner keeps this to ONE diagnostic instead of a
                // cascade of "expected X, found Error" at every downstream use.
                if let Some(owners) = self.ambiguous_user_variant_owners(name) {
                    let qualified = owners
                        .iter()
                        .map(|e| format!("`{e}.{name}`"))
                        .collect::<Vec<_>>()
                        .join(" or ");
                    let declared = owners
                        .iter()
                        .map(|e| format!("`{e}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.type_error(
                        format!(
                            "ambiguous variant name `{name}`: declared by {declared} — \
                             write {qualified} to say which one"
                        ),
                        expr.span,
                        crate::typechecker::TypeErrorKind::AmbiguousBareVariant,
                    );
                }
                // A TYPE NAME where a value belongs (B-2026-08-11-6). Reaching
                // `Type::Error` here means every real resolution failed —
                // including the enum-variant, distinct-type and comptime-`Type`
                // arms, which are the only bare-name value/constructor forms
                // there are (Kāra has no tuple structs: `struct T(i64)` is a
                // parse error). `resolve_identifier_type`'s own fallback is
                // SILENT on the assumption that the resolver already reported
                // the name — true for a typo, false for a type name, which
                // resolves perfectly well and simply is not a value.
                //
                // Nothing downstream could honour it and each backend failed
                // differently and late: the interpreter raised its own
                // "this is a compiler bug" internal error or hit an
                // `unreachable!`, while JIT and AOT silently discarded the
                // argument and evaluated the call to `0` — `let a = i64(42)`
                // printed `val=0`. The call form `i64(42)` lands here too,
                // because `infer_call` infers its callee through this arm.
                //
                // Deliberately on the BARE-IDENTIFIER arm rather than inside
                // `resolve_identifier_type`: that helper is also the first-
                // segment fallback for `resolve_path_type`, where a resource
                // dispatch like `RandomSource.next()` legitimately passes
                // through before later machinery resolves it.
                if ty == Type::Error {
                    // B-2026-08-17-7 — before diagnosing a type name in value
                    // position, try the USER's colliding enum variant. In this
                    // position (a bare identifier that is the whole
                    // expression, including a call's callee) a prelude type
                    // or module name has no legal meaning, so every shape
                    // this resolves was an error until now — and pattern
                    // position already binds the same bare name to the same
                    // variant. See `user_variant_value_type` for the
                    // exclusions and why this must not live in
                    // `resolve_identifier_type`.
                    if let Some(vt) = self.user_variant_value_type(name) {
                        return vt;
                    }
                    if let Some(msg) = self.type_name_in_value_position_message(name) {
                        self.type_error(
                            msg,
                            expr.span,
                            crate::typechecker::TypeErrorKind::NotCallable,
                        );
                    }
                }
                ty
            }
            ExprKind::Path { segments, .. } => self.resolve_path_type(segments, &expr.span),

            ExprKind::SelfValue => self.current_self_type.clone().unwrap_or(Type::Error),
            ExprKind::SelfType => self.current_self_type.clone().unwrap_or(Type::Error),

            // Operators
            ExprKind::Binary { op, left, right } => self.infer_binary(op, left, right, &expr.span),
            ExprKind::Pipe { left, right } => self.infer_pipe(left, right, &expr.span),
            ExprKind::Unary { op, operand } => {
                // B-2026-07-02-7: a negated SUFFIXED literal (`-1u8`) — the
                // negated value must fit the suffix's own range (the plain
                // suffixed check in the Integer arm above only sees the
                // positive operand). Pre-fix `-1u8` printed -1 under `karac
                // run` and 255 under `karac build`.
                let saved_neg_key = self.neg_validated_suffixed_literal;
                if matches!(op, UnaryOp::Neg) {
                    if let ExprKind::Integer(n, Some(sfx)) = &operand.kind {
                        let ty = self.type_from_int_suffix(Some(*sfx), operand.span);
                        self.check_int_literal_fits(-*n, &ty, &expr.span, None);
                        // The negated value ruled; suppress the Integer arm's
                        // positive-operand check for this operand (`-128i8` —
                        // bare `128i8` is out of range, the negated form is
                        // not).
                        self.neg_validated_suffixed_literal =
                            Some((operand.span.offset, operand.span.length));
                    }
                }
                let ty = self.infer_unary(op, operand, &expr.span);
                self.neg_validated_suffixed_literal = saved_neg_key;
                ty
            }

            // Postfix
            ExprKind::Question(inner) => {
                if self.in_defer {
                    self.type_error(
                        "'?' operator is not allowed inside defer/errdefer blocks".to_string(),
                        expr.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                self.infer_question(inner, &expr.span)
            }

            ExprKind::OptionalChain {
                object,
                field_or_method,
                args,
            } => self.infer_optional_chain(object, field_or_method, args, &expr.span),

            // Infix
            ExprKind::NilCoalesce { left, right } => {
                self.infer_nil_coalesce(left, right, &expr.span)
            }

            ExprKind::Call { callee, args } => {
                let t = self.infer_call(callee, args, &expr.span);
                // B-2026-09-03-20 — the PRELUDE constructors. `Some(x)` /
                // `Ok(x)` / `Err(e)` consume their payload by value exactly as
                // a user enum's `E.A(x)` does, but they never reach the
                // argument loop that covers the user form: they are answered by
                // dedicated arms that type the payload against a slot derived
                // from the expectation. Reading the payload's type back out of
                // `expr_types` keeps this to one site and asks nothing of those
                // arms; a payload the call did not record is skipped rather
                // than guessed at.
                self.check_prelude_ctor_payload_partial_move(callee, args);
                t
            }

            ExprKind::MethodCall {
                object,
                method,
                args,
                turbofish: _,
                args_close_span,
            } => {
                let t = self.infer_method_call(object, method, args, &expr.span, args_close_span);
                // The GENERIC-QUALIFIED prelude ctor: `Option[R].Some(w.r)` and
                // `Result[R, E].Ok(w.r)` parse as a MethodCall on a type
                // receiver, not as a Call, so the `infer_call` hook above never
                // sees them. Same helper, same read-back-the-recorded-type
                // bargain; the method NAME is the discriminator here.
                if matches!(method.as_str(), "Some" | "Ok" | "Err") {
                    self.check_prelude_ctor_payload_partial_move_named(method, args);
                }
                t
            }

            ExprKind::FieldAccess { object, field } => {
                self.infer_field_access(object, field, &expr.span)
            }

            ExprKind::TupleIndex { object, index } => {
                let obj_ty = self.infer_expr(object);
                // Project through a borrow, exactly as `infer_field_access`
                // does for a struct receiver (B-2026-08-11-11). `.0` on a
                // `ref (i64, i64)` and `.a` on a `ref P` are the same
                // operation on two aggregate kinds, but only the struct side
                // peeled, so `v.get(i).unwrap().0` on a `Vec[(i64,i64)]` was
                // rejected `tuple index on non-tuple type 'ref (i64, i64)'`
                // while the struct spelling type-checked. `Vec.get` is the
                // safe accessor the language steers people toward over
                // indexing, so the shape it returns has to be usable; the
                // workaround was to abandon `get` for `v[i].0`.
                //
                // Like the field case, the projection yields the element's
                // BY-VALUE type — a read through a borrow, not a reborrow —
                // so nothing downstream has to learn a new shape.
                let obj_ty = match &obj_ty {
                    Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                    _ => obj_ty,
                };
                match &obj_ty {
                    Type::Tuple(types) => {
                        let idx = *index as usize;
                        if idx < types.len() {
                            types[idx].clone()
                        } else {
                            self.type_error(
                                format!(
                                    "tuple index {} out of bounds for tuple of length {}",
                                    idx,
                                    types.len()
                                ),
                                expr.span,
                                TypeErrorKind::InvalidTupleIndex,
                            );
                            Type::Error
                        }
                    }
                    Type::Error => Type::Error,
                    _ => {
                        self.type_error(
                            format!("tuple index on non-tuple type '{}'", type_display(&obj_ty)),
                            expr.span,
                            TypeErrorKind::InvalidTupleIndex,
                        );
                        Type::Error
                    }
                }
            }

            ExprKind::Index { object, index } => {
                // B-2026-08-08-4 gap B — captured before the sub-expressions
                // are inferred, and deliberately NOT cleared. `a.b[i]` reaches
                // this arm from INSIDE `infer_field_access`, which infers its
                // object BEFORE capturing its own `is_lhs` (the union-read gate
                // needs the object typed first); clearing here therefore made
                // the enclosing field access read as a non-LHS, so
                // `orig[1].random = x` upgraded its `weak` field slot to
                // `Option[Node]` and rejected the strong RHS the downgrade
                // coercion is there to accept. Two ASAN fixtures caught it.
                // A pure read changes nothing for any existing path.
                let index_is_lhs = self.assigning_lhs;
                let obj_ty = self.infer_expr(object);
                let idx_ty = self.infer_expr(index);
                // `t.0[i]` — indexing a `Vec`/`VecDeque` that lives in a TUPLE
                // element. Codegen resolves the element's storage pointer
                // structurally (GEP into the tuple) but needs the element's
                // full `TypeExpr` to load with the correct width; the per-var
                // tuple name registry is lossy (drops generic args, unpopulated
                // for a Call-RHS binding). Record it in the span-keyed
                // `temp_recv_elem_types` table, keyed by the TupleIndex
                // receiver's span, exactly where codegen's tuple-index arm
                // reads it. WITHOUT this, `t.0[i]` failed codegen LOUD ("Index
                // operator applied to non-array type") while the interpreter
                // read the element — a run-vs-build gap (B-2026-07-20-2). Sibling
                // of the `.iter()`-receiver recording in `infer_method_call`.
                if matches!(&object.kind, ExprKind::TupleIndex { .. }) {
                    // B-2026-08-10-5 — a SLICE-typed tuple element needs the
                    // same recording. `Slice[T]` is `Type::Slice`, not
                    // `Type::Named`, so it missed this gate entirely and
                    // codegen's tuple-index arm never fired: `t.0[i]` fell to
                    // the generic tail and failed the build for both a read
                    // and a store, while the interpreter handled both. Newly
                    // common because `split_at_mut` (B-2026-08-10-4) returns
                    // exactly a tuple of slices.
                    let recorded = match &obj_ty {
                        Type::Named { name, args }
                            if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                        {
                            Some(args[0].clone())
                        }
                        Type::Slice { element, .. } => Some((**element).clone()),
                        _ => None,
                    };
                    if let Some(elem) = recorded {
                        let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
                        let te = Self::type_to_type_expr(&resolved);
                        self.temp_recv_elem_types
                            .insert(SpanKey::from_span(&object.span), te);
                    }
                }
                // B-2026-08-14-38 — indexing the result of a Vec-RETURNING
                // METHOD CALL (`v.clone()[1]`, `nums[1..3].to_vec()[0]`, any
                // `<method-chain>[i]` whose receiver is a fresh temporary).
                // Codegen's Vec index dispatch is keyed on a NAME, and a
                // temporary has none; its fallback for a nameless Vec
                // (`inline_temp_vec_te`) recovers the element type from a
                // free function's declared return, which a method call has no
                // entry for. Recording the receiver's own `Vec[T]` here gives
                // that fallback the one fact it was missing — and it has to be
                // its own table, not `expr_types`, because the parser stamps a
                // postfix expression with its receiver's span: the `Index` and
                // its object collide there and the index's ELEMENT type wins.
                // Same collision and same remedy as `tensor_index_recv_types`
                // just below, and as `temp_recv_elem_types` above.
                //
                // A `ref Vec` return is `Type::Ref` and never matches this
                // gate, so a borrow can't reach the path that frees the
                // temporary. An element still carrying an inference metavar
                // declines too — codegen would size the load from it.
                if matches!(&object.kind, ExprKind::MethodCall { .. }) {
                    if let Type::Named { name, args } = &obj_ty {
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 {
                            let elem = resolve_type_var_top(&args[0], &self.env.substitutions);
                            if !contains_type_var(&elem) {
                                let te = Self::type_to_type_expr(&Type::Named {
                                    name: name.clone(),
                                    args: vec![elem],
                                });
                                self.index_recv_vec_types
                                    .insert(SpanKey::from_span(&object.span), te);
                            }
                        }
                    }
                }
                // Phase 11: Tensor multi-dim indexing — `t[i, j, k]`
                // arrives as a tuple index (parser desugar per design.md
                // § Numerical Types > Indexing). Arity must equal the
                // rank when the static shape is splice-free; literal
                // indices bounds-check against concrete dims at compile
                // time. Returns the element type `T`.
                {
                    let tensor_ty = match &obj_ty {
                        Type::Named { name, args } if name == "Tensor" => Some((name, args)),
                        Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                            Type::Named { name, args } if name == "Tensor" => Some((name, args)),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some((_, args)) = tensor_ty {
                        if args.len() == 2 {
                            // B-2026-08-14-17 — record the RECEIVER's tensor
                            // type before this arm returns the element type.
                            // The parser stamps a postfix expression with its
                            // receiver's span, so the value recorded for this
                            // `Index` will overwrite the object's entry in
                            // `expr_types` — and `tensor_typed_exprs`, which
                            // codegen reads to decide whether a `Binary` is
                            // element-wise tensor arithmetic, is derived from
                            // it. `(t * 2)[0]` therefore reached the binop
                            // lowering with no tensor entry and was compiled as
                            // scalar arithmetic on two pointers, while
                            // `let r = t * 2; r[0]` compiled fine. Sibling of
                            // the `temp_recv_elem_types` recording above, for
                            // the same collision and the same reason.
                            self.tensor_index_recv_types
                                .insert(SpanKey::from_span(&object.span), obj_ty.clone());
                            let elem_ty = args[0].clone();
                            let idx_arity = match &idx_ty {
                                Type::Tuple(parts) => {
                                    for (part_ty, part_expr) in
                                        parts.iter().zip(tuple_index_parts(index).iter())
                                    {
                                        if !is_integer(part_ty) && *part_ty != Type::Error {
                                            self.type_error(
                                                format!(
                                                    "tensor index components must be \
                                                     integers, found '{}'",
                                                    type_display(part_ty)
                                                ),
                                                part_expr.map(|e| e.span).unwrap_or(index.span),
                                                TypeErrorKind::TypeMismatch,
                                            );
                                        }
                                    }
                                    Some(parts.len())
                                }
                                t if is_integer(t) => Some(1),
                                Type::Error => None,
                                _ => {
                                    self.type_error(
                                        format!(
                                            "tensor index must be integers (one per dim), \
                                             found '{}'",
                                            type_display(&idx_ty)
                                        ),
                                        index.span,
                                        TypeErrorKind::TypeMismatch,
                                    );
                                    None
                                }
                            };
                            if let (Some(arity), Type::Shape(dims)) = (idx_arity, &args[1]) {
                                let splice_free = !dims
                                    .iter()
                                    .any(|d| matches!(d, DimArg::Splice(_) | DimArg::SpliceVar(_)));
                                if splice_free && arity != dims.len() {
                                    self.type_error(
                                        format!(
                                            "rank-{} tensor requires {} index component(s), \
                                             found {} — index every dim explicitly \
                                             (`t[i, :, :]` slicing is v1.5)",
                                            dims.len(),
                                            dims.len(),
                                            arity
                                        ),
                                        index.span,
                                        TypeErrorKind::TypeMismatch,
                                    );
                                } else if splice_free {
                                    // Compile-time bounds check: literal
                                    // index against concrete dim.
                                    for (pos, (dim, idx_expr)) in
                                        dims.iter().zip(tuple_index_parts(index).iter()).enumerate()
                                    {
                                        if let (
                                            DimArg::Const(ConstArg::Literal(d)),
                                            Some(Expr {
                                                kind: ExprKind::Integer(i, _),
                                                span,
                                                ..
                                            }),
                                        ) = (dim, idx_expr)
                                        {
                                            if *i < 0 || *i >= i128::from(*d) {
                                                self.type_error(
                                                    format!(
                                                        "index {} out of bounds for dim {} \
                                                         (size {})",
                                                        i, pos, d
                                                    ),
                                                    *span,
                                                    TypeErrorKind::TypeMismatch,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            return elem_ty;
                        }
                    }
                }
                // Phase 11: Column positional indexing — `c[i] -> Option[T]`
                // (Some for a valid slot, None for a SQL null). The index
                // is a single integer; the null-vs-valid distinction is a
                // runtime property, so the static result is always
                // `Option[T]`.
                {
                    let column_elem = match &obj_ty {
                        Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                            Some(args[0].clone())
                        }
                        Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                            Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                                Some(args[0].clone())
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(elem_ty) = column_elem {
                        if !is_integer(&idx_ty) && idx_ty != Type::Error {
                            self.type_error(
                                format!(
                                    "column index must be an integer, found '{}'",
                                    type_display(&idx_ty)
                                ),
                                index.span,
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                        self.index_read_is_fresh_value
                            .insert(SpanKey::from_span(&expr.span));
                        return Type::Named {
                            name: "Option".to_string(),
                            args: vec![elem_ty],
                        };
                    }
                }
                // Map / SortedMap key index — `m[k] -> V` (design.md § Subscript
                // Trait: `[]` → `index(ref self, key: ref K) -> ref V`, panics if
                // the key is missing). B-2026-07-16-13: type it so `karac check`
                // accepts a non-integer key (`m["x"]` on `Map[String, i64]`) and
                // returns `V`, not the `Type::Error` the generic integer gate
                // would leave behind. Placed BEFORE the integer/range gate so a
                // String / struct key doesn't error there. Both backends have
                // NATIVE Map-index support (codegen's `compile_map_index` hashes
                // any K; the interpreter's `(Value::Map, key)` arm), so no
                // desugar is needed — this arm only unblocks the typecheck gate.
                // `m[1]` on `Map[i64, V]` also routes here now (it used to slip
                // through the integer gate and return `Type::Error` —
                // check-passing but interp-`unreachable!`).
                {
                    let map_kv = match &obj_ty {
                        Type::Named { name, args }
                            if matches!(name.as_str(), "Map" | "SortedMap") && args.len() == 2 =>
                        {
                            Some((args[0].clone(), args[1].clone()))
                        }
                        Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                            Type::Named { name, args }
                                if matches!(name.as_str(), "Map" | "SortedMap")
                                    && args.len() == 2 =>
                            {
                                Some((args[0].clone(), args[1].clone()))
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some((k_ty, v_ty)) = map_kv {
                        // The key must be assignable to `K`. A `ref`/`mut ref`
                        // key expression (`m[borrowed_key]`) is accepted against
                        // an owned `K` — indexing borrows the key (`ref K`), the
                        // same relaxation `Map.get`/`contains_key` got in
                        // B-2026-07-16-12.
                        let key_ty = match &idx_ty {
                            Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                            other => other.clone(),
                        };
                        if key_ty != Type::Error {
                            self.check_assignable(&k_ty, &key_ty, index.span);
                        }
                        // `V` for BOTH a read (`x = m[k]`, panics if missing)
                        // and an assignment target (`m[k] = v`, inserts /
                        // overwrites — the assign checks the RHS against `V`).
                        // Codegen already implements both (`compile_map_index` /
                        // `compile_index_store`); the interpreter's read + store
                        // arms mirror them (B-2026-07-16-13).
                        return v_ty;
                    }
                }
                // B-2026-07-15-3: an integer `ref` / `mut ref` indexes
                // through the borrow (`preorder[cur]` with `cur: mut ref
                // i64`) — same auto-deref arithmetic performs.
                let idx_ty = match idx_ty {
                    Type::Ref(inner) | Type::MutRef(inner) if is_integer(&inner) => *inner,
                    other => other,
                };
                let is_range_idx = matches!(&idx_ty, Type::Named { name, .. }
                    if matches!(name.as_str(), "Range" | "RangeInclusive" | "RangeFrom"
                        | "RangeTo" | "RangeToInclusive" | "RangeFull"));
                if !is_integer(&idx_ty) && !is_range_idx && idx_ty != Type::Error {
                    self.type_error(
                        format!(
                            "index must be an integer or range, found '{}'",
                            type_display(&idx_ty)
                        ),
                        index.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                if is_range_idx {
                    // String slicing: `s[a..b]` → a fresh `String` (a
                    // sub-range copy), NOT a `Slice[T]`. UTF-8 char-boundary
                    // validation happens at runtime (panic
                    // `E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY` on a non-boundary
                    // index, mirroring Rust). No borrowed-substring view at
                    // v1. See phase-8-stdlib-floor.md "String substring /
                    // slicing surface".
                    let is_string = matches!(&obj_ty, Type::Str)
                        || matches!(&obj_ty, Type::Ref(inner) | Type::MutRef(inner)
                            if matches!(inner.as_ref(), Type::Str));
                    if is_string {
                        self.index_read_is_fresh_value
                            .insert(SpanKey::from_span(&expr.span));
                        return Type::Str;
                    }
                    // Range indexing: `collection[a..b]` → `Slice[T]` where T
                    // is the element type of the indexed collection. See
                    // design.md § Slices and § Subscript Trait.
                    let element_ty = match &obj_ty {
                        Type::Array { element, .. } => Some(*element.clone()),
                        Type::Slice { element, .. } => Some(*element.clone()),
                        Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                            Some(args[0].clone())
                        }
                        Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                            Type::Array { element, .. } => Some(*element.clone()),
                            Type::Slice { element, .. } => Some(*element.clone()),
                            Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                                Some(args[0].clone())
                            }
                            _ => None,
                        },
                        Type::Error => return Type::Error,
                        _ => None,
                    };
                    self.index_read_is_fresh_value
                        .insert(SpanKey::from_span(&expr.span));
                    return match element_ty {
                        Some(el) => Type::Slice {
                            element: Box::new(el.clone()),
                            // B-2026-08-20-38 — a sub-range handed to a
                            // `mut Slice[T]` parameter is design.md § Slices'
                            // second example (`sort_in_place(mut v[1..4])`),
                            // and this arm produced a READ-ONLY header
                            // unconditionally — so the spec's own line failed
                            // with "expected 'mut Slice[i64]', found
                            // 'Slice[i64]'", with no other spelling available
                            // (`let s = mut v[1..4]` is not syntax).
                            //
                            // Keyed on the PARAMETER, not on the call-site
                            // `mut` marker, even though the marker is what the
                            // spec line writes. The marker rule is a
                            // free-function rule (design.md Feature 4 Part 1½
                            // — "method calls never mark"), so a method taking
                            // `mut Slice[T]` has no marker to key on and would
                            // otherwise inherit the same dead end. Marker
                            // discipline stays where it already lives, in
                            // `check_call_site_marker`, which sees this
                            // upgraded type and classifies the argument as
                            // FRESH — the mut-ness is a borrow taken here, not
                            // one forwarded in.
                            //
                            // Whether the BASE may yield a mutable view is
                            // asked with `types_compatible` against the very
                            // slot type this produces, rather than by
                            // re-listing sources: that is the same predicate
                            // the argument is checked against a moment later,
                            // so the upgrade cannot admit anything the
                            // assignability table would reject. A `ref Vec[T]`
                            // base and a read-only `Slice[T]` base both
                            // decline, and still report the read-only type
                            // they actually have.
                            mutable: self.mut_through_param_arg
                                && super::types::types_compatible(
                                    &Type::Slice {
                                        element: Box::new(el),
                                        mutable: true,
                                    },
                                    &obj_ty,
                                ),
                        },
                        None => {
                            self.type_error(
                                format!(
                                    "range indexing requires a Vec, Array, or Slice; found '{}'",
                                    type_display(&obj_ty)
                                ),
                                expr.span,
                                TypeErrorKind::TypeMismatch,
                            );
                            Type::Error
                        }
                    };
                }
                // `s[i]` on a `String` is a compile error (design.md
                // § Character type): UTF-8 is variable-width, so scalar
                // indexing would hide an O(n) scan behind `[]` syntax that
                // reads as O(1). Range slicing `s[a..b]` is a deliberate,
                // explicit exception handled by the range path above (it
                // returns a fresh `String`); only scalar indexing reaches
                // here. Without this rejection the (String, Int) operand
                // pair falls through to `_ => Type::Error` *silently* — no
                // diagnostic — so the program typechecks and reaches the
                // interpreter, where `Value::String[Value::Int]` trips an
                // `unreachable!` (eval_expr.rs). `s.char_at(i)` (a method
                // call) and `s.bytes()[i]` (indexing the `Slice[u8]` view)
                // are separate paths and keep working.
                let is_string = matches!(&obj_ty, Type::Str)
                    || matches!(&obj_ty, Type::Ref(inner) | Type::MutRef(inner)
                        if matches!(inner.as_ref(), Type::Str));
                if is_string {
                    self.type_error(
                        "String does not support indexing with []\n  \
                         s[i] would hide an O(n) scan — Strings are UTF-8 encoded \
                         and characters\n  \
                         are variable-width.\n  \
                         help: use s.char_at(i) for the i-th character (O(n)) \
                         — it returns Option[char], so match it or unwrap it,\n        \
                         or s.bytes()[i] for raw byte access (O(1))"
                            .to_string(),
                        expr.span,
                        TypeErrorKind::StringNotIndexable,
                    );
                    return Type::Error;
                }
                // B-2026-08-17-10 — the same hole as the String rejection
                // directly above, one type over, and it stayed open because
                // `Iterator[T]` is a `Type::Named` that simply matches no arm
                // of `elem_result` and falls to `_ => Type::Error` WITHOUT a
                // diagnostic. `karac check` then printed "All checks passed"
                // for `w.chars()[0]`, and each backend improvised: the
                // interpreter hit the `unreachable!` in `eval_expr.rs`
                // (obj=Value::Iterator), while codegen compiled a LET-BOUND
                // `chars()` index correctly but failed the build on the inline
                // form and on `.iter()`. A working feature by accident,
                // reachable only through a temporary binding.
                //
                // Rejection is the answer rather than making the backends
                // agree: an iterator is a lazy cursor with no positional
                // storage, and design.md specs no indexable `Iterator`.
                // `.collect()` (materialize, then index) and `.nth(i)` (one
                // element) are the shipped ways to get an element — both
                // verified on check / --interp / build before being named
                // here.
                let iterator_arg = match &obj_ty {
                    Type::Named { name, args } if name == "Iterator" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                };
                if let Some(elem) = iterator_arg {
                    let elem_str =
                        type_display(&resolve_type_var_top(&elem, &self.env.substitutions));
                    self.type_error(
                        format!(
                            "Iterator does not support indexing with []\n  \
                             an iterator is a lazy cursor over a sequence, not a container — \
                             it has no\n  \
                             positional storage to index, and it can only be walked forward.\n  \
                             help: it.collect() materializes a Vec[{elem_str}] you can \
                             index (O(n) once),\n        \
                             or it.nth(i) reads the i-th element directly \
                             — it returns Option[{elem_str}]"
                        ),
                        expr.span,
                        TypeErrorKind::IteratorNotIndexable,
                    );
                    return Type::Error;
                }
                let elem_result = match &obj_ty {
                    Type::Array { element, .. } => *element.clone(),
                    Type::Slice { element, .. } => *element.clone(),
                    // `Vector[T, N]` lane read `v[i] -> T` (design.md § Portable
                    // SIMD). Range indexing of a vector is not part of the v1
                    // surface, so it falls through to the range-error path above.
                    Type::Vector { element, lanes } => {
                        // Record the lane-read receiver, mirroring the
                        // method-call write in `infer_method_call`: the
                        // Index node shares the receiver's span and is
                        // about to overwrite it in `expr_types` with the
                        // element type, erasing the vector's `(T, N)` —
                        // which the signedness side-channel
                        // (`unsigned_vector_exprs`, fed from this table
                        // in lowering.rs) needs for `println(v[i])` on
                        // unsigned elements (2026-06-07).
                        if let Some(n) = lanes.as_usize() {
                            self.vector_method_receivers
                                .insert(SpanKey::from_span(&expr.span), ((**element).clone(), n));
                        }
                        *element.clone()
                    }
                    // `VecDeque` indexes exactly like `Vec` and was simply
                    // absent from this arm (B-2026-08-11-1), so `d[i]` inferred
                    // `Type::Error`. Most uses survive that — `d[0] + 1` and
                    // `let x: i64 = d[0]` both work, because they recover the
                    // type from the operator or the annotation — but METHOD
                    // DISPATCH cannot: an `Error` receiver records no
                    // `method_callee_types` entry, so codegen's `dispatch_key`
                    // is `None` and `d[0].to_string()` fell through to "no
                    // handler for method 'to_string'". The range-index arm
                    // above already pairs the two names; this brings scalar
                    // indexing in line.
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        args[0].clone()
                    }
                    // Peel an immutable/exclusive borrow before extracting the
                    // element type: integer-indexing a borrowed collection
                    // (`m[i]` where `m: ref Vec[Vec[T]]` / `mut ref Slice[T]`)
                    // must yield the inner element, not silently fall to the
                    // `_ => Error` arm. Without this a `let row = m[i]` binding
                    // infers `Type::Error`, which records no surface/element
                    // type and trips codegen's "no handler for method" on a
                    // later `row.len()` / `row[j]`. The range-index path above
                    // (and the Tensor/Column arms) already peel Ref/MutRef this
                    // way; this brings scalar integer indexing in line.
                    Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                        Type::Array { element, .. } => *element.clone(),
                        Type::Slice { element, .. } => *element.clone(),
                        Type::Vector { element, lanes } => {
                            if let Some(n) = lanes.as_usize() {
                                self.vector_method_receivers.insert(
                                    SpanKey::from_span(&expr.span),
                                    ((**element).clone(), n),
                                );
                            }
                            *element.clone()
                        }
                        // Same pairing behind a borrow — `d[i]` where
                        // `d: ref VecDeque[T]` (B-2026-08-11-1).
                        Type::Named { name, args }
                            if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                        {
                            args[0].clone()
                        }
                        _ => Type::Error,
                    },
                    Type::Error => Type::Error,
                    // B-2026-08-17-10 — this arm used to be a SILENT
                    // `Type::Error`, and that silence is the root defect the
                    // row is about. Probed on the parent, `s[0]` on a struct,
                    // a bool, an `i64`, an `Option` and a closure ALL printed
                    // "All checks passed" and then tripped the same
                    // `unreachable!` in the interpreter — the `Iterator` case
                    // the row was filed for is one instance of a class, not a
                    // one-off. Emitting here rejects the whole class at the
                    // phase that owns it, per the coding standard that every
                    // phase diagnoses with a span rather than leaving a
                    // backend to panic.
                    //
                    // An UNRESOLVED type stays silent: mid-inference the
                    // operand can still be a metavariable (a generic body
                    // typechecked before instantiation), and erroring there
                    // would reject correct programs. `Type::Error` keeps its
                    // own arm above so an upstream failure does not cascade
                    // into a second diagnostic.
                    other => {
                        if !contains_type_var(other) {
                            self.type_error(
                                format!(
                                    "'{}' does not support indexing with []\n  \
                                     help: indexable types are Array[T, N], Slice[T], \
                                     Vec[T], VecDeque[T],\n        \
                                     Map[K, V] (by key), and Vector[T, N] (lane read)",
                                    type_display(other)
                                ),
                                expr.span,
                                TypeErrorKind::TypeNotIndexable,
                            );
                        }
                        Type::Error
                    }
                };
                // B-2026-08-08-4 gap B (read-back) — a `weak T` ELEMENT read is
                // an UPGRADE, exactly as a `weak T` FIELD read is
                // (`infer_field_access`): `v[i]` on a `Vec[weak N]` yields
                // `Option[N]`, `Some` while a strong ref to the target still
                // exists and `None` once it is gone.
                //
                // Without this the element read handed back a bare `weak N`,
                // and `match v[i] { Some(x) => .. }` then TYPECHECKED while
                // binding nothing — codegen failed with `Undefined variable
                // 'x'` and the interpreter errored too. B-2026-08-08-5 landed
                // the store half of this container/field symmetry and left the
                // read half open, which is what made a `Vec[weak T]`
                // store-only: usable for cycle-breaking back-edges (they exist
                // not to be traversed) and not for a parent-pointer walk.
                //
                // A store LHS keeps the raw `weak T` place type so the
                // assignment path still coerces the RHS (the downgrade) — the
                // same `is_lhs` split the field read uses, and what keeps
                // `v[i] = strong_handle` compiling. Captured at the TOP of this
                // arm rather than read here: inferring the object runs
                // `infer_field_access`, which resets the flag.
                if !index_is_lhs {
                    if let Type::Weak(inner) = &elem_result {
                        // B-2026-08-08-14 — record the read for the interpreter
                        // (see the store twin in `infer_method_call`).
                        self.weak_elem_read_sites
                            .insert(SpanKey::from_span(&expr.span));
                        self.index_read_is_fresh_value
                            .insert(SpanKey::from_span(&expr.span));
                        return Type::Named {
                            name: "Option".to_string(),
                            args: vec![(**inner).clone()],
                        };
                    }
                }
                elem_result
            }

            // Compound
            ExprKind::Block(block) => self.infer_block(block),
            // `comptime { ... }` — the block runs at compile time and its
            // constant result is spliced in by the comptime fold pass
            // (`crate::comptime`, slice 2). For typing purposes the whole
            // expression has the inner block's type: the folded literal the
            // evaluator substitutes has exactly that type, so the surrounding
            // expression checks identically whether it sees the `comptime`
            // node or the folded constant. Spec: deferred.md § Comptime —
            // AST→AST `comptime fn`, "Implementation phases" substrate 1.
            //
            // The block body is a comptime context (substrate 2): a `Type`
            // pseudovalue (a bare type name used as a value) is legal here,
            // so bump `comptime_depth` for the duration of the block.
            ExprKind::Comptime(block) => {
                self.comptime_depth += 1;
                let ty = self.infer_block(block);
                self.comptime_depth -= 1;
                // Substrate 3: when the block yields an `Expr` AST value (a
                // quasi-quote like `ast.expr("x * 3")`), the fold pass splices
                // the *generated code* — not an `Expr`-typed value — at this
                // site. Its type is whatever the spliced code evaluates to,
                // which can't be known before evaluation, so hand back a fresh
                // inference var: an annotation or downstream use constrains it,
                // and the interpreter (dynamically typed) does the real work.
                if matches!(&ty, Type::Named { name, .. } if name == "Expr") {
                    self.env.fresh_type_var()
                } else {
                    ty
                }
            }

            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                let cond_ty = self.infer_expr(condition);
                if cond_ty != Type::Bool && cond_ty != Type::Error {
                    self.type_error(
                        format!(
                            "condition must be 'bool', found '{}'",
                            type_display(&cond_ty)
                        ),
                        condition.span,
                        TypeErrorKind::ConditionNotBool,
                    );
                }
                let then_ty = self.infer_block(then_block);
                if let Some(ref else_expr) = else_branch {
                    let else_ty = self.infer_expr(else_expr);
                    if then_ty == Type::Never {
                        return else_ty;
                    }
                    if else_ty == Type::Never {
                        return then_ty;
                    }
                    match self.join_branch_types(&then_ty, &else_ty) {
                        Some(joined) => joined,
                        None => {
                            if then_ty != Type::Error && else_ty != Type::Error {
                                self.type_error(
                                    format!(
                                        "if/else branches have incompatible types: '{}' and '{}'",
                                        type_display(&then_ty),
                                        type_display(&else_ty)
                                    ),
                                    expr.span,
                                    TypeErrorKind::BranchTypeMismatch,
                                );
                            }
                            then_ty
                        }
                    }
                } else {
                    Type::Unit
                }
            }

            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                let scrut_ty = self.infer_expr(value);
                // Bind the pattern's variables for the duration of the
                // then-block so identifier-leaf bindings inside if-let
                // (e.g. `if let Some(l) = cur.left { queue.push_back(l); }`)
                // get the right scrutinee-derived type. Without this the
                // pattern's bindings stay un-typed (silent fall-through
                // to `Type::Error`), which breaks downstream
                // `pattern_binding_types` recording, codegen's
                // `var_type_names` propagation, and method dispatch.
                let (mode, dispatch_ty) = ScrutineeMode::classify(&scrut_ty);
                let dispatch_ty = dispatch_ty.clone();
                self.local_scope.push();
                self.check_pattern_against(pattern, &dispatch_ty, mode);
                let then_ty = self.infer_block(then_block);
                self.local_scope.pop();
                if let Some(ref else_expr) = else_branch {
                    let else_ty = self.infer_expr(else_expr);
                    if then_ty == Type::Never {
                        return else_ty;
                    }
                    if else_ty == Type::Never {
                        return then_ty;
                    }
                    match self.join_branch_types(&then_ty, &else_ty) {
                        Some(joined) => joined,
                        None => {
                            if then_ty != Type::Error && else_ty != Type::Error {
                                self.type_error(
                                    format!(
                                        "if let/else branches have incompatible types: '{}' and '{}'",
                                        type_display(&then_ty),
                                        type_display(&else_ty)
                                    ),
                                    expr.span,
                                    TypeErrorKind::BranchTypeMismatch,
                                );
                            }
                            then_ty
                        }
                    }
                } else {
                    Type::Unit
                }
            }

            ExprKind::Match { scrutinee, arms } => self.infer_match(scrutinee, arms, &expr.span),

            ExprKind::While {
                condition,
                body,
                label,
                ..
            } => {
                let cond_ty = self.infer_expr(condition);
                if cond_ty != Type::Bool && cond_ty != Type::Error {
                    self.type_error(
                        format!(
                            "while condition must be 'bool', found '{}'",
                            type_display(&cond_ty)
                        ),
                        condition.span,
                        TypeErrorKind::ConditionNotBool,
                    );
                }
                // A valueless frame: it catches unlabeled `break`s so they
                // cannot leak out to an enclosing `loop`'s LUB, but rejects
                // a value (design.md: `while` / `for` always have type `()`).
                self.break_value_types
                    .push(BreakFrame::for_valueless_loop(label.clone()));
                self.infer_block(body);
                self.break_value_types.pop();
                Type::Unit
            }

            ExprKind::For {
                pattern,
                iterable,
                body,
                label,
                ..
            } => {
                let iter_ty = self.infer_expr(iterable);
                self.local_scope.push();
                // Resolve element type via IntoIterator.Item (impl_assoc_types),
                // covering Vec, Map, SortedSet, Set, Slice, Array, Range* and
                // any user type that has registered an "Item" assoc binding.
                let elem_ty = self.element_type_of(&iter_ty);
                self.bind_pattern_types(pattern, &elem_ty);
                // See the `While` arm: valueless frame, so an unlabeled
                // `break` stops here instead of reaching an outer `loop`.
                self.break_value_types
                    .push(BreakFrame::for_valueless_loop(label.clone()));
                for stmt in &body.stmts {
                    self.check_stmt(stmt);
                }
                if let Some(ref final_expr) = body.final_expr {
                    self.infer_expr(final_expr);
                }
                self.break_value_types.pop();
                self.local_scope.pop();
                Type::Unit
            }

            ExprKind::Loop { body, label, .. } => {
                // design.md § `loop` type inference. A `loop` is an
                // EXPRESSION: its type is the LUB of its reachable
                // `break expr` values, and `Never` only when it has none
                // (it runs forever or diverges). This arm used to return
                // `Never` unconditionally, which is B-2026-08-24-10: the
                // break value was dropped, `-> i64` went unchecked, and
                // codegen — trusting `Never` — emitted `unreachable` at
                // the loop exit, so LLVM deleted the exit edge and the
                // compiled binary HUNG where the interpreter returned `()`.
                //
                // The tail type is `Never`, not the body's: falling off the
                // end of a loop body starts the next iteration, it does not
                // produce a value.
                self.break_value_types
                    .push(BreakFrame::for_loop(label.clone()));
                self.infer_block(body);
                let frame = self.break_value_types.pop();
                let values = frame.map(|f| f.values).unwrap_or_default();
                self.check_break_values_agree(&values, expr.span);
                lub_block_type(Type::Never, &values)
            }

            ExprKind::LabeledBlock { label, body, .. } => {
                // LB3 — push a fresh per-label collector frame, infer the
                // body's tail type, pop the frame, and compute the block's
                // type as the LUB of `tail_type` and the collected
                // `break label expr` value types.
                self.break_value_types
                    .push(BreakFrame::for_labeled_block(label.clone()));
                let tail_ty = self.infer_block(body);
                let frame = self
                    .break_value_types
                    .pop()
                    .map(|f| f.values)
                    .unwrap_or_default();
                self.check_break_values_agree(&frame, expr.span);
                lub_block_type(tail_ty, &frame)
            }

            ExprKind::Closure {
                params,
                capture_mode,
                prefix_span: _,
                body,
            } => {
                // Round 12.44 (Step 2) — once-callability inference at construction.
                // Snapshot the OUTER local scope before pushing the closure's
                // own param scope so the body walker can identify which
                // identifiers refer to outer bindings (captures).
                let outer_bindings = self.flatten_local_scope_snapshot();
                let closure_param_names: Vec<String> = params
                    .iter()
                    .flat_map(|p| p.pattern.binding_names())
                    .collect();
                // LB4 — closure-boundary rule for the LUB collector. A
                // `break label` inside a closure body cannot target an
                // enclosing labeled block (the resolver rejects it as
                // `undefined loop label`), but we still save/restore the
                // collector stack defensively so an inner labeled-block
                // frame doesn't leak across closure bodies if the
                // resolver's check is bypassed (e.g., during
                // single-phase typechecker tests). Closure bodies start
                // with a fresh empty stack; restored on exit.
                let saved_break_values = std::mem::take(&mut self.break_value_types);
                // B-2026-07-15-16 — pre-seeds for un-annotated params, published
                // by a direct collection/Result method dispatch (`retain`,
                // `and_then`, …) that knows the param type from the receiver but
                // wants free body inference. Consumed here (removed) so a nested
                // closure at the same span can't re-read a stale seed.
                let param_seeds = self
                    .closure_param_seeds
                    .remove(&SpanKey::from_span(&expr.span));
                self.local_scope.push();
                let param_types: Vec<Type> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let annotated = p.ty.as_ref().map(|t| self.lower_type_expr(t, &[]));
                        let seeded = param_seeds.as_ref().and_then(|s| s.get(i).cloned());
                        // B-2026-08-08-24 — the annotation still wins, but the
                        // seed is no longer discarded UNREAD when both exist:
                        // an owned annotation over a borrowed payload is a type
                        // error, not a silent reinterpretation in codegen.
                        if let (Some(ann), Some(seed), Some(ty_expr)) =
                            (&annotated, &seeded, p.ty.as_ref())
                        {
                            self.check_closure_param_annotation_against_seed(ann, seed, ty_expr);
                        }
                        let ty = annotated
                            .or(seeded)
                            .unwrap_or_else(|| self.env.fresh_type_var());
                        if !self.is_irrefutable_pattern(&p.pattern, &ty) {
                            self.type_error(
                                "refutable pattern in closure parameter; use `if let` or `match` for patterns that may not match".to_string(),
                                p.pattern.span,
                                TypeErrorKind::RefutablePattern,
                            );
                        }
                        self.bind_pattern_types(&p.pattern, &ty);
                        ty
                    })
                    .collect();
                // B-2026-07-31-18 — closure-scoped `return`: push a collector
                // frame so `return E` inside THIS body records E's type here
                // (the Return arm's collector path) instead of checking
                // against the enclosing FN's return type. Per design.md
                // (§ with_provider signature: the body is `Fn() -> T`) and
                // the interpreter/codegen, a closure's `return` exits the
                // closure — its type belongs to the closure's return type.
                self.closure_return_types
                    .push(super::ClosureReturnFrame::default());
                let body_ty = self.infer_expr(body);
                let collected = self
                    .closure_return_types
                    .pop()
                    .expect("closure return collector pushed above");
                self.local_scope.pop();
                self.break_value_types = saved_break_values;
                // Resolve any closure param inference vars the body solved
                // (pull-side inference, B-2026-07-12-10) so the closure's
                // `Function` type carries the concrete param type — `Fn(i64) ->
                // i64` for `|x| x + 1`, not an unsolved `?T0`. Identity for
                // annotated params and for the push-side case (already concrete).
                let param_types: Vec<Type> = param_types
                    .iter()
                    .map(|t| resolve_type_var_top(t, &self.env.substitutions))
                    .collect();
                let body_ty = resolve_type_var_top(&body_ty, &self.env.substitutions);
                // B-2026-07-31-18 — the closure's return type is the
                // unification of its tail type with every closure-scoped
                // `return E` collected above. A `!`-typed tail (the body
                // ends in a return/break) contributes nothing; conflicting
                // concrete types are a hard error at the closure, matching
                // how a fn body's tail-vs-return mismatch reports.
                let body_ty = {
                    let mut ret_ty = match body_ty {
                        Type::Never => None,
                        ref t => Some(t.clone()),
                    };
                    for (t, _span) in collected.returns {
                        let t = resolve_type_var_top(&t, &self.env.substitutions);
                        if matches!(t, Type::Never | Type::Error) {
                            continue;
                        }
                        match &ret_ty {
                            None => ret_ty = Some(t),
                            Some(cur) if *cur != t && *cur != Type::Error => {
                                self.type_error(
                                    format!(
                                        "closure returns conflicting types: '{}' vs '{}'",
                                        type_display(cur),
                                        type_display(&t)
                                    ),
                                    expr.span,
                                    TypeErrorKind::ReturnTypeMismatch,
                                );
                            }
                            _ => {}
                        }
                    }
                    ret_ty.unwrap_or(Type::Never)
                };
                // B-2026-07-31-19 — solve the closure's Result/Option shape
                // from the `?` demands its body raised. A `?` on Err returns
                // `Err(e)` from the CLOSURE, so a body like
                // `|| { let v = f(x)?; Ok(v * 10) }` (whose bare `Ok` tail
                // left the Err side an unbound param) is pinned to
                // `Result[T, E]` with E from the `?` operands.
                let body_ty = self.solve_closure_question_demands(
                    body_ty,
                    collected.question_errs,
                    collected.question_option,
                    &expr.span,
                );
                self.closure_type_with_capture_inference(
                    &expr.span,
                    *capture_mode,
                    &closure_param_names,
                    body,
                    &outer_bindings,
                    param_types,
                    body_ty,
                )
            }

            ExprKind::Return(inner) => {
                // B-2026-07-31-18 — inside a closure literal body, `return E`
                // returns from the CLOSURE (design.md § with_provider
                // signature: the body is `Fn() -> T`; the interpreter and
                // codegen both scope it to the closure). Record E's type in
                // the innermost collector for post-body unification instead
                // of checking against the enclosing FN's return type.
                // (`?` deliberately stays on the fn-level path below —
                // its closure-scoped typing is B-2026-07-31-19's scope.)
                if !self.closure_return_types.is_empty() {
                    let t = match inner {
                        Some(ref expr) => self.infer_expr(expr),
                        None => Type::Unit,
                    };
                    let span = inner.as_ref().map(|e| e.span).unwrap_or_else(|| expr.span);
                    self.closure_return_types
                        .last_mut()
                        .expect("checked non-empty above")
                        .returns
                        .push((t, span));
                    return Type::Never;
                }
                if let Some(ref expr) = inner {
                    // B-2026-09-03-20 — `return w.r;` is a value position
                    // exactly as a `let` initializer is, and was one of the
                    // five that escaped the rule while it was site-based.
                    // Measured on a LOCAL root: two `R` bodies for one value,
                    // and the second one diverged run-vs-build (the
                    // interpreter's ran over the LIVE field, both compiled
                    // backends' over the ZEROED one).
                    let ret_ty = if let Some(ref ret_ty) = self.current_return_type.clone() {
                        self.check_expr(expr, ret_ty);
                        ret_ty.clone()
                    } else {
                        self.infer_expr(expr)
                    };
                    self.warn_partial_move_of_drop_struct(expr, &ret_ty);
                } else if let Some(ref ret_ty) = self.current_return_type.clone() {
                    if *ret_ty != Type::Unit && *ret_ty != Type::Error {
                        self.type_error(
                            format!("expected return value of type '{}'", type_display(ret_ty)),
                            expr.span,
                            TypeErrorKind::ReturnTypeMismatch,
                        );
                    }
                }
                Type::Never
            }

            ExprKind::Break { label, value } => {
                let val_ty = if let Some(ref e) = value {
                    self.infer_expr(e)
                } else {
                    Type::Unit
                };
                // Resolve the target frame: a labeled `break` names its
                // frame (innermost wins); an unlabeled one takes the
                // innermost frame that accepts unlabeled breaks — every
                // loop form, but never a labeled block, which is reachable
                // only by name (design.md § Labeled blocks).
                let target = match label {
                    Some(name) => self
                        .break_value_types
                        .iter_mut()
                        .rev()
                        .find(|f| f.label.as_deref() == Some(name.as_str())),
                    None => self
                        .break_value_types
                        .iter_mut()
                        .rev()
                        .find(|f| f.unlabeled_target),
                };
                if let Some(frame) = target {
                    if frame.accepts_value {
                        frame.values.push(val_ty);
                    } else if val_ty != Type::Unit && val_ty != Type::Error {
                        // design.md § `break expr`: `while` / `while let` /
                        // `for` always have type `()`, so a value here has
                        // nowhere to go. Reported rather than silently
                        // dropped — dropping it is the class of bug this
                        // whole change exists to remove.
                        let what = frame
                            .label
                            .clone()
                            .map(|l| format!("`{l}`"))
                            .unwrap_or_else(|| "the enclosing loop".to_string());
                        self.type_error(
                            format!(
                                "`break` with a value of type '{}' is not allowed here: \
                                 {what} is a `while`/`for` loop, which always has type '()'. \
                                 Only `loop` and labeled blocks can break with a value",
                                type_display(&val_ty)
                            ),
                            expr.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Never
            }
            ExprKind::Continue { .. } => Type::Never,

            ExprKind::Tuple(exprs) => {
                // The empty-tuple literal `()` IS the unit value — canonicalize
                // it to `Type::Unit` so it matches the `()` *type* annotation
                // (which lowers to `Type::Unit`). Without this, `Some(())` /
                // `Ok(())` / `fn f() -> Result[(), E] { Ok(()) }` infer a
                // `Type::Tuple(vec![])` payload that prints identically to `()`
                // but is not `types_compatible` with `Type::Unit`, producing the
                // baffling `expected 'Option<()>', found 'Option<()>'` mismatch.
                if exprs.is_empty() {
                    Type::Unit
                } else {
                    let types: Vec<Type> = exprs.iter().map(|e| self.infer_expr(e)).collect();
                    // B-2026-09-03-20 — each element is consumed by value, so
                    // `(w.r, 1)` moves `r` out of an own-`Drop` `W` exactly as
                    // `let x = w.r;` does. Measured: two bodies, and the second
                    // diverged run-vs-build.
                    for (e, t) in exprs.iter().zip(types.iter()) {
                        self.warn_partial_move_of_drop_struct(e, t);
                    }
                    Type::Tuple(types)
                }
            }

            ExprKind::StructLiteral {
                path,
                fields,
                spread,
                generic_args,
            } => {
                // Slice 2c — FFI union literal arm. Unions share the
                // `Name { field: value, ... }` shape with struct
                // literals but have distinct construction rules
                // (exactly one field, no spread, no missing-field
                // recovery), so they branch off before
                // `infer_struct_literal` runs.
                let target_name = path.last().cloned().unwrap_or_default();
                if self.env.unions.contains_key(&target_name) {
                    return self.infer_union_literal(
                        &target_name,
                        fields,
                        spread.as_deref(),
                        &expr.span,
                    );
                }
                // Enum struct-variant construction `Enum.Variant { ... }`:
                // when the second-to-last segment names a known enum whose
                // `Variant` is struct-shaped, route to enum-variant inference
                // (else `infer_struct_literal` looks up `Variant` as a struct
                // and rejects "not a struct"). See `enum_struct_variant_fields`.
                if path.len() >= 2 {
                    let enum_name = path[path.len() - 2].clone();
                    if let Some(decl_fields) =
                        self.enum_struct_variant_fields(&enum_name, &target_name)
                    {
                        if let Some(ref spread_expr) = spread {
                            self.infer_expr(spread_expr);
                        }
                        return self.infer_enum_struct_variant_literal(
                            &enum_name,
                            &target_name,
                            &decl_fields,
                            fields,
                            &expr.span,
                        );
                    }
                }
                // Unqualified struct-variant construction `Variant { ... }`:
                // the parser produces a single-segment `StructLiteral` path
                // identical to a plain struct literal, so `target_name` is the
                // bare variant name. The resolver has already bound it to its
                // `EnumVariant` symbol; recover the parent enum from that
                // resolution and route to enum-variant inference (otherwise
                // `infer_struct_literal` looks `Variant` up as a struct and
                // rejects "not a struct"). Mirrors the qualified arm above and
                // the unqualified pattern-binding path. See
                // `unqualified_enum_struct_variant`.
                if path.len() == 1 {
                    if let Some((enum_name, decl_fields)) =
                        self.unqualified_enum_struct_variant(&expr.span, &target_name)
                    {
                        if let Some(ref spread_expr) = spread {
                            self.infer_expr(spread_expr);
                        }
                        return self.infer_enum_struct_variant_literal(
                            &enum_name,
                            &target_name,
                            &decl_fields,
                            fields,
                            &expr.span,
                        );
                    }
                }
                if let Some(ref spread_expr) = spread {
                    // Inferred for its own diagnostics' sake; the base itself
                    // goes nowhere (see `infer_struct_literal_with_spread`).
                    self.infer_expr(spread_expr);
                    return self.infer_struct_literal_with_spread(path, fields, &expr.span);
                }
                // Explicit generic arguments written AT the literal
                // (`Connection[Disconnected] { socket: ... }`). Seed them as
                // the expected type args, which is the same channel the
                // annotated form (`let c: C[S] = C { .. }`) already uses.
                //
                // This is what makes a PHANTOM parameter work without an
                // annotation: `S` appears in no field, so the field values
                // cannot solve it and inference reports "cannot infer type
                // parameter". What the programmer wrote is the only source of
                // truth for it. B-2026-08-24-3.
                if let Some(args) = generic_args {
                    if let Some(lowered) =
                        self.lower_literal_generic_args(&target_name, args, &expr.span)
                    {
                        return self.infer_struct_literal_expected(
                            path,
                            fields,
                            &expr.span,
                            Some(&lowered),
                            false,
                        );
                    }
                }
                self.infer_struct_literal(path, fields, &expr.span)
            }

            ExprKind::Cast { expr: inner, ty } => {
                let from_ty = self.infer_expr(inner);
                let to_ty = self.lower_type_expr(ty, &[]);
                self.check_cast_pair(&from_ty, &to_ty, &inner.span);
                // B-2026-08-14-3 — record whether the SOURCE is unsigned, so
                // codegen can pick zext over sext on the widening lane. It
                // cannot read that off `expr_types`: the parser gives a `Cast`
                // its operand's span verbatim, so the cast's own (target) type
                // is the last write at that key and the operand's type is gone.
                // Codegen's syntactic fallback covers a concretely-spelled
                // operand; this table is what covers a GENERIC one, where the
                // declared return type is a bare `T`.
                if matches!(
                    resolve_type_var_top(&from_ty, &self.env.substitutions),
                    Type::UInt(_)
                ) {
                    self.cast_source_unsigned
                        .insert(crate::resolver::SpanKey::from_span(&expr.span));
                }
                to_ty
            }

            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let start_ty = start.as_deref().map(|e| self.infer_expr(e));
                let end_ty = end.as_deref().map(|e| self.infer_expr(e));
                // When both bounds are present, verify they share a type.
                if let (Some(ref s), Some(ref e)) = (&start_ty, &end_ty) {
                    if !self.types_compatible_with_projections(s, e)
                        && *s != Type::Error
                        && *e != Type::Error
                    {
                        self.type_error(
                            format!(
                                "range bounds must have same type: '{}' and '{}'",
                                type_display(s),
                                type_display(e)
                            ),
                            expr.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                // Synthesise the appropriate Range variant.
                let elem_ty = start_ty.or(end_ty).unwrap_or(Type::Int(IntSize::I64));
                let name = match (start.is_some(), end.is_some(), inclusive) {
                    (true, true, false) => "Range",
                    (true, true, true) => "RangeInclusive",
                    (true, false, _) => "RangeFrom",
                    (false, true, false) => "RangeTo",
                    (false, true, true) => "RangeToInclusive",
                    (false, false, _) => "RangeFull",
                };
                if name == "RangeFull" {
                    Type::Named {
                        name: "RangeFull".to_string(),
                        args: vec![],
                    }
                } else {
                    Type::Named {
                        name: name.to_string(),
                        args: vec![elem_ty],
                    }
                }
            }

            ExprKind::Unsafe(block) => {
                // Track lexical unsafe depth so use-site rules like
                // `E_UNION_READ_REQUIRES_UNSAFE` (slice 2a) and the
                // upcoming borrow / literal gates can read a single
                // flag rather than each implementing their own walker.
                self.unsafe_depth += 1;
                let ty = self.infer_block(block);
                self.unsafe_depth -= 1;
                ty
            }

            ExprKind::Try(block) => {
                // v1 stub — typechecker pipeline (?-retargeting against
                // the block, error-type unification, From-chain coercion)
                // lands in P1 per design.md § Error Handling > Try Blocks.
                // We still type-check inner expressions so unrelated
                // errors inside the body still surface; the block's
                // overall type is the error sentinel.
                self.infer_block(block);
                self.type_error(
                    "error[E_TRY_BLOCK_NOT_IMPLEMENTED_YET]: try block syntax \
                     is recognized but the typechecker pipeline lands in P1 \
                     — extract the body into a helper function returning \
                     Result for now"
                        .to_string(),
                    expr.span,
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }

            ExprKind::WhileLet {
                pattern,
                value,
                body,
                label,
                ..
            } => {
                let scrut_ty = self.infer_expr(value);
                // Bind the pattern's variables for the duration of the loop
                // body, mirroring `if let` — without this the bindings stay
                // un-typed (silent fall-through to `Type::Error`), breaking
                // `pattern_binding_types` recording and codegen's binding-type
                // propagation for `while let Some(x) = … { … x … }`.
                let (mode, dispatch_ty) = ScrutineeMode::classify(&scrut_ty);
                let dispatch_ty = dispatch_ty.clone();
                self.local_scope.push();
                self.check_pattern_against(pattern, &dispatch_ty, mode);
                // See the `While` arm.
                self.break_value_types
                    .push(BreakFrame::for_valueless_loop(label.clone()));
                self.infer_block(body);
                self.break_value_types.pop();
                self.local_scope.pop();
                Type::Unit
            }

            ExprKind::Seq(block) => self.infer_block(block),
            ExprKind::Par(block) => {
                // Phase 6 line 170 slice 3b — cross-task-safe boundary
                // check: every binding the parallel branches read from the
                // enclosing scope crosses a task boundary. Run before the
                // branch bindings enter the enclosing scope so the snapshot
                // is the pre-par scope.
                self.check_cross_task_safe_par_block(block, &expr.span);
                // The join barrier hoists each branch's top-level `let` into
                // the ENCLOSING scope (no fresh block scope, unlike
                // `infer_block`) so the bindings are live after the `par {}`
                // statement — the shape `par { let a = f(); let b = g(); }
                // (a, b)` needs. Mirrors the resolver's hoisting and the
                // auto-parallelizer's enclosing-scope grouped locals
                // (B-2026-07-11-3). Sibling isolation is already enforced by
                // the resolver, so branch reads are known-valid here.
                for stmt in &block.stmts {
                    self.check_stmt(stmt);
                }
                if let Some(ref tail) = block.final_expr {
                    self.infer_expr(tail)
                } else {
                    Type::Unit
                }
            }

            ExprKind::Lock { mutex, alias, body } => {
                // `lock <place> [alias] { body }` — acquire the `Mutex[T]` named
                // by `place` (a binding `m` or a field `self.state`), expose its
                // inner `T` as a mutable binding for the body, release on exit.
                // The body's value is the block's value. (design.md § Part 5:
                // Shared Types > `lock` blocks.)
                //
                // Infer the place's type and the inner `T`. `Mutex[T]` lowers to
                // `Type::Named { "Mutex", [T] }`; a field access on a `par` /
                // `shared` struct yields the field type directly.
                let mutex_ty = self.infer_expr(mutex);
                let inner = match &mutex_ty {
                    Type::Named { name, args } if name == "Mutex" && args.len() == 1 => {
                        Ok(args[0].clone())
                    }
                    // A borrowed mutex (`ref`/`mut ref Mutex[T]` parameter) —
                    // codegen loads through the reference to reach the
                    // `{ lockflag, value }` aggregate (the pointee struct type
                    // is recovered from `ref_params`).
                    Type::Ref(b) | Type::MutRef(b)
                        if matches!(b.as_ref(),
                            Type::Named { name, args } if name == "Mutex" && args.len() == 1) =>
                    {
                        match b.as_ref() {
                            Type::Named { args, .. } => Ok(args[0].clone()),
                            _ => unreachable!("guarded by the matches! above"),
                        }
                    }
                    // `Type::Error` (unresolved place) is tolerated silently —
                    // the resolver already reported any undefined name.
                    Type::Error => Ok(Type::Error),
                    _ => Err(format!(
                        "`lock` target must be a `Mutex[T]`, found `{}`",
                        type_display(&mutex_ty)
                    )),
                };
                let inner = match inner {
                    Ok(t) => t,
                    Err(msg) => {
                        self.type_error(msg, expr.span, TypeErrorKind::LockTargetNotMutex);
                        Type::Error
                    }
                };
                // The body needs a name for the inner value. With an explicit
                // `alias` it's that name; without one, an `Identifier` place's
                // own name is shadowed. A field place (`self.state`) has no name
                // to shadow, so an alias is required.
                let bind_name = match (alias.clone(), &mutex.kind) {
                    (Some(a), _) => Some(a),
                    (None, ExprKind::Identifier(n)) => Some(n.clone()),
                    (None, _) => {
                        self.type_error(
                            "a `lock` on a field (e.g. `lock self.state`) requires an alias: \
                             write `lock self.state s { … }` and use `s` for the inner value"
                                .to_string(),
                            expr.span,
                            TypeErrorKind::LockTargetNotMutex,
                        );
                        None
                    }
                };
                // Early exits (`return` / `break` / `continue`) out of a lock
                // body are legal: codegen seeds the lock release as a
                // `CleanupAction::ReleaseMutex` on the body's scope-cleanup
                // frame, so every exit path (fall-through, break/continue,
                // return) releases the lock on the way out. (The old
                // `LockEarlyExit` / `E0259` rejection was retired with that
                // codegen change.)
                // Bind the inner-value name to `T` so `name = v` / `name.f = v` /
                // `name += 1` typecheck against `T`. The binding lives only for
                // the body's scope.
                self.local_scope.push();
                if let Some(name) = bind_name {
                    self.local_scope.insert(name, inner);
                }
                let ty = self.infer_block(body);
                self.local_scope.pop();
                ty
            }

            ExprKind::Providers { bindings, body } => {
                // Provider values are plain expressions; infer their types
                // for side effects (diagnostics, subexpression typing). The
                // block's type is the body's type. Provider-trait
                // conformance is checked below, as in the call form.
                //
                // Phase 6 line 170 slice 3c — cross-task-safe check on the
                // concrete provider type. A `with_provider[R](p, || …)`
                // provider is shared with the closure body, which may run
                // across spawned tasks, so a provider whose type reaches a
                // not-cross-task-safe leaf is rejected at the call site
                // (design.md line 7213 + § Structured Concurrency Lifetime
                // Guarantees: with_provider is one of the five boundary
                // sites). No sole-ownership carve-out — the full unsafe set
                // is rejected, shared struct/enum included. This replaces
                // the historical "Send + Sync on the provider type" deferral
                // (the closed enumeration is the v1 mechanism, no auto-trait
                // infrastructure to wait on).
                //
                // B-2026-08-19-15 — the declared-provider-trait check runs
                // here too. `providers { R => p } in { … }` DESUGARS to
                // `with_provider[R](p, || …)` (design.md § Provider-Rooted
                // Resources), so a provider that does not implement `R`'s
                // declared bound is exactly as broken in this form: codegen
                // builds the same resource vtable from the same bound's
                // methods either way. B-2026-08-19-4 wired the check into the
                // call form only, which left the block form — the shape the
                // spec's own examples use for multi-resource setup — silently
                // accepting a provider implementing nothing at all.
                for b in bindings {
                    let provider_ty = self.infer_expr(&b.value);
                    if let Err(path) =
                        is_cross_task_safe_with(&provider_ty, &self.env.structs, &self.env.enums)
                    {
                        let descr = format!("provider for resource `{}`", b.resource);
                        self.emit_cross_task_unsafe_value(
                            &descr,
                            &provider_ty,
                            &path,
                            &b.resource_span,
                        );
                    }
                    self.check_provider_satisfies_declared_bound(
                        &b.resource,
                        &provider_ty,
                        &b.resource_span,
                    );
                }
                self.infer_block(body)
            }

            ExprKind::ArrayLiteral(elements) => {
                // Bare `[...]` defaults to `Vec[T]` in synthesis mode.
                // Use check_expr when an Array annotation is present (handled in check_expr).
                if elements.is_empty() {
                    Type::Named {
                        name: "Vec".to_string(),
                        args: vec![Type::Error],
                    }
                } else {
                    let first_ty = self.infer_expr(&elements[0]);
                    for elem in &elements[1..] {
                        let elem_ty = self.infer_expr(elem);
                        self.check_assignable(&first_ty, &elem_ty, elem.span);
                    }
                    Type::Named {
                        name: "Vec".to_string(),
                        args: vec![first_ty],
                    }
                }
            }

            ExprKind::PrefixCollectionLiteral { type_name, items } => {
                // Empty prefix-literal in synthesis mode — no element type
                // to infer. Check-mode (`let v: Vec[T] = Vec[]`, typed call
                // arguments, typed struct-field initializers) intercepts
                // earlier in `check_expr` and recovers via the expected
                // type. Anything that reaches this branch had no annotation
                // and gets the focused
                // `E_EMPTY_PREFIX_LITERAL_NEEDS_ANNOTATION` diagnostic per
                // design.md § Collection Literals.
                if items.is_empty() {
                    self.report_empty_prefix_literal(type_name, &expr.span);
                    return match type_name.as_str() {
                        "Array" => Type::Array {
                            element: Box::new(Type::Error),
                            size: ConstArg::Literal(0),
                        },
                        _ => Type::Named {
                            name: type_name.clone(),
                            args: vec![Type::Error],
                        },
                    };
                }
                match type_name.as_str() {
                    "Array" => {
                        let first_ty = self.infer_expr(&items[0]);
                        for item in &items[1..] {
                            let ty = self.infer_expr(item);
                            self.check_assignable(&first_ty, &ty, item.span);
                        }
                        Type::Array {
                            element: Box::new(first_ty),
                            size: ConstArg::Literal(items.len() as i64),
                        }
                    }
                    "Vec" => {
                        let first_ty = self.infer_expr(&items[0]);
                        for item in &items[1..] {
                            let ty = self.infer_expr(item);
                            self.check_assignable(&first_ty, &ty, item.span);
                        }
                        Type::Named {
                            name: "Vec".to_string(),
                            args: vec![first_ty],
                        }
                    }
                    "Set" => {
                        let first_ty = self.infer_expr(&items[0]);
                        for item in &items[1..] {
                            let ty = self.infer_expr(item);
                            self.check_assignable(&first_ty, &ty, item.span);
                        }
                        Type::Named {
                            name: "Set".to_string(),
                            args: vec![first_ty],
                        }
                    }
                    other => {
                        // Map's `Map[k: v, ...]` form goes through
                        // `ExprKind::MapLiteral` separately; this arm
                        // catches future prefix-literal types and the
                        // `Map[v1, v2, ...]` (positional-only, no `:`) shape
                        // — which the parser does not emit today but is
                        // future-compatible.
                        let first_ty = self.infer_expr(&items[0]);
                        for item in &items[1..] {
                            self.infer_expr(item);
                        }
                        Type::Named {
                            name: other.to_string(),
                            args: vec![first_ty],
                        }
                    }
                }
            }

            ExprKind::RepeatLiteral {
                type_name,
                value,
                count,
            } => {
                let elem_ty = self.infer_expr(value);
                let count_ty = self.infer_expr(count);
                // Count must be an integer type; report otherwise but keep going.
                let count_is_int = matches!(count_ty, Type::Int(_) | Type::UInt(_) | Type::Error);
                if !count_is_int {
                    self.type_error(
                        format!(
                            "repeat-literal count must be an integer, found '{}'",
                            type_display(&count_ty)
                        ),
                        count.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                match type_name.as_deref() {
                    Some("Array") => {
                        // `Array[v; n]` requires a compile-time integer literal.
                        let size = match &count.kind {
                            ExprKind::Integer(n, _) if *n >= 0 => *n as usize,
                            _ => {
                                self.type_error(
                                    "Array[v; n] requires n to be a non-negative integer literal"
                                        .to_string(),
                                    count.span,
                                    TypeErrorKind::TypeMismatch,
                                );
                                0
                            }
                        };
                        Type::Array {
                            element: Box::new(elem_ty),
                            size: ConstArg::Literal(size as i64),
                        }
                    }
                    None | Some("Vec") => {
                        // Bare `[v; n]` defaults to `Vec[T]` in synthesis mode
                        // (check_expr coerces against `Array[T, N]` when an
                        // array annotation is present).
                        Type::Named {
                            name: "Vec".to_string(),
                            args: vec![elem_ty],
                        }
                    }
                    Some(other) => {
                        self.type_error(
                            format!(
                                "{}[v; n] is not supported; repeat literals only apply to `Vec` and `Array`",
                                other
                            ),
                            expr.span,
                            TypeErrorKind::TypeMismatch,
                        );
                        Type::Error
                    }
                }
            }

            ExprKind::MapLiteral(entries) => {
                let (first_key, first_val) = &entries[0];
                let key_ty = self.infer_expr(first_key);
                let val_ty = self.infer_expr(first_val);
                for (k, v) in &entries[1..] {
                    let kt = self.infer_expr(k);
                    let vt = self.infer_expr(v);
                    self.check_assignable(&key_ty, &kt, k.span);
                    self.check_assignable(&val_ty, &vt, v.span);
                }
                // `Map`, not `HashMap`. `HashMap` is the RUST representation
                // (design.md § type mapping: `Map[K, V]` -> `HashMap<K, V>`),
                // not a name Kāra source can write — `let m: HashMap[K, V]`
                // is rejected with `undefined type 'HashMap'`. Producing it
                // here gave a map literal a type no annotation could match, so
                // `let m: Map[String, i64] = Map["x": 1];` and a literal in a
                // `Map`-typed struct field both failed with `expected
                // 'Map<String, i64>', found 'HashMap<String, i64>'`. Every
                // downstream consumer already matches `"Map" | "HashMap"`.
                Type::Named {
                    name: "Map".to_string(),
                    args: vec![key_ty, val_ty],
                }
            }

            ExprKind::PipePlaceholder => {
                self.type_error(
                    "'_' placeholder is only valid inside a pipe expression argument list"
                        .to_string(),
                    expr.span,
                    TypeErrorKind::InvalidPipePlaceholder,
                );
                Type::Error
            }

            ExprKind::OffsetOf { ty, field_path } => {
                self.infer_offset_of(ty, field_path, &expr.span)
            }

            ExprKind::Error => Type::Error,
        }
    }
}

/// Line 549 slice 2b — translate a callee's formal parameter type into
/// the `borrow_context` string consumed by `infer_field_access`. Only
/// the immediate `Type::Ref(_)` / `Type::MutRef(_)` wrappers count;
/// owned / `Slice[T]` / `mut Slice[T]` / value-typed parameters are
/// not borrow positions for union-field-access purposes. The mut-slice
/// case is handled by the slice-assignment write-only contract (no
/// read of the union storage), so it intentionally does not gate.
pub(super) fn borrow_context_for_param(param_ty: &Type) -> Option<&'static str> {
    match param_ty {
        Type::Ref(_) => Some("ref"),
        Type::MutRef(_) => Some("mut ref"),
        _ => None,
    }
}

/// Does passing an argument to this formal parameter WRITE THROUGH it?
///
/// The two spellings that do — `mut ref T` and `mut Slice[T]` — are one
/// operation with two syntaxes, so any rule about writing must see both.
/// Deliberately separate from [`borrow_context_for_param`], which answers a
/// different question (which union diagnostic applies) and for which
/// `mut Slice[T]` correctly does NOT count.
///
/// `check_call_site_marker` computes the same predicate to decide whether a
/// `mut` marker is required at the call site; this is that definition, shared
/// so the marker rule and the shared-field write gate cannot drift apart
/// (B-2026-08-06-4 — they had, and `mut Slice[T]` fell through the gap).
pub(super) fn param_mutates_through(param_ty: &Type) -> bool {
    matches!(
        param_ty,
        Type::MutRef(_) | Type::Slice { mutable: true, .. }
    )
}

/// True if `ty` contains an `AssocProjection` node anywhere in its
/// structure. Used to gate the GAT-only `substitute_type_params` pass in
/// `check_call_args_with_substitution_full` (B-2026-07-12-6): a projection's
/// `param` string is the only thing `resolve_type_vars` can't rewrite, so
/// the extra substitution pass is worth running only when a projection is
/// actually present — over a projection-free type it re-substitutes bare
/// `TypeParam`s that resolution already handled and can nest a spurious
/// extra layer when a solution value re-introduces the same param name.
fn type_contains_assoc_projection(ty: &Type) -> bool {
    match ty {
        Type::AssocProjection { .. } => true,
        Type::Tuple(elems) => elems.iter().any(type_contains_assoc_projection),
        Type::Named { args, .. } => args.iter().any(type_contains_assoc_projection),
        Type::Array { element, .. }
        | Type::Vector { element, .. }
        | Type::Slice { element, .. } => type_contains_assoc_projection(element),
        Type::Rc(inner)
        | Type::Arc(inner)
        | Type::Ref(inner)
        | Type::MutRef(inner)
        | Type::Weak(inner)
        | Type::Pointer { inner, .. } => type_contains_assoc_projection(inner),
        Type::Function {
            params,
            return_type,
        }
        | Type::OnceFunction {
            params,
            return_type,
        } => {
            params.iter().any(type_contains_assoc_projection)
                || type_contains_assoc_projection(return_type)
        }
        Type::Existential { trait_args, .. } => {
            trait_args.iter().any(type_contains_assoc_projection)
        }
        Type::Refinement { base, .. } => type_contains_assoc_projection(base),
        // Terminal / projection-free shapes.
        Type::Int(_)
        | Type::UInt(_)
        | Type::Float(_)
        | Type::Bool
        | Type::Char
        | Type::Str
        | Type::Unit
        | Type::Never
        | Type::Shared(_)
        | Type::TypeParam(_)
        | Type::TypeVar(_)
        | Type::Shape(_)
        | Type::Error => false,
    }
}

/// A side of an `AssocTypeEq` discharge is only worth comparing once it has
/// bottomed out in a concrete type. An unresolved projection, a bare type
/// param, an inference variable, or an already-reported error means the
/// comparison would be against a placeholder — see
/// `discharge_assoc_type_eq_bounds` for why those are skipped rather than
/// reported.
fn assoc_eq_bound_is_comparable(ty: &Type) -> bool {
    !matches!(
        ty,
        Type::AssocProjection { .. } | Type::TypeParam(_) | Type::TypeVar(_) | Type::Error
    )
}
