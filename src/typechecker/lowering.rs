//! Type-expression lowering: AST `TypeExpr` → internal `Type`.
//!
//! Houses the recursive `lower_type_expr*` walker, the path-type
//! resolver (`lower_path_type`), array / slice lowering with
//! const-arg threading, primitive-type name lookup, generic-param
//! name collection, parameter-bound gathering, associated-type
//! projection resolution, alias-chain resolution, and element-type
//! extraction. Lives in a sibling `impl<'a> super::TypeChecker<'a>`
//! block.

use crate::ast::*;
use std::collections::{HashMap, HashSet};

use super::const_eval::{const_value_to_array_size, const_value_type};
use super::inference::substitute_type_params;
use super::types::{
    type_display, ConstArg, DimArg, FloatSize, IntSize, SubstValue, Type, UIntSize,
};
use super::TypeErrorKind;
use crate::ast::narrow_literal_to_i64;
use crate::token::Span;

impl<'a> super::TypeChecker<'a> {
    // ── lower_type_expr ─────────────────────────────────────────

    pub(super) fn lower_type_expr(&mut self, ty: &TypeExpr, generic_scope: &[String]) -> Type {
        // Top-level entry: by default the type is in a sized-by-value
        // position. Slice 1b's `E_OPAQUE_TYPE_REQUIRES_INDIRECTION` check
        // flips `parent_is_ref` to `true` only when descending through
        // `TypeKind::Ref` / `TypeKind::MutRef` / `TypeKind::Pointer` (raw
        // pointers are sized regardless of pointee, just like references —
        // the slice-1b carry-forward closed once `*const T` surface landed),
        // so opaque-foreign-type names are accepted at `ref Foo` /
        // `mut ref Foo` / `*const Foo` / `*mut Foo` and rejected everywhere
        // else (fn params/return, struct fields, enum payloads, let
        // bindings, generic args, tuples, arrays, etc.).
        self.lower_type_expr_inner(ty, generic_scope, false)
    }

    pub(super) fn lower_type_expr_inner(
        &mut self,
        ty: &TypeExpr,
        generic_scope: &[String],
        parent_is_ref: bool,
    ) -> Type {
        match &ty.kind {
            TypeKind::Path(path) => {
                let lowered = self.lower_path_type(path, generic_scope);
                // Slice 1b: opaque foreign types declared via `unsafe extern
                // "ABI" { type Foo; }` have no known size and cannot appear
                // by value. `parent_is_ref` is `true` only when the
                // immediate parent is `Ref` / `MutRef` / `Pointer`, so
                // `Vec[Foo]` (Foo by-value inside Vec) and `ref Vec[Foo]`
                // (Foo still by-value inside Vec) both correctly emit;
                // `ref Foo`, `mut ref Foo`, `*const Foo`, and `*mut Foo`
                // do not. The lowered type is returned unchanged so
                // downstream phases see the user's intent for recovery
                // purposes.
                if !parent_is_ref {
                    if let Type::Named { name, .. } = &lowered {
                        if self.env.opaque_foreign_types.contains(name) {
                            self.type_error(
                                format!(
                                    "error[E_OPAQUE_TYPE_REQUIRES_INDIRECTION]: opaque \
                                     foreign type '{name}' has no known size and cannot \
                                     appear by value here; wrap it in `ref {name}` / \
                                     `mut ref {name}` (or `*const {name}` / `*mut {name}` \
                                     in FFI signatures) to use it through indirection"
                                ),
                                ty.span,
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    }
                }
                lowered
            }
            TypeKind::Tuple(types) => Type::Tuple(
                types
                    .iter()
                    .map(|t| self.lower_type_expr_inner(t, generic_scope, false))
                    .collect(),
            ),
            TypeKind::Array { element, .. } => {
                Type::Array {
                    element: Box::new(self.lower_type_expr_inner(element, generic_scope, false)),
                    size: ConstArg::Literal(0), // const eval deferred
                }
            }
            // Raw pointers are sized regardless of their pointee, exactly
            // like references — `*const Foo` for opaque foreign `Foo` is the
            // canonical C opaque-handle FFI shape and must not fire
            // `E_OPAQUE_TYPE_REQUIRES_INDIRECTION` (phase-5 slice-1b
            // carry-forward, closed once `*const T` parser surface landed).
            TypeKind::Pointer { is_mut, inner } => Type::Pointer {
                is_mut: *is_mut,
                inner: Box::new(self.lower_type_expr_inner(inner, generic_scope, true)),
            },
            TypeKind::FnType {
                params,
                return_type,
                is_once,
                ..
            } => {
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|t| self.lower_type_expr_inner(t, generic_scope, false))
                    .collect();
                let ret = return_type
                    .as_ref()
                    .map(|t| self.lower_type_expr_inner(t, generic_scope, false))
                    .unwrap_or(Type::Unit);
                if *is_once {
                    Type::OnceFunction {
                        params: param_types,
                        return_type: Box::new(ret),
                    }
                } else {
                    Type::Function {
                        params: param_types,
                        return_type: Box::new(ret),
                    }
                }
            }
            // STAGE 1 (B-2026-08-01-33 mechanism 3): `frozen T` lowers to the
            // SAME semantic type as `T` — there is deliberately no
            // `Type::Frozen`. Keeping the mode out of the semantic type is what
            // makes this increment inert: no downstream pass can act on it, so
            // none can act on it WRONGLY before the escape checker exists.
            // `parent_is_ref` is passed through rather than forced to `true`
            // (as the `Ref` arm below does) so an opaque foreign type under a
            // bare `frozen` is still rejected — fail-closed while the mode
            // carries no guarantees yet.
            TypeKind::Frozen(inner) => {
                self.lower_type_expr_inner(inner, generic_scope, parent_is_ref)
            }
            TypeKind::Ref(inner) => Type::Ref(Box::new(self.lower_type_expr_inner(
                inner,
                generic_scope,
                true,
            ))),
            TypeKind::MutRef(inner) => Type::MutRef(Box::new(self.lower_type_expr_inner(
                inner,
                generic_scope,
                true,
            ))),
            TypeKind::MutSlice(element) => Type::Slice {
                element: Box::new(self.lower_type_expr_inner(element, generic_scope, false)),
                mutable: true,
            },
            TypeKind::Weak(inner) => Type::Weak(Box::new(self.lower_type_expr_inner(
                inner,
                generic_scope,
                false,
            ))),
            // `impl Trait` slice 3: return-position / RPITIT / TAIT-RHS
            // lower to `Type::Existential` carrying the named trait bound
            // and a `SpanKey` origin that identifies the declaration site
            // (so two `impl Iterator` declarations on different fns stay
            // distinct even with structurally identical bounds).
            // Argument-position was already eliminated by the slice-2
            // resolver desugar, so any `TypeKind::ImplTrait` reaching this
            // site is in a return / TAIT / non-arg position. Caller-side
            // opacity, body-return checking, and trait-surface method
            // dispatch are handled at `check_assignable` /
            // `is_subtype_with_projections` / `type_satisfies_bound` and
            // the receiver-dispatch path. See phase-5-diagnostics.md line
            // 397 for the slice 3 entry.
            TypeKind::ImplTrait {
                trait_path,
                args,
                assoc_bindings,
                ..
            } => {
                let trait_args: Vec<Type> = args
                    .iter()
                    .filter_map(|a| match a {
                        GenericArg::Type(t) => {
                            Some(self.lower_type_expr_inner(t, generic_scope, false))
                        }
                        // Const-args on trait paths aren't part of slice 3
                        // — the parser accepts them but `Type::Existential`
                        // has no representation for const arguments today.
                        GenericArg::Const(_) => None,
                        // Shape args on trait paths have no meaning until
                        // the Dim/Shape kind system lands (Phase 11 Q1).
                        GenericArg::Shape(_) => None,
                    })
                    .collect();
                // Inline associated-type bindings ride the existential —
                // there is no named parameter to hang a where-constraint on
                // (B-2026-08-22-4). Argument-position `impl Trait` never
                // reaches here: the slice-2 desugar already turned it into a
                // named parameter and hoisted its bindings onto the where
                // clause, which is why only the return / TAIT positions need
                // this field at all.
                let trait_name = trait_path.segments.join(".");
                let lowered_bindings: Vec<(String, Type)> = assoc_bindings
                    .iter()
                    .map(|b| {
                        (
                            b.name.clone(),
                            self.lower_type_expr_inner(&b.ty, generic_scope, false),
                        )
                    })
                    .collect();
                // A binding that names no associated type of the trait binds
                // nothing: the projection it was meant to answer keeps a
                // different name, so it stays unresolved and the caller gets
                // the pre-binding "cannot infer" error pointing at their own
                // correct code. Reject it here, where the typo is.
                for b in assoc_bindings {
                    self.check_trait_declares_assoc_type(&trait_name, &b.name, b.span);
                }
                Type::Existential {
                    trait_name,
                    trait_args,
                    assoc_bindings: lowered_bindings,
                    origin: crate::resolver::SpanKey::from_span(&ty.span),
                    // Slice 6: the TAIT marker is set later by
                    // `env_add_type_alias` when this lowering happens
                    // inside a `type X = impl Trait;` declaration.
                    // Return-position / RPITIT existentials from slice
                    // 3 stay `None` — their alias-less origin is the
                    // distinguishing signal for slice 6's TAIT-aware
                    // diagnostics.
                    tait_alias: None,
                }
            }
            // `dyn Trait` slice 5 — the dual of `impl Trait`. Slice 5
            // ships the RPITIT-incompatibility check: when the named
            // trait has any method declared with `-> impl Trait`
            // (return-position impl trait in trait), `dyn Trait`
            // cannot synthesize a fixed vtable slot for that method.
            // Lower to `Type::Error` with `E_RPITIT_INCOMPATIBLE_WITH_DYN`
            // naming the first offending method. When the trait has no
            // RPITIT methods, emit the generic `E_DYN_TRAIT_NOT_IMPLEMENTED_YET`
            // P1-deferred stub (general `dyn Trait` value/type
            // semantics, vtable layout, and effect checking are P1 per
            // design.md § Polymorphism). Walking generic args before
            // emitting surfaces any nested type errors.
            TypeKind::Dyn {
                trait_path, args, ..
            } => {
                for a in args {
                    if let GenericArg::Type(t) = a {
                        let _ = self.lower_type_expr_inner(t, generic_scope, false);
                    }
                }
                let trait_name = trait_path.segments.join(".");
                if let Some(offending) = Self::trait_first_rpitit_method(self.program, &trait_name)
                {
                    self.type_error(
                        format!(
                            "error[E_RPITIT_INCOMPATIBLE_WITH_DYN]: cannot use \
                             `dyn {trait_name}` because method `{offending}` returns \
                             `impl Trait` — return-position `impl Trait` in trait \
                             methods (RPITIT) has no fixed vtable slot, so `dyn`-dispatched \
                             callers cannot synthesize a thunk; route through a generic \
                             parameter `[T: {trait_name}]` instead"
                        ),
                        ty.span,
                        TypeErrorKind::TypeMismatch,
                    );
                } else {
                    self.type_error(
                        format!(
                            "error[E_DYN_TRAIT_NOT_IMPLEMENTED_YET]: `dyn {trait_name}` is \
                             parsed but the trait-object machinery (vtable construction, \
                             dynamic dispatch, effect-opacity story) is P1-deferred per \
                             design.md § Polymorphism; route through a generic parameter \
                             `[T: {trait_name}]` for now"
                        ),
                        ty.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                Type::Error
            }
            TypeKind::Unit => Type::Unit,
            TypeKind::Error => Type::Error,
        }
    }

    /// `impl Trait` slice 5 — return `Some(method_name)` for the first
    /// method of `trait_name` whose return type is `TypeKind::ImplTrait`,
    /// i.e. the trait declares an RPITIT method. Walks the AST trait
    /// declaration directly (the typed env doesn't carry per-method
    /// return-shape information in a queryable form). Returns `None`
    /// when the trait is not defined in the current program (e.g.,
    /// baked stdlib trait), when no methods return `impl Trait`, or
    /// when the name resolves to a non-trait item — slice 5's check
    /// fires only on positively-identified RPITIT traits; the generic
    /// `E_DYN_TRAIT_NOT_IMPLEMENTED_YET` stub covers the rest.
    /// Emit a diagnostic when `trait_name` is defined in this program and does
    /// NOT declare an associated type called `assoc`.
    ///
    /// Silent when the trait is not found in `program.items` — a baked stdlib
    /// trait has no AST here, and failing closed would reject every correct
    /// binding on one. Same fail-open rule as `trait_first_rpitit_method`.
    fn check_trait_declares_assoc_type(&mut self, trait_name: &str, assoc: &str, span: Span) {
        let Some(t) = self.program.items.iter().find_map(|item| match item {
            Item::TraitDef(t) if t.name == trait_name => Some(t),
            _ => None,
        }) else {
            return;
        };
        if !self
            .assoc_binding_typos_reported
            .insert((span.offset, span.length))
        {
            return;
        }
        let mut declared: Vec<&str> = Vec::new();
        for ti in &t.items {
            if let TraitItem::AssocType(a) = ti {
                if a.name == assoc {
                    return;
                }
                declared.push(&a.name);
            }
        }
        let known = if declared.is_empty() {
            format!("trait `{trait_name}` declares no associated types")
        } else {
            format!("`{trait_name}` declares: {}", declared.join(", "))
        };
        self.type_error(
            format!(
                "error[E_UNKNOWN_ASSOC_TYPE_BINDING]: `{assoc}` is not an associated \
                 type of trait `{trait_name}`, so the binding `{assoc} = ...` \
                 constrains nothing — {known}"
            ),
            span,
            TypeErrorKind::TypeMismatch,
        );
    }

    fn trait_first_rpitit_method(
        program: &crate::ast::Program,
        trait_name: &str,
    ) -> Option<String> {
        for item in &program.items {
            if let Item::TraitDef(t) = item {
                if t.name != trait_name {
                    continue;
                }
                for trait_item in &t.items {
                    if let TraitItem::Method(method) = trait_item {
                        if let Some(ref ret) = method.return_type {
                            if matches!(ret.kind, TypeKind::ImplTrait { .. }) {
                                return Some(method.name.clone());
                            }
                        }
                    }
                }
                return None;
            }
        }
        None
    }

    /// Const generics slice 3d: deferred-F regression-pin diagnostic.
    /// `Type::Named.args` is `Vec<Type>` — there's no representation
    /// for a const-arg on a user-defined struct / enum. When a
    /// `GenericArg::Const` payload arrives at a `Type::Named` lowering
    /// site, emit a focused "const-args on user-defined types are not
    /// yet supported" diagnostic and drop the const-arg. The
    /// surrounding type name (when known) flows into the message so
    /// users see which call site triggered the rejection. `Array[T, N]`
    /// const-args don't reach here — they're special-cased in
    /// `lower_array_type` before `lower_generic_args` is invoked.
    fn lower_generic_args_named(
        &mut self,
        generic_args: &Option<Vec<GenericArg>>,
        generic_scope: &[String],
        type_name: Option<&str>,
    ) -> Vec<Type> {
        generic_args
            .as_ref()
            .map(|ga| {
                ga.iter()
                    .enumerate()
                    .filter_map(|(arg_idx, arg)| match arg {
                        GenericArg::Type(t) => Some(self.lower_type_expr(t, generic_scope)),
                        GenericArg::Const(expr) => {
                            let target = type_name.unwrap_or("this type");
                            self.type_error(
                                format!(
                                    "const generic argument on user-defined type '{}' is \
                                     not yet supported in this slice; only the built-in \
                                     `Array[T, N]` accepts const-args at v1",
                                    target
                                ),
                                expr.span,
                                TypeErrorKind::TypeMismatch,
                            );
                            None
                        }
                        // Shape literal arg — legal iff the target type
                        // declares a shape-variadic (`...S`) param at this
                        // position (Phase 11 Q1; design.md § Numerical
                        // Types > Shape kind).
                        GenericArg::Shape(lit) => {
                            let accepts_shape = type_name
                                .and_then(|n| self.env.shape_param_positions.get(n))
                                .and_then(|positions| positions.get(arg_idx))
                                .copied()
                                .unwrap_or(false);
                            if accepts_shape {
                                Some(self.lower_shape_literal(lit, generic_scope))
                            } else {
                                let target = type_name.unwrap_or("this type");
                                self.type_error(
                                    format!(
                                        "shape literal argument on '{}' does not match a \
                                         shape-kinded generic parameter at this position — \
                                         declare the parameter as `...S` (shape-variadic) \
                                         to accept a shape literal here",
                                        target
                                    ),
                                    lit.span,
                                    TypeErrorKind::TypeMismatch,
                                );
                                None
                            }
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The two hasher SELECTORS a `Map` / `Set` may name in its trailing type
    /// argument (`runtime/stdlib/hash.kara`). design.md § `Hash` and `Hasher`
    /// spells both.
    pub(crate) const HASHER_SELECTORS: [&'static str; 2] =
        ["SipHash13BuildHasher", "FxBuildHasher"];

    /// True when `ty` names one of [`Self::HASHER_SELECTORS`].
    pub(crate) fn is_hasher_selector(ty: &Type) -> bool {
        matches!(ty, Type::Named { name, args }
            if args.is_empty() && Self::HASHER_SELECTORS.contains(&name.as_str()))
    }

    /// True when `ty` is anything that may legally sit in a container's hasher
    /// slot — one of the two builtin selectors, or a user type that
    /// `impl BuildHasher for` (B-2026-08-22-6).
    ///
    /// Only reachable for an argument the PARSER left in place, which today
    /// means the ordered-container arm below: a `Map` / `Set` hasher is already
    /// gone by the time lowering runs, and is validated against the same impl
    /// table by [`Self::check_recorded_container_hasher`].
    pub(crate) fn is_hasher_argument(&self, ty: &Type) -> bool {
        if Self::is_hasher_selector(ty) {
            return true;
        }
        matches!(ty, Type::Named { name, args }
            if args.is_empty() && self.env.has_impl("BuildHasher", name, &[]))
    }

    /// Report a `Map[K, V, H]` / `Set[T, H]` whose trailing argument names a
    /// type that does not `impl BuildHasher` (B-2026-08-22-6).
    ///
    /// WHY THE CHECK IS HERE AND NOT IN THE PARSER. The parser has no impl
    /// table, so it strips ANY bare trailing path and records the name (see
    /// `Parser::take_container_hasher_arg`); `Map[K, V, i64]`, `Map[K, V,
    /// Fxbuildhasher]` and a legitimate `Map[K, V, MyBuildHasher]` all arrive
    /// here indistinguishable. This is the first phase that can tell them
    /// apart, and it reads the impl table both backends will later dispatch
    /// through, so an accepted spelling is one they can both actually run.
    ///
    /// Keyed off `path.span`, which is the same span the parser recorded under
    /// — `PathExpr::span` covers the whole `Map[…]` write and is built once, in
    /// `parse_path_type`, before the argument is removed.
    fn check_recorded_container_hasher(&mut self, name: &str, path: &PathExpr) {
        if name != "Map" && name != "Set" {
            return;
        }
        let key = crate::resolver::SpanKey::from_span(&path.span);
        let Some(crate::hasher_kind::HasherKind::User(builder)) =
            self.program.container_hashers.get(&key).cloned()
        else {
            return;
        };
        if !self.checked_container_hashers.insert(key) {
            return;
        }
        if !self.env.has_impl("BuildHasher", &builder, &[]) {
            self.type_error(
                format!(
                    "the last type argument of '{name}' is the hasher, and '{builder}' cannot \
                     be one: it does not implement 'BuildHasher'. Use 'SipHash13BuildHasher' \
                     (the default: SipHash-1-3, keyed per process) or 'FxBuildHasher' (unkeyed \
                     and floodable, but stable across runs), or write \
                     `impl BuildHasher for {builder} {{ type Hasher = …; \
                     fn build(ref self) -> … }}`"
                ),
                path.span,
                TypeErrorKind::TypeMismatch,
            );
            return;
        }

        // The slot names a TYPE, not a value, so there is nowhere for a
        // stateful builder's fields to come from: both backends construct the
        // builder themselves before calling `build()`, and neither has a
        // constructor argument to pass. A field-carrying builder would
        // therefore be read as whatever its default construction happened to
        // be, which is exactly the silent-wrong-answer this rejects. A seed
        // belongs inside `build()`, where it can be read from wherever it
        // actually lives.
        if let Some(info) = self.env.structs.get(&builder) {
            if !info.fields.is_empty() {
                let field = info.fields[0].0.clone();
                self.type_error(
                    format!(
                        "'{builder}' cannot be a hasher for '{name}': a 'BuildHasher' named in a \
                         container's type must be a struct with NO fields, but '{builder}' has \
                         '{field}'. The container names a TYPE, so there is no value to carry \
                         per-table configuration — construct the state inside \
                         `{builder}.build()` instead"
                    ),
                    path.span,
                    TypeErrorKind::TypeMismatch,
                );
            }
        }

        // The `type Hasher: Hasher` bound is NOT checked here any more
        // (B-2026-08-22-28). It used to be, because the general
        // associated-type-bound machinery could not discharge a bound declared
        // on a stdlib-BAKED trait — it looked the decl up in `program.items`,
        // which holds only the user program. That is fixed, so `impl
        // BuildHasher for B { type Hasher = <not a Hasher> }` is now rejected
        // at the IMPL, where the mistake is, and keeping this copy produced
        // two errors for one mistake (measured). The two checks above stay:
        // neither "does not implement BuildHasher" nor "a builder named in a
        // container's type must be field-less" is an associated-type bound, so
        // the general mechanism does not cover them.
    }

    /// Validate and STRIP the trailing hasher argument of `Map[K, V, H]` /
    /// `Set[T, H]` (B-2026-08-21-6).
    ///
    /// Stripping rather than carrying `H` in the `Type` is the whole trick.
    /// `Map.new()` mints `Map[?K, ?V]`, hundreds of sites index `args[0]` /
    /// `args[1]`, and `Map[String, i64]` must stay the SAME type as
    /// `Map[String, i64, SipHash13BuildHasher]` — naming the default cannot
    /// change what a value is assignable to. A third `Type` argument would
    /// have to be threaded through unification, substitution, display and
    /// every one of those sites to buy nothing, because the hasher is not part
    /// of the value's shape: it decides which hash function the CONSTRUCTOR
    /// installs, and both backends read that off the syntactic `TypeExpr` at
    /// the construction site. So the choice never needs to survive inference.
    ///
    /// The arity check here is new. Before it, `Map[String, i64, i64]` type
    /// checked silently — extra arguments were dropped on the floor — which
    /// would have made this feature indistinguishable from a no-op for anyone
    /// who mistyped the hasher name.
    ///
    /// Since B-2026-08-22-6 the parser strips a bare trailing path from a `Map`
    /// / `Set` whatever it names, so the `args.len() == base + 1` arms below are
    /// reachable only for an argument that is NOT a bare path — a parameterized
    /// `Map[K, V, Vec[u8]]`, a tuple, a function type. The plain-name cases
    /// (including `Map[K, V, i64]` and every misspelling) are reported by
    /// [`Self::check_recorded_container_hasher`], called first from here so the
    /// two halves cannot both fire on one write.
    fn take_hasher_type_arg(&mut self, name: &str, args: Vec<Type>, path: &PathExpr) -> Vec<Type> {
        self.check_recorded_container_hasher(name, path);
        let base = match name {
            "Map" => 2,
            "Set" => 1,
            // Ordered containers compare, they do not hash. Naming a hasher on
            // one is a misunderstanding worth reporting rather than ignoring.
            "SortedMap" | "SortedSet" => {
                let base = if name == "SortedMap" { 2 } else { 1 };
                if args.len() == base + 1 && self.is_hasher_argument(&args[base]) {
                    self.type_error(
                        format!(
                            "'{name}' is an ordered container and does not hash its keys, so \
                             it takes no hasher argument; drop it, or use \
                             '{}' if you want a hash container",
                            if name == "SortedMap" { "Map" } else { "Set" }
                        ),
                        path.span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    let mut args = args;
                    args.truncate(base);
                    return args;
                }
                return args;
            }
            _ => return args,
        };

        // Zero args is the inference-driven form (`Map.new()` before the slots
        // are solved) and `base` args is the ordinary spelling; neither has a
        // hasher to take.
        if args.len() <= base {
            return args;
        }

        let mut args = args;
        if args.len() > base + 1 {
            self.type_error(
                format!(
                    "'{name}' takes {base} type argument{} plus an optional hasher, but {} \
                     were supplied",
                    if base == 1 { "" } else { "s" },
                    args.len()
                ),
                path.span,
                TypeErrorKind::WrongNumberOfArgs,
            );
            args.truncate(base);
            return args;
        }

        if !self.is_hasher_argument(&args[base]) {
            self.type_error(
                format!(
                    "the last type argument of '{name}' is the hasher and must be \
                     'SipHash13BuildHasher' (the default: SipHash-1-3, keyed per process), \
                     'FxBuildHasher' (unkeyed and floodable, but stable across runs), or a \
                     type that implements 'BuildHasher'; found '{}'",
                    type_display(&args[base])
                ),
                path.span,
                TypeErrorKind::TypeMismatch,
            );
        }
        args.truncate(base);
        args
    }

    pub(super) fn lower_path_type(&mut self, path: &PathExpr, generic_scope: &[String]) -> Type {
        if path.segments.len() == 1 {
            let name = &path.segments[0];
            // Built-in `Array[T, N]` — fixed-size array with const-generic size
            if name == "Array" {
                if let Some(ty) = self.lower_array_type(&path.generic_args, generic_scope) {
                    return ty;
                }
            }
            // Built-in `Vector[T: Numeric, const N: i64]` — portable-SIMD lane
            // vector (design.md § Portable SIMD). Mirrors `Array` lowering but
            // produces `Type::Vector` and enforces N > 0 + numeric element T.
            if name == "Vector" {
                if let Some(ty) =
                    self.lower_vector_type(&path.generic_args, generic_scope, &path.span)
                {
                    return ty;
                }
            }
            // Built-in `Slice[T]` — borrowed view into contiguous memory
            if name == "Slice" {
                if let Some(ty) = self.lower_slice_type(&path.generic_args, generic_scope) {
                    return ty;
                }
            }
            // Check primitives
            if let Some(prim) = self.primitive_type(name) {
                return prim;
            }
            // Check generic scope
            if generic_scope.contains(name) {
                return Type::TypeParam(name.clone());
            }
            // Check type aliases — resolve transitively so that
            // `type AdminId = UserId; type UserId = i64;` sees `AdminId`
            // as `i64` regardless of source order.
            if self.env.type_aliases.contains_key(name) {
                let mut visited: HashSet<String> = HashSet::new();
                let body = self.resolve_alias_deep(name.clone(), &mut visited);
                // Generic alias (`type Pair[T] = Vec[T]`): substitute the
                // use-site args into the body, arity-check, and enforce each
                // parameter's declared bounds. Without the substitution the
                // body's `TypeParam`s would leak unsubstituted and silently
                // unify with anything. Non-generic aliases have no
                // `type_alias_params` entry and fall straight through to the
                // transparent body.
                if let Some(params) = self.env.type_alias_params.get(name).cloned() {
                    return self.instantiate_alias(name, &params, body, path, generic_scope);
                }
                return body;
            }
            // Named type (struct/enum/import)
            let mut args =
                self.lower_generic_args_named(&path.generic_args, generic_scope, Some(name));
            // `Map[K, V, H]` / `Set[T, H]` — validate the trailing hasher
            // selector and STRIP it, so every downstream consumer keeps seeing
            // the two-ary `Map` / one-ary `Set` it always has (B-2026-08-21-6).
            args = self.take_hasher_type_arg(name, args, path);
            // Intercept stdlib Rc[T] / Arc[T] wrappers — sub-item 2 of the
            // Type::Shared/Rc/Arc representation work. Single-arg form
            // only; zero/multi-arg keeps flowing through Type::Named so
            // the existing arity diagnostics still fire from there.
            if name == "Rc" && args.len() == 1 {
                return Type::Rc(Box::new(args.into_iter().next().unwrap()));
            }
            if name == "Arc" && args.len() == 1 {
                return Type::Arc(Box::new(args.into_iter().next().unwrap()));
            }
            // Intercept shared / par structs — bare struct name `S` lowers to
            // Type::Shared(S) when `S` was declared as `shared struct S` or
            // `par struct S` (both are reference-semantics handle types; see
            // the construction-site twin in `fields.rs` and design.md § Part 5b).
            // Plain structs continue through Type::Named.
            if let Some(info) = self.env.structs.get(name) {
                if info.is_shared || info.is_par {
                    return Type::Shared(name.clone());
                }
            }
            // `#[deprecated]` slice 4 — emit deprecation warning when
            // a type-position reference resolves to a deprecated
            // struct / enum / trait. Lowering happens AFTER the
            // enclosing item's `lint_override_stack` frame is pushed
            // (the typechecker walks signatures inside `check_function`
            // / `check_impl_block` after pushing), so the cascade
            // honours `#[allow(deprecated)]` on the enclosing scope.
            if self.env.structs.contains_key(name)
                || self.env.enums.contains_key(name)
                || self.env.traits.contains_key(name)
            {
                self.check_deprecated_use_at(&path.span, name);
                self.check_unstable_use_at(&path.span, name);
            }
            Type::Named {
                name: name.clone(),
                args,
            }
        } else {
            // Two-segment path where the first segment is a type parameter:
            // `I.Item` — an associated type projection. Exactly two segments
            // required; deeper paths (`A.B.C`) are module paths, not projections.
            // Generic-associated-type form `F.Mapped[i64]` (GAT slice 4): the
            // path's `generic_args` attach to the GAT name (the parser writes
            // them on the path as a whole; for a 2-segment path that's the
            // assoc-type segment by convention). Lower them as the
            // projection's `args` so the type system retains the
            // instantiation through substitution and resolution.
            if path.segments.len() == 2 && generic_scope.contains(&path.segments[0]) {
                let assoc = path.segments[1].clone();
                let args =
                    self.lower_generic_args_named(&path.generic_args, generic_scope, Some(&assoc));
                // GAT slice 5: `receiver_args` is empty at lowering time —
                // the source-level `F.Mapped[i64]` shape has the receiver
                // as a bare type param. `substitute_type_params` populates
                // `receiver_args` once `F` is solved to a concrete generic
                // receiver like `Wrapper[String]`.
                return Type::AssocProjection {
                    param: path.segments[0].clone(),
                    assoc,
                    args,
                    receiver_args: vec![],
                };
            }
            // Multi-segment module path — use last segment as type name
            let name = path.segments.last().unwrap().clone();
            let args =
                self.lower_generic_args_named(&path.generic_args, generic_scope, Some(&name));
            Type::Named { name, args }
        }
    }

    /// Lower `Array[T, N]` to `Type::Array { element, size }`.
    /// N must be a positive integer literal (const-eval of arithmetic expressions deferred).
    /// Lower a parsed shape literal to `Type::Shape` (Phase 11 Q1).
    /// Dims: a non-negative integer literal lowers to
    /// `DimArg::Const(Literal)`; an identifier naming a generic param in
    /// scope lowers to `DimArg::Const(ConstParam)` (the param is
    /// Dim-kinded by usage — same discovery idiom as `Array[T, N]` const
    /// params); any other const expression routes through the
    /// const-expression evaluator. Arithmetic over shape params
    /// (`[A + B]`) is deferred to v1.5 (design.md § Shape-param
    /// arithmetic) and gets a focused diagnostic. `?` lowers to
    /// `DimArg::Dynamic`; `...S` lowers to `DimArg::Splice` (at most one
    /// splice per literal — the unifier needs an unambiguous split).
    pub(super) fn lower_shape_literal(
        &mut self,
        lit: &crate::ast::ShapeLit,
        generic_scope: &[String],
    ) -> Type {
        fn mentions_scope_param(expr: &Expr, scope: &[String]) -> bool {
            match &expr.kind {
                ExprKind::Identifier(n) => scope.contains(n),
                ExprKind::Binary { left, right, .. } => {
                    mentions_scope_param(left, scope) || mentions_scope_param(right, scope)
                }
                ExprKind::Unary { operand, .. } => mentions_scope_param(operand, scope),
                _ => false,
            }
        }
        let mut dims = Vec::new();
        let mut seen_splice = false;
        for dim in &lit.dims {
            match dim {
                crate::ast::ShapeDim::Const(expr) => match &expr.kind {
                    ExprKind::Integer(n, _) if *n >= 0 => {
                        dims.push(DimArg::Const(ConstArg::Literal(narrow_literal_to_i64(*n))));
                    }
                    ExprKind::Integer(n, _) => {
                        self.type_error(
                            format!("shape dim must be non-negative; got {}", n),
                            expr.span,
                            TypeErrorKind::TypeMismatch,
                        );
                        dims.push(DimArg::Dynamic);
                    }
                    // A shape dim naming a generic param — in the passed scope
                    // (the signature path's `gp`) OR an enclosing generic (a
                    // BODY annotation `let p: Tensor[f32, [D]]` inside
                    // `fn f[D](...)`, whose `generic_scope` is empty; the fn's
                    // `D` is recovered from `enclosing_bounds`). Lowered to a
                    // symbolic `ConstParam` dim rather than const-evaluated —
                    // without the enclosing-generic arm a body-annotation `D`
                    // errored "'D' is not a known const" (B-2026-07-13-5 leg B).
                    ExprKind::Identifier(name)
                        if generic_scope.contains(name) || self.is_enclosing_generic(name) =>
                    {
                        dims.push(DimArg::Const(ConstArg::ConstParam(name.clone())));
                    }
                    _ if mentions_scope_param(expr, generic_scope) => {
                        self.type_error(
                            "shape-param arithmetic (`[A + B]`, `[N * 2]`) is deferred to \
                             v1.5 — it requires the type-level const-evaluator (design.md \
                             § Shape-param arithmetic)"
                                .to_string(),
                            expr.span,
                            TypeErrorKind::TypeMismatch,
                        );
                        dims.push(DimArg::Dynamic);
                    }
                    _ => match self.eval_const_expr(expr, &Type::UInt(UIntSize::Usize)) {
                        Ok(cv) => match const_value_to_array_size(&cv) {
                            Some(n) => dims.push(DimArg::Const(ConstArg::Literal(n as i64))),
                            None => {
                                self.type_error(
                                    "shape dim const-expression must evaluate to a \
                                     non-negative integer"
                                        .to_string(),
                                    expr.span,
                                    TypeErrorKind::TypeMismatch,
                                );
                                dims.push(DimArg::Dynamic);
                            }
                        },
                        Err(e) => {
                            self.emit_const_eval_error(e);
                            dims.push(DimArg::Dynamic);
                        }
                    },
                },
                crate::ast::ShapeDim::Dynamic { .. } => dims.push(DimArg::Dynamic),
                crate::ast::ShapeDim::Splice { name, span } => {
                    if !generic_scope.contains(name) {
                        self.type_error(
                            format!(
                                "unknown shape-variadic parameter '...{}' — declare it in \
                                 the generic-param list (`[T, ...{}]`)",
                                name, name
                            ),
                            *span,
                            TypeErrorKind::TypeMismatch,
                        );
                    } else if seen_splice {
                        self.type_error(
                            "a shape literal may contain at most one `...S` splice — two \
                             variadic splices have no unambiguous dim split"
                                .to_string(),
                            *span,
                            TypeErrorKind::TypeMismatch,
                        );
                    } else {
                        seen_splice = true;
                        dims.push(DimArg::Splice(name.clone()));
                    }
                }
            }
        }
        Type::Shape(dims)
    }

    fn lower_array_type(
        &mut self,
        generic_args: &Option<Vec<GenericArg>>,
        generic_scope: &[String],
    ) -> Option<Type> {
        let args = generic_args.as_ref()?;
        if args.len() != 2 {
            return None;
        }
        let element_ty = match &args[0] {
            GenericArg::Type(t) => self.lower_type_expr(t, generic_scope),
            GenericArg::Const(_) => return None,
            GenericArg::Shape(_) => return None,
        };
        let size: ConstArg = match &args[1] {
            GenericArg::Const(expr) => match &expr.kind {
                ExprKind::Integer(n, _) if *n >= 0 => ConstArg::Literal(narrow_literal_to_i64(*n)),
                // Const generics slice 3 (fork G4): an `Identifier` whose
                // name is a const-generic param in scope flows through
                // as `ConstArg::ConstParam(name)` — the inference solver
                // will substitute when the function is monomorphized.
                ExprKind::Identifier(name) if generic_scope.contains(name) => {
                    ConstArg::ConstParam(name.clone())
                }
                // Other shapes (non-literal, non-const-param) route
                // through slice 2's const-expression evaluator. A
                // successful evaluation to a non-negative integer
                // becomes a `Literal`; eval errors emit a focused
                // diagnostic and fall back to `Literal(0)` so downstream
                // consumers don't crash.
                _ => match self.eval_const_expr(expr, &Type::UInt(UIntSize::Usize)) {
                    Ok(cv) => match const_value_to_array_size(&cv) {
                        Some(n) => ConstArg::Literal(n as i64),
                        None => {
                            self.type_error(
                                format!(
                                    "Array size const-arg must evaluate to a \
                                     non-negative integer; got {}",
                                    type_display(&const_value_type(&cv))
                                ),
                                expr.span,
                                TypeErrorKind::TypeMismatch,
                            );
                            return None;
                        }
                    },
                    Err(e) => {
                        self.emit_const_eval_error(e);
                        return None;
                    }
                },
            },
            // Parser-side carveout (const generics slice 3b): the
            // generic-arg parser routes plain `Identifier` to
            // `GenericArg::Type` (it can't disambiguate a type-param
            // ref from a const-param ref without scope info). At type
            // lowering we recover: an Identifier in scope that's
            // *not* a type-param can be treated as a `ConstParam`
            // reference for the Array size position. `lower_array_type`
            // is the only place that needs this disambiguation today
            // (other `Type::Named.args` consumers stay type-only per
            // the deferred-F carveout).
            GenericArg::Type(te) => {
                if let TypeKind::Path(p) = &te.kind {
                    if p.segments.len() == 1 && p.generic_args.is_none() {
                        let name = &p.segments[0];
                        if generic_scope.contains(name) {
                            ConstArg::ConstParam(name.clone())
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            GenericArg::Shape(_) => return None,
        };
        Some(Type::Array {
            element: Box::new(element_ty),
            size,
        })
    }

    /// Lower `Vector[T, N]` to `Type::Vector { element, lanes }`.
    ///
    /// Two structural constraints beyond `Array`'s, both enforced here until
    /// the first-class `Numeric` trait + const-arg evaluator gate land (see
    /// phase-7 line 289 sub-slices):
    ///   - `N` must be a *positive* (`> 0`) lane count — a zero-lane SIMD
    ///     vector has no native representation. (`Array` permits `N == 0`.)
    ///   - `T` must be a primitive numeric type (`i8`…`i128`, `u8`…`u128`,
    ///     `f32`/`f64`). `usize` is excluded per design.md § Portable SIMD
    ///     ("`usize` is not a permitted element type"). A non-numeric `T`
    ///     emits a focused diagnostic. 128-bit lanes ARE permitted again
    ///     (B-2026-08-19-8 stage 5) and classify as `UnsupportedElement` —
    ///     no target has a 128-bit-lane ALU, so `Vector[i128, N]` compiles to
    ///     the scalar fallback, which is design.md § Portable SIMD's own
    ///     worked example.
    pub(super) fn lower_vector_type(
        &mut self,
        generic_args: &Option<Vec<GenericArg>>,
        generic_scope: &[String],
        span: &Span,
    ) -> Option<Type> {
        let args = generic_args.as_ref()?;
        if args.len() != 2 {
            return None;
        }
        let element_ty = match &args[0] {
            GenericArg::Type(t) => self.lower_type_expr(t, generic_scope),
            GenericArg::Const(_) => return None,
            GenericArg::Shape(_) => return None,
        };
        // Element-type constraint: the element must satisfy the built-in
        // `Numeric` trait. A type-param `T` in scope is permitted — the bound
        // is re-checked at monomorphization once the const-arg is concrete.
        if !matches!(element_ty, Type::TypeParam(_))
            && !self.type_satisfies_bound(&element_ty, "Numeric")
        {
            self.type_error(
                format!(
                    "Vector element type must be a primitive numeric type \
                     (i8..i128, u8..u128, f32, f64); got {}",
                    type_display(&element_ty)
                ),
                *span,
                TypeErrorKind::TypeMismatch,
            );
            return None;
        }
        // Reuse Array's const-arg parsing shape, then reject N <= 0.
        let lanes: ConstArg = match &args[1] {
            GenericArg::Const(expr) => match &expr.kind {
                ExprKind::Integer(n, _) => {
                    if *n <= 0 {
                        self.type_error(
                            format!("Vector lane count must be positive (N > 0); got {}", n),
                            expr.span,
                            TypeErrorKind::TypeMismatch,
                        );
                        return None;
                    }
                    ConstArg::Literal(narrow_literal_to_i64(*n))
                }
                ExprKind::Identifier(name) if generic_scope.contains(name) => {
                    ConstArg::ConstParam(name.clone())
                }
                _ => match self.eval_const_expr(expr, &Type::UInt(UIntSize::Usize)) {
                    Ok(cv) => match const_value_to_array_size(&cv) {
                        Some(0) | None => {
                            self.type_error(
                                "Vector lane count must evaluate to a positive integer (N > 0)"
                                    .to_string(),
                                expr.span,
                                TypeErrorKind::TypeMismatch,
                            );
                            return None;
                        }
                        Some(n) => ConstArg::Literal(n as i64),
                    },
                    Err(e) => {
                        self.emit_const_eval_error(e);
                        return None;
                    }
                },
            },
            // Parser carveout: plain identifier routed to GenericArg::Type
            // (same disambiguation as `lower_array_type`).
            GenericArg::Type(te) => {
                if let TypeKind::Path(p) = &te.kind {
                    if p.segments.len() == 1 && p.generic_args.is_none() {
                        let name = &p.segments[0];
                        if generic_scope.contains(name) {
                            ConstArg::ConstParam(name.clone())
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            GenericArg::Shape(_) => return None,
        };
        Some(Type::Vector {
            element: Box::new(element_ty),
            lanes,
        })
    }

    /// Walk a type alias chain until reaching a non-alias type. Guards
    /// against cycles (`type A = B; type B = A;`) by tracking visited
    /// names and returning `Type::Error` on re-entry — a later diagnostic
    /// pass can surface the cycle; the important invariant here is
    /// termination.
    fn resolve_alias_deep(&self, name: String, visited: &mut HashSet<String>) -> Type {
        if !visited.insert(name.clone()) {
            return Type::Error;
        }
        let Some(ty) = self.env.type_aliases.get(&name) else {
            return Type::Named {
                name,
                args: Vec::new(),
            };
        };
        if let Type::Named {
            name: inner,
            args: _,
        } = ty
        {
            if self.env.type_aliases.contains_key(inner) {
                return self.resolve_alias_deep(inner.clone(), visited);
            }
        }
        ty.clone()
    }

    /// Instantiate a generic type alias at a use site: substitute the
    /// supplied generic args into the already-resolved `body`, arity-check
    /// the argument count against the alias's declared parameters, and
    /// enforce each parameter's trait bounds against the corresponding
    /// argument (design.md § Type Aliases / v60 item 50).
    ///
    /// Bounds on an argument that is itself a generic type parameter in
    /// scope are deferred — they re-check at monomorphization once the
    /// parameter is concrete, mirroring `lower_vector_type`'s `Numeric`
    /// rule. Substitution is what makes `Pair[i64]` actually carry `i64`:
    /// without it the body keeps `TypeParam("T")`, which unifies with
    /// anything and silently swallows real type errors.
    fn instantiate_alias(
        &mut self,
        alias_name: &str,
        params: &GenericParams,
        body: Type,
        path: &PathExpr,
        generic_scope: &[String],
    ) -> Type {
        let supplied =
            self.lower_generic_args_named(&path.generic_args, generic_scope, Some(alias_name));
        let expected = params.params.len();
        if supplied.len() != expected {
            self.type_error(
                format!(
                    "type alias '{}' expects {} type argument{}, but {} {} supplied",
                    alias_name,
                    expected,
                    if expected == 1 { "" } else { "s" },
                    supplied.len(),
                    if supplied.len() == 1 { "was" } else { "were" },
                ),
                path.span,
                TypeErrorKind::TypeMismatch,
            );
        }
        let mut subs: HashMap<String, SubstValue> = HashMap::new();
        for (param, arg) in params.params.iter().zip(supplied.iter()) {
            if !matches!(arg, Type::TypeParam(_)) {
                for bound in &param.bounds {
                    let Some(trait_name) = bound.path.last() else {
                        continue;
                    };
                    if !self.type_satisfies_bound(arg, trait_name) {
                        self.type_error(
                            format!(
                                "'{}' does not satisfy '{}' required by type alias \
                                 '{}' parameter '{}'",
                                type_display(arg),
                                trait_name,
                                alias_name,
                                param.name,
                            ),
                            path.span,
                            TypeErrorKind::TypeAliasBoundNotSatisfied,
                        );
                    }
                }
            }
            subs.insert(param.name.clone(), SubstValue::Type(arg.clone()));
        }
        substitute_type_params(&body, &subs)
    }

    /// Lower `Slice[T]` to `Type::Slice { element, mutable: false }`.
    /// The `mut Slice[T]` form is produced by the parser when it sees the
    /// `mut` modifier; path-type lowering always yields the read-only form.
    fn lower_slice_type(
        &mut self,
        generic_args: &Option<Vec<GenericArg>>,
        generic_scope: &[String],
    ) -> Option<Type> {
        let args = generic_args.as_ref()?;
        if args.len() != 1 {
            return None;
        }
        let element_ty = match &args[0] {
            GenericArg::Type(t) => self.lower_type_expr(t, generic_scope),
            GenericArg::Const(_) => return None,
            GenericArg::Shape(_) => return None,
        };
        Some(Type::Slice {
            element: Box::new(element_ty),
            mutable: false,
        })
    }

    pub(super) fn primitive_type(&self, name: &str) -> Option<Type> {
        match name {
            "i8" => Some(Type::Int(IntSize::I8)),
            "i16" => Some(Type::Int(IntSize::I16)),
            "i32" => Some(Type::Int(IntSize::I32)),
            "i64" => Some(Type::Int(IntSize::I64)),
            "i128" => Some(Type::Int(IntSize::I128)),
            "u8" => Some(Type::UInt(UIntSize::U8)),
            "u16" => Some(Type::UInt(UIntSize::U16)),
            "u32" => Some(Type::UInt(UIntSize::U32)),
            "u64" => Some(Type::UInt(UIntSize::U64)),
            "u128" => Some(Type::UInt(UIntSize::U128)),
            "usize" => Some(Type::UInt(UIntSize::Usize)),
            "isize" => Some(Type::Int(IntSize::Isize)),
            "f16" => Some(Type::Float(FloatSize::F16)),
            "bf16" => Some(Type::Float(FloatSize::BF16)),
            "f32" => Some(Type::Float(FloatSize::F32)),
            "f64" => Some(Type::Float(FloatSize::F64)),
            "bool" => Some(Type::Bool),
            "char" => Some(Type::Char),
            "String" => Some(Type::Str),
            // F32/F64/F16/Bf16 are stdlib total-order wrappers (NaN sorts last, implements Eq/Ord/Hash)
            "F32" => Some(Type::Named {
                name: "F32".to_string(),
                args: vec![],
            }),
            "F64" => Some(Type::Named {
                name: "F64".to_string(),
                args: vec![],
            }),
            "F16" => Some(Type::Named {
                name: "F16".to_string(),
                args: vec![],
            }),
            "Bf16" => Some(Type::Named {
                name: "Bf16".to_string(),
                args: vec![],
            }),
            _ => None,
        }
    }

    /// True when `name` is a generic param of the function/impl body currently
    /// being checked. `enclosing_bounds` is pre-populated with every generic
    /// param name (bounded or not — see `collect_param_bounds`), so its keys
    /// are the "names in scope" set. Used to recognize a shape-dim identifier
    /// (`Tensor[f32, [D]]`) as a bound shape param in a BODY annotation, where
    /// the passed `generic_scope` is empty (the signature path threads the full
    /// `gp` list instead). Kept to SHAPE-dim resolution only — body type-param
    /// spelling stays the bare `Named{"T"}` form downstream code relies on.
    pub(super) fn is_enclosing_generic(&self, name: &str) -> bool {
        self.enclosing_bounds.contains_key(name)
    }

    pub(super) fn generic_param_names(generics: &Option<GenericParams>) -> Vec<String> {
        generics
            .as_ref()
            .map(|g| g.params.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Positional shape-kinded flags for a generic-param list — `Some`
    /// only when at least one param is declared `...S` (Phase 11 Q1).
    /// Keeps `TypeEnv::shape_param_positions` sparse.
    pub(super) fn shape_param_positions(generics: &Option<GenericParams>) -> Option<Vec<bool>> {
        let g = generics.as_ref()?;
        if g.params.iter().any(|p| p.is_variadic_shape) {
            Some(g.params.iter().map(|p| p.is_variadic_shape).collect())
        } else {
            None
        }
    }

    /// Collect inline + where-clause trait bounds keyed by the generic param's
    /// textual name. Mirrors the resolver's per-symbol `generic_param_bounds`
    /// map but is name-keyed so callers can look up bounds for a
    /// `Type::TypeParam(name)` directly. Pure AST walk — no symbol-table
    /// lookup needed.
    pub(super) fn collect_param_bounds(
        generics: &Option<GenericParams>,
        where_clause: &Option<WhereClause>,
    ) -> HashMap<String, Vec<crate::ast::TraitBound>> {
        let mut map: HashMap<String, Vec<crate::ast::TraitBound>> = HashMap::new();
        // Pre-populate with every generic param name (empty bound vec
        // when none were declared). Callers rely on `enclosing_bounds`
        // doubling as the "names in scope" set — sub-step 2a's
        // unsolved-T diagnostic uses `keys()` to skip type params that
        // belong to an enclosing function/impl. Pre-2a callers used
        // `.get(name)?` and short-circuited on absence; with always-
        // present entries they get `Some(vec![])` and proceed to find
        // no matching trait-bound candidates — same final outcome.
        if let Some(ref gp) = generics {
            for param in &gp.params {
                let entry = map.entry(param.name.clone()).or_default();
                entry.extend(param.bounds.iter().cloned());
            }
        }
        if let Some(ref wc) = where_clause {
            for c in &wc.constraints {
                if let WhereConstraint::TypeBound {
                    type_name, bounds, ..
                } = c
                {
                    map.entry(type_name.clone())
                        .or_default()
                        .extend(bounds.iter().cloned());
                }
            }
        }
        map
    }

    /// Walk `ty` and resolve any `AssocProjection { param, assoc, args,
    /// receiver_args }` nodes whose `param` (after substitution it holds
    /// the concrete receiver's bare type name) has an entry in
    /// `impl_assoc_types`. This is called after `substitute_type_params`
    /// so that `T.Item` first gets its `T` replaced by the concrete
    /// receiver (base name in `param`, args in `receiver_args`), then
    /// gets resolved to the actual associated type.
    ///
    /// GAT slice 5 — two-sided substitution: when the lookup hits, the
    /// resolver builds a substitution map from BOTH
    /// (a) the struct's `generic_params` zipped with the projection's
    ///     `receiver_args` — substituting impl-side TypeParams in the
    ///     template (e.g., the `T` in `Pair[T, U]` for an impl
    ///     `impl[T] Functor for Wrapper[T] { type Mapped[U] = Pair[T, U] }`),
    /// AND
    /// (b) the entry's `gat_params` zipped with the projection's own `args` —
    ///     substituting GAT-side TypeParams (the `U` in the same example).
    /// The substitution runs once on the template, yielding the concrete
    /// resolved type. If no entry is found, the projection is reconstructed
    /// with the args/receiver_args recursively resolved so the consumer
    /// sees a well-formed projection node.
    pub(super) fn resolve_assoc_projections(&self, ty: &Type) -> Type {
        match ty {
            Type::AssocProjection {
                param,
                assoc,
                args,
                receiver_args,
            } => {
                let resolved_args: Vec<Type> = args
                    .iter()
                    .map(|a| self.resolve_assoc_projections(a))
                    .collect();
                let resolved_recv_args: Vec<Type> = receiver_args
                    .iter()
                    .map(|a| self.resolve_assoc_projections(a))
                    .collect();
                if let Some(entry) = self
                    .env
                    .impl_assoc_types
                    .get(&(param.clone(), assoc.clone()))
                {
                    // Build the two-sided substitution map. Impl-side
                    // params come from the struct's `generic_params`
                    // paired with the projection's `receiver_args` —
                    // mirroring the `element_type_of` substitution
                    // pattern. GAT-side params come from the entry's
                    // own `gat_params` list paired with the
                    // projection's `args`. If the lengths don't match
                    // (arity mismatch — should be caught upstream as
                    // an error), zip truncates silently and the rest
                    // of the template is left with unsolved TypeParams,
                    // which downstream typechecking will surface.
                    let mut subs: HashMap<String, SubstValue> = HashMap::new();
                    if let Some(info) = self.env.structs.get(param) {
                        for (p, a) in info.generic_params.iter().zip(resolved_recv_args.iter()) {
                            subs.insert(p.clone(), SubstValue::Type(a.clone()));
                        }
                    }
                    for (p, a) in entry.gat_params.iter().zip(resolved_args.iter()) {
                        subs.insert(p.clone(), SubstValue::Type(a.clone()));
                    }
                    let substituted = substitute_type_params(&entry.ty, &subs);
                    // Recursively resolve in case the substituted type
                    // contains further projections (e.g., when the
                    // template's RHS itself references an assoc type).
                    self.resolve_assoc_projections(&substituted)
                } else {
                    Type::AssocProjection {
                        param: param.clone(),
                        assoc: assoc.clone(),
                        args: resolved_args,
                        receiver_args: resolved_recv_args,
                    }
                }
            }
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| self.resolve_assoc_projections(e))
                    .collect(),
            ),
            Type::Array { element, size } => Type::Array {
                element: Box::new(self.resolve_assoc_projections(element)),
                size: size.clone(),
            },
            Type::Slice { element, mutable } => Type::Slice {
                element: Box::new(self.resolve_assoc_projections(element)),
                mutable: *mutable,
            },
            Type::Ref(inner) => Type::Ref(Box::new(self.resolve_assoc_projections(inner))),
            Type::MutRef(inner) => Type::MutRef(Box::new(self.resolve_assoc_projections(inner))),
            Type::Weak(inner) => Type::Weak(Box::new(self.resolve_assoc_projections(inner))),
            Type::Pointer { is_mut, inner } => Type::Pointer {
                is_mut: *is_mut,
                inner: Box::new(self.resolve_assoc_projections(inner)),
            },
            Type::Named { name, args } => Type::Named {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|a| self.resolve_assoc_projections(a))
                    .collect(),
            },
            Type::Function {
                params,
                return_type,
            } => Type::Function {
                params: params
                    .iter()
                    .map(|p| self.resolve_assoc_projections(p))
                    .collect(),
                return_type: Box::new(self.resolve_assoc_projections(return_type)),
            },
            Type::OnceFunction {
                params,
                return_type,
            } => Type::OnceFunction {
                params: params
                    .iter()
                    .map(|p| self.resolve_assoc_projections(p))
                    .collect(),
                return_type: Box::new(self.resolve_assoc_projections(return_type)),
            },
            _ => ty.clone(),
        }
    }

    /// Return the element type produced when iterating over `ty`.
    ///
    /// For built-in collection types this consults `impl_assoc_types` keyed by
    /// `(type_name, "Item")`, then substitutes any `TypeParam` placeholders
    /// using the struct's declared `generic_params` paired with the concrete
    /// type arguments from `ty`. Falls back to `ty` itself for unknown types
    /// so the rest of the type checker can proceed without a hard error.
    pub(super) fn element_type_of(&self, ty: &Type) -> Type {
        // Strings iterate as `char` BY VALUE — a `char` is a Copy scalar decoded
        // from the UTF-8 storage, not a borrow into it — so `for c in s` and
        // `for c in (ref/mut ref String)` all bind `c: char`, never `String` /
        // `ref String`. Peel any borrow first so the borrowed forms don't fall to
        // the `Ref`/`MutRef` arms below (which would re-wrap to `ref char`).
        // Without this, `String`/`Str` fell through to the `_ => ty.clone()` tail
        // and `for c in s` mistyped `c` as the whole `String` — a typechecker/
        // codegen MISMATCH, since `compile_for_string_chars` already binds the
        // codepoint as `char` (warning under `karac run`, hard error under
        // `karac build` the moment `c` is used as a char). B-2026-06-18-2.
        {
            let mut base = ty;
            while let Type::Ref(inner) | Type::MutRef(inner) = base {
                base = inner;
            }
            if matches!(base, Type::Str) {
                return Type::Char;
            }
        }
        match ty {
            // Borrowed collections (`for w in (ref Vec[T])` / `mut ref`):
            // iterating a borrow yields *borrowed* elements (design.md's
            // iteration table: `Vec[T].iter()` Item is `ref T`). Recurse to
            // the collection's element type, then re-wrap in the same borrow
            // form so the loop variable is `ref T` / `mut ref T`, not owned
            // `T`. Two failures this prevents:
            //   * Mistyping: without unwrapping, `for w in words` over a
            //     `ref Vec[String]` param binds `w` to the whole
            //     `ref Vec<String>`, so element-as-String uses (`map.get(w)`,
            //     `w.clone()`) fail (warning under `karac run`, hard error
            //     under `karac build`).
            //   * Unsoundness: unwrapping to owned `T` would let `out.push(w)`
            //     move an element *out of a borrowed collection*. Keeping the
            //     borrow form rejects the move (and forces an explicit
            //     `.clone()`), matching the move-out-of-borrow rule.
            Type::Ref(inner) => {
                let elem = self.element_type_of(inner);
                // A borrowed Copy *scalar* element (`for x in (ref Vec[i64])`,
                // and the inner loop of a nested `ref Vec[Vec[i64]]`) binds BY
                // VALUE as the bare scalar, NOT `ref i64`. Copying a scalar out
                // of a shared borrow is not a move, so it is sound, it keeps the
                // element usable in arithmetic (`total + x`), and it matches
                // both `Slice[i64]` iteration (yields `i64`) and the `Str` →
                // `char` peel above. Without this the inner element was `ref
                // i64`, which arithmetic does not auto-deref — `total + x`
                // warned under `karac run` but HARD-errored under `karac build`,
                // a run/build divergence. Aggregates (`ref Vec[i64]`, `ref
                // String`, structs) keep the borrow form so the move-out-of-
                // borrow rejection above still fires. The `mut ref` arm below
                // unwraps scalars the SAME way (B-2026-06-30-6): bare `for` is
                // read-only shared iteration regardless of the collection's own
                // borrow form (design.md line 2739 — `for x in c` desugars to
                // `c.iter()`), so a `mut ref Vec[i64]` element is a by-value
                // `i64` too, never `mut ref i64`. B-2026-06-30-4.
                if is_borrow_copy_scalar(&elem) {
                    elem
                } else {
                    Type::Ref(Box::new(elem))
                }
            }
            Type::MutRef(inner) => {
                // Bare `for` over a `mut ref` collection is STILL shared /
                // read-only iteration: design.md § Iteration (line 2739) —
                // `for x in collection` desugars to `collection.iter()`, which
                // borrows and yields `ref T`, REGARDLESS of the collection's own
                // borrow form (line 2758 reaffirms: "bare `for` calls `.iter()`
                // (which borrows)"). Mutable iteration is the explicit
                // `.iter_mut()` path (`for x in xs.iter_mut()` → `mut ref T`),
                // not bare `for`. So a `mut ref` collection's bare-`for` element
                // is treated exactly like the shared `Type::Ref` arm above: a
                // Copy scalar binds BY VALUE (`for x in (mut ref Vec[i64])` →
                // `x: i64`), keeping it usable in arithmetic. Without this the
                // element was `mut ref i64`, which arithmetic does not auto-
                // deref: `x * 2` warned under `karac run` but HARD-errored under
                // `karac build` — a run/build divergence (the mutable-borrow
                // sibling of B-2026-06-30-4). Binding by value also keeps the
                // loop var's type HONEST: it is a read-only shared element, so a
                // `x = x * 2` write is a plain local reassignment that does NOT
                // reach the collection (bare `for` never mutates — use
                // `xs[i] = ...` over `0..xs.len()`, or the future `.iter_mut()`),
                // and is consistent across run / check / build. Aggregates
                // (`mut ref Vec`, `mut ref String`, structs) keep the borrow
                // form so the move-out-of-borrow rejection is preserved.
                // B-2026-06-30-6.
                let elem = self.element_type_of(inner);
                if is_borrow_copy_scalar(&elem) {
                    elem
                } else {
                    Type::MutRef(Box::new(elem))
                }
            }
            // A refinement type (`type NonEmpty[T] = Vec[T] where
            // self.len() > 0`) is iterated as its BASE. The nominal wrapper
            // exists to gate *construction* (design.md § Refinement Types),
            // not to hide the base's read operations — a refinement declares
            // no `Item` binding of its own, and one over a non-collection
            // base still has no `Item`, so projecting is never wrong here.
            // Without this arm the wrapper fell to the `_ => ty.clone()`
            // tail and `for r in rows` bound `r` to the whole `NonEmpty`,
            // so `r.price` failed with `no field 'price' on type
            // 'NonEmpty'` (B-2026-07-29-26). The borrowed forms reach this
            // arm through the `Ref` / `MutRef` recursion above, so their
            // element keeps the borrow wrapper as usual.
            Type::Refinement { base, .. } => self.element_type_of(base),
            // Primitive borrowed views — element type is the inner type.
            Type::Array { element, .. } | Type::Slice { element, .. } => *element.clone(),
            Type::Named { name, args } => {
                // Look up the "Item" associated type for this collection.
                let Some(entry) = self
                    .env
                    .impl_assoc_types
                    .get(&(name.clone(), "Item".to_string()))
                else {
                    return ty.clone();
                };
                // Build substitution from generic_params → concrete args.
                // Range types store the bound type at args[0] under param "T".
                let generic_params: &[String] = self
                    .env
                    .structs
                    .get(name)
                    .map(|s| s.generic_params.as_slice())
                    .unwrap_or(&[]);
                let subs: HashMap<String, SubstValue> = generic_params
                    .iter()
                    .zip(args.iter())
                    .map(|(p, a)| (p.clone(), SubstValue::Type(a.clone())))
                    .collect();
                substitute_type_params(&entry.ty, &subs)
            }
            _ => ty.clone(),
        }
    }
}

/// True for the Copy scalar primitives that bind BY VALUE when iterated out of
/// a borrowed collection — shared `for x in (ref Vec[T])` (B-2026-06-30-4) or
/// mutable `for x in (mut ref Vec[T])` (B-2026-06-30-6). Bare `for` borrows via
/// `.iter()` in both cases (design.md line 2739), so copying a scalar out is
/// not a move: yielding the bare `T` (rather than `ref T` / `mut ref T`) is
/// sound and keeps the loop variable usable in arithmetic — matching `Slice[T]`
/// iteration and the `Str` → `char` peel in `element_type_of`. Aggregates
/// (`Vec`/`String`/struct/enum) are deliberately excluded so their borrow form
/// is preserved and moving an element out of a borrowed collection stays
/// rejected. `Str` is absent because borrowed `String` iteration is already
/// peeled to `char` before the borrow arms run.
fn is_borrow_copy_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
    )
}
