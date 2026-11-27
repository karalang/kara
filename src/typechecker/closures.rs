//! Closure-capture inference and the once-callability walker.
//!
//! Houses `closure_type_with_capture_inference` (the driver that
//! computes a closure's `Fn` vs. `OnceFn` type from its body),
//! `closure_consumes_captured_non_copy` (the use-predicate scan),
//! the recursive `walk_capture_consume` / `walk_capture_consume_block`
//! walkers that track whether outer-captured non-Copy bindings are
//! consumed, and supporting helpers (`name_is_shadowed`,
//! `flatten_local_scope_snapshot`, `is_copy_type_during_check`).
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use std::collections::{HashMap, HashSet};

use super::inference::resolve_type_var_top;
use super::types::Type;
use super::{CaptureWalkMode, OnceReason};

impl<'a> super::TypeChecker<'a> {
    pub(super) fn flatten_local_scope_snapshot(&self) -> HashMap<String, Type> {
        let mut out: HashMap<String, Type> = HashMap::new();
        for scope in &self.local_scope.scopes {
            for (k, v) in scope {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// A `shared` / `par` struct or enum handle, or an `Rc` / `Arc`: a read
    /// copies the handle and counts it (core-semantics §6.1).
    pub(super) fn is_shared_handle(&self, ty: &Type) -> bool {
        match ty {
            Type::Shared(_) | Type::Rc(_) | Type::Arc(_) => true,
            Type::Named { name, .. } => {
                self.env
                    .structs
                    .get(name)
                    .is_some_and(|s| s.is_shared || s.is_par)
                    || self
                        .env
                        .enums
                        .get(name)
                        .is_some_and(|s| s.is_shared || s.is_par)
            }
            _ => false,
        }
    }

    /// Lightweight Copy classification using the in-progress type env.
    /// Mirrors `ownership::is_copy_type` but reads from `self.env.structs`
    /// / `self.env.enums` / `self.env.distinct_types` directly — the
    /// typechecker is mid-build, so the canonical `TypeCheckResult` does
    /// not yet exist. Used by the once-callability walker to decide
    /// whether a captured outer binding's type is Copy (no consume
    /// possible) or non-Copy (consume promotes the closure to OnceFn).
    /// Whether trait `name` is `Copy` or has it among its supertraits, at
    /// any depth.
    pub(super) fn trait_is_copy(&self, name: &str) -> bool {
        let mut seen: Vec<&str> = Vec::new();
        let mut stack = vec![name];
        while let Some(n) = stack.pop() {
            if n == "Copy" {
                return true;
            }
            if seen.contains(&n) {
                continue;
            }
            seen.push(n);
            if let Some(t) = self.env.traits.get(n) {
                stack.extend(t.supertraits.iter().map(String::as_str));
            }
        }
        false
    }

    /// Every trait [`Self::trait_is_copy`] holds of, for
    /// `TypeCheckResult::copy_traits`.
    pub(super) fn copy_traits(&self) -> rustc_hash::FxHashSet<String> {
        self.env
            .traits
            .keys()
            .filter(|n| self.trait_is_copy(n))
            .cloned()
            .collect()
    }

    pub(super) fn is_copy_type_during_check(&self, ty: &Type) -> bool {
        // A number whose type a later use has yet to fix is a number either
        // way (B-2026-11-07-3).
        if let Type::TypeVar(id) =
            super::inference::resolve_type_var_top(ty, &self.env.substitutions)
        {
            if super::inference::literal_var_kind(id).is_some() {
                return true;
            }
        }
        if let Type::TypeVar(_) = ty {
            let now = self.resolved_now(ty);
            if !matches!(now, Type::TypeVar(_)) {
                return self.is_copy_type_during_check(&now);
            }
        }
        if matches!(
            ty,
            Type::Int(_)
                | Type::UInt(_)
                | Type::Float(_)
                | Type::Bool
                | Type::Char
                | Type::Unit
                | Type::Never
                | Type::Error
        ) {
            return true;
        }
        // B-2026-09-02-19 — a generic parameter declared `T: Copy` IS `Copy`
        // inside its own body, and the bound is checked at every
        // instantiation, so nothing reaches codegen that the bound does not
        // already guarantee. Without this the declared bound was simply not
        // consulted here: `fn f[T: Copy](v: ref Vec[T]) -> T { let a = v[0]; }`
        // was rejected by `E_INDEX_MOVE_NON_COPY` with "this element type is
        // not `Copy`" — a message contradicting the signature one line above
        // it, and contradicting design.md § Deref, which says `v[i]` "requires
        // `T: Copy`". Both spellings of the parameter are accepted (the
        // Named-vs-TypeParam trap; see `type_param_has_trait_bound`), which is
        // what also covers a `Vec[T]` written as a body annotation.
        if self.type_param_has_copy_bound(ty) {
            return true;
        }
        match ty {
            Type::Tuple(types) => types.iter().all(|t| self.is_copy_type_during_check(t)),
            Type::Array { element, .. } => self.is_copy_type_during_check(element),
            Type::Slice { mutable, .. } => !mutable,
            // A raw pointer is `Copy` (design.md §15), as `is_type_copy`
            // already says: `.raw()` of a borrowed `distinct type SafeFile =
            // *mut CFile` copies the pointer rather than borrowing it
            // (B-2026-11-22-22).
            Type::Pointer { .. } => true,
            // design.md § `impl Trait`: a bound that is itself `Copy` (a
            // trait that is `Copy` or has it as a supertrait) makes the value
            // `Copy` (B-2026-11-24-19).
            Type::Existential { trait_name, .. } => {
                self.trait_is_copy(trait_name.rsplit('.').next().unwrap_or(trait_name))
            }
            // B-2026-10-14-10 — v2 core §1.1: a `ref T` is `Copy`, so
            // `let a = v[0];` over a `Vec[ref Node]` copies the reference.
            // Strict commands only, like `StringSlice` below.
            Type::Ref(_) => self.cli_lint_overrides.strict_core || self.library_methods_from_source,
            Type::Named { name, args } => {
                if matches!(name.as_str(), "Option" | "Result") {
                    return args.iter().all(|a| self.is_copy_type_during_check(a));
                }
                // v2 core §5.2: `StringSlice` is `Str`, a shared view, which
                // copies as the reference it is.
                if name == "StringSlice"
                    && (self.cli_lint_overrides.strict_core || self.library_methods_from_source)
                {
                    return true;
                }
                if let Some(info) = self.env.structs.get(name) {
                    info.derived_traits.contains("Copy")
                } else if let Some(info) = self.env.enums.get(name) {
                    info.derived_traits.contains("Copy")
                } else if let Some(traits) = self.env.distinct_types.get(name) {
                    traits.contains("Copy")
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Decide the closure expression's type based on capture-mode prefix
    /// and a body walk for capture-consumes. Round 12.44 (Step 2) wires
    /// this in BOTH the synth path (`infer_expr`'s `Closure` arm) and the
    /// expected-type pushdown (`check_expr`'s `Closure` arm) so the type
    /// the typechecker assigns to a closure expression reflects whether
    /// it consumes a captured outer non-Copy binding.
    ///
    /// `Some(CaptureMode::Ref)` / `Some(CaptureMode::MutRef)` force
    /// `Type::Function` regardless of body — the explicit prefix is
    /// the user's promise that captures are borrowed, never moved
    /// (matches round 12.6's repeatable-closure rule). `None` /
    /// `Some(CaptureMode::Own)` (capture-by-ownership) walk the body.
    ///
    /// Round 12.45 (Step 3): when the walk produces a reason, the reason
    /// is recorded in `closure_once_reasons` keyed by the closure-expr
    /// span so the slot-rejection diagnostic can name the consumed binding.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn closure_type_with_capture_inference(
        &mut self,
        closure_span: &Span,
        capture_mode: Option<CaptureMode>,
        closure_param_names: &[String],
        body: &Expr,
        outer_bindings: &HashMap<String, Type>,
        param_types: Vec<Type>,
        body_ty: Type,
    ) -> Type {
        // FE-3c — inside a `#[gpu]` function, reject a closure that captures
        // host (non-`GpuSafe`) state. Runs here (the single closure-typing
        // chokepoint) so both the synth and expected-type paths are covered;
        // the capture set is computed shadow-aware from `outer_bindings`.
        if self.current_fn_is_gpu {
            self.check_gpu_closure_captures(
                closure_span,
                closure_param_names,
                body,
                outer_bindings,
            );
        }

        let return_type = Box::new(body_ty);
        let force_repeatable = matches!(
            capture_mode,
            Some(CaptureMode::Ref) | Some(CaptureMode::MutRef)
        );
        let reason = if force_repeatable {
            None
        } else {
            self.closure_consumes_captured_non_copy(
                body,
                closure_param_names,
                &param_types,
                outer_bindings,
            )
        };
        match reason {
            Some(r) => {
                self.closure_once_reasons
                    .insert(SpanKey::from_span(closure_span), r);
                Type::OnceFunction {
                    cross_task: Default::default(),
                    params: param_types,
                    return_type,
                }
            }
            None => Type::Function {
                cross_task: Default::default(),
                params: param_types,
                return_type,
            },
        }
    }

    /// Returns `true` iff `body` consumes at least one captured outer
    /// non-Copy binding — the criterion that flips a closure's type from
    /// `Function` to `OnceFunction`. Mirrors the legacy ownership-side
    /// detection (`use_classifier::once_callable_closures`, populated
    /// when a `let p = closure_expr;` body walk produces a
    /// `ConsumeOrigin::ClosureCapture`-tagged consume) directly inside
    /// the typechecker, so the closure expression's inferred type stays
    /// self-consistent without a cross-phase rewrite of `expr_types`.
    ///
    /// `outer_bindings` is the `flatten_local_scope_snapshot` taken just
    /// BEFORE the closure pushes its own param scope. Closure params
    /// themselves are pushed onto the shadow stack (`closure_param_names`)
    /// so a body identifier matching a param name isn't mistaken for a
    /// capture. Body-local `let`/`for`/`match`/`if let` bindings push
    /// their own shadow scopes during the walk.
    ///
    /// The walker tracks Reading vs Consuming mode mirroring
    /// `use_classifier::walk_expr`. Owned-arg slots in `Call` (decided
    /// by the callee's signature) and the owned positions in
    /// `MethodCall` / `StructLiteral` / `Return(Some)` / `Question` /
    /// `Break(Some)` flip into Consuming. An identifier-leaf in
    /// Consuming mode whose name resolves to an outer non-Copy binding
    /// flags the closure as once-callable. The `MethodCall` receiver is
    /// walked in Reading mode (a conservative simplification — the
    /// typechecker doesn't currently track per-method `SelfParam` modes;
    /// the classifier on the ownership side does, and Step 3 closes any
    /// remaining slot-rejection gap).
    pub(super) fn closure_consumes_captured_non_copy(
        &self,
        body: &Expr,
        closure_param_names: &[String],
        closure_param_types: &[Type],
        outer_bindings: &HashMap<String, Type>,
    ) -> Option<OnceReason> {
        let mut shadow_stack: Vec<HashSet<String>> = Vec::new();
        let mut params_set: HashSet<String> = HashSet::new();
        for n in closure_param_names {
            params_set.insert(n.clone());
        }
        shadow_stack.push(params_set);
        // B-2026-08-15-22 — the walker needs to TYPE a method-call receiver, and
        // `expr_types` cannot answer: the parser gives a postfix expression its
        // receiver's span, so looking up `object.span` returns the METHOD CALL's
        // own type (`bool` for `s.contains(b)`), not the receiver's. Names are
        // the reliable key, so resolve receivers through this map instead.
        //
        // Extending the captures map with the closure's own PARAMS is safe for
        // the capture decision it also drives: `name_is_shadowed` is tested
        // BEFORE `outer.get(name)` at the identifier leaf, and every param name
        // is in the shadow stack pushed above — so a param can never reach that
        // lookup, and this only ever feeds the receiver typing.
        let mut env = outer_bindings.clone();
        for (n, t) in closure_param_names.iter().zip(closure_param_types) {
            env.insert(n.clone(), t.clone());
        }
        let outer_bindings = &env;
        let mut reason: Option<OnceReason> = None;
        // v2 core §9.6: the body's value is the closure's result and moves
        // out to the caller, so a closure whose result is a non-`Copy`
        // capture (`|| x`) gives it away and can run once.
        let body_mode = if self.cli_lint_overrides.strict_core {
            CaptureWalkMode::Consuming
        } else {
            CaptureWalkMode::Reading
        };
        let returned = returned_closure_spans(body, &|object: &Expr| {
            self.eager_receiver(object, outer_bindings)
        });
        let saved = self.returned_closure_spans.replace(returned);
        let saved_by_move = self.capture_walk_by_move.replace(0);
        self.walk_capture_consume(
            body,
            body_mode,
            outer_bindings,
            &mut shadow_stack,
            &mut reason,
        );
        self.capture_walk_by_move.set(saved_by_move);
        self.returned_closure_spans.replace(saved);
        reason
    }

    /// Whether `object`, the receiver of a method named like a lazy
    /// adaptor, is an `Option` or `Result`, whose `map`, `filter` and `take`
    /// run their closure at once (docs/library/core-types.md) and so return
    /// no closure (B-2026-11-18-78). An untyped receiver answers false.
    fn eager_receiver(&self, object: &Expr, env: &HashMap<String, Type>) -> bool {
        let ty = match &object.kind {
            ExprKind::Identifier(n) => env.get(n.as_str()).cloned(),
            ExprKind::SelfValue => env.get("self").cloned(),
            _ => None,
        }
        .or_else(|| {
            self.node_types
                .get(&object.id)
                .map(|(t, _)| resolve_type_var_top(t, &self.env.substitutions))
        })
        .map(|t| resolve_type_var_top(&t, &self.env.substitutions));
        ty.is_some_and(|t| crate::core_escape::eager_receiver_ty(&t))
    }

    /// The `OnceReason` for a consuming use of the field path `expr`
    /// (`o.a.b`, `t.0`) when its root is an owned capture and the path's
    /// value is not `Copy`. A path through a `ref` capture, or one whose
    /// value is `Copy` or a `shared` handle, moves nothing out of the
    /// closure's environment.
    fn consumed_capture_path(
        &self,
        expr: &Expr,
        outer: &HashMap<String, Type>,
        shadows: &[HashSet<String>],
    ) -> Option<OnceReason> {
        let mut parts: Vec<String> = Vec::new();
        let mut root = expr;
        loop {
            match &root.kind {
                ExprKind::FieldAccess { object, field } => {
                    parts.push(field.clone());
                    root = object;
                }
                ExprKind::TupleIndex { object, index } => {
                    parts.push(index.to_string());
                    root = object;
                }
                _ => break,
            }
        }
        let name = match &root.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            _ => return None,
        };
        if Self::name_is_shadowed(name, shadows) {
            return None;
        }
        let root_ty = outer.get(name)?;
        let root_ty = resolve_type_var_top(root_ty, &self.env.substitutions);
        if matches!(root_ty, Type::Ref(_) | Type::MutRef(_))
            || self.copy_is_only_an_rc_retain(&root_ty)
        {
            return None;
        }
        let ty = self
            .node_types
            .get(&expr.id)
            .map(|(t, _)| resolve_type_var_top(t, &self.env.substitutions))?;
        if matches!(ty, Type::TypeVar(_))
            || self.is_copy_type_during_check(&ty)
            || self.copy_is_only_an_rc_retain(&ty)
        {
            return None;
        }
        parts.push(name.to_string());
        parts.reverse();
        Some(OnceReason {
            consumed_binding: parts.join("."),
            consumed_span: expr.span,
            into_returned: false,
        })
    }

    /// Whether `for _ in iterable` borrows `iterable`: a place (a name or a
    /// field path) whose type is a standard collection, which a `for`
    /// walks through `Iterable` without consuming it.
    fn for_iterable_borrows(&self, iterable: &Expr, outer: &HashMap<String, Type>) -> bool {
        let ty = match &iterable.kind {
            ExprKind::Identifier(n) => outer
                .get(n.as_str())
                .cloned()
                .or_else(|| self.node_types.get(&iterable.id).map(|(t, _)| t.clone())),
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } => {
                self.node_types.get(&iterable.id).map(|(t, _)| t.clone())
            }
            _ => None,
        };
        let Some(ty) = ty else {
            return false;
        };
        let mut ty = resolve_type_var_top(&ty, &self.env.substitutions);
        while let Type::Ref(inner) | Type::MutRef(inner) = ty {
            ty = *inner;
        }
        match &ty {
            Type::Array { .. } => true,
            Type::Named { name, .. } => matches!(
                name.as_str(),
                "Vec" | "VecDeque" | "Map" | "SortedMap" | "Set" | "SortedSet" | "Array"
            ),
            _ => false,
        }
    }

    /// Whether `object.method(..)` takes `own self`: a method of the
    /// program's own impls, or of the library's, declared with an `own self`
    /// receiver. A receiver whose type is unknown answers false.
    fn method_takes_own_self(
        &self,
        object: &Expr,
        method: &str,
        outer: &HashMap<String, Type>,
    ) -> bool {
        let ty = match &object.kind {
            ExprKind::Identifier(n) => outer.get(n.as_str()).cloned(),
            ExprKind::SelfValue => outer.get("self").cloned(),
            _ => None,
        }
        .or_else(|| self.node_types.get(&object.id).map(|(t, _)| t.clone()));
        let Some(ty) = ty else {
            return false;
        };
        let ty = resolve_type_var_top(&ty, &self.env.substitutions);
        let name = match &ty {
            Type::Named { name, .. } | Type::Shared(name) => name.as_str(),
            _ => return false,
        };
        for item in &self.program.items {
            let Item::ImplBlock(imp) = item else { continue };
            let TypeKind::Path(p) = &imp.target_type.kind else {
                continue;
            };
            if p.segments.last().map(String::as_str) != Some(name) {
                continue;
            }
            for it in &imp.items {
                if let ImplItem::Method(m) = it {
                    if m.name == method && m.self_param.is_some() {
                        return m.self_is_own;
                    }
                }
            }
        }
        crate::prelude::library_method_self_is_own(name, method).unwrap_or(false)
    }

    fn name_is_shadowed(name: &str, shadow_stack: &[HashSet<String>]) -> bool {
        shadow_stack.iter().any(|s| s.contains(name))
    }

    /// B-2026-08-15-22 — is this a BUILTIN container/String predicate that only
    /// READS its argument, so passing a capture to it must not demote the
    /// closure to `OnceFn`?
    ///
    /// Gated on the RECEIVER'S TYPE, not on the method name alone, and that is
    /// the whole safety argument: a user type is free to declare a `contains`
    /// that stores what it is handed, and this must not speak for it. An
    /// unknown receiver type (`None`) answers false, so the previous
    /// consume-everything behaviour is what any shape this cannot classify
    /// falls back to.
    ///
    /// The membership list is deliberately short and every entry was measured
    /// non-consuming the same way before being added: called TWICE on one
    /// binding with a read of that binding afterwards, checked clean and run for
    /// the right answer. `push` / `insert` / `join` and friends retain their
    /// argument and are absent for that reason — a fixture pins two of them as
    /// still-demoting.
    fn builtin_read_only_arg_method(recv_ty: Option<&Type>, method: &str) -> bool {
        fn peel(t: &Type) -> &Type {
            match t {
                Type::Ref(inner) | Type::MutRef(inner) => peel(inner),
                other => other,
            }
        }
        let Some(recv) = recv_ty.map(peel) else {
            return false;
        };
        match recv {
            // A `String` or `Str` receiver's methods take text as `Str`, and
            // characters and counts by value (docs/library/strings.md), so an
            // argument is read and copied from, never kept. `extend` is the
            // one that takes `own I` (B-2026-10-28-28, B-2026-10-29-2).
            Type::Str => method != "extend",
            Type::Named { name, .. } if name == "StringSlice" => method != "extend",
            // Container membership: the probe is compared against stored
            // elements/keys, never adopted.
            Type::Named { name, .. } if name == "Vec" || name == "VecDeque" => method == "contains",
            Type::Named { name, .. } if name == "Set" || name == "SortedSet" => {
                method == "contains"
            }
            Type::Named { name, .. } if name == "Map" || name == "SortedMap" => {
                method == "contains_key"
            }
            _ => false,
        }
    }

    /// Which of a user method's parameters borrow, by the receiver's type:
    /// all but the `own` ones under D5. `None` (an unknown receiver, or a
    /// method this cannot find) leaves every argument consuming.
    fn user_method_borrow_positions(&self, recv: &Type, method: &str) -> Option<Vec<bool>> {
        let mut t = recv;
        while let Type::Ref(inner) | Type::MutRef(inner) = t {
            t = inner;
        }
        let (Type::Named { name, .. } | Type::Shared(name)) = t else {
            return None;
        };
        let own = self.d5_own_params(&format!("{name}.{method}"))?;
        Some(own.iter().map(|o| !o).collect())
    }

    fn walk_capture_consume_block(
        &self,
        block: &Block,
        terminal_mode: CaptureWalkMode,
        outer: &HashMap<String, Type>,
        shadows: &mut Vec<HashSet<String>>,
        reason: &mut Option<OnceReason>,
    ) {
        if reason.is_some() {
            return;
        }
        shadows.push(HashSet::new());
        for stmt in &block.stmts {
            if reason.is_some() {
                break;
            }
            match &stmt.kind {
                StmtKind::MultiAssign { .. } => unreachable!(
                    "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
                ),
                StmtKind::Let { pattern, value, .. } => {
                    self.walk_capture_consume(
                        value,
                        CaptureWalkMode::Consuming,
                        outer,
                        shadows,
                        reason,
                    );
                    let names = pattern.binding_names();
                    if let Some(top) = shadows.last_mut() {
                        for n in names {
                            top.insert(n);
                        }
                    }
                }
                StmtKind::LetUninit { name, .. } => {
                    if let Some(top) = shadows.last_mut() {
                        top.insert(name.clone());
                    }
                }
                StmtKind::LetElse {
                    pattern,
                    value,
                    else_block,
                    ..
                } => {
                    self.walk_capture_consume(
                        value,
                        CaptureWalkMode::Consuming,
                        outer,
                        shadows,
                        reason,
                    );
                    self.walk_capture_consume_block(
                        else_block,
                        CaptureWalkMode::Reading,
                        outer,
                        shadows,
                        reason,
                    );
                    let names = pattern.binding_names();
                    if let Some(top) = shadows.last_mut() {
                        for n in names {
                            top.insert(n);
                        }
                    }
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.walk_capture_consume_block(
                        body,
                        CaptureWalkMode::Reading,
                        outer,
                        shadows,
                        reason,
                    );
                }
                StmtKind::Assign { target, value } => {
                    self.walk_capture_consume(
                        value,
                        CaptureWalkMode::Consuming,
                        outer,
                        shadows,
                        reason,
                    );
                    self.walk_capture_consume(
                        target,
                        CaptureWalkMode::Reading,
                        outer,
                        shadows,
                        reason,
                    );
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.walk_capture_consume(
                        value,
                        CaptureWalkMode::Reading,
                        outer,
                        shadows,
                        reason,
                    );
                    self.walk_capture_consume(
                        target,
                        CaptureWalkMode::Reading,
                        outer,
                        shadows,
                        reason,
                    );
                }
                StmtKind::Expr(e) => {
                    self.walk_capture_consume(e, CaptureWalkMode::Reading, outer, shadows, reason);
                }
            }
        }
        if reason.is_none() {
            if let Some(tail) = &block.final_expr {
                self.walk_capture_consume(tail, terminal_mode, outer, shadows, reason);
            }
        }
        shadows.pop();
    }

    fn walk_capture_consume(
        &self,
        expr: &Expr,
        mode: CaptureWalkMode,
        outer: &HashMap<String, Type>,
        shadows: &mut Vec<HashSet<String>>,
        reason: &mut Option<OnceReason>,
    ) {
        if reason.is_some() {
            return;
        }
        match &expr.kind {
            ExprKind::Identifier(name) => {
                // Inside a closure the body returns, every capture moves
                // (core §9.3, B-2026-10-26-9).
                let by_move = self.capture_walk_by_move.get() > 0;
                if (mode == CaptureWalkMode::Consuming || by_move)
                    && !Self::name_is_shadowed(name, shadows)
                {
                    if let Some(ty) = outer.get(name) {
                        // A `shared` handle used as a value is retained, and
                        // the capture stays usable (core §6.1), so the
                        // closure moves nothing out (B-2026-10-25-13).
                        if !self.is_copy_type_during_check(ty)
                            && !self.copy_is_only_an_rc_retain(ty)
                        {
                            *reason = Some(OnceReason {
                                consumed_binding: name.clone(),
                                consumed_span: expr.span,
                                into_returned: mode != CaptureWalkMode::Consuming,
                            });
                        }
                    }
                }
            }
            ExprKind::SelfValue => {
                let by_move = self.capture_walk_by_move.get() > 0;
                if (mode == CaptureWalkMode::Consuming || by_move)
                    && !Self::name_is_shadowed("self", shadows)
                {
                    if let Some(ty) = outer.get("self") {
                        if !self.is_copy_type_during_check(ty) {
                            *reason = Some(OnceReason {
                                consumed_binding: "self".to_string(),
                                consumed_span: expr.span,
                                into_returned: mode != CaptureWalkMode::Consuming,
                            });
                        }
                    }
                }
            }

            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::Bool(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::ByteStringLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::InterpolatedStringLit(..)
            | ExprKind::CStringLit { .. }
            | ExprKind::Path { .. }
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}

            ExprKind::Binary { left, right, .. }
            | ExprKind::Pipe { left, right }
            | ExprKind::NilCoalesce { left, right } => {
                self.walk_capture_consume(left, CaptureWalkMode::Reading, outer, shadows, reason);
                self.walk_capture_consume(right, CaptureWalkMode::Reading, outer, shadows, reason);
            }
            ExprKind::Unary { operand, .. } => {
                self.walk_capture_consume(
                    operand,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
            }

            ExprKind::Call { callee, args } => {
                self.walk_capture_consume(callee, CaptureWalkMode::Reading, outer, shadows, reason);
                // A call through a closure-valued local (`far(n, tree)` with
                // `let far = |a: ref Vec[i64], b: ref Vec[i64]| ...`) lends
                // where that closure's parameters borrow, as a `fn`'s
                // signature does below. The local, not a same-named item,
                // is what the call reaches (B-2026-10-14-23).
                let local_fn = match &callee.kind {
                    ExprKind::Identifier(n) => match outer.get(n.as_str()) {
                        Some(Type::Function { params, .. } | Type::OnceFunction { params, .. }) => {
                            Some(params.iter().map(Self::is_borrow_param_type).collect())
                        }
                        _ => None,
                    },
                    _ => None,
                };
                let borrow_modes = local_fn.or_else(|| self.callee_borrow_positions(callee));
                for (i, arg) in args.iter().enumerate() {
                    let is_borrow = arg.mut_marker
                        || borrow_modes
                            .as_ref()
                            .and_then(|m| m.get(i))
                            .copied()
                            .unwrap_or(false);
                    let arg_mode = if is_borrow {
                        CaptureWalkMode::Reading
                    } else {
                        CaptureWalkMode::Consuming
                    };
                    self.walk_capture_consume(&arg.value, arg_mode, outer, shadows, reason);
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                // An `own self` method (`w.eat()`, `o.opt.unwrap()`,
                // `xs.into_iter()`) moves its receiver: a capture there is
                // consumed (B-2026-11-27-24).
                let recv_mode = if self.method_takes_own_self(object, method, outer) {
                    CaptureWalkMode::Consuming
                } else {
                    CaptureWalkMode::Reading
                };
                self.walk_capture_consume(object, recv_mode, outer, shadows, reason);
                // B-2026-08-15-22: the `Call` arm above decides an argument's
                // mode from the CALLEE'S SIGNATURE; this one had no signature to
                // consult and called every non-`mut` argument Consuming. For a
                // builtin read-only predicate that is simply wrong, and it is
                // wrong in the one place a user cannot work around — the needle
                // of `s.contains(bad)` is not retained by anything, but
                // `String.contains` is not a signature anyone can re-declare.
                //
                // The two analyses disagreed, and the sequential one is right:
                // outside a closure, `s.contains(b)` twice followed by `b.len()`
                // checks clean and runs. So a predicate factory —
                // `fn forbids(bad: String) -> Fn(ref String) -> bool
                //  { |s| not s.contains(bad) }`, the ordinary way to build a rule
                // list — was rejected as `OnceFn` with no spelling that compiles.
                // A receiver that is no captured name (the closure's own
                // parameter, or a call's result as in `s.trim().contains(x)`)
                // has the type its body recorded.
                let recv_ty = match &object.kind {
                    ExprKind::Identifier(n) => outer.get(n.as_str()).cloned(),
                    ExprKind::SelfValue => outer.get("self").cloned(),
                    _ => None,
                }
                .or_else(|| {
                    self.node_types
                        .get(&object.id)
                        .map(|(t, _)| resolve_type_var_top(t, &self.env.substitutions))
                });
                let recv_ty = recv_ty.as_ref();
                let arg_is_read_only = Self::builtin_read_only_arg_method(recv_ty, method);
                // A user method's parameter borrows unless it is `own`, as a
                // free function's does in the `Call` arm (B-2026-10-25-13).
                let borrow_modes =
                    recv_ty.and_then(|t| self.user_method_borrow_positions(t, method));
                for (i, arg) in args.iter().enumerate() {
                    let is_borrow = borrow_modes
                        .as_ref()
                        .and_then(|m| m.get(i))
                        .copied()
                        .unwrap_or(false);
                    let arg_mode = if arg.mut_marker || arg_is_read_only || is_borrow {
                        CaptureWalkMode::Reading
                    } else {
                        CaptureWalkMode::Consuming
                    };
                    self.walk_capture_consume(&arg.value, arg_mode, outer, shadows, reason);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                // Each field path of a capture is its own capture (core §9.3),
                // so moving one out (`take(o.a)`, `|| t.0`) makes the closure
                // once-callable as moving the whole binding does
                // (B-2026-11-27-24).
                if mode == CaptureWalkMode::Consuming {
                    if let Some(r) = self.consumed_capture_path(expr, outer, shadows) {
                        *reason = Some(r);
                        return;
                    }
                }
                self.walk_capture_consume(object, CaptureWalkMode::Reading, outer, shadows, reason);
            }
            ExprKind::Index { object, index } => {
                self.walk_capture_consume(object, CaptureWalkMode::Reading, outer, shadows, reason);
                self.walk_capture_consume(index, CaptureWalkMode::Reading, outer, shadows, reason);
            }

            ExprKind::Block(block) | ExprKind::Comptime(block) => {
                self.walk_capture_consume_block(block, mode, outer, shadows, reason);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_capture_consume(
                    condition,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
                self.walk_capture_consume_block(then_block, mode, outer, shadows, reason);
                if let Some(eb) = else_branch {
                    self.walk_capture_consume(eb, mode, outer, shadows, reason);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.walk_capture_consume(value, CaptureWalkMode::Reading, outer, shadows, reason);
                let mut arm_scope: HashSet<String> = HashSet::new();
                for n in pattern.binding_names() {
                    arm_scope.insert(n);
                }
                shadows.push(arm_scope);
                self.walk_capture_consume_block(then_block, mode, outer, shadows, reason);
                shadows.pop();
                if let Some(eb) = else_branch {
                    self.walk_capture_consume(eb, mode, outer, shadows, reason);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // A `match` moves its scrutinee only where an arm binds a part
                // that could move (core §4.6); one whose arms bind `Copy`
                // parts, handles, `ref`s or nothing reads it (B-2026-10-25-13).
                let scrutinee_mode = if !(self.cli_lint_overrides.strict_core
                    || self.library_methods_from_source)
                    || self
                        .moving_scrutinees
                        .contains(&SpanKey::from_span(&scrutinee.span))
                {
                    CaptureWalkMode::Consuming
                } else {
                    CaptureWalkMode::Reading
                };
                self.walk_capture_consume(scrutinee, scrutinee_mode, outer, shadows, reason);
                for arm in arms {
                    let mut arm_scope: HashSet<String> = HashSet::new();
                    for n in arm.pattern.binding_names() {
                        arm_scope.insert(n);
                    }
                    shadows.push(arm_scope);
                    if let Some(g) = &arm.guard {
                        self.walk_capture_consume(
                            g,
                            CaptureWalkMode::Reading,
                            outer,
                            shadows,
                            reason,
                        );
                    }
                    self.walk_capture_consume(&arm.body, mode, outer, shadows, reason);
                    shadows.pop();
                }
            }

            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_capture_consume(
                    condition,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
                self.walk_capture_consume_block(
                    body,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.walk_capture_consume(value, CaptureWalkMode::Reading, outer, shadows, reason);
                let mut arm_scope: HashSet<String> = HashSet::new();
                for n in pattern.binding_names() {
                    arm_scope.insert(n);
                }
                shadows.push(arm_scope);
                self.walk_capture_consume_block(
                    body,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
                shadows.pop();
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                par,
                ..
            } => {
                if let Some(n) = par.as_ref().and_then(|p| p.limit.as_ref()) {
                    self.walk_capture_consume(n, CaptureWalkMode::Reading, outer, shadows, reason);
                }
                // A `for` over a collection borrows it through `Iterable`;
                // only an `Iterator` is consumed (design.md § for,
                // B-2026-11-27-24).
                let iterable_mode = if self.for_iterable_borrows(iterable, outer) {
                    CaptureWalkMode::Reading
                } else {
                    CaptureWalkMode::Consuming
                };
                self.walk_capture_consume(iterable, iterable_mode, outer, shadows, reason);
                let mut arm_scope: HashSet<String> = HashSet::new();
                for n in pattern.binding_names() {
                    arm_scope.insert(n);
                }
                shadows.push(arm_scope);
                self.walk_capture_consume_block(
                    body,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
                shadows.pop();
            }
            ExprKind::Loop { body, .. } => {
                self.walk_capture_consume_block(
                    body,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
            }

            ExprKind::LabeledBlock { body, .. } => {
                self.walk_capture_consume_block(
                    body,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
            }

            ExprKind::Break { value: Some(v), .. } | ExprKind::Return(Some(v)) => {
                self.walk_capture_consume(v, CaptureWalkMode::Consuming, outer, shadows, reason);
            }
            ExprKind::Break { value: None, .. }
            | ExprKind::Continue { .. }
            | ExprKind::Return(None) => {}

            ExprKind::Question(inner) => {
                self.walk_capture_consume(
                    inner,
                    CaptureWalkMode::Consuming,
                    outer,
                    shadows,
                    reason,
                );
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_capture_consume(object, CaptureWalkMode::Reading, outer, shadows, reason);
                if let Some(arg_list) = args {
                    for arg in arg_list {
                        let arg_mode = if arg.mut_marker {
                            CaptureWalkMode::Reading
                        } else {
                            CaptureWalkMode::Consuming
                        };
                        self.walk_capture_consume(&arg.value, arg_mode, outer, shadows, reason);
                    }
                }
            }

            ExprKind::Closure {
                params: nested_params,
                body: nested_body,
                ..
            } => {
                let mut nested_scope: HashSet<String> = HashSet::new();
                for p in nested_params {
                    for n in p.pattern.binding_names() {
                        nested_scope.insert(n);
                    }
                }
                shadows.push(nested_scope);
                // A closure the body returns escapes and moves what it
                // captures out of this closure (core §9.3, B-2026-10-26-9).
                let returned = self
                    .returned_closure_spans
                    .borrow()
                    .contains(&SpanKey::from_span(&expr.span));
                self.capture_walk_by_move
                    .set(self.capture_walk_by_move.get() + u32::from(returned));
                self.walk_capture_consume(
                    nested_body,
                    CaptureWalkMode::Reading,
                    outer,
                    shadows,
                    reason,
                );
                self.capture_walk_by_move
                    .set(self.capture_walk_by_move.get() - u32::from(returned));
                shadows.pop();
            }

            ExprKind::Cast { expr: inner, .. } => {
                self.walk_capture_consume(inner, mode, outer, shadows, reason);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_capture_consume(s, CaptureWalkMode::Reading, outer, shadows, reason);
                }
                if let Some(e) = end {
                    self.walk_capture_consume(e, CaptureWalkMode::Reading, outer, shadows, reason);
                }
            }

            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.walk_capture_consume(e, mode, outer, shadows, reason);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_capture_consume(e, mode, outer, shadows, reason);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_capture_consume(value, mode, outer, shadows, reason);
                self.walk_capture_consume(count, CaptureWalkMode::Reading, outer, shadows, reason);
            }
            ExprKind::MapLiteral { entries, .. } => {
                for (k, v) in entries {
                    self.walk_capture_consume(k, mode, outer, shadows, reason);
                    self.walk_capture_consume(v, mode, outer, shadows, reason);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_capture_consume(
                        &f.value,
                        CaptureWalkMode::Consuming,
                        outer,
                        shadows,
                        reason,
                    );
                }
                if let Some(s) = spread {
                    self.walk_capture_consume(
                        s,
                        CaptureWalkMode::Consuming,
                        outer,
                        shadows,
                        reason,
                    );
                }
            }

            ExprKind::Par(body)
            | ExprKind::Seq(body)
            | ExprKind::Unsafe(body)
            | ExprKind::Try(body) => {
                self.walk_capture_consume_block(body, mode, outer, shadows, reason);
            }
            ExprKind::Lock { body, .. } => {
                self.walk_capture_consume_block(body, mode, outer, shadows, reason);
            }
            ExprKind::Providers { bindings, body } => {
                for binding in bindings {
                    self.walk_capture_consume(
                        &binding.value,
                        CaptureWalkMode::Consuming,
                        outer,
                        shadows,
                        reason,
                    );
                }
                self.walk_capture_consume_block(body, mode, outer, shadows, reason);
            }
        }
    }
}

/// The closure literals in the value a closure body returns, followed
/// through block, `if` and `match` tails, `return`, and tuple, collection
/// and struct literals, as the MIR builder's `returned_closures_of` does.
fn returned_closure_spans(body: &Expr, eager: &dyn Fn(&Expr) -> bool) -> HashSet<SpanKey> {
    let mut out = HashSet::new();
    let mut work = vec![body];
    while let Some(e) = work.pop() {
        match &e.kind {
            ExprKind::Closure { .. } => {
                out.insert(SpanKey::from_span(&e.span));
            }
            ExprKind::Block(b) => work.extend(b.final_expr.as_deref()),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            }
            | ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                work.extend(then_block.final_expr.as_deref());
                work.extend(else_branch.as_deref());
            }
            ExprKind::Match { arms, .. } => work.extend(arms.iter().map(|a| &a.body)),
            ExprKind::Return(Some(x)) => work.push(x),
            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => work.extend(es),
            ExprKind::StructLiteral { fields, .. } => work.extend(fields.iter().map(|f| &f.value)),
            // B-2026-11-15-32: a lazy adaptor keeps the closure it is
            // handed inside the iterator it returns, so a returned
            // `(0..3).map(|c| m + clears[c])` returns that closure too, and
            // so does every adaptor further down the receiver chain.
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if crate::core_escape::LAZY_ADAPTORS.contains(&method.as_str())
                // `Option.map` / `Result.map` (and `filter`, `take`) share
                // a name with an adaptor but run the closure at once and
                // keep nothing (B-2026-11-18-78).
                && !eager(object) =>
            {
                work.push(object);
                work.extend(args.iter().map(|a| &a.value));
            }
            _ => {}
        }
    }
    out
}
