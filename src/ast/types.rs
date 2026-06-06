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
    /// Path segments — `["allow"]` for bare `#[allow]`, `["diagnostic",
    /// "on_unimplemented"]` for namespaced `#[diagnostic::on_unimplemented]`
    /// (per syntax.md §8). Always at least one segment.
    pub path: Vec<String>,
    pub args: Vec<AttrArg>,
    pub string_value: Option<String>,
}

impl Attribute {
    /// True iff the attribute is a bare single-segment path matching `name`.
    /// Use this for matching compiler-recognised attributes — e.g.
    /// `attr.is_bare("allow")` does NOT match `#[diagnostic::allow]`. The
    /// namespace dispatch for multi-segment paths is handled separately.
    pub fn is_bare(&self, name: &str) -> bool {
        self.path.len() == 1 && self.path[0] == name
    }
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
    pub effect_params: Vec<EffectParam>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<TraitBound>,
    pub is_const: bool,
    pub const_type: Option<TypeExpr>,
    /// `...S` — shape-variadic parameter (syntax.md § GENERIC_PARAM
    /// variadic row): binds a full dim list (Shape kind). Mutually
    /// exclusive with `is_const`. A `: Dim` bound on a non-variadic
    /// param is carried in `bounds` and classified at the typechecker
    /// (like the structural `Effect` marker).
    pub is_variadic_shape: bool,
    pub span: Span,
}

/// An effect-parameter generic, e.g. `with E` (positional, bounds empty)
/// or `E: Effect` (slice 8ac trait-bound form, bounds carries the single
/// `Effect` marker). `Effect` is a built-in trait name recognised
/// structurally by the parser; classification of `E: Effect` as an
/// effect-bounded param happens at parse time when the first bound's
/// single-segment path is exactly `Effect`. Multi-bound effect-params
/// (`E: Effect + UserExtension`) and granular constraint bounds
/// (`E: no writes(R)`, design.md line 3150) remain reserved syntax —
/// the parser stores any extras for future use but only the leading
/// `Effect` marker is acted on in v1.
#[derive(Debug, Clone)]
pub struct EffectParam {
    pub name: String,
    pub bounds: Vec<TraitBound>,
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
    /// A shape literal — `[3, 4, ?]`, `[...S, M]` — in generic-argument
    /// position (e.g., `Tensor[f64, [3, 4, ?]]`). Legal only as a generic
    /// arg whose target kind is Shape; never a standalone type. See
    /// syntax.md § SHAPE_LIT and design.md § Numerical Types > Tensor.
    Shape(ShapeLit),
}

/// A shape literal: a type-level list of dims constructing a Shape-kinded
/// generic argument. Shape literals do not nest — a dim is a const
/// expression, a `?`, or a `...IDENT` variadic splice, never another
/// shape literal (syntax.md § SHAPE_LIT).
#[derive(Debug, Clone)]
pub struct ShapeLit {
    pub dims: Vec<ShapeDim>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ShapeDim {
    /// Const-expression dim — static at compile time: an integer literal
    /// (`3`), a Dim-kinded generic param (`N`), or a module-level integer
    /// constant. Arithmetic on shape params (`[A + B]`) parses but is
    /// deferred to v1.5 at the typechecker (roadmap § Phase 11 Q2).
    /// Boxed to keep the enum small next to the marker variants.
    Const(Box<Expr>),
    /// `?` — dynamic dim marker; the dim is determined at runtime. Reuses
    /// the expression-level question-mark token; legal only inside a shape
    /// literal (context disambiguates — outside shape position `?` remains
    /// the try operator, syntax.md §5.21).
    Dynamic { span: Span },
    /// `...IDENT` — variadic shape splice binding the remainder of the
    /// shape (`Tensor[T, [...S, M]]`).
    Splice { name: String, span: Span },
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
    /// `impl Trait[GenericArgs] [with EffectList]` — existential / sugar
    /// type marker (see design.md § `impl Trait` (Existential Types) and
    /// the parent epic in phase-5-diagnostics.md line 391).
    ///
    /// Slice 1 lands the parser surface + AST node only. Downstream
    /// semantic handling is split across later slices:
    ///
    /// - Slice 2 — resolver desugar of argument-position occurrences
    ///   into anonymous generic parameters `[T_impl_N: Trait]`.
    /// - Slice 3 — typechecker semantics for return-position + RPITIT.
    /// - Slice 4 — capture-set checker.
    ///
    /// Until those slices land, downstream passes treat `ImplTrait`
    /// analogously to a `Path` type referencing the named trait — the
    /// resolver / typechecker will report the trait name as "type not
    /// yet supported in this position" via their existing diagnostics
    /// rather than panicking on the new variant.
    ///
    /// Fields:
    /// - `trait_path` — the trait's path (e.g. `Iterator`,
    ///   `std.iter.Iterator`). Mirrors the `PathExpr` shape used by
    ///   regular path types so the resolver can route the lookup
    ///   through the same surface.
    /// - `args` — `[GenericArg, ...]` after the trait path
    ///   (e.g. the `Item = i64` in `impl Iterator[Item = i64]`). Empty
    ///   when the bound has no generic args.
    /// - `use_effects` — `with EFFECT_LIST` suffix on the type
    ///   expression. Distinct from the surrounding function's
    ///   execution-effect `with` clause — see design.md §
    ///   "Effect surface — split construction and use".
    /// - `span` — full source span of the `impl Trait[…] with …`
    ///   type expression.
    ImplTrait {
        trait_path: PathExpr,
        args: Vec<GenericArg>,
        use_effects: Option<EffectList>,
        span: Span,
    },
    /// `dyn TRAIT_PATH[GENERIC_ARGS]` — trait-object type marker. The
    /// general `dyn Trait` feature (vtable construction, dynamic
    /// dispatch, effect-opacity story) is **P1-deferred** per design.md
    /// § Polymorphism. The parser accepts the surface today only so
    /// the `impl Trait` epic's slice-5 check (RPITIT blocks `dyn Trait`)
    /// has a syntactic target. The typechecker lowers `TypeKind::Dyn`
    /// to `Type::Error` with one of two focused diagnostics:
    ///
    /// 1. `E_RPITIT_INCOMPATIBLE_WITH_DYN` — when the named trait has
    ///    one or more methods that return `impl Trait` (no fixed
    ///    vtable slot can be synthesized for those methods); the
    ///    diagnostic names the offending method so the user can
    ///    refactor.
    /// 2. `E_DYN_TRAIT_NOT_IMPLEMENTED_YET` — otherwise, the generic
    ///    P1-deferred stub; the trait is not RPITIT but `dyn Trait`
    ///    as a value/type form is not yet wired through the type
    ///    system / codegen / effect checker.
    ///
    /// Fields mirror `ImplTrait` — `trait_path` is the trait's path
    /// (e.g. `Display`, `std.io.Read`); `args` carries positional
    /// generic args on the trait; `span` is the full source span of
    /// the `dyn Trait[…]` type expression.
    Dyn {
        trait_path: PathExpr,
        args: Vec<GenericArg>,
        span: Span,
    },
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
