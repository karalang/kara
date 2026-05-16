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
use crate::resolver::{SpanKey, SymbolKind};
use crate::token::Span;
use std::collections::HashMap;

use super::env::{FunctionSig, ImplInfo};
use super::inference::{
    const_value_from_literal, expr_as_type_expr, instantiate_signature_with_fresh_vars,
    is_literal_const_arg_expr, resolve_const_arg, resolve_type_var_top, resolve_type_vars,
    substitute_const_idents_in_expr, substitute_type_params, unify_types, InstantiatedSignature,
};
use super::types::{
    clone_self_type_for, contains_type_param, impl_args_match, impl_table_key, is_integer,
    is_numeric, is_prelude_type_or_module_name, iterator_item_type_for, lub_block_type,
    method_callee_type_name, receiver_for_method_lookup, type_display,
    type_to_concrete_or_param_name, types_compatible, ConstArg, IntSize, ScrutineeMode, SubstValue,
    Type, UIntSize, VariantTypeInfo,
};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    pub(super) fn check_expr(&mut self, expr: &Expr, expected: &Type) -> Type {
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
                            ("Map", Type::Named { name, .. }) => name == "Map",
                            _ => false,
                        };
                        if matches_expected {
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
                        expr.span.clone(),
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
                            count.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
            } else {
                self.type_error(
                    "Array[T, N] repeat-literal requires a non-negative integer literal count"
                        .to_string(),
                    count.span.clone(),
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
                                p.pattern.span.clone(),
                                TypeErrorKind::RefutablePattern,
                            );
                        }
                        self.bind_pattern_types(&p.pattern, &ty);
                        ty
                    })
                    .collect();
                let body_ty = self.check_expr(body, expected_ret);
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
                self.check_assignable(expected, &actual, expr.span.clone());
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

        let actual = self.infer_expr(expr);
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
        self.check_assignable(expected, &actual, expr.span.clone());
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
                        Some(self.dispatch_trait_assoc_fn(target, &trait_method, args, span))
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
                            span.clone(),
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
                            span.clone(),
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
        let mut frame: HashMap<String, String> = HashMap::new();
        for (name, ty) in solutions {
            if let Some(resolved) = type_to_concrete_or_param_name(ty) {
                frame.insert(name.clone(), resolved);
            }
        }
        if !frame.is_empty() {
            self.call_type_subs.insert(SpanKey::from_span(span), frame);
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
    fn check_call_args_with_substitution(
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
    fn check_call_args_with_substitution_full(
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
        let has_where_const_predicate = where_clause
            .map(|wc| {
                wc.constraints
                    .iter()
                    .any(|c| matches!(c, WhereConstraint::ConstPredicate { .. }))
            })
            .unwrap_or(false);
        let has_generic = params.iter().any(contains_type_param)
            || contains_type_param(return_type)
            || has_where_const_predicate;
        if !has_generic {
            for (arg, param_ty) in args.iter().zip(params.iter()) {
                let arg_ty = self.check_expr(&arg.value, param_ty);
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
                    if let GenericArg::Type(te) = explicit_arg {
                        let ty = self.lower_type_expr(te, &[]);
                        self.env.substitutions.insert(type_id, ty);
                    }
                }
            }
        }

        let mut arg_tys: Vec<Option<Type>> = Vec::with_capacity(args.len());
        for arg in args {
            if matches!(arg.value.kind, ExprKind::Closure { .. }) {
                arg_tys.push(None);
            } else {
                arg_tys.push(Some(self.infer_expr(&arg.value)));
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
                    self.check_assignable(&resolved, arg_ty, arg.value.span.clone());
                    if apply_call_site_marker {
                        self.check_call_site_marker(arg, &resolved, arg_ty);
                    }
                }
                None => {
                    let arg_ty = self.check_expr(&arg.value, &resolved);
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
            self.discharge_type_bounds(wc, &solutions, discharge_span);
        }

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
        discharge_span: &Span,
    ) {
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
                if self.type_satisfies_bound(concrete_ty, trait_name) {
                    continue;
                }
                self.type_error(
                    format!(
                        "trait bound `{}: {}` is not satisfied; `{}` does not implement `{}`",
                        type_name,
                        trait_name,
                        type_display(concrete_ty),
                        trait_name
                    ),
                    discharge_span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
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
    fn type_satisfies_bound(&self, ty: &Type, trait_name: &str) -> bool {
        // Built-in coverage via the type_supports_* helpers — these
        // recognize primitives implicitly + named types via
        // `#[derive(Trait)]` registration.
        match trait_name {
            "Hash" => return self.type_supports_hash(ty),
            "Eq" => return self.type_supports_eq(ty),
            "PartialEq" => return self.type_supports_partial_eq(ty),
            "Ord" => return self.type_supports_ord(ty),
            "PartialOrd" => return self.type_supports_partial_ord(ty),
            "Display" => return self.type_supports_display(ty),
            _ => {}
        }
        // Other traits: explicit impl in the table, with supertrait closure.
        let Some((ty_name, ty_args)) = impl_table_key(ty) else {
            return false;
        };
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
                        discharge_span.clone(),
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
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return sig.return_type.clone();
        }
        for (arg, param_ty) in args.iter().zip(sig.params.iter()) {
            let arg_ty = self.infer_expr(&arg.value);
            self.check_assignable(param_ty, &arg_ty, arg.value.span.clone());
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
            expr.span.clone(),
            TypeErrorKind::TypeMismatch,
        );
        self.record_expr_type(&expr.span, &Type::Error);
        Some(Type::Error)
    }

    /// Recognize `x.try_into()` at an expected `Result[Target, _]` position.
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
            expr.span.clone(),
            TypeErrorKind::TypeMismatch,
        );
        self.record_expr_type(&expr.span, &Type::Error);
        Some(Type::Error)
    }

    pub(super) fn infer_expr(&mut self, expr: &Expr) -> Type {
        let ty = self.infer_expr_inner(expr);
        self.record_expr_type(&expr.span, &ty);
        ty
    }

    fn infer_expr_inner(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            // Literals
            ExprKind::Integer(_, sfx) => self.type_from_int_suffix(*sfx, expr.span.clone()),
            ExprKind::Float(_, sfx) => Self::type_from_float_suffix(*sfx),
            ExprKind::CharLit(_) => Type::Char,
            ExprKind::StringLit(_) | ExprKind::MultiStringLit(_) => Type::Str,
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(inner_expr) = part {
                        let ty = self.infer_expr(inner_expr);
                        if ty != Type::Error && !self.type_supports_display(&ty) {
                            self.type_error(
                                format!(
                                    "type '{}' does not implement Display; \
                                     cannot interpolate in f-string",
                                    type_display(&ty)
                                ),
                                inner_expr.span.clone(),
                                TypeErrorKind::TraitBoundNotSatisfied,
                            );
                        }
                    }
                }
                Type::Str
            }
            ExprKind::Bool(_) => Type::Bool,

            // Identifiers
            ExprKind::Identifier(name) => self.resolve_identifier_type(name, &expr.span),
            ExprKind::Path { segments, .. } => self.resolve_path_type(segments, &expr.span),

            ExprKind::SelfValue => self.current_self_type.clone().unwrap_or(Type::Error),
            ExprKind::SelfType => self.current_self_type.clone().unwrap_or(Type::Error),

            // Operators
            ExprKind::Binary { op, left, right } => self.infer_binary(op, left, right, &expr.span),
            ExprKind::Pipe { left, right } => self.infer_pipe(left, right, &expr.span),
            ExprKind::Unary { op, operand } => self.infer_unary(op, operand, &expr.span),

            // Postfix
            ExprKind::Question(inner) => {
                if self.in_defer {
                    self.type_error(
                        "'?' operator is not allowed inside defer/errdefer blocks".to_string(),
                        expr.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                self.infer_question(inner, &expr.span)
            }

            ExprKind::OptionalChain { object, .. } => {
                let _obj_ty = self.infer_expr(object);
                Type::Error // Needs advanced option handling, stubbed for now
            }

            // Infix
            ExprKind::NilCoalesce { left, right } => {
                let l_ty = self.infer_expr(left);
                let r_ty = self.infer_expr(right);
                if l_ty != Type::Error && r_ty != Type::Error {
                    if let Type::Named { name, args } = &l_ty {
                        if name == "Option" && args.len() == 1 {
                            self.check_assignable(&args[0], &r_ty, right.span.clone());
                            return args[0].clone();
                        }
                    }
                }
                Type::Error
            }

            ExprKind::Call { callee, args } => self.infer_call(callee, args, &expr.span),

            ExprKind::MethodCall {
                object,
                method,
                args,
                turbofish: _,
            } => self.infer_method_call(object, method, args, &expr.span),

            ExprKind::FieldAccess { object, field } => {
                self.infer_field_access(object, field, &expr.span)
            }

            ExprKind::TupleIndex { object, index } => {
                let obj_ty = self.infer_expr(object);
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
                                expr.span.clone(),
                                TypeErrorKind::InvalidTupleIndex,
                            );
                            Type::Error
                        }
                    }
                    Type::Error => Type::Error,
                    _ => {
                        self.type_error(
                            format!("tuple index on non-tuple type '{}'", type_display(&obj_ty)),
                            expr.span.clone(),
                            TypeErrorKind::InvalidTupleIndex,
                        );
                        Type::Error
                    }
                }
            }

            ExprKind::Index { object, index } => {
                let obj_ty = self.infer_expr(object);
                let idx_ty = self.infer_expr(index);
                let is_range_idx = matches!(&idx_ty, Type::Named { name, .. }
                    if matches!(name.as_str(), "Range" | "RangeInclusive" | "RangeFrom"
                        | "RangeTo" | "RangeToInclusive" | "RangeFull"));
                if !is_integer(&idx_ty) && !is_range_idx && idx_ty != Type::Error {
                    self.type_error(
                        format!(
                            "index must be an integer or range, found '{}'",
                            type_display(&idx_ty)
                        ),
                        index.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                if is_range_idx {
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
                                expr.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                            Type::Error
                        }
                    };
                }
                match &obj_ty {
                    Type::Array { element, .. } => *element.clone(),
                    Type::Slice { element, .. } => *element.clone(),
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        args[0].clone()
                    }
                    Type::Error => Type::Error,
                    _ => Type::Error,
                }
            }

            // Compound
            ExprKind::Block(block) => self.infer_block(block),

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
                        condition.span.clone(),
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
                    if !types_compatible(&then_ty, &else_ty)
                        && then_ty != Type::Error
                        && else_ty != Type::Error
                    {
                        self.type_error(
                            format!(
                                "if/else branches have incompatible types: '{}' and '{}'",
                                type_display(&then_ty),
                                type_display(&else_ty)
                            ),
                            expr.span.clone(),
                            TypeErrorKind::BranchTypeMismatch,
                        );
                    }
                    then_ty
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
                    if !types_compatible(&then_ty, &else_ty)
                        && then_ty != Type::Error
                        && else_ty != Type::Error
                    {
                        self.type_error(
                            format!(
                                "if let/else branches have incompatible types: '{}' and '{}'",
                                type_display(&then_ty),
                                type_display(&else_ty)
                            ),
                            expr.span.clone(),
                            TypeErrorKind::BranchTypeMismatch,
                        );
                    }
                    then_ty
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
                        condition.span.clone(),
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
                self.local_scope.push();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        let ty =
                            p.ty.as_ref()
                                .map(|t| self.lower_type_expr(t, &[]))
                                .unwrap_or_else(|| self.env.fresh_type_var());
                        if !self.is_irrefutable_pattern(&p.pattern, &ty) {
                            self.type_error(
                                "refutable pattern in closure parameter; use `if let` or `match` for patterns that may not match".to_string(),
                                p.pattern.span.clone(),
                                TypeErrorKind::RefutablePattern,
                            );
                        }
                        self.bind_pattern_types(&p.pattern, &ty);
                        ty
                    })
                    .collect();
                let body_ty = self.infer_expr(body);
                self.local_scope.pop();
                self.break_value_types = saved_break_values;
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
                            expr.span.clone(),
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
                let types: Vec<Type> = exprs.iter().map(|e| self.infer_expr(e)).collect();
                Type::Tuple(types)
            }

            ExprKind::StructLiteral {
                path,
                fields,
                spread,
            } => {
                if let Some(ref spread_expr) = spread {
                    self.infer_expr(spread_expr);
                }
                self.infer_struct_literal(path, fields, &expr.span)
            }

            ExprKind::Cast { expr: inner, ty } => {
                let from_ty = self.infer_expr(inner);
                let to_ty = self.lower_type_expr(ty, &[]);
                self.check_cast_pair(&from_ty, &to_ty, &inner.span);
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
                    if !types_compatible(s, e) && *s != Type::Error && *e != Type::Error {
                        self.type_error(
                            format!(
                                "range bounds must have same type: '{}' and '{}'",
                                type_display(s),
                                type_display(e)
                            ),
                            expr.span.clone(),
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

            ExprKind::Unsafe(block) => self.infer_block(block),

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
                    expr.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }

            ExprKind::WhileLet { value, body, .. } => {
                self.infer_expr(value);
                self.infer_block(body);
                Type::Unit
            }

            ExprKind::Seq(block) => self.infer_block(block),
            ExprKind::Par(block) => self.infer_block(block),

            ExprKind::Lock { body, .. } => self.infer_block(body),

            ExprKind::Providers { bindings, body } => {
                // Provider values are plain expressions; infer their types
                // for side effects (diagnostics, subexpression typing). The
                // block's type is the body's type. Full provider-trait
                // conformance — verifying each provider implements the
                // resource's declared `ProviderTrait` — is deferred along
                // with the `Send + Sync` auto-trait enforcement tracked at
                // `docs/deferred.md § Send + Sync Enforcement on
                // with_provider Concrete Provider Type`.
                for b in bindings {
                    self.infer_expr(&b.value);
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
                        self.check_assignable(&first_ty, &elem_ty, elem.span.clone());
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
                            self.check_assignable(&first_ty, &ty, item.span.clone());
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
                            self.check_assignable(&first_ty, &ty, item.span.clone());
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
                            self.check_assignable(&first_ty, &ty, item.span.clone());
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
                        count.span.clone(),
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
                                    count.span.clone(),
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
                            expr.span.clone(),
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
                    self.check_assignable(&key_ty, &kt, k.span.clone());
                    self.check_assignable(&val_ty, &vt, v.span.clone());
                }
                Type::Named {
                    name: "HashMap".to_string(),
                    args: vec![key_ty, val_ty],
                }
            }

            ExprKind::PipePlaceholder => {
                self.type_error(
                    "'_' placeholder is only valid inside a pipe expression argument list"
                        .to_string(),
                    expr.span.clone(),
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

    /// Type-check `offset_of[T](field.path)`. Per `design.md § Field
    /// Offsets`, the target type must be a struct (concrete or
    /// generic-with-fully-resolved args); opaque foreign types and
    /// generic type parameters are rejected at the first segment.
    /// Each path segment must name a field of the type at the previous
    /// segment's resolved type. Returns `usize` (also `Type::Error` on
    /// failure for downstream tolerance).
    fn infer_offset_of(&mut self, ty: &TypeExpr, field_path: &[String], span: &Span) -> Type {
        let usize_ty = Type::UInt(UIntSize::Usize);
        // Lower the target with `parent_is_ref = true` so the slice-1b
        // walker doesn't fire E_OPAQUE_TYPE_REQUIRES_INDIRECTION; this
        // intrinsic emits E_OFFSET_OF_OPAQUE_TYPE instead.
        let resolved = self.lower_type_expr_inner(ty, &[], true);
        let (mut current_struct_name, _initial_args) = match &resolved {
            Type::Named { name, args } => (name.clone(), args.clone()),
            // Per design.md, generic type-parameter targets are rejected:
            // the typechecker can't see a layout without a concrete
            // instantiation. `Type::TypeParam` and other non-Named
            // shapes route here.
            Type::TypeParam(name) => {
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_GENERIC_PARAM]: offset_of requires a \
                         concrete type; the type parameter '{name}' is not \
                         resolvable to a layout at this call site"
                    ),
                    ty.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            _ => {
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_NON_STRUCT_TARGET]: offset_of requires a \
                         struct target; got '{}'",
                        type_display(&resolved)
                    ),
                    ty.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };
        if self.env.opaque_foreign_types.contains(&current_struct_name) {
            self.type_error(
                format!(
                    "error[E_OFFSET_OF_OPAQUE_TYPE]: offset_of cannot be applied to \
                     opaque foreign type '{current_struct_name}'; the type's layout \
                     is unknown to Kāra"
                ),
                ty.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        if field_path.is_empty() {
            self.type_error(
                "error[E_OFFSET_OF_INVALID_PATH]: offset_of requires at least \
                 one field-name segment"
                    .to_string(),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            return Type::Error;
        }
        // Walk each segment, validating membership in the current struct's
        // declared field set and chasing the field's type for the next
        // segment. At each segment, the current struct is looked up by
        // name in `env.structs`; if absent (e.g., the surface type is an
        // enum or a primitive), `E_OFFSET_OF_NON_STRUCT_TARGET` fires.
        for (segment_idx, segment_name) in field_path.iter().enumerate() {
            let Some(info) = self.env.structs.get(&current_struct_name).cloned() else {
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_NON_STRUCT_TARGET]: offset_of cannot \
                         walk into '{current_struct_name}'; only struct types \
                         have field offsets"
                    ),
                    ty.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            };
            let mut found = None;
            for (fname, ftype, is_pub) in &info.fields {
                if fname == segment_name {
                    found = Some((ftype.clone(), *is_pub));
                    break;
                }
            }
            let Some((field_ty, is_pub)) = found else {
                let available: Vec<&str> = info.fields.iter().map(|(n, _, _)| n.as_str()).collect();
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_UNKNOWN_FIELD]: type '{current_struct_name}' \
                         has no field '{segment_name}'; available fields are: {}",
                        available.join(", ")
                    ),
                    span.clone(),
                    TypeErrorKind::UndefinedField,
                );
                return Type::Error;
            };
            if !is_pub {
                self.check_cross_module_field_access(&current_struct_name, segment_name, span);
            }
            // If this is the last segment, we're done — return usize.
            if segment_idx + 1 == field_path.len() {
                return usize_ty;
            }
            // Otherwise, the field's type must itself be a struct so the
            // next segment can walk into it.
            current_struct_name = match field_ty {
                Type::Named { name, .. } => name,
                _ => {
                    self.type_error(
                        format!(
                            "error[E_OFFSET_OF_NON_STRUCT_TARGET]: field \
                             '{segment_name}' is not a struct type; cannot walk \
                             further into the offset_of path"
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
            };
        }
        usize_ty
    }

    // ── Identifier Resolution ───────────────────────────────────

    fn resolve_identifier_type(&mut self, name: &str, span: &Span) -> Type {
        // Check local scope first
        if let Some(ty) = self.local_scope.lookup(name) {
            return ty.clone();
        }
        // Check functions
        if let Some(sig) = self.env.functions.get(name) {
            return Type::Function {
                params: sig.params.clone(),
                return_type: Box::new(sig.return_type.clone()),
            };
        }
        // Check constants
        if let Some(ty) = self.env.constants.get(name) {
            return ty.clone();
        }
        // Check enum variants (unit variants used as values; tuple variants
        // as constructor functions). Generic enums thread their declared
        // type parameters through the return type's `args` so call-site
        // inference can solve them (see `infer_call`).
        //
        // **Variant-name shadow rule (Slice F).** Skip variants whose
        // bare name collides with a primitive type name (`String`,
        // `Array`, `Map`, `Set`, etc.) — those identifiers are
        // overwhelmingly used as type/module aliases at the call-site
        // (`String.from(...)`, `Map.new()`, `Vec.new()`), not as
        // variant constructors. Without this skip, declaring an enum
        // like `Json.String(String)` retroactively breaks every
        // pre-existing `String.from("...")` call by routing it through
        // the variant-as-function dispatch instead of the impl
        // resolution. Variants are still reachable through the
        // qualified path form (`Json.String(...)`) — `resolve_path_type`
        // above runs before this fallback and finds them by enum name.
        for (enum_name, enum_info) in &self.env.enums {
            for (variant_name, variant_type) in &enum_info.variants {
                if variant_name == name {
                    if is_prelude_type_or_module_name(name) {
                        continue;
                    }
                    let return_args: Vec<Type> = enum_info
                        .generic_params
                        .iter()
                        .map(|p| Type::TypeParam(p.clone()))
                        .collect();
                    let return_ty = Type::Named {
                        name: enum_name.clone(),
                        args: return_args,
                    };
                    match variant_type {
                        VariantTypeInfo::Unit => return return_ty,
                        VariantTypeInfo::Tuple(fields) => {
                            return Type::Function {
                                params: fields.clone(),
                                return_type: Box::new(return_ty),
                            };
                        }
                        _ => {}
                    }
                }
            }
        }
        // Fallback — likely a name the resolver already handled
        // Return Error silently (resolver already reported it)
        let _ = span;
        Type::Error
    }

    fn resolve_path_type(&mut self, segments: &[String], span: &Span) -> Type {
        if segments.len() == 2 {
            let type_name = &segments[0];
            let member = &segments[1];

            // Check for enum variant. Generic enums thread their declared
            // type parameters through the return type's `args` so call-site
            // inference can solve them (see `infer_call`).
            if let Some(enum_info) = self.env.enums.get(type_name).cloned() {
                for (variant_name, variant_type) in &enum_info.variants {
                    if variant_name == member {
                        let return_args: Vec<Type> = enum_info
                            .generic_params
                            .iter()
                            .map(|p| Type::TypeParam(p.clone()))
                            .collect();
                        let return_ty = Type::Named {
                            name: type_name.clone(),
                            args: return_args,
                        };
                        match variant_type {
                            VariantTypeInfo::Unit => return return_ty,
                            VariantTypeInfo::Tuple(fields) => {
                                return Type::Function {
                                    params: fields.clone(),
                                    return_type: Box::new(return_ty),
                                };
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Check for associated function (from impl). No call-site args
            // context — type_name comes from a Path expression without
            // generic args. Theme-4 conservative: only generic-on-name
            // impls participate; specialized impls (`impl Foo for
            // Bar[i32]`) need an args-aware path-expr lookup that this
            // site doesn't carry.
            for imp in &self.env.impls.clone() {
                if imp.target_type == *type_name && imp.target_args.is_empty() {
                    if let Some(sig) = imp.methods.get(member) {
                        return Type::Function {
                            params: sig.params.clone(),
                            return_type: Box::new(sig.return_type.clone()),
                        };
                    }
                }
            }

            // Module-path free functions registered as "module.fn" in the
            // function table — `process.exit`, `env.args`, `env.var`. The
            // ambient effect-resource methods (`Stdin.read_line`,
            // `FileSystem.write`, …) used to land here too, but the slice-1
            // through slice-3 migration moved every `Type.method` entry into
            // `env.impls` via baked source, so this fallback now only serves
            // module-path free functions.
            let dotted = format!("{}.{}", type_name, member);
            if let Some(sig) = self.env.functions.get(&dotted) {
                return Type::Function {
                    params: sig.params.clone(),
                    return_type: Box::new(sig.return_type.clone()),
                };
            }
        }
        // First segment as identifier
        if let Some(first) = segments.first() {
            return self.resolve_identifier_type(first, span);
        }
        Type::Error
    }

    // ── Binary / Unary Operators ────────────────────────────────

    fn infer_binary(&mut self, op: &BinOp, left: &Expr, right: &Expr, span: &Span) -> Type {
        let left_ty = self.infer_expr(left);
        let right_ty = self.infer_expr(right);

        if left_ty == Type::Error || right_ty == Type::Error {
            return Type::Error;
        }

        // Q4 literal promotion: for arithmetic, comparison, and equality ops,
        // when one operand is a suffix-free numeric literal and the other is a
        // concrete numeric type T, re-record the literal's span with type T so
        // the lowering pass sees a homogeneous pair. `effective_ty` tracks the
        // canonical type for the whole expression after promotion.
        let is_promotable_op = matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::Eq
                | BinOp::NotEq
        );
        // After promotion these hold the effective operand types seen by the
        // match arms below. Initialised to the inferred types; overwritten when
        // promotion fires.
        let (eff_left_ty, eff_right_ty) = if is_promotable_op {
            let left_is_unsuffixed = matches!(
                &left.kind,
                ExprKind::Integer(_, None) | ExprKind::Float(_, None)
            );
            let right_is_unsuffixed = matches!(
                &right.kind,
                ExprKind::Integer(_, None) | ExprKind::Float(_, None)
            );
            if right_is_unsuffixed && !left_is_unsuffixed && is_numeric(&left_ty) {
                // Float literal cannot be promoted to an integer type.
                let can_promote = !(matches!(&right.kind, ExprKind::Float(_, None))
                    && matches!(left_ty, Type::Int(_) | Type::UInt(_)));
                if can_promote {
                    self.record_expr_type(&right.span, &left_ty);
                    (left_ty.clone(), left_ty.clone())
                } else {
                    (left_ty.clone(), right_ty.clone())
                }
            } else if left_is_unsuffixed && !right_is_unsuffixed && is_numeric(&right_ty) {
                let can_promote = !(matches!(&left.kind, ExprKind::Float(_, None))
                    && matches!(right_ty, Type::Int(_) | Type::UInt(_)));
                if can_promote {
                    self.record_expr_type(&left.span, &right_ty);
                    (right_ty.clone(), right_ty.clone())
                } else {
                    (left_ty.clone(), right_ty.clone())
                }
            } else {
                (left_ty.clone(), right_ty.clone())
            }
        } else {
            (left_ty.clone(), right_ty.clone())
        };
        let left_ty = eff_left_ty;
        let right_ty = eff_right_ty;

        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                if is_numeric(&left_ty) {
                    if !types_compatible(&left_ty, &right_ty) {
                        self.type_error(
                            format!(
                                "expected '{}', found '{}'",
                                type_display(&left_ty),
                                type_display(&right_ty)
                            ),
                            right.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    left_ty
                } else if self.distinct_type_has_arithmetic(&left_ty) {
                    // Arithmetic on a distinct type: both operands must be the same type.
                    if left_ty != right_ty {
                        self.type_error(
                            format!(
                                "arithmetic on distinct type '{}' requires both operands to have \
                                 the same type, found '{}'",
                                type_display(&left_ty),
                                type_display(&right_ty)
                            ),
                            right.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    left_ty
                } else {
                    self.type_error(
                        format!(
                            "arithmetic operator requires numeric type, found '{}'",
                            type_display(&left_ty)
                        ),
                        left.span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                    Type::Error
                }
            }
            BinOp::Eq | BinOp::NotEq => {
                if !types_compatible(&left_ty, &right_ty) {
                    self.type_error(
                        format!(
                            "cannot compare '{}' and '{}'",
                            type_display(&left_ty),
                            type_display(&right_ty)
                        ),
                        span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                } else if !self.type_supports_partial_eq(&left_ty) {
                    self.type_error(
                        format!(
                            "type '{}' does not implement Eq; add #[derive(Eq)] to use == or !=",
                            type_display(&left_ty)
                        ),
                        span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                Type::Bool
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                if !types_compatible(&left_ty, &right_ty) {
                    self.type_error(
                        format!(
                            "cannot compare '{}' and '{}'",
                            type_display(&left_ty),
                            type_display(&right_ty)
                        ),
                        span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                Type::Bool
            }
            BinOp::And | BinOp::Or => {
                if left_ty != Type::Bool {
                    self.type_error(
                        format!(
                            "logical operator requires 'bool', found '{}'",
                            type_display(&left_ty)
                        ),
                        left.span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                if right_ty != Type::Bool {
                    self.type_error(
                        format!(
                            "logical operator requires 'bool', found '{}'",
                            type_display(&right_ty)
                        ),
                        right.span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                Type::Bool
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if !is_integer(&left_ty) {
                    self.type_error(
                        format!(
                            "bitwise operator requires integer type, found '{}'",
                            type_display(&left_ty)
                        ),
                        left.span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                    return Type::Error;
                }
                if !types_compatible(&left_ty, &right_ty) {
                    self.type_error(
                        format!(
                            "expected '{}', found '{}'",
                            type_display(&left_ty),
                            type_display(&right_ty)
                        ),
                        right.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                left_ty
            }
            BinOp::Range | BinOp::RangeInclusive => {
                if !types_compatible(&left_ty, &right_ty) {
                    self.type_error(
                        "range bounds must have same type".to_string(),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                Type::Named {
                    name: "Range".to_string(),
                    args: vec![left_ty],
                }
            }
        }
    }

    fn infer_unary(&mut self, op: &UnaryOp, operand: &Expr, span: &Span) -> Type {
        let ty = self.infer_expr(operand);
        if ty == Type::Error {
            return Type::Error;
        }

        match op {
            UnaryOp::Neg => {
                if !is_numeric(&ty) && !self.distinct_type_has_arithmetic(&ty) {
                    self.type_error(
                        format!(
                            "unary '-' requires numeric type, found '{}'",
                            type_display(&ty)
                        ),
                        span.clone(),
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                } else {
                    ty
                }
            }
            UnaryOp::Not => {
                if ty != Type::Bool {
                    self.type_error(
                        format!("unary '!' requires 'bool', found '{}'", type_display(&ty)),
                        span.clone(),
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                } else {
                    Type::Bool
                }
            }
            UnaryOp::BitNot => {
                if !is_integer(&ty) {
                    self.type_error(
                        format!(
                            "unary '~' requires integer type, found '{}'",
                            type_display(&ty)
                        ),
                        span.clone(),
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                } else {
                    ty
                }
            }
            UnaryOp::Deref => match ty {
                Type::Ref(inner) | Type::MutRef(inner) => *inner,
                // Raw-pointer dereference (`*const T` / `*mut T`) typechecks
                // to the pointee type. The operation itself is *unsafe* — the
                // `unsafe_op_in_unsafe_fn` lint (`src/unsafe_lint.rs`) rejects
                // it outside an `unsafe { }` block. Soundness lives at the
                // lint layer, not the type layer, so callers can still reason
                // about the deref's result type.
                Type::Pointer { inner, .. } => *inner,
                _ => {
                    self.type_error(
                        format!(
                            "unary '*' requires 'ref T', 'mut ref T', or a raw pointer \
                             ('*const T' / '*mut T'), found '{}'",
                            type_display(&ty)
                        ),
                        span.clone(),
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                }
            },
        }
    }

    // ── Function Calls ──────────────────────────────────────────

    /// Const generics slice 1c shared path: dispatch a generic free
    /// function call that carries an explicit generic-args list.
    /// Looks up the function's `FunctionSig`, threads
    /// `(explicit_args, formal_generic_params)` into the call-args
    /// substitution flow so the inference solver pre-binds each
    /// ConstVar / TypeVar to its user-supplied value before
    /// arg-position unification.
    fn infer_explicit_generic_args_call(
        &mut self,
        name: &str,
        explicit_args: &[GenericArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let Some(sig) = self.env.functions.get(name).cloned() else {
            // No matching function — fall through to the bare-identifier
            // dispatch via a synthetic Identifier callee so existing
            // error reporting fires.
            let synthetic = Expr {
                kind: ExprKind::Identifier(name.to_string()),
                span: span.clone(),
            };
            return self.infer_call(&synthetic, args, span);
        };
        if args.len() != sig.params.len() {
            self.type_error(
                format!(
                    "expected {} argument(s), found {}",
                    sig.params.len(),
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return sig.return_type;
        }
        let formal_generic_params = sig.generic_params.clone();
        let where_clause = sig.where_clause.clone();
        self.check_call_args_with_substitution_full(
            args,
            &sig.params,
            &sig.return_type,
            span,
            true,
            Some(explicit_args),
            Some(&formal_generic_params),
            where_clause.as_ref(),
            span,
        )
    }

    /// Type-check a call to a layout-introspection intrinsic
    /// (`size_of[T]()` / `align_of[T]()`). Both share the same shape:
    /// exactly one type argument, no value arguments, returns `usize`.
    /// Per `design.md § Field Offsets`, opaque foreign types are
    /// rejected with `error[E_OPAQUE_TYPE_NO_KNOWN_SIZE]` since their
    /// layout is unknown to Kāra.
    ///
    /// The type argument is lowered via `lower_type_expr_inner(_, _, true)`
    /// so the slice-1b walker's `E_OPAQUE_TYPE_REQUIRES_INDIRECTION`
    /// emission is suppressed — for layout queries, "wrap in `ref T`"
    /// is the wrong remediation hint (`size_of[ref Foo]()` measures
    /// the reference, not Foo). The `parent_is_ref = true` flag is a
    /// minor semantic misnomer here ("opaque is allowed at this leaf
    /// because the caller will check it explicitly"), but reusing the
    /// existing flag keeps `lower_type_expr_inner` from sprouting a
    /// second control parameter.
    fn infer_layout_query_intrinsic(
        &mut self,
        name: &str,
        explicit_args: &[GenericArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        if !args.is_empty() {
            self.type_error(
                format!(
                    "error[E_LAYOUT_QUERY_TAKES_NO_ARGS]: `{name}` takes a type \
                     argument only — call shape is `{name}[T]()`, no value arguments"
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for a in args {
                self.infer_expr(&a.value);
            }
        }
        let usize_ty = Type::UInt(UIntSize::Usize);
        let type_arg_expr = match explicit_args {
            [GenericArg::Type(te)] => te,
            _ => {
                self.type_error(
                    format!(
                        "error[E_LAYOUT_QUERY_TYPE_ARG_REQUIRED]: `{name}` requires \
                         exactly one type argument — call shape is `{name}[T]()`"
                    ),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return usize_ty;
            }
        };
        let resolved = self.lower_type_expr_inner(type_arg_expr, &[], true);
        if let Type::Named {
            name: ref ty_name, ..
        } = resolved
        {
            if self.env.opaque_foreign_types.contains(ty_name) {
                self.type_error(
                    format!(
                        "error[E_OPAQUE_TYPE_NO_KNOWN_SIZE]: `{name}` cannot be \
                         applied to opaque foreign type '{ty_name}'; the type's \
                         size and alignment are unknown to Kāra"
                    ),
                    type_arg_expr.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        usize_ty
    }

    fn infer_call(&mut self, callee: &Expr, args: &[CallArg], span: &Span) -> Type {
        // Const generics slice 1b + 1c: explicit-generic-args call
        // shapes. Two forms reach here:
        //
        //   1. `Path { segments: [name], generic_args: Some(args) }` —
        //      multi-arg shape `name[T, 4](args)` recognized by the
        //      parser's `lookahead_generic_args_call` (requires a
        //      top-level `,` inside the brackets).
        //   2. `Index { object: Identifier(name), index: literal }` —
        //      single-arg shape `name[8](args)` that the parser
        //      can't disambiguate from `callbacks[0]()`. The Vec-of-
        //      functions case at interpreter:1985 must keep working,
        //      so we only treat as a generic-args call when `name`
        //      resolves to a generic free function in `env.functions`.
        //
        // Both shapes route through `infer_explicit_generic_args_call`,
        // which threads the formal-param names + explicit args into
        // `check_call_args_with_substitution_full` so the inference
        // solver pre-binds each ConstVar / TypeVar to its
        // user-supplied value before arg-position unification.
        // Layout-introspection intrinsics: `size_of[T]()` / `align_of[T]()`.
        // Intercepted before the regular generic-call dispatch so the
        // slice-1b walker's `E_OPAQUE_TYPE_REQUIRES_INDIRECTION` emission
        // on the type argument is suppressed (the "wrap in `ref T`" hint
        // would be misleading for a layout query — `size_of[ref Foo]()`
        // measures the reference, not Foo). The intrinsic emits the
        // focused `E_OPAQUE_TYPE_NO_KNOWN_SIZE` instead. See
        // `runtime/stdlib/intrinsics.kara` for the placeholder
        // declarations and `compile_call` for the codegen counterpart.
        //
        // Two AST shapes reach here. Multi-arg generic calls
        // (`size_of[T, _]()`, never used today but kept symmetric) parse
        // as `Path { generic_args: Some([T]) }` because
        // `lookahead_generic_args_call` requires a top-level comma.
        // Single-arg `size_of[T]()` cannot be disambiguated from
        // `arr[i]()` so it parses as `Call { callee: Index { Ident, T } }`
        // — `T` is a value-position `Expr` that actually denotes a type.
        if let ExprKind::Path {
            segments,
            generic_args: Some(ga),
        } = &callee.kind
        {
            if segments.len() == 1 {
                let name = &segments[0];
                if name == "size_of" || name == "align_of" {
                    return self.infer_layout_query_intrinsic(name, ga, args, span);
                }
            }
        }
        if let ExprKind::Index { object, index } = &callee.kind {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "size_of" || name == "align_of" {
                    if let Some(te) = expr_as_type_expr(index) {
                        let synth = vec![GenericArg::Type(te)];
                        return self.infer_layout_query_intrinsic(name, &synth, args, span);
                    }
                    self.type_error(
                        format!(
                            "error[E_LAYOUT_QUERY_TYPE_ARG_REQUIRED]: `{name}` requires \
                             a type argument — call shape is `{name}[T]()`"
                        ),
                        callee.span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    return Type::UInt(UIntSize::Usize);
                }
            }
        }

        if let Some((name, explicit_args)) = match &callee.kind {
            ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } if segments.len() == 1 => Some((segments[0].clone(), ga.clone())),
            ExprKind::Index { object, index } if is_literal_const_arg_expr(index) => {
                if let ExprKind::Identifier(name) = &object.kind {
                    if self
                        .env
                        .functions
                        .get(name)
                        .map(|s| !s.generic_params.is_empty())
                        .unwrap_or(false)
                    {
                        Some((name.clone(), vec![GenericArg::Const((**index).clone())]))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        } {
            return self.infer_explicit_generic_args_call(&name, &explicit_args, args, span);
        }

        // Type-parameter associated calls: `T.method(args)` parses as
        // `Call { callee: Path(["T", "method"]), args }`. Intercept this
        // shape before the generic call infrastructure tries to read `T`
        // as a value. Concrete types (`Wrapper.method()`) fall through —
        // `resolve_path_type` already finds their impl methods.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 {
                if let Some(ty) = self.try_dispatch_typeparam_assoc_fn(
                    &segments[0],
                    &segments[1],
                    &callee.span,
                    args,
                    span,
                ) {
                    return ty;
                }
            }
        }

        // Bare identifier callee that is unresolvable as a value but matches a
        // trait-declared associated function name: the resolver suppressed the
        // undefined-name error for these so the typechecker could dispatch via
        // expected type. We are here because synthesis mode reached `infer_call`
        // — meaning no expected-type slot was available — so emit the
        // "cannot infer type" diagnostic instead of silently returning Error.
        if let ExprKind::Identifier(name) = &callee.kind {
            if self.is_unresolvable_trait_assoc_fn(name) {
                self.type_error(
                    format!(
                        "cannot infer type for associated function call '{}': add a type annotation \
                         (e.g. `let x: T = {}(...)`) or call as `T.{}(...)`",
                        name, name, name,
                    ),
                    span.clone(),
                    TypeErrorKind::CannotInferAssocFn,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        }

        // Built-in diverging functions: todo() and unreachable()
        // Accept 0 or 1 String argument; return Never (they never return normally).
        if let ExprKind::Identifier(name) = &callee.kind {
            if name == "todo" || name == "unreachable" {
                match args.len() {
                    0 => {}
                    1 => {
                        let arg_ty = self.infer_expr(&args[0].value);
                        if arg_ty != Type::Str && arg_ty != Type::Error {
                            self.type_error(
                                format!(
                                    "{}() message must be a 'str', found '{}'",
                                    name,
                                    type_display(&arg_ty)
                                ),
                                args[0].value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                    _ => {
                        self.type_error(
                            format!("{}() takes 0 or 1 argument(s), found {}", name, args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                    }
                }
                return Type::Never;
            }
        }

        // Built-in collection constructors with no syntactic stdlib
        // declaration: `Vec.new()`, `VecDeque.new()`, `Set.new()`,
        // `SortedSet.new()`, `Map.new()`. These are dispatched at runtime
        // by the interpreter / codegen, but the typechecker still needs
        // a meaningful return type at the call site so a downstream
        // `q.push_back(x)` can solve the element typevar (otherwise the
        // binding's type collapses to `Type::Error`, the
        // `pattern_binding_types` / `pattern_binding_inner_types`
        // side-tables stay empty, and codegen's let-statement
        // `vec_elem_types` registration never fires). Returns
        // `Type::Named { name: <coll>, args: [TypeVar(fresh)] }` (or two
        // typevars for `Map`) so the standard inference machinery does
        // the rest. Per design.md § Collections (`Vec.new` / `Map.new`
        // are the canonical constructors).
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "new" && args.is_empty() {
                let collection = segments[0].as_str();
                let result_ty = match collection {
                    "Vec" | "VecDeque" | "Set" | "SortedSet" => Some(Type::Named {
                        name: collection.to_string(),
                        args: vec![self.env.fresh_type_var()],
                    }),
                    "Map" => Some(Type::Named {
                        name: "Map".to_string(),
                        args: vec![self.env.fresh_type_var(), self.env.fresh_type_var()],
                    }),
                    _ => None,
                };
                if let Some(ty) = result_ty {
                    self.record_expr_type(span, &ty);
                    return ty;
                }
            }
        }

        // Built-in output functions: println() / print() / eprintln().
        // Accept 0 or 1 Display-implementing argument; return Unit.
        if let ExprKind::Identifier(name) = &callee.kind {
            if name == "println" || name == "print" || name == "eprintln" {
                match args.len() {
                    0 => {}
                    1 => {
                        let arg_ty = self.infer_expr(&args[0].value);
                        if arg_ty != Type::Error && !self.type_supports_display(&arg_ty) {
                            self.type_error(
                                format!(
                                    "{}() argument must implement Display, \
                                     but '{}' does not",
                                    name,
                                    type_display(&arg_ty)
                                ),
                                args[0].value.span.clone(),
                                TypeErrorKind::TraitBoundNotSatisfied,
                            );
                        }
                    }
                    _ => {
                        self.type_error(
                            format!("{}() takes 0 or 1 argument(s), found {}", name, args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                    }
                }
                return Type::Unit;
            }
        }

        // Look up parameter names for label validation
        let param_names: Option<Vec<Option<String>>> = match &callee.kind {
            ExprKind::Identifier(name) => self
                .env
                .functions
                .get(name)
                .map(|sig| sig.param_names.clone()),
            ExprKind::Path { segments, .. } => segments.last().and_then(|name| {
                self.env
                    .functions
                    .get(name)
                    .map(|sig| sig.param_names.clone())
            }),
            _ => None,
        };

        if let Some(ref names) = param_names {
            self.validate_labels(args, names, span);
        }

        // Const generics slice 3c: look up the callee's where-clause
        // so the regular generic-call dispatch can discharge
        // `ConstPredicate`s against inferred const-args. The
        // explicit-generic-args path (`infer_explicit_generic_args_call`)
        // already threads the where-clause; this branch covers the
        // type-inferred case (`f(arr)` where N is inferred from
        // `arr`'s type).
        let callee_where_clause: Option<WhereClause> = match &callee.kind {
            ExprKind::Identifier(name) => self
                .env
                .functions
                .get(name)
                .and_then(|sig| sig.where_clause.clone()),
            ExprKind::Path { segments, .. } => segments.last().and_then(|name| {
                self.env
                    .functions
                    .get(name)
                    .and_then(|sig| sig.where_clause.clone())
            }),
            _ => None,
        };

        let callee_ty = self.infer_expr(callee);

        match &callee_ty {
            Type::Function {
                params,
                return_type,
            }
            | Type::OnceFunction {
                params,
                return_type,
            } => {
                if args.len() != params.len() {
                    self.type_error(
                        format!(
                            "expected {} argument(s), found {}",
                            params.len(),
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    // Still type-check the args we have
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return *return_type.clone();
                }
                let params = params.clone();
                let return_type = *return_type.clone();
                self.check_call_args_with_substitution_full(
                    args,
                    &params,
                    &return_type,
                    span,
                    /* apply_call_site_marker = */ true,
                    None,
                    None,
                    callee_where_clause.as_ref(),
                    span,
                )
            }
            Type::Error => {
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                Type::Error
            }
            _ => {
                self.type_error(
                    format!("type '{}' is not callable", type_display(&callee_ty)),
                    span.clone(),
                    TypeErrorKind::NotCallable,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                Type::Error
            }
        }
    }

    // ── Call-Site Mutation Marker (design.md Part 1½) ────────────

    /// Enforces the 1A call-site rule:
    ///   - Fresh binding to `mut ref T` / `mut Slice[T]` param → marker required.
    ///   - Forwarded mut-ref argument → marker not required (accept either).
    ///   - Owned / `ref T` param → marker rejected.
    ///
    /// "Forwarded" is classified by the place-expression root (or the argument's
    /// own type if it is already a mut-ref / mut-slice value — covers nested
    /// mut-ref returns like `other(wrap(mut v))`).
    fn check_call_site_marker(&mut self, arg: &CallArg, param_ty: &Type, arg_ty: &Type) {
        let param_is_mutating = matches!(param_ty, Type::MutRef(_))
            || matches!(param_ty, Type::Slice { mutable: true, .. });

        if !param_is_mutating {
            if arg.mut_marker {
                self.type_error(
                    format!(
                        "`mut` marker is not legal here — parameter expects `{}` \
                         (not a mutable borrow). Remove `mut`.",
                        type_display(param_ty)
                    ),
                    arg.span.clone(),
                    TypeErrorKind::InvalidMutMarker,
                );
            }
            return;
        }

        let forwarded = self.is_arg_forwarded(&arg.value, arg_ty);

        if arg.mut_marker && forwarded {
            // The argument is already a mut-ref (either by type or by
            // place-root) — marking it is redundant and, in the nested
            // mut-ref-return case, actively wrong.
            self.type_error(
                "this argument is already a mut-ref; drop the `mut` marker. \
                 The mutation surface was announced at the callee or enclosing \
                 scope's signature."
                    .to_string(),
                arg.span.clone(),
                TypeErrorKind::InvalidMutMarker,
            );
            return;
        }

        if !arg.mut_marker && !forwarded {
            self.type_error(
                format!(
                    "parameter expects `{}`; call with fresh binding requires \
                     a `mut` marker at this argument to permit the mutation. \
                     Write `mut <expr>`.",
                    type_display(param_ty)
                ),
                arg.span.clone(),
                TypeErrorKind::MissingMutMarker,
            );
        }
    }

    /// An argument is *forwarded* (already a mut-ref handed to this call) if:
    ///   (A) its own inferred type is `mut ref T` / `mut Slice[T]`, or
    ///   (B) it is a place expression whose root binding is typed
    ///       `mut ref T` / `mut Slice[T]` in the current scope.
    /// Otherwise the argument is *fresh* (owned local, temporary, literal,
    /// non-mut-ref call return, etc.).
    fn is_arg_forwarded(&self, expr: &Expr, arg_ty: &Type) -> bool {
        // (A) Argument's own type is already mut-ref / mut-slice.
        if matches!(arg_ty, Type::MutRef(_)) || matches!(arg_ty, Type::Slice { mutable: true, .. })
        {
            return true;
        }
        // (B) Place-expression root is a mut-ref / mut-slice binding.
        self.place_root_is_mut_borrow(expr)
    }

    fn place_root_is_mut_borrow(&self, expr: &Expr) -> bool {
        let mut e = expr;
        loop {
            match &e.kind {
                ExprKind::Identifier(name) => {
                    return matches!(
                        self.local_scope.lookup(name),
                        Some(Type::MutRef(_)) | Some(Type::Slice { mutable: true, .. })
                    );
                }
                ExprKind::SelfValue => {
                    return matches!(
                        self.local_scope.lookup("self"),
                        Some(Type::MutRef(_)) | Some(Type::Slice { mutable: true, .. })
                    );
                }
                ExprKind::FieldAccess { object, .. } => e = object,
                ExprKind::TupleIndex { object, .. } => e = object,
                ExprKind::Index { object, .. } => e = object,
                // Non-place expressions: literal, call, block, binop, etc.
                _ => return false,
            }
        }
    }

    // ── Pipe Desugaring ──────────────────────────────────────────

    fn infer_pipe(&mut self, left: &Expr, right: &Expr, span: &Span) -> Type {
        match &right.kind {
            // a |> f => f(a)
            ExprKind::Identifier(_) | ExprKind::Path { .. } => {
                let synthetic_arg = CallArg {
                    label: None,
                    mut_marker: false,
                    value: left.clone(),
                    span: left.span.clone(),
                };
                self.infer_call(right, &[synthetic_arg], span)
            }

            // a |> f(args...) => f(a, args...) or f(args with _ replaced)
            ExprKind::Call { callee, args } => {
                // Count _ placeholders in args
                let placeholder_count = args
                    .iter()
                    .filter(|arg| matches!(arg.value.kind, ExprKind::PipePlaceholder))
                    .count();

                if placeholder_count > 1 {
                    self.type_error(
                        "at most one '_' placeholder allowed per pipe stage".to_string(),
                        right.span.clone(),
                        TypeErrorKind::InvalidPipePlaceholder,
                    );
                    self.infer_expr(callee);
                    for arg in args {
                        if !matches!(arg.value.kind, ExprKind::PipePlaceholder) {
                            self.infer_expr(&arg.value);
                        }
                    }
                    return Type::Error;
                }

                // Build the desugared argument list
                let desugared_args: Vec<CallArg> = if placeholder_count == 1 {
                    // Replace _ with the left-hand value
                    args.iter()
                        .map(|arg| {
                            if matches!(arg.value.kind, ExprKind::PipePlaceholder) {
                                CallArg {
                                    label: arg.label.clone(),
                                    mut_marker: arg.mut_marker,
                                    value: left.clone(),
                                    span: left.span.clone(),
                                }
                            } else {
                                arg.clone()
                            }
                        })
                        .collect()
                } else {
                    // No placeholder — prepend left as first argument
                    let mut new_args = vec![CallArg {
                        label: None,
                        mut_marker: false,
                        value: left.clone(),
                        span: left.span.clone(),
                    }];
                    new_args.extend(args.iter().cloned());
                    new_args
                };

                self.infer_call(callee, &desugared_args, span)
            }

            _ => {
                self.type_error(
                    "right-hand side of pipe must be a function name or function call".to_string(),
                    right.span.clone(),
                    TypeErrorKind::NotCallable,
                );
                self.infer_expr(right);
                Type::Error
            }
        }
    }

    // ── ? operator ──────────────────────────────────────────────

    /// Type-check `inner?`: validate that the operand is `Result[T, E1]` or
    /// `Option[T]`, that the enclosing function returns a compatible variant,
    /// and (for Result) that error types match exactly or convert via `From`.
    /// Returns the unwrapped success type (`T`).
    fn infer_question(&mut self, inner: &Expr, span: &Span) -> Type {
        let inner_ty = self.infer_expr(inner);
        if inner_ty == Type::Error {
            return Type::Error;
        }

        let (inner_name, inner_args) = match &inner_ty {
            Type::Named { name, args } => (name.clone(), args.clone()),
            _ => {
                self.type_error(
                    format!(
                        "'?' operator requires `Result` or `Option`, found '{}'",
                        type_display(&inner_ty)
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };

        let return_ty = match self.current_return_type.clone() {
            Some(t) => t,
            None => {
                self.type_error(
                    "'?' operator used outside a function body".to_string(),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };
        let (ret_name, ret_args) = match &return_ty {
            Type::Named { name, args } => (name.clone(), args.clone()),
            _ => {
                self.type_error(
                    format!(
                        "'?' requires the enclosing function to return `Result` or `Option`, found '{}'",
                        type_display(&return_ty)
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };

        match (inner_name.as_str(), ret_name.as_str()) {
            ("Option", "Option") if inner_args.len() == 1 && ret_args.len() == 1 => {
                inner_args[0].clone()
            }
            ("Result", "Result") if inner_args.len() == 2 && ret_args.len() == 2 => {
                let inner_err = &inner_args[1];
                let ret_err = &ret_args[1];
                if inner_err == ret_err {
                    return inner_args[0].clone();
                }
                // Cross-error type: require `impl From[InnerErr] for RetErr`.
                let target_name = match ret_err {
                    Type::Named { name, .. } => name.clone(),
                    _ => {
                        self.type_error(
                            format!(
                                "'?' cannot propagate error '{}' as '{}': target is not a named type",
                                type_display(inner_err),
                                type_display(ret_err)
                            ),
                            span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                };
                if self
                    .env
                    .find_from_impl(inner_err, &target_name, &[])
                    .is_some()
                {
                    self.question_conversions
                        .insert(SpanKey::from_span(span), target_name.clone());
                    return inner_args[0].clone();
                }
                self.type_error(
                    format!(
                        "'?' cannot convert error '{}' to '{}': no `impl From[{}] for {}` in scope",
                        type_display(inner_err),
                        type_display(ret_err),
                        type_display(inner_err),
                        target_name
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
            ("Result", "Option") | ("Option", "Result") => {
                self.type_error(
                    format!(
                        "'?' cannot mix `Result` and `Option`: operand is '{}', function returns '{}'",
                        type_display(&inner_ty),
                        type_display(&return_ty)
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
            _ => {
                self.type_error(
                    format!(
                        "'?' requires operand and return type to be `Result` or `Option`, found '{}' and '{}'",
                        type_display(&inner_ty),
                        type_display(&return_ty)
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
        }
    }

    // ── Method Calls ────────────────────────────────────────────

    /// True when `name` is unresolvable as a value (no local, function,
    /// constant, or builtin), but at least one visible trait declares it as
    /// an associated function. Mirrors the resolver's `is_trait_assoc_fn_name`
    /// suppression rule — used by `infer_call` to surface a "cannot infer"
    /// error in synthesis position rather than silently returning `Type::Error`.
    fn is_unresolvable_trait_assoc_fn(&self, name: &str) -> bool {
        if self.local_scope.lookup(name).is_some()
            || self.env.functions.contains_key(name)
            || self.env.constants.contains_key(name)
            || matches!(
                name,
                "todo" | "unreachable" | "println" | "print" | "eprintln" | "panic"
            )
        {
            return false;
        }
        // Also skip if the name resolves as an enum variant constructor.
        for enum_info in self.env.enums.values() {
            if enum_info.variants.iter().any(|(v, _)| v == name) {
                return false;
            }
        }
        for item in &self.program.items {
            if let Item::TraitDef(t) = item {
                for ti in &t.items {
                    if let TraitItem::Method(m) = ti {
                        if m.name == name && m.self_param.is_none() {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Locate the AST `TraitMethod` declaration for `trait_name.method_name`.
    /// Walks `program.items` looking for a matching `Item::TraitDef`. Returns
    /// `None` if the trait is not declared in the current program (stdlib /
    /// derive-only / built-in traits do not have AST nodes here, so callers
    /// must treat absence as "trait does not declare this method via AST").
    fn find_trait_method<'p>(
        &'p self,
        trait_name: &str,
        method_name: &str,
    ) -> Option<&'p crate::ast::TraitMethod> {
        // User program first (so user-defined traits with the same name
        // shadow stdlib if such a case ever arises — though stdlib trait
        // names are reserved per design.md).
        for item in &self.program.items {
            if let Item::TraitDef(t) = item {
                if t.name == trait_name {
                    for ti in &t.items {
                        if let TraitItem::Method(m) = ti {
                            if m.name == method_name {
                                return Some(m);
                            }
                        }
                    }
                }
            }
        }
        // Baked stdlib (`STDLIB_PROGRAMS`): trait declarations like
        // `Display`, `Iterator`, `Ord`, etc. live here. Walking the
        // baked surface lets `T: Display`-bounded type params resolve
        // their `.to_string()` etc. without requiring user redeclaration.
        // Slice 2 of the method-resolution CR — the receiver-form
        // dispatch path needs this for `T: Display` to find Display's
        // `to_string` method, and the same fix benefits the existing
        // type-prefixed dispatch.
        for (_, program) in crate::prelude::STDLIB_PROGRAMS.iter() {
            for item in &program.items {
                if let Item::TraitDef(t) = item {
                    if t.name == trait_name {
                        for ti in &t.items {
                            if let TraitItem::Method(m) = ti {
                                if m.name == method_name {
                                    return Some(m);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Attempt to dispatch `T.method(args)` where `T` is a generic type
    /// parameter (resolver records its bounds under the receiver's SymbolId).
    /// `callee_span` is the span of the `Path(["T", "method"])` expression
    /// — the resolver records `T`'s SymbolId there. Returns `Some(return_type)`
    /// when dispatch succeeds, `None` to fall through to the existing
    /// concrete-type / value-receiver paths.
    ///
    /// Multiple bound traits declaring the same method name → ambiguity error
    /// plus `Type::Error`. Exactly one match → lower the trait method's
    /// signature with `Self → Type::TypeParam(type_name)` substitution and
    /// validate args.
    /// Receiver-form complement to [`Self::try_dispatch_typeparam_assoc_fn`].
    /// Slice 2 of the method-resolution CR (see `phase-4-interpreter.md` item
    /// 8). Called from `infer_method_call`'s receiver-type match when the
    /// receiver is `Type::TypeParam(name)`. Looks up `name`'s bounds in
    /// `enclosing_bounds` (populated by `collect_param_bounds`), finds bound
    /// traits that declare a *method* (with `self_param`) of the requested
    /// name, and dispatches.
    ///
    /// Branch on candidate count:
    /// - zero → emit `NoMethodFound` diagnostic, return `Type::Error`.
    /// - one → dispatch via `dispatch_trait_assoc_fn` (which substitutes
    ///   `Self → Type::TypeParam(name)` in the method's signature). The
    ///   trait method's `params` already excludes `self_param` per the
    ///   AST shape, so `args.len()` matches `method.params.len()` — no
    ///   off-by-one for the implicit receiver.
    /// - more → emit `AmbiguousAssocFn` (E0233) listing each candidate
    ///   trait with a UFCS-disambiguation hint.
    ///
    /// Self-mode compatibility (calling a `mut ref self` method on a `ref`
    /// receiver) is the param-binding layer's concern, not this dispatcher's.
    fn dispatch_typeparam_receiver_method(
        &mut self,
        type_param_name: &str,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let bounds = match self.enclosing_bounds.get(type_param_name) {
            Some(b) => b.clone(),
            None => Vec::new(),
        };
        let candidates: Vec<(String, crate::ast::TraitMethod)> = bounds
            .iter()
            .filter_map(|b| b.path.last().cloned())
            .filter_map(|trait_name| {
                let m = self.find_trait_method(&trait_name, method)?;
                // Only methods (with self_param) are receiver-form
                // candidates. Associated functions (no self_param) reach
                // the dispatch only through type-prefixed `T.method()`.
                m.self_param.as_ref()?;
                Some((trait_name, m.clone()))
            })
            .collect();

        match candidates.len() {
            0 => {
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                self.type_error(
                    format!(
                        "no method '{}' on type parameter '{}'; \
                         add a trait bound declaring it (e.g. `{}: SomeTrait`)",
                        method, type_param_name, type_param_name,
                    ),
                    span.clone(),
                    TypeErrorKind::NoMethodFound,
                );
                Type::Error
            }
            1 => {
                let (_trait_name, trait_method) = candidates.into_iter().next().unwrap();
                self.dispatch_trait_assoc_fn(type_param_name, &trait_method, args, span)
            }
            _ => {
                let trait_list = candidates
                    .iter()
                    .map(|(t, _)| format!("`{}`", t))
                    .collect::<Vec<_>>()
                    .join(", ");
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                self.type_error(
                    format!(
                        "ambiguous method '{}' on type parameter '{}': declared by {}. \
                         Use UFCS `Trait.{}(receiver, ...)` to disambiguate.",
                        method, type_param_name, trait_list, method,
                    ),
                    span.clone(),
                    TypeErrorKind::AmbiguousAssocFn,
                );
                Type::Error
            }
        }
    }

    /// Receiver-form `self.method(args)` dispatch inside a trait default
    /// body. Slice 3.5 of the method-resolution CR — see
    /// `phase-4-interpreter.md` item 8. Closes the explicit `name == "Self"`
    /// silent-fallthrough that slice 2 left in place when wiring the
    /// receiver-form `Type::TypeParam` arm.
    ///
    /// Candidates are gathered from the enclosing trait's *own* methods plus
    /// every method on traits in the supertrait closure (filtered to those
    /// declaring a `self_param`, since associated functions reach the
    /// dispatch only through type-prefixed `Type.method()`).
    ///
    /// Branch on candidate count:
    /// - zero → `NoMethodFound` (E0236).
    /// - one → dispatch via `dispatch_trait_assoc_fn` with `target = "Self"`.
    /// - more → `AmbiguousAssocFn` (E0233) listing each declarer with a UFCS
    ///   hint. (Slice 3's `AmbiguousMethod` is for cross-impl ambiguity at
    ///   concrete-receiver sites; the Self-receiver path is closer in shape
    ///   to the type-parameter dispatcher's multi-bound case.)
    ///
    /// Returns `Type::Error` outside a trait body (when `enclosing_trait` is
    /// `None`) so the caller's silent-fallthrough behavior is preserved for
    /// non-trait `Self` cases (impl-method bodies bind `Self` to the impl's
    /// target type via `current_self_type`, a different mechanism).
    fn dispatch_self_receiver_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let trait_name = match self.enclosing_trait.clone() {
            Some(name) => name,
            None => {
                // Not inside a trait body — `Self` here resolves through a
                // different mechanism (impl-method `current_self_type`).
                // Preserve the pre-existing silent fallthrough.
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        };

        // Candidate traits: enclosing trait first, then its supertrait closure.
        let candidate_traits = self.env.supertrait_closure_traits(&trait_name);
        let candidates: Vec<(String, crate::ast::TraitMethod)> = candidate_traits
            .iter()
            .filter_map(|t| {
                let m = self.find_trait_method(t, method)?;
                // Receiver-form requires a self_param.
                m.self_param.as_ref()?;
                Some((t.clone(), m.clone()))
            })
            .collect();

        match candidates.len() {
            0 => {
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                self.type_error(
                    format!(
                        "no method '{}' found on `Self` in trait '{}'; \
                         declare it on the trait or a supertrait",
                        method, trait_name,
                    ),
                    span.clone(),
                    TypeErrorKind::NoMethodFound,
                );
                Type::Error
            }
            1 => {
                let (_t, trait_method) = candidates.into_iter().next().unwrap();
                self.dispatch_trait_assoc_fn("Self", &trait_method, args, span)
            }
            _ => {
                let trait_list = candidates
                    .iter()
                    .map(|(t, _)| format!("`{}`", t))
                    .collect::<Vec<_>>()
                    .join(", ");
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                self.type_error(
                    format!(
                        "ambiguous method '{}' on `Self` in trait '{}': declared by {}. \
                         Use UFCS `Trait.{}(self, ...)` to disambiguate.",
                        method, trait_name, trait_list, method,
                    ),
                    span.clone(),
                    TypeErrorKind::AmbiguousAssocFn,
                );
                Type::Error
            }
        }
    }

    fn try_dispatch_typeparam_assoc_fn(
        &mut self,
        type_name: &str,
        method: &str,
        callee_span: &Span,
        args: &[CallArg],
        call_span: &Span,
    ) -> Option<Type> {
        let span_key = SpanKey::from_span(callee_span);
        let sym_id = self.resolve_result.resolutions.get(&span_key).copied()?;
        let sym = self.resolve_result.symbol_table.get_symbol(sym_id);
        if !matches!(sym.kind, SymbolKind::TypeParam) {
            return None;
        }
        let bounds = self.resolve_result.symbol_table.get_generic_bounds(sym_id);
        let candidates: Vec<String> = bounds
            .iter()
            .filter_map(|b| b.path.last().cloned())
            .filter(|trait_name| self.find_trait_method(trait_name, method).is_some())
            .collect();
        match candidates.len() {
            0 => None,
            1 => {
                let trait_name = candidates[0].clone();
                let trait_method = self.find_trait_method(&trait_name, method)?.clone();
                Some(self.dispatch_trait_assoc_fn(type_name, &trait_method, args, call_span))
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
                        method, type_name, trait_list, method,
                    ),
                    call_span.clone(),
                    TypeErrorKind::AmbiguousAssocFn,
                );
                Some(Type::Error)
            }
        }
    }

    /// Lower a trait method's signature with `Self → Type::TypeParam(target)`
    /// substitution, then validate `args` against it. Used for type-parameter
    /// dispatch (`T.method()` where `T: Trait`). The returned type is the
    /// substituted return type; `Unit` for methods with no return.
    fn dispatch_trait_assoc_fn(
        &mut self,
        target: &str,
        method: &crate::ast::TraitMethod,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let mut subs: HashMap<String, SubstValue> = HashMap::new();
        subs.insert(
            "Self".to_string(),
            SubstValue::Type(Type::TypeParam(target.to_string())),
        );

        let mut scope = vec!["Self".to_string()];
        if let Some(ref gp) = method.generic_params {
            scope.extend(gp.params.iter().map(|p| p.name.clone()));
        }

        let param_types: Vec<Type> = method
            .params
            .iter()
            .map(|p| {
                let lowered = self.lower_type_expr(&p.ty, &scope);
                substitute_type_params(&lowered, &subs)
            })
            .collect();

        if args.len() != param_types.len() {
            self.type_error(
                format!(
                    "method '{}' expects {} argument(s), found {}",
                    method.name,
                    param_types.len(),
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
        } else {
            for (arg, param) in args.iter().zip(param_types.iter()) {
                let arg_ty = self.infer_expr(&arg.value);
                self.check_assignable(param, &arg_ty, arg.value.span.clone());
            }
        }

        let ret = method
            .return_type
            .as_ref()
            .map(|rt| self.lower_type_expr(rt, &scope))
            .unwrap_or(Type::Unit);
        substitute_type_params(&ret, &subs)
    }

    fn infer_method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // Lowercase stdlib module aliases: `env.args()`, `env.var(name)`.
        // These use lowercase module names (design.md § I/O), distinct from
        // the capitalized resource names used by the effect system. Map each
        // lowercase module to its capitalized resource equivalent so the
        // shared method signatures are found — first in the baked-impl table
        // (`env.impls`, where the slice-2 migration moved `Env.args` /
        // `Env.var`), then in `env.functions` for any future entries that
        // can't be expressed as impl methods.
        if let ExprKind::Identifier(mod_name) = &object.kind {
            let resource_name = match mod_name.as_str() {
                "env" => Some("Env"),
                _ => None,
            };
            if let Some(resource) = resource_name {
                let impl_sig = self.env.impls.iter().find_map(|imp| {
                    // Lowercase-module dispatch (`env.args()`) targets
                    // ambient resource impls registered with empty
                    // target_args; specialized variants of these don't
                    // exist today.
                    if imp.target_type == resource && imp.target_args.is_empty() {
                        imp.methods.get(method).cloned()
                    } else {
                        None
                    }
                });
                let dotted = format!("{}.{}", resource, method);
                let sig_opt = impl_sig.or_else(|| self.env.functions.get(&dotted).cloned());
                if let Some(sig) = sig_opt {
                    if args.len() != sig.params.len() {
                        self.type_error(
                            format!(
                                "'{}.{}' expects {} argument(s), found {}",
                                mod_name,
                                method,
                                sig.params.len(),
                                args.len()
                            ),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return sig.return_type;
                    }
                    for (arg, param_ty) in args.iter().zip(sig.params.iter()) {
                        let at = self.infer_expr(&arg.value);
                        self.check_assignable(param_ty, &at, arg.value.span.clone());
                    }
                    return sig.return_type;
                }
            }
        }

        // Type-receiver associated calls: `T.method(args)` where `T` is a
        // type name (struct, enum, or primitive). The parser produces a
        // MethodCall with `object = Identifier("T")`; the regular receiver
        // pipeline below would treat `T` as a value and fail.
        //
        // From dispatch is special-cased — the source type of the argument
        // disambiguates between multiple `impl From[X] for T` impls.
        if let ExprKind::Identifier(type_name) = &object.kind {
            let is_known_type = self.env.structs.contains_key(type_name)
                || self.env.enums.contains_key(type_name)
                || matches!(
                    type_name.as_str(),
                    "i8" | "i16"
                        | "i32"
                        | "i64"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "usize"
                        | "f32"
                        | "f64"
                        | "bool"
                        | "char"
                        | "String"
                );
            if is_known_type {
                // Cancel-narrowing side-table: record `Type.method` for this
                // call site so codegen can elide the par-branch cancel check
                // when the resolved callee is provably non-effectful.
                self.method_callee_types.insert(
                    SpanKey::from_span(span),
                    format!("{}.{}", type_name, method),
                );
                if method == "from" && args.len() == 1 {
                    let arg_ty = self.infer_expr(&args[0].value);
                    if arg_ty == Type::Error {
                        return Type::Error;
                    }
                    if let Some(imp) = self.env.find_from_impl(&arg_ty, type_name, &[]) {
                        return imp
                            .methods
                            .get("from")
                            .map(|sig| sig.return_type.clone())
                            .unwrap_or(Type::Error);
                    }
                    self.type_error(
                        format!(
                            "no `impl From[{}] for {}` is in scope",
                            type_display(&arg_ty),
                            type_name
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                // General associated call: look up the method on the target
                // type with inherent-beats-trait priority per design.md
                // § Method Resolution Step 3. Multi-inherent / multi-trait
                // ambiguity detection (Step 4) is deferred.
                if let Some(sig) = self.env.find_method(type_name, &[], method).cloned() {
                    if args.len() != sig.params.len() {
                        self.type_error(
                            format!(
                                "method '{}' expects {} argument(s), found {}",
                                method,
                                sig.params.len(),
                                args.len()
                            ),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return sig.return_type;
                    }
                    for (arg, param) in args.iter().zip(sig.params.iter()) {
                        let arg_ty = self.infer_expr(&arg.value);
                        self.check_assignable(param, &arg_ty, arg.value.span.clone());
                    }
                    return sig.return_type;
                }
                // Known type but no matching method — fall through so the
                // existing "method not found" diagnostic fires below.
            }
        }

        // Concrete-type UFCS dispatch — `TypeName[T1, T2, ...].method(args)`.
        // The parser disambiguates `TypeName[…].method(` to a single-segment
        // `Path { generic_args: Some(...) }` object; here we route through
        // `find_methods_with_args` so impl-level bounds discharge against
        // the explicit type-args, then substitute each impl-level generic
        // param with its concrete arg in the sig before validating call args.
        // (Sub-item 5B of `phase-4-interpreter.md` § method resolution;
        // canonical entry at `phase-2-parser-ast.md` § "Path expression with
        // generic args — concrete-type UFCS support".)
        if let ExprKind::Path {
            segments,
            generic_args: Some(generic_args),
        } = &object.kind
        {
            if segments.len() == 1 {
                let type_name = segments[0].clone();
                // Concrete-type UFCS at slice 1b widens generic_args to
                // `Vec<GenericArg>`; the dispatch surface still consumes
                // type args only — const-arg binding for UFCS calls
                // lands when slice 3's call-site solver threads the
                // substitution through. Const-args at this position are
                // ignored for dispatch but still parsed so the shape
                // round-trips cleanly.
                let target_args: Vec<Type> = generic_args
                    .iter()
                    .filter_map(|a| match a {
                        GenericArg::Type(t) => Some(self.lower_type_expr(t, &[])),
                        GenericArg::Const(_) => None,
                    })
                    .collect();
                self.method_callee_types.insert(
                    SpanKey::from_span(span),
                    format!("{}.{}", type_name, method),
                );
                let candidates: Vec<(ImplInfo, FunctionSig)> = self
                    .env
                    .find_methods_with_args(&type_name, &target_args, method)
                    .into_iter()
                    .map(|(imp, sig)| (imp.clone(), sig.clone()))
                    .collect();
                // Slice 5C of the method-resolution CR — see
                // `phase-4-interpreter.md` § method-resolution sub-item 5C.
                // `find_methods_with_args` already applies the inherent-
                // beats-trait priority partition + bounds-discharge filter
                // (slices 1 + 3); a length-≥2 result here means multiple
                // candidates of the same priority tier survived. The
                // user must pick a specific UFCS form (`TraitName.method(...)`)
                // to disambiguate. Mirrors slice 3's receiver-form
                // `AmbiguousMethod` (E0239) but uses `AmbiguousAssocFn`
                // (E0233) to match slice 3.5 and slice 5A's framing —
                // type-prefixed dispatch is the natural disambiguation
                // form for UFCS.
                if candidates.len() > 1 {
                    let receiver_display = if target_args.is_empty() {
                        type_name.clone()
                    } else {
                        format!(
                            "{}[{}]",
                            type_name,
                            target_args
                                .iter()
                                .map(type_display)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    let candidate_lines: Vec<String> = candidates
                        .iter()
                        .map(|(imp, sig)| {
                            let dispatcher = imp
                                .trait_name
                                .clone()
                                .unwrap_or_else(|| imp.target_type.clone());
                            let subs: HashMap<String, SubstValue> = imp
                                .generic_params
                                .as_ref()
                                .map(|gp| {
                                    gp.params
                                        .iter()
                                        .zip(target_args.iter())
                                        .map(|(p, t)| (p.name.clone(), SubstValue::Type(t.clone())))
                                        .collect()
                                })
                                .unwrap_or_default();
                            let params_display = sig
                                .params
                                .iter()
                                .map(|p| type_display(&substitute_type_params(p, &subs)))
                                .collect::<Vec<_>>()
                                .join(", ");
                            let return_display =
                                type_display(&substitute_type_params(&sig.return_type, &subs));
                            format!(
                                "    `{}.{}({})` -> {}",
                                dispatcher, method, params_display, return_display,
                            )
                        })
                        .collect();
                    self.type_error(
                        format!(
                            "ambiguous method '{}' on `{}`: \
                             multiple candidates apply. Use UFCS to disambiguate:\n{}",
                            method,
                            receiver_display,
                            candidate_lines.join("\n"),
                        ),
                        span.clone(),
                        TypeErrorKind::AmbiguousAssocFn,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                if let Some((imp, sig)) = candidates.first() {
                    let subs: HashMap<String, SubstValue> = imp
                        .generic_params
                        .as_ref()
                        .map(|gp| {
                            gp.params
                                .iter()
                                .zip(target_args.iter())
                                .map(|(p, t)| (p.name.clone(), SubstValue::Type(t.clone())))
                                .collect()
                        })
                        .unwrap_or_default();
                    let param_types: Vec<Type> = sig
                        .params
                        .iter()
                        .map(|p| substitute_type_params(p, &subs))
                        .collect();
                    let return_ty = substitute_type_params(&sig.return_type, &subs);
                    if args.len() != param_types.len() {
                        self.type_error(
                            format!(
                                "method '{}' expects {} argument(s), found {}",
                                method,
                                param_types.len(),
                                args.len()
                            ),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return return_ty;
                    }
                    for (arg, param) in args.iter().zip(param_types.iter()) {
                        let arg_ty = self.infer_expr(&arg.value);
                        self.check_assignable(param, &arg_ty, arg.value.span.clone());
                    }
                    return return_ty;
                }
                // No matching impl-table entry. Built-in types (Vec, Option,
                // etc.) whose methods dispatch through special-case infer
                // paths rather than `env.impls` are out of scope for this
                // slice; falling through to a focused diagnostic.
                self.type_error(
                    format!("no method '{}' on `{}[…]`", method, type_name),
                    span.clone(),
                    TypeErrorKind::NoMethodFound,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        }

        let obj_ty = self.infer_expr(object);
        if obj_ty == Type::Error {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }

        // Cancel-narrowing side-table: record `Type.method` for this call
        // site so codegen can elide the par-branch cancel check when the
        // resolved callee is provably non-effectful. Populated here once so
        // it covers every dispatch path below (Slice, String, Map, named
        // types, etc.) — the parser sets `MethodCall.span == receiver.span`,
        // so we use `method_callee_types` rather than `expr_types` (which
        // would race with the return-type insertion at the same key).
        if let Some(type_name) = method_callee_type_name(&obj_ty) {
            self.method_callee_types.insert(
                SpanKey::from_span(span),
                format!("{}.{}", type_name, method),
            );
        }

        // Stdlib slice views on sequence types. `.as_slice()` / `.as_slice_mut()`
        // on a `Vec[T]` or `Array[T, N]` (or their ref borrows) produce a
        // `Slice[T]` / `mut Slice[T]` handle, per design.md § Slices.
        if (method == "as_slice" || method == "as_slice_mut") && args.is_empty() {
            let mutable = method == "as_slice_mut";
            let element = match &obj_ty {
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
                _ => None,
            };
            if let Some(el) = element {
                return Type::Slice {
                    element: Box::new(el),
                    mutable,
                };
            }
        }

        // `Slice[T]` and `mut Slice[T]` method dispatch. These types are not
        // `Type::Named` so they fall through the generic branch below; handle
        // them here before the named-type extraction.
        if let Type::Slice { element, mutable } = &obj_ty.clone() {
            return self.infer_slice_method(element, *mutable, method, args, span);
        }

        // Iterator-source methods: `iter()` / `into_iter()` on any iterable
        // collection produce an `Iterator[Item = T]` value. Handled here in
        // one place so per-collection method handlers don't have to repeat
        // the registration. The borrow-vs-consume distinction between
        // `iter()` and `into_iter()` is a typechecker concern in design.md
        // but immaterial at this layer — both return the same Iterator type.
        // See `wip-list2.md` § Iterator trait — full adaptor surface.
        if method == "iter" || method == "into_iter" {
            if let Some(item_ty) = iterator_item_type_for(&obj_ty) {
                if !args.is_empty() {
                    self.type_error(
                        format!("'{}' takes no arguments", method),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                return Type::Named {
                    name: "Iterator".to_string(),
                    args: vec![item_ty],
                };
            }
        }

        // `clone()` on collection types — `Vec[T]`, `String`, `Map[K, V]`,
        // `Set[T]`, `SortedSet[T]`, `Array[T, N]` all implement Clone per
        // design.md § Iteration line 1692. Returns `Self`. The `T: Clone`
        // bound on element types is enforced via the existing trait-bound
        // checking; primitives and String satisfy it trivially. The
        // canonical bullet lives in `phase-8-stdlib-floor.md` (search
        // `Clone trait surface for collections`).
        if method == "clone" {
            if let Some(self_ty) = clone_self_type_for(&obj_ty) {
                if !args.is_empty() {
                    self.type_error(
                        "clone() takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                }
                return self_ty;
            }
        }

        // Iterator method dispatch — `Iterator[Item = T].next()` and the
        // adaptor surface (added in subtask 3+). Keyed on the receiver's
        // outer Type::Named name; the Item type is at args[0].
        // `Range` / `RangeInclusive` are also Iterators (matches Rust),
        // routed through the same dispatch so `(0..10).step_by(2)` works
        // without a redundant `.iter()` call.
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty
        {
            if name == "Iterator"
                || name == "Peekable"
                || name == "Range"
                || name == "RangeInclusive"
            {
                let item_ty = type_args.first().cloned().unwrap_or(Type::Error);
                let is_peekable = name == "Peekable";
                return self.infer_iterator_method(&item_ty, method, args, span, is_peekable);
            }
        }

        // `Vec[T].push(item: T)` slot check (round 12.46 / Step 4). Vec is a
        // built-in prelude type with no impl block, so without this dispatch
        // `push` falls through to the silent `Type::Error` arm below and the
        // argument never gets checked against the element type. Routing the
        // single argument through `check_assignable(element, arg_ty, span)`
        // means a once-callable closure value flowing into a `Vec[Fn(...)]`
        // element slot triggers `OnceFnIntoFnSlot` via the same path Step 3
        // wired for parameter slots. Other Vec methods continue through the
        // historical fall-through to preserve existing test behavior — Step 5
        // can promote them when needed.
        if method == "push" && args.len() == 1 {
            let element_ty = match &obj_ty {
                Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                let arg_ty = self.infer_expr(&args[0].value);
                // Unify so an unsolved element typevar bound to the
                // receiver (e.g. `let mut v = Vec.new(); v.push(x);`)
                // gets pinned to the first push's value type. Otherwise
                // the binding's `pattern_binding_inner_types` entry
                // stays unresolved and codegen registers `i64` instead
                // of the right LLVM element type.
                unify_types(
                    &elem,
                    &arg_ty,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
                // Resolve `elem` so a successful unification doesn't
                // leave the assignability check comparing the stale
                // typevar against the (now-pinned) arg type and emitting
                // a spurious `?T → ArgT` mismatch diagnostic.
                let resolved_elem = resolve_type_var_top(&elem, &self.env.substitutions);
                self.check_assignable(&resolved_elem, &arg_ty, args[0].value.span.clone());
                return Type::Unit;
            }
        }

        // `Vec[T].pop()` / `Vec[T].pop_back()` and `VecDeque[T]`'s
        // `pop_front` / `pop_back` all return `Option[T]` per design.md.
        // The codegen-side pop arm builds an `Option[T]` aggregate via
        // multi-word payload words (commit 76263d1); without the
        // typechecker recording the return type, an unannotated
        // `match q.pop_front() { Some(node) => ... }` infers scrutinee
        // type `Error` and pattern bindings lose their tuple types,
        // breaking the `Some(node) => let (a, b) = node` shape's
        // tuple-binding reconstitution in codegen.
        if matches!(method, "pop" | "pop_back" | "pop_front") && args.is_empty() {
            let element_ty = match &obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                // Resolve typevars so a `let mut q = VecDeque.new(); q.push(x);
                // let _ = q.pop_front();` round-trips the element type — without
                // this, `?T` solved by `push` stays unresolved in the
                // `Option[?T]` return, and downstream `Some(x)` bindings lose
                // the surface type they need for codegen routing.
                let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
                return Type::Named {
                    name: "Option".to_string(),
                    args: vec![resolved],
                };
            }
        }

        // `Vec[T].get_unchecked(i: i64) -> T` — unsafe direct-index read.
        // Skips the bounds check that `vec[i]` and `Vec.get(i)` emit; UB on
        // out-of-range index. Must be called inside `unsafe { ... }`; the
        // enforcement is hardcoded in `unsafe_lint::build_unsafe_fn_registry`
        // (the built-in equivalent of marking an impl-method `unsafe fn`).
        // Counterpart to the deferred `Slice.get_unchecked` plan at
        // `phase-7-codegen.md:481`; surfaced as the perf lever for the
        // bounds-check tax measured on kata #5 (see `wip-kata5-perf.md`).
        if method == "get_unchecked" && args.len() == 1 {
            let element_ty = match &obj_ty {
                Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                let arg_ty = self.infer_expr(&args[0].value);
                self.check_assignable(
                    &Type::Int(IntSize::I64),
                    &arg_ty,
                    args[0].value.span.clone(),
                );
                return resolve_type_var_top(&elem, &self.env.substitutions);
            }
        }

        // `VecDeque[T].push_back(item)` / `push_front(item)` — slot
        // check sibling to `Vec.push`. Returns `Type::Unit`.
        if matches!(method, "push_back" | "push_front") && args.len() == 1 {
            let element_ty = match &obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = element_ty {
                let arg_ty = self.infer_expr(&args[0].value);
                // Mirror the unification in the `Vec.push` arm above so an
                // unsolved receiver-element typevar gets pinned to the first
                // pushed value type.
                unify_types(
                    &elem,
                    &arg_ty,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
                let resolved_elem = resolve_type_var_top(&elem, &self.env.substitutions);
                self.check_assignable(&resolved_elem, &arg_ty, args[0].value.span.clone());
                return Type::Unit;
            }
        }

        // `String` method dispatch. `Type::Str` is not `Type::Named` so it
        // also falls through the generic branch; handle it here.
        if obj_ty == Type::Str {
            return self.infer_str_method(method, args, span);
        }

        // `Map[K, V]` method dispatch. K and V thread through return types.
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty
        {
            if name == "Map" {
                let key = type_args.first().cloned().unwrap_or(Type::Error);
                let val = type_args.get(1).cloned().unwrap_or(Type::Error);
                return self.infer_map_method(&key, &val, method, args, span);
            }
        }

        // `Entry[K, V]` method dispatch — `or_insert`, `or_insert_with`,
        // `and_modify`. Produced by `Map.entry(k)`.
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty
        {
            if name == "Entry" {
                let key = type_args.first().cloned().unwrap_or(Type::Error);
                let val = type_args.get(1).cloned().unwrap_or(Type::Error);
                return self.infer_entry_method(&key, &val, method, args, span);
            }
        }

        // `SortedSet[T]` method dispatch. Named type but with dedicated
        // per-method typing (generic T threads through return types).
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty
        {
            if name == "SortedSet" {
                let element = type_args.first().cloned().unwrap_or(Type::Error);
                return self.infer_sorted_set_method(&element, method, args, span);
            }
            if name == "Set" {
                let element = type_args.first().cloned().unwrap_or(Type::Error);
                return self.infer_set_method(&element, method, args, span);
            }
        }

        // `Regex` method dispatch.
        if let Type::Named { name, .. } = &obj_ty {
            if name == "Regex" {
                return self.infer_regex_method(method, args, span);
            }
        }

        // `Client` / `Response` / `HttpError` method dispatch.
        if let Type::Named { name, .. } = &obj_ty {
            match name.as_str() {
                "Client" => return self.infer_http_client_method(method, args, span),
                "Response" => return self.infer_http_response_method(method, args, span),
                "HttpError" => return self.infer_http_error_method(method, args, span),
                _ => {}
            }
        }

        // `Sender[T]` / `Receiver[T]` method dispatch.
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty
        {
            if name == "Sender" || name == "Receiver" {
                let element = type_args.first().cloned().unwrap_or(Type::Error);
                let is_sender = name == "Sender";
                return self.infer_channel_method(is_sender, &element, method, args, span);
            }
        }

        // `Vec[T].sort_by` / `Vec[T].sorted_by` / `Vec[T].sort_by_key` /
        // `Vec[T].sorted_by_key` — closure-shape validation. Vec has no
        // stdlib impl block; without this intercept the call falls through
        // to the silent-no-method path below, leaving the closure arg
        // synth-typed with fresh metavars (no pushdown into params, no
        // check on the body's return type). A wrong-shape closure would
        // typecheck and runtime-panic in the interpreter's closure-honoring
        // sort paths. `sort_by` / `sort_by_key` mutate in place and return
        // Unit; `sorted_by` / `sorted_by_key` return a new Vec. Receiver
        // mutability is enforced at the binding layer (calling `.sort_by`
        // on a non-`mut` binding errors there), so no explicit mutability
        // gate is duplicated here.
        if matches!(
            method,
            "sort_by" | "sorted_by" | "sort_by_key" | "sorted_by_key"
        ) {
            let elem_for_vec: Option<Type> = match &obj_ty {
                Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = elem_for_vec {
                let is_key = method.ends_with("_key");
                let arg_label = if is_key { "key" } else { "comparator" };
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Vec.{}() expects 1 argument ({} closure), found {}",
                            method,
                            arg_label,
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else if is_key {
                    self.check_sort_key_closure(&elem, &args[0], method, span);
                } else {
                    self.check_sort_comparator(&elem, &args[0], method, span);
                }
                let mutates_in_place = method == "sort_by" || method == "sort_by_key";
                return if mutates_in_place {
                    Type::Unit
                } else {
                    Type::Named {
                        name: "Vec".to_string(),
                        args: vec![elem],
                    }
                };
            }
        }

        // Strip outer `ref` / `mut ref` to get the named receiver per
        // design.md § Method Resolution Step 1 (autoref candidates `T`,
        // `ref T`, `mut ref T` collapse to the same name lookup; the
        // receiver/self-mode compatibility check happens at the
        // param-binding layer). Shared-struct / Rc / Arc deref handled
        // here (sub-item 3a of the `Type::Shared` / `Type::Rc` /
        // `Type::Arc` representation work) — `Rc[Foo].method()` and
        // `let s: SharedStruct; s.method()` resolve through the inner
        // type's methods. Refinement-base candidate (1C) remains
        // deferred on `Type::Refinement` from phase-9.
        let receiver_for_lookup: Type = receiver_for_method_lookup(&obj_ty);
        let (type_name, type_args) = match &receiver_for_lookup {
            Type::Named { name, args } => (name.clone(), args.clone()),
            Type::TypeParam(name) if name == "Self" => {
                // Self-receiver dispatch (slice 3.5 of the method-resolution
                // CR — `phase-4-interpreter.md` item 8). `self.method()`
                // inside a trait default body resolves through the enclosing
                // trait's own methods + supertrait closure. Outside trait
                // bodies (`enclosing_trait == None`) the dispatcher returns
                // `Type::Error` to preserve the pre-existing silent
                // fallthrough — impl-method bodies bind `Self` via
                // `current_self_type`, a different mechanism.
                return self.dispatch_self_receiver_method(method, args, span);
            }
            Type::TypeParam(name) => {
                // Receiver-form generic call-site dispatch (slice 2 of the
                // method-resolution CR — see `phase-4-interpreter.md` item 8).
                // The complement to type-prefixed `T.method()` dispatch via
                // `try_dispatch_typeparam_assoc_fn` (`infer_call`): for
                // `t.method(args)` where `t: T` and `T: SomeTrait` declares
                // `method`, look up T's bounds in `enclosing_bounds`, find
                // the trait declaring `method`, and lower the trait method's
                // signature with `Self → Type::TypeParam(T)` substitution.
                // Multiple matching bounds → AmbiguousAssocFn (UFCS hint);
                // zero matches → NoMethodFound; exactly one → dispatch.
                //
                // `Self` is handled in the arm above (slice 3.5) — it
                // routes to `dispatch_self_receiver_method` which consults
                // the enclosing trait being defined, not just bounds.
                return self.dispatch_typeparam_receiver_method(name, method, args, span);
            }
            _ => {
                // For non-named types, just type-check args and return Error
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        };

        // Look up method on the receiver type with inherent-beats-trait
        // priority per design.md § Method Resolution Step 3, plus
        // conditional-impl filtering against the receiver's concrete
        // generic args (slice 1 of the method-resolution CR — see
        // `phase-4-interpreter.md`). All-candidates collection lets us
        // detect Step-4 ambiguity (slice 3): >1 surviving candidate at
        // the same priority tier (e.g. two trait impls when no inherent
        // matches) emits AmbiguousMethod and returns Type::Error.
        let candidates = self
            .env
            .find_methods_with_args(&type_name, &type_args, method);
        let method_sig: Option<FunctionSig> = if candidates.len() > 1 {
            // Render each candidate as `Trait.method(receiver)` (or
            // `Type.method(receiver)` for the rare inherent-vs-inherent
            // case). The signature display includes the receiver-then-args
            // tuple plus return type so the programmer can tell the
            // candidates apart at a glance.
            let candidate_lines: Vec<String> = candidates
                .iter()
                .map(|(imp, sig)| {
                    let dispatcher = imp
                        .trait_name
                        .clone()
                        .unwrap_or_else(|| imp.target_type.clone());
                    let params_display = std::iter::once(type_name.clone())
                        .chain(sig.params.iter().map(type_display))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "    `{}.{}({})` -> {}",
                        dispatcher,
                        method,
                        params_display,
                        type_display(&sig.return_type),
                    )
                })
                .collect();
            let receiver_display = if type_args.is_empty() {
                type_name.clone()
            } else {
                format!(
                    "{}[{}]",
                    type_name,
                    type_args
                        .iter()
                        .map(type_display)
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            };
            self.type_error(
                format!(
                    "ambiguous method '{}' on receiver of type '{}': \
                     multiple candidates apply. Use UFCS to disambiguate:\n{}",
                    method,
                    receiver_display,
                    candidate_lines.join("\n"),
                ),
                span.clone(),
                TypeErrorKind::AmbiguousMethod,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        } else {
            candidates.into_iter().next().map(|(_, sig)| sig.clone())
        };

        match method_sig {
            Some(sig) => {
                // Validate labels against method parameter names
                self.validate_labels(args, &sig.param_names, span);
                // Check argument count (excluding self)
                if args.len() != sig.params.len() {
                    self.type_error(
                        format!(
                            "method '{}' expects {} argument(s), found {}",
                            method,
                            sig.params.len(),
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return sig.return_type.clone();
                }
                // Reuse the round-10.1 closure-pushdown helper so generic
                // methods solve `T` from non-closure args before checking
                // closure args. `apply_call_site_marker` is `false`: per
                // design.md, the call-site `mut` marker rule applies only to
                // free-function calls, never to method calls.
                self.check_call_args_with_substitution(
                    args,
                    &sig.params,
                    &sig.return_type,
                    span,
                    /* apply_call_site_marker = */ false,
                )
            }
            None => {
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                // Tightening: error only for user-defined types whose impls
                // are exhaustively known. Built-in prelude types (`Option`,
                // `Result`, `Vec`, `Regex`, etc. — see `prelude::PRELUDE_TYPES`)
                // have a partially-implicit method surface (`.unwrap()`,
                // `.is_ok()`, regex methods that route through Type::Named
                // dispatch above but may not match every name) so they keep
                // the historical silent fall-through.
                let is_user_defined = (self.env.structs.contains_key(&type_name)
                    || self.env.enums.contains_key(&type_name))
                    && !crate::prelude::PRELUDE_TYPES.contains(&type_name.as_str());
                // Args-specialization tightening: even on prelude types, fire
                // NoMethodFound when the method exists on a *different*
                // args-specialization of this type-name (e.g.,
                // `Option[i32].is_lt()` when only `impl Option[Ordering]`
                // declares `is_lt`). Preserves the silent fall-through when
                // the method is genuinely absent (`Vec[i32].some_typo()`
                // stays silent) while surfacing the args-mismatch case that
                // would otherwise silently reach the interpreter and produce
                // a wrong answer through unrelated dispatch.
                let method_on_other_specialization =
                    self.env.impls.iter().any(|imp| {
                        imp.target_type == type_name && imp.methods.contains_key(method)
                    });
                if is_user_defined || method_on_other_specialization {
                    let candidates = self.env.collect_method_names(&type_name, &[]);
                    let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
                    let mut msg = format!("no method '{}' on type '{}'", method, type_name);
                    if let Some(suggestion) =
                        crate::edit_distance::suggest_similar(method, &candidate_refs)
                    {
                        msg.push_str(&format!(", did you mean '{}'?", suggestion));
                    }
                    self.type_error(msg, span.clone(), TypeErrorKind::NoMethodFound);
                }
                Type::Error
            }
        }
    }

    // ── Field Access ────────────────────────────────────────────
}
