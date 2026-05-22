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
use crate::token::Span;

use super::inference::{expr_as_type_expr, is_literal_const_arg_expr};
use super::types::{type_display, IntSize, Type, UIntSize};
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

    pub(super) fn infer_call(&mut self, callee: &Expr, args: &[CallArg], span: &Span) -> Type {
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
}
