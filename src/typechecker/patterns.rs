//! Pattern checking, refutability, exhaustiveness, and the
//! pattern-aware control-flow forms (`if let`, `match`).
//!
//! Houses `check_pattern_against` (structural pattern checker with
//! scrutinee-mode propagation), `bind_pattern_types` (binding
//! collector), `is_irrefutable_pattern` / `is_irrefutable_param_pattern`
//! / `check_param_irrefutable` (refutability checks),
//! `check_exhaustiveness` (the match-arm coverage algorithm), and the
//! top-level `check_if_against` / `check_if_let_against` /
//! `check_match_against` / `infer_match` drivers.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use std::collections::HashMap;

use super::inference::{resolve_type_vars, substitute_type_params};
use super::types::{
    strip_refinement, type_display, ConstArg, ConstVarId, DimArg, FloatSize, IntSize,
    ScrutineeMode, SubstValue, Type, TypeVarId, UIntSize, VariantTypeInfo,
};
use super::TypeErrorKind;

/// Canonical surface name for a sub-64-bit integer / `char` payload
/// binding, or `None` for everything else (width-64 ints, floats,
/// aggregates, named types). Recorded in `pattern_binding_types` so
/// codegen's `reconstruct_payload_value` narrows the i64 payload word
/// back to the binding's real width — the integer analogue of the
/// `bool` → i1 narrowing. Width-64 ints (`i64`/`u64`/`usize`) and the
/// 128-bit widths are intentionally excluded: the former need no trunc
/// from the i64 word, and the latter aren't single-word payloads.
fn narrow_int_surface_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Int(IntSize::I8) => Some("i8".to_string()),
        Type::Int(IntSize::I16) => Some("i16".to_string()),
        Type::Int(IntSize::I32) => Some("i32".to_string()),
        Type::UInt(UIntSize::U8) => Some("u8".to_string()),
        Type::UInt(UIntSize::U16) => Some("u16".to_string()),
        Type::UInt(UIntSize::U32) => Some("u32".to_string()),
        Type::Char => Some("char".to_string()),
        _ => None,
    }
}

/// Surface name for a float-typed pattern binding. Recorded so codegen
/// bitcasts the i64 payload word back to the float and tracks the binding as
/// float (not i64) — without it, enum float payloads (`Option[f64]`, the
/// lexer's `Token::Float(f64, …)`) bind/print as raw integer bits.
fn float_surface_name(ty: &Type) -> Option<String> {
    match ty {
        // f16/bf16 included (B-2026-07-20-12): without a recorded surface
        // name, an f16 enum-payload binding stayed a raw i64 word at codegen
        // and arithmetic on it read the bit pattern as an integer VALUE.
        Type::Float(FloatSize::F16) => Some("f16".to_string()),
        Type::Float(FloatSize::BF16) => Some("bf16".to_string()),
        Type::Float(FloatSize::F32) => Some("f32".to_string()),
        Type::Float(FloatSize::F64) => Some("f64".to_string()),
        _ => None,
    }
}

/// Map an integer-literal suffix to its concrete `Type` for range-bound
/// type-matching. `None` (no suffix) returns `None` so the bound's width
/// defers to the other bound / scrutinee.
fn int_suffix_to_type(sfx: &Option<crate::token::IntSuffix>) -> Option<Type> {
    use crate::token::IntSuffix as S;
    Some(match sfx.as_ref()? {
        S::I8 => Type::Int(IntSize::I8),
        S::I16 => Type::Int(IntSize::I16),
        S::I32 => Type::Int(IntSize::I32),
        S::I64 => Type::Int(IntSize::I64),
        S::I128 => Type::Int(IntSize::I128),
        S::U8 => Type::UInt(UIntSize::U8),
        S::U16 => Type::UInt(UIntSize::U16),
        S::U32 => Type::UInt(UIntSize::U32),
        S::U64 => Type::UInt(UIntSize::U64),
        S::U128 => Type::UInt(UIntSize::U128),
    })
}

/// True when two resolved range bounds have incompatible types: a
/// char-vs-integer family mismatch, or two *explicitly*-typed integer
/// bounds of differing width. A suffix-less integer literal
/// (`explicit == false`) adopts the other bound's width and never
/// conflicts. Each tuple is `(value, type, explicit_width)` as produced
/// by `resolve_range_bound_value`.
fn range_bounds_type_conflict(a: &(i128, Type, bool), b: &(i128, Type, bool)) -> bool {
    let a_char = matches!(a.1, Type::Char);
    let b_char = matches!(b.1, Type::Char);
    if a_char != b_char {
        return true; // char vs integer
    }
    if a_char {
        return false; // both char
    }
    // Both integer: conflict only when both carry an explicit width and
    // those widths differ.
    a.2 && b.2 && a.1 != b.1
}

impl<'a> super::TypeChecker<'a> {
    /// Check-mode form of `if`. Threads `expected` into both branches so
    /// closures and other check-sensitive shapes inside arms see the
    /// target type. The condition is still synthesized + asserted Bool.
    /// When the `else` branch is missing, the expected type must accept
    /// `Unit` (the synth path's behavior); we delegate to
    /// `check_assignable` for that diagnostic so the message is uniform
    /// with non-branching cases.
    pub(super) fn check_if_against(
        &mut self,
        condition: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
        expected: &Type,
        span: &Span,
    ) -> Type {
        let cond_ty = self.infer_expr(condition);
        if cond_ty != Type::Bool && cond_ty != Type::Error {
            self.type_error(
                format!(
                    "condition must be 'bool', found '{}'",
                    type_display(&cond_ty)
                ),
                condition.span.clone(),
                TypeErrorKind::ConditionNotBool,
            );
        }
        let then_ty = self.check_block_against(then_block, expected);
        if let Some(else_expr) = else_branch {
            let else_ty = self.check_expr(else_expr, expected);
            // Each branch's check_expr already reported a TypeMismatch
            // against `expected` if it didn't comply; no need to re-check
            // cross-branch compatibility (it's transitive through expected).
            // Pick a non-Never type as the recorded result.
            let result_ty = if then_ty != Type::Never {
                then_ty
            } else {
                else_ty
            };
            self.record_expr_type(span, &result_ty);
            result_ty
        } else {
            // No else: the if returns Unit. Surface the standard
            // assignability diagnostic if the caller expected non-Unit.
            self.check_assignable(expected, &Type::Unit, span.clone());
            self.record_expr_type(span, &Type::Unit);
            Type::Unit
        }
    }

    /// Check-mode form of `if let`. Same shape as `check_if_against`
    /// but binds the pattern's variables in the then-block scope before
    /// checking it. Pattern type-checking against the value's type is
    /// deferred to the synthesis-mode `infer_expr` arm — we mirror its
    /// current behavior (synth value, no pattern type check) so this
    /// slice doesn't change diagnostics around irrefutable-let.
    pub(super) fn check_if_let_against(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
        expected: &Type,
        span: &Span,
    ) -> Type {
        let scrut_ty = self.infer_expr(value);
        // Mirror `infer_if_let`'s pattern binding so the then-block sees
        // the pattern's bindings with their scrutinee-derived types.
        let (mode, dispatch_ty) = ScrutineeMode::classify(&scrut_ty);
        let dispatch_ty = dispatch_ty.clone();
        self.local_scope.push();
        self.check_pattern_against(pattern, &dispatch_ty, mode);
        let then_ty = self.check_block_against(then_block, expected);
        self.local_scope.pop();
        if let Some(else_expr) = else_branch {
            let else_ty = self.check_expr(else_expr, expected);
            let result_ty = if then_ty != Type::Never {
                then_ty
            } else {
                else_ty
            };
            self.record_expr_type(span, &result_ty);
            result_ty
        } else {
            self.check_assignable(expected, &Type::Unit, span.clone());
            self.record_expr_type(span, &Type::Unit);
            Type::Unit
        }
    }

    /// Check-mode form of `match`. Each arm body is checked against
    /// `expected`. Mirrors `infer_match` for scrutinee/guard/pattern
    /// machinery and exhaustiveness; only the arm-body inference is
    /// replaced with check-mode dispatch. Per-arm assignability
    /// diagnostics from `check_expr` replace the synth path's aggregate
    /// `BranchTypeMismatch` (more specific — points at the offending
    /// arm rather than the whole match).
    pub(super) fn check_match_against(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        expected: &Type,
        span: &Span,
    ) -> Type {
        let scrut_ty = self.infer_expr(scrutinee);
        let (mode, dispatch_ty) = ScrutineeMode::classify(&scrut_ty);
        let dispatch_ty = dispatch_ty.clone();
        let mut arm_types: Vec<Type> = Vec::new();
        let mut scrutinee_mismatch = false;
        for arm in arms {
            self.local_scope.push();
            let errs_before = self.errors.len();
            self.check_pattern_against(&arm.pattern, &dispatch_ty, mode);
            scrutinee_mismatch |= self.errors[errs_before..]
                .iter()
                .any(|e| e.kind == TypeErrorKind::PatternScrutineeMismatch);
            if let Some(guard) = &arm.guard {
                let guard_ty = self.infer_expr(guard);
                if guard_ty != Type::Bool && guard_ty != Type::Error {
                    self.type_error(
                        format!(
                            "match guard must be 'bool', found '{}'",
                            type_display(&guard_ty)
                        ),
                        guard.span.clone(),
                        TypeErrorKind::ConditionNotBool,
                    );
                }
            }
            let arm_ty = self.check_expr(&arm.body, expected);
            arm_types.push(arm_ty);
            self.local_scope.pop();
        }
        // A variant-pattern/scrutinee mismatch already poisons the match; the
        // scrutinee isn't the enum the arms destructure, so a follow-on
        // "non-exhaustive match" would be redundant and misleading
        // (B-2026-07-17-6).
        if !scrutinee_mismatch {
            self.check_exhaustiveness(&scrut_ty, arms, span.clone());
        }
        let result_ty = arm_types
            .iter()
            .find(|t| **t != Type::Never)
            .cloned()
            .unwrap_or(Type::Never);
        self.record_expr_type(span, &result_ty);
        result_ty
    }

    pub(super) fn infer_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: &Span) -> Type {
        let scrut_ty = self.infer_expr(scrutinee);
        let (mode, dispatch_ty) = ScrutineeMode::classify(&scrut_ty);
        let dispatch_ty = dispatch_ty.clone();
        let mut arm_types: Vec<Type> = Vec::new();
        let mut scrutinee_mismatch = false;

        for arm in arms {
            self.local_scope.push();
            let errs_before = self.errors.len();
            self.check_pattern_against(&arm.pattern, &dispatch_ty, mode);
            scrutinee_mismatch |= self.errors[errs_before..]
                .iter()
                .any(|e| e.kind == TypeErrorKind::PatternScrutineeMismatch);
            if let Some(guard) = &arm.guard {
                let guard_ty = self.infer_expr(guard);
                if guard_ty != Type::Bool && guard_ty != Type::Error {
                    self.type_error(
                        format!(
                            "match guard must be 'bool', found '{}'",
                            type_display(&guard_ty)
                        ),
                        guard.span.clone(),
                        TypeErrorKind::ConditionNotBool,
                    );
                }
            }
            let arm_ty = self.infer_expr(&arm.body);
            arm_types.push(arm_ty);
            self.local_scope.pop();
        }

        // Check exhaustiveness for enum types — but not when a variant
        // pattern already mismatched the scrutinee type, which poisons the
        // match and would make a "non-exhaustive" tail redundant
        // (B-2026-07-17-6).
        if !scrutinee_mismatch {
            self.check_exhaustiveness(&scrut_ty, arms, span.clone());
        }

        // Fold the (non-Never, non-Error) arm types into their least-upper-
        // bound. Refinement arms widen to their shared base (design.md
        // § Refinement Types > LUB rule 4) via `join_branch_types`; for
        // non-refinement arms the fold returns the first arm unchanged, so a
        // homogeneous match keeps its previous result type. On the first
        // incompatible arm, emit a single `BranchTypeMismatch` and stop
        // widening (the running `result_ty` is the recovery type).
        let mut result_ty = arm_types
            .iter()
            .find(|t| **t != Type::Never)
            .cloned()
            .unwrap_or(Type::Never);
        let mut reported = false;

        for arm_ty in &arm_types {
            if *arm_ty == Type::Never || *arm_ty == Type::Error || result_ty == Type::Error {
                continue;
            }
            match self.join_branch_types(&result_ty, arm_ty) {
                Some(joined) => result_ty = joined,
                None => {
                    if !reported {
                        self.type_error(
                            format!(
                                "match arms have incompatible types: '{}' and '{}'",
                                type_display(&result_ty),
                                type_display(arm_ty)
                            ),
                            span.clone(),
                            TypeErrorKind::BranchTypeMismatch,
                        );
                        reported = true;
                    }
                }
            }
        }

        result_ty
    }

    /// Validate a slice/array pattern against the scrutinee type and
    /// return `(element_type, rest_binding_type)` — the per-element type
    /// used to recurse into prefix/suffix sub-patterns, and the type the
    /// `..name` rest binding should receive. Emits per-case diagnostics
    /// (arity overflow, under-coverage without `..`, non-literal `Array`
    /// size, `String` redirect, unsupported scrutinee shape). Returns
    /// `(Type::Error, Type::Error)` on rejection so callers can still
    /// recurse cleanly without crashing. Sub-item 2 of the slice/array-
    /// patterns entry (phase 5.2).
    fn slice_pattern_types(
        &mut self,
        prefix: &[Pattern],
        rest: &Option<RestPattern>,
        suffix: &[Pattern],
        scrutinee_type: &Type,
        span: &Span,
    ) -> (Type, Type) {
        let head = prefix.len();
        let tail = suffix.len();
        let used = head + tail;
        match scrutinee_type {
            Type::Error => (Type::Error, Type::Error),
            Type::Array { element, size } => match size.as_usize() {
                Some(n) => {
                    if used > n {
                        self.type_error(
                            format!(
                                "slice pattern has {used} element{} but \
                                 `Array[_, {n}]` has length {n}; remove \
                                 element patterns or add a `..` marker",
                                if used == 1 { "" } else { "s" },
                            ),
                            span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return (Type::Error, Type::Error);
                    }
                    if rest.is_none() && used != n {
                        self.type_error(
                            format!(
                                "slice pattern covers {used} of {n} \
                                 positions on `Array[_, {n}]`; add a `..` \
                                 marker if the remaining positions should \
                                 match anything"
                            ),
                            span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return ((**element).clone(), Type::Error);
                    }
                    let remainder = (n - used) as i64;
                    let rest_ty = Type::Array {
                        element: element.clone(),
                        size: ConstArg::Literal(remainder),
                    };
                    ((**element).clone(), rest_ty)
                }
                None => {
                    self.type_error(
                        "slice patterns on `Array[T, N]` require `N` to be \
                         a compile-time literal; const-parameter array \
                         sizes are not yet supported in pattern position"
                            .to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    ((**element).clone(), Type::Error)
                }
            },
            Type::Slice { element, mutable } => {
                let rest_ty = Type::Slice {
                    element: element.clone(),
                    mutable: *mutable,
                };
                ((**element).clone(), rest_ty)
            }
            Type::Named { name, args } if name == "Vec" => {
                let element = args.first().cloned().unwrap_or(Type::Error);
                let rest_ty = Type::Slice {
                    element: Box::new(element.clone()),
                    mutable: false,
                };
                (element, rest_ty)
            }
            Type::Str => {
                self.type_error(
                    "slice patterns do not apply to `String` — UTF-8 is \
                     variable-width per code point, so byte-level positional \
                     patterns would produce invalid boundaries. To match on \
                     string content, convert to `Slice[u8]` with `.bytes()` \
                     or `Iterator[char]` with `.chars()` first"
                        .to_string(),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                (Type::Error, Type::Error)
            }
            _ => {
                self.type_error(
                    format!(
                        "slice patterns apply to `Array[T, N]`, `Vec[T]`, \
                         and `Slice[T]`; cannot match a value of type `{}`",
                        type_display(scrutinee_type)
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                (Type::Error, Type::Error)
            }
        }
    }

    /// Walk `pattern` against the scrutinee's structural type `expected`
    /// under `mode` (Owned / Ref / MutRef), binding any introduced names
    /// into `self.local_scope`. The mode is the **match scrutinee's**
    /// borrow form, captured at the match entry by
    /// `ScrutineeMode::classify` after stripping one outer `ref` /
    /// `mut ref` (so the variant / struct / tuple dispatch below keeps
    /// matching the unwrapped shape). `mode` propagates transitively
    /// into every sub-pattern; each leaf binding's type is wrapped via
    /// `ScrutineeMode::wrap_binding_ty` so a `ref T` scrutinee yields
    /// `ref FieldType` bindings — design.md § Match Arm Binding Modes.
    /// Owned scrutinees keep the prior behaviour exactly (no wrap).
    /// Mirror `bind_pattern_types`'s side-table writes so codegen can
    /// reconstitute struct payloads and dispatch methods/fields on
    /// pattern bindings. Shared by the `Binding` leaf arm and the
    /// `AtBinding` outer-alias arm of `check_pattern_against` (the
    /// alias has the scrutinee's type at its position — without this,
    /// `x @ Foo { … }` left `x` untyped in codegen's `var_type_names`
    /// and `x.field` silently read 0).
    ///
    /// `Type::Str` registers `"String"` parallel to how
    /// `Type::Named { name: "Vec" }` registers `"Vec"` — required by
    /// the tuple-payload destructure path (`pattern_payload_word_count`)
    /// for variant-payload tuples containing String elements (Theme 5,
    /// 2026-05-10). `Type::Shared(name)` registers under its bare
    /// struct name so codegen's `shared_type_for_expr` lookup finds the
    /// heap layout for `node.field` access after a `Some(node)` pattern
    /// binding. A refinement records its *base*'s surface name (codegen
    /// dispatches a refined value as its base, phase-9 step 5a) —
    /// `local_scope` keeps the real refinement type for type-checking.
    pub(super) fn record_pattern_binding_surface_types(
        &mut self,
        pattern: &Pattern,
        expected: &Type,
    ) {
        let expected = strip_refinement(expected);
        // Peel an immutable/exclusive borrow: a `ref T` / `mut ref T` payload
        // binding (e.g. `Some(w)` from `Vec.first()` / `Vec.get(i)`, now typed
        // `Option[ref T]`) reconstructs at codegen as the inner *value* T — a
        // by-value aliasing borrow whose borrow-ness is carried by cleanup
        // suppression (`scrutinee_is_borrow_call`), not by the recorded layout
        // name. Record the inner T's surface name + element side-tables so the
        // binding gets T's true word-count/dispatch; without this it falls to
        // the 1-word default and a `ref String` truncates to a single word.
        if let Type::Ref(inner) | Type::MutRef(inner) = expected {
            self.record_pattern_binding_surface_types(pattern, inner);
            return;
        }
        if let Type::Named {
            name: type_name, ..
        } = expected
        {
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), type_name.clone());
        } else if matches!(expected, Type::Str) {
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), "String".to_string());
        } else if let Type::Shared(type_name) = expected {
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), type_name.clone());
        } else if matches!(expected, Type::Bool) {
            // Match-arm parallel to the `bind_pattern_types` bool
            // case — see that site for the trunc-narrowing
            // motivation.
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), "bool".to_string());
        } else if matches!(expected, Type::Pointer { .. }) {
            // Match-arm parallel to the `bind_pattern_types`
            // pointer case — record `*const T` / `*mut T` so
            // `raii_check` can detect raw-pointer-typed bindings
            // held across a yield point.
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), type_display(expected));
        } else if let Some(narrow) = narrow_int_surface_name(expected) {
            // Match-arm parallel to the `bind_pattern_types`
            // narrow-int case — record `u8` / `i32` / `char` /
            // … so codegen narrows the i64 payload word back to
            // the binding's real width (e.g. `Vec[u8].pop()`'s
            // `Some(b) => b == other_u8`).
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), narrow);
        } else if let Some(fname) = float_surface_name(expected) {
            // Float payload binding (`Some(x)` over `Option[f64]`, the lexer's
            // `Token::Float(f64, …)`): record `f64` / `f32` so codegen bitcasts
            // the payload word back to the float and dispatches float-typed
            // (println, arithmetic) rather than reading the raw i64 bits.
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), fname);
        }
        // PB sibling slice (2026-05-09): mirror
        // `bind_pattern_types`'s sibling-table write so direct
        // method dispatch on a pattern-bound `Vec[T]` / `Slice[T]`
        // payload (the canonical match-arm shape) routes through
        // the right element-typed path.
        self.record_pattern_inner_type(pattern, expected);
    }

    /// Emit `E_AT_BINDING_DOUBLE_CONSUME` for `inner_name` against the
    /// nearest enclosing consuming `@`-binding outer, if any. No-op when
    /// no consuming outer is on the stack (the top-level `@` itself, or
    /// borrow-mode subtrees, never conflict). Wording per the phase-8
    /// `@`-binding entry slice 4 / design.md § @ Bindings "Owned
    /// scrutinee".
    fn report_at_binding_double_consume(&mut self, inner_name: &str, inner_span: &Span) {
        let Some((outer, _)) = self.owned_at_binding_outers.last() else {
            return;
        };
        let outer = outer.clone();
        self.type_error(
            format!(
                "error[E_AT_BINDING_DOUBLE_CONSUME]: cannot bind both \
                 '{outer}' and '{inner_name}' as owned — '{outer}' would \
                 consume the whole value while '{inner_name}' would consume \
                 a field; use 'ref {outer}' to borrow, or omit one of the \
                 two bindings"
            ),
            inner_span.clone(),
            TypeErrorKind::AtBindingDoubleConsume,
        );
    }

    pub(super) fn check_pattern_against(
        &mut self,
        pattern: &Pattern,
        expected: &Type,
        mode: ScrutineeMode,
    ) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Binding(name) => {
                // Check if this binding name is actually an enum variant
                // (unit variants are parsed as Binding since the parser can't distinguish)
                if let Type::Named {
                    name: enum_name, ..
                } = expected
                {
                    if let Some(enum_info) = self.env.enums.get(enum_name).cloned() {
                        if enum_info.variants.iter().any(|(vn, _)| vn == name) {
                            // It's a unit variant match, not a variable binding
                            return;
                        }
                    }
                }
                // A dotted name (`Color.Red`) or a bare PascalCase name that
                // is a known unit variant of some enum (`None`, a user
                // `enum Color { Red }`'s `Red`) is a unit-variant *pattern*,
                // not a fresh binding — the interpreter's structural matcher
                // (pattern_match.rs) classifies it exactly this way and would
                // ICE if no arm matched. Kāra's case-class invariant makes
                // value bindings snake_case and variant names PascalCase, so
                // this never steals a genuine binding. When the scrutinee
                // provably cannot own the variant, emit the same scrutinee-
                // type mismatch as the constructor form and stop — do NOT fall
                // through to treat it as a catch-all binding, which is exactly
                // what made `match x:i64 { None => .. }` pass check and then
                // ICE (B-2026-07-17-6). The bare unit-variant *match* against
                // the correct enum was already handled by the early return
                // above; here `variant_pattern_scrutinee_mismatch` returns
                // false for a scrutinee that does own the variant, so a
                // dotted `Color.Red` against a `Color` scrutinee falls through
                // untouched.
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                let is_variant_pattern = name.contains('.')
                    || (variant_name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_uppercase())
                        && self.is_known_unit_variant(variant_name));
                if is_variant_pattern
                    && self.variant_pattern_scrutinee_mismatch(expected, variant_name)
                {
                    self.emit_pattern_scrutinee_mismatch(
                        name,
                        variant_name,
                        expected,
                        &pattern.span,
                    );
                    return;
                }
                // Cannot-double-consume rule: a by-move non-Copy leaf
                // binding inside a consuming `@` outer claims heap content
                // the outer already owns (design.md § @ Bindings).
                if matches!(mode, ScrutineeMode::Owned)
                    && !self.owned_at_binding_outers.is_empty()
                    && !self.is_copy_type_during_check(expected)
                {
                    self.report_at_binding_double_consume(name, &pattern.span);
                }
                let binding_ty = mode.wrap_binding_ty(expected.clone());
                self.local_scope.insert(name.clone(), binding_ty);
                self.record_pattern_binding_borrow_mode(&pattern.span, mode, expected);
                self.record_pattern_binding_surface_types(pattern, expected);
            }
            PatternKind::Literal(_) => {
                // Type checking of literal patterns deferred
            }
            PatternKind::TupleVariant { path, patterns } => {
                let variant_name = path.last().cloned().unwrap_or_default();
                if let Type::Named { name, args } = expected {
                    if let Some(enum_info) = self.env.enums.get(name).cloned() {
                        if let Some((_, VariantTypeInfo::Tuple(field_types))) =
                            enum_info.variants.iter().find(|(n, _)| n == &variant_name)
                        {
                            // Substitute the enum's generic params with the
                            // concrete args from the scrutinee's type so
                            // sub-patterns see the resolved payload type
                            // (e.g. `Err(e)` against `Result[i64, MyError]`
                            // sees `e: MyError`, not `e: TypeParam("E")`).
                            let subs: HashMap<String, SubstValue> = enum_info
                                .generic_params
                                .iter()
                                .cloned()
                                .zip(args.iter().cloned().map(SubstValue::Type))
                                .collect();
                            for (pat, ty) in patterns.iter().zip(field_types.iter()) {
                                let resolved = if subs.is_empty() {
                                    ty.clone()
                                } else {
                                    substitute_type_params(ty, &subs)
                                };
                                self.check_pattern_against(pat, &resolved, mode);
                            }
                            return;
                        }
                    }
                }
                // Fallback: the scrutinee is not an enum that declares this
                // variant. When we can *prove* the scrutinee type cannot own
                // the variant (a primitive, a struct, an aggregate, or an
                // enum lacking it), this is a genuine type error — emit it
                // instead of silently binding, which used to let `karac
                // check` accept a program the interpreter then ICE'd on
                // (B-2026-07-17-6). The guard stays silent for `Type::Error`,
                // unresolved inference vars, generic params, and opaque types
                // so error-cascade suppression and generic code are never
                // falsely accused.
                if self.variant_pattern_scrutinee_mismatch(expected, &variant_name) {
                    let ctor = path.join(".");
                    let ctor_disp = if patterns.is_empty() {
                        ctor
                    } else {
                        format!("{ctor}(..)")
                    };
                    self.emit_pattern_scrutinee_mismatch(
                        &ctor_disp,
                        &variant_name,
                        expected,
                        &pattern.span,
                    );
                }
                // Bind sub-patterns to Error for recovery so the arm body
                // does not cascade "undefined variable" diagnostics.
                for pat in patterns {
                    self.check_pattern_against(pat, &Type::Error, mode);
                }
            }
            PatternKind::Struct {
                path,
                fields,
                has_rest,
            } => {
                let struct_name = path.last().cloned().unwrap_or_default();

                // `#[non_exhaustive]` slice 4 pattern half — cross-package
                // exhaustive struct pattern (no `..`) on a `#[non_exhaustive]`
                // struct is rejected with the `..`-insertion fix-it. Mirrors
                // the slice-4 literal half in `infer_struct_literal`: same
                // `is_non_exhaustive && defining_stdlib_origin && !current_fn_stdlib_origin`
                // condition, applied at the pattern check site. The fix-it
                // points at inserting `..` before the closing brace so the
                // pattern keeps matching when the defining package adds
                // fields.
                if let Some(info) = self.env.structs.get(&struct_name) {
                    if info.is_non_exhaustive
                        && info.defining_stdlib_origin
                        && !self.current_fn_stdlib_origin
                        && !*has_rest
                    {
                        let fix_it = non_exhaustive_pattern_fix_it(&pattern.span, fields);
                        self.type_error_with_fix_it(
                            format!(
                                "error[E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN]: \
                                 cannot exhaustively destructure `{name}` — \
                                 `{name}` is `#[non_exhaustive]` and defined \
                                 in another package, so its field set may \
                                 grow. Add `..` before the closing `}}` to \
                                 leave room for future fields (e.g. \
                                 `{name} {{ ..your_fields.., .. }}`). See \
                                 design.md § `#[non_exhaustive]` for \
                                 Evolvable Public Types.",
                                name = struct_name
                            ),
                            pattern.span.clone(),
                            TypeErrorKind::NonExhaustiveCrossPackagePattern,
                            fix_it,
                        );
                    }
                }

                // Look up struct or enum variant
                let field_types: Option<Vec<(String, Type)>> =
                    if let Some(info) = self.env.structs.get(&struct_name) {
                        Some(
                            info.fields
                                .iter()
                                .map(|(n, t, _)| (n.clone(), t.clone()))
                                .collect(),
                        )
                    } else if let Type::Named { name, .. } = expected {
                        self.env.enums.get(name).and_then(|e| {
                            e.variants
                                .iter()
                                .find(|(n, _)| n == &struct_name)
                                .and_then(|(_, v)| {
                                    if let VariantTypeInfo::Struct(fields) = v {
                                        Some(fields.clone())
                                    } else {
                                        None
                                    }
                                })
                        })
                    } else {
                        None
                    };

                if let Some(ft) = field_types {
                    for field in fields {
                        let field_ty = ft
                            .iter()
                            .find(|(n, _)| n == &field.name)
                            .map(|(_, t)| t.clone())
                            .unwrap_or(Type::Error);
                        if let Some(ref sub_pattern) = field.pattern {
                            self.check_pattern_against(sub_pattern, &field_ty, mode);
                        } else {
                            // Shorthand `Point { items }`: codegen synthesizes a
                            // leaf `Pattern { Binding(field.name), span:
                            // field.span }` (pattern_binding.rs Struct arm), so
                            // run an identical synthetic binding through
                            // `check_pattern_against` here. The `Binding` arm
                            // records `pattern_binding_types` / the inner-element
                            // table / the borrow mode against that exact span,
                            // AND applies the cannot-double-consume `@` rule —
                            // the same surface every other leaf binding gets.
                            // Without this the shorthand field carried only a
                            // borrow-mode entry, so codegen's leaf arm had no
                            // type for it and could neither dispatch methods
                            // (`items.len()` → "no handler for method") nor
                            // register its scope-exit cleanup (the
                            // struct-destructure heap-field leak). Subsumes the
                            // previously-inlined double-consume + borrow-mode
                            // writes (now the single Binding-arm path). The
                            // explicit `items: x` form already routed here via
                            // its sub-pattern; this brings shorthand to parity.
                            let synthetic = Pattern {
                                kind: PatternKind::Binding(field.name.clone()),
                                span: field.span.clone(),
                            };
                            self.check_pattern_against(&synthetic, &field_ty, mode);
                        }
                    }
                }
            }
            PatternKind::Tuple(patterns) => {
                if let Type::Tuple(types) = expected {
                    for (pat, ty) in patterns.iter().zip(types.iter()) {
                        self.check_pattern_against(pat, ty, mode);
                    }
                }
            }
            PatternKind::RangePattern {
                start,
                end,
                inclusive,
            } => {
                // No bindings, but resolve any const-named bounds and run
                // the const-resolution / bound-ordering / type-matching
                // checks (design.md § Range Patterns, v60 item 51 slices
                // 3–5).
                self.check_range_pattern_bounds(
                    start.as_ref(),
                    end.as_ref(),
                    *inclusive,
                    expected,
                    &pattern.span,
                );
            }
            PatternKind::AtBinding {
                name,
                pattern: inner,
                by_ref,
            } => {
                // `ref name @ PATTERN` flips the whole subtree to borrow
                // mode (design.md § @ Bindings, "Explicit `ref` on the
                // `@` binding") — the outer alias is `ref T` and every
                // inner binding borrows into the (still-owned) scrutinee.
                let effective_mode = if *by_ref { ScrutineeMode::Ref } else { mode };
                let binding_ty = effective_mode.wrap_binding_ty(expected.clone());
                self.local_scope.insert(name.clone(), binding_ty);
                // The `name @` outer alias is recorded against the outer
                // pattern's span; the inner sub-pattern records its own
                // bindings via the recursive call. The surface-type
                // side-tables make `x.field` / method dispatch on the
                // alias work in codegen (pre-existing silent-zero gap,
                // surfaced by the slice-4 E2E: the alias was never
                // registered in `var_type_names`).
                self.record_pattern_binding_borrow_mode(&pattern.span, effective_mode, expected);
                self.record_pattern_binding_surface_types(pattern, expected);
                // Cannot-double-consume rule (design.md § @ Bindings,
                // "Owned scrutinee"): a consuming `@` outer owns the whole
                // value, so any by-move non-Copy binding inside the
                // sub-pattern would own the same heap content twice.
                let consuming_outer = !*by_ref
                    && matches!(effective_mode, ScrutineeMode::Owned)
                    && !self.is_copy_type_during_check(expected);
                if consuming_outer {
                    // A nested consuming `@` is itself an inner by-move
                    // claim against the nearest enclosing outer (slice 8
                    // granularity: `outer @ Foo { f: inner @ Bar(v) }`
                    // reports `inner` against `outer`, then `v` against
                    // `inner` via the recursion below).
                    self.report_at_binding_double_consume(name, &pattern.span);
                    self.owned_at_binding_outers
                        .push((name.clone(), pattern.span.clone()));
                }
                self.check_pattern_against(inner, expected, effective_mode);
                if consuming_outer {
                    self.owned_at_binding_outers.pop();
                }
            }
            PatternKind::Or(alternatives) => {
                for alt in alternatives {
                    self.check_pattern_against(alt, expected, mode);
                }
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let (element_ty, rest_ty) =
                    self.slice_pattern_types(prefix, rest, suffix, expected, &pattern.span);
                for pat in prefix.iter().chain(suffix.iter()) {
                    self.check_pattern_against(pat, &element_ty, mode);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    // Slice-rest mutability propagation
                    // (design.md § Slice and array patterns > Mutability
                    // propagation, lines 7657–7661): a `..rest` over a
                    // `Vec[T]` / `Slice[T]` produces `Slice[T]`
                    // (immutable) under `Owned` / `Ref` and
                    // `mut Slice[T]` under `MutRef`; over an
                    // `Array[T, N]` the rest binds `Array[T, K]` under
                    // `Owned`, `ref Array[T, K]` under `Ref`, and
                    // `mut ref Array[T, K]` under `MutRef`. Generic
                    // `wrap_binding_ty` skips `Type::Slice` (so struct
                    // fields typed `Slice[T]` are never elevated) —
                    // the rest binding's source is the freshly
                    // synthesised window, so the elevation rule is
                    // applied locally here.
                    let binding_ty = match (mode, rest_ty) {
                        (ScrutineeMode::MutRef, Type::Slice { element, .. }) => Type::Slice {
                            element,
                            mutable: true,
                        },
                        (m, ty) => m.wrap_binding_ty(ty),
                    };
                    self.local_scope.insert(name.clone(), binding_ty);
                }
            }
        }
    }

    /// True iff `name` is a **unit** variant of some known enum. Used by the
    /// `Binding` arm of `check_pattern_against` to decide whether a bare
    /// PascalCase name is a unit-variant *pattern* (which the interpreter's
    /// structural matcher treats it as) rather than a fresh binding.
    /// (B-2026-07-17-6.)
    fn is_known_unit_variant(&self, name: &str) -> bool {
        self.env.enums.values().any(|e| {
            e.variants
                .iter()
                .any(|(vn, info)| vn == name && matches!(info, VariantTypeInfo::Unit))
        })
    }

    /// True when `expected` is a fully-resolved type that provably cannot be
    /// destructured by an enum-variant pattern named `variant_name` — i.e.
    /// the pattern/scrutinee pairing is a genuine type error, not an
    /// error-cascade artifact. Conservative by design: returns `false` for
    /// `Type::Error`, unresolved inference variables, generic type
    /// parameters, and any exotic/opaque type (`Shared`, `Rc`, `Arc`,
    /// pointers, existentials, associated-type projections, …), so
    /// error-cascade suppression and generic code are never falsely accused.
    /// (B-2026-07-17-6.)
    fn variant_pattern_scrutinee_mismatch(&self, expected: &Type, variant_name: &str) -> bool {
        // Peel borrow layers: `ScrutineeMode::classify` strips one, and
        // generic instantiation can nest `ref ref T`.
        let mut ty = expected;
        while let Type::Ref(inner) | Type::MutRef(inner) = ty {
            ty = inner;
        }
        match ty {
            // Primitives / structural aggregates / callables: an enum-variant
            // pattern can never match any of these.
            Type::Int(_)
            | Type::UInt(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Unit
            | Type::Tuple(_)
            | Type::Array { .. }
            | Type::Vector { .. }
            | Type::Slice { .. }
            | Type::Function { .. }
            | Type::OnceFunction { .. } => true,
            Type::Named { name, .. } => {
                if let Some(info) = self.env.enums.get(name) {
                    // An enum: a mismatch iff it does not declare the variant.
                    !info.variants.iter().any(|(vn, _)| vn == variant_name)
                } else if self.env.structs.contains_key(name) {
                    // A known struct: a variant pattern cannot destructure it.
                    true
                } else {
                    // Unknown / unresolved `Named` (a type alias, a builtin
                    // collection, or a not-yet-registered name): stay silent.
                    false
                }
            }
            // `Type::Error`, `TypeVar`, `TypeParam`, `Shared`, `Rc`, `Arc`,
            // `Weak`, `Pointer`, `AssocProjection`, `Existential`, `Shape`,
            // `Never`, … — conservative silence.
            _ => false,
        }
    }

    /// Emit the `PatternScrutineeMismatch` type error for an enum-variant
    /// pattern (`ctor_disp` — e.g. `Some(..)`, `Color.Red`, `None`) matched
    /// against a scrutinee whose type cannot own the `variant_name` variant.
    /// (B-2026-07-17-6.)
    fn emit_pattern_scrutinee_mismatch(
        &mut self,
        ctor_disp: &str,
        variant_name: &str,
        expected: &Type,
        span: &Span,
    ) {
        self.type_error(
            format!(
                "pattern `{ctor_disp}` is an enum-variant pattern, but the \
                 scrutinee has type `{ty}` — `{ty}` is not an enum with a \
                 `{variant_name}` variant",
                ty = type_display(expected),
            ),
            span.clone(),
            TypeErrorKind::PatternScrutineeMismatch,
        );
    }

    /// Resolve + validate a range pattern's bounds (design.md § Range
    /// Patterns, v60 item 51 slices 3–5). Const-named bounds resolve via
    /// `eval_const_expr`; failures emit `E_RANGE_PATTERN_BOUND_NOT_CONST`.
    /// Once both bounds resolve, enforce type-matching (same int width or
    /// both `char`) and bound-ordering (lower must not exceed upper).
    fn check_range_pattern_bounds(
        &mut self,
        start: Option<&RangeBound>,
        end: Option<&RangeBound>,
        inclusive: bool,
        scrut_ty: &Type,
        span: &Span,
    ) {
        let lo = start.and_then(|b| self.resolve_range_bound_value(b, scrut_ty));
        let hi = end.and_then(|b| self.resolve_range_bound_value(b, scrut_ty));

        if let (Some(lo), Some(hi)) = (&lo, &hi) {
            // Type-matching (slice 5): char-vs-int family mismatch, or two
            // explicitly-typed integer bounds of differing width, are
            // rejected. A width-agnostic literal (`0`, no suffix) adopts
            // the other bound's width and never conflicts.
            if range_bounds_type_conflict(lo, hi) {
                self.type_error(
                    format!(
                        "range pattern bounds must have the same type; got '{}' and '{}'",
                        type_display(&lo.1),
                        type_display(&hi.1),
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
            // Bound-ordering (slice 4): lower bound must not exceed upper.
            if lo.0 > hi.0 {
                self.type_error(
                    format!(
                        "range pattern lower bound ({}) must not exceed its upper bound ({})",
                        lo.0, hi.0,
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            } else if !inclusive && lo.0 == hi.0 {
                self.type_error(
                    format!(
                        "exclusive range pattern is empty: lower bound ({}) equals upper bound \
                         ({}); use '..=' for an inclusive range or widen the bounds",
                        lo.0, hi.0,
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
    }

    /// Resolve one range bound to `(scalar key, type, explicit_width)`.
    /// `explicit_width` is `false` only for a suffix-less integer literal,
    /// whose width defers to the other bound / scrutinee. A `Path` bound
    /// must name a module-level integer or char const; otherwise emit
    /// `E_RANGE_PATTERN_BOUND_NOT_CONST` and return `None`.
    fn resolve_range_bound_value(
        &mut self,
        bound: &RangeBound,
        scrut_ty: &Type,
    ) -> Option<(i128, Type, bool)> {
        match bound {
            RangeBound::Literal(LiteralPattern::Integer(n, sfx)) => {
                let (ty, explicit) = match int_suffix_to_type(sfx) {
                    Some(t) => (t, true),
                    None => (Type::Int(IntSize::I64), false),
                };
                Some((*n as i128, ty, explicit))
            }
            RangeBound::Literal(LiteralPattern::Char(c)) => Some((*c as i128, Type::Char, true)),
            // The parser only admits integer / char / byte literals in
            // range-bound position, so other literal kinds are unreachable;
            // be defensive rather than panic.
            RangeBound::Literal(_) => None,
            RangeBound::Path { segments, span } => {
                let expr = Expr {
                    kind: if segments.len() == 1 {
                        ExprKind::Identifier(segments[0].clone())
                    } else {
                        ExprKind::Path {
                            segments: segments.clone(),
                            generic_args: None,
                        }
                    },
                    span: span.clone(),
                };
                let resolved = self
                    .eval_const_expr(&expr, scrut_ty)
                    .ok()
                    .and_then(|cv| {
                        let ty = super::const_eval::const_value_type(&cv);
                        super::const_eval::const_value_to_i128(&cv).map(|v| (v, ty))
                    })
                    .filter(|(_, ty)| matches!(ty, Type::Int(_) | Type::UInt(_) | Type::Char));
                match resolved {
                    Some((v, ty)) => Some((v, ty, true)),
                    None => {
                        self.type_error(
                            format!(
                                "range pattern bound '{}' must resolve to a module-level \
                                 integer or char const",
                                segments.join("."),
                            ),
                            span.clone(),
                            TypeErrorKind::RangePatternBoundNotConst,
                        );
                        None
                    }
                }
            }
        }
    }

    fn check_exhaustiveness(&mut self, scrutinee_type: &Type, arms: &[MatchArm], span: Span) {
        use crate::exhaustive::{check_match_exhaustive, unreachable_arms, ExhaustiveResult};
        for idx in unreachable_arms(scrutinee_type, arms, &self.env) {
            self.type_lint_warning(
                "unreachable match arm: pattern is fully covered by an earlier arm".to_string(),
                arms[idx].pattern.span.clone(),
                TypeErrorKind::UnreachableArm,
                "unreachable_arm",
            );
        }

        // Bounded-refinement finite-domain cap (design.md § Pattern
        // Exhaustiveness — "When B − A exceeds 1024 the compiler falls back
        // to requiring a wildcard and emits a lint suggesting an enum"). A
        // refinement like `type T = i64 where self >= 0 and self <= 5000` is
        // bounded but too wide to enumerate, so the exhaustiveness algorithm
        // treats it as open-domain (wildcard required); surface that with a
        // lint so the author can switch to an `enum`.
        if let Some(width) =
            crate::exhaustive::refinement_domain_too_wide(scrutinee_type, &self.env)
        {
            let name = match scrutinee_type {
                Type::Refinement { name, .. } | Type::Named { name, .. } => name.clone(),
                _ => String::new(),
            };
            self.type_lint_warning(
                format!(
                    "match on `{name}`: its refinement domain spans {} values (B − A = {width} \
                     > {cap}), too wide to enumerate for exhaustiveness — a wildcard arm is \
                     required. Consider an `enum` if you need exhaustive matching over this set.",
                    width + 1,
                    cap = crate::exhaustive::MAX_REFINEMENT_FINITE_DOMAIN,
                ),
                span.clone(),
                TypeErrorKind::RefinementDomainTooWide,
                "refinement_domain_too_wide",
            );
        }

        // `#[non_exhaustive]` slice 5 — cross-package enum match must
        // include a wildcard arm regardless of variant coverage. The
        // defining package may add variants without breaking source
        // compatibility, so outside-package consumers cannot enumerate
        // the current variant set exhaustively. Same-package matches
        // fall through to the strict variant-by-variant rule below.
        // The check fires BEFORE `check_match_exhaustive` so a missing
        // wildcard surfaces as the slice-5 diagnostic, not as a
        // generic "missing variant Vn" tail (which would mislead the
        // author into adding the listed variant rather than `_`).
        if let Type::Named { name, .. } = scrutinee_type {
            if let Some(enum_info) = self.env.enums.get(name) {
                // Variant names short-circuit `PatternKind::Binding`'s
                // catch-all classification: `Read` in pattern position
                // parses as `Binding("Read")` but is actually a
                // unit-variant constructor, not a fresh binding.
                let variant_names: std::collections::HashSet<&str> =
                    enum_info.variants.iter().map(|(n, _)| n.as_str()).collect();
                if enum_info.is_non_exhaustive
                    && enum_info.defining_stdlib_origin
                    && !self.current_fn_stdlib_origin
                    && !arms_contain_catchall(arms, &variant_names)
                {
                    let fix_it = non_exhaustive_match_fix_it(
                        &span,
                        arms,
                        "_ => panic(\"handle new variant\")",
                    );
                    self.type_error_with_fix_it(
                        format!(
                            "error[E_NON_EXHAUSTIVE_CROSS_PACKAGE_MATCH]: \
                             match on `{name}` must include a wildcard arm \
                             (`_ => ...`) — `{name}` is `#[non_exhaustive]` \
                             and defined in another package, so new variants \
                             may land without breaking source compatibility. \
                             Add `_ => panic(\"handle new variant\")` (or a \
                             real handler) before the closing brace. \
                             See design.md § `#[non_exhaustive]` for \
                             Evolvable Public Types."
                        ),
                        span.clone(),
                        TypeErrorKind::NonExhaustiveCrossPackageMatch,
                        fix_it,
                    );
                    return;
                }
            }
        }

        match check_match_exhaustive(scrutinee_type, arms, &self.env) {
            ExhaustiveResult::Exhaustive | ExhaustiveResult::Skipped => {}
            ExhaustiveResult::NonExhaustive { witness } => {
                // Preserve the prior diagnostic wording for bool and enum
                // scrutinees when the witness names a single top-level
                // constructor (no nested compound payload). Compound
                // witnesses and non-enum scrutinees use the pattern form.
                let is_simple_witness = !witness.contains('(') && !witness.contains('{');
                let message = match scrutinee_type {
                    Type::Bool if is_simple_witness => {
                        format!("non-exhaustive match on bool: missing {witness}")
                    }
                    Type::Named { name, .. }
                        if is_simple_witness && self.env.enums.contains_key(name) =>
                    {
                        format!("non-exhaustive match: missing variants: {witness}")
                    }
                    _ => format!("non-exhaustive match: pattern `{witness}` not covered"),
                };
                // For an enum scrutinee the witness renders as a valid arm
                // pattern (`Cancelled`, `Failed(_)`, `Point { .. }`, …), so the
                // missing arm can be synthesized as a machine-applicable fix.
                // Bool / integer-range / other witnesses stay descriptive.
                // `check_match_exhaustive` yields one witness at a time, so a
                // match missing several variants is fixed one arm per pass.
                let is_enum = matches!(
                    scrutinee_type,
                    Type::Named { name, .. } if self.env.enums.contains_key(name)
                );
                if is_enum {
                    let arm = format!("{witness} => todo()");
                    let fix_it = non_exhaustive_match_fix_it(&span, arms, &arm);
                    self.type_error_with_fix_it(
                        message,
                        span,
                        TypeErrorKind::NonExhaustiveMatch,
                        fix_it,
                    );
                } else {
                    self.type_error(message, span, TypeErrorKind::NonExhaustiveMatch);
                }
            }
        }
    }

    // ── Pattern Binding for Let ─────────────────────────────────

    /// Reverse direction of `lower_type_expr` for the subset needed by the
    /// pattern-binding sibling table (PB sibling slice 2026-05-09): convert
    /// a `Type` back to a synthetic `TypeExpr` so it can be forwarded
    /// through the lowering pass for codegen consumption (it lowers each
    /// surface element type back to an LLVM type via
    /// `llvm_type_for_type_expr`). Coverage: primitive integer / float /
    /// bool / char / str / unit, `Type::Named` (Vec, Slice, Map, struct,
    /// enum names), `Type::Tuple`, `Type::Array`, `Type::Slice`, `Type::Ref`,
    /// `Type::MutRef`, `Type::Shared`, `Type::Rc`, `Type::Arc`,
    /// `Type::TypeParam`. Pieces outside this set (function types, type
    /// vars, assoc projections, errors) fall back to `TypeKind::Error`,
    /// which `llvm_type_for_type_expr` lowers to i64 — adequate for the
    /// element-type registration use case (those payloads are not
    /// supported in pattern-bound Vec/Slice element positions today).
    pub(crate) fn type_to_type_expr(ty: &Type) -> TypeExpr {
        let span = Span::default();
        let path = |name: &str, args: Vec<TypeExpr>| TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![name.to_string()],
                generic_args: if args.is_empty() {
                    None
                } else {
                    Some(args.into_iter().map(GenericArg::Type).collect())
                },
                span: span.clone(),
            }),
            span: span.clone(),
        };
        match ty {
            Type::Int(IntSize::I8) => path("i8", vec![]),
            Type::Int(IntSize::I16) => path("i16", vec![]),
            Type::Int(IntSize::I32) => path("i32", vec![]),
            Type::Int(IntSize::I64) => path("i64", vec![]),
            Type::UInt(UIntSize::U8) => path("u8", vec![]),
            Type::UInt(UIntSize::U16) => path("u16", vec![]),
            Type::UInt(UIntSize::U32) => path("u32", vec![]),
            Type::UInt(UIntSize::U64) => path("u64", vec![]),
            Type::UInt(UIntSize::Usize) => path("usize", vec![]),
            // f16/bf16 included (B-2026-07-20-12): a tuple binding's recorded
            // `(f16, f16)` TypeExpr must lower its elements as `half`/`bfloat`,
            // not fall to the unknown-name i64 default.
            Type::Float(FloatSize::F16) => path("f16", vec![]),
            Type::Float(FloatSize::BF16) => path("bf16", vec![]),
            Type::Float(FloatSize::F32) => path("f32", vec![]),
            Type::Float(FloatSize::F64) => path("f64", vec![]),
            Type::Bool => path("bool", vec![]),
            Type::Char => path("char", vec![]),
            Type::Str => path("str", vec![]),
            Type::Unit => TypeExpr {
                kind: TypeKind::Unit,
                span,
            },
            Type::Named { name, args } => {
                // Args are usually plain types, but a `Tensor[T, Shape]`
                // carries a `Type::Shape(dims)` second arg that must round-
                // trip as `GenericArg::Shape`, not `GenericArg::Type` — else
                // the reconstructed Tensor loses its shape (the Shape arm
                // below would hit the `_ => Error` fallback) and downstream
                // `tensor_var_info_from_type_expr` rejects it. Used by
                // `owned_temp_drops` for `Vec[Tensor]` (the iter_axis
                // result) so its for-loop element binds as a tensor.
                let generic_args: Vec<GenericArg> = args
                    .iter()
                    .map(|a| match a {
                        Type::Shape(dims) => GenericArg::Shape(ShapeLit {
                            dims: dims
                                .iter()
                                .map(|d| Self::dim_arg_to_shape_dim(d, &span))
                                .collect(),
                            span: span.clone(),
                        }),
                        other => GenericArg::Type(Self::type_to_type_expr(other)),
                    })
                    .collect();
                TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec![name.clone()],
                        generic_args: if generic_args.is_empty() {
                            None
                        } else {
                            Some(generic_args)
                        },
                        span: span.clone(),
                    }),
                    span,
                }
            }
            Type::Shared(name) => path(name, vec![]),
            Type::Rc(inner) => path("Rc", vec![Self::type_to_type_expr(inner)]),
            Type::Arc(inner) => path("Arc", vec![Self::type_to_type_expr(inner)]),
            Type::Tuple(elems) => TypeExpr {
                kind: TypeKind::Tuple(elems.iter().map(Self::type_to_type_expr).collect()),
                span,
            },
            Type::Array { element, size } => TypeExpr {
                kind: TypeKind::Array {
                    element: Box::new(Self::type_to_type_expr(element)),
                    size: Box::new(Expr {
                        kind: ExprKind::Integer(size.as_literal().unwrap_or(0), None),
                        span: span.clone(),
                    }),
                },
                span,
            },
            Type::Slice { element, mutable } => {
                let inner = Box::new(Self::type_to_type_expr(element));
                if *mutable {
                    TypeExpr {
                        kind: TypeKind::MutSlice(inner),
                        span,
                    }
                } else {
                    path("Slice", vec![*inner])
                }
            }
            Type::Ref(inner) => TypeExpr {
                kind: TypeKind::Ref(Box::new(Self::type_to_type_expr(inner))),
                span,
            },
            Type::MutRef(inner) => TypeExpr {
                kind: TypeKind::MutRef(Box::new(Self::type_to_type_expr(inner))),
                span,
            },
            Type::Weak(inner) => TypeExpr {
                kind: TypeKind::Weak(Box::new(Self::type_to_type_expr(inner))),
                span,
            },
            Type::TypeParam(name) => path(name, vec![]),
            // Function / OnceFunction → `Fn(..)` / `OnceFn(..)` so a fn-value's
            // signature round-trips (B-2026-06-21-3): codegen lowers the
            // resulting `FnType` to the closure fat pointer and reads the
            // params/return to build the env-first indirect-call signature. The
            // `Unit` return round-trips as `TypeKind::Unit`, which
            // `closure_abi_fn_type` maps to a `void` return.
            Type::Function {
                params,
                return_type,
            }
            | Type::OnceFunction {
                params,
                return_type,
            } => TypeExpr {
                kind: TypeKind::FnType {
                    params: params.iter().map(Self::type_to_type_expr).collect(),
                    return_type: Some(Box::new(Self::type_to_type_expr(return_type))),
                    effect_spec: None,
                    is_once: matches!(ty, Type::OnceFunction { .. }),
                },
                span,
            },
            // Fallback for shapes that don't have a clean TypeExpr round-trip
            // (TypeVar, Pointer, AssocProjection, Error). The element-type
            // registration use case never sees these as Vec[T] / Slice[T] inner
            // types in a well-typed program, so falling back to Error → i64
            // lowering is safe.
            _ => TypeExpr {
                kind: TypeKind::Error,
                span,
            },
        }
    }

    /// Reconstruct a shape-literal dim (`ShapeDim`) from a typechecker
    /// `DimArg`. Literal dims round-trip to integer-literal `Const`s
    /// (codegen folds them); named dim params to identifier `Const`s;
    /// every dynamic / metavariable form to `?`; splices to `...IDENT`.
    /// Codegen only distinguishes "concrete literal" (static dim) from
    /// everything else (header-read), and rejects splice-bearing shapes
    /// wholesale, so the non-literal mappings need only preserve that
    /// coarse classification.
    fn dim_arg_to_shape_dim(d: &DimArg, span: &Span) -> ShapeDim {
        match d {
            DimArg::Const(ConstArg::Literal(v)) => ShapeDim::Const(Box::new(Expr {
                kind: ExprKind::Integer(*v, None),
                span: span.clone(),
            })),
            DimArg::Const(ConstArg::ConstParam(name)) => ShapeDim::Const(Box::new(Expr {
                kind: ExprKind::Identifier(name.clone()),
                span: span.clone(),
            })),
            DimArg::Splice(name) => ShapeDim::Splice {
                name: name.clone(),
                span: span.clone(),
            },
            DimArg::SpliceVar(_) => ShapeDim::Splice {
                name: "_splice".to_string(),
                span: span.clone(),
            },
            // ConstVar / DynamicDim / Dynamic — all runtime dims.
            _ => ShapeDim::Dynamic { span: span.clone() },
        }
    }

    /// If `ty` is `Vec[T]` / `Slice[T]` / `mut Slice[T]`, record the inner
    /// element type at `pattern.span` in the sibling table so codegen can
    /// register it under the binding's variable name (`vec_elem_types` /
    /// `slice_elem_types`). For `Type::Slice` (which isn't a `Type::Named`
    /// shape), also write the canonical `"Slice"` surface name into the
    /// String-name table so codegen's `bind_pattern_values` knows which
    /// element-type registry to populate. `Vec` already gets a String-name
    /// entry from the existing `Type::Named` write. PB sibling slice
    /// (2026-05-09).
    /// Stamp the pattern-binding span with the borrow form derived from
    /// the enclosing `ScrutineeMode`. Owned mode produces no entry. The
    /// `dispatch_ty` is the unwrapped binding type (i.e., the type
    /// _before_ `wrap_binding_ty` re-wraps it) — used to skip recording
    /// when the field's own type is already a borrow shape, mirroring
    /// `wrap_binding_ty`'s pass-through rule (a `ref FieldT` field
    /// through a `ref Container` scrutinee stays `ref FieldT`, not
    /// `ref ref FieldT`, so the codegen shim must not wrap it again).
    fn record_pattern_binding_borrow_mode(
        &mut self,
        span: &Span,
        mode: ScrutineeMode,
        dispatch_ty: &Type,
    ) {
        let borrow = match mode {
            ScrutineeMode::Owned => return,
            ScrutineeMode::Ref => crate::ast::PatternBindingBorrow::Ref,
            ScrutineeMode::MutRef => crate::ast::PatternBindingBorrow::MutRef,
        };
        // Skip already-borrow leaf shapes (parity with
        // `ScrutineeMode::wrap_binding_ty`'s identity arm). A struct
        // field declared `ref T` / `mut ref T` / `Slice[T]` keeps its
        // own borrow form — codegen would already lower it as a
        // pointer / slice header, so the ref-shim would double-wrap.
        if matches!(
            dispatch_ty,
            Type::Ref(_) | Type::MutRef(_) | Type::Slice { .. }
        ) {
            return;
        }
        self.pattern_binding_borrow_modes
            .insert(SpanKey::from_span(span), borrow);
    }

    /// Built-in generic type names that codegen lowers through a DEDICATED
    /// path (`llvm_type_for_type_expr`'s special cases +
    /// `register_var_from_type_expr`'s early arms), NOT the generic
    /// `mono_struct_type` route that plain user/baked structs take. Their baked
    /// stdlib struct shells live in `env.structs`, so the generic-struct
    /// payload-binding recorder must skip them — recording their inner type
    /// (and its early `return`) would preempt the Vec/Slice/Map element-type
    /// side-table recording and mis-lower a plain `let v = [1, 2, 3]`
    /// (B-2026-07-12-2). Kept in sync with the special-cased names in
    /// `src/codegen/types_lowering.rs`.
    fn is_special_lowered_generic_builtin(name: &str) -> bool {
        matches!(
            name,
            "Vec"
                | "VecDeque"
                | "Slice"
                | "Array"
                | "Vector"
                | "Map"
                | "HashMap"
                | "SortedMap"
                | "Set"
                | "HashSet"
                | "SortedSet"
                | "String"
                | "Atomic"
                | "Mutex"
                | "VolatileCell"
                | "OnceLock"
                | "OnceCell"
                | "Tensor"
                | "Column"
                | "DataFrame"
                | "Sender"
                | "Receiver"
                | "Channel"
                | "File"
                | "Request"
        )
    }

    fn record_pattern_inner_type(&mut self, pattern: &Pattern, ty: &Type) {
        // Tuple bindings (e.g. `Some(node)` where `node: (i64, i64)`):
        // record the whole tuple `TypeExpr` so codegen can reconstruct
        // a tuple struct from the multi-word payload. Without this,
        // `pattern_binding_types` skips anonymous tuple shapes and
        // the codegen's `reconstruct_payload_value` Binding arm falls
        // through to single-word — the downstream `let (a, b) = node`
        // then fails because `node` isn't a struct value.
        if let Type::Tuple(_) = ty {
            let tup_te = Self::type_to_type_expr(ty);
            self.pattern_binding_inner_types
                .insert(SpanKey::from_span(&pattern.span), tup_te);
            self.pattern_binding_types
                .insert(SpanKey::from_span(&pattern.span), "Tuple".to_string());
            return;
        }
        // `Map[K, V]` / `Set[T]` payload binding (e.g.
        // `match opt { Some(m) => m.len() }`): record the FULL collection
        // `TypeExpr` — like the Tuple arm above, NOT the inner-element form
        // used for Vec/Slice below — so codegen can route the binding through
        // `register_var_from_type_expr`, which extracts the K/V (or elem) LLVM
        // types into `map_key_types` / `map_val_types` / `set_elem_types`.
        // Without this, a Map/Set bound by a match arm carries no dispatch
        // side-tables and `m.len()` / `s.contains(x)` fails codegen with
        // "no handler for method". The raw `Type` is stashed for
        // `finalize_pattern_binding_inner_types` so a still-unsolved K/V/elem
        // typevar re-resolves the same way the Vec arm's does.
        if let Type::Named { name, args } = ty {
            // The whole `Map`/`Set` family — `SortedMap` / `SortedSet` share
            // `Map` / `Set`'s K/V/elem extraction (`extract_map_kv_types` /
            // `extract_set_elem_type` accept both), so an enum-payload
            // `SortedMap` binding needs the same full-TypeExpr recording or its
            // codegen dispatch tables are never registered and `m.len()` fails
            // "no handler for method" — the SortedMap leg of B-2026-07-23-3.
            if ((name == "Map" || name == "SortedMap") && args.len() == 2)
                || ((name == "Set" || name == "SortedSet") && args.len() == 1)
            {
                let key = SpanKey::from_span(&pattern.span);
                self.pattern_binding_inner_types
                    .insert(key, Self::type_to_type_expr(ty));
                self.pattern_binding_inner_unresolved
                    .insert(key, ty.clone());
                self.pattern_binding_types.insert(key, name.clone());
                return;
            }
            // B-2026-07-08-9: record `Option[T]` / `Result[T, E]` bindings the
            // same way (full `TypeExpr` + surface name) so codegen can recover
            // the concrete payload type for the Display path (`Some(<T>)`/`None`,
            // `Ok`/`Err`) — Option/Result are generic built-ins whose variant
            // defs carry only `T`, so without the concrete binding type codegen
            // cannot render them (it did via the interpreter only). Dispatch
            // side-table only; no interpreter/typecheck behaviour changes.
            if (name == "Option" && args.len() == 1) || (name == "Result" && args.len() == 2) {
                let key = SpanKey::from_span(&pattern.span);
                self.pattern_binding_inner_types
                    .insert(key, Self::type_to_type_expr(ty));
                self.pattern_binding_inner_unresolved
                    .insert(key, ty.clone());
                self.pattern_binding_types.insert(key, name.clone());
                return;
            }
            // A concretely-instantiated GENERIC user struct payload binding
            // (`Err(e)` where `e: Wrap[String]`, `AlreadySetError[String]`):
            // record the full instantiation `TypeExpr` so codegen recovers the
            // MONO field layout (word count + field GEP) rather than truncating
            // to the all-`i64` generic base. Without this a heap-bearing
            // generic-struct payload moved out of an `Option`/`Result` silently
            // miscompiles — the 3-word `String` field collapses to a single
            // word (B-2026-07-12-2 heap-recovery gap; the true blocker for the
            // OnceLock heap-`T` ungate). A NON-generic struct already resolves
            // via `struct_types` under its concrete name, so only the generic
            // instantiation is lost. The borrow form (`ref Wrap[..]`, peeled
            // upstream by `record_pattern_binding_surface_types`) reconstructs
            // the same full-width by-value aliasing view, so recording it too is
            // correct (its cleanup is separately borrow-suppressed at codegen).
            //
            // EXCLUDE built-in generic types with DEDICATED codegen lowering
            // (`Vec`/`Map`/`Atomic`/`OnceLock`/…): `env.structs` also holds the
            // baked stdlib struct shells for those, but codegen lowers them via
            // special-cased paths (NOT `mono_struct_type`), and this arm's early
            // `return` would otherwise preempt the Vec/Slice element-type
            // recording below — stripping `v.len()`/`v[i]` dispatch side-tables
            // and crashing a plain `let v = [1, 2, 3]`. The codegen consumers are
            // already gated on `is_generic_named_struct_type_expr`
            // (`struct_generic_params`, which excludes these), so this only
            // matters for the preemption. Mirrors `llvm_type_for_type_expr`'s
            // special-cased names.
            if !args.is_empty()
                && self.env.structs.contains_key(name)
                && !Self::is_special_lowered_generic_builtin(name)
            {
                let key = SpanKey::from_span(&pattern.span);
                self.pattern_binding_inner_types
                    .insert(key, Self::type_to_type_expr(ty));
                self.pattern_binding_inner_unresolved
                    .insert(key, ty.clone());
                // `pattern_binding_types[key]` is already the struct base name,
                // set by `record_pattern_binding_surface_types`.
                return;
            }
        }
        let (elem, name): (Option<&Type>, Option<&'static str>) = match ty {
            // `VecDeque[T]` shares `Vec[T]`'s `{ptr, len, cap}` codegen
            // layout (see `extract_vec_elem_type`), so record it under
            // the same sibling-table path. Without this arm, an untyped
            // `let mut q = VecDeque.new();` leaves `vec_elem_types`
            // empty and `q.push_back(x)` falls through method dispatch.
            Type::Named { name, args }
                if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
            {
                (Some(&args[0]), None)
            }
            Type::Slice { element, .. } => (Some(element.as_ref()), Some("Slice")),
            _ => (None, None),
        };
        if let Some(elem_ty) = elem {
            let elem_te = Self::type_to_type_expr(elem_ty);
            let key = SpanKey::from_span(&pattern.span);
            self.pattern_binding_inner_types.insert(key, elem_te);
            // Stash the raw `Type` so `finalize_pattern_binding_inner_types`
            // can re-resolve typevars after the function body completes.
            // Bindings whose element type contains a typevar (e.g. `let mut
            // q = VecDeque.new();` — `?T` solved later by `q.push_back(x)`)
            // depend on this re-resolution to surface the right TypeExpr.
            self.pattern_binding_inner_unresolved
                .insert(key, elem_ty.clone());
            if let Some(canon_name) = name {
                self.pattern_binding_types
                    .insert(SpanKey::from_span(&pattern.span), canon_name.to_string());
            }
        }
    }

    /// Resolve any `Type::TypeVar` entries captured by
    /// `record_pattern_inner_type` against `env.substitutions` and
    /// overwrite the public `pattern_binding_inner_types` table. Runs
    /// once at the end of `check`, after every function body has been
    /// inferred and all typevars have either been solved or surfaced as
    /// "cannot infer" diagnostics.
    pub(super) fn finalize_pattern_binding_inner_types(&mut self) {
        let id_to_name: HashMap<TypeVarId, String> = HashMap::new();
        let const_id_to_name: HashMap<ConstVarId, String> = HashMap::new();
        let updates: Vec<(SpanKey, TypeExpr)> = self
            .pattern_binding_inner_unresolved
            .iter()
            .map(|(key, ty)| {
                let resolved = resolve_type_vars(
                    ty,
                    &self.env.substitutions,
                    &id_to_name,
                    &self.env.const_substitutions,
                    &const_id_to_name,
                );
                (*key, Self::type_to_type_expr(&resolved))
            })
            .collect();
        for (key, te) in updates {
            self.pattern_binding_inner_types.insert(key, te);
        }
    }

    /// Call-site-driven closure param inference (B-2026-07-12-20). A closure
    /// whose un-annotated param was solved only at a LATER call site — e.g.
    /// `let id = |x| x; id(5)` — had its `Fn(?T0) -> ?T0` recorded in
    /// `expr_types` at the closure literal, before the call solved `?T0`.
    /// Re-resolve every recorded `Function` / `OnceFunction` type against the
    /// now-final substitutions so codegen (which reads closure signatures from
    /// `fn_value_typed_exprs`, built in lowering from `expr_types`) sees the
    /// concrete param/return types instead of defaulting an unresolved param to
    /// `i64`. Mirrors `finalize_pattern_binding_inner_types`; runs at the same
    /// finalize point. Only `Fn`-typed entries whose resolution actually changes
    /// are rewritten, so the common (already-concrete) case is untouched.
    pub(super) fn finalize_closure_expr_types(&mut self) {
        let id_to_name: HashMap<TypeVarId, String> = HashMap::new();
        let const_id_to_name: HashMap<ConstVarId, String> = HashMap::new();
        let updates: Vec<(SpanKey, Type)> = self
            .expr_types
            .iter()
            .filter_map(|(key, ty)| {
                let core = match ty {
                    Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
                    other => other,
                };
                if !matches!(core, Type::Function { .. } | Type::OnceFunction { .. }) {
                    return None;
                }
                let resolved = resolve_type_vars(
                    ty,
                    &self.env.substitutions,
                    &id_to_name,
                    &self.env.const_substitutions,
                    &const_id_to_name,
                );
                (resolved != *ty).then_some((*key, resolved))
            })
            .collect();
        for (key, ty) in updates {
            self.expr_types.insert(key, ty);
        }
    }

    pub(super) fn bind_pattern_types(&mut self, pattern: &Pattern, ty: &Type) {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                self.local_scope.insert(name.clone(), ty.clone());
                // Record the surface type for codegen so it can reconstitute
                // struct payloads from the i64 word at match-arm bind sites
                // (see TypeCheckResult.pattern_binding_types). Named types
                // record under their canonical name; `Type::Str` registers
                // its 3-word `String` surface name parallel to how
                // `Type::Named { name: "Vec" }` registers `"Vec"` —
                // required by the tuple-payload destructure path
                // (`pattern_payload_word_count`) which needs to slice a
                // flat tuple payload into per-element word ranges.
                // Other primitives and references stay unrecorded — their
                // 1-word default matches their actual layout.
                //
                // A refinement (`type Email = String where …`) records its
                // *base*'s surface name — codegen dispatches a refined value
                // as its base (phase-9 step 5a); `local_scope` above keeps
                // the real refinement type for type-checking.
                let ty = strip_refinement(ty);
                if let Type::Named {
                    name: type_name, ..
                } = ty
                {
                    self.pattern_binding_types
                        .insert(SpanKey::from_span(&pattern.span), type_name.clone());
                } else if matches!(ty, Type::Str) {
                    self.pattern_binding_types
                        .insert(SpanKey::from_span(&pattern.span), "String".to_string());
                } else if let Type::Shared(type_name) = ty {
                    // Parallel to `check_pattern_against`: register the
                    // bare struct name so codegen's `shared_type_for_expr`
                    // lookup resolves `node.field` access on a pattern-
                    // bound shared-struct handle.
                    self.pattern_binding_types
                        .insert(SpanKey::from_span(&pattern.span), type_name.clone());
                } else if matches!(ty, Type::Bool) {
                    // Enum-payload bool extraction (e.g. `Json.Bool(b) => b`):
                    // the variant payload word is i64 in the word stream
                    // but the binding's surface type is bool (i1). Without
                    // this record, `reconstruct_payload_value` returns the
                    // i64 word as-is and the function return path trips
                    // the LLVM verifier ("ret i64 / expected i1"). Codegen
                    // sees "bool" and inserts a `trunc i64 → i1` before
                    // creating the binding's alloca.
                    self.pattern_binding_types
                        .insert(SpanKey::from_span(&pattern.span), "bool".to_string());
                } else if matches!(ty, Type::Pointer { .. }) {
                    // Raw-pointer surface name (`*const T` / `*mut T`) so
                    // `raii_check` can recognise raw-pointer-typed
                    // bindings held across a yield point and reject them
                    // as NOT-CancelSafe per the design spec (raw pointers
                    // carry no Drop hook — a cancel during their live
                    // range leaks whatever they reference). Codegen
                    // ignores this entry; raw-pointer layout is fixed.
                    self.pattern_binding_types
                        .insert(SpanKey::from_span(&pattern.span), type_display(ty));
                } else if let Some(narrow) = narrow_int_surface_name(ty) {
                    // Sub-64-bit int / `char` payload binding (e.g.
                    // `let Some(b) = vec_u8.pop()`): record the surface
                    // name so `reconstruct_payload_value` truncates the
                    // i64 payload word back to the binding's real width.
                    // Match-arm parallel lives in `check_pattern_against`.
                    self.pattern_binding_types
                        .insert(SpanKey::from_span(&pattern.span), narrow);
                }
                // PB sibling slice (2026-05-09): record the inner element
                // type for `Vec[T]` / `Slice[T]` bindings so codegen can
                // register the LLVM elem type under the binding's variable
                // name (vec_elem_types / slice_elem_types). Lights up
                // direct method dispatch (`xs.len()`, `xs[0]`, `xs.push(...)`)
                // on a pattern-bound collection payload without needing
                // function-arg routing as a work-around.
                self.record_pattern_inner_type(pattern, ty);
            }
            PatternKind::Tuple(patterns) => {
                if let Type::Tuple(types) = ty {
                    if patterns.len() != types.len() {
                        self.type_error(
                            format!(
                                "tuple pattern has {} element(s) but type has {}",
                                patterns.len(),
                                types.len()
                            ),
                            pattern.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        for pat in patterns {
                            self.bind_pattern_types(pat, &Type::Error);
                        }
                    } else {
                        for (pat, t) in patterns.iter().zip(types.iter()) {
                            self.bind_pattern_types(pat, t);
                        }
                    }
                } else if *ty != Type::Error {
                    self.type_error(
                        format!("tuple pattern used but type is `{}`", type_display(ty)),
                        pattern.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    for pat in patterns {
                        self.bind_pattern_types(pat, &Type::Error);
                    }
                } else {
                    for pat in patterns {
                        self.bind_pattern_types(pat, &Type::Error);
                    }
                }
            }
            PatternKind::Struct {
                path,
                fields,
                has_rest,
            } => {
                let struct_name = path.last().map(String::as_str).unwrap_or("");

                // `#[non_exhaustive]` slice 4 pattern half (let-binding
                // route) — same check as the match-arm site in
                // `check_pattern_against`. `let X { ... } = e;` goes
                // through `bind_pattern_types`, not `check_pattern_against`,
                // so the slice-4 rule must fire here too. Without this,
                // exhaustive let-destructures of cross-package
                // non-exhaustive structs would silently slip through.
                if let Some(info) = self.env.structs.get(struct_name) {
                    if info.is_non_exhaustive
                        && info.defining_stdlib_origin
                        && !self.current_fn_stdlib_origin
                        && !*has_rest
                    {
                        let fix_it = non_exhaustive_pattern_fix_it(&pattern.span, fields);
                        self.type_error_with_fix_it(
                            format!(
                                "error[E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN]: \
                                 cannot exhaustively destructure `{name}` — \
                                 `{name}` is `#[non_exhaustive]` and defined \
                                 in another package, so its field set may \
                                 grow. Add `..` before the closing `}}` to \
                                 leave room for future fields. See design.md \
                                 § `#[non_exhaustive]` for Evolvable Public \
                                 Types.",
                                name = struct_name
                            ),
                            pattern.span.clone(),
                            TypeErrorKind::NonExhaustiveCrossPackagePattern,
                            fix_it,
                        );
                    }
                }

                let field_source_ty = if let Type::Named { name, .. } = ty {
                    if name == struct_name || ty == &Type::Error {
                        Some(name.clone())
                    } else if *ty != Type::Error {
                        self.type_error(
                            format!(
                                "struct pattern `{}` used but type is `{}`",
                                struct_name,
                                type_display(ty)
                            ),
                            pattern.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        None
                    } else {
                        None
                    }
                } else if *ty != Type::Error {
                    self.type_error(
                        format!(
                            "struct pattern `{}` used but type is `{}`",
                            struct_name,
                            type_display(ty)
                        ),
                        pattern.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    None
                } else {
                    None
                };
                for field in fields {
                    let field_ty = if let Some(ref sname) = field_source_ty {
                        if let Some(info) = self.env.structs.get(sname) {
                            if let Some((_, t, _)) =
                                info.fields.iter().find(|(n, _, _)| n == &field.name)
                            {
                                t.clone()
                            } else {
                                self.type_error(
                                    format!(
                                        "no field `{}` found on struct `{}`",
                                        field.name, sname
                                    ),
                                    field.span.clone(),
                                    TypeErrorKind::UndefinedField,
                                );
                                Type::Error
                            }
                        } else {
                            Type::Error
                        }
                    } else {
                        Type::Error
                    };
                    if let Some(ref sub) = field.pattern {
                        self.bind_pattern_types(sub, &field_ty);
                    } else {
                        self.local_scope.insert(field.name.clone(), field_ty);
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. } => {
                for pat in patterns {
                    self.bind_pattern_types(pat, &Type::Error);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
            PatternKind::AtBinding { name, pattern, .. } => {
                self.local_scope.insert(name.clone(), ty.clone());
                self.bind_pattern_types(pattern, ty);
            }
            PatternKind::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.bind_pattern_types(first, ty);
                }
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let (element_ty, rest_ty) =
                    self.slice_pattern_types(prefix, rest, suffix, ty, &pattern.span);
                for pat in prefix.iter().chain(suffix.iter()) {
                    self.bind_pattern_types(pat, &element_ty);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    self.local_scope.insert(name.clone(), rest_ty);
                }
            }
        }
    }

    // ── Irrefutability Check for Parameter Patterns ──────────────

    /// Returns true if `pat` is irrefutable for a value of type `ty`. Prefers
    /// the Maranget machinery (`U([PAT], _) == false`) when the type is in
    /// the handled set; falls back to the legacy syntactic check on
    /// `ref`/`function`/`typeparam`/etc. types that Maranget skips. Slice 6
    /// of the exhaustiveness upgrade.
    pub(super) fn is_irrefutable_pattern(&self, pat: &Pattern, ty: &Type) -> bool {
        match crate::exhaustive::is_pattern_irrefutable(pat, ty, &self.env) {
            Some(b) => b,
            None => self.is_irrefutable_param_pattern(pat),
        }
    }

    /// Legacy syntactic refutability check. Retained as the fallback for
    /// types Maranget doesn't reason about (refs, function values, generic
    /// parameters, etc.). Prefer `is_irrefutable_pattern` when a type is
    /// available.
    fn is_irrefutable_param_pattern(&self, pat: &Pattern) -> bool {
        match &pat.kind {
            PatternKind::Binding(_) | PatternKind::Wildcard => true,
            PatternKind::Tuple(patterns) => patterns
                .iter()
                .all(|p| self.is_irrefutable_param_pattern(p)),
            PatternKind::Struct {
                path,
                fields,
                has_rest: _,
            } => {
                // A struct pattern is irrefutable only if the name refers to a
                // struct type (not an enum variant). Enum variant names are
                // refutable — they only match one branch.
                if path.len() == 1 {
                    let name = &path[0];
                    if self.env.structs.contains_key(name) {
                        // Known struct — irrefutable iff all field sub-patterns are
                        fields.iter().all(|f| {
                            f.pattern
                                .as_ref()
                                .is_none_or(|p| self.is_irrefutable_param_pattern(p))
                        })
                    } else if self
                        .env
                        .enums
                        .values()
                        .any(|e| e.variants.iter().any(|(v, _)| v == name))
                    {
                        // Known enum variant name — refutable
                        false
                    } else {
                        // Unknown name — let type errors surface; treat as irrefutable
                        // to avoid double-diagnosing the same source mistake.
                        true
                    }
                } else {
                    false
                }
            }
            PatternKind::AtBinding { pattern, .. } => self.is_irrefutable_param_pattern(pattern),
            PatternKind::Literal(_)
            | PatternKind::RangePattern { .. }
            | PatternKind::TupleVariant { .. }
            | PatternKind::Or(_) => false,
            // Slice patterns are refutable in general — `[]` does not cover
            // `Vec[T]`, and any non-`..` prefix/suffix narrows the matched
            // length class. Sub-item 2 will refine this to `irrefutable iff
            // every nested pattern is irrefutable` for `Array[T, N]` only.
            PatternKind::Slice { .. } => false,
        }
    }

    /// Emit a `RefutablePattern` error if `param`'s pattern is refutable
    /// for its declared type.
    pub(super) fn check_param_irrefutable(&mut self, param: &Param, ty: &Type) {
        if !self.is_irrefutable_pattern(&param.pattern, ty) {
            self.type_error(
                "refutable pattern in function parameter; use `if let` or `match` for patterns that may not match".to_string(),
                param.pattern.span.clone(),
                TypeErrorKind::RefutablePattern,
            );
        }
    }
}

/// Slice 5 helper — does the arm list include a catch-all pattern?
/// `_` (Wildcard) is the canonical form; a bare binding (`x`) whose
/// name is NOT one of the scrutinee enum's variants without a guard
/// also catches every value and counts as a catch-all. Or-patterns
/// whose alternatives include a wildcard, and `name @ _` at-bindings,
/// are also catch-alls. A guarded arm (`x if cond =>`) is NOT a
/// catch-all because the guard can fail.
///
/// `variant_names` is the scrutinee enum's variant-name set:
/// `PatternKind::Binding("Read")` matches a `Read` variant constructor
/// when `Read` is in the set (not a catch-all) but a free binding
/// otherwise (catch-all). The parser cannot distinguish these at
/// parse time — `Binding(name)` is the surface for both — so the
/// typechecker discriminates here using its env.
fn arms_contain_catchall(
    arms: &[MatchArm],
    variant_names: &std::collections::HashSet<&str>,
) -> bool {
    arms.iter()
        .any(|arm| arm.guard.is_none() && pattern_is_catchall(&arm.pattern, variant_names))
}

fn pattern_is_catchall(p: &Pattern, variant_names: &std::collections::HashSet<&str>) -> bool {
    match &p.kind {
        PatternKind::Wildcard => true,
        PatternKind::Binding(name) => !variant_names.contains(name.as_str()),
        PatternKind::AtBinding { pattern, .. } => pattern_is_catchall(pattern, variant_names),
        PatternKind::Or(alts) => alts.iter().any(|p| pattern_is_catchall(p, variant_names)),
        _ => false,
    }
}

/// `#[non_exhaustive]` slice 7 — build the machine-applicable fix-it
/// that inserts the rest-pattern (`..`) into a cross-package
/// struct pattern. With existing fields, the insertion is anchored
/// just after the last field (so the result reads `Foo { a, b, .. }`
/// even when the pattern has trailing whitespace before the `}`).
/// With no fields, the insertion lands just before the closing `}`
/// of the pattern. The `pattern_span` is the whole struct-pattern
/// span ending at `}`; field spans cover the field name and any
/// nested sub-pattern. Insertion-only — never replaces existing
/// source text.
fn non_exhaustive_pattern_fix_it(
    pattern_span: &Span,
    fields: &[crate::ast::FieldPattern],
) -> crate::typechecker::FixIt {
    let (offset, line, column, replacement) = if let Some(last) = fields.last() {
        // Insert `, ..` right after the last field's span. Works
        // cleanly whether the source has `Foo { a, b }`,
        // `Foo { a, b, }`, or `Foo {a,b}` — the comma we emit
        // chains onto the existing last-field token unambiguously.
        let after_last = last.span.offset + last.span.length;
        (after_last, last.span.line, last.span.column, ", ..")
    } else {
        // Empty field list — `Foo { }`. Insert `..` just before
        // `}`. `pattern_span.length` is always >= 1 because the
        // pattern source ends with `}`, so `length - 1` is a
        // valid byte offset pointing at `}`.
        let brace = pattern_span.offset + pattern_span.length.saturating_sub(1);
        (brace, pattern_span.line, pattern_span.column, "..")
    };
    crate::typechecker::FixIt {
        span: Span {
            line,
            column,
            offset,
            length: 0,
        },
        replacement: replacement.to_string(),
    }
}

/// `#[non_exhaustive]` slice 7 — build the machine-applicable fix-it
/// that appends a wildcard arm to a cross-package non-exhaustive
/// match. With existing arms, the insertion anchors right after the
/// last arm (using its own span, before any trailing comma) and the
/// replacement starts with `, ` so it chains cleanly regardless of
/// whether the source already carried a trailing comma. With no
/// arms, the insertion anchors just before the closing `}` of the
/// match block and emits the bare arm with no leading comma.
/// Insertion-only — never replaces existing source text.
/// Build a machine-applicable fix-it that inserts `arm` (a full
/// `pattern => body`, no trailing comma) into a non-exhaustive `match`.
/// When the match has arms, the arm is inserted after the last one prefixed
/// with `, ` (the last arm's span ends before any trailing comma, so this
/// yields a well-formed comma-separated list); an empty match inserts just
/// before the closing brace. The span is a zero-length insertion point.
fn non_exhaustive_match_fix_it(
    match_span: &Span,
    arms: &[MatchArm],
    arm: &str,
) -> crate::typechecker::FixIt {
    let (offset, line, column, replacement) = if let Some(last) = arms.last() {
        let after_last = last.span.offset + last.span.length;
        (
            after_last,
            last.span.line,
            last.span.column,
            format!(", {arm}"),
        )
    } else {
        let brace = match_span.offset + match_span.length.saturating_sub(1);
        (brace, match_span.line, match_span.column, arm.to_string())
    };
    crate::typechecker::FixIt {
        span: Span {
            line,
            column,
            offset,
            length: 0,
        },
        replacement,
    }
}
