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

    /// `T.Item` — an associated type projection. `param` is the generic type
    /// parameter name (e.g. `"I"`); `assoc` is the associated type name
    /// (e.g. `"Item"`). Resolved to a concrete type when the parameter is
    /// instantiated via `resolve_assoc_projections`.
    AssocProjection {
        param: String,
        assoc: String,
    },

    Error,
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
