//! Type-expression, attribute, and generics AST — `TypeExpr` and its
//! `TypeKind` variants, `EffectSpec` for `fn ... with E` annotations,
//! `PathExpr` for generic-path expressions, the `Attribute` / `AttrArg`
//! pair, and the full generics surface (`GenericParams`,
//! `GenericParam`, `TraitBound`, `GenericArg`, `WhereClause`,
//! `WhereConstraint`).

use crate::token::Span;

use super::{EffectList, EffectVerb, Expr};

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
    /// `#[no_effect(allocates(Heap), panics)]` payload — the effect verbs
    /// this attribute forbids, parsed with the SAME `try_parse_effect_verb`
    /// the `with` clause uses.
    ///
    /// A separate field rather than `args` because effect verbs are reserved
    /// KEYWORDS: `allocates` cannot travel through `AttrArg::value`, which
    /// holds an `Expr`, without either being rejected outright (the shipped
    /// behaviour before this existed — `'allocates' is a reserved keyword`)
    /// or being flattened into an identifier string that loses the resource
    /// list. `reads(A, B)` and `reads(fs.File[N])` are both legal effect
    /// syntax, so the flattening would not round-trip. Empty for every other
    /// attribute.
    pub effect_args: Vec<EffectVerb>,
}

impl Attribute {
    /// True iff the attribute is a bare single-segment path matching `name`.
    /// Use this for matching compiler-recognised attributes — e.g.
    /// `attr.is_bare("allow")` does NOT match `#[diagnostic::allow]`. The
    /// namespace dispatch for multi-segment paths is handled separately.
    pub fn is_bare(&self, name: &str) -> bool {
        self.path.len() == 1 && self.path[0] == name
    }

    /// True iff this attribute is `#[repr(C)]`.
    ///
    /// Hoisted onto `Attribute` (B-2026-08-21-2) so the C-header emitter and
    /// the resolver's `repr_c_layout_ignored` check answer one question one
    /// way. They disagree about `#[repr(C)]` at their peril: the header
    /// emitter decides a struct crosses the C ABI, and the resolver decides
    /// that the same struct's layout block is therefore ignored — a copy that
    /// drifted would let one of them act on a repr the other did not see.
    pub fn is_repr_c(&self) -> bool {
        use super::ExprKind;
        self.is_bare("repr")
            && self.args.iter().any(|arg| {
                if arg.name.is_some() {
                    return false;
                }
                match arg.value.as_ref().map(|e| &e.kind) {
                    Some(ExprKind::Identifier(s)) => s == "C",
                    Some(ExprKind::Path { segments, .. }) => {
                        segments.len() == 1 && segments[0] == "C"
                    }
                    _ => false,
                }
            })
    }

    /// True for the loop attribute that opts a Collect-shaped loop into
    /// parallel fan-out: canonical `#[par_order_free]`, or its
    /// deprecated pre-rename spelling `#[par_unordered]`.
    ///
    /// B-2026-07-29-30 renamed it. The old name asserted a fact about
    /// BEHAVIOUR — "the output ordering differs from iteration-order" —
    /// that no shipped code path exhibits: with no work-stealing, chunks
    /// are statically assigned and contiguous, so concatenating
    /// per-worker partials in worker order IS iteration order, and the
    /// tabulate specialization writes each element straight into its
    /// final slot. Measured with a position-sensitive digest over
    /// 200_000 elements, both Collect paths produce byte-identical
    /// output under `KARAC_AUTO_PAR=1` and `=0`.
    ///
    /// The gate itself is correct and stays: requiring an opt-in is what
    /// keeps "auto-par never changes what your program prints" an
    /// unconditional invariant, and it reserves the freedom to ship
    /// work-stealing later without silently reordering existing
    /// programs. Only the NAME changed — from a claim about what the
    /// compiler does to a promise the user makes ("my output does not
    /// depend on order; reorder if it helps"). That is also the
    /// vocabulary the other end of the pipe already uses:
    /// `KARAC_PAR_ORDER_FREE_FLAG`, bit 63 of the descriptor's cost
    /// field.
    ///
    /// The old spelling is still accepted, unchanged in meaning, so
    /// out-of-tree sources keep compiling across the rename.
    pub fn is_par_order_free(&self) -> bool {
        self.is_bare("par_order_free") || self.is_bare("par_unordered")
    }

    /// Return the display name (`"inline"`, `"inline(always)"`,
    /// `"inline(never)"`, `"cold"`) iff this attribute is one of the
    /// codegen-hint attributes (design.md § Codegen Hint Attributes),
    /// else `None`. Classifies by surface shape only — arg-shape
    /// validation and conflict detection live in the parser scan
    /// (`scan_codegen_hint_attrs`); a malformed `#[inline(bogus)]`
    /// still classifies as `"inline"` here so placement gates can name
    /// it. Used by the resolver placement checks and the parser's
    /// closure-position check.
    pub fn codegen_hint_name(&self) -> Option<&'static str> {
        use crate::ast::ExprKind;
        if self.is_bare("cold") {
            return Some("cold");
        }
        if self.is_bare("inline") {
            if self.args.len() == 1 && self.args[0].name.is_none() {
                let ident = match self.args[0].value.as_ref().map(|e| &e.kind) {
                    Some(ExprKind::Identifier(s)) => Some(s.as_str()),
                    Some(ExprKind::Path { segments, .. }) if segments.len() == 1 => {
                        Some(segments[0].as_str())
                    }
                    _ => None,
                };
                return Some(match ident {
                    Some("always") => "inline(always)",
                    Some("never") => "inline(never)",
                    _ => "inline",
                });
            }
            return Some("inline");
        }
        None
    }
}

/// The CPU features enabled by every `#[target_feature(...)]` attribute in
/// `attrs`, as bare feature names (no `+`/`-` prefix), de-duplicated in first-
/// seen order. Recognizes the design's `#[target_feature(enable = "avx2,bmi2")]`
/// form (design.md § Multiversioning, floor/ceiling composition) and the
/// checklist's positional `#[target_feature("avx2")]` shorthand; a feature
/// string may be a comma-separated list. Empty when no such attribute is
/// present. Consumed by codegen (emits the LLVM per-function `target-features`
/// string attribute) and by the parser's placement validation.
pub fn target_feature_enables(attrs: &[Attribute]) -> Vec<String> {
    use crate::ast::ExprKind;
    let mut out: Vec<String> = Vec::new();
    let push_list = |s: &str, out: &mut Vec<String>| {
        for feat in s.split(',') {
            let f = feat.trim();
            if !f.is_empty() && !out.iter().any(|e| e == f) {
                out.push(f.to_string());
            }
        }
    };
    for attr in attrs {
        if !attr.is_bare("target_feature") {
            continue;
        }
        // `#[target_feature("avx2")]` — the string rides `string_value`.
        if let Some(s) = &attr.string_value {
            push_list(s, &mut out);
        }
        // `#[target_feature(enable = "avx2")]` / positional `#[target_feature("avx2")]`.
        for arg in &attr.args {
            let is_enable = arg.name.as_deref() == Some("enable") || arg.name.is_none();
            if !is_enable {
                continue;
            }
            if let Some(Expr {
                kind: ExprKind::StringLit(s),
                ..
            }) = &arg.value
            {
                push_list(s, &mut out);
            }
        }
    }
    out
}

/// The feature-name list of a `#[multiversion(baseline, "avx2", "avx512f")]`
/// attribute (its string-literal args; the `baseline` marker identifier is
/// ignored — a baseline variant is always synthesized). A feature string may be
/// a comma-separated list. `None` when no `#[multiversion]` attribute is present;
/// `Some(vec![])` when it is present but names no feature (a parser error). Order
/// is preserved (the desugar dispatches last-listed-first = widest-first).
pub fn multiversion_feature_list(attrs: &[Attribute]) -> Option<Vec<String>> {
    use crate::ast::ExprKind;
    let attr = attrs.iter().find(|a| a.is_bare("multiversion"))?;
    let mut feats: Vec<String> = Vec::new();
    let push = |s: &str, out: &mut Vec<String>| {
        for f in s.split(',') {
            let f = f.trim();
            if !f.is_empty() && !out.iter().any(|e| e == f) {
                out.push(f.to_string());
            }
        }
    };
    if let Some(s) = &attr.string_value {
        push(s, &mut feats);
    }
    for arg in &attr.args {
        if let Some(Expr {
            kind: ExprKind::StringLit(s),
            ..
        }) = &arg.value
        {
            push(s, &mut feats);
        }
    }
    Some(feats)
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

/// Declared variance of a generic type parameter — the `+T` / `-T` /
/// `=T` marker surface (syntax.md §6.4 VARIANCE_MARKER; design.md
/// § Variance). No marker defaults to `Invariant`, the conservative
/// choice. At v1 the `+`/`-` markers are legal only on stdlib
/// parametric type declarations (user code gets
/// `E_VARIANCE_USER_DECL_NOT_YET`); the verifier proves stdlib
/// declarations consistent with the type's structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variance {
    /// `+T` — `Foo[Sub] <: Foo[Super]` when `Sub <: Super`.
    Covariant,
    /// `-T` — `Foo[Super] <: Foo[Sub]` when `Sub <: Super`.
    Contravariant,
    /// `=T` or no marker — `Foo[A]` / `Foo[B]` unrelated unless `A == B`.
    Invariant,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<TraitBound>,
    pub is_const: bool,
    pub const_type: Option<TypeExpr>,
    /// Declared variance (design.md § Variance). `Invariant` when no
    /// marker is written. `variance_span` is `Some` only when an
    /// explicit marker token (`+`/`-`/`=`) was present — the stdlib
    /// explicit-variance lint distinguishes explicit `=T` from the
    /// implicit default, and the user-side rejection diagnostic
    /// underlines the marker itself.
    pub variance: Variance,
    pub variance_span: Option<Span>,
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
    /// Inline associated-type bindings written inside the bound's bracket
    /// list — the `Item = T` of `Iterator[Item = T]` (design.md § Iterator
    /// and the Vec/Map/SortedMap method tables; B-2026-08-21-9). Kept
    /// SEPARATE from `generic_args` because they are not positional type
    /// arguments: the desugar pass hoists each one into an equivalent
    /// `WhereConstraint::AssocTypeEq` on the enclosing item, which is the
    /// form the rest of the compiler already understands, so nothing
    /// downstream of `desugar_program` ever sees this field non-empty.
    pub assoc_bindings: Vec<AssocBinding>,
    pub span: Span,
}

/// One `IDENT = TYPE` inside a trait bound's bracket list. See
/// [`TraitBound::assoc_bindings`].
#[derive(Debug, Clone)]
pub struct AssocBinding {
    pub name: String,
    pub ty: TypeExpr,
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
    /// `frozen T` — B-2026-08-01-33 mechanism 3, stage 1. A non-owning,
    /// NON-COUNTING handle to a deeply-immutable `shared` value whose lifetime
    /// is guaranteed to span the current region. See
    /// [`docs/spikes/freeze-point-design.md`](../../docs/spikes/freeze-point-design.md).
    ///
    /// **NOTHING CONSTRUCTS THIS VARIANT TODAY — read [`Param::is_frozen`]
    /// first.** The parser records the mode on the *parameter*, not inside its
    /// type, because a mode in the type tree has to be unwrapped at every site
    /// that already unwraps `Ref | MutRef`, and four separate rounds of such
    /// sites turned up in codegen alone before that approach was abandoned.
    /// The variant and its (exhaustively handled) match arms are retained
    /// because stage 2 — widening `frozen` past parameter position, where a
    /// per-param bit no longer suffices — genuinely needs a type-level mode,
    /// and re-deriving the ~16 walk sites is work already done and verified.
    /// Anything added here must keep the arms complete; do not reach for a
    /// wildcard.
    ///
    /// Stage 1 lands the SURFACE ONLY: every downstream pass treats
    /// `frozen T` exactly as `T`, so the mode is inert — RC traffic is still
    /// emitted and `E_CONCURRENT_SHARED_STRUCT` still fires on a multi-branch
    /// capture. That is deliberate and is the safe order to build this in:
    /// admitting a `frozen` binding across `par` branches BEFORE the escape
    /// checker exists would hand out a non-counting handle with nothing
    /// stopping it from outliving its owner, which is a use-after-free. The
    /// admission is wired only once the checks it depends on hold.
    Frozen(Box<TypeExpr>),
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
    /// - `args` — POSITIONAL `[GenericArg, ...]` after the trait path
    ///   (the `i64` in `impl Reduce[i64]`). Empty when the bound has no
    ///   positional generic args. An inline `Item = i64` is NOT one of
    ///   these — see `assoc_bindings` below.
    /// - `use_effects` — `with EFFECT_LIST` suffix on the type
    ///   expression. Distinct from the surrounding function's
    ///   execution-effect `with` clause — see design.md §
    ///   "Effect surface — split construction and use".
    /// - `span` — full source span of the `impl Trait[…] with …`
    ///   type expression.
    ImplTrait {
        trait_path: PathExpr,
        args: Vec<GenericArg>,
        /// Inline associated-type bindings written inside the bracket list —
        /// the `Item = i64` of `impl Iterator[Item = i64]` (design.md's own
        /// Map/Vec method tables spell returns this way; B-2026-08-22-4).
        /// Kept SEPARATE from `args` for the same reason
        /// [`TraitBound::assoc_bindings`] is: they are not positional type
        /// arguments.
        ///
        /// Unlike the bound-position form, these are NOT hoisted away by
        /// `desugar_program` in every position. Argument-position
        /// `impl Trait` desugars to a named synthetic type parameter, so its
        /// bindings do hoist onto a `WhereConstraint::AssocTypeEq` like any
        /// other bound's. A RETURN-position `impl Trait` has no name to
        /// constrain — the binding is a property of the existential itself —
        /// so it survives lowering into `Type::Existential::assoc_bindings`.
        assoc_bindings: Vec<AssocBinding>,
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
        /// Inline associated-type bindings — see the `ImplTrait` field of the
        /// same name. Parsed for surface parity with `impl Trait` (a parse
        /// error here would be a strictly worse diagnostic than the focused
        /// P1-deferred one the lowering already emits) and then dropped:
        /// `dyn Trait` lowers to `Type::Error` regardless.
        assoc_bindings: Vec<AssocBinding>,
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
