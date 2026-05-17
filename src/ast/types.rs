//! Type-expression, attribute, and generics AST — `TypeExpr` and its
//! `TypeKind` variants, `EffectSpec` for `fn ... with E` annotations,
//! `PathExpr` for generic-path expressions, the `Attribute` / `AttrArg`
//! pair, and the full generics surface (`GenericParams`,
//! `GenericParam`, `TraitBound`, `GenericArg`, `WhereClause`,
//! `WhereConstraint`).

use crate::token::Span;

use super::{EffectList, Expr};

// ── Attributes ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Attribute {
    pub span: Span,
    pub name: String,
    pub args: Vec<AttrArg>,
    pub string_value: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AttrArg {
    /// `Some(name)` for `name = value` / `name: value` style. `None`
    /// for positional arguments like `#[with_provider(Clock, FakeClock.new)]`
    /// — rare in existing attributes but required for constructor-fn
    /// attributes where the argument is an arbitrary expression rather
    /// than a named key.
    pub name: Option<String>,
    pub value: Option<Expr>,
    pub span: Span,
}

// ── Generics ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GenericParams {
    pub params: Vec<GenericParam>,
    pub effect_params: Vec<String>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<TraitBound>,
    pub is_const: bool,
    pub const_type: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TraitBound {
    pub path: Vec<String>,
    pub generic_args: Option<Vec<GenericArg>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum GenericArg {
    Type(TypeExpr),
    Const(Expr),
}

// ── Where Clauses ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WhereClause {
    pub constraints: Vec<WhereConstraint>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum WhereConstraint {
    /// `T: Bound1 + Bound2` — type parameter bounds
    TypeBound {
        type_name: String,
        bounds: Vec<TraitBound>,
        span: Span,
    },
    /// `T::Assoc = ConcreteType` — associated type equality
    AssocTypeEq {
        type_name: String,
        assoc_name: String,
        ty: TypeExpr,
        span: Span,
    },
    /// `F.Mapped[i64]: FromIterator[i64]` — bound on an associated-type
    /// projection. Distinct from `TypeBound` because the LHS is a full
    /// projection type-expression rather than a bare type-parameter
    /// name. GAT slice 8a: discharged at call sites in
    /// `discharge_projection_bounds` after substituting the call's
    /// type-arg solutions into the projection and resolving via
    /// `resolve_assoc_projections`.
    ProjectionBound {
        projection: TypeExpr,
        bounds: Vec<TraitBound>,
        span: Span,
    },
    /// `N >= 0`, `M < 4096`, etc. — const-expression predicate over
    /// const-generic params. Parsed at slice 1; evaluated by slice 2's
    /// const-expression evaluator and discharged at call sites by slice 3.
    ConstPredicate { expr: Expr, span: Span },
}

// ── Types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum TypeKind {
    Path(PathExpr),
    Tuple(Vec<TypeExpr>),
    Array {
        element: Box<TypeExpr>,
        size: Box<Expr>,
    },
    Pointer {
        is_mut: bool,
        inner: Box<TypeExpr>,
    },
    FnType {
        params: Vec<TypeExpr>,
        return_type: Option<Box<TypeExpr>>,
        effect_spec: Option<EffectSpec>,
        /// Round 12.46 (Step 4): set when the surface annotation is
        /// `OnceFn(...)` rather than `Fn(...)`. The two share AST shape and
        /// effect-spec structure; only the lowering target differs
        /// (`Type::OnceFunction` vs `Type::Function`).
        is_once: bool,
    },
    Ref(Box<TypeExpr>),
    MutRef(Box<TypeExpr>),
    /// `mut Slice[T]` — a mutable slice view. The inner `TypeExpr` is the
    /// element type `T`. Distinct from `MutRef(Slice[T])`, which would be a
    /// mutable borrow of a slice value (and is not part of the language).
    MutSlice(Box<TypeExpr>),
    Weak(Box<TypeExpr>),
    Unit,
    Error,
}

#[derive(Debug, Clone)]
pub enum EffectSpec {
    Polymorphic,
    Specific(EffectList),
}

// ── Paths ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PathExpr {
    pub segments: Vec<String>,
    pub generic_args: Option<Vec<GenericArg>>,
    pub span: Span,
}
