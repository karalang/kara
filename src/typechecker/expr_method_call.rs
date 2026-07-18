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
use super::inference::{
    resolve_type_var_top, resolve_type_vars, substitute_type_params, unify_types,
};
use super::types::{
    clone_self_type_for, is_numeric, iterator_item_type_for, method_callee_type_name,
    receiver_for_method_lookup, type_display, ConstArg, FloatSize, IntSize, SubstValue, Type,
    UIntSize,
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

    /// Locate the AST `TraitDef` for `trait_name` — user program first,
    /// then the baked stdlib, mirroring [`Self::find_trait_method`]'s
    /// lookup order. Needed to read the trait's own `generic_params` when
    /// binding a bound's generic args (`C: Reduce[i64]` → `T := i64`).
    pub(super) fn find_trait_def<'p>(
        &'p self,
        trait_name: &str,
    ) -> Option<&'p crate::ast::TraitDef> {
        for item in &self.program.items {
            if let Item::TraitDef(t) = item {
                if t.name == trait_name {
                    return Some(t);
                }
            }
        }
        for (_, program) in crate::prelude::STDLIB_PROGRAMS.iter() {
            for item in &program.items {
                if let Item::TraitDef(t) = item {
                    if t.name == trait_name {
                        return Some(t);
                    }
                }
            }
        }
        None
    }

    /// Bind a trait bound's generic args to the trait's declared generic
    /// params: `C: Reduce[i64]` on `trait Reduce[T]` yields `[("T", i64)]`.
    /// Bound args are lowered in the enclosing generic scope (they may name
    /// sibling type params — `C: Reduce[U]`). Returns an empty vec when the
    /// bound has no args, the trait is unknown (built-in / derive-only), or
    /// the trait declares no generic params — the pre-existing Self-only
    /// substitution then applies unchanged.
    fn trait_bound_arg_subs(&mut self, bound: &crate::ast::TraitBound) -> Vec<(String, Type)> {
        let Some(args) = &bound.generic_args else {
            return Vec::new();
        };
        let Some(trait_name) = bound.path.last() else {
            return Vec::new();
        };
        let param_names: Vec<String> = match self
            .find_trait_def(trait_name)
            .and_then(|t| t.generic_params.as_ref())
        {
            Some(gp) => gp.params.iter().map(|p| p.name.clone()).collect(),
            None => return Vec::new(),
        };
        let arg_tes: Vec<crate::ast::TypeExpr> = args
            .iter()
            .filter_map(|a| match a {
                crate::ast::GenericArg::Type(te) => Some(te.clone()),
                _ => None,
            })
            .collect();
        let scope: Vec<String> = self.enclosing_bounds.keys().cloned().collect();
        param_names
            .into_iter()
            .zip(arg_tes)
            .map(|(name, te)| {
                let ty = self.lower_type_expr(&te, &scope);
                (name, ty)
            })
            .collect()
    }

    /// The element type of a `Trait[T]` bound (named `trait_name`) carried by a
    /// generic type parameter — `C` under `C: Reduce[i64]` → `i64`, or `C`
    /// under `C: ElementwiseMap[i64]` → `i64` — or `None` if `C` has no such
    /// bound. Lets the closure-primitive intercepts (`fold` on `Reduce`,
    /// `map`/`zip_with` on `ElementwiseMap`) type a BOUND-GENERIC receiver: the
    /// element is the trait bound's argument, the same value `trait_bound_arg_
    /// subs` binds for the trait-level `T` in the method signature, and the
    /// closure's element parameter IS this type.
    fn bound_element_for_trait(&mut self, type_param_name: &str, trait_name: &str) -> Option<Type> {
        let bounds = self.enclosing_bounds.get(type_param_name)?.clone();
        for b in &bounds {
            if b.path.last().map(String::as_str) == Some(trait_name) {
                if let Some((_, elem)) = self.trait_bound_arg_subs(b).into_iter().next() {
                    return Some(elem);
                }
            }
        }
        None
    }

    /// The `Type::TypeParam` name of a receiver expression's type, peeling one
    /// `ref`/`mut ref`. Used by the closure-primitive intercepts to recognize a
    /// bound-generic receiver (`c: ref C`) and consult its trait bound.
    fn receiver_type_param_name(obj_ty: &Type) -> Option<String> {
        match obj_ty {
            Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                Type::TypeParam(p) => Some(p.clone()),
                _ => None,
            },
            Type::TypeParam(p) => Some(p.clone()),
            _ => None,
        }
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
        let candidates: Vec<(crate::ast::TraitBound, crate::ast::TraitMethod)> = bounds
            .iter()
            .filter_map(|b| {
                let trait_name = b.path.last()?;
                let m = self.find_trait_method(trait_name, method)?;
                // Only methods (with self_param) are receiver-form
                // candidates. Associated functions (no self_param) reach
                // the dispatch only through type-prefixed `T.method()`.
                m.self_param.as_ref()?;
                Some((b.clone(), m.clone()))
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
                let (bound, trait_method) = candidates.into_iter().next().unwrap();
                // B-2026-07-08-6 secondary — record the resolved `Trait.method`
                // key so the ownership checker can see this generic trait-method
                // call's PARAMETER modes (a `ref Self` param is a borrow, not a
                // move). `method_callee_types` deliberately skips type-param
                // receivers (it feeds codegen/effect dispatch, which must key on
                // a concrete type), so this dedicated map carries the trait key.
                if let Some(trait_name) = bound.path.last() {
                    self.method_typeparam_trait_key.insert(
                        SpanKey::from_span(span),
                        format!("{}.{}", trait_name, method),
                    );
                }
                // Bind the bound's generic args to the trait's declared
                // params (`C: Reduce[i64]` → `T := i64`) so the method's
                // signature substitutes trait-level `T`s, not just `Self`
                // — without this, `c.sum()` under `C: Reduce[i64]` typed
                // as the raw TypeParam `T` (S6a, expected-i64-found-T).
                let trait_subs = self.trait_bound_arg_subs(&bound);
                self.dispatch_trait_assoc_fn(
                    type_param_name,
                    &trait_method,
                    &trait_subs,
                    args,
                    span,
                )
            }
            _ => {
                let trait_list = candidates
                    .iter()
                    .map(|(b, _)| format!("`{}`", b.path.last().cloned().unwrap_or_default()))
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
            return self.dispatch_trait_assoc_fn(trait_name, &m, &[], args, span);
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
                self.dispatch_trait_assoc_fn("Self", &trait_method, &[], args, span)
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
                Some(self.dispatch_trait_assoc_fn(type_name, &trait_method, &[], args, call_span))
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
    ///
    /// `trait_subs` binds the trait's own generic params from the bound's
    /// args (`C: Reduce[i64]` → `[("T", i64)]`, via
    /// [`Self::trait_bound_arg_subs`]); pass `&[]` at call sites without a
    /// parameterized bound in hand — the Self-only substitution then
    /// applies as before.
    pub(super) fn dispatch_trait_assoc_fn(
        &mut self,
        target: &str,
        method: &crate::ast::TraitMethod,
        trait_subs: &[(String, Type)],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let mut subs: HashMap<String, SubstValue> = HashMap::new();
        subs.insert(
            "Self".to_string(),
            SubstValue::Type(Type::TypeParam(target.to_string())),
        );
        for (name, ty) in trait_subs {
            subs.insert(name.clone(), SubstValue::Type(ty.clone()));
        }

        let mut scope = vec!["Self".to_string()];
        // Trait-level generic params are in scope for the method signature
        // (`fn sum(ref self) -> T` inside `trait Reduce[T]`) — without
        // them, `lower_type_expr` treats `T` as an unknown named type.
        scope.extend(trait_subs.iter().map(|(name, _)| name.clone()));
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

    /// True if `name` is a struct / enum / union known to the env — i.e. a
    /// name that denotes a `Type` pseudovalue for comptime reflection.
    pub(super) fn is_type_name(&self, name: &str) -> bool {
        self.env.structs.contains_key(name)
            || self.env.enums.contains_key(name)
            || self.env.unions.contains_key(name)
    }

    /// The fixed set of comptime `Type`-reflection method names (substrate 2).
    /// `size_of` / `align_of` / `methods` / `attributes` / `generic_args` are
    /// later slices — they need the layout pass / impl-table threading.
    pub(super) fn is_reflection_method(method: &str) -> bool {
        matches!(
            method,
            "name"
                | "is_struct"
                | "is_enum"
                | "is_union"
                | "is_generic"
                | "fields"
                | "variants"
                | "derives"
                | "element_type"
                | "key_type"
                | "value_type"
        )
    }

    /// The `Iterator[T]` adaptor/terminal surface — the exact method set
    /// `infer_iterator_method` (src/typechecker/stdlib_iter.rs) accepts on an
    /// `Iterator` receiver. A direct call of one of these on a `Vec`/`VecDeque`
    /// (no `.iter()` hop) is rejected with an actionable `.iter()` hint rather
    /// than an edit-distance neighbour (B-2026-07-17-12). Keep in sync with the
    /// `require_known_method` list at the tail of `infer_iterator_method`.
    pub(super) fn is_iterator_surface_method(method: &str) -> bool {
        matches!(
            method,
            "all"
                | "any"
                | "chain"
                | "chunk_by"
                | "chunks"
                | "collect"
                | "count"
                | "cycle"
                | "enumerate"
                | "filter"
                | "flat_map"
                | "fold"
                | "for_each"
                | "inspect"
                | "map"
                | "max"
                | "min"
                | "next"
                | "peekable"
                | "product"
                | "reduce"
                | "scan"
                | "skip"
                | "skip_while"
                | "step_by"
                | "sum"
                | "take"
                | "take_while"
                | "windows"
                | "zip"
        )
    }

    /// Result type of a comptime `Type`-reflection method. The caller has
    /// already established the receiver is the `Type` pseudotype and the
    /// method is in [`Self::is_reflection_method`]. Reflection methods take
    /// no arguments — any supplied are inferred (for diagnostics) and an
    /// arity error is emitted. `fields()` → `Vec[Field]`, `variants()` →
    /// `Vec[Variant]` (the built-in record structs registered in
    /// `register_compiler_intrinsic_env`). Spec: deferred.md § Comptime —
    /// Reflection API.
    pub(super) fn infer_type_reflection_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let arg_tys: Vec<Type> = args.iter().map(|arg| self.infer_expr(&arg.value)).collect();
        // `derives(trait_name)` is the one reflection method that takes an
        // argument — a single `String` naming the trait. Every other method
        // is nullary.
        if method == "derives" {
            if arg_tys.len() != 1 {
                self.type_error(
                    "reflection method `derives` takes exactly one argument (the trait name)"
                        .to_string(),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
            } else if !matches!(arg_tys[0], Type::Str | Type::Error) {
                self.type_error(
                    "reflection method `derives` expects a `String` trait name".to_string(),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
            return Type::Bool;
        }
        if !args.is_empty() {
            self.type_error(
                format!("reflection method `{method}` takes no arguments"),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
        }
        let named = |n: &str| Type::Named {
            name: n.to_string(),
            args: vec![],
        };
        let vec_of = |el: Type| Type::Named {
            name: "Vec".to_string(),
            args: vec![el],
        };
        match method {
            "name" => Type::Str,
            "is_struct" | "is_enum" | "is_union" | "is_generic" => Type::Bool,
            "fields" => vec_of(named("Field")),
            "variants" => vec_of(named("Variant")),
            // `element_type()` peels one generic argument (e.g. `Vec[T]` → `T`)
            // and yields it as a `Type` pseudovalue, so a derive can reflect on
            // a repeated field's element. A non-generic type returns itself.
            // `key_type()` / `value_type()` peel the 1st / 2nd argument of a
            // two-parameter type like `Map[K, V]`.
            "element_type" | "key_type" | "value_type" => named("Type"),
            // Unreachable: caller gates on `is_reflection_method`.
            _ => Type::Error,
        }
    }

    pub(super) fn infer_method_call(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        // The closing-paren span of the call (`)` token). A leaf span that is
        // NEVER aliased by an outer expr — unlike `span`, which the parser sets
        // equal to the receiver's span, so the generic `infer_expr` post-record
        // (and any outer chained `MethodCall`) clobbers `expr_types[span]` with
        // the call's RESULT type. Receiver-width-dependent methods whose result
        // type differs from the receiver (`count_ones`/`leading_zeros`/
        // `trailing_zeros` → u32) stash the receiver type here so the
        // interpreter can recover the exact width. See `pow` / the bit-intrinsic
        // arms below.
        args_close_span: &Span,
    ) -> Type {
        // Comptime stdlib modules (substrate 3): `ast.expr(s)` /
        // `compiler.error(msg)` parse as method calls on the lowercase module
        // identifier. Route them to the comptime-module typing (which gates on
        // comptime context) before the receiver is typed as a value.
        if let ExprKind::Identifier(module) = &object.kind {
            if module == "ast" || module == "compiler" {
                if let Some(ret) = self.comptime_module_call_type(module, method, args, span) {
                    self.record_expr_type(span, &ret);
                    return ret;
                }
            }
        }

        // GPU dispatch (spike slice-0c): `gpu.dispatch(kernel, buffer)` parses
        // as a method call on the lowercase magic module `gpu` (registered by
        // the resolver alongside `ast` / `process`). Type it as the element-wise
        // map it is — validate the `#[gpu]` kernel + `Vec[f32]` buffer, bake the
        // WGSL for codegen — before the receiver is typed as a value. `gpu` is a
        // resolver-reserved module name (like `ast` / `process`), so this never
        // shadows a user binding.
        if let ExprKind::Identifier(module) = &object.kind {
            if module == "gpu" && method == "dispatch" {
                return self.infer_gpu_dispatch(args, span);
            }
            // GPU-SLIP-4b-2: resident device buffers. `gpu.upload(vec)` moves a
            // SoA `Vec[S]` to the GPU and returns an owned `GpuBuffer[S]` handle;
            // `gpu.download(buf)` moves the handle back to a `Vec[S]`. Both are
            // magic-module method calls whose arg defaults to a move (the buffer
            // is consumed), matching the owner-decided move semantics.
            if module == "gpu" && method == "upload" {
                return self.infer_gpu_upload(args, span);
            }
            if module == "gpu" && method == "download" {
                return self.infer_gpu_download(args, span);
            }
        }

        // Critical sections (design.md § Critical sections):
        // `critical_section.acquire()` parses as a method call on the
        // lowercase magic module `critical_section` (registered by the
        // resolver alongside `ptr` / `gpu`). Type it as the RAII guard
        // constructor it is — zero args, returns `CriticalSectionGuard` —
        // before the receiver is typed as a value. Skipped when a local
        // binding shadows `critical_section` (prelude-shadow rule, mirroring
        // the `ptr` guard below).
        if let ExprKind::Identifier(module) = &object.kind {
            if module == "critical_section"
                && method == "acquire"
                && self.local_scope.lookup("critical_section").is_none()
            {
                return self.infer_critical_section_acquire(args, span);
            }
        }

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
                let base_ret = self.infer_method_call(object, base, args, span, args_close_span);
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

        // Record the result type `T` of a `TaskHandle[T].join()` so codegen
        // sizes the cross-task result transfer for a NON-scalar `T` (a
        // `Vec`/`String`/struct spawn return). Without this the join lowering
        // reads `i64`-shaped bytes from the runtime result buffer, so a heap
        // return comes back as garbage and traps. `infer_expr(object)` is
        // idempotent for an already-typechecked receiver (re-asserts its
        // expr_types entry), so this read has no downstream side effect; the
        // normal generic-impl dispatch below still computes the call's type.
        // Mirrors the `spawn` intercept above and the channel-elem recording
        // in `stdlib_io.rs`.
        if method == "join" && args.is_empty() {
            let recv_ty = self.infer_expr(object);
            let handle_inner = match &recv_ty {
                Type::Named { name, args: targs } if name == "TaskHandle" && targs.len() == 1 => {
                    Some(targs[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args: targs }
                        if name == "TaskHandle" && targs.len() == 1 =>
                    {
                        Some(targs[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(t) = handle_inner {
                let resolved = resolve_type_var_top(&t, &self.env.substitutions);
                let te = Self::type_to_type_expr(&resolved);
                self.task_join_return_types
                    .insert(SpanKey::from_span(span), te);
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
                // Comptime `Type` reflection (substrate 2): `MyType.name()`,
                // `.fields()`, `.variants()`, `.is_struct()`, … The reflection
                // API is fixed by the language and only valid at comptime (a
                // `Type` value cannot exist at runtime). Reserve the reflection
                // method names when inside a comptime context; outside it, fall
                // through so an identically-named user associated fn still
                // resolves. Spec: deferred.md § Comptime — Reflection API.
                if Self::is_reflection_method(method) && self.comptime_depth > 0 {
                    let ty = self.infer_type_reflection_method(method, args, span);
                    self.record_expr_type(span, &ty);
                    return ty;
                }
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
                // Numeric narrowing / sign-changing `T.try_from(x: <int>) ->
                // Result[T, String]` for an integer target `T` (design.md
                // § Conversion Traits — "fails if out of range"). Dispatch
                // mirrors the `from` arm above: the registered built-in TryFrom
                // impls (env_build) disambiguate on the source type, and the
                // arm returns the impl's `Result[T, String]` return type. `char`
                // has its own `try_from` arm above; refinement / distinct-type
                // `try_from` target their own names, so this only fires for the
                // primitive integer targets and never shadows them.
                if method == "try_from"
                    && matches!(
                        type_name.as_str(),
                        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    )
                {
                    if args.len() != 1 {
                        self.type_error(
                            format!(
                                "{}.try_from expects 1 argument, got {}",
                                type_name,
                                args.len()
                            ),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        return Type::Error;
                    }
                    let arg_ty = self.infer_expr(&args[0].value);
                    if arg_ty == Type::Error {
                        return Type::Error;
                    }
                    if let Some(imp) = self.env.find_tryfrom_impl(&arg_ty, type_name, &[]) {
                        return imp
                            .methods
                            .get("try_from")
                            .map(|sig| sig.return_type.clone())
                            .unwrap_or(Type::Error);
                    }
                    self.type_error(
                        format!(
                            "`{}.try_from` expects an integer argument, got `{}`",
                            type_name,
                            type_display(&arg_ty)
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

        // Raw-pointer instance methods (design.md § raw pointers; additive-
        // interop Slice 4 Path A). Record the receiver's pointee keyed by
        // the call span BEFORE the method's result type overwrites the
        // receiver's `*T` entry at the same (collided) span key — codegen
        // needs the receiver pointee for the GEP/load/store, and `.read` /
        // `.write` results (`T` / unit) are not pointers. Mirrors the
        // `vector_method_receivers` fix for the same span collision.
        // Raw-pointer inherent methods (`*const T` / `*mut T`) — design.md
        // § "Method dispatch on raw pointers requires a known pointee". These are
        // inherent methods on the pointer type itself (no auto-deref to `T`), so
        // they must be resolved here BEFORE the generic dispatch below (which
        // would return `Type::Error` for a pointer receiver, silently degrading a
        // `let p1 = p.offset(1)` intermediate to `Error` and breaking every
        // downstream `p1.read()` — B-fixed here). `unsafe { }` is enforced
        // separately by `unsafe_lint`; the pointee side-table feeds codegen.
        if let Type::Pointer { inner, is_mut } = &obj_ty {
            let is_mut = *is_mut;
            let inner_ty = (**inner).clone();
            // Record the pointee for every raw-pointer method so codegen can
            // (a) identify the receiver as a raw pointer — the method-call span
            // equals the receiver span, and the call's result type overwrites the
            // receiver's `*T` entry in `expr_types`, so this side-table is how
            // `compile_pointer_instance_method` recovers the pointer-ness — and
            // (b) size its typed load/store/GEP. `is_null` ignores the pointee in
            // its lowering (a null-bits check) and stays SAFE (no `unsafe { }`,
            // matching `ptr.is_null(p)`), but still records it for (a).
            let is_ptr_method = matches!(
                method,
                "offset"
                    | "add"
                    | "read"
                    | "read_unaligned"
                    | "read_volatile"
                    | "write"
                    | "write_unaligned"
                    | "write_volatile"
                    | "is_null"
            );
            if is_ptr_method {
                let pointee = Self::type_to_type_expr(&inner_ty);
                self.pointer_method_receiver_pointees
                    .insert(SpanKey::from_span(span), pointee);
            }
            // `E_RAW_POINTER_UNRESOLVED_POINTEE` (design.md § "Method dispatch on
            // raw pointers requires a known pointee"). A size/stride-dependent
            // method (`read`/`write`/`offset`/`add` + unaligned/volatile) cannot
            // be lowered when `T` is unresolved — the load/store width and GEP
            // stride depend on it. `is_null` is EXEMPT (a null-bits check reads no
            // pointee, so it accepts an unresolved `T`). A generic parameter `T`
            // that is IN SCOPE (`fn f[T](p: *const T) { p.read() }`) is resolved
            // per-instantiation at monomorphization and does NOT fire — that is
            // exactly what `find_unbound_type_param` (which consults the enclosing
            // generic bounds) distinguishes from an un-pinned metavariable.
            // Emitted at the RECEIVER span (the user's question is "what type is
            // `p`?"), and returns `Type::Error` to suppress the cascade (the
            // binding-level infer error is already suppressed for the pointer
            // construction — see `check_unsolved_type_param`).
            let sized_ptr_op = matches!(
                method,
                "offset"
                    | "add"
                    | "read"
                    | "read_unaligned"
                    | "read_volatile"
                    | "write"
                    | "write_unaligned"
                    | "write_volatile"
            );
            if sized_ptr_op {
                let unresolved: Option<String> = {
                    let in_scope: std::collections::HashSet<&str> =
                        self.enclosing_bounds.keys().map(|s| s.as_str()).collect();
                    super::inference::find_unbound_type_param(&inner_ty, &in_scope)
                        .map(|s| s.to_string())
                };
                if let Some(pointee_name) = unresolved {
                    self.type_error(
                        format!(
                            "error[E_RAW_POINTER_UNRESOLVED_POINTEE]: method '{method}' on a \
                             raw pointer requires a known pointee type; the pointee type \
                             '{pointee_name}' is unresolved at this call site. Annotate the \
                             pointer's declared type (e.g. `let p: *const u8 = ...`), or pin it \
                             with a turbofish on the originating constructor (e.g. \
                             `ptr.null[u8]()`)."
                        ),
                        object.span.clone(),
                        TypeErrorKind::CannotInferTypeParam,
                    );
                    return Type::Error;
                }
            }
            let arg_count_ok = |s: &mut Self, want: usize| -> bool {
                if args.len() != want {
                    s.type_error(
                        format!(
                            "'{method}' on a raw pointer takes {want} argument{}",
                            if want == 1 { "" } else { "s" }
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    return false;
                }
                true
            };
            match method {
                // `p.offset(n) / p.add(n) -> *_ T` — same pointee + mutability.
                "offset" | "add" => {
                    if arg_count_ok(self, 1) {
                        self.check_expr(&args[0].value, &Type::Int(IntSize::I64));
                    }
                    return Type::Pointer {
                        is_mut,
                        inner: Box::new(inner_ty),
                    };
                }
                // `p.read() / read_unaligned() / read_volatile() -> T`.
                "read" | "read_unaligned" | "read_volatile" => {
                    arg_count_ok(self, 0);
                    return inner_ty;
                }
                // `p.write(v) / write_unaligned(v) / write_volatile(v) -> Unit`,
                // with `v: T`.
                "write" | "write_unaligned" | "write_volatile" => {
                    if arg_count_ok(self, 1) {
                        self.check_expr(&args[0].value, &inner_ty);
                    }
                    return Type::Unit;
                }
                // `p.is_null() -> bool` — the method-form of `ptr.is_null(p)`
                // (design.md § raw pointers). SAFE and pointee-agnostic.
                "is_null" => {
                    arg_count_ok(self, 0);
                    return Type::Bool;
                }
                _ => {}
            }
        }

        // Comptime `Type` reflection on a `Type`-typed *value* receiver — a
        // binding or `comptime T: Type` parameter holding a type value
        // (`let t = MyStruct; t.fields()`, or `T.fields()` inside a
        // `comptime fn f(comptime T: Type)`). The bare `TypeName.method()`
        // form is handled by the associated-call intercept above; this arm
        // covers the case where the receiver is a value of the `Type`
        // pseudotype. Substrate 2.
        if matches!(&obj_ty, Type::Named { name, .. } if name == "Type") {
            let ty = self.infer_type_reflection_method(method, args, span);
            self.record_expr_type(span, &ty);
            return ty;
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
                "unwrap"
                    | "expect"
                    | "is_some"
                    | "is_none"
                    | "is_ok"
                    | "is_err"
                    | "unwrap_or"
                    | "unwrap_err"
                    | "expect_err"
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
        // Type-param receiver (`x.m()` where `x: T` inside a generic body):
        // `method_callee_type_name(TypeParam)` is None, so no concrete callee is
        // recorded above. Record the type-param NAME separately so the
        // interpreter can resolve it through its runtime type-subs stack and
        // dispatch a bound-trait method on the concrete instantiation — the
        // width-erased `Value::Int`/`Value::Float` cannot recover the declared
        // primitive width on its own (B-2026-07-03-24).
        if let Type::TypeParam(pname) = &obj_ty {
            self.method_typeparam_receiver
                .insert(SpanKey::from_span(span), pname.clone());
        }

        // General owned-temp tracking, slice 3b — element-type-aware read
        // methods (`get`/`first`/`last`/`get_unchecked`/`contains`) on a
        // FRESH-TEMP (non-identifier) `Vec`/`VecDeque` receiver
        // (`make_vec().get(0)`). Codegen materializes the temp into a synthetic
        // local and re-dispatches through `compile_vec_method`, which needs the
        // receiver's ELEMENT type to shape the `Option[T]` payload — but it
        // cannot recover it from `expr_types` because `MethodCall.span ==
        // receiver.span` holds the method's *result* type (`Option[T]`), not the
        // receiver's `Vec[T]`. Record the element `TypeExpr` here (where
        // `obj_ty` is the receiver type), keyed by the call span — the same
        // collision dodge `method_unwrap_inner_types` / `method_callee_types`
        // use. Gated to `Call`/`MethodCall` receivers — the fresh-temp shapes
        // codegen's `expr_yields_fresh_owned_temp` recognizes; a place-expression
        // receiver (identifier / field / index) is owned elsewhere and routes
        // through the named-binding dispatch.
        //
        // Element scope: SCALAR elements service all five read methods — a
        // scalar element owns no nested heap, so the single outer
        // `FreeVecBuffer` is the complete, double-free-free drop. STRING
        // elements (slice 3b-heap) service the borrow-returning
        // `get`/`first`/`last` plus `contains`:
        //   - `get`/`first`/`last` return `Option[ref String]` aliasing an
        //     element inside the soon-freed temp buffer, but
        //     `scrutinee_is_borrow_call` (receiver-shape-agnostic — it keys off
        //     the *method*, not the object) already suppresses the `Some(s)`
        //     arm binding's independent drop, and the `FreeVecBuffer` vec-struct
        //     recursion frees each per-element String buffer, so the borrow is
        //     the sole reader of storage freed exactly once at frame exit.
        //   - `contains` returns `bool` — no borrow escapes, so there is no
        //     aliasing/suppression obligation at all; it only needs the receiver
        //     temp per-element freed, which the same `FreeVecBuffer` recursion
        //     does. The compared arg is borrowed, not consumed (the named
        //     `Vec[String].contains` path already does element `==` via memcmp
        //     without freeing the arg); a *fresh-owned* arg (`contains(make_str())`)
        //     is the separate 3b-c operand-temp leak, out of scope here.
        // `get_unchecked` (bare `ref String` via a let-binding suppression path
        // that doesn't cover builtin methods, and it needs an `unsafe` block)
        // stays scalar-only — a distinct follow-on. Other heap elements
        // (`Vec[T]`, user struct/enum, Map/Set) need element-drop threading
        // (`elem_agg_drop`) the helper doesn't carry — also follow-ons.
        if matches!(
            &object.kind,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. }
        ) {
            let elem = match &obj_ty {
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque") && args.len() == 1 =>
                {
                    Some(args[0].clone())
                }
                _ => None,
            };
            if let Some(elem) = elem {
                let resolved = resolve_type_var_top(&elem, &self.env.substitutions);
                let is_scalar = matches!(
                    resolved,
                    Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
                );
                // An owned `String` element resolves to `Type::Str` here (the
                // checker's owned-string representation); a `Type::Named` form
                // is accepted too for robustness. Both `type_to_type_expr` to a
                // `str`/`String` `TypeExpr` that `llvm_type_for_type_expr`
                // lowers to `vec_struct_type`, so the `FreeVecBuffer` vec-struct
                // recursion per-element frees each element's buffer.
                let is_string = matches!(&resolved, Type::Str)
                    || matches!(
                        &resolved,
                        Type::Named { name, args } if name == "String" && args.is_empty()
                    );
                // A one-level nested `Vec[scalar]` / `VecDeque[scalar]` element
                // (`Vec[Vec[i64]]` — matrices, adjacency lists). The element is a
                // `vec_struct_type`, so the `FreeVecBuffer` vec-struct recursion
                // (the documented `Vec[Vec[T]]` one-level path) per-element frees
                // each inner POD buffer, and `get`/`first`/`last` return
                // `Option[ref Vec[scalar]]` — a borrow `scrutinee_is_borrow_call`
                // suppresses. INNER must be scalar: a `Vec[Vec[String]]` would
                // leak the innermost String buffers (two-level nesting exceeds the
                // one-level recursion) — excluded. `contains` (Vec content-eq) and
                // `get_unchecked` stay out for nested Vec.
                let is_pod_vec = matches!(
                    &resolved,
                    Type::Named { name, args }
                        if (name == "Vec" || name == "VecDeque")
                            && args.len() == 1
                            && matches!(
                                resolve_type_var_top(&args[0], &self.env.substitutions),
                                Type::Int(_)
                                    | Type::UInt(_)
                                    | Type::Float(_)
                                    | Type::Bool
                                    | Type::Char
                            )
                );
                // A user-defined STRUCT element (`Vec[Rec]`, `Rec` carrying a
                // `String`/`Vec`/`shared` field). Unlike scalar/String/nested-Vec
                // — whose element either has no destructor or reuses the
                // `vec_struct_type` recursion — a struct element needs its
                // synthesized per-element `__karac_drop_<S>` threaded into the
                // `FreeVecBuffer` (codegen's `vec_elem_agg_drop_for_type_expr` +
                // `track_vec_of_aggs_var`). `get`/`first`/`last` return
                // `Option[ref Rec]`, a borrow `scrutinee_is_borrow_call`
                // suppresses, so each element's heap fields are freed once at
                // frame exit while the borrow reads it. A user ENUM element
                // (`Vec[Tok]`, `Tok` a variant carrying a `String`/`Vec`/shared
                // payload) rides the SAME machinery:
                // `vec_elem_agg_drop_for_type_expr` already routes a non-shared
                // enum to `emit_enum_drop_switch` (and a `shared enum` to a
                // per-element rc-dec), so the per-element drop is threaded
                // identically — no new codegen mechanism. Still excluded:
                // `contains` (enum content-eq) and `get_unchecked`.
                let is_user_struct = matches!(
                    &resolved,
                    Type::Named { name, args } if args.is_empty() && self.env.structs.contains_key(name)
                );
                let is_user_enum = matches!(
                    &resolved,
                    Type::Named { name, args } if args.is_empty() && self.env.enums.contains_key(name)
                );
                let record = (is_scalar
                    && matches!(
                        method,
                        "get" | "first" | "last" | "get_unchecked" | "contains"
                    ))
                    || (is_string && matches!(method, "get" | "first" | "last" | "contains"))
                    || (is_pod_vec && matches!(method, "get" | "first" | "last"))
                    || (is_user_struct && matches!(method, "get" | "first" | "last"))
                    || (is_user_enum && matches!(method, "get" | "first" | "last"))
                    // `for x in make_vec().iter()` / `.into_iter()` — a fresh-temp
                    // receiver iterated in a for-loop. The element type drives the
                    // same materialize-iterate-drop path as the read methods, but
                    // here the for-loop peels `.iter()` and recurses on the
                    // receiver: at the collided MethodCall span `expr_types` holds
                    // `Iterator[T]` (clobbering the receiver's `Vec[T]`), so
                    // `owned_temp_drops` has no entry and the loop body is silently
                    // skipped (output 0 vs the interpreter). Recording the element
                    // span-keyed lets codegen reconstruct `Vec[elem]`. Every
                    // element shape above is supported (scalar/String/POD-Vec/user
                    // struct/user enum) — the for-loop reuses the read-method
                    // cleanup threading verbatim.
                    || ((is_scalar
                        || is_string
                        || is_pod_vec
                        || is_user_struct
                        || is_user_enum)
                        && matches!(method, "iter" | "into_iter"));
                if record {
                    let te = Self::type_to_type_expr(&resolved);
                    self.temp_recv_elem_types
                        .insert(SpanKey::from_span(span), te);
                }
            }
        }

        // Sibling of the Vec block above for `Map`/`Set` fresh-temp receivers
        // (`make_map().get(k)`, `make_set().contains(x)`): record the receiver's
        // whole `Map[K,V]` / `Set[T]` type — codegen needs K+V to redispatch
        // through `compile_map_method` and to classify the handle's
        // `FreeMapHandle` drop, so a single element type doesn't suffice. Same
        // `Call`/`MethodCall` fresh-temp gate. Scalar K/V/elem only: `Map.get`
        // returns `Option[ref V]` (a borrow the receiver-shape-agnostic
        // `scrutinee_is_borrow_call` already suppresses), and a scalar V owns no
        // nested heap, so the single `FreeMapHandle` is the complete drop;
        // `contains_key`/`contains` return `bool` (no borrow). Heap K/V (per-entry
        // String/Vec drop) is a follow-on.
        if matches!(
            &object.kind,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. }
        ) {
            let subs = &self.env.substitutions;
            // Scalar OR owned `String` (which resolves to `Type::Str` here, as in
            // the Vec[String] slice). A String K/V makes the handle's
            // `FreeMapHandle` per-entry drop the element buffers
            // (`map_temp_cleanup_parts` classifies `key_is_vec`/`val_is_vec` from
            // the type), and a `Map[_, String].get` returns `Option[ref String]`
            // whose arm binding is suppressed by `scrutinee_is_borrow_call` — the
            // same single-free shape the `Vec[String]` slice established. Other
            // heap K/V (`Vec[T]`, user struct/enum, nested Map) are excluded —
            // they need element-drop threading the helper doesn't carry.
            let is_scalar_or_string = |t: &Type| {
                let r = resolve_type_var_top(t, subs);
                matches!(
                    r,
                    Type::Int(_)
                        | Type::UInt(_)
                        | Type::Float(_)
                        | Type::Bool
                        | Type::Char
                        | Type::Str
                ) || matches!(&r, Type::Named { name, args } if name == "String" && args.is_empty())
            };
            // `iter` is recorded for the for-loop temp path (`for (k, v) in
            // make_map().iter()`): the for-loop peels `.iter()`, recurses on the
            // receiver, and codegen's `try_compile_for_mapset_value` reconstructs
            // the handle from this side-table (the collided `.iter()` span holds
            // `Iterator[(K,V)]` in `expr_types`, so `owned_temp_drops` misses).
            // Same scalar/String K/V constraint as `get` — the `FreeMapHandle`
            // per-entry drop only frees scalar/String entries.
            //
            // `keys` / `values` / `entries` materialize a fresh `Vec[K]` /
            // `Vec[V]` / `Vec[(K,V)]` and take the same fresh-temp Map path
            // (codegen re-dispatches through `compile_map_method` →
            // `compile_map_keys_values_entries`, which CLONES each scalar/String
            // element into the result Vec, so freeing the map handle afterward —
            // `track_map_var` — never dangles the returned Vec). The returned Vec
            // is owned by the enclosing binding / for-loop like any collection
            // method result — `entries` needs no extra tuple-element handling
            // here: its `Vec[(K,V)]` result drop is the SAME machinery the
            // named-map `let es: Vec[(i64,String)] = m.entries()` path already
            // uses; only the Map RECEIVER temp needs this side-table so codegen
            // recognizes the fresh-temp shape at all.
            let record = match &obj_ty {
                Type::Named { name, args }
                    if name == "Map"
                        && args.len() == 2
                        && matches!(
                            method,
                            "get" | "contains_key" | "iter" | "keys" | "values" | "entries"
                        )
                        && is_scalar_or_string(&args[0])
                        && is_scalar_or_string(&args[1]) =>
                {
                    Some(Type::Named {
                        name: "Map".to_string(),
                        args: vec![
                            resolve_type_var_top(&args[0], subs),
                            resolve_type_var_top(&args[1], subs),
                        ],
                    })
                }
                Type::Named { name, args }
                    if name == "Set"
                        && args.len() == 1
                        && matches!(method, "contains" | "iter")
                        && is_scalar_or_string(&args[0]) =>
                {
                    Some(Type::Named {
                        name: "Set".to_string(),
                        args: vec![resolve_type_var_top(&args[0], subs)],
                    })
                }
                _ => None,
            };
            if let Some(resolved_recv) = record {
                let te = Self::type_to_type_expr(&resolved_recv);
                self.temp_recv_mapset_types
                    .insert(SpanKey::from_span(span), te);
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
            "unwrap"
                | "expect"
                | "is_some"
                | "is_none"
                | "is_ok"
                | "is_err"
                | "unwrap_or"
                | "unwrap_err"
                | "expect_err"
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
                // `unwrap_err` / `expect_err` extract the ERR payload of a
                // `Result[T, E]`, so their reconstituted inner type is `E` (the
                // SECOND type arg), not `T`. Every other family member (incl. the
                // uniform `is_*` recording) uses the first arg. `_err` is not a
                // valid method on `Option` (no Err half).
                let inner_ty = if matches!(method, "unwrap_err" | "expect_err") {
                    if name == "Result" {
                        args.get(1).cloned()
                    } else {
                        None
                    }
                } else {
                    match name.as_str() {
                        "Option" | "Result" => args.first().cloned(),
                        _ => None,
                    }
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
                        "unwrap" | "expect" | "unwrap_or" | "unwrap_err" | "expect_err" => resolved,
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

        // `Array[T, N].as_ptr()` / `.as_mut_ptr()` and `Vec[T].as_ptr()` /
        // `.as_mut_ptr()` — raw element-0 pointer producers (the language's
        // FFI handoff; mirrors `CStr.as_ptr`). `as_ptr -> *const T`,
        // `as_mut_ptr -> *mut T`. The codegen handler GEPs element 0 of the
        // array storage / loads the `Vec` header's data field. Without a
        // precise arm here the call falls through to the permissive
        // array/vec-method path and binds `Type::Error`, losing the pointer
        // type for downstream FFI / deref. Handles owned arrays + `Vec[T]`
        // and their `ref` / `mut ref` borrows. The `Vec` arm is what feeds a
        // heap buffer (e.g. a framebuffer) to a `host fn` blit — an
        // `Array[u8, N]` of framebuffer size would overflow the wasm stack.
        if (method == "as_ptr" || method == "as_mut_ptr") && args.is_empty() {
            let vec_elem = |t: &Type| -> Option<Type> {
                match t {
                    Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                }
            };
            let elem = match &obj_ty {
                Type::Array { element, .. } => Some(*element.clone()),
                Type::Named { .. } => vec_elem(&obj_ty),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Array { element, .. } => Some(*element.clone()),
                    other => vec_elem(other),
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

        // Fixed-size `Array[T, N]` read-only method surface for a SCALAR
        // element (B-2026-07-17-19). A fixed array is a structural
        // `Type::Array`, not `Type::Named`, so it otherwise falls through to
        // the silent `Type::Error` catch-all — which typechecked `a.get(0)` /
        // `a.contains(x)` clean and then either ran only under the interpreter
        // (which dispatches a fixed array as a Vec) or BUILD-FAILED under AOT.
        // Model exactly the subset both backends now run (`compile_fixed_array_
        // read` provides the matching codegen): `len`/`is_empty`/`get`/`first`/
        // `last`/`contains`. `iter`/`into_iter`/`as_slice`/`as_ptr` are handled
        // by their own arms (above / the iterator-source arm below), so they are
        // deliberately excluded here to fall through to them; the wider Vec
        // surface (`to_vec`/`slice`/`rev`/iterator adaptors) is NOT modelled and
        // is rejected at the structural fall-through. Non-scalar element arrays
        // (String/Vec/struct elements) need heap/borrow handling the codegen
        // arm does not provide, so they are excluded and stay rejected.
        if matches!(
            method,
            "len" | "is_empty" | "get" | "first" | "last" | "contains"
        ) {
            let array_elem = match &obj_ty {
                Type::Array { element, .. } => Some((**element).clone()),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Array { element, .. } => Some((**element).clone()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = array_elem {
                if matches!(
                    elem,
                    Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
                ) {
                    let option_elem = Type::Named {
                        name: "Option".to_string(),
                        args: vec![elem.clone()],
                    };
                    match method {
                        "len" => {
                            if !args.is_empty() {
                                self.type_error(
                                    "Array.len() takes no arguments".to_string(),
                                    span.clone(),
                                    TypeErrorKind::WrongNumberOfArgs,
                                );
                            }
                            return Type::Int(IntSize::I64);
                        }
                        "is_empty" => {
                            if !args.is_empty() {
                                self.type_error(
                                    "Array.is_empty() takes no arguments".to_string(),
                                    span.clone(),
                                    TypeErrorKind::WrongNumberOfArgs,
                                );
                            }
                            return Type::Bool;
                        }
                        "first" | "last" => {
                            if !args.is_empty() {
                                self.type_error(
                                    format!("Array.{}() takes no arguments", method),
                                    span.clone(),
                                    TypeErrorKind::WrongNumberOfArgs,
                                );
                            }
                            return option_elem;
                        }
                        "get" => {
                            for arg in args {
                                let at = self.infer_expr(&arg.value);
                                self.check_assignable(
                                    &Type::Int(IntSize::I64),
                                    &at,
                                    arg.value.span.clone(),
                                );
                            }
                            return option_elem;
                        }
                        "contains" => {
                            for arg in args {
                                let at = self.infer_expr(&arg.value);
                                self.check_assignable(&elem, &at, arg.value.span.clone());
                            }
                            return Type::Bool;
                        }
                        _ => unreachable!(),
                    }
                }
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
            "iter_axis" | "reshape" | "permute" | "slice" | "squeeze" | "transpose" | "matmul"
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
                // Record the receiver's ELEMENT type keyed by the reduction call
                // span so codegen can reduce over a NON-IDENTIFIER receiver — a
                // tensor-producing chain like `a.zip_with(b, f).sum()`
                // (B-2026-07-13-5 legs A/C). `MethodCall.span == receiver.span`
                // collapses the sum/zip_with/`a` spans into one, so
                // `expr_types` (hence `tensor_typed_exprs`) at that span holds
                // the OUTERMOST scalar reduce result, not the intermediate
                // Tensor — the element type is unrecoverable from the span
                // otherwise. Reuses `temp_recv_elem_types` (the fresh-temp
                // non-identifier collection-receiver element-type table); the
                // by-name codegen path ignores it (it uses `tensor_var_infos`),
                // so recording unconditionally is harmless. Only the scalar
                // full reductions codegen wires (`sum`/`mean`/`prod`/`min`/`max`)
                // are recorded; `sum_axis`/`mean_axis` (tensor result) stay on
                // the by-name path.
                if matches!(method, "sum" | "mean" | "prod" | "min" | "max")
                    && !tensor_args.is_empty()
                {
                    let elem_te = Self::type_to_type_expr(&tensor_args[0]);
                    self.temp_recv_elem_types
                        .insert(SpanKey::from_span(span), elem_te);
                }
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

        // Column[T] result-typed methods (phase-11 Arrow): `iter` ->
        // `Vec[Option[T]]`, `iter_valid` -> `Vec[T]`, `fillna(value)` /
        // `dropna` -> `Column[T]`. Their result type mentions the impl
        // type-param `T`, which baked-signature dispatch doesn't bind from
        // the receiver, so it's computed here (binding `T` from the
        // receiver's element type) — and `iter` must intercept *before* the
        // generic `iter()` iterator-source handler just below would claim
        // it. `len`/`null_count`/`valid_count`/`is_null`/`push`/`push_null`
        // keep flowing through normal baked dispatch (their result types are
        // concrete).
        if matches!(method, "iter" | "iter_valid" | "fillna" | "dropna") {
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
            if let Some(elem) = column_elem {
                // `fillna` takes the fill `value` plus an optional
                // `treat_nan_as_null: bool` (1 or 2 args, the flag default
                // `false`); the rest take none.
                if method == "fillna" {
                    if args.is_empty() || args.len() > 2 {
                        self.type_error(
                            format!("fillna expects 1 or 2 argument(s), got {}", args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        return Type::Error;
                    }
                } else if !args.is_empty() {
                    self.type_error(
                        format!("{method} expects 0 argument(s), got {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    return Type::Error;
                }
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                // The `treat_nan_as_null` flag (labeled, or the 2nd
                // positional arg) must be a bool — the only statically
                // checkable arg, since the fill `value`'s type `T` isn't
                // bound from the receiver for baked generic methods.
                if method == "fillna" {
                    if let Some(flag) = args
                        .iter()
                        .find(|a| a.label.as_deref() == Some("treat_nan_as_null"))
                        .or_else(|| args.iter().filter(|a| a.label.is_none()).nth(1))
                    {
                        let flag_ty = self.infer_expr(&flag.value);
                        self.check_assignable(&Type::Bool, &flag_ty, flag.value.span.clone());
                    }
                }
                let vec_of = |inner: Type| Type::Named {
                    name: "Vec".to_string(),
                    args: vec![inner],
                };
                return match method {
                    "iter" => vec_of(Type::Named {
                        name: "Option".to_string(),
                        args: vec![elem],
                    }),
                    "iter_valid" => vec_of(elem),
                    // fillna / dropna
                    _ => Type::Named {
                        name: "Column".to_string(),
                        args: vec![elem],
                    },
                };
            }
        }

        // `Column[T]` / `Tensor[T, ...S]` `.fold[A](init: A, f: Fn(A, T) -> A)
        // -> A` — the general left-fold primitive. `A` is inferred from `init`
        // (concrete after `infer_expr`), so the closure params `(A, T)` and its
        // return `A` are all concrete and `check_expr` drives closure-param
        // pushdown (same shape as `Iterator.fold`). `T` is the receiver's
        // element (`Column[T]` → the sole arg; `Tensor[T, ...S]` → the leading
        // arg). Typed here because baked generic dispatch can't bind `A` from
        // an argument nor thread the receiver's `T` into the closure signature.
        if method == "fold" {
            // The element `T` and the container's display name, for either
            // handle-backed reducer.
            let fold_receiver = |ty: &Type| -> Option<(Type, &'static str)> {
                match ty {
                    Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                        Some((args[0].clone(), "Column"))
                    }
                    Type::Named { name, args } if name == "Tensor" && !args.is_empty() => {
                        Some((args[0].clone(), "Tensor"))
                    }
                    _ => None,
                }
            };
            let mut elem_and_kind = match &obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => fold_receiver(inner),
                other => fold_receiver(other),
            };
            // Bound-generic receiver (`c: ref C` where `C: Reduce[T]`): the
            // element is the trait bound's argument. Falls into the SAME
            // A-from-init + closure-pushdown below, so `fold` on a `Reduce`-
            // bounded generic type-checks (S6c); the mono'd receiver routes to
            // the inline-closure kernel exactly as `sum`/`max` do. Interp
            // dispatches on the concrete `Column`/`Tensor` Value at runtime.
            if elem_and_kind.is_none() {
                if let Some(pname) = Self::receiver_type_param_name(&obj_ty) {
                    if let Some(elem) = self.bound_element_for_trait(&pname, "Reduce") {
                        elem_and_kind = Some((elem, "Reduce"));
                    }
                }
            }
            if let Some((elem, container)) = elem_and_kind {
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "{container}.fold expects 2 arguments (init, closure), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                let acc_ty = self.infer_expr(&args[0].value);
                let f_ty = Type::Function {
                    params: vec![acc_ty.clone(), elem],
                    return_type: Box::new(acc_ty.clone()),
                };
                self.check_expr(&args[1].value, &f_ty);
                return acc_ty;
            }
        }

        // `Column[T]` / `Tensor[T, ...S]` `.map(|x| ...) -> Self` — the
        // element-wise map surface (S6c-2, the `ElementwiseMap` trait's `map`).
        // Same element type first cut (`Fn(T) -> T`), so the result is the
        // receiver's own container type. Typed here (like `fold`) because the
        // closure's parameter `T` is the receiver's element, which baked
        // generic dispatch can't thread into the closure signature.
        if method == "map" {
            // `Option[T].map(f)` / `Result[T, E].map(f)`: apply `f: Fn(T) -> R`
            // to the present payload, yielding `Option[R]` / `Result[R, E]`
            // (an absent receiver passes through). Push `T` into the closure
            // param so an un-annotated `|x| ..` infers, read `R` back from the
            // solved return type, and record the SOURCE inner `T` in
            // `method_unwrap_inner_types` for codegen payload reconstruction.
            // design.md documents `.map` on Result as intended; previously this
            // fell through to a permissive fallback that typechecked but had no
            // runtime dispatch in either backend (B-2026-07-12-11).
            let optres_map = |ty: &Type| -> Option<(&'static str, Type, Option<Type>)> {
                match ty {
                    Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                        Some(("Option", args[0].clone(), None))
                    }
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                        Some(("Result", args[0].clone(), Some(args[1].clone())))
                    }
                    _ => None,
                }
            };
            let optres_recv = match &obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => optres_map(inner),
                other => optres_map(other),
            };
            if let Some((enum_name, t_ty, e_ty)) = optres_recv {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "{enum_name}.map expects 1 argument (closure), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                // Infer the mapper's type. For a fn-reference or an ANNOTATED
                // closure this yields a concrete `Fn(T') -> R`; seed the param
                // from `T` (so an un-annotated typevar param picks it up) and
                // read `R` as the result payload type. `check_expr` with a
                // fresh return typevar can't be used here — `check_assignable`
                // is subtyping, not unification, so it never solves the return
                // var. (Fully inferring an un-annotated `|x| ..` param from `T`
                // is the separate closure-param-inference gap B-2026-07-12-10.)
                let f_actual = self.infer_expr(&args[0].value);
                let f_resolved = resolve_type_var_top(&f_actual, &self.env.substitutions);
                let r_resolved = match &f_resolved {
                    Type::Function {
                        params,
                        return_type,
                    }
                    | Type::OnceFunction {
                        params,
                        return_type,
                    } => {
                        if let Some(p0) = params.first() {
                            unify_types(
                                p0,
                                &t_ty,
                                &mut self.env.substitutions,
                                &mut self.env.const_substitutions,
                            );
                        }
                        resolve_type_var_top(return_type, &self.env.substitutions)
                    }
                    _ => {
                        self.type_error(
                            format!(
                                "{enum_name}.map expects a function argument, got '{}'",
                                type_display(&f_resolved)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                };
                let t_resolved = resolve_type_var_top(&t_ty, &self.env.substitutions);
                // Codegen reconstructs the receiver's payload from these words
                // to feed the mapper; the RESULT `R` is read off the mapper's
                // compiled SSA value, so only the SOURCE `T` needs recording.
                self.method_unwrap_inner_types.insert(
                    SpanKey::from_span(span),
                    Self::type_to_type_expr(&t_resolved),
                );
                let result = if enum_name == "Option" {
                    Type::Named {
                        name: "Option".to_string(),
                        args: vec![r_resolved],
                    }
                } else {
                    Type::Named {
                        name: "Result".to_string(),
                        args: vec![r_resolved, e_ty.unwrap_or(Type::Error)],
                    }
                };
                self.record_expr_type(span, &result);
                return result;
            }
            // (element `T`, the owned `Self` container type, display name).
            let map_receiver = |ty: &Type| -> Option<(Type, Type, &'static str)> {
                match ty {
                    Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                        Some((args[0].clone(), ty.clone(), "Column"))
                    }
                    Type::Named { name, args } if name == "Tensor" && !args.is_empty() => {
                        Some((args[0].clone(), ty.clone(), "Tensor"))
                    }
                    _ => None,
                }
            };
            let mut recv = match &obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => map_receiver(inner),
                other => map_receiver(other),
            };
            // Bound-generic receiver (`c: ref C` where `C: ElementwiseMap[T]`):
            // `map` returns `Self = C`, and the closure param `T` is the bound's
            // element. Mono routes the receiver to the inline-closure kernel
            // (which allocates a fresh `Self`) exactly as the concrete surface
            // does; interp dispatches on the concrete Column/Tensor Value.
            if recv.is_none() {
                if let Some(pname) = Self::receiver_type_param_name(&obj_ty) {
                    if let Some(elem) = self.bound_element_for_trait(&pname, "ElementwiseMap") {
                        recv = Some((elem, Type::TypeParam(pname), "ElementwiseMap"));
                    }
                }
            }
            if let Some((elem, self_ty, container)) = recv {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "{container}.map expects 1 argument (closure), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                let f_ty = Type::Function {
                    params: vec![elem.clone()],
                    return_type: Box::new(elem),
                };
                self.check_expr(&args[0].value, &f_ty);
                self.record_expr_type(span, &self_ty);
                return self_ty;
            }
        }

        // ── Option / Result combinators, non-closure batch (B-2026-07-14-6) ──
        // A family of standard combinators the typechecker previously rejected
        // (no dedicated arm → `no method 'X' on Option/Result`) and which had no
        // runtime dispatch in either backend. Modelled here so they type
        // correctly; interpreter arms live in `method_call_optres.rs`, codegen
        // arms in `calls.rs`. The SOURCE payload type is recorded in
        // `method_unwrap_inner_types` (keyed by the call span) so codegen can
        // reconstruct the receiver's payload words, mirroring `map`/`unwrap`.
        // This batch is the CLOSURE-FREE subset: `ok`/`err` (Result→Option),
        // `or` (passthrough), `ok_or` (Option→Result), `flatten` (Option
        // un-nest). The closure-taking combinators are a separate arm below.
        if matches!(
            method,
            "ok" | "err" | "or" | "and" | "ok_or" | "flatten" | "take" | "get_or_insert"
        ) {
            let optres = |ty: &Type| -> Option<(bool, Type, Option<Type>)> {
                match ty {
                    Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                        Some((false, args[0].clone(), None))
                    }
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                        Some((true, args[0].clone(), Some(args[1].clone())))
                    }
                    _ => None,
                }
            };
            let recv = match &obj_ty {
                Type::Ref(i) | Type::MutRef(i) => optres(i),
                other => optres(other),
            };
            if let Some((is_result, t_ty, e_ty)) = recv {
                let opt = |payload: Type| Type::Named {
                    name: "Option".to_string(),
                    args: vec![payload],
                };
                let record_src = |s: &mut Self, ty: &Type| {
                    let resolved = resolve_type_var_top(ty, &s.env.substitutions);
                    s.method_unwrap_inner_types
                        .insert(SpanKey::from_span(span), Self::type_to_type_expr(&resolved));
                };
                let result = match method {
                    // `Result[T, E].ok() -> Option[T]` / `.err() -> Option[E]`.
                    "ok" | "err" if is_result => {
                        if !args.is_empty() {
                            self.type_error(
                                format!("Result.{method} takes no arguments"),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        let payload = if method == "ok" {
                            t_ty.clone()
                        } else {
                            e_ty.clone().unwrap_or(Type::Error)
                        };
                        record_src(self, &payload);
                        Some(opt(resolve_type_var_top(&payload, &self.env.substitutions)))
                    }
                    // `Option[T].or(alt: Option[T]) -> Option[T]` /
                    // `Result[T,E].or(alt: Result[T,F]) -> Result[T,F]` — eager
                    // alternative, returned when the receiver is absent/err.
                    // `and` is the dual: `Option[T].and(other: Option[U]) ->
                    // Option[U]` / `Result[T,E].and(other: Result[U,E]) ->
                    // Result[U,E]` — the eager `other`, returned when the
                    // receiver is PRESENT (else the absent receiver passes
                    // through). Both take the argument's type as the result
                    // (its payload governs the present/other branch), kept
                    // permissive like `unwrap_or`.
                    "or" | "and" => {
                        let arg_ty = args
                            .first()
                            .map(|a| self.infer_expr(&a.value))
                            .unwrap_or(Type::Error);
                        record_src(self, &t_ty);
                        Some(resolve_type_var_top(&arg_ty, &self.env.substitutions))
                    }
                    // `Option[T].ok_or(err: E) -> Result[T, E]` — eager error.
                    "ok_or" if !is_result => {
                        let err_ty = args
                            .first()
                            .map(|a| self.infer_expr(&a.value))
                            .unwrap_or(Type::Error);
                        record_src(self, &t_ty);
                        Some(Type::Named {
                            name: "Result".to_string(),
                            args: vec![
                                resolve_type_var_top(&t_ty, &self.env.substitutions),
                                resolve_type_var_top(&err_ty, &self.env.substitutions),
                            ],
                        })
                    }
                    // `Option[Option[U]].flatten() -> Option[U]`.
                    "flatten" if !is_result => {
                        if !args.is_empty() {
                            self.type_error(
                                "Option.flatten takes no arguments".to_string(),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        let inner = resolve_type_var_top(&t_ty, &self.env.substitutions);
                        match &inner {
                            Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                                record_src(self, &inner);
                                Some(opt(args[0].clone()))
                            }
                            _ => {
                                self.type_error(
                                    format!(
                                        "Option.flatten requires an `Option[Option[T]]` \
                                         receiver, found `Option[{}]`",
                                        type_display(&inner)
                                    ),
                                    span.clone(),
                                    TypeErrorKind::TypeMismatch,
                                );
                                Some(Type::Error)
                            }
                        }
                    }
                    // `Option[T].take() -> Option[T]` — MUTATING: returns the
                    // receiver's current value and leaves `None` in its place.
                    // Receiver-mutation is seeded in the effectchecker builtin
                    // table (`Option.take`) so the auto-par write-dependency
                    // gate serializes it against sibling reads (the
                    // B-2026-07-14-17 standing rule for in-place mutators).
                    "take" if !is_result => {
                        if !args.is_empty() {
                            self.type_error(
                                "Option.take takes no arguments".to_string(),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        record_src(self, &t_ty);
                        Some(opt(resolve_type_var_top(&t_ty, &self.env.substitutions)))
                    }
                    // `Option[T].get_or_insert(v: T) -> T` — MUTATING: inserts
                    // `Some(v)` when the receiver is `None`, then yields the
                    // (now guaranteed-present) payload. Kāra models the result
                    // BY VALUE (a copy of the payload), not Rust's `&mut T` —
                    // a mut-ref result needs place-ref machinery deferred with
                    // the rest of that surface. Mutation is seeded in the
                    // effectchecker table (`Option.get_or_insert`).
                    "get_or_insert" if !is_result => {
                        if let Some(a) = args.first() {
                            let at = self.infer_expr(&a.value);
                            self.check_assignable(&t_ty, &at, a.value.span.clone());
                        } else {
                            self.type_error(
                                "Option.get_or_insert expects 1 argument".to_string(),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                        }
                        record_src(self, &t_ty);
                        Some(resolve_type_var_top(&t_ty, &self.env.substitutions))
                    }
                    _ => None,
                };
                if let Some(result) = result {
                    self.record_expr_type(span, &result);
                    return result;
                }
            }
        }

        // ── Option / Result combinators, CLOSURE batch (B-2026-07-14-6) ──────
        // The closure-taking siblings of the non-closure block above:
        // `unwrap_or_else`, `map_or`, `map_or_else`, `map_err` (Result),
        // `and_then`, `or_else`, `filter` (Option). Each infers its closure
        // argument, seeds the closure's parameter from the receiver's payload
        // (present `T` / error `E`), reads the closure's return, and shapes the
        // result type accordingly. The SOURCE payload `T` is recorded in
        // `method_unwrap_inner_types` for codegen payload reconstruction (as
        // `map` does). Interpreter arms in `method_call_optres.rs`, codegen in
        // `calls.rs`.
        if matches!(
            method,
            "unwrap_or_else"
                | "map_or"
                | "map_or_else"
                | "map_err"
                | "and_then"
                | "or_else"
                | "filter"
        ) {
            let optres = |ty: &Type| -> Option<(bool, Type, Option<Type>)> {
                match ty {
                    Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                        Some((false, args[0].clone(), None))
                    }
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                        Some((true, args[0].clone(), Some(args[1].clone())))
                    }
                    _ => None,
                }
            };
            let recv = match &obj_ty {
                Type::Ref(i) | Type::MutRef(i) => optres(i),
                other => optres(other),
            };
            if let Some((is_result, t_ty, e_ty)) = recv {
                let e_ty = e_ty.unwrap_or(Type::Error);
                // Two closure-checking strategies:
                //  - `check_closure` (used when the closure's RETURN is already
                //    known — `filter`'s `bool`, `unwrap_or_else`'s payload `T`):
                //    `check_expr` against a fully-concrete `Fn(params) -> ret`
                //    SEEDS the closure's parameter, so an un-annotated `|x| x > 3`
                //    predicate type-checks against `T` (the `infer_expr`-then-
                //    unify order left `x` unsolved and `x > 3` failed as "cannot
                //    compare '?T' and 'i64'").
                //  - `infer_closure_ret` (used when the return is UNKNOWN —
                //    `map_or`/`map_err`/`and_then`/…): infer the closure, unify
                //    its first param with the seed, read the resolved return.
                //    Same limitation `map` has: an un-annotated param is only
                //    inferred for a body that unifies it (arithmetic), not a bare
                //    comparison — annotate `|x: T|` for those.
                let check_closure = |s: &mut Self, arg: &CallArg, params: Vec<Type>, ret: Type| {
                    let f_ty = Type::Function {
                        params,
                        return_type: Box::new(ret),
                    };
                    s.check_expr(&arg.value, &f_ty);
                };
                let infer_closure_ret =
                    |s: &mut Self, arg: &CallArg, seed: Option<&Type>| -> Type {
                        // B-2026-07-15-16: publish the param seed for the
                        // closure's un-annotated param, then infer the body
                        // FREELY (no return-type expectation). Seeding-then-
                        // free-infer binds a `?T` param to the receiver's payload
                        // up front — so `r.and_then(|x| x > 0)` /
                        // `v.retain(|x| x != 3)` stop failing "cannot compare
                        // '?T0' and 'i64'" — while a wrapper-returning body
                        // (`Ok(..)` / `Some(..)`) still infers its own payload and
                        // the enclosing context binds the rest. (Check-mode with a
                        // fresh return var leaves a bare constructor body
                        // un-adoptable.) An explicit param annotation wins in the
                        // synth-mode closure arm. No seed (a zero-param
                        // absent-branch closure, `Option.or_else(|| …)`) → the
                        // seed insert is skipped and the body infers as before.
                        if let (ExprKind::Closure { .. }, Some(seed)) = (&arg.value.kind, seed) {
                            s.closure_param_seeds
                                .insert(SpanKey::from_span(&arg.value.span), vec![seed.clone()]);
                        }
                        let f_actual = s.infer_expr(&arg.value);
                        let f_resolved = resolve_type_var_top(&f_actual, &s.env.substitutions);
                        match &f_resolved {
                            Type::Function {
                                params: _,
                                return_type,
                            }
                            | Type::OnceFunction {
                                params: _,
                                return_type,
                            } => resolve_type_var_top(return_type, &s.env.substitutions),
                            _ => Type::Error,
                        }
                    };
                // Record the payload type codegen reconstructs. `map_err` maps
                // over the `Err` payload (`Ok` passes through untouched), so it
                // records `E`; every other combinator reconstructs the present
                // payload `T`.
                let recorded_payload = if method == "map_err" {
                    resolve_type_var_top(&e_ty, &self.env.substitutions)
                } else {
                    resolve_type_var_top(&t_ty, &self.env.substitutions)
                };
                self.method_unwrap_inner_types.insert(
                    SpanKey::from_span(span),
                    Self::type_to_type_expr(&recorded_payload),
                );
                // The RESULT forms of the absent-closure combinators pass the
                // `Err` value `e` to that closure, so codegen additionally needs
                // `E` — recorded in the sibling table (the present-payload slot
                // above already holds `T` for these methods).
                if is_result && matches!(method, "unwrap_or_else" | "map_or_else" | "or_else") {
                    let e_resolved = resolve_type_var_top(&e_ty, &self.env.substitutions);
                    self.method_unwrap_err_types.insert(
                        SpanKey::from_span(span),
                        Self::type_to_type_expr(&e_resolved),
                    );
                }
                let opt = |payload: Type| Type::Named {
                    name: "Option".to_string(),
                    args: vec![payload],
                };
                // The closure's param list for the ABSENT branch (`unwrap_or_else`
                // / `map_or_else` default / `or_else`): none for Option, the error
                // `E` for Result.
                let absent_params = || {
                    if is_result {
                        vec![e_ty.clone()]
                    } else {
                        vec![]
                    }
                };
                let result: Option<Type> = match method {
                    // `unwrap_or_else(f)` — present payload, else `f()`/`f(e)`. → T.
                    // Return is the known payload `T`, so `check_closure` seeds
                    // the absent-branch param (`E` for Result) precisely.
                    "unwrap_or_else" => {
                        let t = resolve_type_var_top(&t_ty, &self.env.substitutions);
                        if let Some(a) = args.first() {
                            check_closure(self, a, absent_params(), t.clone());
                        }
                        Some(t)
                    }
                    // `map_or(default, f)` — `f(T)` if present, else `default`. → U.
                    "map_or" => {
                        let default_ty = args
                            .first()
                            .map(|a| self.infer_expr(&a.value))
                            .unwrap_or(Type::Error);
                        let r = args
                            .get(1)
                            .map(|a| infer_closure_ret(self, a, Some(&t_ty)))
                            .unwrap_or(default_ty);
                        Some(resolve_type_var_top(&r, &self.env.substitutions))
                    }
                    // `map_or_else(default_fn, f)` — `f(T)` if present, else
                    // `default_fn()`/`default_fn(e)`. → U (the mapper's return).
                    "map_or_else" => {
                        let r = args
                            .get(1)
                            .map(|a| infer_closure_ret(self, a, Some(&t_ty)))
                            .unwrap_or(Type::Error);
                        let r = resolve_type_var_top(&r, &self.env.substitutions);
                        // Seed the default_fn against the now-known result type.
                        if let Some(a) = args.first() {
                            check_closure(self, a, absent_params(), r.clone());
                        }
                        Some(r)
                    }
                    // `Result[T,E].map_err(f)` — `Err(f(e))`; `Ok` passes through.
                    // → `Result[T, F]`.
                    "map_err" if is_result => {
                        let f_ret = args
                            .first()
                            .map(|a| infer_closure_ret(self, a, Some(&e_ty)))
                            .unwrap_or(Type::Error);
                        Some(Type::Named {
                            name: "Result".to_string(),
                            args: vec![
                                resolve_type_var_top(&t_ty, &self.env.substitutions),
                                resolve_type_var_top(&f_ret, &self.env.substitutions),
                            ],
                        })
                    }
                    // `and_then(f)` — `f(T)` (itself an Option/Result) if present,
                    // else the absent receiver. → the closure's return type.
                    "and_then" => {
                        let r = args
                            .first()
                            .map(|a| infer_closure_ret(self, a, Some(&t_ty)))
                            .unwrap_or(Type::Error);
                        Some(resolve_type_var_top(&r, &self.env.substitutions))
                    }
                    // `or_else(f)` — present receiver, else `f()`/`f(e)` (itself
                    // an Option/Result). → the closure's return type.
                    "or_else" => {
                        let r = args
                            .first()
                            .map(|a| infer_closure_ret(self, a, absent_params().first()))
                            .unwrap_or_else(|| obj_ty.clone());
                        Some(resolve_type_var_top(&r, &self.env.substitutions))
                    }
                    // `Option[T].filter(pred)` — `Some(x)` kept iff `pred(x)`,
                    // else `None`. → `Option[T]`. Return is `bool`, so
                    // `check_closure` seeds `pred`'s param as `T`.
                    "filter" if !is_result => {
                        if let Some(a) = args.first() {
                            check_closure(self, a, vec![t_ty.clone()], Type::Bool);
                        }
                        Some(opt(resolve_type_var_top(&t_ty, &self.env.substitutions)))
                    }
                    _ => None,
                };
                if let Some(result) = result {
                    self.record_expr_type(span, &result);
                    return result;
                }
            }
        }

        // `ElementwiseMap`'s binary form `zip_with(other: Self, f: Fn(T, T) ->
        // T) -> Self` on the handle-backed containers (S6c) — element-wise
        // combine of two same-shape containers through the closure, yielding a
        // fresh `Self`. `other` must be the SAME container type; the closure is
        // typed `Fn(T, T) -> T` (both params + result the receiver's element).
        // Result is `Self`, which baked dispatch can't bind, so it's typed here
        // like `map`.
        if method == "zip_with" {
            // (element `T`, the owned `Self` container type, display name).
            let zip_receiver = |ty: &Type| -> Option<(Type, Type, &'static str)> {
                match ty {
                    Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                        Some((args[0].clone(), ty.clone(), "Column"))
                    }
                    Type::Named { name, args } if name == "Tensor" && !args.is_empty() => {
                        Some((args[0].clone(), ty.clone(), "Tensor"))
                    }
                    _ => None,
                }
            };
            let mut recv = match &obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => zip_receiver(inner),
                other => zip_receiver(other),
            };
            // Bound-generic receiver (`a: ref C` where `C: ElementwiseMap[T]`):
            // `zip_with` returns `Self = C`; `other` must also be `C`, and the
            // closure is `Fn(T, T) -> T` over the bound's element. Same mono
            // routing as `map` (fresh `Self` allocation); interp dispatches on
            // the concrete Column/Tensor Value.
            if recv.is_none() {
                if let Some(pname) = Self::receiver_type_param_name(&obj_ty) {
                    if let Some(elem) = self.bound_element_for_trait(&pname, "ElementwiseMap") {
                        recv = Some((elem, Type::TypeParam(pname), "ElementwiseMap"));
                    }
                }
            }
            if let Some((elem, self_ty, container)) = recv {
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "{container}.zip_with expects 2 arguments (other, closure), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                // `other` must be the same container type. The baked
                // signature declares it `other: ref Self` (a read borrow), so
                // a `ref Tensor` / `ref Column` argument — e.g. forwarding a
                // `ref Tensor[T, S]` parameter, the shape `dot`/`cosine`
                // helpers need — is correct; unwrap the borrow before the
                // same-container check, symmetric to the receiver unwrap
                // above. An owned argument (which auto-refs at the call)
                // passes through unchanged. (B-2026-07-13-5 gap C.)
                let other_ty_raw = self.infer_expr(&args[0].value);
                let other_ty = match &other_ty_raw {
                    Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                    _ => other_ty_raw,
                };
                self.check_assignable(&self_ty, &other_ty, args[0].value.span.clone());
                let f_ty = Type::Function {
                    params: vec![elem.clone(), elem.clone()],
                    return_type: Box::new(elem),
                };
                self.check_expr(&args[1].value, &f_ty);
                self.record_expr_type(span, &self_ty);
                return self_ty;
            }
        }

        // `ElementwiseOrd`'s ordering reductions on the handle-backed
        // containers (S6c) — `argmin` / `argmax` → `Option[i64]` (the FIRST
        // min/max index, `None` on empty/all-null), `sorted` → `Vec[T]`
        // (ascending values), `argsort` → `Vec[i64]` (the indices that sort
        // ascending, stable). The result mentions `T` (or is independent of
        // it), so it can't be expressed in a baked signature that binds `T`
        // from the receiver — typed here. For `Column` these operate on the
        // valid slots (nulls skipped; `argmin`/`argsort` report ORIGINAL slot
        // positions — `Series.idxmin` semantics); for `Tensor` over all
        // elements in flat C-order.
        if matches!(method, "argmin" | "argmax" | "sorted" | "argsort") {
            // (element `T`, display name) for a Column[T] / Tensor[T, ...S].
            let ord_receiver = |ty: &Type| -> Option<(Type, &'static str)> {
                match ty {
                    Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                        Some((args[0].clone(), "Column"))
                    }
                    Type::Named { name, args } if name == "Tensor" && !args.is_empty() => {
                        Some((args[0].clone(), "Tensor"))
                    }
                    _ => None,
                }
            };
            let recv = match &obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => ord_receiver(inner),
                other => ord_receiver(other),
            };
            if let Some((elem, container)) = recv {
                if !args.is_empty() {
                    self.type_error(
                        format!(
                            "{container}.{method} expects 0 arguments, got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                if !is_numeric(&elem) && !self.type_param_has_numeric_bound(&elem) {
                    self.type_error(
                        format!(
                            "{container}.{method} requires a numeric element type, found '{}'",
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                let ret = match method {
                    // The index of the first min/max, or None on empty/all-null.
                    "argmin" | "argmax" => Type::Named {
                        name: "Option".to_string(),
                        args: vec![Type::Int(IntSize::I64)],
                    },
                    // Ascending-sorted values (nulls dropped for a Column).
                    "sorted" => Type::Named {
                        name: "Vec".to_string(),
                        args: vec![elem.clone()],
                    },
                    // The indices that sort ascending (stable, original slots).
                    _ => Type::Named {
                        name: "Vec".to_string(),
                        args: vec![Type::Int(IntSize::I64)],
                    },
                };
                self.record_expr_type(span, &ret);
                // Stash the ELEMENT type at the non-aliased close-paren leaf so
                // the interpreter can recover element signedness for the
                // unsigned-64 sort order (B-2026-07-04-8). The result type
                // (`Vec[i64]` / `Option[i64]` for argsort/argmin/argmax, or the
                // `Vec[T]` that `record_expr_type(span, …)` just wrote) clobbers
                // `expr_types[receiver.span]`, so a `u64` element is otherwise
                // unrecoverable — the same receiver-span aliasing the `pow` /
                // bit-intrinsic paths work around via `args_close_span`.
                self.record_expr_type(args_close_span, &elem);
                return ret;
            }
        }

        // Column[T] statistical reductions (phase-11 stats). All operate on
        // the valid (non-null) slots — SQL/pandas aggregate semantics.
        // `sum`/`min`/`max` -> T; `mean`/`var`/`std`/`median`/`quantile` ->
        // f64 (the numerical world promotes integer stats to float, and
        // `Value` can't distinguish f32/f64 — the `Tensor.mean` rule).
        // `corr` is Pearson over two `Column[f64]` -> f64. Baked generic
        // dispatch can't bind `T` (nor the result type) from the receiver,
        // so the whole surface is typed here from the receiver's element.
        if matches!(
            method,
            "sum" | "mean" | "min" | "max" | "var" | "std" | "median" | "quantile" | "corr"
        ) {
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
            if let Some(elem) = column_elem {
                let nargs = usize::from(matches!(method, "quantile" | "corr"));
                if args.len() != nargs {
                    self.type_error(
                        format!(
                            "Column.{method} expects {nargs} argument(s), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                // `corr` is f64-only and binary (the other column); the rest
                // accept any numeric element.
                if method == "corr" {
                    if !matches!(elem, Type::Float(FloatSize::F64)) {
                        self.type_error(
                            format!(
                                "Column.corr requires an f64 column, found '{}'",
                                type_display(&elem)
                            ),
                            span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        self.infer_expr(&args[0].value);
                        return Type::Error;
                    }
                    let arg_ty = self.infer_expr(&args[0].value);
                    let expected = Type::Named {
                        name: "Column".to_string(),
                        args: vec![Type::Float(FloatSize::F64)],
                    };
                    self.check_assignable(&expected, &arg_ty, args[0].value.span.clone());
                    return Type::Float(FloatSize::F64);
                }
                if !is_numeric(&elem) && !self.type_param_has_numeric_bound(&elem) {
                    self.type_error(
                        format!(
                            "Column.{method} requires a numeric element type, found '{}'",
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                // `quantile(q)` — `q` is an f64 in [0, 1] (range checked at
                // runtime, since it isn't a compile-time constant in general).
                if method == "quantile" {
                    let q_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(
                        &Type::Float(FloatSize::F64),
                        &q_ty,
                        args[0].value.span.clone(),
                    );
                }
                return match method {
                    "sum" | "min" | "max" => elem,
                    // mean / var / std / median / quantile
                    _ => Type::Float(FloatSize::F64),
                };
            }
        }

        // DataFrame methods (phase-11 Arrow, interpreter MVP). `DataFrame`
        // is non-generic, so a result that mentions an element type can't
        // bind it from the receiver: `column(name)` types as `Column[?]`
        // — a fresh var pinned by the binding annotation / downstream use
        // (the `Column.new()` posture; a wrong annotation isn't caught
        // statically, the codegen slice tightens it). The concrete-typed
        // methods are handled here too so the whole surface is predictable
        // for a brand-new builtin rather than leaning on baked dispatch.
        let is_dataframe = match &obj_ty {
            Type::Named { name, .. } => name == "DataFrame",
            Type::Ref(inner) | Type::MutRef(inner) => {
                matches!(inner.as_ref(), Type::Named { name, .. } if name == "DataFrame")
            }
            _ => false,
        };
        if is_dataframe
            && matches!(
                method,
                "column"
                    | "insert"
                    | "has_column"
                    | "column_names"
                    | "width"
                    | "height"
                    | "select"
                    | "describe"
            )
        {
            let arity = |m: &str| match m {
                "insert" => 2usize,
                "column" | "has_column" | "select" => 1,
                _ => 0,
            };
            let want = arity(method);
            if args.len() != want {
                self.type_error(
                    format!("{method} expects {want} argument(s), got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            // Infer every arg (side effects / diagnostics); the leading
            // `name` of `column` / `has_column` / `insert` must be a
            // String, and `select`'s arg a `Vec[String]`. `insert`'s `col`
            // arg type isn't bound from the receiver (the baked-generic
            // limitation) — accepted as-is.
            let arg_tys: Vec<Type> = args.iter().map(|a| self.infer_expr(&a.value)).collect();
            if matches!(method, "column" | "has_column" | "insert") {
                self.check_assignable(&Type::Str, &arg_tys[0], args[0].value.span.clone());
            } else if method == "select" {
                self.check_assignable(
                    &Type::Named {
                        name: "Vec".to_string(),
                        args: vec![Type::Str],
                    },
                    &arg_tys[0],
                    args[0].value.span.clone(),
                );
            }
            return match method {
                "column" => Type::Named {
                    name: "Column".to_string(),
                    args: vec![self.env.fresh_type_var()],
                },
                "has_column" => Type::Bool,
                "column_names" => Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::Str],
                },
                "width" | "height" => Type::Int(IntSize::I64),
                "select" | "describe" => Type::Named {
                    name: "DataFrame".to_string(),
                    args: vec![],
                },
                // insert
                _ => Type::Unit,
            };
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

        // Direct iterator TERMINALS on an iterable collection receiver —
        // `v.sum()` / `v.product()` / `v.max()` / `v.min()` without the
        // `.iter()` hop (B-2026-07-16-14). Pre-fix these fell through to the
        // silent unknown-method leniency (`Type::Error`, which unifies with
        // anything), so `karac check` passed programs every backend then
        // trapped on — a check/execution hole on the exact shapes LLM authors
        // write constantly. Route them through `infer_iterator_method` as if
        // `.iter()` were present: the terminal's span-keyed metadata
        // (`iter_terminal_elem_types` etc.) records against THIS call's span,
        // and the lowering desugar (`src/lowering.rs`) rewrites the AST to the
        // canonical `.iter().<terminal>()` chain the backends implement.
        // Scoped to the no-closure numeric/ordering terminals; `join`/`concat`
        // are Vec[String]-receiver METHODS handled in their own arm (they
        // never had an Iterator form).
        // Narrowed to Vec/VecDeque receivers: SortedMap/SortedSet (and Map)
        // have their OWN min/max surfaces (Option[(K, V)] pairs, sorted-order
        // first/last) with dedicated typing + lowering — routing them here
        // regressed test_sorted_map_min_max_return_option_pair on the first
        // battery run.
        if matches!(method, "sum" | "product" | "max" | "min") {
            let vec_like_item = match &obj_ty {
                Type::Named { name, args: targs }
                    if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                {
                    Some(targs[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args: targs }
                        if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                    {
                        Some(targs[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(item_ty) = vec_like_item {
                return self.infer_iterator_method(&item_ty, method, args, span, false);
            }
        }

        // `Vec[String].join(sep) -> String` / `.concat() -> String` — the
        // string-collection terminals (B-2026-07-16-14's other half). These
        // are collection METHODS (no Iterator form): join places `sep`
        // between every adjacent pair (positionally — an empty first element
        // still gets a separator after it), concat is join with the empty
        // separator. Non-String elements are rejected here so the
        // check/execution contract holds (pre-fix these fell into the same
        // silent Type::Error leniency as the terminals above). VecDeque is
        // included — same layout, same runtime walk.
        if matches!(method, "join" | "concat") {
            let elem_is_str = match &obj_ty {
                Type::Named { name, args: targs }
                    if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                {
                    Some(matches!(targs[0], Type::Str))
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args: targs }
                        if matches!(name.as_str(), "Vec" | "VecDeque") && targs.len() == 1 =>
                    {
                        Some(matches!(targs[0], Type::Str))
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(is_str) = elem_is_str {
                if !is_str {
                    self.type_error(
                        format!("Vec.{method}() requires String elements"),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
                let expected_args = usize::from(method == "join");
                if args.len() != expected_args {
                    self.type_error(
                        format!(
                            "Vec.{method}() expects {expected_args} argument(s), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Str;
                }
                if method == "join" {
                    let sep_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(&Type::Str, &sep_ty, args[0].value.span.clone());
                }
                return Type::Str;
            }
        }

        // `StdinLines` (`stdin.lines()`) and `LinesIter` (`BufReader.lines()`)
        // are opaque line-iterator markers with NO surface methods — iteration
        // is via `for line in <iter>` only (the drain/codegen loop pulls one
        // line per turn). Reject ANY method call on them LOUDLY: an adaptor
        // (`.map()`/`.filter()`/…) or terminal is not wired into the for-loop
        // materialization, so without this it either falls through to a silent
        // zero-iteration no-op (`LinesIter`) or an unhelpful generic "no method"
        // (`StdinLines`) — B-2026-07-11-34. Direct for-loop iteration does not
        // go through method dispatch, so it is unaffected.
        if let Type::Named { name, .. } = &obj_ty {
            if name == "StdinLines" || name == "LinesIter" {
                self.type_error(
                    format!(
                        "`.{method}()` is not available on `{name}` — line iterators support no \
                         adaptors/terminals at v1; iterate directly with `for line in <iter>` and \
                         filter/map inside the loop body (each item is `Result[String, IoError]`)"
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

        // `Atomic[T]` load / store / read-modify-write ops — each takes an
        // explicit `MemoryOrdering` argument and has NO implicit-ordering
        // overload (deferred.md § Atomic Operations, lines 339–345):
        //   `load(ord) -> T`, `store(val, ord)`, and the RMW family
        //   `fetch_add` / `fetch_sub` / `fetch_and` / `fetch_or` /
        //   `fetch_xor` / `swap` — all `(val, ord) -> T`.
        // Without this arm these fell through to the silent `Type::Error`
        // catch-all below: arity went unchecked, so the implicit-ordering form
        // (`c.fetch_add(1)`) passed typecheck and ran fine under the
        // interpreter (which ignores the ordering) while codegen rejected it —
        // a run/build divergence (B-2026-06-30-5). Requiring the ordering here,
        // with a run-fatal `AtomicMissingOrdering`, makes `run` and `build`
        // agree: both reject the implicit form. Modeling the real return type
        // (`T` for load/RMW, `Unit` for store) also replaces the
        // universally-assignable `Type::Error` with the correct type. The
        // receiver gate (`Atomic[T]`, possibly behind a borrow) leaves the
        // same-named Vec/Slice `swap(i, j)` method untouched — it falls through
        // to its own handling below. `compare_exchange` (4 args, `Result`-typed)
        // is handled separately above. The ordering arg's `MemoryOrdering.X`
        // shape is validated at codegen.
        if matches!(
            method,
            "load"
                | "store"
                | "fetch_add"
                | "fetch_sub"
                | "fetch_and"
                | "fetch_or"
                | "fetch_xor"
                | "swap"
        ) {
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
                // `load` takes (ordering); `store` and every RMW op take
                // (value, ordering). Both forms require the trailing ordering.
                let want = if method == "load" { 1 } else { 2 };
                if args.len() != want {
                    let shape = if method == "load" {
                        "(ordering: MemoryOrdering)"
                    } else {
                        "(value, ordering: MemoryOrdering)"
                    };
                    self.type_error(
                        format!(
                            "Atomic.{method} takes {shape} — {want} argument{}; every atomic \
                             operation requires an explicit MemoryOrdering (there is no \
                             implicit-ordering form), found {}",
                            if want == 1 { "" } else { "s" },
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::AtomicMissingOrdering,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else if method == "load" {
                    // The single argument is the ordering — inferred for
                    // recording; its `MemoryOrdering.X` shape is a codegen check.
                    self.infer_expr(&args[0].value);
                } else {
                    // store / RMW: the leading value must be assignable to the
                    // atomic's inner type `T`; the trailing ordering is inferred.
                    // (`swap` accepts any `T`, including `Atomic[bool]`.)
                    let val_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(&inner, &val_ty, args[0].value.span.clone());
                    self.infer_expr(&args[1].value);
                }
                let ret = if method == "store" { Type::Unit } else { inner };
                self.record_expr_type(span, &ret);
                return ret;
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
                // Resolve BOTH sides through the substitutions the unify
                // just populated, so the assignability check doesn't
                // compare against a stale typevar. `resolve_type_var_top`
                // only reaches the TOP level — enough for the scalar case
                // (`let mut v = Vec.new(); v.push(5)` pins the receiver's
                // element var), but NOT for an empty container constructor
                // in argument position: `out.push(Vec.new())` with
                // `out: Vec[Vec[i64]]` unifies `Vec[i64]` with `Vec[?T0]`
                // (binding `?T0 = i64`), yet the arg's NESTED `?T0` stayed
                // unresolved and reported a spurious `Vec[i64]` vs
                // `Vec[?T0]` mismatch (B-2026-07-11-10). Deep-resolving the
                // arg makes the empty-constructor push infer without the
                // `let empty: Vec[i64] = Vec.new()` annotation.
                let no_names = HashMap::new();
                let no_const_names = HashMap::new();
                let resolved_elem = resolve_type_vars(
                    &elem,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                let resolved_arg = resolve_type_vars(
                    &arg_ty,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                self.check_assignable(&resolved_elem, &resolved_arg, args[0].value.span.clone());
                return Type::Unit;
            }
        }
        // `Vec[T].insert(idx: i64, value: T) -> ()` — shift the tail up and
        // place `value` at `idx` (`idx == len` appends). Sibling of `push`
        // (same element-var unification so `let mut v = Vec.new(); v.insert(0,
        // x)` pins the element type) and `remove` (arg 0 is the i64 index).
        if method == "insert" && args.len() == 2 {
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
                let idx_ty = self.infer_expr(&args[0].value);
                self.check_assignable(
                    &Type::Int(IntSize::I64),
                    &idx_ty,
                    args[0].value.span.clone(),
                );
                let val_ty = self.infer_expr(&args[1].value);
                unify_types(
                    &elem,
                    &val_ty,
                    &mut self.env.substitutions,
                    &mut self.env.const_substitutions,
                );
                let no_names = HashMap::new();
                let no_const_names = HashMap::new();
                let resolved_elem = resolve_type_vars(
                    &elem,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                let resolved_arg = resolve_type_vars(
                    &val_ty,
                    &self.env.substitutions,
                    &no_names,
                    &self.env.const_substitutions,
                    &no_const_names,
                );
                self.check_assignable(&resolved_elem, &resolved_arg, args[1].value.span.clone());
                return Type::Unit;
            }
        }

        // `Vec[T].extend_from_slice(other)` — `other` may be
        // `Slice[T]`, `Vec[T]`, or `Array[T, N]`. We unify the
        // receiver's element type with the source's element type so
        // that an unsolved typevar on the receiver (e.g. `let mut v =
        // Vec.new(); v.extend_from_slice(other);`) gets pinned to the
        // source's element type, mirroring `push`'s behavior.
        if matches!(method, "extend_from_slice" | "extend") && args.len() == 1 {
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
                    // A structural `Type::Slice { element }` source — what
                    // `String.bytes()` / `Vec.as_slice()` / a `v.slice(a, b)`
                    // view produce (the byte-slice shape, distinct from the
                    // `Type::Named { name: "Slice" }` spelling). Both backends
                    // already accept a 2-field slice source
                    // (`vec_method.rs`); without this arm the call fell through
                    // to the silent prelude Error-typing (part of
                    // B-2026-07-17-12) — e.g. `buf.extend_from_slice(s.bytes())`
                    // typed as `Type::Error` instead of `()`.
                    Type::Slice { element, .. } => Some((**element).clone()),
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
        if matches!(method, "remove" | "swap_remove") && args.len() == 1 {
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

        // `ref String` / `mut ref String` also route to the stdlib String
        // surface. Previously only a bare String did, and a borrowed receiver
        // took the impl-block deref path — which resolves the impl-style
        // methods but does NOT type the stdlib-only ones (`find` / `slice` /
        // `sorted` / …), so e.g. `s.find(' ')` on a `ref String` param degraded
        // to `Type::Error` (typecheck-permissive) and the `unwrap_or` inner-type
        // side-table never recorded → codegen fell through. String has no user
        // impl block, so `infer_str_method` is the complete surface for it; the
        // impl-style read-methods (`len`/`contains`/`is_empty`) it already
        // enumerates keep working.
        if matches!(&obj_ty, Type::Ref(i) | Type::MutRef(i) if **i == Type::Str) {
            return self.infer_str_method(method, args, span);
        }

        // `StringSlice` (a borrowed view over a `String`'s UTF-8 bytes, same
        // `{ptr,len,cap}` layout with `cap == 0`) shares String's read-method
        // surface — `len`/`is_empty`/`contains`/`split`/`find`/`slice`/
        // `to_string`/… — so route it (and `ref StringSlice`) through the same
        // stdlib dispatch (design.md § StringSlice). No impl block exists for
        // it, so unlike `ref String` the derefed form routes here too.
        if matches!(&obj_ty_for_named, Type::Named { name, .. } if name == "StringSlice") {
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
            if name == "SortedMap" {
                let key = type_args.first().cloned().unwrap_or(Type::Error);
                let value = type_args.get(1).cloned().unwrap_or(Type::Error);
                return self.infer_sorted_map_method(&key, &value, method, args, span);
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

        // `CString` method dispatch — the owning C-string produced by
        // `String.to_cstring()` (design.md § C-String Literals). Same hardcoded-
        // arm pattern as `CStr`: record the `CString.<method>` callee so codegen
        // routes it (no impl block backs the type), then infer via
        // `infer_cstring_method` (`as_ptr` / `len` / `is_empty` / `as_bytes`).
        if let Type::Named { name, .. } = &obj_ty_for_named {
            if name == "CString" {
                self.method_callee_types
                    .insert(SpanKey::from_span(span), format!("CString.{}", method));
                return self.infer_cstring_method(method, args, span);
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
        // `Vec[T].retain(pred)` / `VecDeque[T].retain(pred)` — keep each element
        // for which `pred: Fn(T) -> bool` holds; mutates in place, returns Unit.
        // Vec has no stdlib impl, so an unhandled `retain` fell to the silent
        // prelude path that infers the closure arg with an UN-seeded `?T` param
        // — `v.retain(|x| x != 3)` then failed "cannot compare '?T0' and 'i64'"
        // (B-2026-07-15-16). Seed the param via the concrete-return `Fn(T) ->
        // bool` check-mode pushdown, exactly as `Option.filter` / the
        // `.iter().filter(..)` adaptor already do. (Map/Set `retain` take a
        // 2-arg `Fn(K, V) -> bool` — a separate arity, not covered here.)
        if method == "retain" {
            let elem_for_vec: Option<Type> = match &obj_ty {
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
            if let Some(elem) = elem_for_vec {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "Vec.retain() expects 1 argument (predicate closure), found {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    let f_ty = Type::Function {
                        params: vec![elem],
                        return_type: Box::new(Type::Bool),
                    };
                    self.check_expr(&args[0].value, &f_ty);
                }
                return Type::Unit;
            }
        }
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
                // Stash the element type at the non-aliased close-paren leaf so
                // the interpreter recovers element signedness for the unsigned-64
                // sort order (B-2026-07-04-8). `sort()` is typed `Unit` and
                // `sorted()`'s `Vec[T]` result also clobbers the receiver span,
                // so this leaf is the reliable channel to a `u64` element (same
                // mechanism as the Column / Tensor ordering methods).
                if matches!(method, "sort" | "sorted") {
                    self.record_expr_type(args_close_span, &elem);
                }
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
        // `.cmp(other)` on a `#[derive(Ord)]` struct/enum returns `Ordering` —
        // the method form of the `<`/`>` operators, which already work for such
        // types via the lexicographic `karac_cmp_<T>` comparator (codegen) /
        // `value_compare` (interpreter). The derive registers NO `cmp` entry in
        // `env.impls`, so without this intercept a Named receiver falls through
        // to the `NoMethodFound` arm ("no method 'cmp' on type 'P'"), breaking
        // `p.cmp(q)`, `min`/`max`/`clamp` on struct/enum types (their bodies
        // call `a.cmp(b)`), sorting, and any `fn f[T: Ord]` body. Gated to the
        // DERIVED case (`!has_user_impl_ord`) so a hand-written `impl Ord` still
        // resolves through the normal impl-table path. Mirrors the String `cmp`
        // handler (`stdlib_seq.rs`) and the primitive Ord builtin-impl
        // (`env_build.rs`). roadmap Phase 8 § Eq/Ord.
        if method == "cmp" {
            if let Type::Named { name, .. } = &receiver_for_lookup {
                let name = name.clone();
                if self.type_supports_ord(&receiver_for_lookup) && !self.has_user_impl_ord(&name) {
                    if args.len() != 1 {
                        self.type_error(
                            format!("'cmp' expects 1 argument, found {}", args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                    } else {
                        let arg_ty = self.infer_expr(&args[0].value);
                        self.check_assignable(
                            &receiver_for_lookup,
                            &arg_ty,
                            args[0].value.span.clone(),
                        );
                    }
                    return Type::Named {
                        name: "Ordering".to_string(),
                        args: Vec::new(),
                    };
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
        // Built-in `signum` — `x.signum() -> Self`. Signed-int receivers yield
        // -1 / 0 / 1 (Rust `iN::signum`); float receivers yield -1.0 / +1.0 /
        // NaN, with `signum` carrying the sign of a signed zero (Rust
        // `f64::signum` = `copysign(1.0, x)`, NaN-preserving). Unsigned integers
        // have no `signum` in Rust, so `UInt` falls through to `NoMethodFound`.
        // Backends: interpreter `method_call.rs`, codegen `method_call.rs`.
        if method == "signum"
            && args.is_empty()
            && matches!(&receiver_for_lookup, Type::Int(_) | Type::Float(_))
        {
            return receiver_for_lookup.clone();
        }
        // Built-in `sqrt` on float primitives — `x.sqrt() -> Self`. Float-only
        // (no integer square root); lowers to the `llvm.sqrt` intrinsic in
        // codegen (a single `f64.sqrt` instruction on wasm — no libm) and
        // `f64::sqrt` in the interpreter. The first piece of a numeric math
        // surface, driven by Plume's flow field needing vector normalization
        // (`docs/dogfooding.md`). The rest of that surface (sin/cos/tan/exp/ln/
        // log2/pow/atan2/floor/ceil/round) lives in the `crate::float_math`
        // block just below. Backends: interpreter `method_call.rs`, codegen
        // `method_call.rs`.
        if method == "sqrt" && args.is_empty() && matches!(&receiver_for_lookup, Type::Float(_)) {
            return receiver_for_lookup.clone();
        }
        // Built-in float arithmetic helpers — `x.recip() -> Self` (`1.0 / x`),
        // `x.to_degrees() -> Self`, `x.to_radians() -> Self`, `x.fract() -> Self`
        // (`x - x.trunc()`). Pure IEEE arithmetic (no libm, no intrinsic):
        // `recip` is a single `fdiv`, the angle conversions a single `fmul` by
        // the same constant the interpreter uses, and `fract` an `fsub` against
        // `llvm.trunc`, so `run == build` is bit-exact. Float-only.
        if matches!(method, "recip" | "to_degrees" | "to_radians" | "fract")
            && args.is_empty()
            && matches!(&receiver_for_lookup, Type::Float(_))
        {
            return receiver_for_lookup.clone();
        }
        // Built-in scalar transcendental + rounding math on float primitives —
        // `x.sin()` / `x.cos()` / `x.tan()` / `x.exp()` / `x.ln()` / `x.log2()`
        // / `x.floor()` / `x.ceil()` / `x.round()` (unary, `-> Self`) and
        // `x.pow(y)` / `x.atan2(y)` (binary, one argument of the same float
        // type, `-> Self`). The value-receiver shape, mirroring `sqrt`/`abs`;
        // the surface is the single `crate::float_math` table the interpreter
        // and codegen share. Float-only — integer receivers fall through to
        // `NoMethodFound`. Backends: interpreter `method_call.rs` (Rust
        // `f64::*`), codegen `method_call.rs` (LLVM intrinsics; `atan2` via a
        // direct libm call). Driven by the Plume flow-field dogfood.
        if matches!(&receiver_for_lookup, Type::Float(_)) {
            if let Some(kind) = crate::float_math::classify(method) {
                match kind {
                    crate::float_math::FloatMathKind::Unary => {
                        if !args.is_empty() {
                            self.type_error(
                                format!("{method} expects 0 arguments, got {}", args.len()),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                            return Type::Error;
                        }
                        return receiver_for_lookup.clone();
                    }
                    crate::float_math::FloatMathKind::Binary => {
                        if args.len() != 1 {
                            self.type_error(
                                format!("{method} expects 1 argument, got {}", args.len()),
                                span.clone(),
                                TypeErrorKind::WrongNumberOfArgs,
                            );
                            return Type::Error;
                        }
                        // The argument is the same float type as the receiver. A
                        // suffix-free float literal promotes to it (Q4 rule, like
                        // `wrapping_*`); otherwise it must match exactly.
                        let arg = &args[0].value;
                        let arg_ty = self.infer_expr(arg);
                        if matches!(&arg.kind, ExprKind::Float(_, None)) {
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
                }
            }
        }
        // IEEE-754 bit reinterpretation (used by protobuf `float`/`double`
        // fixed-width codecs). `to_bits` → `u64` (f64 pattern), `to_bits32` →
        // `u32` (the value rounded to f32, then its 32-bit pattern). The width
        // is in the method name so no receiver-width recovery is needed.
        if args.is_empty() && matches!(&receiver_for_lookup, Type::Float(_)) {
            if method == "to_bits" {
                return Type::UInt(UIntSize::U64);
            }
            if method == "to_bits32" {
                return Type::UInt(UIntSize::U32);
            }
        }
        // The inverse: reinterpret an integer's low bits as a float.
        // `bits_as_f64` (from a `u64`) / `bits_as_f32` (from a `u32`).
        if args.is_empty() && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if method == "bits_as_f64" {
                return Type::Float(FloatSize::F64);
            }
            if method == "bits_as_f32" {
                return Type::Float(FloatSize::F32);
            }
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
        // Euclidean division / remainder — `div_euclid` / `rem_euclid`
        // (design.md § Arithmetic Overflow, the Rust `iN::div_euclid` /
        // `rem_euclid` semantics: the remainder is always non-negative, so
        // `(-7).rem_euclid(3) == 2`). Both trap like `/` and `%` — `division by
        // zero` on a zero divisor, `integer overflow` on `iN::MIN / -1`.
        // **Scoped to `i64` in this slice** (same 64-bit-first cut as
        // `wrapping_*`): i64 is i64-backed end-to-end, so the interpreter's
        // `checked_div_euclid`/`checked_rem_euclid` and codegen's signed
        // correction agree without width-masking. Narrow signed widths and the
        // unsigned widths (where Euclidean == truncating) are a tracked
        // follow-on (`NoMethodFound` until then). Same strict same-type /
        // literal-promotion arg rule as `wrapping_*`.
        if matches!(method, "div_euclid" | "rem_euclid")
            && matches!(&receiver_for_lookup, Type::Int(IntSize::I64))
        {
            if args.len() != 1 {
                self.type_error(
                    format!("{method} expects 1 argument, got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &receiver_for_lookup);
                self.record_expr_type(args_close_span, &receiver_for_lookup);
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
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return receiver_for_lookup.clone();
        }
        // Overflow-aware integer arithmetic — `{checked,saturating,overflowing}_{add,sub,mul}`
        // (design.md § Arithmetic Overflow): the explicit-overflow siblings of the
        // checked `+`/`-`/`*` path. Unlike `wrapping_*` (64-bit only), these are
        // defined on EVERY integer width: codegen is naturally width-aware (LLVM
        // overflow/saturating intrinsics on the receiver's iN/uN type), and the
        // interpreter recovers the receiver width from `expr_types` (the same
        // span→type lookup `narrow_oob` uses). Return shapes:
        //   checked_*      -> Option[Self]   (None on overflow)
        //   saturating_*   -> Self           (clamped to iN::MAX/MIN / uN::MAX/0)
        //   overflowing_*  -> (Self, bool)    (result + overflow flag)
        // Both operands and the result are the receiver's type (same strict
        // same-type / literal-promotion rule as `wrapping_*`). Backends:
        // interpreter + codegen `method_call.rs`.
        {
            let checked = matches!(method, "checked_add" | "checked_sub" | "checked_mul");
            let saturating = matches!(
                method,
                "saturating_add" | "saturating_sub" | "saturating_mul"
            );
            let overflowing = matches!(
                method,
                "overflowing_add" | "overflowing_sub" | "overflowing_mul"
            );
            if (checked || saturating || overflowing)
                && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            {
                if args.len() != 1 {
                    self.type_error(
                        format!("{method} expects 1 argument, got {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    return Type::Error;
                }
                let arg = &args[0].value;
                let arg_ty = self.infer_expr(arg);
                // Suffix-free integer literal arg promotes to the receiver type
                // (mirrors `wrapping_*`); otherwise it must match exactly.
                if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                    self.record_expr_type(&arg.span, &receiver_for_lookup);
                } else if arg_ty != Type::Error && arg_ty != receiver_for_lookup {
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
                let self_ty = receiver_for_lookup.clone();
                return if checked {
                    Type::Named {
                        name: "Option".to_string(),
                        args: vec![self_ty],
                    }
                } else if saturating {
                    self_ty
                } else {
                    Type::Tuple(vec![self_ty, Type::Bool])
                };
            }
        }
        // Integer `.pow(exp)` — `n.pow(k) -> Self`, the repeated-multiply power
        // (design.md § Arithmetic). The exponent is `u32` (matching Rust's
        // `iN::pow(self, exp: u32)`); a suffix-free integer-literal exponent is
        // promoted to `u32`, otherwise it must already be `u32` (cast with
        // `as u32`). Overflow TRAPS as `integer overflow` — the same app/lib
        // behavior as the `*` operator it iterates. Defined on every integer
        // width; the interpreter recovers the receiver width from the receiver
        // type stashed at `args_close_span` (the non-aliased close-paren leaf)
        // so the trap fires at the declared width. Backends: interpreter +
        // codegen `method_call.rs`.
        if method == "pow" && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if args.len() != 1 {
                self.type_error(
                    format!("pow expects 1 argument, got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            let u32_ty = Type::UInt(UIntSize::U32);
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &u32_ty);
            } else if arg_ty != Type::Error && arg_ty != u32_ty {
                self.type_error(
                    format!(
                        "pow expects an exponent of type `u32`, got `{}` (cast with `as u32`)",
                        type_display(&arg_ty)
                    ),
                    arg.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return receiver_for_lookup.clone();
        }
        // `min` / `max` on a numeric scalar — `a.min(b)` / `a.max(b)` return the
        // smaller / larger of the two (Rust's `Ord::min`/`max`, and `f64::min`/
        // `max` for floats, which are NaN-propagating-free like Rust). The arg
        // must be the same numeric type (a bare literal coerces to it). Gated on
        // a scalar receiver so it never shadows `Vec`/iterator `min`/`max`.
        if matches!(method, "min" | "max")
            && matches!(
                &receiver_for_lookup,
                Type::Int(_) | Type::UInt(_) | Type::Float(_)
            )
        {
            if args.len() != 1 {
                self.type_error(
                    format!("`{method}` expects 1 argument, got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            let is_bare_num_lit = matches!(
                &arg.kind,
                ExprKind::Integer(_, None) | ExprKind::Float(_, None)
            );
            if is_bare_num_lit {
                self.record_expr_type(&arg.span, &receiver_for_lookup);
            } else if arg_ty != Type::Error && arg_ty != receiver_for_lookup {
                self.type_error(
                    format!(
                        "`{method}` expects an argument of type `{}`, got `{}`",
                        type_display(&receiver_for_lookup),
                        type_display(&arg_ty)
                    ),
                    arg.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return receiver_for_lookup.clone();
        }
        // `clamp` on a numeric scalar — `v.clamp(lo, hi)` pins `v` into the
        // inclusive range `[lo, hi]`, the method sibling of the `clamp[T: Ord]`
        // free fn (ordering.kara). Same nested-bound semantics: `v < lo → lo`,
        // else `v > hi → hi`, else `v` (so `lo` wins on an inverted range).
        // Both bounds must be the receiver type (bare literals coerce). Gated
        // on a scalar receiver so it never shadows a user/collection `clamp`.
        if method == "clamp"
            && matches!(
                &receiver_for_lookup,
                Type::Int(_) | Type::UInt(_) | Type::Float(_)
            )
        {
            if args.len() != 2 {
                self.type_error(
                    format!("`clamp` expects 2 arguments, got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            for arg in args.iter() {
                let arg = &arg.value;
                let arg_ty = self.infer_expr(arg);
                let is_bare_num_lit = matches!(
                    &arg.kind,
                    ExprKind::Integer(_, None) | ExprKind::Float(_, None)
                );
                if is_bare_num_lit {
                    self.record_expr_type(&arg.span, &receiver_for_lookup);
                } else if arg_ty != Type::Error && arg_ty != receiver_for_lookup {
                    self.type_error(
                        format!(
                            "`clamp` expects an argument of type `{}`, got `{}`",
                            type_display(&receiver_for_lookup),
                            type_display(&arg_ty)
                        ),
                        arg.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
            }
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return receiver_for_lookup.clone();
        }
        // Bit intrinsics on integer scalars — `count_ones` / `leading_zeros` /
        // `trailing_zeros` -> u32 (Rust's `iN::{count_ones,leading_zeros,
        // trailing_zeros}`). All width-dependent: `leading_zeros` / `trailing_zeros`
        // count within the receiver's bit width, and `count_ones` over its `bits`
        // low bits (a signed `iN`'s sign-extended interpreter representation is
        // masked to width first). The `u32` result differs from the receiver, so
        // the generic `infer_expr` post-record clobbers `expr_types[receiver.span]`
        // — the interpreter reads the receiver type stashed at the non-aliased
        // `args_close_span` leaf instead. Effect-free; codegen lowers to the
        // overloaded `llvm.ctpop` / `llvm.ctlz` / `llvm.cttz` intrinsics.
        if args.is_empty()
            && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            && matches!(
                method,
                "count_ones" | "count_zeros" | "leading_zeros" | "trailing_zeros"
            )
        {
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return Type::UInt(UIntSize::U32);
        }
        // `is_power_of_two` on unsigned integer scalars -> bool (Rust's
        // `uN::is_power_of_two`; unsigned-only, since power-of-two-ness is
        // meaningless for a signed/negative value). The bool result differs from
        // the receiver, so the receiver type is stashed at `args_close_span` for
        // the interpreter to recover the width (it masks the stored value to
        // width before the single-bit test). Effect-free; codegen lowers to the
        // inline `(x != 0) & ((x & (x-1)) == 0)`.
        if args.is_empty()
            && matches!(&receiver_for_lookup, Type::UInt(_))
            && method == "is_power_of_two"
        {
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return Type::Bool;
        }
        // `next_power_of_two` on unsigned integer scalars -> Self (Rust's
        // `uN::next_power_of_two`; unsigned-only). The smallest power of two ≥
        // self (0 and 1 both → 1). TRAPS `integer overflow` when the result
        // would exceed the width (`self > 2^(bits-1)`), matching the `*`/`pow`
        // trap policy. The Self result keeps the receiver span's type; the
        // interpreter recovers the width from `args_close_span`. Effect-free;
        // codegen lowers via `llvm.ctlz` + a shift with an overflow-trap branch.
        if args.is_empty()
            && matches!(&receiver_for_lookup, Type::UInt(_))
            && method == "next_power_of_two"
        {
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return receiver_for_lookup.clone();
        }
        // `abs_diff(self, other) -> unsigned sibling` (Rust `iN/uN::abs_diff`):
        // the absolute difference of two same-type integers ALWAYS fits the
        // unsigned type of the same width (`i8::MIN.abs_diff(i8::MAX) == 255u8`),
        // so it never overflows and never traps. The result type differs from
        // the receiver (signed → unsigned sibling; unsigned → itself), so the
        // receiver type is stashed at `args_close_span` for the interpreter's
        // width recovery. Effect-free; codegen lowers to `select(a≥b, a-b, b-a)`
        // (signed/unsigned compare per receiver signedness) then zero-extends the
        // iN magnitude to the i64-backed representation.
        if method == "abs_diff" && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_)) {
            if args.len() != 1 {
                self.type_error(
                    format!("abs_diff expects 1 argument, got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            // A suffix-free integer literal arg promotes to the receiver type;
            // otherwise it must match exactly (mirrors `checked_*`).
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &receiver_for_lookup);
            } else if arg_ty != Type::Error && arg_ty != receiver_for_lookup {
                self.type_error(
                    format!(
                        "abs_diff expects an argument of type `{}`, got `{}`",
                        type_display(&receiver_for_lookup),
                        type_display(&arg_ty)
                    ),
                    arg.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return match &receiver_for_lookup {
                Type::Int(IntSize::I8) => Type::UInt(UIntSize::U8),
                Type::Int(IntSize::I16) => Type::UInt(UIntSize::U16),
                Type::Int(IntSize::I32) => Type::UInt(UIntSize::U32),
                Type::Int(IntSize::I64) => Type::UInt(UIntSize::U64),
                Type::Int(IntSize::I128) => Type::UInt(UIntSize::U128),
                // Already unsigned — `abs_diff` returns the same unsigned type.
                other => other.clone(),
            };
        }
        // Bit-permutation intrinsics on integer scalars — `reverse_bits` /
        // `swap_bytes` -> Self (Rust's `iN::{reverse_bits,swap_bytes}`). Both
        // are width-dependent (they permute within the receiver's `bits`), so
        // the `Self` result means the receiver span keeps its type; the
        // interpreter recovers the width from `args_close_span` like the count
        // family. Effect-free; codegen lowers to `llvm.bitreverse` / `llvm.bswap`
        // on the receiver's iN type.
        if args.is_empty()
            && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_))
            && matches!(method, "reverse_bits" | "swap_bytes")
        {
            self.record_expr_type(args_close_span, &receiver_for_lookup);
            return receiver_for_lookup.clone();
        }
        // Bit-rotation intrinsics on integer scalars — `rotate_left(n)` /
        // `rotate_right(n)` -> Self (Rust's `iN::rotate_{left,right}`, `n: u32`).
        // Width-dependent: the rotation wraps within the receiver's `bits`
        // (`n` is taken mod `bits`). The `Self` result keeps the receiver span's
        // type; the interpreter recovers the width from `args_close_span`.
        // Codegen lowers to `llvm.fshl` / `llvm.fshr` on the receiver's iN.
        if matches!(method, "rotate_left" | "rotate_right")
            && matches!(&receiver_for_lookup, Type::Int(_) | Type::UInt(_))
        {
            if args.len() != 1 {
                self.type_error(
                    format!(
                        "`{method}` expects 1 argument (the rotation amount), got {}",
                        args.len()
                    ),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            // The amount is `u32`; a suffix-free integer literal promotes.
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &Type::UInt(UIntSize::U32));
            } else if arg_ty != Type::Error && !matches!(arg_ty, Type::Int(_) | Type::UInt(_)) {
                self.type_error(
                    format!(
                        "`{method}` expects an integer rotation amount, got `{}`",
                        type_display(&arg_ty)
                    ),
                    arg.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            self.record_expr_type(args_close_span, &receiver_for_lookup);
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
                "is_alphabetic"
                    | "is_numeric"
                    | "is_alphanumeric"
                    | "is_whitespace"
                    | "is_uppercase"
                    | "is_lowercase"
                    | "is_ascii"
            )
        {
            return Type::Bool;
        }
        // ASCII case folding on a `char` — `to_ascii_uppercase` /
        // `to_ascii_lowercase` -> char (Rust's `char::to_ascii_*case`): only the
        // ASCII letters `a`..`z` / `A`..`Z` are mapped, every other codepoint
        // (incl. non-ASCII) is returned unchanged. Unlike the Unicode
        // `to_uppercase` (which yields an *iterator* — `ß` → `SS`), the ASCII
        // form is a pure char→char map, so it lowers to inline codepoint
        // arithmetic in codegen (no Unicode tables). Char-only.
        if args.is_empty()
            && matches!(&receiver_for_lookup, Type::Char)
            && matches!(method, "to_ascii_uppercase" | "to_ascii_lowercase")
        {
            return Type::Char;
        }
        // `char.to_digit(radix) -> Option[u32]` (Rust's `char::to_digit`): the
        // numeric value of `self` as a digit in `radix`, `None` if `self` is not
        // a digit in that radix. `radix` is `u32` (a suffix-free literal
        // promotes); an out-of-range radix (`< 2` or `> 36`) traps at run time,
        // matching Rust's panic. Interpreter-complete; codegen emits an honest
        // "not yet supported under `karac build`" error (the Option[u32]
        // construction lowering is shared with the `checked_to_*` follow-on).
        if method == "to_digit" && matches!(&receiver_for_lookup, Type::Char) {
            if args.len() != 1 {
                self.type_error(
                    format!("to_digit expects 1 argument, got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Type::Error;
            }
            let u32_ty = Type::UInt(UIntSize::U32);
            let arg = &args[0].value;
            let arg_ty = self.infer_expr(arg);
            if matches!(&arg.kind, ExprKind::Integer(_, None)) {
                self.record_expr_type(&arg.span, &u32_ty);
            } else if arg_ty != Type::Error && arg_ty != u32_ty {
                self.type_error(
                    format!(
                        "to_digit expects a radix of type `u32`, got `{}` (cast with `as u32`)",
                        type_display(&arg_ty)
                    ),
                    arg.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            return Type::Named {
                name: "Option".to_string(),
                args: vec![u32_ty],
            };
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
                    // Payload-bearing `#[derive(Display)]` enums now render
                    // under codegen exactly as f-string interpolation does
                    // (`Other(disk full)` etc.) — the payload-enum Display
                    // renderer that the old all-unit restriction waited on has
                    // landed, so explicit `.to_string()` types for them too
                    // (verified interp == JIT == AOT). `#[display_snake_case]`
                    // enums stay excluded pending their own renderer follow-on.
                    let enum_display = self
                        .env
                        .enums
                        .get(name)
                        .map(|e| {
                            e.derived_traits.contains("Display")
                                && !self.display_snake_case_enums.contains(name)
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
                // A NUMERIC PRIMITIVE receiver (`i64`, `u32`, `f64`, …) with a
                // USER trait/inherent impl method (`impl Dbl for u8 { fn
                // dbl(self) -> Self { ... } }`) dispatches through the same
                // impl-table path a `Named` receiver uses: register it as
                // `(prim, [])` and fall through to the resolution below. The
                // builtin comparison / cast ops have dedicated backend arms and
                // their baked stdlib impls (`Ord`/`Eq`/… on primitives) carry a
                // `(self, other)`-shaped signature that the impl-table dispatch
                // mis-counts (`a.cmp(b)` → "expects 2 args, found 1"); keep them
                // on the historical poison-with-diagnostic path. So route ONLY a
                // NON-builtin method that has a real impl candidate; everything
                // else falls through to the poison branch. B-2026-07-03-5.
                const PRIMITIVE_VALUE_METHODS: &[&str] =
                    &["cmp", "eq", "ne", "lt", "le", "gt", "ge", "cast"];
                if matches!(
                    &receiver_for_lookup,
                    Type::Int(_) | Type::UInt(_) | Type::Float(_)
                ) {
                    if let Some(prim) = method_callee_type_name(&receiver_for_lookup) {
                        if !PRIMITIVE_VALUE_METHODS.contains(&method)
                            && !self
                                .env
                                .find_methods_with_args(&prim, &[], method)
                                .is_empty()
                        {
                            // Route to impl-table dispatch (arg inference /
                            // label validation / Self resolution happen there).
                            (prim, Vec::new())
                        } else {
                            // For non-impl methods, just type-check args and
                            // return Error. Close the silent-poison hole for
                            // numeric receivers: their method surface is closed
                            // (registered builtin ops + a small value-receiver
                            // special set), so an unknown method here is a
                            // genuine typo, not a partially-implicit prelude
                            // surface. Without the error it returned
                            // `Type::Error` (poison, universally assignable, so
                            // `let s: String = x.bogus()` typechecked clean) and
                            // then exploded in the backend. `abs`/`clone`/
                            // `to_string` are handled in the early intercept
                            // above and never reach here.
                            for arg in args {
                                self.infer_expr(&arg.value);
                            }
                            if !PRIMITIVE_VALUE_METHODS.contains(&method) {
                                self.type_error(
                                    format!("no method '{}' on type '{}'", method, prim),
                                    span.clone(),
                                    TypeErrorKind::NoMethodFound,
                                );
                            }
                            return Type::Error;
                        }
                    } else {
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return Type::Error;
                    }
                } else if matches!(receiver_for_lookup, Type::Array { .. }) {
                    // A fixed-size `Array[T, N]` receiver whose method was not
                    // resolved by the modelled read arm (`len`/`is_empty`/`get`/
                    // `first`/`last`/`contains`), the iterator-source arm
                    // (`iter`/`into_iter`), or `as_slice`/`as_ptr`: the method is
                    // genuinely absent on both backends (`to_vec`/`slice`/`rev`
                    // are interp 'method not found'; a direct iterator adaptor
                    // `a.map(...)` miscompiles). Reject rather than the silent
                    // `Type::Error` (B-2026-07-17-19), with the same actionable
                    // `.iter()` hint Vec uses when the name is an iterator-
                    // surface method (a fixed array is iterable).
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    let mut msg = format!("no method '{}' on type 'Array'", method);
                    if Self::is_iterator_surface_method(method) {
                        let recv = match &object.kind {
                            ExprKind::Identifier(n) => n.clone(),
                            _ => "xs".to_string(),
                        };
                        msg.push_str(&format!(
                            ": iterator adaptors/terminals require an explicit `.iter()` — write `{}.iter().{}(...)`",
                            recv, method
                        ));
                    }
                    self.type_error(msg, span.clone(), TypeErrorKind::NoMethodFound);
                    return Type::Error;
                } else {
                    // For other non-named types (`String`/`bool`/`char` left on
                    // the historical silent fall-through — String has a large
                    // partially-implicit method surface not modelled in the
                    // impl table), just type-check args and return Error.
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
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
        let method_pick: Option<(ImplInfo, FunctionSig)> = if candidates.len() > 1 {
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
            candidates
                .into_iter()
                .next()
                .map(|(imp, sig)| (imp.clone(), sig.clone()))
        };

        match method_pick {
            Some((imp, sig)) => {
                // Validate labels against method parameter names
                self.validate_labels(args, &sig.param_names, span);
                // Pre-bind the impl's generic params to the receiver's
                // concrete type args (mirroring the concrete-type UFCS path
                // above) BEFORE solving the call args. Without this, a method
                // whose only `T`-position is the return type or a
                // closure-return param — e.g. `OnceLock[T].get_or_init(init:
                // Fn() -> T) -> ref T` — leaves `T` unsolved (nothing in the
                // non-closure args pins it), so the receiver's concrete
                // `[i64]` never reaches the signature and inference fails with
                // "cannot infer type parameter 'T'". Binding here makes the
                // value-receiver path consistent with UFCS dispatch; for a
                // non-generic receiver (empty `type_args` / no impl generics)
                // `recv_subs` is empty and behavior is unchanged.
                let recv_subs: HashMap<String, SubstValue> = imp
                    .generic_params
                    .as_ref()
                    .map(|gp| {
                        gp.params
                            .iter()
                            .zip(type_args.iter())
                            .map(|(p, t)| (p.name.clone(), SubstValue::Type(t.clone())))
                            .collect()
                    })
                    .unwrap_or_default();
                // Resolve `Self` in the signature to the concrete receiver
                // type. `recv_subs` only binds the impl's own generic params
                // (e.g. `T`); a method declared `-> Self` (or taking
                // `other: Self`) otherwise leaves `Self` unresolved at the call
                // site, so `a.m()` would type as `Self` and downstream field
                // access / codegen field-offset recovery fails (reads 0). In a
                // concrete-receiver dispatch `Self` always names the receiver's
                // type. (Self-receiver dispatch returned earlier at the
                // `TypeParam("Self")` arm, so `receiver_for_lookup` is concrete
                // here.)
                let params: Vec<Type> = sig
                    .params
                    .iter()
                    .map(|p| substitute_type_params(p, &recv_subs))
                    .map(|p| Self::resolve_self_in_type(p, &receiver_for_lookup))
                    .collect();
                let return_type = Self::resolve_self_in_type(
                    substitute_type_params(&sig.return_type, &recv_subs),
                    &receiver_for_lookup,
                );
                // Check argument count (excluding self)
                if args.len() != params.len() {
                    self.type_error(
                        format!(
                            "method '{}' expects {} argument(s), found {}",
                            method,
                            params.len(),
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return return_type;
                }
                // Reuse the round-10.1 closure-pushdown helper so any
                // remaining method-level generics solve from non-closure args
                // before checking closure args. `apply_call_site_marker` is
                // `false`: per design.md, the call-site `mut` marker rule
                // applies only to free-function calls, never to method calls.
                self.check_call_args_with_substitution(
                    args,
                    &params,
                    &return_type,
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
                // These prelude types have method surfaces EXHAUSTIVELY
                // resolved before this fall-through, so a method that reaches
                // here is genuinely absent. For `Option`/`Result` the surface
                // is small and every valid method (`unwrap`, `map`, `is_some`,
                // `ok_or`, `map_err`, …) resolves via a dedicated arm above or
                // a baked stdlib impl. For `Vec`/`VecDeque` the native
                // surface (`push`/`pop`/`get`/`len`/`sort`/`sum`/`join`/…)
                // resolves in dedicated arms, and the iterator ADAPTOR/TERMINAL
                // surface (`map`/`filter`/`collect`/`fold`/…) resolves only
                // through an explicit `.iter()` (the `Iterator[T]` dispatch
                // above) — a direct `v.map(...)` runs on NO backend
                // (interpreter: "method not found"; AOT: link/miscompile), so
                // accepting it was a pure check/execution hole
                // (B-2026-07-17-12, extended to `Tensor`/`DataFrame`).
                // For `Tensor`/`DataFrame` the numerical surface
                // (`reshape`/`zip_with`/`matmul`/`map`/`sum`/…) resolves in
                // `infer_tensor_*` / the dataframe arms; an absent name that
                // slips past them ran on no backend just the same. (The
                // fixed-size `Array[T, N]` type is a STRUCTURAL `Type::Array`,
                // not `Type::Named{"Array"}`, so it never reaches this
                // Named-keyed arm — its own unknown-method hole is a separate
                // follow-up.) The common
                // way to reach here is either that iter-less adaptor call or a
                // wrong-container call — invoking an inner-type method on an
                // un-unwrapped optional (`opt.len()`, `res.push(x)`,
                // `grid.get(i).len()` where `get` returns `Option[Vec[_]]`).
                // The silent fall-through poisoned those to `Type::Error`
                // (universally assignable), so they typechecked clean and then
                // detonated at runtime — an interpreter `unreachable!` (“the
                // typechecker accepted .len() on a type without one”) or a
                // codegen “no handler for method”. Reject them here like a
                // user-defined type, the same silent-poison tightening applied
                // to numeric receivers (B-2026-07-03-5) and user types.
                const EXHAUSTIVE_PRELUDE: &[&str] =
                    &["Option", "Result", "Vec", "VecDeque", "Tensor", "DataFrame"];
                let is_exhaustive_prelude = EXHAUSTIVE_PRELUDE.contains(&type_name.as_str());
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
                // A comptime-derived type (e.g. `#[derive(Message)]`) gains
                // methods only after typecheck, so its method set is open here —
                // suppress the not-found diagnostic for such types.
                // Before the generic "no method" message: the method may
                // genuinely EXIST on a matching impl that was FILTERED OUT of
                // `find_methods_with_args` because the receiver's element type
                // fails one of the impl's bounds (`impl[T: Ord] Trait for
                // Column[T]` invoked on `Column[f64]` — f64 is deliberately not
                // `Ord`). "no method 'span'" hides that; surface the failing
                // bound instead (reusing the float-`Ord`/`Eq`/`Hash` → wrapper
                // hint). This is the clarity B-2026-07-04-15 lacked — the
                // rejection there was CORRECT-BY-DESIGN, but read as a
                // container/monomorphization bug because the message named the
                // wrong problem.
                let bound_gate = self.env.impls.iter().find_map(|imp| {
                    if imp.target_type != type_name
                        || !imp.methods.contains_key(method)
                        || !super::types::impl_args_match(&imp.target_args, &type_args)
                    {
                        return None;
                    }
                    self.env
                        .first_unsatisfied_bound(imp, &type_args)
                        .map(|(pn, b, cty)| (imp.trait_name.clone(), pn, b, cty))
                });
                if let Some((trait_of_impl, param_name, bound, concrete)) = bound_gate {
                    let bound_trait = bound.path.last().cloned().unwrap_or_default();
                    let detail = self.render_unsatisfied_bound_message(
                        &param_name,
                        &bound_trait,
                        &concrete,
                        &bound,
                    );
                    let via = trait_of_impl
                        .map(|t| format!(" (via `impl {} for {}`)", t, type_name))
                        .unwrap_or_default();
                    let msg = format!(
                        "method '{}' is not callable on this `{}`{}: {}",
                        method, type_name, via, detail
                    );
                    self.type_error(msg, span.clone(), TypeErrorKind::NoMethodFound);
                    return Type::Error;
                }
                if (is_user_defined || is_exhaustive_prelude || method_on_other_specialization)
                    && !self.type_has_comptime_derive(&type_name)
                {
                    let mut msg = format!("no method '{}' on type '{}'", method, type_name);
                    // Iterator adaptors/terminals (`map`/`filter`/`collect`/…)
                    // are not methods on a `Vec`/`VecDeque` directly — they live
                    // on `Iterator[T]`, reached via `.iter()`. A direct
                    // `v.map(...)` reaches here (silent `Type::Error` pre-fix,
                    // runs on no backend). When the absent method IS an iterator
                    // method, the actionable fix is the `.iter()` hop, not an
                    // edit-distance neighbour — surface that instead
                    // (B-2026-07-17-12). Scoped to the iterable sequence types;
                    // `Tensor`/`DataFrame` are not `.iter()`-adapted, so an
                    // absent method there falls to the edit-distance suggestion.
                    if matches!(type_name.as_str(), "Vec" | "VecDeque")
                        && Self::is_iterator_surface_method(method)
                    {
                        let recv = match &object.kind {
                            ExprKind::Identifier(n) => n.clone(),
                            _ => "xs".to_string(),
                        };
                        msg.push_str(&format!(
                            ": iterator adaptors/terminals require an explicit `.iter()` — write `{}.iter().{}(...)`",
                            recv, method
                        ));
                    } else {
                        let candidates = self.env.collect_method_names(&type_name, &[]);
                        let candidate_refs: Vec<&str> =
                            candidates.iter().map(String::as_str).collect();
                        if let Some(suggestion) =
                            crate::edit_distance::suggest_similar(method, &candidate_refs)
                        {
                            msg.push_str(&format!(", did you mean '{}'?", suggestion));
                        }
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
            // `std.simd.math` transcendentals + rounding (phase-11 numerical
            // stdlib): element-wise `sqrt` / `exp` / `ln` / `tanh` / `sigmoid`
            // and `floor` / `ceil` / `round` / `trunc` on a FLOAT-element
            // vector, yielding the same `Vector[T, N]`. No arguments. Codegen
            // lowers `sqrt`/`exp`/`ln` and the four rounding ops to the
            // overloaded LLVM vector intrinsics and derives `sigmoid` / `tanh`
            // from `exp`; the interpreter computes them per lane.
            "sqrt" | "exp" | "ln" | "tanh" | "sigmoid" | "floor" | "ceil" | "round" | "trunc" => {
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
                if !matches!(elem, Type::Float(_)) {
                    self.type_error(
                        format!(
                            "'{}' requires a floating-point Vector element (f32 / f64); \
                             Vector element is '{}'",
                            method,
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                Type::Vector {
                    element: Box::new(elem),
                    lanes: lanes.clone(),
                }
            }
            // `std.simd.math` bit-reinterpretation (phase-11): element-wise
            // IEEE-754 bitcast between a float vector and a same-width integer
            // vector — the vector siblings of the scalar `to_bits` /
            // `bits_as_f*` surface, and the primitive the Sleef range-reduction
            // uses to build `2^n` from an integer exponent. No arguments;
            // codegen lowers each to a single LLVM vector `bitcast`.
            // `to_bits` on a float vector → the same-width UNSIGNED-int vector
            // (f32 → u32, f64 → u64); `bits_as_f32` / `bits_as_f64` are the
            // inverses, requiring a 32- / 64-bit integer element.
            "to_bits" => {
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
                let out_elem = match &elem {
                    Type::Float(FloatSize::F32) => Type::UInt(UIntSize::U32),
                    Type::Float(FloatSize::F64) => Type::UInt(UIntSize::U64),
                    _ => {
                        self.type_error(
                            format!(
                                "'to_bits' requires an f32 / f64 Vector element; \
                                 Vector element is '{}'",
                                type_display(&elem)
                            ),
                            span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                };
                Type::Vector {
                    element: Box::new(out_elem),
                    lanes: lanes.clone(),
                }
            }
            "bits_as_f32" | "bits_as_f64" => {
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
                let want_bits = if method == "bits_as_f32" { 32 } else { 64 };
                let elem_bits = match &elem {
                    Type::UInt(UIntSize::U32) | Type::Int(IntSize::I32) => 32,
                    Type::UInt(UIntSize::U64) | Type::Int(IntSize::I64) => 64,
                    _ => 0,
                };
                if elem_bits != want_bits {
                    self.type_error(
                        format!(
                            "'{}' requires a {}-bit integer Vector element (i{} / u{}); \
                             Vector element is '{}'",
                            method,
                            want_bits,
                            want_bits,
                            want_bits,
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                let out = if method == "bits_as_f32" {
                    FloatSize::F32
                } else {
                    FloatSize::F64
                };
                Type::Vector {
                    element: Box::new(Type::Float(out)),
                    lanes: lanes.clone(),
                }
            }
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
                        "Vec" | "VecDeque" | "Map" | "SortedMap" | "Set" | "SortedSet"
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

    /// Type `gpu.dispatch(kernel, buffer)` (GPU spike slice-0c) as the
    /// element-wise map it is: `kernel` must be a bare identifier naming a
    /// `#[gpu] fn(f32) -> f32`, `buffer` must be `Vec[f32]`, and the result is
    /// a fresh `Vec[f32]` of the same length. On the happy path the WGSL shader
    /// `gpu_wgsl::emit_kernel` produces is stashed in `gpu_dispatch_wgsl` keyed
    /// on the kernel argument's span so codegen can bake it without re-walking
    /// the AST (codegen-containment: the `ast`-importing emit stays out of
    /// `codegen.rs`). Every rejection is a focused `E_GPU_DISPATCH_*`
    /// diagnostic; the return type stays `Vec[f32]` so downstream inference has
    /// something concrete to work with even on error.
    /// Type `critical_section.acquire()` (design.md § Critical sections). Takes
    /// no arguments and returns the RAII `CriticalSectionGuard`. The guard's
    /// Drop (hand-rolled in codegen) re-enables interrupts at end of scope; the
    /// `writes(Hardware)` effect is contributed by the effectchecker's
    /// `critical_section.acquire` seed (mirroring the `volatile_*` MMIO seeds).
    fn infer_critical_section_acquire(&mut self, args: &[CallArg], span: &Span) -> Type {
        let guard = Type::Named {
            name: "CriticalSectionGuard".to_string(),
            args: vec![],
        };
        if !args.is_empty() {
            self.type_error(
                format!(
                    "`critical_section.acquire` takes no arguments, found {}",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            // Still type the args so downstream diagnostics don't cascade.
            for arg in args {
                self.infer_expr(&arg.value);
            }
        }
        self.record_expr_type(span, &guard);
        guard
    }

    /// `gpu.upload(vec)` (GPU-SLIP-4b-2): move a SoA `Vec[S]` to a resident GPU
    /// buffer and return an owned `GpuBuffer[S]` handle. `vec` must be a bare
    /// binding carrying a `layout` block over an all-`f32` struct `S` (the same
    /// buffer shape `gpu.dispatch` accepts) — codegen reads its per-group
    /// pointers and calls `karac_runtime_gpu_upload_soa`. The `Vec` is MOVED
    /// (the magic-module arg default), so the host binding is consumed. No WGSL
    /// is baked (a pure memory transfer, no kernel).
    fn infer_gpu_upload(&mut self, args: &[CallArg], span: &Span) -> Type {
        fn te_name(ty: &TypeExpr) -> Option<&str> {
            match &ty.kind {
                TypeKind::Path(p) if p.generic_args.is_none() && p.segments.len() == 1 => {
                    Some(p.segments[0].as_str())
                }
                _ => None,
            }
        }
        let gpu_buffer = |elem: Type| Type::Named {
            name: "GpuBuffer".to_string(),
            args: vec![elem],
        };
        if args.len() != 1 {
            self.type_error(
                format!(
                    "error[E_GPU_UPLOAD_ARITY]: `gpu.upload` takes exactly one buffer — \
                     `gpu.upload(vec)` (found {} argument(s))",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            return gpu_buffer(Type::Error);
        }
        let buf_ty = self.infer_expr(&args[0].value);
        // Must be `Vec[S]` over a nullary struct name.
        let struct_ty = match &buf_ty {
            Type::Named { name, args: ta }
                if name == "Vec"
                    && ta.len() == 1
                    && matches!(&ta[0], Type::Named { args: sa, .. } if sa.is_empty()) =>
            {
                ta[0].clone()
            }
            _ => {
                self.type_error(
                    "error[E_GPU_UPLOAD_BUFFER]: `gpu.upload` requires a `Vec[S]` over a struct \
                     element with a `layout` block"
                        .to_string(),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return gpu_buffer(Type::Error);
            }
        };
        let Type::Named {
            name: struct_name, ..
        } = &struct_ty
        else {
            return gpu_buffer(Type::Error);
        };
        // The element must be an all-`f32` struct (the decided GPU precision).
        let sdef = self.program.items.iter().find_map(|it| match it {
            Item::StructDef(s) if s.name == *struct_name => Some(s),
            _ => None,
        });
        match sdef {
            Some(s) if s.fields.iter().all(|f| te_name(&f.ty) == Some("f32")) => {}
            Some(_) => {
                self.type_error(
                    format!(
                        "error[E_GPU_UPLOAD_BUFFER]: every field of `{struct_name}` must be `f32` \
                         (the decided GPU precision) to `gpu.upload`"
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return gpu_buffer(struct_ty);
            }
            None => {
                self.type_error(
                    format!(
                        "error[E_GPU_UPLOAD_BUFFER]: `gpu.upload` buffer element `{struct_name}` \
                         is not a struct"
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return gpu_buffer(struct_ty);
            }
        }
        // The buffer must be a bare binding with a matching `layout` block —
        // codegen reads the per-group pointers by name (`active_soa_layout`).
        let ExprKind::Identifier(buf_name) = &args[0].value.kind else {
            self.type_error(
                "error[E_GPU_UPLOAD_BUFFER]: `gpu.upload` requires a bare binding carrying a \
                 `layout` block (bind the buffer to a `let` first)"
                    .to_string(),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return gpu_buffer(struct_ty);
        };
        if !self
            .program
            .items
            .iter()
            .any(|it| matches!(it, Item::LayoutDef(l) if l.name == *buf_name))
        {
            self.type_error(
                format!(
                    "error[E_GPU_UPLOAD_BUFFER]: `gpu.upload` requires a `layout` block for \
                     `{buf_name}` (each field group becomes a GPU buffer)"
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return gpu_buffer(struct_ty);
        }
        gpu_buffer(struct_ty)
    }

    /// `gpu.download(buf)` (GPU-SLIP-4b-2): move a `GpuBuffer[S]` handle back to
    /// a host `Vec[S]`. The handle is MOVED (consumed) — the magic-module arg
    /// default — so it cannot be used again and its scope-exit free is
    /// suppressed. Returns `Vec[S]`; codegen calls `karac_runtime_gpu_download_soa`
    /// and (if the receiving binding is a SoA `layout`) scatters the AoS result
    /// into its per-group buffers, exactly like a `gpu.dispatch` result.
    fn infer_gpu_download(&mut self, args: &[CallArg], span: &Span) -> Type {
        let vec_of = |elem: Type| Type::Named {
            name: "Vec".to_string(),
            args: vec![elem],
        };
        if args.len() != 1 {
            self.type_error(
                format!(
                    "error[E_GPU_DOWNLOAD_ARITY]: `gpu.download` takes exactly one buffer handle — \
                     `gpu.download(buf)` (found {} argument(s))",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            return vec_of(Type::Error);
        }
        let buf_ty = self.infer_expr(&args[0].value);
        match &buf_ty {
            Type::Named { name, args: ta } if name == "GpuBuffer" && ta.len() == 1 => {
                vec_of(ta[0].clone())
            }
            _ => {
                self.type_error(
                    "error[E_GPU_DOWNLOAD_BUFFER]: `gpu.download` requires a `GpuBuffer[S]` handle \
                     (the result of `gpu.upload` / `gpu.dispatch`)"
                        .to_string(),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                vec_of(Type::Error)
            }
        }
    }

    fn infer_gpu_dispatch(&mut self, args: &[CallArg], span: &Span) -> Type {
        // The WGSL-native 4-byte scalar element types slice-0 supports, and
        // their shared Kāra/WGSL spelling. `None` for any other type.
        fn elem_spelling(t: &Type) -> Option<&'static str> {
            match t {
                Type::Float(FloatSize::F32) => Some("f32"),
                Type::Int(IntSize::I32) => Some("i32"),
                Type::UInt(UIntSize::U32) => Some("u32"),
                _ => None,
            }
        }
        // The single-segment scalar name of a kernel parameter's `TypeExpr`.
        fn typeexpr_scalar(ty: &TypeExpr) -> Option<&str> {
            match &ty.kind {
                TypeKind::Path(p) if p.generic_args.is_none() && p.segments.len() == 1 => {
                    Some(p.segments[0].as_str())
                }
                _ => None,
            }
        }
        let vec_of = |elem: Type| Type::Named {
            name: "Vec".to_string(),
            args: vec![elem],
        };

        if args.len() < 2 {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_ARITY]: `gpu.dispatch` takes a kernel and a buffer \
                     (and, for a struct buffer, scalar uniforms) — \
                     `gpu.dispatch(kernel, buffer, ...)` (found {} argument(s))",
                    args.len()
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            return vec_of(Type::Float(FloatSize::F32));
        }

        // Buffer (arg 1): `Vec[f32|i32|u32]` (scalar element-wise map) OR — CG-4 —
        // `Vec[S]` over a POD `f32` struct `S` with a `layout` block (multi-buffer,
        // one coalesced GPU buffer per group). Infer it so its element type is
        // recorded for codegen's element-typed read + the result type.
        let buf_ty = self.infer_expr(&args[1].value);
        // A `Vec[struct]` element (`Type::Named`, non-scalar) routes to the CG-4
        // struct path; scalars are `Type::Float`/`Int`/`UInt`, never `Named`.
        if let Type::Named { name, args: ta } = &buf_ty {
            if name == "Vec" && ta.len() == 1 {
                if let Type::Named {
                    name: sname,
                    args: sa,
                } = &ta[0]
                {
                    if sa.is_empty() {
                        let struct_ty = ta[0].clone();
                        let sname = sname.clone();
                        return self.infer_gpu_dispatch_soa(args, span, &struct_ty, &sname);
                    }
                }
            }
        }
        // GPU-SLIP-4b-2b: a `GpuBuffer[S]` buffer arg is a RESIDENT dispatch —
        // device→device, borrowing the handle and returning a fresh `GpuBuffer[S]`
        // (no host round-trip). The kernel validation is the SoA path's; only the
        // buffer form + result type differ.
        if let Type::Named { name, args: ta } = &buf_ty {
            if name == "GpuBuffer" && ta.len() == 1 {
                if let Type::Named {
                    name: sname,
                    args: sa,
                } = &ta[0]
                {
                    if sa.is_empty() {
                        let struct_ty = ta[0].clone();
                        let sname = sname.clone();
                        return self.infer_gpu_dispatch_resident(args, span, &struct_ty, &sname);
                    }
                }
            }
        }
        // The scalar element-wise-map path takes no uniforms (GPU-LBM-2 is
        // struct-only) — extra arguments require a struct buffer.
        if args.len() != 2 {
            self.type_error(
                "error[E_GPU_DISPATCH_ARITY]: extra `gpu.dispatch` arguments (scalar uniforms) \
                 require a struct buffer with a `layout` block"
                    .to_string(),
                args[2].value.span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            return vec_of(Type::Float(FloatSize::F32));
        }
        let elem = match &buf_ty {
            Type::Named { name, args: ta } if name == "Vec" && ta.len() == 1 => {
                elem_spelling(&ta[0]).map(|s| (s, ta[0].clone()))
            }
            _ => None,
        };
        let (elem_spell, elem_ty) = match elem {
            Some(e) => e,
            None => {
                if buf_ty != Type::Error {
                    self.type_error(
                        format!(
                            "error[E_GPU_DISPATCH_BUFFER]: `gpu.dispatch` buffer must be \
                             `Vec[f32]`, `Vec[i32]`, or `Vec[u32]` in slice-0, found `{}`",
                            type_display(&buf_ty)
                        ),
                        args[1].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                // Element undetermined — default the result element to f32 so
                // downstream inference still has a concrete type.
                ("f32", Type::Float(FloatSize::F32))
            }
        };
        let result_vec = vec_of(elem_ty);

        // Kernel (arg 0): a bare identifier naming a `#[gpu] fn(T) -> T`.
        let ExprKind::Identifier(kernel_name) = &args[0].value.kind else {
            self.type_error(
                "error[E_GPU_DISPATCH_KERNEL]: the `gpu.dispatch` kernel must be a bare \
                 `#[gpu]` function name in slice-0"
                    .to_string(),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        };

        // `self.program` is `&'a Program` (not borrowed from `&self`), so
        // copying the reference lets the kernel lookup + WGSL emit run without
        // conflicting with the `&mut self` diagnostic/table writes below.
        let program = self.program;
        let kernel = program.items.iter().find_map(|item| match item {
            Item::Function(f) if f.name == *kernel_name && f.is_gpu => Some(f),
            _ => None,
        });
        let Some(kernel) = kernel else {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_KERNEL]: no `#[gpu]` function named `{kernel_name}` \
                     is in scope for `gpu.dispatch`"
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        };

        // The kernel element must match the buffer element — the byte-oriented
        // dispatch reinterprets the buffer bytes as the shader's `array<T>`, so
        // an `i32` kernel over a `Vec[f32]` buffer would silently miscompute.
        if let Some(kernel_elem) = kernel.params.first().and_then(|p| typeexpr_scalar(&p.ty)) {
            if kernel_elem != elem_spell {
                self.type_error(
                    format!(
                        "error[E_GPU_DISPATCH_KERNEL]: kernel `{kernel_name}` maps `{kernel_elem}` \
                         but the buffer is `Vec[{elem_spell}]` — the element types must match"
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return result_vec;
            }
        }

        // Other `#[gpu]` functions are candidate helpers the kernel may call
        // (GPU-LBM-5); the emitter selects the reachable ones.
        let helpers: Vec<&Function> = program
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Function(f) if f.is_gpu && f.name != *kernel_name => Some(f),
                _ => None,
            })
            .collect();
        match crate::gpu_wgsl::emit_kernel(kernel, &helpers) {
            Ok(wgsl) => {
                self.gpu_dispatch_wgsl.insert(
                    SpanKey(args[0].value.span.offset, args[0].value.span.length),
                    wgsl,
                );
            }
            Err(e) => {
                self.type_error(
                    format!(
                        "error[E_GPU_DISPATCH_KERNEL]: cannot lower `{kernel_name}` to a GPU \
                         shader — {}",
                        e.reason()
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }

        self.record_expr_type(span, &result_vec);
        result_vec
    }

    /// CG-4 struct-buffer path for `gpu.dispatch(kernel, buffer)`: a `Vec[S]`
    /// over a POD all-`f32` struct `S` bound with a `layout` block dispatches as
    /// a multi-buffer kernel — one coalesced GPU buffer per layout group. The
    /// typechecker is **layout-blind**, so it only *validates* here; codegen
    /// (which owns the SoA layout via `active_soa_layout`) emits the per-group
    /// WGSL, so nothing is baked into `gpu_dispatch_wgsl`. Returns `Vec[S]`.
    fn infer_gpu_dispatch_soa(
        &mut self,
        args: &[CallArg],
        span: &Span,
        struct_ty: &Type,
        struct_name: &str,
    ) -> Type {
        // Single-segment type name of a `TypeExpr` (`Particle`, `f32`, …).
        fn te_name(ty: &TypeExpr) -> Option<&str> {
            match &ty.kind {
                TypeKind::Path(p) if p.generic_args.is_none() && p.segments.len() == 1 => {
                    Some(p.segments[0].as_str())
                }
                _ => None,
            }
        }
        // The element type name of a `Vec[S]` — a stencil kernel's whole-buffer
        // parameter (GPU-LBM-6). `None` for any non-`Vec` / non-plain-element type.
        fn te_vec_elem(ty: &TypeExpr) -> Option<&str> {
            let TypeKind::Path(p) = &ty.kind else {
                return None;
            };
            if p.segments.len() != 1 || p.segments[0] != "Vec" {
                return None;
            }
            match p.generic_args.as_deref() {
                Some([GenericArg::Type(elem)]) => te_name(elem),
                _ => None,
            }
        }
        let result_vec = Type::Named {
            name: "Vec".to_string(),
            args: vec![struct_ty.clone()],
        };
        let program = self.program;

        // The element must be a real struct whose fields are all `f32` (the
        // decided GPU precision; one WGSL `array<f32>` binding per group).
        let sdef = program.items.iter().find_map(|it| match it {
            Item::StructDef(s) if s.name == *struct_name => Some(s),
            _ => None,
        });
        let Some(sdef) = sdef else {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_BUFFER]: `gpu.dispatch` buffer element `{struct_name}` \
                     is not a struct"
                ),
                args[1].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        };
        if !sdef.fields.iter().all(|f| te_name(&f.ty) == Some("f32")) {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_BUFFER]: a struct `gpu.dispatch` buffer requires every \
                     field of `{struct_name}` to be `f32` (the decided GPU precision)"
                ),
                args[1].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        }

        // The buffer must be a bare binding with a matching `layout` block — the
        // SoA group structure is what maps to per-field GPU buffers. Name-match
        // on `program.items`; codegen resolves the physical layout by the same
        // name (`active_soa_layout`).
        let ExprKind::Identifier(buf_name) = &args[1].value.kind else {
            self.type_error(
                "error[E_GPU_DISPATCH_BUFFER]: a struct `gpu.dispatch` buffer must be a bare \
                 binding carrying a `layout` block"
                    .to_string(),
                args[1].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        };
        if !program
            .items
            .iter()
            .any(|it| matches!(it, Item::LayoutDef(l) if l.name == *buf_name))
        {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_BUFFER]: `gpu.dispatch` on a struct buffer requires a \
                     `layout` block for `{buf_name}` (each field group becomes a GPU buffer)"
                ),
                args[1].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        }

        // Kernel: a bare `#[gpu] fn(S) -> S` over the same struct.
        let ExprKind::Identifier(kernel_name) = &args[0].value.kind else {
            self.type_error(
                "error[E_GPU_DISPATCH_KERNEL]: the `gpu.dispatch` kernel must be a bare `#[gpu]` \
                 function name"
                    .to_string(),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        };
        let kernel = program.items.iter().find_map(|it| match it {
            Item::Function(f) if f.name == *kernel_name && f.is_gpu => Some(f),
            _ => None,
        });
        let Some(kernel) = kernel else {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_KERNEL]: no `#[gpu]` function named `{kernel_name}` is \
                     in scope for `gpu.dispatch`"
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        };
        // Two kernel shapes over a struct buffer:
        //  • element-wise — `fn k(x: S, ...uniforms) -> S`: first param is the
        //    element `S`; uniforms follow (GPU-LBM-2).
        //  • stencil (GPU-LBM-6) — `fn k(buf: Vec[S], i: <int>, ...uniforms) -> S`:
        //    first param is the whole buffer, second an integer index the thread
        //    fills; the body reads neighbours `buf[j].field`. Uniforms follow.
        let first_is_elem = kernel.params.first().and_then(|p| te_name(&p.ty)) == Some(struct_name);
        let is_stencil = !first_is_elem
            && kernel.params.first().and_then(|p| te_vec_elem(&p.ty)) == Some(struct_name);
        let uniform_start = if is_stencil { 2 } else { 1 };
        let n_uniform_params = kernel.params.len().saturating_sub(uniform_start);
        // A stencil's index parameter must be an integer.
        let index_ok = !is_stencil
            || matches!(
                kernel.params.get(1).and_then(|p| te_name(&p.ty)),
                Some("i64" | "i32" | "u64" | "u32" | "usize" | "isize")
            );
        let shape_ok = first_is_elem || is_stencil;
        let uniforms_ok = kernel
            .params
            .get(uniform_start..)
            .unwrap_or(&[])
            .iter()
            .all(|p| te_name(&p.ty) == Some("f32"));
        let ret_ok = kernel.return_type.as_ref().and_then(te_name) == Some(struct_name);
        if !shape_ok || !index_ok || !uniforms_ok || !ret_ok {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_KERNEL]: kernel `{kernel_name}` must be either \
                     `{struct_name} -> {struct_name}` (element-wise) or \
                     `Vec[{struct_name}], <int index> -> {struct_name}` (stencil), with any extra \
                     params `f32` uniforms"
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return result_vec;
        }
        // The dispatch's extra args (beyond kernel + buffer) are the uniform
        // values — count must match the kernel's uniform params, each `f32`.
        let n_uniform_args = args.len() - 2;
        if n_uniform_args != n_uniform_params {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_ARITY]: kernel `{kernel_name}` takes \
                     {n_uniform_params} uniform(s) but {n_uniform_args} were passed"
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            return result_vec;
        }
        for ua in &args[2..] {
            let ut = self.infer_expr(&ua.value);
            // Any float is accepted — a bare literal (`10.0`) defaults to `f64`;
            // codegen narrows it to `f32` for the uniform buffer.
            if !matches!(ut, Type::Float(_)) && ut != Type::Error {
                self.type_error(
                    "error[E_GPU_DISPATCH_UNIFORM]: `gpu.dispatch` uniform arguments must be a \
                     float (`f32`/`f64`)"
                        .to_string(),
                    ua.value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }

        // No WGSL bake: the typechecker is layout-blind. Codegen emits the
        // per-group multi-buffer shader via `active_soa_layout` +
        // `gpu_wgsl::emit_kernel_soa`.
        self.record_expr_type(span, &result_vec);
        result_vec
    }

    /// GPU-SLIP-4b-2b: `gpu.dispatch(kernel, buf: GpuBuffer[S], uniforms…)` — a
    /// RESIDENT dispatch. The buffer stays on the device; the kernel runs
    /// device→device and returns a fresh `GpuBuffer[S]` (no host round-trip). Same
    /// kernel validation as the SoA round-trip path; only the buffer form + result
    /// type differ. The buffer is BORROWED (owner-decided), so it can feed the
    /// next dispatch or be downloaded. Codegen emits `karac_runtime_gpu_dispatch_resident`.
    fn infer_gpu_dispatch_resident(
        &mut self,
        args: &[CallArg],
        span: &Span,
        struct_ty: &Type,
        struct_name: &str,
    ) -> Type {
        fn te_name(ty: &TypeExpr) -> Option<&str> {
            match &ty.kind {
                TypeKind::Path(p) if p.generic_args.is_none() && p.segments.len() == 1 => {
                    Some(p.segments[0].as_str())
                }
                _ => None,
            }
        }
        fn te_vec_elem(ty: &TypeExpr) -> Option<&str> {
            let TypeKind::Path(p) = &ty.kind else {
                return None;
            };
            if p.segments.len() != 1 || p.segments[0] != "Vec" {
                return None;
            }
            match p.generic_args.as_deref() {
                Some([GenericArg::Type(elem)]) => te_name(elem),
                _ => None,
            }
        }
        let gpu_buffer = Type::Named {
            name: "GpuBuffer".to_string(),
            args: vec![struct_ty.clone()],
        };
        let program = self.program;
        let ExprKind::Identifier(kernel_name) = &args[0].value.kind else {
            self.type_error(
                "error[E_GPU_DISPATCH_KERNEL]: the `gpu.dispatch` kernel must be a bare `#[gpu]` \
                 function name"
                    .to_string(),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return gpu_buffer;
        };
        let kernel = program.items.iter().find_map(|it| match it {
            Item::Function(f) if f.name == *kernel_name && f.is_gpu => Some(f),
            _ => None,
        });
        let Some(kernel) = kernel else {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_KERNEL]: no `#[gpu]` function named `{kernel_name}` is \
                     in scope for `gpu.dispatch`"
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return gpu_buffer;
        };
        let first_is_elem = kernel.params.first().and_then(|p| te_name(&p.ty)) == Some(struct_name);
        let is_stencil = !first_is_elem
            && kernel.params.first().and_then(|p| te_vec_elem(&p.ty)) == Some(struct_name);
        let uniform_start = if is_stencil { 2 } else { 1 };
        let n_uniform_params = kernel.params.len().saturating_sub(uniform_start);
        let index_ok = !is_stencil
            || matches!(
                kernel.params.get(1).and_then(|p| te_name(&p.ty)),
                Some("i64" | "i32" | "u64" | "u32" | "usize" | "isize")
            );
        let shape_ok = first_is_elem || is_stencil;
        let uniforms_ok = kernel
            .params
            .get(uniform_start..)
            .unwrap_or(&[])
            .iter()
            .all(|p| te_name(&p.ty) == Some("f32"));
        let ret_ok = kernel.return_type.as_ref().and_then(te_name) == Some(struct_name);
        if !shape_ok || !index_ok || !uniforms_ok || !ret_ok {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_KERNEL]: kernel `{kernel_name}` must be `fn(S[, i]) -> S` \
                     over `{struct_name}` with `f32` uniforms for a resident `gpu.dispatch`"
                ),
                args[0].value.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return gpu_buffer;
        }
        if args.len().saturating_sub(2) != n_uniform_params {
            self.type_error(
                format!(
                    "error[E_GPU_DISPATCH_ARITY]: kernel `{kernel_name}` takes {n_uniform_params} \
                     uniform(s) but got {}",
                    args.len().saturating_sub(2)
                ),
                span.clone(),
                TypeErrorKind::WrongNumberOfArgs,
            );
            return gpu_buffer;
        }
        for ua in args.iter().skip(2) {
            let ut = self.infer_expr(&ua.value);
            if !matches!(ut, Type::Float(_)) {
                self.type_error(
                    "error[E_GPU_DISPATCH_UNIFORM]: `gpu.dispatch` uniform arguments must be `f32`"
                        .to_string(),
                    ua.value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        self.record_expr_type(span, &gpu_buffer);
        gpu_buffer
    }

    // ── Field Access ────────────────────────────────────────────
}
