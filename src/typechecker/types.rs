//! Leaf type definitions for the typechecker.
//!
//! Holds the `Type` enum and its primitive-size sub-enums
//! (`IntSize`, `UIntSize`, `FloatSize`), the `TypeVarId` / `ConstVarId`
//! metavariable ids, the `ConstArg` / `SubstValue` payloads consumed by
//! the inference solver, the `ScrutineeMode` match-arm classifier, and
//! the `VariantTypeInfo` descriptor used by exhaustiveness and ownership.
//!
//! Pure type predicates and display helpers will land here in
//! subsequent extractions; for now they remain alongside the
//! `TypeChecker` impl in `super`.

// ── Internal Type Representation ────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Int(IntSize),
    UInt(UIntSize),
    Float(FloatSize),
    Bool,
    Char,
    Str,
    Unit,
    Never,

    Tuple(Vec<Type>),
    Array {
        element: Box<Type>,
        /// Const generics slice 3 (fork G4): `size` widened from `usize`
        /// to `ConstArg` so the Type carries the const-arg shape through
        /// inference. Literal sizes flow through `ConstArg::Literal(n)`;
        /// const-param references flow through `ConstArg::ConstParam(name)`;
        /// the call-site solver mints `ConstArg::ConstVar(id)` and resolves
        /// to one of the other variants. Codegen and interpreter consumers
        /// reach for the literal via `ConstArg::as_literal`.
        size: ConstArg,
    },
    /// `Vector[T: Numeric, const N: i64]` — the portable-SIMD lane vector
    /// (design.md § Portable SIMD — `Vector[T, N]`). Distinct from
    /// `Type::Array` because the two have different physical layout and
    /// operation surfaces: `Array` lowers to LLVM `[N x T]` (aggregate),
    /// `Vector` lowers to `<N x T>` (`repr(simd)`) and supports element-wise
    /// arithmetic / lane access that arrays do not. `element` is restricted
    /// to primitive numeric `T` (enforced structurally at lowering until the
    /// first-class `Numeric` trait lands — see phase-7 line 289 sub-slices);
    /// `lanes` carries the const-arg shape exactly like `Array::size`.
    Vector {
        element: Box<Type>,
        lanes: ConstArg,
    },
    Slice {
        element: Box<Type>,
        mutable: bool,
    },

    /// A user-defined struct or enum, referenced by name.
    Named {
        name: String,
        args: Vec<Type>,
    },

    /// A `shared struct S { ... }` value type — RC-tracked struct with
    /// reference semantics. Carries the struct name only; shared structs
    /// are non-generic at v1 (no `shared struct S[T]`) per design.md
    /// § Part 5: Shared Types. Distinct from `Type::Named { name: "S" }`
    /// so consumers can match shared-ness off the type directly without
    /// consulting `StructDef.is_shared` in the item table.
    Shared(String),

    /// `Rc[T]` — explicit reference-counted wrapper, single-task only.
    /// Not assignable to `Arc[T]`; the `Rc → Arc` migration story is
    /// manual (per design.md § RC integration). The auto-promotion in
    /// `OwnershipChecker::promote_rc_to_arc` rewrites the value site,
    /// not the type, so the typechecker compat rule and the
    /// ownership-checker's promotion are orthogonal.
    Rc(Box<Type>),

    /// `Arc[T]` — atomically-reference-counted wrapper, cross-task safe.
    /// Not assignable to `Rc[T]`; see `Type::Rc` for the migration note.
    Arc(Box<Type>),

    Function {
        params: Vec<Type>,
        return_type: Box<Type>,
    },

    /// A once-callable closure type: a closure that consumes a captured
    /// owned non-Copy value and therefore can only be invoked one time.
    /// Distinct from `Function` because `OnceFunction` cannot substitute
    /// into a `Function` slot (or a `ref Function` slot) — the slot would
    /// permit multiple invocations, which the once-callable contract
    /// forbids. Identity-compatible with itself only at this stage; later
    /// rounds may add a `Function ⇒ OnceFunction` widening at slot
    /// boundaries.
    OnceFunction {
        params: Vec<Type>,
        return_type: Box<Type>,
    },

    Ref(Box<Type>),
    MutRef(Box<Type>),
    Weak(Box<Type>),
    Pointer {
        is_mut: bool,
        inner: Box<Type>,
    },

    TypeParam(String),
    TypeVar(TypeVarId),

    /// A Shape-kinded generic argument — the lowered form of a shape
    /// literal `[3, 4, ?]` (or a bound shape-variadic param's dim list).
    /// Lives inside `Named.args` next to ordinary type args, so the
    /// existing recursion over generic args (substitution, unification,
    /// compatibility, display) reaches it without new plumbing. See
    /// design.md § Numerical Types > "Shape is a new generic-parameter
    /// kind".
    Shape(Vec<DimArg>),

    /// `T.Item` — an associated type projection. `param` is the generic type
    /// parameter name (e.g. `"I"`); after `substitute_type_params` has run
    /// it carries the resolved receiver's bare type name (e.g. `"Wrapper"`
    /// for a `Wrapper[String]` receiver). `assoc` is the associated type
    /// name (e.g. `"Item"`). `args` carries the projection's own type
    /// arguments for a generic associated type (GAT) like `F.Mapped[i64]`
    /// (slice 4 of the GAT epic) — empty for the non-generic form `F.Item`.
    /// `receiver_args` carries the resolved receiver's own type arguments
    /// (e.g. `[String]` for `Wrapper[String]`) once `substitute_type_params`
    /// has solved the receiver — empty pre-substitution and for receivers
    /// that aren't generic (slice 5 addition). The split between `param`
    /// (base name) and `receiver_args` (the receiver's args) is what lets
    /// `resolve_assoc_projections` look up the impl entry by bare name AND
    /// substitute the impl-block's generic params, while the projection's
    /// own `args` substitute the GAT params. Slice 5 wires the two-sided
    /// substitution.
    AssocProjection {
        param: String,
        assoc: String,
        args: Vec<Type>,
        receiver_args: Vec<Type>,
    },

    /// Return-position `impl Trait` — an opaque existential whose witness
    /// type is computed from the function body (slice 3 of the `impl Trait`
    /// epic; see `phase-5-diagnostics.md` line 397 and `design.md § `impl
    /// Trait` (Existential Types)`).
    ///
    /// Caller-side opacity: a value of `Type::Existential` is callable only
    /// through methods declared on `trait_name`; the typechecker rejects
    /// attempts to assign it into a slot expecting the witness's concrete
    /// type, to pattern-match past the trait surface, or otherwise to use
    /// it in a position that requires the concrete representation.
    ///
    /// Callee-side check: the function body's tail-expr type must implement
    /// `trait_name`; failure emits `error[E_IMPL_TRAIT_MISSING_BOUND]`.
    ///
    /// Fields:
    /// - `trait_name` — joined trait path (`Iterator`, `std.iter.Iterator`).
    /// - `trait_args` — positional generic args on the trait. Associated-
    ///   type bindings (`Iterator[Item = i64]`) are not yet parseable so
    ///   only positional args reach this field.
    /// - `origin` — `SpanKey` of the `TypeKind::ImplTrait` AST node. Two
    ///   distinct `impl Iterator` declarations (e.g., on two different
    ///   functions) yield distinct origins so the typechecker keeps their
    ///   witnesses separate even when their bounds match structurally.
    /// - `tait_alias` — `Some(alias_name)` when this existential was minted
    ///   from a `type X = impl Trait;` declaration (Type Alias `impl
    ///   Trait`, TAIT — slice 6 of the `impl Trait` epic). The TAIT
    ///   marker drives the slice-6 `E_TAIT_NOT_IMPLEMENTED_YET`
    ///   diagnostic at witness-required use sites (e.g., method calls
    ///   for methods not declared on the trait but defined on the
    ///   witness type). `None` for return-position existentials
    ///   (slice 3) — those have no alias name and their witness is
    ///   computed from the defining function's body, not via TAIT
    ///   witness-inference.
    Existential {
        trait_name: String,
        trait_args: Vec<Type>,
        origin: crate::resolver::SpanKey,
        tait_alias: Option<String>,
    },

    /// A refinement type `type Name = Base where <pred>` — a base type
    /// carrying a distinct *nominal* identity plus a value predicate
    /// (design.md § Refinement Types; phase-9-verification step 1).
    ///
    /// The predicate itself is **not** embedded here — it lives in
    /// `TypeEnv.refinement_predicates` keyed by `name`, so `Type` never
    /// holds an `Expr` and stays cheap to `Clone` / `PartialEq`. The
    /// `base` is embedded inline (rather than looked up) so the pure
    /// `types_compatible` / `strip_refinement` helpers can normalize a
    /// refinement to its base without a `&TypeChecker` / env handle.
    ///
    /// Equality is **nominal**: two refinements are equal iff their
    /// `name` *and* `base` match (the derived `PartialEq`). Distinct
    /// refinements over the same base (`Positive` vs `NonZero`, both
    /// `i64`) are unequal and must widen to the shared base to compare —
    /// see `types_compatible` and design.md § LUB rule 4.
    Refinement {
        name: String,
        base: Box<Type>,
    },

    Error,
}

/// Normalize a refinement type to its underlying base, leaving every
/// other `Type` untouched. Pure (no `&TypeChecker`) so it can be called
/// from `types_compatible`, `unify_types`, the binary-op synthesis arm,
/// and the ~80 other `Type` match sites that treat a refinement
/// transparently. A single unwrap is sufficient — `Type::Refinement`'s
/// `base` is never itself a refinement (the env-build lowering wraps a
/// fully-resolved base exactly once), but the loop costs nothing and
/// keeps the helper correct if that invariant ever loosens.
pub(super) fn strip_refinement(ty: &Type) -> &Type {
    let mut cur = ty;
    while let Type::Refinement { base, .. } = cur {
        cur = base;
    }
    cur
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntSize {
    I8,
    I16,
    I32,
    I64,
    I128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UIntSize {
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatSize {
    F32,
    F64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVarId(pub u32);

/// Const-arg metavariable id (const generics slice 3 — fork G1). Mirrors
/// `TypeVarId` but for const-arg unification at call sites. Minted by
/// `instantiate_signature_with_fresh_vars` per unique const-param name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConstVarId(pub u32);

/// Const-generic argument carried inside a `Type` (currently `Type::Array.size`;
/// slice 3 ships this surface for `Type::Array` only — `Type::Named.args`
/// const-arg representation is the deferred-F carve-out per phase-5).
///
/// At type lowering time the parser's `GenericArg::Const(Expr)` payload
/// becomes either `Literal(i64)` (when the const-arg is a literal /
/// fold-through-evaluator) or `ConstParam(name)` (when the const-arg
/// references a const-generic param in scope). The inference solver
/// substitutes `ConstParam → ConstVar(id)` at signature instantiation;
/// `resolve_type_vars` walks back through the const_substitutions map
/// to swap `ConstVar` for its bound value.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstArg {
    Literal(i64),
    ConstParam(String),
    ConstVar(ConstVarId),
    /// A dim metavariable bound from a `?` dynamic dim at a call site
    /// (Phase 11 Q1). A *weak* binding — mirrors the Never-as-bottom
    /// rule for type metavars: a concrete sibling constraint upgrades
    /// it, and it never demotes a concrete binding. Resolves to
    /// `DimArg::Dynamic` in shape positions (design.md § Dynamic-dim
    /// unification); never constructed in `Array`/`Vector` size
    /// positions.
    DynamicDim,
}

/// One dim of a `Type::Shape`. Dim params are integer-valued at compile
/// time, exactly like const params — so the `Const` case reuses the
/// whole `ConstArg` machinery (param naming, call-site `ConstVar`
/// minting, `unify_const_args` binding, `resolve_const_arg`
/// resolution). The extra variants are shape-specific:
/// `Dynamic` is the `?` marker (unifies with any dim and degrades the
/// result position to `?` — design.md § Dynamic-dim unification);
/// `Splice`/`SpliceVar` carry a `...S` variadic splice through
/// declaration and call-site instantiation respectively.
#[derive(Debug, Clone, PartialEq)]
pub enum DimArg {
    Const(ConstArg),
    Dynamic,
    /// `...S` in a declared shape literal, pre-instantiation.
    Splice(String),
    /// `...S` after call-site instantiation: the splice binds the
    /// matched dim sub-list as `Type::Shape` into the ordinary type
    /// substitution map under this fresh `TypeVarId`.
    SpliceVar(TypeVarId),
}

impl ConstArg {
    /// Extract the integer literal value if this is a `Literal` variant.
    /// Returns `None` for `ConstParam` / `ConstVar` (which carry
    /// unresolved or symbolic references). Used by codegen / interpreter
    /// / typechecker consumers that care about concrete sizes.
    pub fn as_literal(&self) -> Option<i64> {
        match self {
            ConstArg::Literal(n) => Some(*n),
            _ => None,
        }
    }

    /// Extract the integer literal value as a `usize`. Returns `None`
    /// for negative literals or non-literal variants. Used at places
    /// where the legacy `Type::Array.size: usize` representation was
    /// consumed directly (length checks, repeat-literal arity checks).
    pub fn as_usize(&self) -> Option<usize> {
        match self {
            ConstArg::Literal(n) if *n >= 0 => usize::try_from(*n).ok(),
            _ => None,
        }
    }
}

/// Borrow form of a match scrutinee, captured once at match entry and
/// propagated transitively into every sub-pattern via
/// `check_pattern_against`. The classifier strips a single outer
/// `ref` / `mut ref` so variant / struct / tuple dispatch keeps matching
/// the unwrapped shape; each leaf binding's type is then re-wrapped
/// through `wrap_binding_ty` so the bindings carry the borrow back —
/// design.md § Match Arm Binding Modes.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(crate) enum ScrutineeMode {
    Owned,
    Ref,
    MutRef,
}

impl ScrutineeMode {
    /// Classify `scrut_ty` into `(mode, dispatch_ty)`. A single layer of
    /// `Type::Ref` / `Type::MutRef` is stripped — deeper nesting is rare
    /// (Kāra has no `&&T`-style syntax; `ref ref T` only arises from
    /// generic instantiation) and the outer-most borrow already governs
    /// what arm bindings may do, so the simpler shallow strip suffices.
    pub(crate) fn classify(scrut_ty: &Type) -> (Self, &Type) {
        match scrut_ty {
            Type::Ref(inner) => (ScrutineeMode::Ref, inner.as_ref()),
            Type::MutRef(inner) => (ScrutineeMode::MutRef, inner.as_ref()),
            _ => (ScrutineeMode::Owned, scrut_ty),
        }
    }

    /// Wrap a leaf binding's declared type with the appropriate borrow
    /// form for this mode. `Owned` is the identity. Types that are
    /// **already** a borrow shape (`Ref`, `MutRef`, `Slice` /
    /// `mut Slice`) are left alone so a `ref Foo` field through a
    /// `ref Container` scrutinee stays `ref Foo` instead of becoming
    /// `ref ref Foo`, and a struct field declared `Slice[T]`
    /// (immutable) is not silently elevated to `mut Slice[T]` by a
    /// `mut ref Container` scrutinee — the field's declared mutability
    /// is its own ceiling. Mutability propagation for **slice rest
    /// bindings** (Array[T, K] / Slice[T] freshly synthesized from a
    /// `mut ref Vec[T]` scrutinee per design.md § Slice patterns —
    /// Mutability propagation) is handled at the Slice arm of
    /// `check_pattern_against`, not here.
    pub(crate) fn wrap_binding_ty(self, ty: Type) -> Type {
        match self {
            ScrutineeMode::Owned => ty,
            ScrutineeMode::Ref => match ty {
                Type::Ref(_) | Type::MutRef(_) | Type::Slice { .. } => ty,
                _ => Type::Ref(Box::new(ty)),
            },
            ScrutineeMode::MutRef => match ty {
                Type::Ref(_) | Type::MutRef(_) | Type::Slice { .. } => ty,
                _ => Type::MutRef(Box::new(ty)),
            },
        }
    }
}

/// User-facing rendering of a `ConstArg` for diagnostic messages.
/// `Literal(4)` → `"4"`; `ConstParam(N)` → `"N"`; `ConstVar(id)` →
/// `"?C{id}"` (parallel to `?M{id}` for type metavars).
pub fn const_arg_display(arg: &ConstArg) -> String {
    match arg {
        ConstArg::Literal(n) => n.to_string(),
        ConstArg::ConstParam(name) => name.clone(),
        ConstArg::ConstVar(id) => format!("?C{}", id.0),
        ConstArg::DynamicDim => "?".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VariantTypeInfo {
    Unit,
    Tuple(Vec<Type>),
    Struct(Vec<(String, Type)>),
}

/// Generic-substitution payload (F1 — Const generics slice 1). A single
/// `HashMap<String, SubstValue>` carries both type and const substitutions
/// so the typechecker's structural substitution and the call-site solver
/// can flow type-args and literal const-args through the same context.
/// Slice 2's const-expression evaluator and slice 4's codegen consume the
/// `Const` variant; slice 1 binds only literal const-args via step (e).
#[derive(Debug, Clone)]
pub enum SubstValue {
    Type(Type),
    Const(crate::prelude::ConstValue),
}

impl SubstValue {
    /// Extract the inner `Type` if this is a `Type` substitution; `None`
    /// for `Const`. Used by structural type substitution paths that should
    /// ignore const-arg bindings (e.g. `substitute_type_params` walking a
    /// `Type::TypeParam(name)`).
    pub fn as_type(&self) -> Option<&Type> {
        match self {
            SubstValue::Type(t) => Some(t),
            SubstValue::Const(_) => None,
        }
    }
}

// ── Type Operations ─────────────────────────────────────────────

// ── Type Display ────────────────────────────────────────────────

/// Reduce a `Type` to a single textual head name suitable for runtime
/// dispatch — the concrete struct/enum name for `Type::Named`, the textual
/// name for `Type::TypeParam` (caller will resolve it transitively against
/// the runtime substitution stack), or one of the primitive lowercase names
/// (`"i32"`, `"bool"`, ...). Returns `None` for compound shapes (tuples,
/// arrays, references, function values) — those don't dispatch through
/// `Type.method` impl entries.
pub fn type_to_concrete_or_param_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Named { name, .. } => Some(name.clone()),
        Type::TypeParam(name) => Some(name.clone()),
        Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char | Type::Str => {
            Some(type_display(ty))
        }
        _ => None,
    }
}

/// Head name + type-argument vector suitable for `env.impls` lookup.
/// Primitives are keyed under their stringified name (`"i32"`, `"f64"`,
/// `"bool"`, …) by `register_stdlib_impls` with empty args. Named types
/// return their nominal head name and the recursive argument list.
/// Returns `None` for type variables, function types, slices, tuples,
/// etc. — none of which can satisfy a nominal trait bound today. Strips
/// outer `ref` / `mut ref` so a borrowed receiver discharges against
/// the same impls as its inner type.
pub(super) fn impl_table_key(ty: &Type) -> Option<(String, Vec<Type>)> {
    match ty {
        Type::Int(s) => Some((
            match s {
                IntSize::I8 => "i8",
                IntSize::I16 => "i16",
                IntSize::I32 => "i32",
                IntSize::I64 => "i64",
                IntSize::I128 => "i128",
            }
            .to_string(),
            Vec::new(),
        )),
        Type::UInt(s) => Some((
            match s {
                UIntSize::U8 => "u8",
                UIntSize::U16 => "u16",
                UIntSize::U32 => "u32",
                UIntSize::U64 => "u64",
                UIntSize::U128 => "u128",
                UIntSize::Usize => "usize",
            }
            .to_string(),
            Vec::new(),
        )),
        Type::Float(s) => Some((
            match s {
                FloatSize::F32 => "f32",
                FloatSize::F64 => "f64",
            }
            .to_string(),
            Vec::new(),
        )),
        Type::Bool => Some(("bool".to_string(), Vec::new())),
        Type::Char => Some(("char".to_string(), Vec::new())),
        Type::Str => Some(("String".to_string(), Vec::new())),
        Type::Named { name, args } => Some((name.clone(), args.clone())),
        // A refinement keys its own inherent / trait impls under its
        // nominal name (`impl Positive { ... }`), distinct from the base's
        // impls. Method resolution consults this first, then the base
        // (phase-9 step 2, §1C). Non-generic at v1, so no args.
        Type::Refinement { name, .. } => Some((name.clone(), Vec::new())),
        Type::Ref(inner) | Type::MutRef(inner) => impl_table_key(inner),
        _ => None,
    }
}

/// Match rule for the Theme-4 impl-table key shape: a stored impl's
/// `target_args` matches a call-site args vector iff the stored args
/// are empty (impl is generic-on-name and applies to any instantiation)
/// OR the two args vectors are equal. Length mismatch when the stored
/// args are non-empty is a non-match.
pub(super) fn impl_args_match(stored: &[Type], call_site: &[Type]) -> bool {
    stored.is_empty() || stored == call_site
}

/// Strip the outer wrapper from a method-call receiver type to surface
/// the named receiver for impl-table lookup. Per design.md § Method
/// Resolution Step 1, the autoref candidates `T`, `ref T`, `mut ref T`
/// collapse to the same name lookup. Sub-item 3a of the
/// `Type::Shared` / `Type::Rc` / `Type::Arc` representation work
/// extends this with three more wrappers — shared structs lower their
/// outer `Type::Shared(name)` to `Type::Named { name, args: [] }`
/// (matches the user-defined-struct lookup path verbatim);
/// `Rc(inner)` / `Arc(inner)` deref to the inner type so the
/// wrapped-type's methods become reachable.
pub(super) fn receiver_for_method_lookup(obj_ty: &Type) -> Type {
    match obj_ty {
        Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
        Type::Shared(name) => Type::Named {
            name: name.clone(),
            args: vec![],
        },
        Type::Rc(inner) | Type::Arc(inner) => (**inner).clone(),
        other => other.clone(),
    }
}

/// `true` iff every `TypeParam` / `TypeVar` / `AssocProjection` is
/// absent from the type recursively. Used by `env_add_impl` to decide
/// whether an impl's target args should be stored as specialized
/// (fully concrete → keep) or treated as generic-on-name (any
/// non-concrete piece → drop the args, store empty).
pub(super) fn type_is_fully_concrete(ty: &Type) -> bool {
    match ty {
        Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } => false,
        // `impl Trait` existentials are opaque to impl-table specialization
        // — an impl block can never name an existential as its target, so
        // treat as non-concrete to keep them out of the specialized lane.
        Type::Existential { .. } => false,
        Type::Named { args, .. } => args.iter().all(type_is_fully_concrete),
        // A shape is concrete iff no dim references an unresolved
        // param/var. `?` is concrete — it is a committed runtime dim,
        // not an unsolved metavariable.
        Type::Shape(dims) => dims
            .iter()
            .all(|d| matches!(d, DimArg::Const(ConstArg::Literal(_)) | DimArg::Dynamic)),
        // A refinement is concrete iff its base is — a generic refinement
        // (`NonEmpty[T]`) carries the type param inside `base`.
        Type::Refinement { base, .. } => type_is_fully_concrete(base),
        Type::Tuple(types) => types.iter().all(type_is_fully_concrete),
        Type::Array { element, .. } => type_is_fully_concrete(element),
        Type::Vector { element, .. } => type_is_fully_concrete(element),
        Type::Slice { element, .. } => type_is_fully_concrete(element),
        Type::Ref(inner) | Type::MutRef(inner) | Type::Weak(inner) => type_is_fully_concrete(inner),
        Type::Rc(inner) | Type::Arc(inner) => type_is_fully_concrete(inner),
        Type::Pointer { inner, .. } => type_is_fully_concrete(inner),
        Type::Function {
            params,
            return_type,
        }
        | Type::OnceFunction {
            params,
            return_type,
        } => params.iter().all(type_is_fully_concrete) && type_is_fully_concrete(return_type),
        Type::Int(_)
        | Type::UInt(_)
        | Type::Float(_)
        | Type::Bool
        | Type::Char
        | Type::Str
        | Type::Unit
        | Type::Never
        | Type::Shared(_)
        | Type::Error => true,
    }
}

pub fn type_display(ty: &Type) -> String {
    match ty {
        Type::Shape(dims) => {
            let rendered: Vec<String> = dims
                .iter()
                .map(|d| match d {
                    DimArg::Const(ConstArg::Literal(n)) => n.to_string(),
                    DimArg::Const(ConstArg::ConstParam(name)) => name.clone(),
                    DimArg::Const(ConstArg::ConstVar(_)) => "_".to_string(),
                    DimArg::Const(ConstArg::DynamicDim) => "?".to_string(),
                    DimArg::Dynamic => "?".to_string(),
                    DimArg::Splice(name) => format!("...{}", name),
                    DimArg::SpliceVar(_) => "..._".to_string(),
                })
                .collect();
            format!("[{}]", rendered.join(", "))
        }
        Type::Int(s) => match s {
            IntSize::I8 => "i8",
            IntSize::I16 => "i16",
            IntSize::I32 => "i32",
            IntSize::I64 => "i64",
            IntSize::I128 => "i128",
        }
        .to_string(),
        Type::UInt(s) => match s {
            UIntSize::U8 => "u8",
            UIntSize::U16 => "u16",
            UIntSize::U32 => "u32",
            UIntSize::U64 => "u64",
            UIntSize::U128 => "u128",
            UIntSize::Usize => "usize",
        }
        .to_string(),
        Type::Float(s) => match s {
            FloatSize::F32 => "f32",
            FloatSize::F64 => "f64",
        }
        .to_string(),
        Type::Bool => "bool".to_string(),
        Type::Char => "char".to_string(),
        Type::Str => "String".to_string(),
        Type::Unit => "()".to_string(),
        Type::Never => "!".to_string(),
        Type::Tuple(types) => {
            let inner: Vec<String> = types.iter().map(type_display).collect();
            format!("({})", inner.join(", "))
        }
        Type::Array { element, size } => format!(
            "Array[{}, {}]",
            type_display(element),
            const_arg_display(size)
        ),
        Type::Vector { element, lanes } => format!(
            "Vector[{}, {}]",
            type_display(element),
            const_arg_display(lanes)
        ),
        Type::Slice { element, mutable } => {
            if *mutable {
                format!("mut Slice[{}]", type_display(element))
            } else {
                format!("Slice[{}]", type_display(element))
            }
        }
        Type::Named { name, args } if args.is_empty() => name.clone(),
        Type::Named { name, args } => {
            let inner: Vec<String> = args.iter().map(type_display).collect();
            format!("{}<{}>", name, inner.join(", "))
        }
        Type::Shared(name) => name.clone(),
        Type::Rc(inner) => format!("Rc[{}]", type_display(inner)),
        Type::Arc(inner) => format!("Arc[{}]", type_display(inner)),
        Type::Function {
            params,
            return_type,
        } => {
            let p: Vec<String> = params.iter().map(type_display).collect();
            if **return_type == Type::Unit {
                format!("Fn({})", p.join(", "))
            } else {
                format!("Fn({}) -> {}", p.join(", "), type_display(return_type))
            }
        }
        Type::OnceFunction {
            params,
            return_type,
        } => {
            let p: Vec<String> = params.iter().map(type_display).collect();
            if **return_type == Type::Unit {
                format!("OnceFn({})", p.join(", "))
            } else {
                format!("OnceFn({}) -> {}", p.join(", "), type_display(return_type))
            }
        }
        Type::Ref(inner) => format!("ref {}", type_display(inner)),
        Type::MutRef(inner) => format!("mut ref {}", type_display(inner)),
        Type::Weak(inner) => format!("weak {}", type_display(inner)),
        Type::Pointer { is_mut, inner } => {
            if *is_mut {
                format!("*mut {}", type_display(inner))
            } else {
                format!("*const {}", type_display(inner))
            }
        }
        Type::TypeParam(name) => name.clone(),
        Type::TypeVar(id) => format!("?T{}", id.0),
        Type::AssocProjection {
            param,
            assoc,
            args,
            receiver_args,
        } => {
            let recv_str = if receiver_args.is_empty() {
                param.clone()
            } else {
                let inner: Vec<String> = receiver_args.iter().map(type_display).collect();
                format!("{}<{}>", param, inner.join(", "))
            };
            if args.is_empty() {
                format!("{}.{}", recv_str, assoc)
            } else {
                let inner: Vec<String> = args.iter().map(type_display).collect();
                format!("{}.{}[{}]", recv_str, assoc, inner.join(", "))
            }
        }
        Type::Existential {
            trait_name,
            trait_args,
            ..
        } => {
            if trait_args.is_empty() {
                format!("impl {}", trait_name)
            } else {
                let inner: Vec<String> = trait_args.iter().map(type_display).collect();
                format!("impl {}[{}]", trait_name, inner.join(", "))
            }
        }
        // Bespoke: print the refinement's nominal name (`Positive`), not
        // the base (`i64`), so diagnostics name the type the user wrote.
        Type::Refinement { name, .. } => name.clone(),
        Type::Error => "<error>".to_string(),
    }
}

// ── Type Compatibility ──────────────────────────────────────────

/// True iff `name` is a primitive / prelude type or stdlib module name
/// reachable at scope-0 — used by `resolve_identifier_type`'s variant
/// fallback to skip name-shadow cases like `Json.String(String)` where
/// the variant name collides with the primitive type name. See the
/// comment block at the variant-fallback site.
pub(super) fn is_prelude_type_or_module_name(name: &str) -> bool {
    crate::prelude::PRELUDE_PRIMITIVES.contains(&name)
        || crate::prelude::PRELUDE_TYPES.contains(&name)
}

pub(super) fn is_numeric(ty: &Type) -> bool {
    matches!(ty, Type::Int(_) | Type::UInt(_) | Type::Float(_))
}

/// True for operands `String + String` accepts: a `String` value or a
/// borrow of one (`ref String` / `mut ref String`). Borrows are stripped
/// because both backends materialize the underlying String value for the
/// concat — the interpreter holds a plain `Value::String` for a borrowed
/// param, and codegen auto-loads the pointee before the binop.
pub(super) fn is_string_concat_operand(ty: &Type) -> bool {
    match ty {
        Type::Str => true,
        Type::Ref(inner) | Type::MutRef(inner) => matches!(inner.as_ref(), Type::Str),
        _ => false,
    }
}

pub(super) fn is_integer(ty: &Type) -> bool {
    matches!(ty, Type::Int(_) | Type::UInt(_))
}

/// Width of an integer type in bits, for the char→int narrowing check.
/// `usize` / `isize` are conservatively treated as 32-bit so a 32-bit
/// target rejects `char as usize`; on 64-bit targets the cast is still
/// allowed via the wider-int path. The actual address-width of `usize`
/// is platform-dependent and folded in at codegen.
pub(super) fn integer_width_bits(ty: &Type) -> Option<u32> {
    match ty {
        Type::Int(IntSize::I8) => Some(8),
        Type::Int(IntSize::I16) => Some(16),
        Type::Int(IntSize::I32) => Some(32),
        Type::Int(IntSize::I64) => Some(64),
        Type::UInt(UIntSize::U8) => Some(8),
        Type::UInt(UIntSize::U16) => Some(16),
        Type::UInt(UIntSize::U32) => Some(32),
        Type::UInt(UIntSize::U64) => Some(64),
        Type::UInt(UIntSize::Usize) => Some(64),
        _ => None,
    }
}

/// Width (in bits) and signedness of an integer scalar type, covering the
/// full 8/16/32/64/128 range plus `usize` (modeled at 64, matching the
/// codegen target). `None` for anything that is not a concrete integer.
/// Used by the B-2026-07-09-7 implicit-widening gate — unlike
/// `integer_width_bits` it does not drop `i128`/`u128` and it reports the
/// sign, both of which the widening predicate needs.
pub(super) fn int_signed_width(ty: &Type) -> Option<(u32, bool)> {
    match ty {
        Type::Int(IntSize::I8) => Some((8, true)),
        Type::Int(IntSize::I16) => Some((16, true)),
        Type::Int(IntSize::I32) => Some((32, true)),
        Type::Int(IntSize::I64) => Some((64, true)),
        Type::Int(IntSize::I128) => Some((128, true)),
        Type::UInt(UIntSize::U8) => Some((8, false)),
        Type::UInt(UIntSize::U16) => Some((16, false)),
        Type::UInt(UIntSize::U32) => Some((32, false)),
        Type::UInt(UIntSize::U64) => Some((64, false)),
        Type::UInt(UIntSize::U128) => Some((128, false)),
        Type::UInt(UIntSize::Usize) => Some((64, false)),
        _ => None,
    }
}

/// Is coercing a value of integer type `from` into integer slot `to`
/// value-preserving for EVERY representable value (i.e. implicit-widening,
/// no `as` required)? The B-2026-07-09-7 rule (design decision (B)):
///   - signed→signed / unsigned→unsigned: widen iff `to` is at least as wide;
///   - unsigned→signed: widen iff `to` is strictly wider (`u8`→`i16` fits,
///     `u8`→`i8` does not — the sign bit steals a value);
///   - signed→unsigned: NEVER implicit (negative values change meaning).
///
/// Equal types are trivially fine. Non-integer types return `false` here;
/// callers gate on both sides being integers first.
pub(super) fn int_coercion_is_widening(from: &Type, to: &Type) -> bool {
    if from == to {
        return true;
    }
    let (Some((fw, fs)), Some((tw, ts))) = (int_signed_width(from), int_signed_width(to)) else {
        return false;
    };
    match (fs, ts) {
        (true, true) | (false, false) => tw >= fw,
        (false, true) => tw > fw,
        (true, false) => false,
    }
}

/// Map a typechecked receiver type to the receiver-name segment used in the
/// `Type.method` keys of `EffectCheckResult.{inferred,declared}_effects`
/// (and therefore in `Program.callee_effectful`). Returns `None` for
/// shapes that don't carry method dispatch in v1 (function types, type
/// variables, `Type::Error`, etc.). Used by `infer_method_call` to
/// populate `method_callee_types`, which feeds the par-branch cancel-check
/// narrowing.
pub(super) fn method_callee_type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Named { name, .. } => Some(name.clone()),
        // A `shared struct` / `shared enum` (or `par`) receiver is a real
        // `Type.method` call site — its RC-pointer value carries the same
        // inherent/impl methods a value receiver does. Without this arm the
        // `method_callee_types` side-table skips every shared-receiver call, so
        // codegen's `dispatch_key` is `None` for them — which broke fresh-temp
        // shared method dispatch (`make_shared().m()`) until this was added.
        Type::Shared(name) => Some(name.clone()),
        Type::Str => Some("String".to_string()),
        Type::Slice { .. } => Some("Slice".to_string()),
        Type::Array { .. } => Some("Array".to_string()),
        Type::Vector { .. } => Some("Vector".to_string()),
        Type::Bool => Some("bool".to_string()),
        Type::Char => Some("char".to_string()),
        Type::Int(IntSize::I8) => Some("i8".to_string()),
        Type::Int(IntSize::I16) => Some("i16".to_string()),
        Type::Int(IntSize::I32) => Some("i32".to_string()),
        Type::Int(IntSize::I64) => Some("i64".to_string()),
        Type::UInt(UIntSize::U8) => Some("u8".to_string()),
        Type::UInt(UIntSize::U16) => Some("u16".to_string()),
        Type::UInt(UIntSize::U32) => Some("u32".to_string()),
        Type::UInt(UIntSize::U64) => Some("u64".to_string()),
        Type::UInt(UIntSize::Usize) => Some("usize".to_string()),
        Type::Float(FloatSize::F32) => Some("f32".to_string()),
        Type::Float(FloatSize::F64) => Some("f64".to_string()),
        Type::Ref(inner) | Type::MutRef(inner) | Type::Weak(inner) => {
            method_callee_type_name(inner)
        }
        _ => None,
    }
}

/// Resolve the `Item` type of an iterable receiver — the element yielded by
/// `next()` after `iter()` / `into_iter()`. Returns `None` if `ty` is not an
/// iterable collection. `Map[K, V]` yields `(K, V)` tuples per design.md
/// § Iteration; `Vec`, `Set`, `SortedSet`, `Array`, `Slice` yield `T`.
/// `ref` / `mut ref` borrows are unwrapped transparently.
pub(super) fn iterator_item_type_for(ty: &Type) -> Option<Type> {
    match ty {
        Type::Array { element, .. } => Some((**element).clone()),
        Type::Slice { element, .. } => Some((**element).clone()),
        Type::Named { name, args } => match name.as_str() {
            "Vec" | "Set" | "SortedSet" | "VecDeque" if args.len() == 1 => Some(args[0].clone()),
            "Map" | "SortedMap" if args.len() == 2 => {
                Some(Type::Tuple(vec![args[0].clone(), args[1].clone()]))
            }
            // `Range` / `RangeInclusive` are Iterators — `(0..n).iter()` is
            // a redundant pass-through that yields the bound element type.
            "Range" | "RangeInclusive" if args.len() == 1 => Some(args[0].clone()),
            _ => None,
        },
        Type::Ref(inner) | Type::MutRef(inner) => iterator_item_type_for(inner),
        _ => None,
    }
}

/// Return the `Self` type for `clone()` on stdlib collection types, or
/// None if the receiver isn't a Clone-bearing collection. Used by the
/// `clone()` arm in `infer_method_call` so any `ref`/`mut ref` borrow of
/// a collection still resolves to the underlying owned type. See the
/// `Clone trait surface for collections` bullet in
/// `phase-8-stdlib-floor.md`.
///
/// Element-type `T: Clone` bound checking rides the existing trait-bound
/// machinery — primitives, `String`, and stdlib collection types satisfy
/// `Clone` trivially; user structs without `#[derive(Clone)]` would be
/// rejected at the bound-resolution layer when that lands.
pub(super) fn clone_self_type_for(ty: &Type) -> Option<Type> {
    match ty {
        Type::Str => Some(Type::Str),
        Type::Array { .. } => Some(ty.clone()),
        Type::Vector { .. } => Some(ty.clone()),
        Type::Named { name, args: _ } => match name.as_str() {
            "Vec" | "Set" | "SortedSet" | "VecDeque" | "Map" | "SortedMap" | "TreeMap" => {
                Some(ty.clone())
            }
            _ => None,
        },
        Type::Ref(inner) | Type::MutRef(inner) => clone_self_type_for(inner),
        _ => None,
    }
}

/// Walk `ty` looking for any `Type::TypeParam` or `Type::AssocProjection` node.
/// Used by `infer_call` to decide whether a callee signature needs ad-hoc
/// generic instantiation.
pub(super) fn contains_type_param(ty: &Type) -> bool {
    match ty {
        Type::TypeParam(_) | Type::AssocProjection { .. } => true,
        Type::Tuple(elems) => elems.iter().any(contains_type_param),
        // Const generics slice 3b: a `ConstArg::ConstParam` in
        // `Type::Array.size` is also a "generic dependency" — the
        // call-site inference solver needs to fire so the const-param
        // gets resolved. Pre-3b this returned false for
        // `Array[i64, ConstParam(N)]` and the generic-call path was
        // skipped entirely; post-3b we check the size too.
        Type::Array { element, size } => {
            contains_type_param(element) || matches!(size, ConstArg::ConstParam(_))
        }
        Type::Vector { element, lanes } => {
            contains_type_param(element) || matches!(lanes, ConstArg::ConstParam(_))
        }
        Type::Slice { element, .. } => contains_type_param(element),
        // Phase 11 Q1: a dim param or splice inside a shape is a generic
        // dependency exactly like Array's const-param size — the
        // call-site solver must fire so dims get resolved.
        Type::Shape(dims) => dims.iter().any(|d| {
            matches!(
                d,
                DimArg::Const(ConstArg::ConstParam(_)) | DimArg::Splice(_) | DimArg::SpliceVar(_)
            )
        }),
        Type::Ref(inner) | Type::MutRef(inner) | Type::Weak(inner) => contains_type_param(inner),
        Type::Pointer { inner, .. } => contains_type_param(inner),
        Type::Named { args, .. } => args.iter().any(contains_type_param),
        Type::Function {
            params,
            return_type,
        }
        | Type::OnceFunction {
            params,
            return_type,
        } => params.iter().any(contains_type_param) || contains_type_param(return_type),
        _ => false,
    }
}

/// Directional subsumption: can a value of type `sub_ty` be used where
/// `super_ty` is expected? Used by `check_assignable` (item 131 sub-step 3).
///
/// Differs from `types_compatible` in two ways:
///   1. **Function-type variance** — params are contravariant
///      (`is_subtype(b_p, s_p)` per pair) and return is covariant
///      (`is_subtype(s_r, b_r)`). For Kāra v1 with no user-declared
///      subtyping, this is observationally equivalent to the symmetric
///      check on the body — the variance plumbing is foundational for
///      future subtyping (refinement narrowing, declarable trait variance).
///   2. **`Fn → OnceFn` upward subtyping** — a `Type::Function` value
///      satisfies a `Type::OnceFunction` slot (callable-once is a weaker
///      contract than repeatedly-callable). The reverse direction is
///      rejected here and produces the focused E0235 (`OnceFnIntoFnSlot`)
///      diagnostic via `check_assignable`'s `is_once_into_fn_shape` arm.
///
/// Borrow forms (`Ref`/`MutRef`) recurse through `is_subtype` so the
/// function-arm subsumption applies under references too. Everything
/// else delegates to `types_compatible`; deep variance on nested
/// compound types (`Vec[Fn(...)]` → `Vec[OnceFn(...)]`, tuple element
/// subsumption) is intentionally out of scope until Kāra introduces
/// declarable variance for user-defined generics.
///
/// Effect-set variance (the third leg of design.md § Type Inference's
/// subsumption rule) is deferred until phase-3 lands effect variables
/// on `Type::Function` — the type lacks an effect-set field today.
pub(super) fn is_subtype(super_ty: &Type, sub_ty: &Type) -> bool {
    if super_ty == sub_ty {
        return true;
    }
    match (super_ty, sub_ty) {
        (
            Type::Function {
                params: sp,
                return_type: sr,
            },
            Type::Function {
                params: bp,
                return_type: br,
            },
        )
        | (
            Type::OnceFunction {
                params: sp,
                return_type: sr,
            },
            Type::OnceFunction {
                params: bp,
                return_type: br,
            },
        )
        | (
            Type::OnceFunction {
                params: sp,
                return_type: sr,
            },
            Type::Function {
                params: bp,
                return_type: br,
            },
        ) => {
            sp.len() == bp.len()
                && sp.iter().zip(bp.iter()).all(|(s, b)| is_subtype(b, s))
                && is_subtype(sr, br)
        }
        // `ref T` target is covariant; `mut ref T` is invariant in `T`
        // (design.md § Variance — load-bearing soundness pin): the
        // refinement-base widening is rejected in both directions, so
        // the MutRef arm demands mutual subtyping.
        (Type::Ref(s), Type::Ref(b)) => is_subtype(s, b),
        // A `mut ref T` reborrows down to a read-only `ref U` view: sound and
        // covariant in the pointee *exactly like* the `Ref → Ref` arm above,
        // because the destination `ref` cannot write through the borrow — so
        // pointee widening is safe here, unlike the invariant `MutRef → MutRef`
        // arm below (which must reject refinement-base widening in both
        // directions because the callee writes through it). This is the
        // `&mut T → &T` reborrow. Without it, passing a `mut ref` binding to a
        // `ref` parameter is accepted by the interpreter (`karac run`, a
        // warning) but rejected by `karac build` — the run/build divergence
        // tracked as B-2026-06-17-4.
        (Type::Ref(s), Type::MutRef(b)) => is_subtype(s, b),
        (Type::MutRef(s), Type::MutRef(b)) => is_subtype(s, b) && is_subtype(b, s),
        // Owned-to-ref coercion: a `ref T` slot accepts an owned `T` value.
        // design.md Feature 4 Part 3 § Explicit ref for Borrow Returns — the
        // borrow source is inferred from context (function-return tail, call
        // arg, field init, let RHS) and the ownership checker handles the
        // borrow-not-consume semantics via the slot's declared mode. The
        // reverse (`ref T` → owned `T`) requires explicit deref and is not
        // accepted here.
        (Type::Ref(inner), sub) if !matches!(sub, Type::Ref(_) | Type::MutRef(_)) => {
            is_subtype(inner, sub)
        }
        // Owned-to-mut-ref coercion (slice 8ag): a `mut ref T` slot
        // accepts an owned `T` source at call boundaries. The `mut`
        // marker is enforced separately by `check_call_site_marker`;
        // is_subtype / types_compatible are type-level only, mirroring
        // the existing `mut Slice[T]` ↔ owned `Vec[T]` / `Array[T, N]`
        // coercion pattern in `types_compatible`. The `Ref` exclusion
        // blocks `ref T → mut ref T` (loss-of-mutability would be
        // wrong); the identity `MutRef → MutRef` case is handled by
        // the dedicated arm above.
        // Invariant like the MutRef↔MutRef arm: the callee writes
        // through the borrow into the caller's owned binding, so a
        // widened slot could store a refinement-violating value.
        (Type::MutRef(inner), sub) if !matches!(sub, Type::Ref(_) | Type::MutRef(_)) => {
            is_subtype(inner, sub) && is_subtype(sub, inner)
        }
        _ => types_compatible(super_ty, sub_ty),
    }
}

/// LB3 — labeled-block LUB inference helper.
///
/// Compute the labeled-block expression's type by joining the tail
/// expression's type with each `break label expr` value-type collected
/// during body inference. Rules:
/// - `Type::Never` is the unit element: a `break label expr` after which
///   control cannot fall through (or vice versa) doesn't constrain the
///   block type.
/// - `Type::Error` propagates (any error participating in the LUB poisons
///   the result so cascading errors don't fire).
/// - All non-`Never` participants must be pairwise `types_compatible`;
///   otherwise the block type collapses to `Type::Error`. Diagnosing the
///   actual mismatch is left to the surrounding context (the
///   labeled-block expression participates as an operand and the parent
///   site emits the focused diagnostic).
///
/// The helper is deliberately conservative — it does not perform unification
/// across type metavariables. The current `if`-arm joining path uses the
/// same one-shot `types_compatible` check, so this is consistent with the
/// rest of the typechecker. A more aggressive `lub_n` over metavariables
/// is a future refactor (out-of-scope for this slice).
pub(super) fn lub_block_type(tail: Type, breaks: &[Type]) -> Type {
    // Pick the first non-Never as the candidate.
    let mut candidate: Option<Type> = if tail != Type::Never {
        Some(tail.clone())
    } else {
        None
    };
    for b in breaks {
        if *b == Type::Never {
            continue;
        }
        if *b == Type::Error {
            return Type::Error;
        }
        match &candidate {
            None => candidate = Some(b.clone()),
            Some(c) => {
                if *c == Type::Error {
                    return Type::Error;
                }
                if !types_compatible(c, b) {
                    return Type::Error;
                }
            }
        }
    }
    candidate.unwrap_or(tail)
}

/// GAT slice 8c — detect the "one-sided projection vs concrete with
/// the projection still unresolvable" shape after both sides have
/// been routed through `resolve_assoc_projections`. The
/// `types_compatible_with_projections` /
/// `is_subtype_with_projections` wrappers use this as a permissive
/// short-circuit: if either side is still a projection AND the other
/// is anything else (including a structurally-different projection),
/// the wrapper returns `true` rather than firing a spurious
/// diagnostic on a generic function body's return / arg position
/// where the projection's eventual binding depends on the caller's
/// type-param choice. Two structurally identical projections fall
/// through to the regular structural check below (which accepts
/// them) — the unresolvable-fallback only triggers on the mixed
/// shape. The strict negative path fires at the call site where the
/// projection's receiver is bound to a concrete type and the
/// impl-table lookup succeeds; the resolved RHS then unifies via
/// the regular structural arm.
pub(super) fn projection_unresolvable_with(a: &Type, b: &Type) -> bool {
    match (a, b) {
        (Type::AssocProjection { .. }, Type::AssocProjection { .. }) => false,
        (Type::AssocProjection { .. }, _) | (_, Type::AssocProjection { .. }) => true,
        _ => false,
    }
}

/// Generic-argument compatibility under a slot's declared variance
/// (design.md § Variance > per-type variance). The invariant form is
/// *mutual* compatibility rather than structural equality so the
/// symmetric permissive arms of `types_compatible` (TypeParam
/// wildcards, integer-width compat, Never/Error) keep working during
/// inference, while one-directional widenings (refined→base) are
/// rejected.
fn generic_arg_compatible(variance: crate::ast::Variance, a: &Type, b: &Type) -> bool {
    match variance {
        crate::ast::Variance::Covariant => types_compatible(a, b),
        crate::ast::Variance::Contravariant => types_compatible(b, a),
        crate::ast::Variance::Invariant => types_compatible(a, b) && types_compatible(b, a),
    }
}

pub(super) fn types_compatible(a: &Type, b: &Type) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Type::Error, _) | (_, Type::Error) => true,
        (Type::Never, _) | (_, Type::Never) => true,
        // `impl Trait` slice 3 — two existentials unify only when they
        // come from the same declaration site (matched `origin` SpanKey).
        // Same-origin guarantees same witness; different origins are two
        // independent existentials even when the named trait matches. A
        // one-sided existential against a non-existential is rejected
        // here as the structural rule — the trait-surface compatibility
        // (concrete witness satisfies the trait bound) lives in
        // `check_assignable` / `type_satisfies_bound`, where the impl
        // table is reachable through `&TypeChecker`.
        (
            Type::Existential {
                origin: a_origin, ..
            },
            Type::Existential {
                origin: b_origin, ..
            },
        ) => a_origin == b_origin,
        (Type::Existential { .. }, _) | (_, Type::Existential { .. }) => false,
        // Unresolved type parameters and associated type projections are
        // treated as permissive — they appear when a generic enum constructor
        // leaves an argument unconstrained (e.g. `let x: Option[i64] = None`
        // — the `None` arm has no value from which to solve `T`). Equivalent
        // to the `TypeVar` handling below.
        (Type::TypeParam(_), _) | (_, Type::TypeParam(_)) => true,
        // GAT slice 8c — `AssocProjection` arm tightened to structural
        // equality only. Pre-slice-8c the arm was wildcard-permissive
        // (`(AssocProjection, _) | (_, AssocProjection) => true`), so
        // an unresolved projection would unify with any RHS — masking
        // legitimate mismatches whenever a projection survived the
        // resolution step. The slice 8c rule:
        //   - Two `AssocProjection` nodes match iff their `param`,
        //     `assoc`, `args`, and `receiver_args` all structurally
        //     match (component-wise `types_compatible`).
        //   - A one-sided projection against any non-projection /
        //     non-error / non-never type falls through to `false`.
        // Callers with access to `&TypeChecker` should route through
        // `types_compatible_with_projections` (in `typechecker.rs`) to
        // get the projection-aware resolution behaviour before this
        // structural check fires — that wrapper resolves projections
        // through `impl_assoc_types` first, then falls through here.
        // This function stays pure (no `&TypeChecker`) so it can be
        // Phase 11 Q1: shape-vs-shape compatibility. `?` matches any
        // dim (committed-at-runtime); unresolved dim params / vars and
        // splices are permissive (mirroring the TypeParam wildcard rule
        // above) — concrete-vs-concrete dims must agree.
        (Type::Shape(xd), Type::Shape(yd)) => {
            let has_splice = |dims: &[DimArg]| {
                dims.iter()
                    .any(|d| matches!(d, DimArg::Splice(_) | DimArg::SpliceVar(_)))
            };
            if has_splice(xd) || has_splice(yd) {
                // Splices bind arbitrary-length middles — defer to the
                // unifier's split; structurally permissive here.
                true
            } else {
                xd.len() == yd.len()
                    && xd.iter().zip(yd.iter()).all(|(x, y)| match (x, y) {
                        (DimArg::Dynamic, _) | (_, DimArg::Dynamic) => true,
                        (DimArg::Const(cx), DimArg::Const(cy)) => match (cx, cy) {
                            (ConstArg::Literal(a), ConstArg::Literal(b)) => a == b,
                            // Unresolved params / metavars are wildcards
                            // at the compatibility layer.
                            _ => true,
                        },
                        _ => true,
                    })
            }
        }
        // called from `inference.rs::unify_types`, `lub_block_type`,
        // and the slice/array coercion arms below.
        (
            Type::AssocProjection {
                param: ap,
                assoc: aa,
                args: aas,
                receiver_args: ar,
            },
            Type::AssocProjection {
                param: bp,
                assoc: ba,
                args: bas,
                receiver_args: br,
            },
        ) => {
            ap == bp
                && aa == ba
                && aas.len() == bas.len()
                && aas
                    .iter()
                    .zip(bas.iter())
                    .all(|(x, y)| types_compatible(x, y))
                && ar.len() == br.len()
                && ar
                    .iter()
                    .zip(br.iter())
                    .all(|(x, y)| types_compatible(x, y))
        }
        (Type::AssocProjection { .. }, _) | (_, Type::AssocProjection { .. }) => false,
        // Pragmatic: integer literals (i64) compatible with any int/uint
        (Type::Int(_), Type::Int(_)) => true,
        (Type::UInt(_), Type::UInt(_)) => true,
        (Type::Int(_), Type::UInt(_)) | (Type::UInt(_), Type::Int(_)) => true,
        (Type::Float(_), Type::Float(_)) => true,
        // Implicit widening: int/uint ↔ float (pragmatic, bidirectional for compatibility checks)
        (Type::Int(_), Type::Float(_)) | (Type::Float(_), Type::Int(_)) => true,
        (Type::UInt(_), Type::Float(_)) | (Type::Float(_), Type::UInt(_)) => true,
        (Type::Tuple(a_types), Type::Tuple(b_types)) => {
            a_types.len() == b_types.len()
                && a_types
                    .iter()
                    .zip(b_types.iter())
                    .all(|(a, b)| types_compatible(a, b))
        }
        // Named types: generic-argument compatibility is governed by
        // the type's per-slot variance declaration (design.md §
        // Variance; `prelude::STDLIB_VARIANCE`). `+T` slots accept the
        // refinement-to-base widening directionally (`Iterator[Positive]`
        // → `Iterator[i32]`); `=T` slots — every parameter of every
        // user type, and the conservative stdlib default — demand
        // mutual compatibility, which rejects one-directional widening
        // (`Vec[Positive]` does NOT widen to `Vec[i32]`) while keeping
        // the symmetric permissive arms (TypeParam wildcards, integer
        // compat, Never/Error) intact; `-T` slots flip the direction.
        (
            Type::Named {
                name: a_name,
                args: a_args,
            },
            Type::Named {
                name: b_name,
                args: b_args,
            },
        ) => {
            let variances = crate::prelude::stdlib_variance(a_name);
            a_name == b_name
                && a_args.len() == b_args.len()
                && a_args
                    .iter()
                    .zip(b_args.iter())
                    .enumerate()
                    .all(|(i, (a, b))| {
                        let slot = variances
                            .and_then(|v| v.get(i).copied())
                            .unwrap_or(crate::ast::Variance::Invariant);
                        generic_arg_compatible(slot, a, b)
                    })
        }
        // `ref T` target is covariant; `mut ref T` is invariant in `T`
        // — the load-bearing soundness pin (design.md § Variance):
        // refinement-base widening (`mut ref Positive` → `mut ref i32`)
        // is rejected in both directions.
        (Type::Ref(a), Type::Ref(b)) => types_compatible(a, b),
        (Type::MutRef(a), Type::MutRef(b)) => {
            generic_arg_compatible(crate::ast::Variance::Invariant, a, b)
        }
        // Owned-to-mut-ref coercion (slice 8ag): a `mut ref T` slot
        // accepts an owned source at call boundaries. The `mut` marker
        // is enforced separately by `check_call_site_marker`;
        // types_compatible is type-level only, mirroring the existing
        // `mut Slice[T]` ↔ owned `Vec[T]` / `Array[T, N]` coercion
        // pattern below. The `Ref`/`MutRef` exclusion blocks the cross-
        // mutability case (`ref T` → `mut ref T`). Invariant like the
        // MutRef↔MutRef arm above: the callee writes through the borrow
        // into the caller's owned binding, so refinement-base widening
        // (owned `Positive` source → `mut ref i64` slot) would let the
        // callee store a refinement-violating value.
        (Type::MutRef(a), sub) if !matches!(sub, Type::Ref(_) | Type::MutRef(_)) => {
            generic_arg_compatible(crate::ast::Variance::Invariant, a, sub)
        }
        // Raw pointers — sibling to the `Ref`/`MutRef` arms above.
        // Constness is part of the shape; cross-constness compatibility
        // is the user's responsibility (the strict-provenance APIs at
        // line 511 provide `ptr.with_addr` / `ptr.with_addr_mut` for the
        // explicit conversion path).
        (
            Type::Pointer {
                is_mut: a_mut,
                inner: a_inner,
            },
            Type::Pointer {
                is_mut: b_mut,
                inner: b_inner,
            },
        ) if a_mut == b_mut => {
            // `*const T` is a read-only view — covariant; `*mut T` is
            // a write window — invariant (same lemma as `mut ref T`).
            if *a_mut {
                generic_arg_compatible(crate::ast::Variance::Invariant, a_inner, b_inner)
            } else {
                types_compatible(a_inner, b_inner)
            }
        }
        (
            Type::Array {
                element: a_el,
                size: a_sz,
            },
            Type::Array {
                element: b_el,
                size: b_sz,
            },
        ) => {
            // `Array[=T, const N]` — invariant element per the stdlib
            // variance audit (mutable through `mut ref`).
            a_sz == b_sz && generic_arg_compatible(crate::ast::Variance::Invariant, a_el, b_el)
        }
        // Slice[T] → Slice[T] with compatible elements (identity case is
        // covered above by `a == b`; this arm handles e.g. integer
        // compatibility on the element type).
        (
            Type::Slice {
                element: a_el,
                mutable: a_mut,
            },
            Type::Slice {
                element: b_el,
                mutable: b_mut,
            },
        ) => {
            // Read-only slot accepts mutable source (reborrow as read-only).
            // Mutable slot rejects read-only source. Elements are
            // invariant per the stdlib variance audit (`Slice[=T]` —
            // the type name is shared between read-only and mut views).
            let mut_ok = !*a_mut || *b_mut;
            mut_ok && generic_arg_compatible(crate::ast::Variance::Invariant, a_el, b_el)
        }
        // Coercion at call boundaries: `Slice[T]` accepts `Vec[T]` / `Array[T, N]`,
        // and their `ref` borrows. One-directional — the reverse is not compatible.
        // See design.md § Slices.
        (
            Type::Slice {
                element: slice_el,
                mutable: false,
            },
            Type::Array {
                element: arr_el, ..
            },
        ) => generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, arr_el),
        (
            Type::Slice {
                element: slice_el,
                mutable: false,
            },
            Type::Named { name, args },
        ) if name == "Vec" && args.len() == 1 => {
            generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, &args[0])
        }
        (
            Type::Slice {
                element: slice_el,
                mutable: false,
            },
            Type::Ref(inner),
        ) => match inner.as_ref() {
            Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, &args[0])
            }
            Type::Array {
                element: arr_el, ..
            } => generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, arr_el),
            Type::Slice {
                element: inner_el, ..
            } => generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, inner_el),
            _ => false,
        },
        // `mut Slice[T]` at the slot — accepts `mut ref Vec[T]` / `mut ref Array[T, N]`
        // / `mut Slice[T]` itself (already covered by the generic Slice→Slice arm),
        // and also owned `Vec[T]` / `Array[T, N]` at call boundaries. The owned-source
        // case requires a `mut` marker at the call site (enforced separately by
        // `check_call_site_marker`); check_assignable is type-level only.
        // Read-only sources (`ref Vec`, `Slice{mutable:false}`) do not upgrade.
        (
            Type::Slice {
                element: slice_el,
                mutable: true,
            },
            Type::MutRef(inner),
        ) => match inner.as_ref() {
            Type::Named { name, args } if name == "Vec" && args.len() == 1 => {
                generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, &args[0])
            }
            Type::Array {
                element: arr_el, ..
            } => generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, arr_el),
            Type::Slice {
                element: inner_el,
                mutable: true,
            } => generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, inner_el),
            _ => false,
        },
        (
            Type::Slice {
                element: slice_el,
                mutable: true,
            },
            Type::Array {
                element: arr_el, ..
            },
        ) => generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, arr_el),
        (
            Type::Slice {
                element: slice_el,
                mutable: true,
            },
            Type::Named { name, args },
        ) if name == "Vec" && args.len() == 1 => {
            generic_arg_compatible(crate::ast::Variance::Invariant, slice_el, &args[0])
        }
        (
            Type::Function {
                params: a_p,
                return_type: a_r,
            },
            Type::Function {
                params: b_p,
                return_type: b_r,
            },
        )
        | (
            Type::OnceFunction {
                params: a_p,
                return_type: a_r,
            },
            Type::OnceFunction {
                params: b_p,
                return_type: b_r,
            },
        ) => {
            // Position-based variance (design.md § Variance table):
            // function arguments are contravariant — a source fn that
            // accepts the *wider* type serves a slot demanding the
            // narrower one — and the return type is covariant. The
            // pre-variance implementation checked arguments
            // covariantly, which let `Fn(Positive)` flow into a
            // `Fn(i64)` slot (unsound) while rejecting the sound
            // direction.
            a_p.len() == b_p.len()
                && a_p
                    .iter()
                    .zip(b_p.iter())
                    .all(|(a, b)| types_compatible(b, a))
                && types_compatible(a_r, b_r)
        }
        // Refinement types — one-directional refined→base widening
        // (design.md § Refinement Types). `types_compatible(target, source)`
        // here treats `a` as the slot/target and `b` as the value/source.
        //
        // Target is a refinement (`a`): only an *identical* refinement is
        // accepted, and that already short-circuited via the `a == b` fast
        // path at the top. A bare base value — or a *different* refinement
        // over the same base — must narrow explicitly through
        // `try_from` / `as`, so it falls through to `false` here (no
        // implicit narrowing into a refined slot).
        (Type::Refinement { .. }, _) => false,
        // Source is a refinement (`b`) against a non-refinement target:
        // widen by stripping to the base and re-checking. This is the
        // refined→base direction — a `Positive` value is accepted wherever
        // an `i64` is expected, and (via the `Named`/`Tuple`/etc. arms
        // recursing into element positions) `Vec[Positive]` against a
        // `Vec[i64]` slot.
        (_, Type::Refinement { .. }) => types_compatible(a, strip_refinement(b)),
        (Type::Shared(a_name), Type::Shared(b_name)) => a_name == b_name,
        // `Rc[=T]` / `Arc[=T]` — invariant per the stdlib variance
        // audit (consuming the handle yields a `T`; widening would
        // violate move-out semantics).
        (Type::Rc(a_inner), Type::Rc(b_inner)) => {
            generic_arg_compatible(crate::ast::Variance::Invariant, a_inner, b_inner)
        }
        (Type::Arc(a_inner), Type::Arc(b_inner)) => {
            generic_arg_compatible(crate::ast::Variance::Invariant, a_inner, b_inner)
        }
        // No (Rc, Arc) / (Arc, Rc) cross arms: `Rc[T]` is not assignable
        // to `Arc[T]` and vice versa per design.md § RC integration. The
        // value-site auto-promotion in `OwnershipChecker::promote_rc_to_arc`
        // is the only path that crosses the boundary, and it rewrites the
        // value's representation, not the type — so type-level compat
        // stays strict.
        _ => false,
    }
}

// ── Local Type Scope ────────────────────────────────────────────

#[cfg(test)]
mod refinement_tests {
    use super::*;

    fn refinement(name: &str, base: Type) -> Type {
        Type::Refinement {
            name: name.to_string(),
            base: Box::new(base),
        }
    }

    #[test]
    fn strip_refinement_unwraps_to_base() {
        let pos = refinement("Positive", Type::Int(IntSize::I64));
        assert_eq!(strip_refinement(&pos), &Type::Int(IntSize::I64));
        // Non-refinement types pass through untouched.
        assert_eq!(strip_refinement(&Type::Bool), &Type::Bool);
        // Nested refinement (defensive — base is normally non-refined) is
        // peeled all the way to the leaf base.
        let nested = refinement("Outer", refinement("Inner", Type::Str));
        assert_eq!(strip_refinement(&nested), &Type::Str);
    }

    #[test]
    fn type_display_uses_nominal_name() {
        let pos = refinement("Positive", Type::Int(IntSize::I64));
        // Bespoke arm: the refinement name, not the base `i64`.
        assert_eq!(type_display(&pos), "Positive");
    }

    #[test]
    fn refined_widens_to_base_but_not_the_reverse() {
        let pos = refinement("Positive", Type::Int(IntSize::I64));
        let base = Type::Int(IntSize::I64);
        // `types_compatible(target, source)`: a refined *source* widens
        // into a base *target* (refined→base) ...
        assert!(types_compatible(&base, &pos));
        // ... but a bare base value does NOT implicitly narrow into a
        // refined slot — that requires explicit `try_from` / `as`.
        assert!(!types_compatible(&pos, &base));
    }

    #[test]
    fn distinct_refinements_over_same_base_do_not_implicitly_coerce() {
        // A refined target only accepts an identical refinement; a
        // *different* refinement over the same base must convert
        // explicitly (no implicit cross-refinement coercion).
        let positive = refinement("Positive", Type::Int(IntSize::I64));
        let nonzero = refinement("NonZero", Type::Int(IntSize::I64));
        assert!(!types_compatible(&positive, &nonzero));
        assert!(!types_compatible(&nonzero, &positive));
        // Both still widen to the shared base in the refined→base direction.
        let base = Type::Int(IntSize::I64);
        assert!(types_compatible(&base, &positive));
        assert!(types_compatible(&base, &nonzero));
    }

    #[test]
    fn identical_refinements_are_equal() {
        // Nominal equality: same name + base ⇒ equal via derived PartialEq,
        // short-circuiting the `a == b` fast path in `types_compatible`.
        let a = refinement("Positive", Type::Int(IntSize::I64));
        let b = refinement("Positive", Type::Int(IntSize::I64));
        assert_eq!(a, b);
        // Different name ⇒ not structurally equal (must widen to compare).
        let c = refinement("NonZero", Type::Int(IntSize::I64));
        assert_ne!(a, c);
    }
}
