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
    type_is_fully_concrete, type_to_concrete_or_param_name, type_to_mono_mangle_token, ConstArg,
    DimArg, IntSize, ScrutineeMode, SubstValue, Type, UIntSize,
};
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
            _ => {}
        }
    }

    pub(super) fn check_expr(&mut self, expr: &Expr, expected: &Type) -> Type {
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
            if !self.check_int_literal_fits(value, ctx, &expr.span) {
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
        if let Some(value) = Self::suffixed_int_literal_value(expr) {
            let ctx = match expected {
                Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                other => other,
            };
            if !self.check_int_literal_fits(value, ctx, &expr.span) {
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
                let body_ty = self.check_expr(body, expected_ret);
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
                    let ty =
                        self.infer_struct_literal_expected(path, fields, &expr.span, Some(args));
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
                            all_fit &= self.check_int_literal_fits(v, elem, &e.span);
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
        for (name, ty) in solutions {
            if let Some(resolved) = type_to_concrete_or_param_name(ty) {
                frame.insert(name.clone(), resolved);
            }
            // Element-aware mangle token (B-2026-07-11-35): the head-name `frame`
            // above erases `Vec[i64]` vs `Vec[String]` to `"Vec"`; this keeps the
            // full spelling so codegen can give each a distinct mono symbol.
            if let Some(tok) = type_to_mono_mangle_token(ty) {
                mangle_frame.insert(name.clone(), tok);
            }
        }
        if !frame.is_empty() {
            self.call_type_subs.insert(SpanKey::from_span(span), frame);
        }
        if !mangle_frame.is_empty() {
            self.call_type_subs_mangle
                .insert(SpanKey::from_span(span), mangle_frame);
        }
    }

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
    pub(super) fn check_call_args_with_substitution(
        &mut self,
        args: &[CallArg],
        params: &[Type],
        return_type: &Type,
        record_subs_for_span: &Span,
        apply_call_site_marker: bool,
    ) -> Type {
        self.check_call_args_with_substitution_full(
            args,
            params,
            return_type,
            record_subs_for_span,
            apply_call_site_marker,
            None,
            None,
            None,
            record_subs_for_span,
        )
    }

    /// Extended variant of `check_call_args_with_substitution` that
    /// accepts explicit call-site generic args + the function's
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
            if !matches!(&resolved, Type::TypeParam(n) if n == name) {
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
            if matches!(
                concrete_ty,
                Type::TypeParam(_) | Type::TypeVar(_) | Type::Error
            ) {
                // Unresolved metavar / propagating-param / already-error —
                // upstream diagnostics handle. Avoid noise.
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
        let candidates = self.impl_candidates_for_trait(trait_name);
        if !candidates.is_empty() {
            out.push_str("; trait `");
            out.push_str(trait_name);
            out.push_str("` is implemented by: ");
            out.push_str(&candidates.join(", "));
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
        if self
            .env
            .find_from_impl(&src_ty, &target_name, &[])
            .is_some()
        {
            self.into_conversions
                .insert(SpanKey::from_span(&expr.span), target_name);
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
            "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "f64"
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
        if self
            .env
            .find_tryfrom_impl(&src_ty, &target_name, &[])
            .is_some()
        {
            self.try_into_conversions
                .insert(SpanKey::from_span(&expr.span), target_name);
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
            // A literal is an i64: only its sign can violate u128.
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
    pub(super) fn check_int_literal_fits(&mut self, value: i128, ty: &Type, span: &Span) -> bool {
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
    fn suffixed_int_literal_value(expr: &Expr) -> Option<i128> {
        match &expr.kind {
            // B-2026-08-06-16: read through `literal_as_i128` rather than a
            // bare `as i128`, so an unsigned-suffixed literal carrying a
            // wrapped bit pattern (the upper half of u64) widens back to its
            // unsigned value instead of sign-extending to a negative one.
            ExprKind::Integer(n, sfx @ Some(_)) => Some(Self::literal_as_i128(*n, *sfx)),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => match &operand.kind {
                // The negated form keeps the signed reading: the operand of a
                // unary minus is a positive magnitude by construction, so there
                // is no bit pattern to recover, and `-5u64` must still report
                // as negative.
                ExprKind::Integer(n, Some(_)) => Some(-*n),
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
        if Self::float_expr_is_compile_time_constant(expr) {
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
                // B-2026-07-02-7: a SUFFIXED literal's own suffix defines its
                // range — `300u8` was admitted and silently diverged (interp
                // printed 300, codegen truncated to 44). Unsuffixed literals
                // are validated against their CONTEXTUAL type in `check_expr`.
                let neg_validated = self
                    .neg_validated_suffixed_literal
                    .is_some_and(|k| k == (expr.span.offset, expr.span.length));
                if sfx.is_some() && !neg_validated {
                    self.check_int_literal_fits(Self::literal_as_i128(*n, *sfx), &ty, &expr.span);
                }
                ty
            }
            ExprKind::Float(_, sfx) => Self::type_from_float_suffix(*sfx),
            ExprKind::CharLit(_) => Type::Char,
            ExprKind::ByteLit(_) => Type::UInt(UIntSize::U8),
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
                        self.check_int_literal_fits(-*n, &ty, &expr.span);
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

            ExprKind::Call { callee, args } => self.infer_call(callee, args, &expr.span),

            ExprKind::MethodCall {
                object,
                method,
                args,
                turbofish: _,
                args_close_span,
            } => self.infer_method_call(object, method, args, &expr.span, args_close_span),

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
                    return match element_ty {
                        Some(el) => Type::Slice {
                            element: Box::new(el),
                            mutable: false,
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
                condition, body, ..
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
                self.infer_block(body);
                Type::Unit
            }

            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                let iter_ty = self.infer_expr(iterable);
                self.local_scope.push();
                // Resolve element type via IntoIterator.Item (impl_assoc_types),
                // covering Vec, Map, SortedSet, Set, Slice, Array, Range* and
                // any user type that has registered an "Item" assoc binding.
                let elem_ty = self.element_type_of(&iter_ty);
                self.bind_pattern_types(pattern, &elem_ty);
                for stmt in &body.stmts {
                    self.check_stmt(stmt);
                }
                if let Some(ref final_expr) = body.final_expr {
                    self.infer_expr(final_expr);
                }
                self.local_scope.pop();
                Type::Unit
            }

            ExprKind::Loop { body, .. } => {
                self.infer_block(body);
                Type::Never
            }

            ExprKind::LabeledBlock { label, body, .. } => {
                // LB3 — push a fresh per-label collector frame, infer the
                // body's tail type, pop the frame, and compute the block's
                // type as the LUB of `tail_type` and the collected
                // `break label expr` value types.
                self.break_value_types.push((label.clone(), Vec::new()));
                let tail_ty = self.infer_block(body);
                let frame = self
                    .break_value_types
                    .pop()
                    .map(|(_, v)| v)
                    .unwrap_or_default();
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
                    if let Some(ref ret_ty) = self.current_return_type.clone() {
                        self.check_expr(expr, ret_ty);
                    } else {
                        self.infer_expr(expr);
                    }
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
                // LB3 — feed the per-label LUB collector for labeled
                // blocks. Find the matching frame by label name (innermost
                // wins) and append the value type. Unlabeled `break`s
                // and breaks targeting a labeled loop have no matching
                // collector frame and are ignored here — loops keep
                // their `Type::Never`-by-default behavior.
                if let Some(name) = label {
                    if let Some(frame) = self
                        .break_value_types
                        .iter_mut()
                        .rev()
                        .find(|(n, _)| n == name)
                    {
                        frame.1.push(val_ty);
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
                    Type::Tuple(types)
                }
            }

            ExprKind::StructLiteral {
                path,
                fields,
                spread,
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
                    self.infer_expr(spread_expr);
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
                self.infer_block(body);
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
