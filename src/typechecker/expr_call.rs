//! Call expression inference + call-site marker checking +
//! layout-query intrinsic.
//!
//! Houses `infer_call` (the main call dispatch on `ExprKind::Call`),
//! the explicit-generic-args entry (`infer_explicit_generic_args_call`),
//! the `Layout.…(T)` intrinsic resolver (`infer_layout_query_intrinsic`),
//! the call-site `mut`-marker check (`check_call_site_marker` plus
//! `is_arg_forwarded` / `place_root_is_mut_borrow`).
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::cross_task_safe::is_cross_task_safe_with;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::inference::{expr_as_type_expr, is_literal_const_arg_expr, resolve_type_var_top};
use super::types::{type_display, ConstArg, DimArg, IntSize, Type, UIntSize};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// `(explicit_args, formal_generic_params)` into the call-args
    /// substitution flow so the inference solver pre-binds each
    /// ConstVar / TypeVar to its user-supplied value before
    /// arg-position unification.
    pub(super) fn infer_explicit_generic_args_call(
        &mut self,
        name: &str,
        explicit_args: &[GenericArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        // Built-in `Vector[T, N](lane0, lane1, ...)` construction (design.md
        // § Portable SIMD). Not a user function — intercept before the
        // function-table lookup. One value argument per lane.
        if name == "Vector" {
            return self.infer_vector_construction(explicit_args, args, span);
        }
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

    /// Type-check a `Vector[T, N](lane0, …, lane{N-1})` construction.
    ///
    /// Slice 1 scope: concrete element / lane-count construction. The element
    /// type and lane count are lowered through [`lower_vector_type`] (which
    /// enforces numeric `T` and `N > 0` and emits its own diagnostics); each
    /// value argument is then checked against the element type, and the arg
    /// count must equal `N`. Returns `Type::Vector { element, lanes }` so the
    /// result flows into binop / index / assignment positions.
    fn infer_vector_construction(
        &mut self,
        explicit_args: &[GenericArg],
        args: &[CallArg],
        span: &Span,
    ) -> Type {
        let lowered = self.lower_vector_type(&Some(explicit_args.to_vec()), &[], span);
        let Some(Type::Vector { element, lanes }) = lowered else {
            // `lower_vector_type` already reported the bad element/lane shape.
            // Still walk the args so downstream inference doesn't cascade.
            for a in args {
                self.infer_expr(&a.value);
            }
            return Type::Error;
        };
        if let ConstArg::Literal(n) = &lanes {
            if args.len() as i64 != *n {
                self.type_error(
                    format!(
                        "Vector[{}, {}] construction expects {} lane argument(s), found {}",
                        type_display(&element),
                        n,
                        n,
                        args.len()
                    ),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
            }
        }
        // Each lane value must be assignable to the element type. `check_expr`
        // threads the element type as the expected type so suffixed literals
        // and exact-typed bindings resolve cleanly.
        for a in args {
            self.check_expr(&a.value, &element);
        }
        let result = Type::Vector { element, lanes };
        self.record_expr_type(span, &result);
        result
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
    pub(super) fn infer_layout_query_intrinsic(
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

    /// Type `collect_all(|| a, || b, …)` — the heterogeneous fixed-arity
    /// (2..=8) parallel gather. Each branch is a zero-arg closure
    /// `Fn() -> Result[Ai, Ei]` (explicit `|| …` for now — auto-thunking
    /// of bare expressions is a follow-up); the result is the tuple
    /// `(Result[A1,E1], …, Result[An,En])`, preserving each branch's own
    /// success/error types. `Type::Error` branches degrade to an `Error`
    /// tuple element so a single bad branch doesn't cascade.
    fn infer_collect_all(&mut self, args: &[CallArg], span: &Span) -> Type {
        if !(2..=8).contains(&args.len()) {
            self.type_error(
                format!(
                    "collect_all takes 2 to 8 branches, found {} (for a single \
                     homogeneous Vec of branches use collect_all_vec)",
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
        let mut elem_types: Vec<Type> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_ty = self.infer_expr(&arg.value);
            // A branch is either an explicit zero-arg closure
            // `|| Result[…]` (use its return type) OR a bare expression
            // `fetch()` of type `Result[…]` that the lowering pass
            // auto-thunks into `|| fetch()` (use the expression's own
            // type). Either way the branch result type is checked to be a
            // `Result[A, E]` below.
            let ret = match &arg_ty {
                Type::Function {
                    params,
                    return_type,
                }
                | Type::OnceFunction {
                    params,
                    return_type,
                } if params.is_empty() => (**return_type).clone(),
                // A closure that takes arguments is not a valid branch (a
                // branch is invoked with no args).
                Type::Function { .. } | Type::OnceFunction { .. } => {
                    self.type_error(
                        format!(
                            "collect_all branch {} must be a zero-argument closure, \
                             found '{}'",
                            i + 1,
                            type_display(&arg_ty)
                        ),
                        arg.value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    Type::Error
                }
                // Bare-expression (auto-thunked) branch: the argument's own
                // type is the branch result.
                other => other.clone(),
            };
            // …whose return type is a Result[A, E].
            let elem = match &ret {
                Type::Named { name, args: targs } if name == "Result" && targs.len() == 2 => {
                    ret.clone()
                }
                Type::Error => Type::Error,
                _ => {
                    self.type_error(
                        format!(
                            "collect_all branch {} must return Result[T, E], found '{}'",
                            i + 1,
                            type_display(&ret)
                        ),
                        arg.value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    Type::Error
                }
            };
            elem_types.push(elem);
        }
        Type::Tuple(elem_types)
    }

    /// Type a call to the comptime stdlib modules `ast` / `compiler`
    /// (substrate 3). Returns `Some(ret)` when `module` is a comptime module
    /// (so the caller short-circuits normal dispatch), `None` otherwise.
    ///
    /// - `ast.expr(s: String) -> Expr` — quasi-quote AST builder.
    /// - `compiler.error(msg: String) -> ()` — compile-time diagnostic.
    ///
    /// Both are comptime-only: a use at runtime is
    /// `E_COMPTIME_MODULE_AT_RUNTIME`. Unknown members of a comptime module
    /// are rejected here rather than falling through to the generic
    /// (and confusing) "undefined function" path.
    pub(super) fn comptime_module_call_type(
        &mut self,
        module: &str,
        member: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Type> {
        if module != "ast" && module != "compiler" {
            return None;
        }
        // Infer args once up front (uniform with the rest of dispatch and so
        // arg-internal diagnostics still fire on the error paths below).
        let arg_tys: Vec<Type> = args.iter().map(|a| self.infer_expr(&a.value)).collect();

        if self.comptime_depth == 0 {
            self.type_error(
                format!(
                    "error[E_COMPTIME_MODULE_AT_RUNTIME]: the `{module}` module is only \
                     available inside a `comptime` context (deferred.md § Comptime — Comptime \
                     stdlib surface)"
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Some(Type::Error);
        }

        // Shared one-`String`-argument validation.
        let expect_one_string = |this: &mut Self, what: &str| {
            if arg_tys.len() != 1 {
                this.type_error(
                    format!("`{what}` expects 1 argument, got {}", arg_tys.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
            } else if !matches!(arg_tys[0], Type::Str | Type::Error) {
                this.type_error(
                    format!(
                        "`{what}` expects a `String` argument, got `{}`",
                        type_display(&arg_tys[0])
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        };

        match (module, member) {
            ("ast", "expr") => {
                expect_one_string(self, "ast.expr");
                Some(Type::Named {
                    name: "Expr".to_string(),
                    args: vec![],
                })
            }
            ("ast", "item") => {
                expect_one_string(self, "ast.item");
                Some(Type::Named {
                    name: "Item".to_string(),
                    args: vec![],
                })
            }
            ("compiler", "error") => {
                expect_one_string(self, "compiler.error");
                Some(Type::Unit)
            }
            _ => {
                self.type_error(
                    format!(
                        "`{module}` has no comptime member `{member}`; this slice supports \
                         `ast.expr(s)`, `ast.item(s)` and `compiler.error(msg)`"
                    ),
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                Some(Type::Error)
            }
        }
    }

    pub(super) fn infer_call(&mut self, callee: &Expr, args: &[CallArg], span: &Span) -> Type {
        // Comptime `Type` reflection in the path-call form. `MyType.name()`,
        // `MyType.fields()`, … parse as `Call(Path([Type, method]))` (the
        // value-receiver form `t.name()` is handled in `infer_method_call`).
        // Reserved only at comptime, and only when the head segment is a known
        // type — outside comptime, an identically-named user associated fn
        // still resolves below. Spec: deferred.md § Comptime — Reflection API.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && self.comptime_depth > 0
                && Self::is_reflection_method(&segments[1])
                && self.is_type_name(&segments[0])
            {
                let ty = self.infer_type_reflection_method(&segments[1], args, span);
                self.record_expr_type(span, &ty);
                return ty;
            }
        }

        // Comptime stdlib surface (substrate 3): the `compiler` and `ast`
        // modules. `ast.expr(s)` is the quasi-quote AST builder (a string →
        // `Expr` node, spliced at the comptime site); `compiler.error(msg)` is
        // the compile-time validation diagnostic. Both are comptime-only —
        // using them at runtime is `E_COMPTIME_MODULE_AT_RUNTIME`. Spec:
        // deferred.md § Comptime — AST builder API / Comptime stdlib surface.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 {
                if let Some(ret) =
                    self.comptime_module_call_type(&segments[0], &segments[1], args, span)
                {
                    self.record_expr_type(span, &ret);
                    return ret;
                }
            }
        }

        // Fallible-allocation constructor companions (phase-8-stdlib-floor
        // item 2). `Type.try_with_capacity(n)` / `Vec.try_from_slice(src)` type
        // identically to their panicking `Type.<base>(...)` constructor but
        // return `Result[<constructor-ret>, AllocError]`. Recurse into the base
        // constructor (rewriting the path's method segment) to reuse its
        // argument validation + return-type synthesis, then wrap. Gated on the
        // recognized `(collection, base)` pairs so an unknown `X.try_<base>`
        // still reports against its own name via the normal rejection path.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 {
                if let Some(base) = crate::fallible_alloc::static_companion_base(&segments[1]) {
                    let coll = segments[0].as_str();
                    let recognized = match base {
                        "with_capacity" => matches!(coll, "Vec" | "VecDeque" | "String"),
                        "from_slice" => coll == "Vec",
                        _ => false,
                    };
                    if recognized {
                        let mut base_callee = callee.clone();
                        if let ExprKind::Path { segments, .. } = &mut base_callee.kind {
                            segments[1] = base.to_string();
                        }
                        let base_ret = self.infer_call(&base_callee, args, span);
                        if base_ret == Type::Error {
                            return Type::Error;
                        }
                        let ty = self.result_alloc_error_type(base_ret);
                        self.record_expr_type(span, &ty);
                        return ty;
                    }
                }
            }
        }

        // Phase 6 line 170 slice 3a — cross-task-safe boundary check at
        // `spawn(closure)` call sites. Fires before any other dispatch so
        // the outer-scope snapshot taken inside
        // `check_cross_task_safe_captures` doesn't include the closure
        // params (those get pushed onto the local scope only when the
        // closure body's typecheck runs, deeper in this function). When
        // the callee isn't bare `spawn` or the arg isn't a closure
        // literal, the call is a no-op — regular dispatch follows
        // unchanged.
        if let ExprKind::Identifier(name) = &callee.kind {
            if name == "spawn" && args.len() == 1 && self.local_scope.lookup("spawn").is_none() {
                self.check_cross_task_safe_captures(&args[0].value, span, "spawn");
            }
        }

        // Phase 6 line 170 slice 3c — cross-task-safe boundary check at
        // `with_provider[R](provider, closure)` call sites. The surface
        // parses as `Call(Index(Ident|Path("with_provider"), R), [provider,
        // closure])` (mirrors the interpreter's `match_with_provider`). The
        // provider value is shared with the closure body, which may run
        // across spawned tasks, so a provider whose concrete type reaches a
        // not-cross-task-safe leaf is rejected at the call site (design.md
        // line 7213). Unlike a `par {}` branch there is no sole-ownership
        // carve-out — the full unsafe set is rejected, shared struct/enum
        // included. Regular call dispatch follows unchanged. (The
        // `providers { R => p } in { body }` block form is checked at the
        // `ExprKind::Providers` arm in `exprs.rs`.)
        if let ExprKind::Index { object, index } = &callee.kind {
            let is_with_provider = match &object.kind {
                ExprKind::Identifier(n) => n == "with_provider",
                ExprKind::Path { segments, .. } => segments.as_slice() == ["with_provider"],
                _ => false,
            };
            let resource = if is_with_provider && args.len() == 2 {
                match &index.kind {
                    ExprKind::Identifier(n) => Some(n.clone()),
                    ExprKind::Path { segments, .. } => segments.last().cloned(),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(resource) = resource {
                let provider_expr = &args[0].value;
                let provider_ty = self.infer_expr(provider_expr);
                if let Err(path) =
                    is_cross_task_safe_with(&provider_ty, &self.env.structs, &self.env.enums)
                {
                    let descr = format!("provider for resource `{resource}`");
                    self.emit_cross_task_unsafe_value(
                        &descr,
                        &provider_ty,
                        &path,
                        &provider_expr.span,
                    );
                }
            }
        }

        // Combined distinct-type constructor: `ValidPort(value)` where
        // `distinct type ValidPort = u16 where pred`. The argument is checked
        // against the base, the predicate is enforced at compile time for a
        // const-evaluable argument (compile error on failure; no runtime
        // check on success) and otherwise at runtime, and the result is the
        // nominal distinct type. design.md § Distinct Types — "Construction
        // semantics for `distinct type T = Base where predicate`". The plain
        // (predicate-free) distinct constructor stays on the normal
        // `Function([base], Named{T})` dispatch from `resolve_identifier_type`.
        if let ExprKind::Identifier(name) = &callee.kind {
            if args.len() == 1
                && self.local_scope.lookup(name).is_none()
                && self.env.refinement_predicates.contains_key(name)
                && self.env.distinct_bases.contains_key(name)
            {
                let base = self.env.distinct_bases.get(name).cloned().unwrap();
                self.check_expr(&args[0].value, &base);
                self.check_distinct_constructor_predicate(name, &base, &args[0].value);
                let ty = Type::Named {
                    name: name.clone(),
                    args: Vec::new(),
                };
                self.record_expr_type(span, &ty);
                return ty;
            }
        }

        // Uppercase-receiver method-dispatch rewrite. The parser at
        // `src/parser/exprs.rs` 1298–1326 greedily wraps `X.method(args)`
        // in `Call(Path([X, method]))` whenever `X` starts uppercase —
        // the parser can't tell at parse time whether `X` is a Type-class
        // root (where the Call(Path) shape is right, e.g. `Vec.new()`,
        // `String.from(x)`) or a value binding that shadows nothing
        // (`let mut TODOS: Vec[i64] = Vec.new(); TODOS.push(1)`). Without
        // this disambiguation, the value-binding case fell through to
        // the default arm and emitted the misleading "type 'Vec[i64]'
        // is not callable" diagnostic from `resolve_path_type`'s
        // identifier fallback. Disambiguate against the env: when the
        // leading segment resolves as a value binding (a local under
        // `local_scope` OR a constant / module-binding in
        // `env.constants` that is NOT also a known type name), route
        // through `infer_method_call` with a synthesized identifier
        // receiver and flag the span for the lowering pass to mutate
        // the AST node to `MethodCall(Identifier(X), method, args)`.
        // Local-shadows-type wins by construction — the local_scope
        // lookup fires before the env-constants + not-type-name guard,
        // so `let Foo = ...; Foo.bar()` (a local shadow of struct Foo)
        // routes to method dispatch on the local. Generic-args (UFCS)
        // and longer paths are deliberately excluded so `Vec[i64].new()`,
        // `module.Sub.fn()`, and similar stay on their existing paths.
        // Effect resources (`Clock`, `UserDB`, etc.) are not in
        // `env.constants` and so naturally fall through to their
        // ambient-resource dispatch — see `expr_ops.rs::resolve_path_type`.
        if let ExprKind::Path {
            segments,
            generic_args: None,
        } = &callee.kind
        {
            if segments.len() == 2 && self.path_first_segment_is_value_binding(&segments[0]) {
                let synth_object = Expr {
                    span: callee.span.clone(),
                    kind: ExprKind::Identifier(segments[0].clone()),
                };
                // No distinct close-paren leaf is reconstructed on this
                // synthesized-`MethodCall` route (`local.method()` where `local`
                // shadows a type name); `span` stands in for `args_close_span`.
                // Integer receivers reach `pow` / the bit intrinsics through the
                // postfix `MethodCall` path (with a real close-paren span), not
                // this two-segment Path route, so the width stash is unaffected.
                let result = self.infer_method_call(&synth_object, &segments[1], args, span, span);
                self.path_call_method_dispatch
                    .insert(SpanKey::from_span(span));
                return result;
            }
        }

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

        // B-2026-07-11-1: a dotted builtin-module call with a SINGLE type-arg
        // turbofish — `ptr.null[u8]()`, `ptr.dangling[T]()` — parses as
        // `Call { callee: Index { object: FieldAccess{Identifier(module), fn},
        // index: <type> } }`, because a one-element `[T]` is indistinguishable
        // from indexing at parse time (the same shape `size_of[T]()` takes).
        // The dotted name lives in `env.functions` and declares a type param that,
        // for the zero-value-arg constructors, can ONLY bind from this turbofish;
        // without routing it to the explicit-generic-args solver the call fell
        // through with `T` unresolved and its `*const T` return collapsed to
        // `Type::Error`, so the binding recorded no raw-pointer pointee and a
        // later `p.read()` failed codegen with "no handler for method 'read'".
        // Gated on the dotted name being a known baked function so a user
        // `receiver.field[i]()` index-call is unaffected.
        if let ExprKind::Index { object, index } = &callee.kind {
            if let ExprKind::FieldAccess {
                object: inner,
                field,
            } = &object.kind
            {
                if let ExprKind::Identifier(module) = &inner.kind {
                    let dotted = format!("{}.{}", module, field);
                    if self.env.functions.contains_key(&dotted) {
                        if let Some(te) = expr_as_type_expr(index) {
                            let synth = vec![GenericArg::Type(te)];
                            return self
                                .infer_explicit_generic_args_call(&dotted, &synth, args, span);
                        }
                    }
                }
            }
        }

        if let Some((name, explicit_args)) = match &callee.kind {
            ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } if segments.len() == 1 => Some((segments[0].clone(), ga.clone())),
            // B-2026-07-11-1: a TWO-segment dotted builtin-module call carrying a
            // turbofish (`ptr.null[u8]()`, `ptr.dangling[T]()`) — these live in
            // `env.functions` under a dotted name and declare a type param `T`
            // that, for the zero-value-arg constructors, can ONLY be bound from
            // the turbofish. The single-segment route above missed them, so `T`
            // stayed unresolved and the `*const T` return collapsed to
            // `Type::Error`; the binding then recorded no raw-pointer pointee and
            // a later `p.read()` fell through in codegen ("no handler for method
            // 'read'"). Gated on the dotted name being a known baked function so
            // user `Type.method[T]()` associated calls (resolved via impls, not
            // `env.functions`) are unaffected.
            ExprKind::Path {
                segments,
                generic_args: Some(ga),
            } if segments.len() == 2 => {
                let dotted = format!("{}.{}", segments[0], segments[1]);
                if self.env.functions.contains_key(&dotted) {
                    Some((dotted, ga.clone()))
                } else {
                    None
                }
            }
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

        // `Stats.<fn>(xs, …)` — element-kind-aware result typing (S5,
        // docs/spikes/reduce-elementwise-trait-unification.md). The baked
        // `stats.kara` signatures are `Slice[f64]`-only and the permissive
        // numeric coercion silently admitted ANY numeric element against
        // them, typing every result f64 — while `karac build` read the raw
        // buffer at f64, bit-reinterpreting integer elements into denormal
        // garbage (B-2026-07-01-9). This intercept types the surface from
        // the argument's element like the Column stats intercept does:
        // f64 keeps the existing table; i64 gets the element-typed rules
        // (`sum`/`prod` → i64, `min`/`max` → Option[i64], `sort` → Vec[i64];
        // the float statistics still promote); any other element is a hard
        // error instead of silent garbage. The element kind is recorded in
        // `stats_elem_types` for codegen.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "Stats"
                && matches!(
                    segments[1].as_str(),
                    "sum"
                        | "prod"
                        | "mean"
                        | "variance"
                        | "stddev"
                        | "median"
                        | "min"
                        | "max"
                        | "percentile"
                        | "argmin"
                        | "argmax"
                        | "sort"
                        | "argsort"
                )
            {
                let method = segments[1].clone();
                return self.infer_stats_call(&method, args, span);
            }
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

        // Built-in variadic gather: `collect_all(|| a, || b, …)` — the
        // heterogeneous fixed-arity (2..=8) sibling of `collect_all_vec`.
        // Each argument is a closure `Fn() -> Result[Ai, Ei]`; the result
        // is the tuple `(Result[A1,E1], Result[A2,E2], …)`, preserving
        // each branch's own success/error types (design.md § Concurrency
        // Semantics > `collect_all`). Resolved here rather than via a
        // stdlib declaration because the arity — and thus the return tuple
        // shape — varies per call site, which no single generic signature
        // can express.
        if let ExprKind::Identifier(name) = &callee.kind {
            if name == "collect_all" {
                return self.infer_collect_all(args, span);
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
                    "Map" | "SortedMap" => Some(Type::Named {
                        name: collection.to_string(),
                        args: vec![self.env.fresh_type_var(), self.env.fresh_type_var()],
                    }),
                    // `String.new()` has no syntactic stdlib declaration
                    // (no `impl String { fn new() -> String }` in
                    // `runtime/stdlib/*.kara`); codegen handles it directly
                    // at `src/codegen/assoc_call.rs` (`String && method ==
                    // "new"` arm) and the typechecker used to fall through
                    // silently. The fall-through was harmless until the
                    // `resolve_path_type` rejection of unknown `Type.method`
                    // calls landed — now the typechecker rejects the call
                    // before codegen can claim it. Surface a real `String`
                    // return type here, mirroring how `Vec.new()` is
                    // covered above. Same fix shape applies to
                    // `String.with_capacity(n)` below.
                    "String" => Some(Type::Str),
                    _ => None,
                };
                if let Some(ty) = result_ty {
                    self.record_expr_type(span, &ty);
                    return ty;
                }
            }
        }

        // `Channel.new() -> (Sender[T], Receiver[T])`. Like the collection
        // `.new` constructors above, `Channel` has no syntactic `impl Channel
        // { fn new }` in stdlib (`channel.kara` bakes only the `struct
        // Channel[=T] {}` shape) — the sender/receiver pair is minted here. A
        // single shared fresh typevar links the two ends so a later
        // `tx.send(x)` / `rx.recv()` (`infer_channel_method`) pins the same
        // `T`. Without this arm the call falls through to the
        // `resolve_path_type` rejection ("no associated function 'new' on
        // type 'Channel'") — which is exactly why channels only ever worked
        // under the typecheck-bypassing interpreter path before the AOT
        // codegen lowering; `karac build` runs the typechecker.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "Channel"
                && segments[1] == "new"
                && args.is_empty()
            {
                let elem = self.env.fresh_type_var();
                let sender = Type::Named {
                    name: "Sender".to_string(),
                    args: vec![elem.clone()],
                };
                let receiver = Type::Named {
                    name: "Receiver".to_string(),
                    args: vec![elem],
                };
                let ty = Type::Tuple(vec![sender, receiver]);
                self.record_expr_type(span, &ty);
                return ty;
            }
        }

        // `Vec.filled(n: i64, val: T) -> Vec[T]` — produces n copies of
        // `val`. Codegen lives at src/codegen/assoc_call.rs:911 (malloc +
        // fill-loop emitting the {data, len=n, cap=n} aggregate). Joins
        // the `Vec.new` / `Vec.with_capacity` family for the same reason
        // those are here — the `resolve_path_type` rejection of unknown
        // `Type.method(...)` calls would otherwise bail out at typecheck
        // before codegen can claim it. Unlike `Vec.with_capacity` we know
        // the element type directly from `val`'s inferred type, so no
        // fresh typevar / downstream-push pinning is needed.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "Vec"
                && segments[1] == "filled"
                && args.len() == 2
            {
                let n_ty = self.infer_expr(&args[0].value);
                self.check_assignable(&Type::Int(IntSize::I64), &n_ty, args[0].value.span.clone());
                let elem_ty = self.infer_expr(&args[1].value);
                let ty = Type::Named {
                    name: "Vec".to_string(),
                    args: vec![elem_ty],
                };
                self.record_expr_type(span, &ty);
                return ty;
            }
        }

        // `Atomic.new(v)` — transparent constructor for the `Atomic[T]`
        // concurrency primitive, recognized in **general expression position**
        // (struct-field-init, local `let`, call args), not just module-binding
        // init (`module_binding_call_is_special_form`). This is what lets the
        // canonical concurrent `par struct Counter { count: Atomic[i64] }` be
        // *constructed*: `Counter { count: Atomic.new(0) }`. There is no
        // `impl Atomic[T] { fn new }` in stdlib (atomic.kara bakes only the
        // type shape), so without this arm the call falls through to the
        // `resolve_path_type` rejection ("no associated function 'new' on type
        // 'Atomic'"). Codegen already handles it — `Atomic[T]` is a transparent
        // wrapper over `T`, so `assoc_call.rs`'s `Atomic && "new"` arm lowers
        // `Atomic.new(v)` to `v` (widening `Atomic[bool]` → i8). The inner type
        // comes straight from the argument, like `Vec.filled`. `Mutex.new(v)`
        // rides the same path — a `Mutex[T]` (spinlock-guarded cell) is built by
        // `assoc_call.rs`'s `Mutex && "new"` arm, and `lock m { ... }` operates
        // on the resulting binding. Both lower to `Wrapper[type_of(v)]`.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && (segments[0] == "Atomic" || segments[0] == "Mutex")
                && segments[1] == "new"
                && args.len() == 1
            {
                let inner_ty = self.infer_expr(&args[0].value);
                // `Atomic[T]` requires `T` to be an integer, `bool`, or raw
                // pointer — the only types LLVM can lower to a single hardware
                // atomic op (deferred.md § Atomic Operations). A non-eligible
                // inner type otherwise slips past `check` and crashes codegen
                // with an opaque LLVM verifier error while the interpreter
                // silently accepts it — a `run`/`build` divergence, so the
                // rejection is run-fatal. `Mutex[T]` is unrestricted (a lock
                // guards any type), so this guard is `Atomic`-only.
                if segments[0] == "Atomic" {
                    let resolved = resolve_type_var_top(&inner_ty, &self.env.substitutions);
                    if let Some(kind) = atomic_ineligible_inner_kind(&resolved) {
                        self.type_error(
                            format!(
                                "Atomic[{}] is not a valid atomic type: {kind} cannot be operated \
                                 on atomically. An atomic's inner type must be an integer \
                                 (i8..i64 / u8..u64 / usize / isize), `bool`, or a raw pointer \
                                 (`*const T` / `*mut T`); to share richer data across tasks use \
                                 `Mutex[T]`, which can guard any type",
                                type_display(&resolved)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::AtomicInvalidInnerType,
                        );
                    }
                }
                let ty = Type::Named {
                    name: segments[0].clone(),
                    args: vec![inner_ty],
                };
                self.record_expr_type(span, &ty);
                return ty;
            }
        }

        // `Vec.with_capacity(n)` — pairs with the `Vec.new()` arm above.
        // Same fresh-typevar return so an untyped `let mut v =
        // Vec.with_capacity(8); v.push(x);` can pin `?T` from the
        // downstream push, and codegen's let-statement
        // `pattern_binding_inner_types` lookup populates `vec_elem_types[v]`
        // — which `pending_let_elem_type` then threads into the
        // `Vec.with_capacity` codegen arm. Without this typechecker arm,
        // the call falls through to the bottom of `infer_call` and
        // returns `Type::Error`, the binding's inner-type table stays
        // empty, codegen sees no pending element type, and errors with
        // "element type unknown — requires a `let v: Vec[T] = ...`
        // annotation". Mirrors `Vec.new`'s shape but checks the
        // capacity arg's type while we're here. `String.with_capacity(n)`
        // joins the family for the same reason `String.new()` is in the
        // `.new` arm above — codegen handles it directly, typechecker
        // would otherwise reject under the `resolve_path_type` rejection
        // path.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[1] == "with_capacity" && args.len() == 1 {
                let collection = segments[0].as_str();
                if collection == "Vec" || collection == "VecDeque" {
                    let cap_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(
                        &Type::Int(IntSize::I64),
                        &cap_ty,
                        args[0].value.span.clone(),
                    );
                    let ty = Type::Named {
                        name: collection.to_string(),
                        args: vec![self.env.fresh_type_var()],
                    };
                    self.record_expr_type(span, &ty);
                    return ty;
                }
                if collection == "String" {
                    let cap_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(
                        &Type::Int(IntSize::I64),
                        &cap_ty,
                        args[0].value.span.clone(),
                    );
                    self.record_expr_type(span, &Type::Str);
                    return Type::Str;
                }
            }
        }

        // `Vec.from_slice(src) -> Vec[T]` — pairs with `Vec.new` /
        // `Vec.with_capacity` / `Vec.filled` in the special-arm family.
        // Codegen handles it (see `src/codegen/assoc_call.rs:~1008`) by
        // bulk-copying the source (Slice[T] / Vec[T] / Array[T,N]) into
        // a freshly-allocated Vec — one malloc + one memcpy/clone-loop,
        // vs the `Vec.new() + push-in-loop` shape which grow-and-reallocs
        // ~log₂(n) times. Without this typechecker arm, the call falls
        // through to the bottom of `infer_call` and panics with
        // "no associated function 'from_slice' on type 'Vec'", as
        // surfaced by kata 1665's `bench/greedy.kara` (2026-05-25). The
        // return type is `Vec[<element>]` where `<element>` is extracted
        // from the source argument's inferred type — recognizes
        // `Slice[T]`, `Vec[T]`, and `Array { element, .. }`.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "Vec"
                && segments[1] == "from_slice"
                && args.len() == 1
            {
                let src_ty = self.infer_expr(&args[0].value);
                let elem_ty = match &src_ty {
                    Type::Slice { element, .. } => (**element).clone(),
                    Type::Named {
                        name,
                        args: ty_args,
                    } if name == "Vec" && ty_args.len() == 1 => ty_args[0].clone(),
                    Type::Array { element, .. } => (**element).clone(),
                    _ => self.env.fresh_type_var(),
                };
                let ty = Type::Named {
                    name: "Vec".to_string(),
                    args: vec![elem_ty],
                };
                self.record_expr_type(span, &ty);
                return ty;
            }
        }

        // `String.from(x)` — codegen-only builtin (no syntactic
        // `impl From for String` in baked stdlib); historically used to
        // convert a `StringSlice` / string literal to an owned `String`,
        // and pervasive across the test corpus (`String.from("hello")`
        // etc.). Joins the special-arm family for the same reason
        // `String.new()` does above — the `resolve_path_type` rejection
        // path would otherwise reject every `String.from(...)` call.
        // We don't strictly validate the arg type here (codegen accepts
        // string literals + StringSlices + Strings transparently); the
        // arg still gets recursive type inference so downstream
        // expressions see its type.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "String"
                && segments[1] == "from"
                && args.len() == 1
            {
                let _ = self.infer_expr(&args[0].value);
                self.record_expr_type(span, &Type::Str);
                return Type::Str;
            }
        }

        // `CStr.from_ptr(p: *const u8) -> ref CStr` — unsafe associated
        // constructor that wraps a raw, caller-owned C string pointer
        // (asserted non-null and NUL-terminated) as a borrowed `CStr`,
        // computing `len` by an O(N) walk to the NUL at runtime (codegen
        // lowers it via libc `strlen`; design.md § Linker Control
        // Attributes / § C-String Literals — the runtime-constructed-`CStr`
        // case). It is the inbound half of the LLVM-C FFI spike's outbound
        // read path: `unsafe { CStr.from_ptr(p) }.to_string()` turns an
        // LLVM-owned `char*` into an owned Kāra `String`
        // (`docs/spikes/self-hosting-llvm-c-ffi.md` sub-q 4). The
        // `unsafe`-context requirement is enforced by the
        // `unsafe_op_in_unsafe_fn` lint (registry seed in `unsafe_lint.rs`),
        // mirroring `ptr.from_exposed`; the interpreter rejects it (no
        // raw-pointer representation under `karac run`). Joins the
        // special-arm family for the same `resolve_path_type`-rejection
        // reason as `String.new()` / `String.from(x)` above.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2
                && segments[0] == "CStr"
                && segments[1] == "from_ptr"
                && args.len() == 1
            {
                let arg_ty = self.infer_expr(&args[0].value);
                let expected = Type::Pointer {
                    is_mut: false,
                    inner: Box::new(Type::UInt(UIntSize::U8)),
                };
                if arg_ty != Type::Error && arg_ty != expected {
                    self.type_error(
                        format!(
                            "CStr.from_ptr expects a `*const u8`, but got `{}`",
                            type_display(&arg_ty)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                let ty = Type::Ref(Box::new(Type::Named {
                    name: "CStr".to_string(),
                    args: vec![],
                }));
                self.record_expr_type(span, &ty);
                return ty;
            }
        }

        // Phase-11 Tensor literal constructor: `Tensor.from(nested array
        // literal)` — dims are inferred from the literal's nesting
        // structure at compile time (design.md § Numerical Types;
        // tracker `phase-11-stdlib-longtail.md` Tensor sub-item "`from`
        // literal constructor"). The argument must be a syntactic array
        // literal: structure (dims) comes from the syntax, leaves are
        // ordinary expressions. A local binding shadowing `Tensor` is
        // routed to method dispatch by the uppercase-receiver rewrite
        // above before this arm is reached.
        if let ExprKind::Path { segments, .. } = &callee.kind {
            if segments.len() == 2 && segments[0] == "Tensor" && segments[1] == "from" {
                return self.infer_tensor_from(args, span);
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

        // Closure-VALUE call through a non-identifier callee — a struct field
        // `(h.f)(x)`, a Vec/array index `v[i](x)`, a tuple index `(t.0)(x)`,
        // etc. The parser gives the callee, the `Call`, and the root atom one
        // shared span, and `infer_expr(callee)` above recorded the callee's
        // `Fn` type at that span in `expr_types` — but the call's *result* type
        // is about to overwrite it there. Stash the callee signature in a
        // dedicated map (codegen recovers the env-first indirect-call ABI from
        // it, B-2026-06-22-4). Identifier / `Path` callees are excluded: a
        // named closure binding is dispatched via `closure_fn_types` and a free
        // fn / assoc fn is a direct call — neither needs this.
        if matches!(
            &callee_ty,
            Type::Function { .. } | Type::OnceFunction { .. }
        ) && !matches!(
            &callee.kind,
            ExprKind::Identifier(_) | ExprKind::Path { .. }
        ) {
            self.fn_value_callee_types.insert(
                SpanKey::from_span(&callee.span),
                Self::type_to_type_expr(&callee_ty),
            );
        }

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
                // Call-site-driven closure param inference (B-2026-07-12-20): a
                // param still an unsolved inference var — an un-annotated closure
                // param with no body constraint to fix it, e.g. `let id = |x| x`
                // — is solved from this monomorphic call's argument type.
                // Closures are monomorphic, so one call fixes the var (a later
                // call with a different type then mismatches, as intended).
                // Only the arg opposite a var param is inferred here (the rare
                // case); concrete params fall straight through to the normal
                // arg-check below. For the identity closure the return shares the
                // param's var, so solving the param also resolves the return. The
                // closure literal's RECORDED type (`expr_types`) is resolved
                // separately in `finalize_closure_expr_types` so codegen's
                // `fn_value_typed_exprs` sees the solved param.
                for (param, arg) in params.iter().zip(args.iter()) {
                    if let Type::TypeVar(id) = resolve_type_var_top(param, &self.env.substitutions)
                    {
                        let arg_ty = self.infer_expr(&arg.value);
                        if arg_ty != Type::Error && !matches!(arg_ty, Type::TypeVar(_)) {
                            self.env.substitutions.insert(id, arg_ty);
                        }
                    }
                }
                let params: Vec<Type> = params
                    .iter()
                    .map(|p| resolve_type_var_top(p, &self.env.substitutions))
                    .collect();
                let return_type =
                    resolve_type_var_top(return_type.as_ref(), &self.env.substitutions);
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
    pub(super) fn check_call_site_marker(&mut self, arg: &CallArg, param_ty: &Type, arg_ty: &Type) {
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

    /// `Tensor.from(nested array literal)` — phase-11 Tensor literal
    /// constructor. Dims come from the literal's *syntactic* nesting
    /// (leftmost spine establishes the rank; every sibling level is
    /// checked against it — raggedness is a compile error), the element
    /// type from the first leaf (remaining leaves are checked against
    /// it). Produces a fully concrete `Tensor[T, [d0, d1, ...]]`, so an
    /// annotated binding gets `E_SHAPE` agreement for free via
    /// `check_assignable`. Leaf array literals are always structure,
    /// never data — runtime-shaped data goes through `Tensor.zeros` /
    /// `Tensor.full` + indexed writes. Interpreter twin (same
    /// syntax-walk, since runtime `Value`s can't distinguish a nested
    /// row from a leaf `Vec`): `eval_tensor_from` in
    /// `src/interpreter/method_call_tensor.rs`.
    fn infer_tensor_from(&mut self, args: &[CallArg], span: &Span) -> Type {
        if args.len() != 1 {
            self.type_error(
                format!(
                    "Tensor.from takes exactly 1 argument (a nested array literal), found {}",
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
        let data = &args[0].value;
        if !matches!(data.kind, ExprKind::ArrayLiteral(_)) {
            self.type_error(
                "Tensor.from requires an array-literal argument — dims are inferred \
                 from the literal's nesting (`Tensor.from([[1.0, 2.0], [3.0, 4.0]])`); \
                 for runtime-shaped data use `Tensor.zeros(dims)` / `Tensor.full(dims, \
                 value)` plus indexed writes"
                    .to_string(),
                data.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            self.infer_expr(data);
            return Type::Error;
        }
        let mut dims: Vec<i64> = Vec::new();
        let mut leaves: Vec<&Expr> = Vec::new();
        if let Err((msg, err_span)) = collect_tensor_literal(data, 0, &mut dims, &mut leaves) {
            self.type_error(msg, err_span, TypeErrorKind::TypeMismatch);
            return Type::Error;
        }
        // `leaves` is non-empty by construction — empty levels error in
        // the walk above.
        let elem_ty = self.infer_expr(leaves[0]);
        for leaf in &leaves[1..] {
            self.check_expr(leaf, &elem_ty);
        }
        let ty = Type::Named {
            name: "Tensor".to_string(),
            args: vec![
                elem_ty,
                Type::Shape(
                    dims.iter()
                        .map(|&d| DimArg::Const(ConstArg::Literal(d)))
                        .collect(),
                ),
            ],
        };
        self.record_expr_type(span, &ty);
        ty
    }

    /// `Stats.<method>(xs, …)` — the S5 element-kind-aware typing of the
    /// `std.stats` free functions (see the intercept in [`Self::infer_call`]
    /// for the rationale). The result table by element kind:
    ///
    /// | method | `Slice[f64]` | `Slice[i64]` |
    /// |---|---|---|
    /// | `sum` / `prod` | `f64` | `i64` (element-typed) |
    /// | `mean`/`variance`/`stddev`/`median`/`percentile` | `f64` | `f64` (promotes) |
    /// | `min` / `max` | `Option[f64]` | `Option[i64]` |
    /// | `argmin` / `argmax` | `Option[i64]` | `Option[i64]` |
    /// | `sort` | `Vec[f64]` | `Vec[i64]` |
    /// | `argsort` | `Vec[i64]` | `Vec[i64]` |
    ///
    /// Any other element kind (`i32`/`u8`/`f32`/…) is a hard error: the
    /// interpreter evaluates ints at i64 and floats at f64, so a narrower
    /// element can't be width-faithful on both surfaces until the
    /// interpreter width-laxity class (B-2026-07-01-3) is resolved — and
    /// before this intercept those calls silently produced denormal garbage
    /// under `karac build`. The chosen kind is recorded in
    /// `stats_elem_types` at the call span for codegen.
    fn infer_stats_call(&mut self, method: &str, args: &[CallArg], span: &Span) -> Type {
        use super::types::FloatSize;
        let f64_t = Type::Float(FloatSize::F64);
        let i64_t = Type::Int(IntSize::I64);
        let opt = |t: Type| Type::Named {
            name: "Option".to_string(),
            args: vec![t],
        };
        let vec_of = |t: Type| Type::Named {
            name: "Vec".to_string(),
            args: vec![t],
        };

        // Arity: `percentile(xs, p)` is binary; everything else is unary.
        let nargs = if method == "percentile" { 2 } else { 1 };
        if args.len() != nargs {
            self.type_error(
                format!(
                    "Stats.{method} expects {nargs} argument(s), got {}",
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

        let arg_ty = self.infer_expr(&args[0].value);
        // Unwrap a borrow; extract the element from Vec / Slice / Array.
        let core = match &arg_ty {
            Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
            other => other,
        };
        let elem = match core {
            Type::Named { name, args } if name == "Vec" && args.len() == 1 => Some(&args[0]),
            Type::Slice { element, .. } => Some(element.as_ref()),
            Type::Array { element, .. } => Some(element.as_ref()),
            _ => None,
        };
        let is_int = match elem {
            Some(Type::Int(IntSize::I64)) => true,
            Some(Type::Float(FloatSize::F64)) => false,
            // An unresolved element (`vec![]` pending inference) keeps the
            // pre-S5 posture: the f64 signature.
            Some(Type::TypeVar(_)) | Some(Type::Error) => false,
            Some(other_elem) => {
                self.type_error(
                    format!(
                        "Stats.{method} supports f64 and i64 slice elements, found \
                         '{}' — convert the elements to f64 or i64 first",
                        type_display(other_elem)
                    ),
                    args[0].value.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                for arg in args.iter().skip(1) {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
            None => {
                if arg_ty != Type::Error {
                    self.type_error(
                        format!(
                            "Stats.{method} expects a Slice[f64] or Slice[i64] argument, \
                             found '{}'",
                            type_display(&arg_ty)
                        ),
                        args[0].value.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                for arg in args.iter().skip(1) {
                    self.infer_expr(&arg.value);
                }
                return Type::Error;
            }
        };

        // `percentile(xs, p)` — `p` is an f64 in [0, 100] (range checked at
        // runtime); an int literal coerces.
        if method == "percentile" {
            let p_ty = self.infer_expr(&args[1].value);
            self.check_assignable(&f64_t, &p_ty, args[1].value.span.clone());
        }

        // Record the element kind for codegen (the reduction's LLVM element
        // type — the buffer would otherwise be read as f64 regardless).
        let elem_te = Self::type_to_type_expr(if is_int { &i64_t } else { &f64_t });
        self.stats_elem_types
            .insert(SpanKey(span.offset, span.length), elem_te);

        let ty = match method {
            "sum" | "prod" => {
                if is_int {
                    i64_t
                } else {
                    f64_t
                }
            }
            "mean" | "variance" | "stddev" | "median" | "percentile" => f64_t,
            "min" | "max" => opt(if is_int { i64_t } else { f64_t }),
            "argmin" | "argmax" => opt(i64_t),
            "sort" => vec_of(if is_int { i64_t } else { f64_t }),
            "argsort" => vec_of(i64_t),
            _ => unreachable!("infer_stats_call gate lists exactly the Stats methods"),
        };
        self.record_expr_type(span, &ty);
        ty
    }
}

/// Classify an `Atomic[T]` inner type for atomic eligibility. Returns
/// `Some(human-readable kind)` when `T` is a *definitively concrete*
/// non-eligible type (so the caller can reject with a clear message), and
/// `None` when `T` is eligible (integer / `bool` / raw pointer, per
/// deferred.md § Atomic Operations) OR when `T` is not yet pinned down —
/// an unresolved inference var, a generic type parameter, an opaque
/// existential / associated-type projection, or an error type. Deferring
/// the uncertain cases keeps the guard from false-positiving inside a
/// generic body (`fn f[T](x: T) { Atomic.new(x) }`); a truly-bad
/// monomorphization is still caught downstream by codegen.
fn atomic_ineligible_inner_kind(ty: &Type) -> Option<&'static str> {
    match ty {
        // Eligible — the set LLVM lowers to a single hardware atomic.
        Type::Int(_) | Type::UInt(_) | Type::Bool | Type::Pointer { .. } => None,
        // Not-yet-known — defer, do not reject.
        Type::TypeVar(_)
        | Type::TypeParam(_)
        | Type::Error
        | Type::AssocProjection { .. }
        | Type::Existential { .. } => None,
        // Concrete but ineligible — name the shape for the diagnostic.
        Type::Float(_) => Some("a floating-point type"),
        Type::Char => Some("`char`"),
        Type::Str => Some("a string type"),
        Type::Tuple(_) => Some("a tuple"),
        Type::Array { .. } => Some("an array"),
        Type::Vector { .. } => Some("a SIMD vector"),
        Type::Slice { .. } => Some("a slice"),
        Type::Shared(_) => Some("a shared struct"),
        Type::Rc(_) => Some("an `Rc[T]`"),
        Type::Arc(_) => Some("an `Arc[T]`"),
        Type::Function { .. } | Type::OnceFunction { .. } => Some("a function type"),
        Type::Ref(_) | Type::MutRef(_) => Some("a reference type"),
        Type::Weak(_) => Some("a `weak` reference"),
        Type::Named { name, .. } if name == "String" => Some("a string type"),
        // Any other concrete named type (a plain struct / enum) is ineligible.
        Type::Named { .. } => Some("a struct or enum type"),
        // Unit / Never / shape-kinded and any future variant — reject by
        // default (the safe direction: a new eligible type must be added to
        // the eligible arm above explicitly, never silently allowed here).
        _ => Some("this type"),
    }
}

/// Recursive walk for `Tensor.from`'s literal argument. The leftmost
/// spine *establishes* `dims` (first visit at each depth pushes its
/// length); every other visit *checks* against the established entry —
/// length mismatch or nesting-depth mismatch is raggedness. Leaf
/// expressions (non-array-literal elements) are collected in C-order;
/// the caller infers their types. Errors carry the user-facing message
/// plus the offending sub-literal's span.
fn collect_tensor_literal<'e>(
    expr: &'e Expr,
    depth: usize,
    dims: &mut Vec<i64>,
    leaves: &mut Vec<&'e Expr>,
) -> Result<(), (String, Span)> {
    let ExprKind::ArrayLiteral(elements) = &expr.kind else {
        // Reached only on a depth mismatch where an established deeper
        // dim expects nesting but this element is a scalar expression.
        return Err((
            format!(
                "ragged tensor literal: expected a nested level at depth {} \
                 (rank established as {} by the first element), found a scalar",
                depth,
                dims.len()
            ),
            expr.span.clone(),
        ));
    };
    if elements.is_empty() {
        return Err((
            "cannot infer tensor dims from an empty literal level — \
             zero-size tensors go through `Tensor.zeros(dims)`"
                .to_string(),
            expr.span.clone(),
        ));
    }
    let len = elements.len() as i64;
    let first_visit = dims.len() == depth;
    if first_visit {
        dims.push(len);
    } else if dims[depth] != len {
        return Err((
            format!(
                "ragged tensor literal: level at depth {} has {} element(s), expected {}",
                depth, len, dims[depth]
            ),
            expr.span.clone(),
        ));
    }
    let nested = if first_visit {
        // Establishing visit — nesting is whatever the elements say,
        // but mixing scalars and arrays in one level is ragged.
        let any_array = elements
            .iter()
            .any(|e| matches!(e.kind, ExprKind::ArrayLiteral(_)));
        let all_array = elements
            .iter()
            .all(|e| matches!(e.kind, ExprKind::ArrayLiteral(_)));
        if any_array && !all_array {
            return Err((
                "ragged tensor literal: level mixes scalar and nested elements".to_string(),
                expr.span.clone(),
            ));
        }
        any_array
    } else {
        // Revisit — the established rank dictates whether this level
        // holds rows or leaves.
        let expect_nested = dims.len() > depth + 1;
        if !expect_nested {
            if let Some(arr) = elements
                .iter()
                .find(|e| matches!(e.kind, ExprKind::ArrayLiteral(_)))
            {
                return Err((
                    format!(
                        "ragged tensor literal: expected a scalar leaf at depth {} \
                         (rank established as {} by the first element), found a nested level",
                        depth + 1,
                        dims.len()
                    ),
                    arr.span.clone(),
                ));
            }
        }
        expect_nested
    };
    if nested {
        for elem in elements {
            collect_tensor_literal(elem, depth + 1, dims, leaves)?;
        }
    } else {
        leaves.extend(elements.iter());
    }
    Ok(())
}
