//! Method-call typechecking dispatch.
//!
//! Houses the `infer_method_call` receiver-shape match and the trait-
//! dispatch arms it relies on: `find_trait_method`,
//! `dispatch_typeparam_receiver_method`, `dispatch_self_receiver_method`,
//! `try_dispatch_typeparam_assoc_fn`, `dispatch_trait_assoc_fn`, and the
//! `is_unresolvable_trait_assoc_fn` lookup used by call-position
//! diagnostics. This is the biggest sub-cluster of expression
//! inference (~1330 lines).
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::{SpanKey, SymbolKind};
use crate::token::Span;
use std::collections::HashMap;

use super::env::{FunctionSig, ImplInfo};
use super::inference::{resolve_type_var_top, substitute_type_params, unify_types};
use super::types::{
    clone_self_type_for, iterator_item_type_for, method_callee_type_name,
    receiver_for_method_lookup, type_display, ConstArg, IntSize, SubstValue, Type, UIntSize,
    VariantTypeInfo,
};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    // ── Method Calls ────────────────────────────────────────────

    /// True when `name` is unresolvable as a value (no local, function,
    /// constant, or builtin), but at least one visible trait declares it as
    /// an associated function. Mirrors the resolver's `is_trait_assoc_fn_name`
    /// suppression rule — used by `infer_call` to surface a "cannot infer"
    /// error in synthesis position rather than silently returning `Type::Error`.
    pub(super) fn is_unresolvable_trait_assoc_fn(&self, name: &str) -> bool {
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
    pub(super) fn find_trait_method<'p>(
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

    /// `impl Trait` slice 6 — `existential.method(args)` dispatch for
    /// both return-position existentials (slice 3, `tait_alias = None`)
    /// and TAIT-sourced existentials (`tait_alias = Some(alias)`).
    /// Looks up `method` on the existential's declared `trait_name`
    /// via [`Self::find_trait_method`]; on hit, dispatches through
    /// [`Self::dispatch_trait_assoc_fn`] with `Self → Type::TypeParam(trait_name)`
    /// — the same lowering used by `Type::TypeParam` receivers, which
    /// keeps the trait-surface call path uniform with slice 3's
    /// `type_satisfies_bound` story.
    ///
    /// On miss, the diagnostic depends on the existential's origin:
    /// - **TAIT-sourced** (`tait_alias = Some(alias)`): emit
    ///   `error[E_TAIT_NOT_IMPLEMENTED_YET]` naming the alias and the
    ///   missing method — the witness might declare the method but
    ///   resolving against the witness requires the P1 witness-inference
    ///   pipeline, so v1 routes through the trait surface only.
    /// - **Return-position** (`tait_alias = None`): emit the generic
    ///   `NoMethodFound` naming the trait — slice 3 caller-side opacity
    ///   already covers the rest of the diagnostic surface.
    fn dispatch_existential_receiver_method(
        &mut self,
        trait_name: &str,
        tait_alias: Option<&str>,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // Trait-surface lookup: the existential's only callable methods
        // are those declared on `trait_name`. `find_trait_method`
        // walks `program.items` for the trait def and returns the
        // matching method; receiver-form requires `self_param`.
        let trait_method = self.find_trait_method(trait_name, method).and_then(|m| {
            // Only methods (with self_param) are receiver-form
            // candidates; associated functions reach dispatch only
            // through type-prefixed `Trait.fn()`.
            m.self_param.as_ref()?;
            Some(m.clone())
        });
        if let Some(m) = trait_method {
            return self.dispatch_trait_assoc_fn(trait_name, &m, args, span);
        }
        // No trait-surface method — surface the focused diagnostic.
        for arg in args {
            self.infer_expr(&arg.value);
        }
        match tait_alias {
            Some(alias) => {
                self.type_error(
                    format!(
                        "error[E_TAIT_NOT_IMPLEMENTED_YET]: TAIT '{alias}' is recognized \
                         but the witness-inference pipeline lands in P1; method '{method}' \
                         is not declared on trait '{trait_name}', and dispatching against \
                         the alias's concrete witness requires the deferred TAIT machinery \
                         — cast through the trait surface for now"
                    ),
                    span.clone(),
                    TypeErrorKind::NoMethodFound,
                );
            }
            None => {
                self.type_error(
                    format!(
                        "no method '{method}' on `impl {trait_name}` value; only methods \
                         declared on trait '{trait_name}' are callable through the \
                         existential"
                    ),
                    span.clone(),
                    TypeErrorKind::NoMethodFound,
                );
            }
        }
        Type::Error
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

    pub(super) fn try_dispatch_typeparam_assoc_fn(
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
    pub(super) fn dispatch_trait_assoc_fn(
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

    pub(super) fn infer_method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // Fallible-allocation companions (phase-8-stdlib-floor item 2). A
        // `try_<base>` instance method on a builtin collection types
        // identically to its panicking `<base>` counterpart but returns
        // `Result[<base-ret>, AllocError]`. Recursing into the base method
        // reuses its argument validation + return-type synthesis verbatim;
        // `infer_expr(object)` is idempotent (the property the `spawn` intercept
        // below relies on) so the receiver double-inference is side-effect-free.
        // Gated on a builtin-collection receiver so a user type that happens to
        // define `try_push` / `try_clone` / … is never shadowed.
        if let Some(base) = crate::fallible_alloc::instance_companion_base(method) {
            if self.receiver_is_alloc_collection(object) {
                let base_ret = self.infer_method_call(object, base, args, span);
                if base_ret == Type::Error {
                    return Type::Error;
                }
                let result_ty = self.result_alloc_error_type(base_ret);
                self.record_expr_type(span, &result_ty);
                return result_ty;
            }
        }

        // Phase 6 line 170 slice 3a — cross-task-safe boundary check at
        // `tg.spawn(closure)` call sites. Fires before any other dispatch
        // so the outer-scope snapshot taken inside
        // `check_cross_task_safe_captures` reflects the scope at the
        // call site, before the closure body's typecheck pushes its
        // own params. Gated on `method == "spawn"` + receiver type
        // resolving to `TaskGroup` so the check only fires on the
        // intended call shape. `infer_expr` on the receiver is
        // idempotent for an already-typechecked identifier (the
        // expr_types entry is just re-asserted), so the pre-inference
        // here has no side-effect downstream of the dispatch fallthrough.
        if method == "spawn" && args.len() == 1 {
            if let ExprKind::Closure { .. } = &args[0].value.kind {
                let recv_ty = self.infer_expr(object);
                if let Type::Named { name, .. } = &recv_ty {
                    if name == "TaskGroup" {
                        self.check_cross_task_safe_captures(
                            &args[0].value,
                            span,
                            "TaskGroup.spawn",
                        );
                    }
                }
            }
        }

        // SIMD static constructors — `Vector[T, N].splat(x)` and
        // `Vector[T, N].from_array([..])` (design.md § Portable SIMD). The
        // receiver is the bare vector type-path (`Path { segments: ["Vector"],
        // generic_args: Some([T, N]) }`), not a value, so it must be
        // intercepted before the normal receiver-type inference below tries to
        // evaluate `Vector[T, N]` as a value and rejects it. Mirrors the
        // `Vector[T,N](...)` construction intercept in
        // `infer_explicit_generic_args_call`.
        if method == "splat"
            || method == "from_array"
            || method == "from_slice"
            || method == "load_masked"
            || method == "gather"
            || method == "cast_from"
        {
            if let ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } = &object.kind
            {
                if segments.len() == 1 && segments[0] == "Vector" {
                    return match method {
                        "splat" => self.infer_vector_splat(ga, args, span),
                        "from_array" => self.infer_vector_from_array(ga, args, span),
                        "load_masked" => self.infer_vector_load_masked(ga, args, span),
                        "gather" => self.infer_vector_gather(ga, args, span),
                        "cast_from" => self.infer_vector_cast_from(ga, args, span),
                        _ => self.infer_vector_from_slice(ga, args, span),
                    };
                }
            }
        }

        // Strict-provenance `ptr` module — `ptr.addr(p)`, `ptr.with_addr(p, a)`,
        // `ptr.from_exposed(a)`, etc. (design.md § Pointer Provenance, v60
        // item 20). Routes through the generic-aware dispatch path because
        // every entry is parameterised over `T`: the bare `infer_method_call`
        // arms below (the `env` arm) use a non-generic `check_assignable`
        // loop which would silently accept any argument shape against a
        // `*const T` slot — `(Type::TypeParam(_), _) => true` in
        // `types_compatible`. The `check_call_args_with_substitution_full`
        // path instantiates `T` to a fresh metavar so the outer `*const ?T`
        // shape unifies properly against the supplied argument's type.
        //
        // Skipped when a local binding shadows `ptr` — the prelude module
        // is registered with `SymbolKind::Module` but local-scope wins
        // by name resolution, mirroring the spec's prelude-shadow rule.
        if let ExprKind::Identifier(mod_name) = &object.kind {
            if mod_name == "ptr"
                && self.local_scope.lookup("ptr").is_none()
                && (method == "const" || method == "mut")
            {
                return self.infer_ptr_construction(method, args, span);
            }
            if mod_name == "ptr" && self.local_scope.lookup("ptr").is_none() {
                let dotted = format!("ptr.{}", method);
                if let Some(sig) = self.env.functions.get(&dotted).cloned() {
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
                    return self.check_call_args_with_substitution_full(
                        args,
                        &sig.params,
                        &sig.return_type,
                        span,
                        false,
                        None,
                        Some(&sig.generic_params),
                        sig.where_clause.as_ref(),
                        span,
                    );
                }
            }
        }

        // Lowercase stdlib module aliases: `env.args()`, `clock.now()`,
        // `stdout.println(s)`, `fs.write(p, c)`, … These use lowercase module
        // names (design.md § I/O), distinct from the capitalized resource
        // names used by the effect system. Map each lowercase module to its
        // capitalized resource equivalent so the shared method signatures are
        // found — first in the baked-impl table (`env.impls`, where the
        // slice-2 migration moved the I/O resource methods), then in
        // `env.functions` for any future entries that can't be expressed as
        // impl methods. Resolving through the baked impl is what gives the
        // call its exact return type (e.g. `Result[String, IoError]`), which
        // flows into `pattern_binding_types` so a `match` arm binds the Ok
        // payload at the right width — mirrors the resolver `push`, the
        // interpreter alias map, and codegen's `ambient_resource_for_alias`.
        if let ExprKind::Identifier(mod_name) = &object.kind {
            let resource_name = match mod_name.as_str() {
                "env" => Some("Env"),
                "clock" => Some("Clock"),
                "rand" => Some("RandomSource"),
                "stdin" => Some("Stdin"),
                "stdout" => Some("Stdout"),
                "stderr" => Some("Stderr"),
                "fs" => Some("FileSystem"),
                _ => None,
            }
            .filter(|_| self.local_scope.lookup(mod_name).is_none());
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
                // `f64.parse(s: String) -> Option[f64]`. Unlike the integer
                // parses (which ride the untyped-primitive-assoc passthrough —
                // their payload is i64, so the Option element defaulting to i64
                // happens to be correct), float parse MUST be typed: the some-
                // payload holds the f64 bit pattern, and without an
                // `Option[f64]` element type the match binding extracts those
                // bits as an i64 and prints garbage. Phase-8 floor for the
                // self-hosting lexer's float literals (f32.parse is deferred —
                // its narrower payload width needs its own runtime path).
                if method == "parse" && type_name == "f64" {
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    let f64_ty = self
                        .primitive_type("f64")
                        .expect("f64 is a known primitive");
                    return Type::Named {
                        name: "Option".to_string(),
                        args: vec![f64_ty],
                    };
                }
                // Integer `<int>.parse(s) -> Option[<int>]` and
                // `<int>.from_str_radix(s, radix) -> Option[<int>]`. These rode
                // the untyped-primitive-assoc passthrough (payload defaulting to
                // i64 — value-correct), but an UNANNOTATED `let o = i64.parse(s)`
                // then left the match-bound `Some(v)` without a concrete element
                // type, so `v.to_string()` / further method dispatch on `v` fell
                // through in codegen (the dispatch key is the typechecker-recorded
                // receiver type — blocker #11). Typing the result explicitly
                // (mirrors the `f64.parse` arm above) records `Option[<int>]` so
                // the binding's element type reaches dispatch; the annotated form
                // (`let o: Option[i64] = …`) already worked.
                if (method == "parse" || method == "from_str_radix")
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    if let Some(int_ty) = self.primitive_type(type_name.as_str()) {
                        return Type::Named {
                            name: "Option".to_string(),
                            args: vec![int_ty],
                        };
                    }
                }
                // `char.try_from(n: <int>) -> Result[char, i64]` — fallible
                // codepoint→char conversion (blocker #10; the
                // `E_INT_AS_CHAR` rejection of `n as char` points here). Not
                // every integer is a valid Unicode scalar (the surrogate range
                // `0xD800..=0xDFFF` and values above `0x10FFFF` are rejected),
                // so the result is a `Result`; the `Err` payload is the
                // offending codepoint value (`i64`) — no dedicated error enum
                // needed (the error type is unspecified at the language level).
                if method == "try_from" && type_name == "char" {
                    if args.len() != 1 {
                        self.type_error(
                            format!("char.try_from expects 1 argument, got {}", args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        return Type::Error;
                    }
                    let arg_ty = self.infer_expr(&args[0].value);
                    if !matches!(arg_ty, Type::Int(_) | Type::UInt(_) | Type::Error) {
                        self.type_error(
                            format!(
                                "char.try_from expects an integer codepoint, got `{}`",
                                type_display(&arg_ty)
                            ),
                            span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                    let char_ty = self
                        .primitive_type("char")
                        .expect("char is a known primitive");
                    let i64_ty = self
                        .primitive_type("i64")
                        .expect("i64 is a known primitive");
                    return Type::Named {
                        name: "Result".to_string(),
                        args: vec![char_ty, i64_ty],
                    };
                }
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
                        // Shape args are ignored for dispatch (Dim/Shape
                        // kind system lands in Phase 11 Q1).
                        GenericArg::Shape(_) => None,
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

        let mut obj_ty = self.infer_expr(object);
        if obj_ty == Type::Error {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        }

        // Record the builtin-collection receiver name keyed by the method-call
        // span for the panicking-alloc rejection pass (phase-8-stdlib-floor
        // item 4). Only populated under `panic_on_alloc_failure = false`; the
        // pass cannot recover the receiver type from `expr_types` because the
        // method-call span equals the receiver's span (which then holds the
        // method's return type).
        if !self.profile_config.panics_on_alloc_failure() {
            if let Some(coll) = super::alloc_rejection::builtin_collection_name(&obj_ty) {
                self.method_receiver_collections
                    .insert(SpanKey::from_span(span), coll.to_string());
            }
        }

        // Refinement base-deref (§1C — phase-9 step 2). A method call on a
        // refinement-typed receiver resolves against the refinement's own
        // inherent / trait impls *first* (design.md § Method Resolution:
        // "inherent and trait methods on the refined type win over methods
        // on the base"), then falls through to the base type's methods.
        //
        // The decision is per method name: if the refinement declares no
        // method of this name, strip to the base so every downstream
        // dispatch path — the String / Vec / Option special cases below
        // *and* the generic impl-table search — sees the base receiver.
        // When the refinement *does* declare the method itself, keep the
        // refined receiver; the generic search keys impls by the
        // refinement's nominal name (`impl_table_key` / the
        // `Type::Refinement` arm of the receiver-name match below).
        if let Type::Refinement { base, name } = &obj_ty {
            let has_own_method = self
                .env
                .impls
                .iter()
                .any(|imp| imp.target_type == *name && imp.methods.contains_key(method));
            if !has_own_method {
                let base = (**base).clone();
                obj_ty = base;
            }
        }

        // Cancel-narrowing side-table: record `Type.method` for this call
        // site so codegen can elide the par-branch cancel check when the
        // resolved callee is provably non-effectful. Populated here once so
        // it covers every dispatch path below (Slice, String, Map, named
        // types, etc.) — the parser sets `MethodCall.span == receiver.span`,
        // so we use `method_callee_types` rather than `expr_types` (which
        // would race with the return-type insertion at the same key).
        //
        // Chained-call span collision: because `MethodCall.span ==
        // receiver.span`, in `recv.inner().outer()` the inner and outer
        // calls share one span key, and a later (outer) insert clobbers the
        // inner one. The unwrap-family accessors (`unwrap` / `expect` /
        // `is_*`) are the common outer link — and they are pure built-ins,
        // dispatched by name for effects (`__builtin_unwrap`) and keyed
        // separately for codegen (`method_unwrap_inner_types`), so they
        // never need a `method_callee_types` entry. Skipping them here keeps
        // the inner call's precise key intact, so an effectful inner call
        // (e.g. `listener.accept().unwrap()`) still resolves to
        // `TcpListener.accept` for effect propagation. Skipping a pure outer
        // can never lose an effectful callee, so this is sound for every
        // consumer (effects / cancel-narrowing / must-use / raii / unsafe).
        // Only the BUILTIN Option/Result unwrap-family is skipped — gated on
        // the receiver type being `Option` / `Result`, exactly as the
        // builtin handler below (line ~962) is. A user method that happens to
        // be named `unwrap` / `is_ok` / … on some other receiver (e.g.
        // `impl Inner { fn unwrap(self) -> i64 }`) is a real call that MUST
        // record its `Type.method` key — the use-classifier reads it to see
        // the owned-`self` receiver mode and tag the projection root as
        // Consume (`use_classifier::tests::owned_self_on_field_consumes_root`).
        let callee_type_name = method_callee_type_name(&obj_ty);
        let is_builtin_unwrap_family =
            matches!(
                method,
                "unwrap" | "expect" | "is_some" | "is_none" | "is_ok" | "is_err" | "unwrap_or"
            ) && matches!(callee_type_name.as_deref(), Some("Option") | Some("Result"));
        if !is_builtin_unwrap_family {
            if let Some(type_name) = callee_type_name {
                // Phase-8 line 96 — instance-method use-site stability lint.
                // Fires for every named-receiver method call (the hardcoded
                // HTTP / TCP / TLS / WS arms below included, since they share
                // this central resolution point). The skipped case is only the
                // builtin Option/Result unwrap-family, which is never
                // `#[unstable]` / `#[deprecated]`.
                self.check_method_stability(&type_name, method, span);
                self.method_callee_types.insert(
                    SpanKey::from_span(span),
                    format!("{}.{}", type_name, method),
                );
            }
        }

        // Option/Result unwrap-family side-table: record the inner `T` /
        // success-`T` so codegen's `compile_method_call` arm for
        // `unwrap`/`expect`/`is_*`/`unwrap_or` knows the LLVM shape of the
        // value to reconstitute from the Option/Result payload words. Sibling
        // to `method_callee_types`; mirrors the per-MethodCall-span keying so
        // the lookup at codegen time is O(1). The `is_*` arms record T for
        // uniformity even though codegen only consumes the tag.
        if matches!(
            method,
            "unwrap" | "expect" | "is_some" | "is_none" | "is_ok" | "is_err" | "unwrap_or"
        ) {
            // `unwrap_or(default)` eagerly evaluates its fallback — infer it
            // here (where `args` is still the method-call arg list, before the
            // `Type::Named { args }` binding below shadows it) so the default's
            // sub-expressions are typed for codegen. Kept permissive (no hard
            // unify with `T`) to avoid a 722-style over-strict rejection of a
            // coercible default; codegen width-coerces an int default to `T`.
            if method == "unwrap_or" {
                if let Some(a) = args.first() {
                    let _ = self.infer_expr(&a.value);
                }
            }
            let receiver_named = match &obj_ty {
                Type::Named { .. } => Some(&obj_ty),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { .. } => Some(inner.as_ref()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(Type::Named { name, args }) = receiver_named {
                let inner_ty = match (name.as_str(), args.first()) {
                    ("Option", Some(t)) => Some(t.clone()),
                    ("Result", Some(t)) => Some(t.clone()),
                    _ => None,
                };
                if let Some(inner_ty) = inner_ty {
                    let resolved = resolve_type_var_top(&inner_ty, &self.env.substitutions);
                    let te = Self::type_to_type_expr(&resolved);
                    self.method_unwrap_inner_types
                        .insert(SpanKey::from_span(span), te);
                    // Surface a proper return type so the binding gets the
                    // right Type rather than falling through to the
                    // prelude-permissive `Type::Error`. Without this,
                    // `let x = m.get(k).unwrap()` binds `x: Type::Error`,
                    // which breaks downstream `x.field` / `x.method(...)`
                    // resolution (field-access dispatch keys off
                    // `var_type_names` populated from `pattern_binding_types`).
                    return match method {
                        "unwrap" | "expect" | "unwrap_or" => resolved,
                        "is_some" | "is_none" | "is_ok" | "is_err" => Type::Bool,
                        _ => unreachable!(),
                    };
                }
            }
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

        // `Array[T, N].as_ptr()` / `.as_mut_ptr()` — raw element-0 pointer
        // producers (the language's FFI handoff; mirrors `CStr.as_ptr`).
        // `as_ptr -> *const T`, `as_mut_ptr -> *mut T`. The codegen handler
        // in `compile_method_call` GEPs to element 0 of the array storage.
        // Without a precise arm here the call falls through to the
        // permissive array-method path and binds `Type::Error`, losing the
        // pointer type for downstream FFI / deref. Handles owned arrays and
        // their `ref` / `mut ref` borrows.
        if (method == "as_ptr" || method == "as_mut_ptr") && args.is_empty() {
            let elem = match &obj_ty {
                Type::Array { element, .. } => Some(*element.clone()),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Array { element, .. } => Some(*element.clone()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(el) = elem {
                return Type::Pointer {
                    is_mut: method == "as_mut_ptr",
                    inner: Box::new(el),
                };
            }
        }

        // `Slice[T]` and `mut Slice[T]` method dispatch. These types are not
        // `Type::Named` so they fall through the generic branch below; handle
        // them here before the named-type extraction.
        if let Type::Slice { element, mutable } = &obj_ty.clone() {
            return self.infer_slice_method(element, *mutable, method, args, span);
        }

        // `Vector[T, N]` instance-method dispatch (design.md § Portable SIMD).
        // Not a `Type::Named`, so handle before the named-type extraction.
        if let Type::Vector { element, lanes } = &obj_ty.clone() {
            // Record the receiver vector type for the SIMD scalarization
            // analysis before delegating — the method-call node is about to
            // overwrite this span in `expr_types` with the method's *result*
            // type (scalar for reductions), erasing the receiver's `(T, N)`.
            // See `TypeCheckResult::vector_method_receivers`.
            if let Some(n) = lanes.as_usize() {
                self.vector_method_receivers
                    .insert(SpanKey::from_span(span), ((**element).clone(), n));
            }
            return self.infer_vector_method(element, lanes, method, args, span);
        }

        // Tensor shape-transform family — `iter_axis` / `reshape` /
        // `permute` / `slice` / `squeeze` (phase-11). Their result types
        // depend on the receiver's shape and the arguments' syntactic
        // form, so they aren't expressible in the baked stdlib signatures
        // and are computed before the impl-table search; `shape()` /
        // `rank()` keep flowing through normal impl dispatch. Typing
        // rules in `src/typechecker/expr_method_tensor.rs`.
        if matches!(
            method,
            "iter_axis" | "reshape" | "permute" | "slice" | "squeeze"
        ) {
            let tensor_args = match &obj_ty {
                Type::Named { name, args } if name == "Tensor" => Some(args),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Tensor" => Some(args),
                    _ => None,
                },
                _ => None,
            };
            if let Some(tensor_args) = tensor_args.cloned() {
                return self.infer_tensor_shape_method(method, &tensor_args, args, span);
            }
        }

        // Tensor reductions: `sum` / `mean` / `prod` / `min` / `max` collapse
        // the whole tensor to a scalar; `sum_axis(n)` / `mean_axis(n)` collapse
        // one axis, yielding a tensor of rank-1 lower. `mean`/`mean_axis`
        // always yield `f64`; the rest preserve the element type. Like the
        // shape family these can't be expressed in the baked signatures, so
        // they intercept before impl dispatch. Typing in
        // `src/typechecker/expr_method_tensor.rs`.
        if matches!(
            method,
            "sum" | "mean" | "prod" | "min" | "max" | "sum_axis" | "mean_axis"
        ) {
            let tensor_args = match &obj_ty {
                Type::Named { name, args } if name == "Tensor" => Some(args),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Tensor" => Some(args),
                    _ => None,
                },
                _ => None,
            };
            if let Some(tensor_args) = tensor_args.cloned() {
                return self.infer_tensor_reduce(method, &tensor_args, args, span);
            }
        }

        // Tensor broadcasting — `broadcast_add` / `broadcast_sub` /
        // `broadcast_mul` / `broadcast_div` apply a binary op with NumPy-style
        // shape broadcasting (size-1 dims expand; shapes align from the
        // right). The result shape depends on *both* operand shapes, so it's
        // computed here before impl dispatch, like the shape/reduce families.
        // Typing in `src/typechecker/expr_method_tensor.rs`.
        if matches!(
            method,
            "broadcast_add" | "broadcast_sub" | "broadcast_mul" | "broadcast_div"
        ) {
            let tensor_args = match &obj_ty {
                Type::Named { name, args } if name == "Tensor" => Some(args),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Tensor" => Some(args),
                    _ => None,
                },
                _ => None,
            };
            if let Some(tensor_args) = tensor_args.cloned() {
                return self.infer_tensor_broadcast(method, &tensor_args, args, span);
            }
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

        // `Atomic[T].compare_exchange(old, new, success, failure) -> Result[T, T]`
        // (deferred.md § Atomic Operations, line 311). Special-cased because its
        // Result-shaped return must be visible to the typechecker so the caller
        // can `match` / `.is_ok()` on the outcome. The other atomic methods
        // (`load` / `store` / `fetch_*` / `swap`) are codegen-only and fall
        // through to the silent `Type::Error` arm below — their inner-type
        // return isn't modeled here, which is harmless because `Type::Error`
        // is universally assignable. `compare_exchange` can't ride that path:
        // a `Result`-typed scrutinee is needed for exhaustive matching.
        // Returns `Ok(prev)` on a successful swap, `Err(actual)` otherwise —
        // both payloads are `T`, hence `Result[T, T]`.
        if method == "compare_exchange" {
            let inner = match &obj_ty {
                Type::Named { name, args } if name == "Atomic" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(b) | Type::MutRef(b) => match b.as_ref() {
                    Type::Named { name, args } if name == "Atomic" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(inner) = inner {
                if args.len() != 4 {
                    self.type_error(
                        format!(
                            "Atomic.compare_exchange expects (old, new, success: MemoryOrdering, \
                             failure: MemoryOrdering) — 4 arguments, found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    // old / new must be assignable to the atomic's inner type T;
                    // the two ordering args are inferred for recording (their
                    // `MemoryOrdering.X` shape is validated at codegen).
                    let old_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(&inner, &old_ty, args[0].value.span.clone());
                    let new_ty = self.infer_expr(&args[1].value);
                    self.check_assignable(&inner, &new_ty, args[1].value.span.clone());
                    self.infer_expr(&args[2].value);
                    self.infer_expr(&args[3].value);
                }
                let result_ty = Type::Named {
                    name: "Result".to_string(),
                    args: vec![inner.clone(), inner],
                };
                self.record_expr_type(span, &result_ty);
                return result_ty;
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

        // `Vec[T].extend_from_slice(other)` — `other` may be
        // `Slice[T]`, `Vec[T]`, or `Array[T, N]`. We unify the
        // receiver's element type with the source's element type so
        // that an unsolved typevar on the receiver (e.g. `let mut v =
        // Vec.new(); v.extend_from_slice(other);`) gets pinned to the
        // source's element type, mirroring `push`'s behavior.
        if method == "extend_from_slice" && args.len() == 1 {
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
                // Peel one layer of Ref/MutRef from the source — the
                // arg may arrive as `ref Slice[T]` / `ref Vec[T]` /
                // `mut Slice[T]` depending on the call site.
                let arg_inner = match &arg_ty {
                    Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                    other => other.clone(),
                };
                let src_elem = match &arg_inner {
                    Type::Named { name, args }
                        if (name == "Slice" || name == "Vec") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    Type::Array { element, .. } => Some((**element).clone()),
                    _ => None,
                };
                if let Some(src) = src_elem {
                    unify_types(
                        &elem,
                        &src,
                        &mut self.env.substitutions,
                        &mut self.env.const_substitutions,
                    );
                    let resolved_elem = resolve_type_var_top(&elem, &self.env.substitutions);
                    let resolved_src = resolve_type_var_top(&src, &self.env.substitutions);
                    self.check_assignable(
                        &resolved_elem,
                        &resolved_src,
                        args[0].value.span.clone(),
                    );
                    return Type::Unit;
                }
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

        // `Vec[T].remove(idx: i64) -> T` — remove the element at `idx`,
        // shift the tail down by one, return the removed value. v1
        // matches Rust's contract: idx out-of-bounds is UB (no bounds
        // check, no graceful Option). Callers ensure idx < len (the
        // backend TODO API kata's DELETE handler at
        // `kara-katas/backend/todo-api/main.kara` finds the index via
        // `find_index_by_id` first, then removes — the index is
        // known-good at the call). Mirrors the pop_front shape but
        // at an arbitrary index instead of 0.
        if method == "remove" && args.len() == 1 {
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
                self.check_assignable(
                    &Type::Int(IntSize::I64),
                    &arg_ty,
                    args[0].value.span.clone(),
                );
                return resolve_type_var_top(&elem, &self.env.substitutions);
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
            // Bare `Slice[T]` receivers dispatch earlier via
            // `infer_slice_method`; this arm covers `Vec[T]` and
            // `ref`/`mut ref` of Vec/Slice.
            // Accepts `Vec[T]` and `Slice[T]` (and `ref`/`mut ref` of either),
            // returning `T` by value — sound for the Copy element types hot
            // scanners use (i64/u8). The `Slice.get_unchecked` escape mirrors
            // the landed `Vec.get_unchecked` so a `Slice[T]` / `mut Slice[T]`
            // param can skip the bounds check the source-level dominator pass
            // can't reach (e.g. KMP's `needle[j]`, where `j` rewinds via the
            // LPS table — provably in-range, not compiler-provable). See
            // phase-7-codegen.md § BCE table-range tier.
            // `Slice[T]` reaches here as either `Type::Slice { element }` (slice
            // expressions / coercions) or `Type::Named { name: "Slice" }`
            // (declared params) — match both, plus `Vec[T]`, through one
            // optional layer of `ref`/`mut ref`.
            fn get_unchecked_elem(t: &Type) -> Option<Type> {
                match t {
                    Type::Named { name, args }
                        if (name == "Vec" || name == "Slice") && args.len() == 1 =>
                    {
                        Some(args[0].clone())
                    }
                    Type::Slice { element, .. } => Some((**element).clone()),
                    _ => None,
                }
            }
            let element_ty = match &obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => get_unchecked_elem(inner.as_ref()),
                other => get_unchecked_elem(other),
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
        //
        // Deref `ref T` / `mut ref T` once when checking the receiver
        // shape against stdlib named types (Map / Set / Entry / Sender /
        // Receiver / Regex / Client / Response / HttpError / String /
        // SortedSet). Without this, `mut ref Map[K,V].get(k)` lands in
        // the impl-block fallback path (which uses
        // `receiver_for_method_lookup` and so does deref), but the
        // map-specific signature — which threads the V into
        // `Option[V]` — is skipped, surfacing the get's return type as
        // `Type::Error`. The subsequent pattern-binding registration
        // misses `pattern_binding_types[Some(x)] = "Node"` and codegen
        // can't reconstitute the shared-struct payload from the i64
        // word, leaving the match-arm value as `i64` while the function
        // return type is `ptr` → LLVM verifier error.
        //
        // Symmetric to the existing deref in `unwrap`/`is_some`/etc.
        // above; the deref-on-named-receiver pattern is what makes a
        // `mut ref Vec[T]` parameter's `.push(x)` typecheck through
        // the existing impl-block-method path (Vec lives in the
        // impl-block surface, not in a stdlib_seq dispatcher) — Map /
        // Set / Entry are the holdouts.
        let obj_ty_for_named = match &obj_ty {
            Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
            _ => obj_ty.clone(),
        };

        // String method dispatch keeps the un-derefed `obj_ty` check
        // intentionally: `ref String` / `mut ref String` are intended
        // to flow through the impl-block-method path (which calls
        // `receiver_for_method_lookup` to deref and then resolves
        // `len` / `contains` / `is_empty` / etc.). `infer_str_method`
        // is the narrow stdlib_seq surface for the small set of
        // String methods that don't live in an impl block (`sorted`,
        // `sorted_by`, `chars`) — routing a derefed `ref String`
        // here would silently fail `len` lookup on those tests.
        if obj_ty == Type::Str {
            return self.infer_str_method(method, args, span);
        }

        // `to_string()` on a Display-able collection (`Vec`/`VecDeque`/`Map`/
        // `Set`) → `String`. Must precede the per-collection method dispatch
        // below: those (`infer_map_method` / `infer_set_method`) return
        // unconditionally, so an unrecognized `to_string` would surface as
        // `NoMethodFound` rather than falling through to the Display-method
        // intercept. Codegen renders via `try_compile_collection_display`.
        if method == "to_string" && args.is_empty() {
            if let Type::Named { name, .. } = &obj_ty_for_named {
                if matches!(name.as_str(), "Vec" | "VecDeque" | "Map" | "Set")
                    && self.type_supports_display(&obj_ty_for_named)
                {
                    return Type::Str;
                }
            }
        }

        // `Map[K, V]` method dispatch. K and V thread through return types.
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty_for_named
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
        } = &obj_ty_for_named
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
        } = &obj_ty_for_named
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
        if let Type::Named { name, .. } = &obj_ty_for_named {
            if name == "Regex" {
                return self.infer_regex_method(method, args, span);
            }
        }

        // `CStr` method dispatch — the `c"..."` literal types as `ref CStr`
        // (see `infer_expr_inner`'s CStringLit arm), so the deref'd
        // named-receiver shape lands here. `as_ptr` / `len` / `is_empty` /
        // `as_bytes` per design.md § C-String Literals. The
        // `method_callee_types` insert mirrors the HTTP arm below: CStr
        // dispatches through a hardcoded arm (no impl block), and codegen's
        // `compile_method_call` keys its CStr routing off the recorded
        // `CStr.<method>` — without it, dispatch falls through to the
        // user-impl-block lookup, which errors.
        if let Type::Named { name, .. } = &obj_ty_for_named {
            if name == "CStr" {
                self.method_callee_types
                    .insert(SpanKey::from_span(span), format!("CStr.{}", method));
                return self.infer_cstr_method(method, args, span);
            }
        }

        // `Client` / `Response` / `HttpError` / `RequestBuilder` method dispatch.
        if let Type::Named { name, .. } = &obj_ty_for_named {
            if matches!(
                name.as_str(),
                "Client" | "Response" | "HttpError" | "RequestBuilder"
            ) {
                // Record the precise `Type.method` callee for this call site.
                // These HTTP types dispatch through a hardcoded arm (not the
                // resolved-method path), so without this insert the effect
                // checker can't reach the `sends(Network)`/`receives(Network)`
                // seeds for `Client.get` / `Client.post` / `RequestBuilder.send`
                // — the call site would resolve to no precise key and the
                // name-only heuristics can't distinguish `client.get()` from
                // `map.get()`. Mirrors the resolved-method insert above.
                self.method_callee_types
                    .insert(SpanKey::from_span(span), format!("{}.{}", name, method));
            }
            match name.as_str() {
                "Client" => return self.infer_http_client_method(method, args, span),
                "Response" => return self.infer_http_response_method(method, args, span),
                "HttpError" => return self.infer_http_error_method(method, args, span),
                "RequestBuilder" => {
                    return self.infer_http_request_builder_method(method, args, span)
                }
                _ => {}
            }
        }

        // `Sender[T]` / `Receiver[T]` method dispatch.
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty_for_named
        {
            if name == "Sender" || name == "Receiver" {
                let element = type_args.first().cloned().unwrap_or(Type::Error);
                let is_sender = name == "Sender";
                return self.infer_channel_method(is_sender, &element, method, args, span);
            }
        }

        // `BoundedChannel[T]` method dispatch — `send` / `recv`. Intercepted
        // before the generic-impl resolution so the concrete element `T` is
        // taken from the receiver's type args (the `impl[T] Foo[T] { fn m()
        // -> T }` return-T binding gap). `new` is an associated call typed by
        // the stdlib signature; other methods fall through to the normal
        // (error) path.
        if let Type::Named {
            name,
            args: type_args,
        } = &obj_ty_for_named
        {
            if name == "BoundedChannel" && matches!(method, "send" | "recv") {
                let element = type_args.first().cloned().unwrap_or(Type::Error);
                return self.infer_bounded_channel_method(&element, method, args, span);
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

        // `Vec[T]` / `VecDeque[T]` read-accessor + in-place-mutator surface
        // (`len`, `is_empty`, `get`, `first`, `last`, `contains`,
        // `binary_search`, `split_at`, `chunks`, `windows`, `sort`,
        // `reverse`, `sorted`, `fill`, `swap`). Vec has no stdlib impl block,
        // so without this intercept these methods fell through to the
        // bottom-of-function `Type::Error` silent-prelude path — and for the
        // value-returning accessors (`len` etc.) that poison `Error` is
        // universally assignable, so `Stdout.println(v.len())` against a
        // `String` param, `let s: String = v.len()`, and friends typechecked
        // clean (the reported soundness hole). Routed here so `Vec` types
        // identically to `Slice`. `infer_vec_method` returns `None` for any
        // method it doesn't own, leaving the generic impl-search / prelude
        // fall-through below untouched (preserving user trait impls on Vec
        // and the typo-stays-silent prelude behaviour).
        let vec_elem_for_dispatch: Option<Type> = match &obj_ty {
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
        if let Some(elem) = vec_elem_for_dispatch {
            if let Some(ty) = self.infer_vec_method(&elem, method, args, span) {
                return ty;
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
        // type's methods. A `Type::Refinement` receiver only reaches here
        // when the refinement declares this method itself (the base-deref
        // above already stripped the no-own-method case); it keeps its
        // nominal name so the generic search finds the refinement's own
        // impl (phase-9 step 2, §1C).
        let receiver_for_lookup: Type = receiver_for_method_lookup(&obj_ty);
        // Distinct-type `.raw()` unwrap + no-deref rule (design.md § Distinct
        // Types). A distinct type flows as a nominal `Type::Named { name }`;
        // its built-in `.raw()` returns the underlying base value (recovered
        // from `env.distinct_bases`). Every *other* method resolves only
        // through inherent impls on the distinct type itself — distinct types
        // do not deref to their base (method-resolution rule 5), so a base
        // method like `i64.abs()` is not callable on a `UserId`. Non-`raw`
        // methods fall through to the generic impl search below; if none
        // matches, the bottom-of-function `NoMethodFound` fires (distinct
        // names are folded into `is_user_defined` there).
        if let Type::Named { name, .. } = &receiver_for_lookup {
            if let Some(base) = self.env.distinct_bases.get(name).cloned() {
                if method == "raw" {
                    if !args.is_empty() {
                        self.type_error(
                            format!("'.raw()' takes no arguments, found {}", args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                    }
                    return base;
                }
            }
        }
        // Opaque foreign types have no methods by definition — impl blocks
        // on them are rejected at `E_OPAQUE_TYPE_NO_INHERENT_OR_TRAIT_IMPLS`,
        // so the generic "method not found" diagnostic that would otherwise
        // fire from the fallthrough at the bottom of this function is
        // technically true but misleading. Emit the focused
        // `E_OPAQUE_TYPE_NO_METHODS` instead so the programmer is steered
        // toward the wrapper-type / free-function pattern.
        if let Type::Named { name, .. } = &receiver_for_lookup {
            if self.env.opaque_foreign_types.contains(name) {
                self.type_error(
                    format!(
                        "error[E_OPAQUE_TYPE_NO_METHODS]: opaque foreign type \
                         '{name}' has no methods — impl blocks on opaque types \
                         are rejected, so no '.{method}(…)' or any other method \
                         call resolves through '{name}'. Use the wrapper-type \
                         pattern (`distinct type Wrapper = *mut {name}; impl Wrapper {{ … }}`) \
                         or call a free function from the `unsafe extern \"C\" {{ … }}` \
                         block that takes `ref {name}` / `mut ref {name}`."
                    ),
                    span.clone(),
                    TypeErrorKind::NoMethodFound,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        }
        // Built-in `abs` on signed-integer and float primitives — `x.abs() ->
        // Self`. Handled here as a dedicated value-receiver method rather than
        // through the registered builtin-impl table: those `Neg`/`Ord` impls
        // model the *type-receiver* / operator-lowering form (`self` in the
        // params list, e.g. `i64.cmp(a, b)`), whose arity is incompatible with
        // the value-receiver `x.abs()` shape. Restricted to `Int` (signed) and
        // `Float`; unsigned `abs` is rejected (no `abs` on `u*`, matching
        // Rust), falling through to the `NoMethodFound` arm below. Backends:
        // interpreter `method_call.rs` (`checked_abs`, traps on `iN::MIN`),
        // codegen `method_call.rs` (`select(x<0, trapping(-x), x)`).
        if method == "abs"
            && args.is_empty()
            && matches!(&receiver_for_lookup, Type::Int(_) | Type::Float(_))
        {
            return receiver_for_lookup.clone();
        }
        // Float→int conversion families (phase-8 § "Saturating float→int",
        // slice 2): `f.{saturating,wrapping,checked,trunc}_to_<intN>()` on
        // `f32`/`f64`. `checked_*` returns `Option[intN]` (None on
        // NaN/out-of-range); the others return `intN`. `trunc_*` additionally
        // carries `panics` (seeded in effectchecker). Method-name → family +
        // target shared with the interpreter / effectchecker via
        // `crate::numeric_conv`. Backends: interpreter `method_call.rs`
        // computes via `numeric_conv::convert_float_to_int`; the bit-exact
        // `fptosi.sat`/`fptoui.sat` codegen is slice 4 (interpreter-only until
        // then — `karac build` errors loudly rather than miscompiling).
        if args.is_empty() && matches!(&receiver_for_lookup, Type::Float(_)) {
            if let Some((family, target, _, _)) = crate::numeric_conv::parse_float_to_int(method) {
                if let Some(int_ty) = self.primitive_type(target) {
                    return match family {
                        crate::numeric_conv::FloatToIntFamily::Checked => Type::Named {
                            name: "Option".to_string(),
                            args: vec![int_ty],
                        },
                        _ => int_ty,
                    };
                }
            }
        }
        // Int→float conversions (same slice): `n.to_f32()` / `n.to_f64()` on
        // every signed/unsigned integer. The implicit-widening cases already
        // work without `as`; these method forms ship for code-style
        // consistency with the float→int families above. Effect-free.
        if args.is_empty() && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if method == "to_f32" {
                if let Some(t) = self.primitive_type("f32") {
                    return t;
                }
            }
            if method == "to_f64" {
                if let Some(t) = self.primitive_type("f64") {
                    return t;
                }
            }
        }
        // ASCII byte-classification predicates on integer scalars (notably the
        // `u8` bytes yielded by `String.bytes()`): `b.is_ascii_digit()`,
        // `b.is_ascii_alphabetic()`, `b.is_ascii_hexdigit()` → `bool`. Phase-8
        // floor for the self-hosting lexer's byte-indexed scan
        // (phase-12-self-hosting.md); mirror Rust's `u8::is_ascii_*`. Effect-free
        // value-receiver methods (codegen lowers to inline range checks; no
        // extern). `is_ascii_alpha`-vs-`_` (`is_alpha`) is composed in Kāra as
        // `b.is_ascii_alphabetic() or b == b'_'`.
        if args.is_empty()
            && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            && matches!(
                method,
                "is_ascii_digit" | "is_ascii_alphabetic" | "is_ascii_hexdigit"
            )
        {
            return Type::Bool;
        }
        // Wrapping integer arithmetic — `wrapping_add` / `wrapping_sub` /
        // `wrapping_mul` (design.md § Arithmetic Overflow, the `wrapping_*`
        // family): two's-complement wraparound with NO overflow trap, the
        // non-trapping sibling of the checked `+`/`-`/`*` path
        // (`emit_checked_int_arith`). Both operands and the result are the
        // receiver's type. **Scoped to the 64-bit widths** (`i64` / `u64` /
        // `usize`) in this slice: those are i64-backed end-to-end (interpreter
        // `Value::Int(i64)`, codegen i64), so a bare wrap is exact. The narrow
        // widths (`i8`..`i32` / `u8`..`u32`) need width-masking in both
        // backends, and `i128`/`u128` are not yet i64-representable in the
        // interpreter — both are a tracked follow-on (`NoMethodFound` fires for
        // them until then). Backends: codegen `method_call.rs`
        // (`build_int_{add,sub,mul}`), interpreter `method_call.rs` (Rust
        // `i64::wrapping_*`). Primary motivation: a wrapping kernel body is
        // straight-line (no per-element overflow-trap branch), which is what
        // lets LLVM auto-vectorize integer slice kernels — see
        // `roadmap.md` § Codegen Optimization.
        if matches!(method, "wrapping_add" | "wrapping_sub" | "wrapping_mul")
            && matches!(
                &receiver_for_lookup,
                Type::Int(IntSize::I64) | Type::UInt(UIntSize::U64) | Type::UInt(UIntSize::Usize)
            )
        {
            if args.len() != 1 {
                self.type_error(
                    format!("{method} expects 1 argument, got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            // Q4 literal promotion (mirrors `infer_binary` in expr_ops.rs): a
            // suffix-free integer literal argument is promoted to the receiver
            // type, so `x.wrapping_add(1)` type-checks. Otherwise the argument
            // must match the receiver type exactly — the same strict same-type
            // rule the `+`/`-`/`*` operators enforce (mixed concrete integer
            // types are a hard error; cast with `as`).
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &receiver_for_lookup);
                return receiver_for_lookup.clone();
            }
            if arg_ty != Type::Error && arg_ty != receiver_for_lookup {
                self.type_error(
                    format!(
                        "{method} expects an argument of type `{}`, got `{}`",
                        type_display(&receiver_for_lookup),
                        type_display(&arg_ty)
                    ),
                    arg.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            return receiver_for_lookup.clone();
        }
        // Built-in `clone` / `to_string` on the scalar numeric + bool + char
        // primitives (all `Copy`). `clone` is identity → `Self`; `to_string`
        // renders the value → `String` (`Type::Str`). Like `abs`, these are
        // dedicated value-receiver methods (the registered builtin impls model
        // the type-receiver/operator form). Backends: interpreter clones the
        // `Value` / formats via `Display`; codegen returns the scalar
        // unchanged / builds an owning `String` from the f-string renderer.
        // `String`/struct receivers are left to their existing paths (not
        // matched here — `Type::Str` and `Type::Named` are excluded).
        if args.is_empty()
            && matches!(
                &receiver_for_lookup,
                Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
            )
        {
            if method == "clone" {
                return receiver_for_lookup.clone();
            }
            if method == "to_string" {
                return Type::Str;
            }
        }
        // Unicode `char` classification predicates (phase-12 #13):
        // `char.is_alphabetic()` / `is_numeric()` / `is_alphanumeric()` /
        // `is_whitespace()` → bool. The Unicode-aware companions of the
        // `u8.is_ascii_*` byte predicates; backed by interp (`char` methods) and
        // codegen (`karac_runtime_char_is_*` externs). Restricted to a `char`
        // receiver — the ASCII predicates stay on the byte/integer scalars.
        if args.is_empty()
            && matches!(&receiver_for_lookup, Type::Char)
            && matches!(
                method,
                "is_alphabetic" | "is_numeric" | "is_alphanumeric" | "is_whitespace"
            )
        {
            return Type::Bool;
        }
        // `to_string()` on `String` (identity copy), on any `#[derive(Display)]`
        // / `impl Display` **struct**, and on an all-unit `#[derive(Display)]`
        // **enum** → `String`. The `Display` trait provides
        // `to_string(ref self) -> String` (design.md § Display); this types the
        // explicit call so it stops poisoning to `Type::Error`. Codegen renders
        // structs in declaration order and all-unit enums as the bare variant
        // name (`synth_display`). Payload-bearing enums and `#[display_snake_case]`
        // enums are excluded — their codegen renderer is a follow-on, so leaving
        // `to_string` untyped keeps a clean typecheck rejection rather than a
        // codegen failure (interp still renders them under `karac run`).
        if method == "to_string" && args.is_empty() {
            let is_display_named = match &receiver_for_lookup {
                Type::Str => true,
                Type::Named { name, .. } if name == "String" => true,
                // Collections render in codegen (`try_compile_collection_display`)
                // when their element/key/value types are `Display`.
                Type::Named { name, .. }
                    if matches!(name.as_str(), "Vec" | "VecDeque" | "Map" | "Set") =>
                {
                    self.type_supports_display(&receiver_for_lookup)
                }
                Type::Named { name, .. } => {
                    let struct_display = self
                        .env
                        .structs
                        .get(name)
                        .map(|s| s.derived_traits.contains("Display"))
                        .unwrap_or(false)
                        || (self.env.structs.contains_key(name)
                            && self.env.impls.iter().any(|i| {
                                i.target_type == *name && i.trait_name.as_deref() == Some("Display")
                            }));
                    let enum_display = self
                        .env
                        .enums
                        .get(name)
                        .map(|e| {
                            e.derived_traits.contains("Display")
                                && !self.display_snake_case_enums.contains(name)
                                && e.variants
                                    .iter()
                                    .all(|(_, vt)| matches!(vt, VariantTypeInfo::Unit))
                        })
                        .unwrap_or(false);
                    struct_display || enum_display
                }
                _ => false,
            };
            if is_display_named {
                return Type::Str;
            }
        }
        let (type_name, type_args) = match &receiver_for_lookup {
            Type::Named { name, args } => (name.clone(), args.clone()),
            // A refinement receiver that survived the base-deref above
            // (i.e. it declares this method itself) resolves under its
            // nominal name. Non-generic at v1, so no type args.
            Type::Refinement { name, .. } => (name.clone(), Vec::new()),
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
            Type::Existential {
                trait_name,
                tait_alias,
                ..
            } => {
                // `impl Trait` slice 6 — TAIT and return-position
                // existentials dispatch through the declared trait
                // surface. Find the trait's own method by name; if
                // missing, emit `E_TAIT_NOT_IMPLEMENTED_YET` (slice 6)
                // when the existential is TAIT-sourced (the witness's
                // own non-trait method might exist but resolving it
                // requires the witness-inference pipeline that lands
                // in P1), or the generic no-method-on-trait diagnostic
                // for return-position existentials. Method calls that
                // hit the trait surface succeed exactly as if the
                // receiver were a `Type::TypeParam` with the trait as
                // its only bound — slice 3's `enclosing_bounds` story
                // already covers the lookup machinery.
                let trait_name_clone = trait_name.clone();
                let tait_alias_clone = tait_alias.clone();
                return self.dispatch_existential_receiver_method(
                    &trait_name_clone,
                    tait_alias_clone.as_deref(),
                    method,
                    args,
                    span,
                );
            }
            _ => {
                // For non-named types, just type-check args and return Error
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                // Close the primitive silent-poison hole for *numeric* receivers
                // (`i64`, `u32`, `f64`, …). Numeric primitives have a closed
                // method surface — the registered builtin ops (add/sub/cmp/eq/…)
                // plus a small value-receiver special set — so an unknown method
                // here is a genuine typo, not a partially-implicit prelude
                // surface. Without this it returns `Type::Error` (poison, which
                // is universally assignable, so `let s: String = x.bogus()`
                // typechecked clean) and then exploded in the backend: codegen's
                // "no handler" error, or the interpreter's `unreachable!` ICE.
                // Fire `NoMethodFound` instead. `String`/`bool`/`char` are left
                // on the historical silent fall-through (String has a large
                // partially-implicit method surface not modelled in the impl
                // table). Type-arg-bearing calls (`x.cast[T]()`) resolve through
                // their own path and don't reach here.
                if matches!(
                    &receiver_for_lookup,
                    Type::Int(_) | Type::UInt(_) | Type::Float(_)
                ) {
                    if let Some(prim) = method_callee_type_name(&receiver_for_lookup) {
                        // Value-receiver methods that work today via dedicated
                        // backend arms rather than the impl table — keep
                        // poisoning so those paths still handle them. (`abs`,
                        // `clone`, and `to_string` are handled in the early
                        // intercept above for these numeric types and so never
                        // reach here; for `u*`, `abs` is correctly absent and
                        // falls through to the error.)
                        const PRIMITIVE_VALUE_METHODS: &[&str] =
                            &["cmp", "eq", "ne", "lt", "le", "gt", "ge", "cast"];
                        let known = PRIMITIVE_VALUE_METHODS.contains(&method)
                            || !self
                                .env
                                .find_methods_with_args(&prim, &[], method)
                                .is_empty();
                        if !known {
                            self.type_error(
                                format!("no method '{}' on type '{}'", method, prim),
                                span.clone(),
                                TypeErrorKind::NoMethodFound,
                            );
                        }
                    }
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
                    || self.env.enums.contains_key(&type_name)
                    // Distinct types have an exhaustively-known method surface
                    // (inherent impls only — no base deref), so an unresolved
                    // method on one is a real `NoMethodFound`, not the
                    // historical silent prelude fall-through.
                    || self.env.distinct_bases.contains_key(&type_name))
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

    // ── Raw Pointer Construction ────────────────────────────────
    //
    // `ptr.const(place)` / `ptr.mut(place)` produce `*const T` /
    // `*mut T` from a place expression rooted at a binding. The
    // argument is parsed as an ordinary expression but typechecked
    // against the place-expression validator instead of the value-
    // expression typecheck — non-place arguments route to
    // `E_PTR_CONST_REQUIRES_PLACE` / `E_PTR_MUT_REQUIRES_PLACE`. The
    // `.mut` form additionally rejects structurally-immutable places
    // (`ref T` root, deref of `*const T`) with
    // `E_PTR_MUT_REQUIRES_MUTABLE_PLACE`, mirroring the structural
    // mut-reachability rule used by `place_root_is_mut_borrow` for
    // mut-ref call-site forwarding. Spec: design.md § Raw Pointer
    // Construction (v60 item 19); tracker: phase-5-diagnostics line
    // 573.
    pub(super) fn infer_ptr_construction(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let is_mut = method == "mut";
        let dotted = if is_mut { "ptr.mut" } else { "ptr.const" };

        // Arity: exactly one positional argument.
        if args.len() != 1 {
            self.type_error(
                format!(
                    "'{}' expects 1 argument (the place to take a raw pointer to), found {}",
                    dotted,
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Pointer {
                is_mut,
                inner: Box::new(Type::Error),
            };
        }
        let arg = &args[0];
        if arg.label.is_some() {
            self.type_error(
                format!("'{}' does not accept labeled arguments", dotted),
                arg.value.span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
        }

        let inner = self.infer_expr(&arg.value);

        // Place-form validation. Structural: chains of binding /
        // field-access / index / tuple-index / deref over those.
        if !Self::is_place_expression(&arg.value) {
            let code = if is_mut {
                "E_PTR_MUT_REQUIRES_PLACE"
            } else {
                "E_PTR_CONST_REQUIRES_PLACE"
            };
            self.type_error(
                format!(
                    "error[{}]: '{}' requires a place expression — a binding, field access, \
                     index, or dereference; got a value-producing expression that has no \
                     stable address",
                    code, dotted
                ),
                arg.value.span.clone(),
                TypeErrorKind::InvalidUnaryOp,
            );
            return Type::Pointer {
                is_mut,
                inner: Box::new(inner),
            };
        }

        // Mutable-place validation (structural, type-driven). Mirrors
        // the rule in `place_root_is_mut_borrow`: root binding's type
        // alone decides reachability — `Type::Ref(_)` → not mutable;
        // deref of `*const T` → not mutable; everything else accepted.
        // Owned-binding `let mut` enforcement is the ownership
        // checker's responsibility.
        if is_mut && !self.place_is_mut_reachable(&arg.value) {
            self.type_error(
                "error[E_PTR_MUT_REQUIRES_MUTABLE_PLACE]: 'ptr.mut' requires a mutably \
                 reachable place — the rooted binding is a shared reference ('ref T') or \
                 the deref chain passes through a '*const T', neither of which permits \
                 mutation"
                    .to_string(),
                arg.value.span.clone(),
                TypeErrorKind::InvalidUnaryOp,
            );
        }

        // Peel `Type::Ref` / `Type::MutRef` so a bare ref-typed
        // binding place (`r: ref T` → `ptr.const(r)`) produces
        // `*const T`, not `*const ref T`. The place's *value* is
        // the underlying T; ref/mut-ref are borrow modes, not
        // distinct address-space layers.
        let pointee = match inner {
            Type::Ref(inner) | Type::MutRef(inner) => *inner,
            other => other,
        };

        Type::Pointer {
            is_mut,
            inner: Box::new(pointee),
        }
    }

    /// Structural place-expression predicate for `ptr.const` /
    /// `ptr.mut`. Matches the borrow-rooting rules: bindings,
    /// `self`, field access, tuple index, index, and dereference of
    /// a place. Method calls / function calls / binops / literals
    /// are not places — they produce values without stable addresses.
    fn is_place_expression(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Identifier(_) | ExprKind::SelfValue => true,
            ExprKind::FieldAccess { object, .. } => Self::is_place_expression(object),
            ExprKind::TupleIndex { object, .. } => Self::is_place_expression(object),
            ExprKind::Index { object, .. } => Self::is_place_expression(object),
            ExprKind::Unary {
                op: UnaryOp::Deref,
                operand,
            } => Self::is_place_expression(operand),
            _ => false,
        }
    }

    /// Walk the place chain to find its root and check that mutation
    /// can structurally reach the leaf. Returns `true` when the root
    /// binding's type is not `Type::Ref(_)` and no deref step on the
    /// path passes through a `*const T` / `Type::Ref(_)`. Owned
    /// bindings (`let x = ...` vs `let mut x = ...`) are always
    /// accepted at this layer — the ownership checker handles
    /// binding-level `let mut` enforcement.
    fn place_is_mut_reachable(&self, expr: &Expr) -> bool {
        let mut e = expr;
        loop {
            match &e.kind {
                ExprKind::Identifier(name) => {
                    return !matches!(self.local_scope.lookup(name), Some(Type::Ref(_)));
                }
                ExprKind::SelfValue => {
                    return !matches!(self.local_scope.lookup("self"), Some(Type::Ref(_)));
                }
                ExprKind::FieldAccess { object, .. } => e = object,
                ExprKind::TupleIndex { object, .. } => e = object,
                ExprKind::Index { object, .. } => e = object,
                ExprKind::Unary {
                    op: UnaryOp::Deref,
                    operand,
                } => {
                    let operand_ty = self
                        .expr_types
                        .get(&SpanKey::from_span(&operand.span))
                        .cloned();
                    match operand_ty {
                        Some(Type::Ref(_)) => return false,
                        Some(Type::Pointer { is_mut: false, .. }) => return false,
                        _ => e = operand,
                    }
                }
                _ => return true,
            }
        }
    }

    /// Type-check an instance-method call on `Vector[T, N]` (design.md
    /// § Portable SIMD). Slices 2 / 2b surface — Vector→scalar:
    ///   - `reduce_{sum,product,and,or,xor}() -> T` — horizontal folds.
    ///     `and`/`or`/`xor` are bitwise (integer element only).
    ///   - `reduce_{min,max}() -> T` — signed-integer / float element only
    ///     (unsigned deferred — needs the signedness side-table).
    ///   - `dot(other: Vector[T, N]) -> T` — dot product (element-wise
    ///     product summed); `other` must be the same `Vector[T, N]`.
    ///
    /// All return the element type `T`. Construction helpers (`splat`/`from_*`)
    /// and `cross` are later sub-slices (phase-7 line 289).
    /// `Vector[T, N].splat(x) -> Vector[T, N]` (design.md § Portable SIMD):
    /// broadcast a scalar to all `N` lanes. `x` must be assignable to the
    /// element type `T`. This is the explicit broadcast slice-1 deliberately
    /// omitted (vector-vs-scalar arithmetic stays a type error; broadcast is
    /// spelled out via `splat`).
    fn infer_vector_splat(&mut self, ga: &[GenericArg], args: &[CallArg], span: &Span) -> Type {
        let lowered = self.lower_vector_type(&Some(ga.to_vec()), &[], span);
        let Some(Type::Vector { element, lanes }) = lowered else {
            // lower_vector_type already reported the bad element/lane shape.
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Error;
        };
        if args.len() != 1 {
            self.type_error(
                format!("'splat' takes exactly one argument, found {}", args.len()),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Vector { element, lanes };
        }
        // The scalar must be assignable to the element type; thread `T` as the
        // expected type so a suffixless literal resolves cleanly.
        self.check_expr(&args[0].value, &element);
        let result = Type::Vector { element, lanes };
        self.record_expr_type(span, &result);
        result
    }

    /// `Vector[T, N].from_array(a) -> Vector[T, N]` (design.md § Portable SIMD):
    /// build a vector from a fixed `[T; N]` array. The single argument is
    /// checked against the expected array type `Array[T, N]` — reusing the
    /// check-mode `[..]`-literal coercion in `check_expr`, this enforces both
    /// the element type (each element assignable to `T`) and the length (the
    /// literal must hold exactly `N` elements) in one shot. `Vector.lanes` and
    /// `Array.size` are the same `ConstArg`, so the lane count flows straight
    /// through as the expected array size.
    fn infer_vector_from_array(
        &mut self,
        ga: &[GenericArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let lowered = self.lower_vector_type(&Some(ga.to_vec()), &[], span);
        let Some(Type::Vector { element, lanes }) = lowered else {
            // lower_vector_type already reported the bad element/lane shape.
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Error;
        };
        if args.len() != 1 {
            self.type_error(
                format!(
                    "'from_array' takes exactly one argument, found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Vector { element, lanes };
        }
        let expected_array = Type::Array {
            element: element.clone(),
            size: lanes.clone(),
        };
        self.check_expr(&args[0].value, &expected_array);
        let result = Type::Vector { element, lanes };
        self.record_expr_type(span, &result);
        result
    }

    /// `Vector[T, N].from_slice(s)` — build a `<N x T>` from a `Slice[T]`. The
    /// slice length is a runtime property (unlike `from_array`'s static `N`),
    /// so the typechecker only verifies the argument is a `Slice` whose element
    /// matches the vector element `T`; the `len == N` check happens at runtime
    /// (codegen panic / interpreter panic). Both `Slice[T]` and `mut Slice[T]`
    /// are accepted — construction reads the window, so mutability is irrelevant.
    fn infer_vector_from_slice(
        &mut self,
        ga: &[GenericArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let lowered = self.lower_vector_type(&Some(ga.to_vec()), &[], span);
        let Some(Type::Vector { element, lanes }) = lowered else {
            // lower_vector_type already reported the bad element/lane shape.
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Error;
        };
        if args.len() != 1 {
            self.type_error(
                format!(
                    "'from_slice' takes exactly one argument, found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Vector { element, lanes };
        }
        let arg_ty = self.infer_expr(&args[0].value);
        match &arg_ty {
            Type::Slice {
                element: arg_elem, ..
            } => {
                if **arg_elem != *element && !matches!(**arg_elem, Type::Error) {
                    self.type_error(
                        format!(
                            "'from_slice' expects a 'Slice[{}]' matching the Vector element, \
                             found 'Slice[{}]'",
                            type_display(&element),
                            type_display(arg_elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            Type::Error => {}
            other => {
                self.type_error(
                    format!(
                        "'from_slice' expects a 'Slice[{}]' argument, found '{}'",
                        type_display(&element),
                        type_display(other)
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        let result = Type::Vector { element, lanes };
        self.record_expr_type(span, &result);
        result
    }

    /// `Vector[T, N].load_masked(slice, mask) -> Vector[T, N]` (design.md
    /// § Portable SIMD, "Masked load/store"): build a `<N x T>` by loading only
    /// the lanes the `mask` selects. `slice` is a `Slice[T]` (or `mut Slice[T]`
    /// — reads ignore mutability); `mask` is the `Vector[bool, N]` predicate.
    /// Lane `i` is *active* iff `mask[i]`; an active lane requires `i < len`
    /// (a runtime panic otherwise), and an inactive lane reads `0` without
    /// touching memory — so a tail mask processes a short slice safely.
    fn infer_vector_load_masked(
        &mut self,
        ga: &[GenericArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let lowered = self.lower_vector_type(&Some(ga.to_vec()), &[], span);
        let Some(Type::Vector { element, lanes }) = lowered else {
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Error;
        };
        if args.len() != 2 {
            self.type_error(
                format!(
                    "'load_masked' takes exactly two arguments (slice, mask), found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Vector { element, lanes };
        }
        // arg0 — a `Slice[T]` (mutable or not) whose element matches `T`.
        let slice_ty = self.infer_expr(&args[0].value);
        match &slice_ty {
            Type::Slice {
                element: arg_elem, ..
            } => {
                if **arg_elem != *element && !matches!(**arg_elem, Type::Error) {
                    self.type_error(
                        format!(
                            "'load_masked' expects a 'Slice[{}]' matching the Vector element, \
                             found 'Slice[{}]'",
                            type_display(&element),
                            type_display(arg_elem)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            Type::Error => {}
            other => {
                self.type_error(
                    format!(
                        "'load_masked' expects a 'Slice[{}]' first argument, found '{}'",
                        type_display(&element),
                        type_display(other)
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        // arg1 — the `Vector[bool, N]` mask (same lane count as the result).
        let mask_ty = self.infer_expr(&args[1].value);
        let expected_mask = Type::Vector {
            element: Box::new(Type::Bool),
            lanes: lanes.clone(),
        };
        if mask_ty != expected_mask && mask_ty != Type::Error {
            self.type_error(
                format!(
                    "'load_masked' mask must be a '{}' (a vector comparison result), found '{}'",
                    type_display(&expected_mask),
                    type_display(&mask_ty)
                ),
                args[1].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
        }
        let result = Type::Vector { element, lanes };
        self.record_expr_type(span, &result);
        result
    }

    /// `Vector[T, N].gather(slice, indices) -> Vector[T, N]` (design.md
    /// § Portable SIMD, "Gather / scatter"): build a `<N x T>` by reading
    /// `slice[indices[i]]` for each lane. `slice` is a `Slice[T]`; `indices` is
    /// a `Vector[U, N]` of integer lane offsets. Every lane is active (no mask);
    /// each index is bounds-checked at run time (`0 <= idx < len`, panic
    /// otherwise) exactly like the `v[i]` / `slice[i]` reads.
    fn infer_vector_gather(&mut self, ga: &[GenericArg], args: &[CallArg], span: &Span) -> Type {
        let lowered = self.lower_vector_type(&Some(ga.to_vec()), &[], span);
        let Some(Type::Vector { element, lanes }) = lowered else {
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Error;
        };
        if args.len() != 2 {
            self.type_error(
                format!(
                    "'gather' takes exactly two arguments (slice, indices), found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Vector { element, lanes };
        }
        // arg0 — a `Slice[T]` (mutable or not) whose element matches `T`.
        let slice_ty = self.infer_expr(&args[0].value);
        match &slice_ty {
            Type::Slice {
                element: arg_elem, ..
            } => {
                if **arg_elem != *element && !matches!(**arg_elem, Type::Error) {
                    self.type_error(
                        format!(
                            "'gather' expects a 'Slice[{}]' matching the Vector element, \
                             found 'Slice[{}]'",
                            type_display(&element),
                            type_display(arg_elem)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            Type::Error => {}
            other => {
                self.type_error(
                    format!(
                        "'gather' expects a 'Slice[{}]' first argument, found '{}'",
                        type_display(&element),
                        type_display(other)
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        // arg1 — a `Vector[U, N]` of integer indices (lane count matches `N`).
        let idx_ty = self.infer_expr(&args[1].value);
        match &idx_ty {
            Type::Vector {
                element: ie,
                lanes: il,
            } => {
                if !matches!(**ie, Type::Int(_) | Type::UInt(_) | Type::Error) {
                    self.type_error(
                        format!(
                            "'gather' indices must be an integer vector, found '{}'",
                            type_display(&idx_ty)
                        ),
                        args[1].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                } else if il != &lanes {
                    self.type_error(
                        format!(
                            "'gather' indices must have the same lane count as the result \
                             ('{}'), found '{}'",
                            type_display(&Type::Vector {
                                element: element.clone(),
                                lanes: lanes.clone(),
                            }),
                            type_display(&idx_ty)
                        ),
                        args[1].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            Type::Error => {}
            other => {
                self.type_error(
                    format!(
                        "'gather' indices must be an integer vector, found '{}'",
                        type_display(other)
                    ),
                    args[1].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        let result = Type::Vector { element, lanes };
        self.record_expr_type(span, &result);
        result
    }

    /// `Vector[U, N].cast_from(v) -> Vector[U, N]` (design.md § Portable SIMD,
    /// "Conversion: `v.cast::<U>()`"): per-lane numeric conversion of a source
    /// `Vector[S, N]` to the target element `U`. Carried as a static
    /// constructor (the `Vector[U, N].method(v)` type-path form already used by
    /// `splat`/`from_array`/`load_masked`/`gather`) because the design's
    /// instance turbofish `v.cast::<U>()` is not yet parseable (`[` after a
    /// method name is not consumed as type-args — see the slice-6c note). Both
    /// `S` and `U` must be numeric (signed/unsigned int or float); the source
    /// lane count must equal the target `N`. Lossy where applicable
    /// (float→int truncates, narrowing int truncates) — matches scalar `as`.
    fn infer_vector_cast_from(&mut self, ga: &[GenericArg], args: &[CallArg], span: &Span) -> Type {
        let lowered = self.lower_vector_type(&Some(ga.to_vec()), &[], span);
        let Some(Type::Vector { element, lanes }) = lowered else {
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Error;
        };
        let numeric = |t: &Type| matches!(t, Type::Int(_) | Type::UInt(_) | Type::Float(_));
        if !numeric(&element) && !matches!(*element, Type::Error) {
            self.type_error(
                format!(
                    "'cast_from' target element must be a numeric type (int or float), found '{}'",
                    type_display(&element)
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
        }
        if args.len() != 1 {
            self.type_error(
                format!(
                    "'cast_from' takes exactly one argument (the source vector), found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Vector { element, lanes };
        }
        let src_ty = self.infer_expr(&args[0].value);
        match &src_ty {
            Type::Vector {
                element: se,
                lanes: sl,
            } => {
                if !numeric(se) && !matches!(**se, Type::Error) {
                    self.type_error(
                        format!(
                            "'cast_from' source element must be a numeric type, found '{}'",
                            type_display(&src_ty)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                } else if sl != &lanes {
                    self.type_error(
                        format!(
                            "'cast_from' source must have the same lane count as the target \
                             ('{}'), found '{}'",
                            type_display(&Type::Vector {
                                element: element.clone(),
                                lanes: lanes.clone(),
                            }),
                            type_display(&src_ty)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            Type::Error => {}
            other => {
                self.type_error(
                    format!(
                        "'cast_from' expects a source Vector[S, N], found '{}'",
                        type_display(other)
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        let result = Type::Vector { element, lanes };
        self.record_expr_type(span, &result);
        result
    }

    fn infer_vector_method(
        &mut self,
        element: &Type,
        lanes: &ConstArg,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let elem = element.clone();
        match method {
            // No-argument horizontal reductions → `T`. `sum`/`product` work
            // on any numeric element; `and`/`or`/`xor` are bitwise and require
            // an integer element (design.md § Portable SIMD: "Bitwise — integer
            // lanes only").
            "reduce_sum" | "reduce_product" | "reduce_and" | "reduce_or" | "reduce_xor" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("'{}' takes no arguments", method),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                }
                let is_bitwise = matches!(method, "reduce_and" | "reduce_or" | "reduce_xor");
                if is_bitwise && !matches!(elem, Type::Int(_) | Type::UInt(_)) {
                    self.type_error(
                        format!(
                            "'{}' requires an integer element type; Vector element is '{}'",
                            method,
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                elem
            }
            // Horizontal min/max → `T`. Any numeric element — signed integer,
            // unsigned integer, or float. The element signedness rides the
            // `unsigned_vector_exprs` span side-table (lowering → codegen) so
            // codegen picks `ult`/`ugt` over `slt`/`sgt` for unsigned lanes;
            // the interpreter reads the same signedness off the receiver's
            // recorded type and compares the `Value::Int` carrier as `u64`.
            "reduce_min" | "reduce_max" => {
                if !args.is_empty() {
                    self.type_error(
                        format!("'{}' takes no arguments", method),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                }
                if !matches!(elem, Type::Int(_) | Type::UInt(_) | Type::Float(_)) {
                    self.type_error(
                        format!(
                            "'{}' requires a numeric Vector element (signed/unsigned \
                             integer or float); Vector element is '{}'",
                            method,
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                elem
            }
            "dot" => {
                let expected = Type::Vector {
                    element: Box::new(elem.clone()),
                    lanes: lanes.clone(),
                };
                if args.len() != 1 {
                    self.type_error(
                        format!("'dot' takes exactly one argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return elem;
                }
                let arg_ty = self.infer_expr(&args[0].value);
                if arg_ty != expected && arg_ty != Type::Error {
                    self.type_error(
                        format!(
                            "'dot' requires the argument to be the same vector type '{}', found '{}'",
                            type_display(&expected),
                            type_display(&arg_ty)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                elem
            }
            // Cross product (design.md § Portable SIMD): `v.cross(w) ->
            // Vector[T, 3]`, defined for 3-lane vectors only. Requires a
            // statically-known lane count of exactly 3 and a same-typed
            // argument; the result is the same `Vector[T, 3]`.
            "cross" => {
                let vec_ty = Type::Vector {
                    element: Box::new(elem.clone()),
                    lanes: lanes.clone(),
                };
                if !matches!(lanes, ConstArg::Literal(3)) {
                    self.type_error(
                        format!(
                            "'cross' is defined only for 3-lane vectors (Vector[T, 3]); \
                             found '{}'",
                            type_display(&vec_ty)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return Type::Error;
                }
                if args.len() != 1 {
                    self.type_error(
                        format!("'cross' takes exactly one argument, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return vec_ty;
                }
                let arg_ty = self.infer_expr(&args[0].value);
                if arg_ty != vec_ty && arg_ty != Type::Error {
                    self.type_error(
                        format!(
                            "'cross' requires the argument to be the same vector type '{}', found '{}'",
                            type_display(&vec_ty),
                            type_display(&arg_ty)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                vec_ty
            }
            // `mask.select(a, b)` (design.md § Portable SIMD): per-lane choose
            // `a[i]` where the mask lane is true, else `b[i]`. Valid only on a
            // mask receiver — a `Vector[bool, N]` produced by a vector
            // comparison. Both arguments must be the same `Vector[T, N]` whose
            // lane count matches the mask; the result is that vector type.
            "select" => {
                let mask_ty = Type::Vector {
                    element: Box::new(elem.clone()),
                    lanes: lanes.clone(),
                };
                if !matches!(elem, Type::Bool) {
                    self.type_error(
                        format!(
                            "'select' is only valid on a mask (Vector[bool, N]) produced \
                             by a vector comparison; receiver is '{}'",
                            type_display(&mask_ty)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return Type::Error;
                }
                if args.len() != 2 {
                    self.type_error(
                        format!("'select' takes exactly two arguments, found {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return Type::Error;
                }
                let a_ty = self.infer_expr(&args[0].value);
                let b_ty = self.infer_expr(&args[1].value);
                match (&a_ty, &b_ty) {
                    (
                        Type::Vector {
                            element: ae,
                            lanes: al,
                        },
                        Type::Vector {
                            element: be,
                            lanes: bl,
                        },
                    ) => {
                        if ae != be || al != bl {
                            self.type_error(
                                format!(
                                    "'select' arguments must be the same Vector[T, N] type; \
                                     found '{}' and '{}'",
                                    type_display(&a_ty),
                                    type_display(&b_ty)
                                ),
                                args[1].value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                            return Type::Error;
                        }
                        if al != lanes {
                            self.type_error(
                                format!(
                                    "'select' arguments must have the same lane count as the \
                                     mask (mask is '{}', arguments are '{}')",
                                    type_display(&mask_ty),
                                    type_display(&a_ty)
                                ),
                                args[0].value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                            return Type::Error;
                        }
                        a_ty
                    }
                    (Type::Error, _) | (_, Type::Error) => Type::Error,
                    _ => {
                        self.type_error(
                            format!(
                                "'select' arguments must be Vector[T, N] values; \
                                 found '{}' and '{}'",
                                type_display(&a_ty),
                                type_display(&b_ty)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        Type::Error
                    }
                }
            }
            // Lane permutations (design.md § Portable SIMD, "Lane shuffling").
            // `reverse()` reverses lane order; `rotate_lanes_left(n)` /
            // `rotate_lanes_right(n)` cyclically shift lanes by a compile-time
            // constant `n`. All return the same `Vector[T, N]`. The permutation
            // is fixed at compile time (it lowers to a constant lane shuffle),
            // so the rotate amount must be a non-negative integer literal — a
            // runtime amount has no constant shuffle mask.
            "reverse" => {
                if !args.is_empty() {
                    self.type_error(
                        "'reverse' takes no arguments".to_string(),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                }
                Type::Vector {
                    element: Box::new(elem),
                    lanes: lanes.clone(),
                }
            }
            "rotate_lanes_left" | "rotate_lanes_right" => {
                let vec_ty = Type::Vector {
                    element: Box::new(elem.clone()),
                    lanes: lanes.clone(),
                };
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "'{method}' takes exactly one argument (the rotate amount), \
                             found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return vec_ty;
                }
                let _ = self.infer_expr(&args[0].value);
                if !matches!(&args[0].value.kind, ExprKind::Integer(n, _) if *n >= 0) {
                    self.type_error(
                        format!(
                            "'{method}' requires a non-negative compile-time integer literal \
                             rotate amount"
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                vec_ty
            }
            // Lane replace (design.md § Portable SIMD, "Lane access /
            // mutation"): `v.replace(i, x) -> Vector[T, N]` returns a new
            // vector with lane `i` set to `x` (the receiver is unchanged — the
            // in-place `set(i, x)` is the paired follow-on). The index is a
            // runtime integer, bounds-checked at run time exactly like the
            // `v[i]` lane read; `x` must be assignable to the element type `T`
            // (suffixless numeric literals coerce, as in construction).
            "replace" => {
                let vec_ty = Type::Vector {
                    element: Box::new(elem.clone()),
                    lanes: lanes.clone(),
                };
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "'replace' takes exactly two arguments (index, value), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return vec_ty;
                }
                let idx_ty = self.infer_expr(&args[0].value);
                if !matches!(idx_ty, Type::Int(_) | Type::UInt(_)) && idx_ty != Type::Error {
                    self.type_error(
                        format!(
                            "'replace' index must be an integer, found '{}'",
                            type_display(&idx_ty)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                self.check_expr(&args[1].value, &elem);
                vec_ty
            }
            // Lane shuffle (design.md § Portable SIMD, "Lane shuffling"):
            // `v.shuffle([i0, i1, .., i_{M-1}]) -> Vector[T, M]` gathers source
            // lanes by a compile-time index list — result lane `j` = source
            // lane `indices[j]`. The result lane count `M` is the index-list
            // length and may differ from the source `N`. The design's
            // turbofish form `shuffle::<[i64; M]>()` is not yet parseable, so
            // the index list is a literal-array argument. Each index must be a
            // non-negative integer literal in range `[0, N)`.
            "shuffle" => {
                let src_vec = Type::Vector {
                    element: Box::new(elem.clone()),
                    lanes: lanes.clone(),
                };
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "'shuffle' takes exactly one argument (the index list), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return src_vec;
                }
                let ExprKind::ArrayLiteral(items) = &args[0].value.kind else {
                    self.type_error(
                        "'shuffle' requires a compile-time array literal of lane indices, \
                         e.g. v.shuffle([0, 2, 1, 3])"
                            .to_string(),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    let _ = self.infer_expr(&args[0].value);
                    return src_vec;
                };
                if items.is_empty() {
                    self.type_error(
                        "'shuffle' index list must select at least one lane".to_string(),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                let n = lanes.as_usize();
                for it in items {
                    let _ = self.infer_expr(it);
                    match &it.kind {
                        ExprKind::Integer(v, _) if *v >= 0 => {
                            if let Some(nn) = n {
                                if (*v as usize) >= nn {
                                    self.type_error(
                                        format!(
                                            "shuffle index {v} out of range for a {nn}-lane \
                                             source vector (valid indices are 0..{nn})"
                                        ),
                                        it.span.clone(),
                                        TypeErrorKind::TypeMismatch,
                                    );
                                }
                            }
                        }
                        _ => {
                            self.type_error(
                                "'shuffle' indices must be non-negative integer literals"
                                    .to_string(),
                                it.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                }
                Type::Vector {
                    element: Box::new(elem),
                    lanes: ConstArg::Literal(items.len() as i64),
                }
            }
            // Masked store (design.md § Portable SIMD, "Masked load/store"):
            // `v.store_masked(slice, mask)` writes each active lane `v[i]`
            // through a `mut Slice[T]`. Lane `i` is active iff `mask[i]`; an
            // active lane past the slice length traps at run time, an inactive
            // lane leaves the slice untouched. The destination must be a
            // *mutable* slice (the write side of `load_masked`); the receiver
            // vector is read-only, so this needs no value-receiver write-back.
            "store_masked" => {
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "'store_masked' takes exactly two arguments (slice, mask), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return Type::Unit;
                }
                let slice_ty = self.infer_expr(&args[0].value);
                match &slice_ty {
                    Type::Slice {
                        element: arg_elem,
                        mutable: true,
                    } => {
                        if **arg_elem != elem && !matches!(**arg_elem, Type::Error) {
                            self.type_error(
                                format!(
                                    "'store_masked' expects a 'mut Slice[{}]' matching the \
                                     Vector element, found 'mut Slice[{}]'",
                                    type_display(&elem),
                                    type_display(arg_elem)
                                ),
                                args[0].value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                    Type::Slice { mutable: false, .. } => {
                        self.type_error(
                            "'store_masked' requires a 'mut Slice[T]' destination (the slice \
                             must be mutable to write into)"
                                .to_string(),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    Type::Error => {}
                    other => {
                        self.type_error(
                            format!(
                                "'store_masked' expects a 'mut Slice[{}]' first argument, \
                                 found '{}'",
                                type_display(&elem),
                                type_display(other)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                let mask_ty = self.infer_expr(&args[1].value);
                let expected_mask = Type::Vector {
                    element: Box::new(Type::Bool),
                    lanes: lanes.clone(),
                };
                if mask_ty != expected_mask && mask_ty != Type::Error {
                    self.type_error(
                        format!(
                            "'store_masked' mask must be a '{}' (a vector comparison result), \
                             found '{}'",
                            type_display(&expected_mask),
                            type_display(&mask_ty)
                        ),
                        args[1].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                Type::Unit
            }
            // Scatter (design.md § Portable SIMD, "Gather / scatter"):
            // `v.scatter(slice, indices)` writes each lane `v[i]` to
            // `slice[indices[i]]` through a `mut Slice[T]`. The destination
            // must be a mutable slice; `indices` is an integer vector of the
            // same lane count; every lane is active (no mask) and each index is
            // bounds-checked at run time. The write mirror of `gather`.
            "scatter" => {
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "'scatter' takes exactly two arguments (slice, indices), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for a in args {
                        self.infer_expr(&a.value);
                    }
                    return Type::Unit;
                }
                let slice_ty = self.infer_expr(&args[0].value);
                match &slice_ty {
                    Type::Slice {
                        element: arg_elem,
                        mutable: true,
                    } => {
                        if **arg_elem != elem && !matches!(**arg_elem, Type::Error) {
                            self.type_error(
                                format!(
                                    "'scatter' expects a 'mut Slice[{}]' matching the Vector \
                                     element, found 'mut Slice[{}]'",
                                    type_display(&elem),
                                    type_display(arg_elem)
                                ),
                                args[0].value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                    Type::Slice { mutable: false, .. } => {
                        self.type_error(
                            "'scatter' requires a 'mut Slice[T]' destination (the slice must \
                             be mutable to write into)"
                                .to_string(),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    Type::Error => {}
                    other => {
                        self.type_error(
                            format!(
                                "'scatter' expects a 'mut Slice[{}]' first argument, found '{}'",
                                type_display(&elem),
                                type_display(other)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                let idx_ty = self.infer_expr(&args[1].value);
                match &idx_ty {
                    Type::Vector {
                        element: ie,
                        lanes: il,
                    } => {
                        if !matches!(**ie, Type::Int(_) | Type::UInt(_) | Type::Error) {
                            self.type_error(
                                format!(
                                    "'scatter' indices must be an integer vector, found '{}'",
                                    type_display(&idx_ty)
                                ),
                                args[1].value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        } else if il != lanes {
                            self.type_error(
                                format!(
                                    "'scatter' indices must have the same lane count as the \
                                     vector ('{}'), found '{}'",
                                    type_display(&Type::Vector {
                                        element: Box::new(elem.clone()),
                                        lanes: lanes.clone(),
                                    }),
                                    type_display(&idx_ty)
                                ),
                                args[1].value.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                    Type::Error => {}
                    other => {
                        self.type_error(
                            format!(
                                "'scatter' indices must be an integer vector, found '{}'",
                                type_display(other)
                            ),
                            args[1].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                }
                Type::Unit
            }
            _ => {
                self.type_error(
                    format!(
                        "no method '{}' on Vector[{}, _] (supported: dot, cross, \
                         reduce_sum, reduce_product, reduce_min, reduce_max, \
                         reduce_and, reduce_or, reduce_xor, reverse, \
                         rotate_lanes_left, rotate_lanes_right, replace, shuffle, \
                         store_masked, scatter; select on a Vector[bool, N] mask)",
                        method,
                        type_display(&elem)
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                for a in args {
                    self.infer_expr(&a.value);
                }
                Type::Error
            }
        }
    }

    /// `Result[ok, AllocError]` — the return shape of every fallible-allocation
    /// `try_*` companion (phase-8-stdlib-floor item 2). `AllocError` is the
    /// baked prelude enum, available without import.
    pub(super) fn result_alloc_error_type(&self, ok: Type) -> Type {
        Type::Named {
            name: "Result".to_string(),
            args: vec![
                ok,
                Type::Named {
                    name: "AllocError".to_string(),
                    args: Vec::new(),
                },
            ],
        }
    }

    /// `true` when `object` infers to a builtin heap-allocating collection
    /// (`Vec` / `VecDeque` / `Map` / `Set` / `SortedSet` / `String`), peeling
    /// `ref` / `mut ref`. Gates the `try_*` companion interception so it only
    /// fires for the builtin collections whose alloc-bearing methods it mirrors
    /// — a user type that defines its own `try_push` / `try_clone` / … falls
    /// through to normal dispatch. `infer_expr` is idempotent for an
    /// already-checked receiver, so this pre-inference is side-effect-free.
    pub(super) fn receiver_is_alloc_collection(&mut self, object: &Expr) -> bool {
        self.alloc_collection_receiver_name(object).is_some()
    }

    /// Builtin-collection display name of `object`'s receiver type (`"Vec"` /
    /// `"VecDeque"` / `"Map"` / `"Set"` / `"SortedSet"` / `"String"`), peeling
    /// `ref` / `mut ref`; `None` for any other type. Drives both the `try_*`
    /// companion gate and the `E_PANICKING_ALLOC_REJECTED` subject string.
    pub(super) fn alloc_collection_receiver_name(&mut self, object: &Expr) -> Option<String> {
        fn coll_name(ty: &Type) -> Option<String> {
            match ty {
                Type::Str => Some("String".to_string()),
                Type::Named { name, .. }
                    if matches!(
                        name.as_str(),
                        "Vec" | "VecDeque" | "Map" | "Set" | "SortedSet"
                    ) =>
                {
                    Some(name.clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => coll_name(inner),
                _ => None,
            }
        }
        coll_name(&self.infer_expr(object))
    }

    // ── Field Access ────────────────────────────────────────────
}
