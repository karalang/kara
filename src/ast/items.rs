//! Item AST — every top-level declaration form: functions, structs,
//! enums, traits, impl blocks, effect declarations, layouts, modules,
//! consts, externs, type aliases, distinct types.
//!
//! `Item` is the discriminator; each variant wraps a per-kind struct
//! holding the parsed shape plus attributes / doc comments / spans.

use crate::token::Span;

use super::{
    Attribute, Block, Expr, ExprKind, GenericArg, GenericParams, PathExpr, Pattern, PatternKind,
    StmtKind, TraitBound, TypeExpr, Visibility, WhereClause,
};

// ── Item deprecation payload ─────────────────────────────────────

/// `#[deprecated]` payload captured at parse time and attached as
/// `Option<Deprecation>` to every AST item kind that the spec lists
/// as a valid target (function, struct, enum, trait, trait alias,
/// marker trait, distinct type — for the slice-1 surface; enum
/// variants, trait methods, type aliases, and module-level consts
/// land alongside attribute-support extensions to those AST nodes).
/// `since` and `note` are both optional — bare `#[deprecated]`
/// produces `Deprecation { since: None, note: None, span }`.
///
/// See design.md § `#[deprecated]` for Item Deprecation.
#[derive(Debug, Clone)]
pub struct Deprecation {
    pub span: Span,
    /// `since: "1.2.0"` — free-form version string surfaced verbatim
    /// in the use-site diagnostic. Advisory; the parser does NOT
    /// validate against `kara.toml`'s `[package].version` field.
    pub since: Option<String>,
    /// `note: "use `read_to_string` instead"` — migration message
    /// surfaced as the use-site warning's note. Also populated by
    /// the shorthand form `#[deprecated = "..."]`.
    pub note: Option<String>,
}

// ── `#[unstable]` payload ────────────────────────────────────────

/// `#[unstable]` payload captured at parse time. Attached as
/// `Option<Unstable>` to every item kind the spec lists as a valid
/// target (struct/enum/fn/method/trait/const — design.md §
/// v1 Positioning > Stable surface vs. unstable extension points,
/// surface table in syntax.md § 8). The attribute marks an API
/// surface point as deliberately unstable across compiler releases;
/// callers must opt in via `#[allow(unstable_api)]` on the
/// enclosing scope or via the global `[lints].allow_unstable_api`
/// opt-in in `kara.toml`. Stdlib-only at v1 (user-side use is a
/// future RFC); enforcement of "stdlib-only" is socially via the
/// stdlib-audit pass rather than parser-rejected.
///
/// Recognised forms (mirror `#[deprecated]`'s surface):
/// - bare `#[unstable]` → `Unstable { note: None, span }`
/// - shorthand `#[unstable = "note"]` → `note` populated
///
/// Long-form (`#[unstable(feature: "...", issue: "...", note: "...")]`)
/// is reserved syntactically — the parser accepts named args today
/// but only `note` is captured; unknown keys soft-warn (no hard
/// error) so a future RFC can add `feature` / `issue` without a
/// source break.
#[derive(Debug, Clone)]
pub struct Unstable {
    pub span: Span,
    /// `note: "low-level frame access — shape may change before \
    /// v1 lock"` — surfaced verbatim in the use-site
    /// `unstable_api` warning. Populated by the shorthand
    /// `#[unstable = "..."]` and by the long-form `note: "..."`.
    pub note: Option<String>,
}

// ── `#[diagnostic::on_unimplemented]` payload ────────────────────

/// `#[diagnostic::on_unimplemented(message: "...", label: "...", note: "...")]`
/// payload captured at parse time and attached as `Option<OnUnimplemented>`
/// to [`TraitDef`]. All three fields are optional named string-literal
/// arguments; any absent field falls back to the default failed-bound
/// diagnostic phrasing at emit time (slice 6 of item 36).
///
/// Template placeholders `{Self}`, `{T0}`, `{T1}`, ... in the message /
/// label / note are recognised; substitution happens at the failed-bound
/// emit site against the solved metavariable map (slice 6). Unknown
/// placeholders are warned about at parse time (slice 3 lint pass) and
/// render literally if reached at emit time.
///
/// See design.md § Diagnostic Namespace Attributes.
#[derive(Debug, Clone)]
pub struct OnUnimplemented {
    pub span: Span,
    pub message: Option<String>,
    pub label: Option<String>,
    pub note: Option<String>,
}

// ── Top-level Items ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Item {
    Function(Function),
    StructDef(StructDef),
    /// `[#[repr(C)]] union NAME { f1: T1, f2: T2, ... }` — FFI union
    /// declaration. v60 item 22 / design.md § FFI Unions. Unions share
    /// storage across their fields and are designed for C interop.
    /// `#[repr(C)]` is required at typecheck time. Per-field `Copy`
    /// bound is required (unions cannot run destructors on overlapping
    /// storage). Field reads and borrows require `unsafe { … }`; field
    /// assignment is unconditionally safe. No generic / tuple / empty
    /// forms — those are rejected at parse with focused diagnostics.
    UnionDef(UnionDef),
    EnumDef(EnumDef),
    TraitDef(TraitDef),
    /// `trait NAME[GENERICS] = bound1 + bound2 + ... [where ...];` — a
    /// trait alias declaration. v60 item 40 / design.md § Trait Aliases.
    /// At v1 the parser, AST, and resolver recognize the form; the
    /// typechecker emits a stub diagnostic (`E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET`)
    /// at every use site. Bound substitution lands in P1.
    TraitAlias(TraitAliasDef),
    /// `marker trait NAME[GENERICS] [: SUPERTRAITS] [where ...] ;` (or
    /// `{ }`). Method-less trait used as a typing tag — the body must be
    /// empty, and impls of the trait must be empty. v60 item 55 /
    /// design.md § Marker Traits.
    MarkerTrait(MarkerTraitDef),
    ImplBlock(ImplBlock),
    EffectResource(EffectResourceDecl),
    EffectGroup(EffectGroupDecl),
    EffectVerbDecl(EffectVerbDecl),
    LayoutDef(LayoutDef),
    UseDecl(UseDecl),
    Import(ImportDecl),
    ConstDecl(ConstDecl),
    /// `[ATTRIBUTES] [VIS] let [mut] NAME[: TYPE] = INIT;` — module-level
    /// binding (immutable or mutable). Spec at `design.md § Module-Level
    /// Bindings`. Slice-1 surface: parser produces the AST node; downstream
    /// phases (resolver, typechecker, effect-checker, ownership, codegen)
    /// emit `E_MODULE_BINDING_NOT_YET_IMPLEMENTED` at the declaration site
    /// until slices 3-9 wire real semantics. Pattern mirrors
    /// [`Item::TraitAlias`] — parser/AST recognized, downstream stub
    /// diagnostic until the dedicated implementation lands.
    ModuleBinding(ModuleBinding),
    AliasDecl(AliasDecl),
    IndependentDecl(IndependentDecl),
    ExternFunction(ExternFunction),
    /// `unsafe extern "ABI" { ... }` block — the trust-boundary form for
    /// foreign-import declarations. The bare standalone `extern "C" fn
    /// name(...);` shape is rejected at module scope; foreign imports
    /// must live inside one of these blocks. Block-level attributes
    /// (`@noblock`, `#[noblock]`) are pre-merged into each contained
    /// item's `attributes` at parse time, so downstream phases process
    /// the inner `ExternFunction`s identically to today's standalone
    /// form. The block itself carries the abi/doc/attributes for
    /// block-scoped consumers (e.g. the `undocumented_unsafe` lint).
    ExternBlock(ExternBlock),
    TypeAlias(TypeAliasDef),
    DistinctType(DistinctTypeDef),
    /// `test "case name" { body }` — top-level test-case declaration per
    /// `design.md § Testing`. The string literal is the user-visible case
    /// name (appears verbatim in `test_pass` / `test_fail` JSONL events
    /// and matches `--filter`). The body is a regular block, evaluated
    /// once by the test runner with `runtime_errors` / `pending_cf` /
    /// `tracked_effects` reset to a clean slate. Visibility modifiers
    /// (`pub` / `private`) are rejected at parse — test cases aren't
    /// callables. Allowed at module scope only; the parser emits
    /// `E_TEST_BLOCK_NOT_TOP_LEVEL` when the `test "..." { }` shape
    /// appears inside a function body.
    TestCase(TestCase),
}

// ── Functions ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Function {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments immediately preceding the
    /// item. `None` when no doc comments were attached. CommonMark.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    /// `unsafe fn ...` declaration marker. The `unsafe` keyword on a fn
    /// declaration is a *precondition* the function asserts callers must
    /// satisfy — it is NOT an implicit `unsafe { ... }` block around the
    /// body. The `unsafe_op_in_unsafe_fn` rule (slice 3 of the v2 unsafe
    /// epic) walks every fn body uniformly and requires raw-ptr derefs,
    /// calls to other `unsafe fn`s, asm intrinsics, volatile reads/writes,
    /// and union field access to be wrapped in `unsafe { ... }` even
    /// inside an `unsafe fn` body. Slice 1 only captures the surface
    /// marker; the lint that consumes it lands in slice 3.
    pub is_unsafe: bool,
    /// `comptime fn ...` declaration marker. The function's body runs at
    /// compile time when invoked from a comptime context (a `comptime { ... }`
    /// block, a const-generic argument, or a default parameter value). A
    /// `comptime fn` may take `comptime`-prefixed parameters and `Type`-typed
    /// parameters. Slice 1 captures the surface marker only; the comptime
    /// evaluator that consumes it lands in a later slice. Spec: deferred.md §
    /// Comptime — AST→AST `comptime fn` (form 1, the declaration).
    pub is_comptime: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub params: Vec<Param>,
    pub self_param: Option<SelfParam>,
    /// `frozen self` — B-2026-08-01-33 mechanism 3, stage 2.7. Recorded as a
    /// BOOL beside the receiver rather than as a fourth [`SelfParam`] variant,
    /// for the reason stage 1 kept `frozen` off the type tree: a new variant
    /// would have to be handled at every one of the ~140 `self_param` sites,
    /// including backends that must never see the mode. `frozen self` parses
    /// to [`SelfParam::Ref`], so codegen and every phase that does not care
    /// see the borrow form they already handle, and only the checking phases
    /// consult this flag. False for every other receiver form and for every
    /// free function.
    pub self_is_frozen: bool,
    pub return_type: Option<TypeExpr>,
    pub effects: Option<EffectList>,
    pub requires: Vec<Expr>,
    pub ensures: Vec<EnsuresClause>,
    pub where_clause: Option<WhereClause>,
    pub body: Block,
    /// True iff this item came from baked stdlib source (CR-202 slice 3b).
    /// The parser writes `false`; the bake step in `prelude.rs` flips it
    /// to `true` after parsing. The resolver's `#[compiler_builtin]` gate
    /// (`E0115`) bypasses items where this is `true` so stdlib source can
    /// use the attribute even when the resolver's session-wide
    /// `is_stdlib_source` flag is unset (e.g. when the bake AST is
    /// spliced into a user-mode program tree).
    pub stdlib_origin: bool,
    /// `#[deprecated]` payload captured at parse time. `None` when the
    /// attribute is absent. Use-site diagnostic emission (slice 4)
    /// reads this through the resolver's symbol table; until then the
    /// field is structurally populated but no warning is emitted.
    /// See design.md § `#[deprecated]` for Item Deprecation.
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// `#[track_caller]` declared on this function — at call sites, the
    /// codegen pass injects a hidden caller-location argument carrying
    /// the call site's `(file, line, col)` so the panic runtime
    /// surfaces the caller's source location rather than this
    /// function's internal panic line. Slice 1 captures the flag only;
    /// the codegen + runtime integration (slices 4–5) consumes it.
    /// See design.md § Error Handling > "Stdlib panic-emitters report
    /// the caller's source location". Parser rejects arguments — the
    /// attribute takes none — and the resolver rejects placement on
    /// items that are not `fn` declarations.
    pub is_track_caller: bool,
    /// Inlining-axis codegen hint declared on this function —
    /// `#[inline]` → `Some(InlineHint::Default)`, `#[inline(always)]` →
    /// `Some(InlineHint::Always)`, `#[inline(never)]` →
    /// `Some(InlineHint::Never)`, absent → `None`. The parser scan
    /// validates the arg shape and rejects intra-axis conflicts
    /// (`E_INLINE_HINT_CONFLICT`); codegen lowers the hint to the
    /// matching LLVM function attribute (`inlinehint` / `alwaysinline`
    /// / `noinline`). A hint on a trait method declaration propagates
    /// to every impl method that does not set its own (see
    /// [`crate::desugar`]). See design.md § Codegen Hint Attributes.
    pub inline_hint: Option<InlineHint>,
    /// `#[cold]` declared on this function — the hot/cold placement
    /// axis, orthogonal to [`Function::inline_hint`]. Lowered to the
    /// LLVM `cold` function attribute. `#[cold]` + `#[inline(always)]`
    /// is rejected at parse (`E_COLD_INLINE_ALWAYS_CONFLICT`); every
    /// other inline/cold combination is legal. Propagates from a trait
    /// method declaration to non-overriding impls like `inline_hint`.
    pub is_cold: bool,
    /// `#[gpu]` declared on this function — the GPU-subset *constraint*
    /// marker (design.md § GPU Subset Constraints). It asserts "this
    /// function uses only the GPU-compatible subset" and makes the
    /// function GPU-callable; it does NOT route the function to the GPU
    /// (dispatch is always the explicit `gpu.dispatch(...)` call). This
    /// slice (FE-1) captures the surface marker only — the parser
    /// rejects arguments (`#[gpu]` is bare, `E_GPU_ARGS_NOT_PERMITTED`)
    /// and the resolver rejects placement on non-`fn` items
    /// (`E_GPU_INVALID_TARGET` / `E0800`). The enforcement that consumes
    /// it — the `GpuSafe` structural type-check (FE-2, `E0801`),
    /// `#[gpu]` call-graph validation (FE-3), and effect enforcement
    /// (FE-4) — lands in later slices. See
    /// `docs/implementation_checklist/phase-10-targets.md` § "GPU
    /// compute shaders — slice breakdown".
    pub is_gpu: bool,
    /// Lint-level overrides declared at this function via
    /// `#[allow(NAME)]` / `#[warn(NAME)]` / `#[deny(NAME)]` /
    /// `#[expect(NAME)]`. Each attribute produces one entry per
    /// lint name in its argument list. The scope cascade (outer
    /// item / module / build-flag) lands in a follow-up slice;
    /// at this slice the list is purely structural and informs
    /// no behavior yet — exists so the data is available to the
    /// emit-time consumer once it lands. See `design.md § Lint
    /// Level Attributes`.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
    /// `#[profile(P1, P2, ...)]` payload — the closed v1 set of
    /// declared profile names this function asserts compatibility
    /// with. Populated by the parser scan helper; the resolver
    /// validates each name against the `CompileProfile` enum and
    /// emits `E_UNKNOWN_PROFILE` on misspellings. Empty when no
    /// `#[profile]` attribute is present. Slice 3 (effect-checker
    /// integration) consults the list to intersect constraints
    /// against the function's effect set.
    pub profile_compat: Vec<String>,
    /// `#[no_effect(allocates(Heap), panics)]` payload — effect verbs this
    /// function asserts are ABSENT from its transitive effect set. Sibling of
    /// `profile_compat` and checked the same way, the difference being that
    /// this names the forbidden effects directly where `#[profile]` names
    /// profiles whose forbidden sets are looked up. Empty when the attribute
    /// is absent. Enforced in `effectchecker/no_effect.rs`; see design.md
    /// § No-Effect Attribute.
    pub no_effect: Vec<EffectVerb>,
    /// FFI export ABI for a *definition* exposed to C —
    /// `[pub] extern "C" fn name(...) { body }` →
    /// `Some("C")`, `extern "C-unwind" fn ...` → `Some("C-unwind")`.
    /// `None` for an ordinary Kāra function. This is the *export*
    /// side of the FFI boundary (a Kāra body callable from C), the
    /// dual of foreign *imports* which live as [`ExternFunction`]
    /// inside `unsafe extern { ... }` blocks. The marker drives two
    /// codegen decisions: the symbol is given External linkage so C
    /// can resolve it (Kāra fn names are already un-mangled), and
    /// `"C-unwind"` exports are gated until the unwind substrate
    /// lands (see design.md § Panic Semantics at the FFI Boundary,
    /// cases 1 & 2). Under the current abort-only panic model a body
    /// panic already aborts the process, which *is* the case-1
    /// auto-abort contract — so no catch trampoline is emitted yet.
    pub abi: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnsuresClause {
    pub param: Option<String>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelfParam {
    Owned,  // self (owned/consuming receiver — the default; `own self` is rejected)
    Ref,    // ref self
    MutRef, // mut ref self
}

#[derive(Debug, Clone)]
pub struct Param {
    pub span: Span,
    pub pattern: Pattern,
    pub ty: TypeExpr,
    pub default_value: Option<Expr>,
    pub doc_comment: Option<String>,
    /// `comptime`-prefixed parameter — the argument at the call site must be a
    /// compile-time-known value. Only legal on a `comptime fn`'s parameter
    /// list. Slice 1 captures the marker only; the typechecker rule (a
    /// `comptime` param requires a comptime-known argument) lands with the
    /// evaluator. Spec: deferred.md § Comptime (form 3, the parameter prefix).
    pub is_comptime: bool,
    /// `frozen`-prefixed parameter — a non-owning, non-counting handle to a
    /// deeply-immutable `shared` value (B-2026-08-01-33 mechanism 3;
    /// [`docs/spikes/freeze-point-design.md`](../../docs/spikes/freeze-point-design.md)).
    /// Stage 1 records the marker and nothing else: the mode is inert, so
    /// `frozen T` still means exactly `T` to every phase.
    ///
    /// **Recorded here, deliberately, rather than in the parameter's
    /// [`TypeExpr`].** `TypeKind::Frozen` exists and every walk handles it,
    /// but the parser does not construct it — carrying the mode inside the
    /// type tree means every phase that unwraps `Ref | MutRef` must learn to
    /// see through a third form, and codegen has more such sites than anyone
    /// enumerated (four rounds found, none of them the last). The mode is a
    /// *property of the parameter*, exactly like `is_comptime` above, and the
    /// phases that will act on it — escape checking, the freeze-site
    /// immutability check, `par` admission — ask about parameters, not about
    /// type trees. Codegen learns which values are non-counting through a
    /// plain-data hint (the `elidable_ref_params` channel), never through the
    /// AST, which is what keeps the codegen-containment invariant intact.
    ///
    /// The cost of that choice is that this bit only spans the one position
    /// stage 1 accepts. Widening `frozen` to `let` annotations, struct fields,
    /// or generic arguments is what `TypeKind::Frozen` is retained for, and it
    /// is a stage-2 decision that needs a checker behind each position first.
    pub is_frozen: bool,
}

impl Param {
    /// Returns the simple name if this param is a plain binding (not a destructuring pattern).
    pub fn name(&self) -> Option<&str> {
        if let PatternKind::Binding(ref name) = self.pattern.kind {
            Some(name)
        } else {
            None
        }
    }
}

/// Items that carry a three-level visibility (pub / default / private).
/// A tiny helper trait lets the resolver / typechecker treat them
/// uniformly without each call site having to read both bool fields.
macro_rules! impl_vis {
    ($ty:ident) => {
        impl $ty {
            pub fn visibility(&self) -> Visibility {
                Visibility::from_flags(self.is_pub, self.is_private)
            }
        }
    };
}

impl_vis!(Function);
impl_vis!(StructDef);
impl_vis!(UnionDef);
impl_vis!(EnumDef);
impl_vis!(TraitDef);
impl_vis!(ConstDecl);
impl_vis!(ModuleBinding);
impl_vis!(ExternFunction);
impl_vis!(OpaqueTypeDecl);
impl_vis!(TypeAliasDef);
impl_vis!(DistinctTypeDef);

// ── Structs ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StructDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the struct.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub is_shared: bool,
    /// `par struct` — concurrent shared type (always Arc; every `mut` field
    /// constrained to `Atomic[T]` / `Mutex[T]`; cross-task-safe by definition).
    /// Mutually exclusive with `is_shared` (a type is plain, `shared`, or `par`,
    /// never two at once — the parser only ever sets one). See design.md
    /// § "Part 5b: Concurrent Shared Types (`par struct`)". `kind_keyword_span`
    /// points at the `par` keyword when this is set.
    pub is_par: bool,
    /// Span of the `struct` keyword token itself (always present).
    /// Powers byte-precise rewrites — `E_CONCURRENT_PLAIN_STRUCT`'s
    /// fix_diff inserts `par ` immediately before this offset. Synthetic
    /// `StructDef`s built outside the parser (prelude, codegen prelude
    /// shims, tests) may carry a zero-length placeholder span; consumers
    /// that need the keyword position should gate on `kind_keyword_span`
    /// being meaningful instead of relying on this span alone.
    pub struct_keyword_span: Span,
    /// Span of the `shared` (or future `par`) keyword preceding `struct`,
    /// when present. `None` for plain `struct` definitions. Powers the
    /// `E_CONCURRENT_SHARED_STRUCT` fix_diff's rename edit (replace this
    /// span with `par`). See L201a in phase-7 tracker.
    pub kind_keyword_span: Option<Span>,
    pub no_rc: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub where_clause: Option<WhereClause>,
    pub fields: Vec<StructField>,
    pub invariants: Vec<Expr>,
    /// `impl invariant <expr>` clauses (design.md § Contracts — `impl
    /// invariant`). Unlike plain [`StructDef::invariants`] (checked only at
    /// `pub` method exits), these are checked at the exit of *every* method
    /// (pub and private). A parallel field rather than folding a scope into
    /// `invariants` keeps the existing `.invariants` readers unchanged.
    pub impl_invariants: Vec<Expr>,
    /// See [`Function::stdlib_origin`]. CR-202 slice 3b.
    pub stdlib_origin: bool,
    /// `#[deprecated]` payload — see [`Deprecation`] and design.md §
    /// `#[deprecated]` for Item Deprecation.
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// `#[non_exhaustive]` declared on this struct — the type may grow
    /// new public fields in future versions, and cross-package
    /// consumers must use `..` in exhaustive struct patterns and a
    /// `Struct.new(...)` constructor instead of a struct literal that
    /// names every field. See design.md § `#[non_exhaustive]` for
    /// Evolvable Public Types. Resolver rejects the attribute on
    /// non-`pub` structs (meaningless without a cross-package boundary).
    pub is_non_exhaustive: bool,
    /// See [`Function::lint_overrides`]. Slice-4a broadens lint-level
    /// attribute attachment beyond `Function` so the eventual scope
    /// cascade (slice 4b) can walk outward through struct / enum /
    /// trait / impl scopes and find the nearest override.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the field. `None`
    /// when no doc comments were attached. CommonMark, same shape as the
    /// item-level `doc_comment` fields.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_mut: bool,
    /// Span of the `mut` keyword token, when present (Some iff
    /// `is_mut`). Powers the `E_CONCURRENT_*_STRUCT` fix_diff's
    /// `mut ` strip edit: deletion runs from `mut_keyword_span.offset`
    /// to `name_span.offset` so trailing whitespace is included
    /// (parser preserves the source's exact spacing). See L201a.
    pub mut_keyword_span: Option<Span>,
    pub name: String,
    /// Span of the field's identifier token. Always present (even when
    /// no `mut` keyword precedes); used as the end-anchor for the
    /// `mut ` strip-range computation.
    pub name_span: Span,
    pub ty: TypeExpr,
}

// ── Unions ───────────────────────────────────────────────────────

/// `[#[repr(C)]] union NAME { f1: T1, f2: T2, ... }` — FFI union.
/// See `Item::UnionDef` for the high-level contract. v60 item 22 /
/// design.md § FFI Unions. Generics, tuple-style, and empty bodies
/// are rejected at parse time, so the AST never carries them; the
/// shape here is intentionally narrower than [`StructDef`] (no
/// `is_shared`, `no_rc`, `invariants`, `is_non_exhaustive` — those
/// are either nonsensical for unions or explicitly forbidden by the
/// spec).
#[derive(Debug, Clone)]
pub struct UnionDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub fields: Vec<UnionField>,
    /// See [`Function::stdlib_origin`]. Unions originating from baked
    /// stdlib source flip this to `true` after parsing.
    pub stdlib_origin: bool,
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

#[derive(Debug, Clone)]
pub struct UnionField {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub name: String,
    pub ty: TypeExpr,
}

// ── Enums ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the enum.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub is_shared: bool,
    /// `par enum` — concurrent shared enum (always Arc; every `mut` variant
    /// field constrained to `Atomic[T]` / `Mutex[T]`; cross-task-safe by
    /// definition). Mutually exclusive with `is_shared`. See design.md
    /// § "Part 5b: Concurrent Shared Types (`par struct`)".
    pub is_par: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub where_clause: Option<WhereClause>,
    pub variants: Vec<Variant>,
    /// See [`Function::stdlib_origin`]. CR-202 slice 3b.
    pub stdlib_origin: bool,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// `#[non_exhaustive]` declared on this enum — the type may grow
    /// new variants in future versions, and cross-package consumers'
    /// `match` expressions must include a wildcard arm regardless of
    /// which variants are covered today. Same-package matches still
    /// flag missing variants via the normal exhaustiveness rule.
    /// See design.md § `#[non_exhaustive]` for Evolvable Public Types.
    /// Resolver rejects the attribute on non-`pub` enums.
    pub is_non_exhaustive: bool,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub span: Span,
    /// Attributes attached to this enum variant. Enables item-level
    /// attributes like `#[deprecated]` and `#[diagnostic::*]` to
    /// attach at variant granularity per the design.md specs. Empty
    /// `Vec` when no attributes were attached.
    pub attributes: Vec<Attribute>,
    /// `#[deprecated]` payload captured at parse time. `None` when
    /// the attribute is absent. Mirrors the `deprecation` field on
    /// every item-kind that supports the attribute.
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// Joined contents of `///` doc comments preceding the variant.
    /// `None` when no doc comments were attached. CommonMark.
    pub doc_comment: Option<String>,
    pub name: String,
    pub kind: VariantKind,
    /// Explicit discriminant: `Variant = CONST_EXPR` (design.md § Explicit
    /// Discriminants on Payload Variants). `None` when the variant declares no
    /// value (`Audio,` — declaration-order-implicit), so the common case
    /// round-trips unchanged. Held as the parsed [`Expr`] (an integer literal,
    /// optionally negated); the typechecker folds it to an `i64` at the
    /// enum-decl site to run the all-or-nothing / range / duplicate / repr
    /// checks. A pure declaration — codegen does not treat it as a layout
    /// commitment at v1.
    pub discriminant: Option<Expr>,
}

#[derive(Debug, Clone)]
pub enum VariantKind {
    Unit,
    Tuple(Vec<TypeExpr>),
    Struct(Vec<StructField>),
}

// ── Traits ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TraitDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the trait.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    /// `trait Foo: Bar + Baz` — logical supertrait constraints.
    pub supertraits: Vec<TraitBound>,
    /// `trait Foo with reads(R)` — default effect ceiling for all methods.
    pub trait_effects: Option<EffectList>,
    pub where_clause: Option<WhereClause>,
    pub items: Vec<TraitItem>,
    /// See [`Function::stdlib_origin`]. CR-202 slice 3b.
    pub stdlib_origin: bool,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
    /// `#[diagnostic::on_unimplemented(...)]` payload — see
    /// [`OnUnimplemented`]. `None` when the trait carries no such
    /// attribute. Slice 3 of item 36 — populated at parse time;
    /// consumed by the failed-bound diagnostic builder at emit time
    /// (slice 6).
    pub on_unimplemented: Option<OnUnimplemented>,
}

#[derive(Debug, Clone)]
pub enum TraitItem {
    Method(Box<TraitMethod>),
    AssocType(Box<AssocTypeDecl>),
}

/// `trait NAME[GENERICS] = bound1 + bound2 + ... [where ...];`
/// (v60 item 40 / design.md § Trait Aliases). Parallel in shape to
/// `TraitDef` so resolver reuse is mechanical; bound substitution at
/// use sites is deferred to P1.
#[derive(Debug, Clone)]
pub struct TraitAliasDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub bounds: Vec<TraitBound>,
    pub where_clause: Option<WhereClause>,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

/// `marker trait NAME[GENERICS] [: SUPERTRAITS] [where ...] (";" | "{" "}")`
/// (v60 item 55 / design.md § Marker Traits). The body is structurally
/// empty — methods, associated types, and associated consts are
/// rejected at parse with a focused diagnostic. The `body_brace`
/// flag preserves whether the user wrote the empty-brace form
/// (`marker trait Foo { }`) or the canonical short form
/// (`marker trait Foo;`) for round-trip-faithful formatting.
#[derive(Debug, Clone)]
pub struct MarkerTraitDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub supertraits: Vec<TraitBound>,
    pub where_clause: Option<WhereClause>,
    /// `true` when the user wrote `marker trait Foo { }`; `false` when
    /// they wrote the canonical `marker trait Foo;`. Drives
    /// formatter round-trip; the resolver treats both forms identically.
    pub body_brace: bool,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

#[derive(Debug, Clone)]
pub struct AssocTypeDecl {
    pub span: Span,
    pub name: String,
    /// Optional `[P1, P2, ...]` parameter list for generic associated
    /// types (GATs). `type Mapped[U]` declares a GAT; `type Item`
    /// without a bracket list is the non-generic form. Effect-
    /// polymorphic GATs (`type Mapped[U, with E]`) are rejected at
    /// parse with `E_GAT_EFFECT_PARAM`; the surface stays type-only.
    pub generic_params: Option<GenericParams>,
    pub bounds: Vec<TraitBound>,
    /// Optional `where ...` clause attached to the GAT declaration.
    /// Constraints reference the GAT's own parameters and the
    /// enclosing trait's parameters.
    pub where_clause: Option<WhereClause>,
}

#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub span: Span,
    /// Attributes attached to this trait method declaration. Enables
    /// item-level attributes like `#[deprecated]`, `#[track_caller]`,
    /// `#[diagnostic::*]` to attach at trait-method granularity per
    /// the design.md specs. Per the spec, `#[track_caller]` on a
    /// trait method declaration applies to every impl unless the impl
    /// explicitly drops it (last-writer-wins propagation); the
    /// per-impl override lives on the `Function.is_track_caller`
    /// flag.
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the method.
    pub doc_comment: Option<String>,
    /// `unsafe fn ...` in a trait-method declaration. Mirrors the
    /// `Function.is_unsafe` precondition role: an impl satisfying this
    /// trait method must itself be `unsafe fn`, and every call site
    /// against the trait must be wrapped per `unsafe_op_in_unsafe_fn`.
    /// Slice 1 captures the surface marker only; the impl/trait
    /// coherence check that an `unsafe`-trait-method is implemented
    /// by an `unsafe fn` is part of the wider v2 epic.
    pub is_unsafe: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub self_param: Option<SelfParam>,
    /// Source span of the receiver tokens (`self`, `ref self`,
    /// `mut ref self`) when `self_param` is present. Lets diagnostics
    /// that fire on the receiver mode (E0412 resource-receiver
    /// contradiction) attach a machine-applicable edit replacing
    /// exactly the receiver text.
    pub self_span: Option<Span>,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub effects: Option<EffectList>,
    pub requires: Vec<Expr>,
    pub ensures: Vec<EnsuresClause>,
    pub where_clause: Option<WhereClause>,
    pub body: Option<Block>,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// `#[track_caller]` on this trait method declaration. Per
    /// design.md the attribute applies to every impl unless the impl
    /// explicitly drops it. Parsed here so the impl coherence pass
    /// can propagate the flag to impl methods (slice 4 codegen).
    pub is_track_caller: bool,
    /// Inlining-axis codegen hint on this trait method declaration —
    /// see [`Function::inline_hint`]. Propagates to every impl method
    /// that does not declare its own inline hint (last-writer-wins,
    /// parallels `#[track_caller]`); the propagation runs in
    /// [`crate::desugar::propagate_codegen_hints`].
    pub inline_hint: Option<InlineHint>,
    /// `#[cold]` on this trait method declaration — see
    /// [`Function::is_cold`]. Propagates to non-overriding impls.
    pub is_cold: bool,
    /// `#[gpu]` on this trait method declaration — see
    /// [`Function::is_gpu`]. Captured here so a `#[gpu]`-annotated trait
    /// method declares GPU-callable intent; FE-1 captures the surface
    /// marker only (parser rejects args; resolver rejects non-`fn`
    /// placement). Propagation to impl methods rides with the FE-3
    /// call-graph slice, paralleling `is_track_caller`.
    pub is_gpu: bool,
}

/// The inlining axis of the codegen-hint attributes (design.md §
/// Codegen Hint Attributes). The three values are mutually exclusive —
/// the parser rejects two inline-axis attributes on one function with
/// `E_INLINE_HINT_CONFLICT`. Orthogonal to the `#[cold]` hot/cold axis,
/// which lives in a separate `is_cold: bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineHint {
    /// `#[inline]` — a non-binding suggestion to inline (LLVM `inlinehint`).
    Default,
    /// `#[inline(always)]` — inline at every site where technically
    /// possible (LLVM `alwaysinline`).
    Always,
    /// `#[inline(never)]` — keep a real call frame at every site (LLVM
    /// `noinline`).
    Never,
}

// ── Impl Blocks ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ImplBlock {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    pub generic_params: Option<GenericParams>,
    pub trait_name: Option<PathExpr>,
    pub target_type: TypeExpr,
    pub where_clause: Option<WhereClause>,
    pub items: Vec<ImplItem>,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
    /// `#[diagnostic::do_not_recommend]` flag — slice 4 of item 36. The
    /// attribute is purely diagnostic: it tells the failed-bound
    /// diagnostic builder to skip this impl when iterating candidate
    /// impls for the "trait is implemented by …" note (slice 6). Has
    /// no effect on coherence, lookup, or trait resolution.
    pub do_not_recommend: bool,
}

#[derive(Debug, Clone)]
pub enum ImplItem {
    Method(Box<Function>),
    AssocType(Box<AssocTypeBinding>),
}

#[derive(Debug, Clone)]
pub struct AssocTypeBinding {
    pub span: Span,
    pub name: String,
    /// Optional `[P1, P2, ...]` parameter list mirroring the
    /// declaration shape. An impl binds the GAT with the same
    /// parameter list it was declared with: `type Mapped[U] = Vec[U]`.
    /// Effect-polymorphic forms (`type Mapped[U, with E] = ...`) are
    /// rejected at parse with `E_GAT_EFFECT_PARAM` for symmetry with
    /// the trait-side rejection.
    pub generic_params: Option<GenericParams>,
    pub ty: TypeExpr,
    /// Optional `where ...` clause attached to the GAT binding's
    /// right-hand side, mirroring the declaration shape.
    pub where_clause: Option<WhereClause>,
}

// ── Effect Declarations ──────────────────────────────────────────

/// One trait bound on an `effect resource` declaration — the `A` in
/// `effect resource R: A;`, or either of the two in `effect resource R: A + B;`
/// (design.md:7216).
#[derive(Debug, Clone)]
pub struct ProviderBound {
    /// The trait's bare name, which is what every consumer but the
    /// typechecker needs.
    pub name: String,
    /// Span of the trait identifier itself, so a diagnostic about the bound
    /// points at the bound rather than at the whole declaration.
    pub name_span: Span,
    /// `: Provider[Request]` — the bound's GENERIC ARGUMENTS (design.md:6071).
    /// `None` for a plain `: Trait` bound.
    ///
    /// These are not decoration. A generic provider trait declared without its
    /// argument is unusable, not merely imprecise: `effect resource RequestCh:
    /// Channel;` against `trait Channel[T]` parses, and then every
    /// `RequestCh.send(v)` fails with "expected 'T', found 'i64'" — naming a
    /// type parameter the user never wrote, because nothing ever bound it.
    /// `resource_dispatch_signature` substitutes these into the trait method's
    /// signature, which is the binding that diagnostic was missing
    /// (B-2026-08-18-41).
    pub args: Option<Vec<GenericArg>>,
}

#[derive(Debug, Clone)]
pub struct EffectResourceDecl {
    pub span: Span,
    pub name: String,
    /// `effect resource UserDB[user_id: i64];` — the PARTITION KEY, per
    /// design.md § Parameterized Resources. `None` for an unparameterized
    /// resource, which is every other declaration in the spec.
    ///
    /// This slot used to hold `Option<GenericParams>`, parsed and read by
    /// nobody but the formatter (B-2026-08-18-41). There is no such thing as a
    /// generic effect resource: the spec declares none, no phase consumed the
    /// field, and the only `[...]` written after a resource name anywhere in
    /// design.md is this key. Parsing it as type parameters is what made
    /// `[user_id: i64]` fail the Type-class naming rule.
    pub key_param: Option<ResourceKeyParam>,
    /// The declared provider trait bounds, in source order.
    ///
    /// EMPTY for a bare resource (`effect resource Latency;`). ONE entry for
    /// the common `effect resource UserDB: DatabaseProvider;`. TWO OR MORE for
    /// the multi-bound form design.md:7216 specifies normatively —
    /// `effect resource UserDB: DatabaseProvider + HealthCheckable;` — under
    /// "Multiple trait bounds are allowed on a resource declaration:" with
    /// semantics attached at :7217 ("Any provider passed to `with_provider`
    /// must implement all declared bounds plus `Send + Sync`").
    ///
    /// This was three parallel `Option` fields (`provider_trait`,
    /// `provider_trait_args`, `provider_trait_span`) until B-2026-08-19-3. A
    /// `Vec` rather than "first bound plus extras" because NO consumer wants
    /// only the first: the resolver existence-checks each, the effect checker
    /// seeds the union of their methods, the typechecker checks each bound's
    /// generic arity and that the provider implements every one, and codegen
    /// lays their methods out end-to-end in one vtable.
    pub provider_bounds: Vec<ProviderBound>,
    /// Phase-10 target gate: when this declaration is an alias-renamed
    /// clone of a gated-stdlib HOST resource (`import std.web.Display
    /// as Screen;` in single-file mode), the canonical host name
    /// (`"Display"`). The target gate keys its provided-resource table
    /// on the canonical name so renames cannot evade the gate. `None`
    /// for ordinary declarations.
    pub canonical_host_name: Option<String>,
}

/// The partition key of a parameterized `effect resource`
/// (design.md § Parameterized Resources).
///
/// The USE side of this feature has been live all along —
/// `Resource::param` carries `writes(UserDB[id])`, and
/// `apply_parameterized_keys` proves literal keys distinct so
/// `update(42)` / `update(99)` parallelize while `update(42)` /
/// `update(42)` serialize. Only the DECLARATION had no syntax.
#[derive(Debug, Clone)]
pub struct ResourceKeyParam {
    /// The key's name (`user_id`). Value-class, like any binding.
    pub name: String,
    pub name_span: Span,
    /// The key's declared type (`i64`).
    pub ty: TypeExpr,
    /// The whole `[name: Type]`, for a diagnostic that wants to point at it.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EffectGroupDecl {
    pub span: Span,
    pub is_pub: bool,
    pub is_stable: bool,
    pub name: String,
    pub body: Vec<EffectGroupTerm>,
}

#[derive(Debug, Clone)]
pub enum EffectGroupTerm {
    Verb(EffectVerb),
    GroupRef(String),
}

#[derive(Debug, Clone)]
pub struct EffectVerbDecl {
    pub span: Span,
    pub is_pub: bool,
    pub is_transparent: bool,
    pub verb_name: String,
}

// ── Effect Annotations ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EffectList {
    pub items: Vec<EffectItem>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum EffectItem {
    Verb(EffectVerb),
    Group(String),
    Polymorphic,      // with _
    Variable(String), // with E  (named effect variable declared in [with E])
}

#[derive(Debug, Clone)]
pub struct EffectVerb {
    pub kind: EffectVerbKind,
    pub resources: Vec<Resource>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EffectVerbKind {
    Reads,
    Writes,
    Sends,
    Receives,
    Allocates,
    Panics,
    Blocks,
    Suspends,
    UserDefined(String),
}

#[derive(Debug, Clone)]
pub struct Resource {
    pub path: Vec<String>,
    pub param: Option<Box<Expr>>,
    pub span: Span,
}

// ── Layout ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct LayoutDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the layout.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub name: String,
    pub collection_type: TypeExpr,
    pub items: Vec<LayoutItem>,
}

#[derive(Debug, Clone)]
pub enum LayoutItem {
    Group {
        name: String,
        fields: Vec<String>,
        /// Optional `align(N)` modifier — N must be a power of two.
        align: Option<u32>,
        span: Span,
    },
    Cold {
        fields: Vec<String>,
        span: Span,
    },
    SplitByVariant(Span),
}

// ── Module & Import ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UseDecl {
    pub span: Span,
    pub is_pub: bool,
    pub path: Vec<String>,
}

/// New module-system import (CR-24). Replaces `UseDecl` once the parser emits
/// `import` syntax in slice 5. Supports brace-grouped multi-item imports and
/// per-item `as` renames:
///
/// ```text
/// import db.connection.Connection;
/// import db.connection.{Connection, Pool as P};
/// pub import db.auth.Token;
/// ```
///
/// `path` is the module prefix (dotted path, no item), and `items` lists the
/// names being imported from that module. A bare `import path.Item;` maps to
/// a single-element `items` vector.
///
/// **Wildcard** (`import path.*;`) and **nested grouping**
/// (`import a.{b.{c, d}, e};`) are both v1 forms (design.md § Module System).
/// Nested grouping is a pure parse-time desugar: one source statement becomes
/// one `ImportDecl` per distinct dotted prefix, so every downstream pass sees
/// only flat imports. A wildcard sets [`ImportDecl::is_wildcard`] and parses
/// with an EMPTY `items` vector; `module::expand_wildcard_imports` fills that
/// vector in with the target module's accessible names once the program tree
/// exists, so — again — every downstream pass sees only flat imports.
#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub span: Span,
    pub is_pub: bool,
    pub path: Vec<String>,
    /// Per-segment spans for `path` (same length, same order). Lets resolver
    /// diagnostics target a precise sub-range of the import statement when a
    /// prefix segment is misspelled — e.g., `import grret.foo;` gets a
    /// machine-applicable replacement covering only `grret.foo`, not the
    /// entire import statement.
    pub path_spans: Vec<Span>,
    pub items: Vec<ImportItem>,
    /// `import path.*;` — bring every accessible item of the module named by
    /// `path` into scope (design.md § Module System > Wildcard imports). The
    /// parser leaves `items` empty; `module::expand_wildcard_imports` fills it
    /// with the concrete names before resolution, applying the spec's three
    /// precedence rules. A decl with `is_wildcard` set and `items` still empty
    /// therefore means either "nothing accessible to import" or single-file
    /// mode (no program tree to expand against) — never "drop me", which is
    /// why the empty-items filters in `module.rs` / `prelude.rs` check it.
    pub is_wildcard: bool,
}

#[derive(Debug, Clone)]
pub struct ImportItem {
    pub span: Span,
    pub name: String,
    pub alias: Option<String>,
}

// ── Constants ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub span: Span,
    /// Attributes attached to this module-level const. Enables
    /// item-level attributes like `#[deprecated]` to attach per
    /// design.md § `#[deprecated]` for Item Deprecation.
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the const.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub ty: TypeExpr,
    pub value: Expr,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

// ── Module-Level Bindings ────────────────────────────────────────

/// `[ATTRIBUTES] [VIS] let [mut] NAME[: TYPE] = INIT;` payload —
/// module-level binding declaration. See [`Item::ModuleBinding`].
///
/// Field layout mirrors [`ConstDecl`] with two structural differences:
/// `is_mut` distinguishes `let` (immutable) from `let mut` (mutable),
/// and `ty` is optional because the type annotation can be elided when
/// the initializer's type is unambiguous (final-form inference rule
/// lands in slice 5; slice 1 only carries the optional through parse).
#[derive(Debug, Clone)]
pub struct ModuleBinding {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined `///` doc-comment text preceding the binding.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub is_mut: bool,
    pub name: String,
    /// Span of the `name` identifier token alone (not the whole `let … = …;`
    /// statement, which is `span`). Lets the resolver's Const-class naming
    /// diagnostic (`E_MODULE_BINDING_NAMING`) attach a machine-applicable
    /// rename edit spanning exactly the identifier — B-2026-07-06-3.
    pub name_span: Span,
    /// Optional `: TYPE` annotation. `None` when elided.
    pub ty: Option<TypeExpr>,
    pub value: Expr,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// See [`Function::lint_overrides`].
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

// ── Test cases ───────────────────────────────────────────────────

/// `test "case name" { body }` — see `Item::TestCase`.
#[derive(Debug, Clone)]
pub struct TestCase {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined `///` doc-comment text preceding the case. Carried for
    /// shape uniformity with other items; the test runner does not
    /// surface it today.
    pub doc_comment: Option<String>,
    /// User-visible case name (the string literal between `test` and
    /// `{`). Surfaced verbatim as the `test` field on every
    /// `test_pass` / `test_fail` / `test_skip` JSONL event and
    /// matched by `--filter`. Escape sequences inside the source
    /// `"..."` literal are decoded by the lexer, so this string is
    /// the post-escape form (e.g. `\"` in source becomes a literal
    /// `"` here).
    pub name: String,
    /// Span of the string literal itself (excludes the surrounding
    /// `test` keyword token and the `{` body opener). Used by the
    /// diagnostic pass that points at the case-name when reporting
    /// duplicate names or unprintable characters in the name.
    pub name_span: Span,
    pub body: Block,
}

// ── Alias & Independent ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AliasDecl {
    pub span: Span,
    pub left: Vec<String>,
    pub right: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IndependentDecl {
    pub span: Span,
    pub left: Vec<String>,
    pub right: Vec<String>,
}

// ── Extern Functions (FFI) ───────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExternFunction {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the extern.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub abi: String,
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub effects: Option<EffectList>,
}

/// `unsafe extern "ABI" { ... }` block — see [`Item::ExternBlock`].
#[derive(Debug, Clone)]
pub struct ExternBlock {
    pub span: Span,
    /// Block-level attributes (e.g. block-scoped `@noblock`). Held
    /// here at the block level and NOT pre-merged into per-item
    /// `attributes` — formatter round-trip needs to preserve which
    /// attributes were authored block-level vs. per-item. Downstream
    /// consumers (effectchecker, codegen) that need the effective union
    /// for a given item take both sets explicitly.
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc-comments preceding the block. The
    /// `undocumented_unsafe` lint reads this to enforce a `# Safety`
    /// section at the block level.
    pub doc_comment: Option<String>,
    pub abi: String,
    pub items: Vec<ExternItem>,
}

#[derive(Debug, Clone)]
pub enum ExternItem {
    /// Boxed to keep the `ExternItem` enum compact: `ExternFunction`
    /// is ~344 bytes (params, return type, effects, attributes) while
    /// `OpaqueTypeDecl` is ~112 bytes — boxing the larger variant
    /// keeps the discriminated union shrinkwrap to one pointer per
    /// item without churning size on the rare `OpaqueType` arm.
    Function(Box<ExternFunction>),
    /// Opaque foreign type declaration: `type Name;` inside an
    /// `unsafe extern "ABI" { ... }` block. Names a C-side type whose
    /// layout is unknown to Kāra (`FILE`, `xmlNode`, `sqlite3`, etc.) —
    /// see [`OpaqueTypeDecl`] and design.md § FFI > Opaque Foreign Types.
    OpaqueType(OpaqueTypeDecl),
}

/// `type Name;` declaration inside an `unsafe extern "ABI" { ... }` block.
/// Carries no fields, no methods, no derives, no body — the type's
/// layout is private to the foreign library. The Kāra side may only
/// reference it behind a pointer (`*const`/`*mut`) or reference
/// (`ref`/`mut ref`); by-value uses are rejected at typecheck.
#[derive(Debug, Clone)]
pub struct OpaqueTypeDecl {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the decl.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
}

// ── Type Aliases ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TypeAliasDef {
    pub span: Span,
    /// Attributes attached to this type alias. Enables item-level
    /// attributes like `#[deprecated]` to attach per design.md §
    /// `#[deprecated]` for Item Deprecation.
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the alias.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub ty: TypeExpr,
    pub refinement: Option<Expr>,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

// ── Distinct Types (Newtypes) ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DistinctTypeDef {
    pub span: Span,
    pub attributes: Vec<Attribute>,
    /// Joined contents of `///` doc comments preceding the distinct type.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub base_type: TypeExpr,
    pub refinement: Option<Expr>,
    /// `#[deprecated]` payload — see [`Deprecation`].
    pub deprecation: Option<Deprecation>,
    /// `#[unstable]` payload — see [`Unstable`] and design.md §
    /// v1 Positioning > Stable surface vs. unstable extension points.
    /// `None` when the attribute is absent. Use-site `unstable_api`
    /// lint emission reads this through the resolver's symbol table.
    pub unstable: Option<Unstable>,
    /// See [`Function::lint_overrides`]. Slice-4a broadens attachment.
    pub lint_overrides: Vec<crate::lints::LintLevelOverride>,
}

/// B-2026-07-01-7 passthrough analysis — whether `f` can RETURN its
/// positional parameter `arg_index` (its body has a tail expression or a
/// `return` statement that is exactly that parameter's bare identifier).
/// Conservative toward `true`: when ANY return site passes the parameter
/// through, the caller-side temp-drop registration for an argument in that
/// slot is skipped — the value flows out to the caller's consumer of the
/// RESULT, whose own binding/temp drop covers it. A `false` means every
/// return site provably yields something else, so the argument dies inside
/// the call and the caller-side temp drop is correct. (Cost of the
/// conservative `true` on mixed-path fns: the non-passthrough paths' arg
/// drop side effect is skipped — a leak-of-side-effect, never a
/// double-drop.) Shared by codegen's `track_inline_owned_aggregate_arg`
/// gate and the interpreter's `run_fresh_temp_arg_drops` gate so both
/// surfaces agree.
pub fn fn_returns_param(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return false;
    };
    /// A return site "yields" the param when it IS the bare identifier, or
    /// (B-2026-08-02-23 leg 2) when it is an AGGREGATE LITERAL that moves the
    /// param into itself — `Holder { xs: v, tag: 9 }`, `(v, 9)`, or either
    /// nested. The value crosses the frame boundary inside the returned
    /// aggregate exactly as it does when returned bare, so the caller's
    /// consumer of the RESULT is the owner either way; recognizing only the
    /// bare form left the caller dropping an arg it had just been handed back
    /// inside a struct, firing the Drop body twice.
    fn expr_is_ident(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => n == name,
            ExprKind::StructLiteral { fields, .. } => {
                fields.iter().any(|f| expr_is_ident(&f.value, name))
            }
            ExprKind::Tuple(elems) => elems.iter().any(|el| expr_is_ident(el, name)),
            _ => false,
        }
    }
    fn walk_expr(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Return(Some(inner)) => expr_is_ident(inner, name) || walk_expr(inner, name),
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b, name),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk_expr(condition, name)
                    || walk_block(then_block, name)
                    || else_branch.as_deref().is_some_and(|x| walk_expr(x, name))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                walk_expr(value, name)
                    || walk_block(then_block, name)
                    || else_branch.as_deref().is_some_and(|x| walk_expr(x, name))
            }
            ExprKind::Match { scrutinee, arms } => {
                walk_expr(scrutinee, name)
                    || arms.iter().any(|a| {
                        // An arm TAIL that is the bare param is a return site
                        // when the match is itself a tail — conservative: any
                        // bare-param arm tail counts.
                        expr_is_ident(&a.body, name) || walk_expr(&a.body, name)
                    })
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body, name),
            _ => false,
        }
    }
    fn walk_block(b: &Block, name: &str) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => walk_expr(e, name),
            _ => false,
        }) || b
            .final_expr
            .as_deref()
            .is_some_and(|fe| expr_is_ident(fe, name) || walk_expr(fe, name))
    }
    walk_block(&f.body, param_name)
}

/// B-2026-08-28-70 — does `f` hand parameter `arg_index` back to its caller on
/// EVERY exit?
///
/// The complement of [`fn_conditionally_returns_param_bare`], and the two are
/// used together: that one asks "escapes on SOME path" (so the callee owns the
/// body behind a per-path flag), this asks "escapes on ALL paths" (so the
/// CALLER's result binding owns it outright and the caller must not fire).
///
/// Deliberately NOT [`fn_returns_param`], which is the UNION over return sites
/// and therefore answers true for a param that escapes on one path and dies on
/// another. Standing a caller down on that union is exactly the trade
/// B-2026-08-28-22 was filed for: measured on the method path, reusing it lost
/// `impl B4 { fn early(ref self, r: R, k: bool) -> R { if k { return R { id: 98 }; } r } }`'s
/// body for `r` when `k` was true, on all three compiled backends, where the
/// pre-existing behaviour had it right. "Escapes somewhere" is not "someone
/// else owns it".
///
/// A body WITH a tail expression is admitted on two conditions, both required:
///
///  1. every LEAF TAIL of the body yields the param, and
///  2. every `return` the walker can see yields it too.
///
/// A body with NO tail expression exits only through `return`, so condition 1
/// has nothing to range over. It is admitted instead when the function declares
/// a return type, at least one `return` yields the param, and condition 2 still
/// holds. That arm is B-2026-08-29-14: it previously declined, and since the
/// `return` channel (`fn_conditionally_returns_param_bare`) declines `return`
/// statements outright, a `return`-only callee was claimed by neither predicate
/// and ran its param's `Drop` body twice under codegen.
///
/// "Yields" matches [`fn_returns_param`]'s own notion — the bare identifier, or
/// an aggregate literal that moves the param into itself (`H { r: r }`,
/// `(r, 9)`), since the value crosses the frame boundary inside the aggregate
/// exactly as it does bare.
///
/// COVERAGE LIMIT, stated rather than hidden: the `return` walk mirrors
/// [`fn_returns_param`]'s traversal, so a `return` buried somewhere that
/// traversal does not descend into (inside a call argument, say) is invisible
/// here as it is there. Condition 1 is what keeps that from mattering in
/// practice — it already requires the body's tail to BE the param or an
/// aggregate around it, so admitted bodies are structurally narrow. Every
/// decline is the pre-existing behaviour, so the failure direction of a miss is
/// a body that keeps firing where it always did, never a new silent loss.
///
/// B-2026-08-29-50 — this predicate was briefly SPLIT, with a strict
/// `fn_always_returns_param_bare` sibling that dropped the aggregate-literal
/// recursion from `yields`, so that only a BARE hand-back could stand the
/// caller's own `Drop` body down. That split has been removed: it rested on a
/// measurement that no longer describes the code. The 3-byte definite leak it
/// cited was produced by RETRACTING the caller's whole `karac_drop_<T>` action,
/// and the stand-down now DOWNGRADES that action instead
/// (`suppress_user_drop_body_keeping_memory`), which keeps the free. Re-measured
/// with the downgrade in place, `fn wrapf(r: Res) -> Hh { Hh { r: r } }` over a
/// named argument is `13 allocs / 13 frees, 0 errors` under valgrind and clean
/// under ASAN+LSan — identical to the bare spelling's `13 / 13` — while the
/// second `Drop` body disappears. The aggregate's two owners turned out to be
/// PARALLEL, not nested as feared: passing an own-heap struct by value already
/// deep-copies it (`10,277` vs `8,229` bytes against an inline-construction
/// oracle, with and without a `Drop` impl alike), so the caller's slot holds a
/// distinct buffer that its downgraded field cleanup still frees.
pub fn fn_always_returns_param(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(name) = &param.pattern.kind else {
        return false;
    };
    let name = name.as_str();

    /// The same test [`fn_returns_param`] applies at a return site: the bare
    /// identifier, or an aggregate literal that moves the param into itself.
    fn yields(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => n == name,
            ExprKind::StructLiteral { fields, .. } => fields.iter().any(|f| yields(&f.value, name)),
            ExprKind::Tuple(elems) => elems.iter().any(|el| yields(el, name)),
            _ => false,
        }
    }
    fn leaf_tails<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
        match &e.kind {
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
                match &then_block.final_expr {
                    Some(t) => leaf_tails(t, out),
                    None => out.push(e),
                }
                match else_branch {
                    Some(x) => leaf_tails(x, out),
                    None => out.push(e),
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    leaf_tails(&arm.body, out);
                }
            }
            ExprKind::Block(b) => match &b.final_expr {
                Some(t) => leaf_tails(t, out),
                None => out.push(e),
            },
            // B-2026-08-29-65 — a tail `return x` is a RETURN SITE, not a leaf
            // tail value. Reached here as the node itself it satisfied nothing,
            // so condition 1 failed and the predicate declined for a body whose
            // every exit hands the param back:
            // `fn f(r: R) -> R { println("m"); return r }` ran the `Drop` body
            // TWICE on every backend while the same function written
            // `return r;` — which has no tail at all and takes the no-tail arm
            // below — ran it once. The semicolon was the whole difference.
            // A bare `return;` still pushes the node and so still declines,
            // which is correct: it yields nothing.
            ExprKind::Return(inner) => match inner.as_deref() {
                Some(x) => leaf_tails(x, out),
                None => out.push(e),
            },
            _ => out.push(e),
        }
    }
    /// Collect the operand of every `return` the walker can see. `None` marks a
    /// bare `return;`, which yields nothing.
    ///
    /// One collector rather than a pair of mirrored predicates, because the
    /// no-tail arm below needs BOTH facts about the same set ("is any return
    /// bad" and "is any return good") and two traversals that must stay in
    /// lockstep are two traversals that can drift apart.
    ///
    /// Deliberately does NOT descend into a closure body: a `return` there
    /// returns from the closure, not from `f`.
    fn return_operands<'a>(e: &'a Expr, out: &mut Vec<Option<&'a Expr>>) {
        match &e.kind {
            ExprKind::Return(inner) => {
                out.push(inner.as_deref());
                if let Some(x) = inner.as_deref() {
                    return_operands(x, out);
                }
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => return_operands_block(b, out),
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
                return_operands_block(then_block, out);
                if let Some(x) = else_branch.as_deref() {
                    return_operands(x, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    return_operands(&a.body, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => return_operands_block(body, out),
            _ => {}
        }
    }
    fn return_operands_block<'a>(b: &'a Block, out: &mut Vec<Option<&'a Expr>>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) | StmtKind::Let { value: e, .. } => return_operands(e, out),
                // A `let ... else { return; }` hides the most common bare
                // `return` in the language behind a statement kind the older
                // walk did not visit. So do the assignment forms and the
                // deferred blocks: each can carry a `return` that decides
                // whether the param still belongs to this frame.
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    return_operands(value, out);
                    return_operands_block(else_block, out);
                }
                StmtKind::Assign { target, value } => {
                    return_operands(target, out);
                    return_operands(value, out);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    return_operands(target, out);
                    return_operands(value, out);
                }
                StmtKind::MultiAssign { targets, values } => {
                    for e in targets.iter().chain(values.iter()) {
                        return_operands(e, out);
                    }
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    return_operands_block(body, out)
                }
                StmtKind::LetUninit { .. } => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            return_operands(fe, out);
        }
    }

    let mut returns = Vec::new();
    return_operands_block(&f.body, &mut returns);
    // Is there a `return` that does NOT hand the param back? A bare `return;`
    // counts: it exits without yielding, so the param dies on that path.
    let any_bad_return = returns.iter().any(|o| !o.is_some_and(|x| yields(x, name)));

    let Some(tail) = f.body.final_expr.as_deref() else {
        // NO TAIL EXPRESSION AT ALL — every exit is a `return` (B-2026-08-29-14).
        //
        // This arm used to decline, on the reasoning that such a body was "left
        // to the `return` channel". It is not: that channel is
        // `fn_conditionally_returns_param_bare`, which declines `return`
        // statements outright, so a `return`-only callee was admitted by
        // NEITHER predicate and its caller kept firing alongside the result
        // binding. Measured on `fn take(ref self, r: Res) -> Res { return r; }`
        // with a fresh-temp argument: two `Drop` bodies under all three
        // compiled backends against one interpreter body, while the BLOCK-TAIL
        // spelling of the identical method and both free-function twins ran one.
        //
        // Safe to claim here for the same reason condition 2 is safe on the tail
        // path: `any_bad_return` has already ruled out every visible exit that
        // does not hand the param back. The extra requirement is a return that
        // DOES hand it back — without it a body that never returns at all (a
        // bare `loop {}`) would be claimed as "always returns the param" on the
        // strength of having no counter-example.
        //
        // A function with no declared return type is excluded outright: it has
        // nothing to hand the param back THROUGH, so its param dies inside and
        // the caller must keep firing.
        let any_good_return = returns.iter().any(|o| o.is_some_and(|x| yields(x, name)));
        return f.return_type.is_some() && any_good_return && !any_bad_return;
    };
    let mut tails = Vec::new();
    leaf_tails(tail, &mut tails);
    if tails.is_empty() || !tails.iter().all(|t| yields(t, name)) {
        return false;
    }
    !any_bad_return
}

/// B-2026-08-28-22 — is `f`'s positional parameter `arg_index` returned on SOME
/// tail paths and not others, by a route the conditional-move drop flag can
/// actually clear?
///
/// [`fn_returns_param`] answers over the UNION of return sites, which is what
/// makes it conservative-true on a branchy callee: the caller then suppresses
/// its side of the drop on EVERY path, and whichever value actually died inside
/// the call loses its user `Drop` body. This predicate identifies the subset of
/// that shape where the callee can take ownership back safely, so the fix is a
/// callee-local ownership flip guarded by B-2026-08-28-51's per-path flag
/// rather than a change to the union answer (which would restore the DOUBLE
/// body the union was chosen to avoid).
///
/// TRUE requires all four, and each one is load-bearing:
///
///   1. Some leaf tail of the body's tail expression IS the bare parameter.
///      Bare because `arm_conditional_move_tail_flag` (codegen) and
///      `record_conditional_move_tail` (interpreter) both key on
///      `ExprKind::Identifier`; they are what clear the flag on the escaping
///      path, so an escape they cannot see would leave the callee dropping a
///      value that left the frame.
///   2. Some OTHER leaf tail does not mention the parameter at all — the
///      conditionality this row is about. An unconditionally-returned param has
///      no missed body to recover, so registering one is pure risk.
///   3. No leaf tail mentions the parameter in any OTHER way. This is the one
///      that rules out the aggregate-literal route `if k { H { r: r } } else
///      { .. }`, which [`fn_returns_param`] counts as an escape (via its
///      `expr_is_ident` recursion into struct/tuple literals) but the flag
///      never clears. Measured with that route admitted: `drop 41` / `41` /
///      `drop 41` on all three compiled backends — a double body plus a read of
///      the dropped value, exactly the defect the union answer exists to
///      prevent.
///   4. No `return` statement anywhere in the body mentions the parameter.
///      Codegen clears the flag at match arms and block tails but NOT at a
///      `return` operand (it suppresses those statically instead), so a
///      `return`-borne escape is outside this mechanism's reach. That shape is
///      B-2026-08-28-65 / -52 and is deliberately left to them.
///
/// Paired with `!`[`fn_moves_param_into_outliving_place`] at the call site: a
/// param stored into `self` or a `ref` param outlives the frame by a route with
/// no tail at all.
///
/// The conservative direction is unchanged from the rest of this family — a
/// shape this predicate declines keeps today's missed body, which is a
/// leak-of-side-effect, never a double drop and never a memory fault.
pub fn fn_conditionally_returns_param_bare(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return false;
    }
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return false;
    };
    let name = param_name.as_str();

    /// May `e` mention `name`? Conservative in the DECLINING direction: any
    /// shape not explicitly recognized answers `true`, which fails condition 3
    /// and leaves that function on today's behaviour. Adding a shape here can
    /// only ever admit more programs, never silently widen an escape route.
    fn may_mention(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => n == name,
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::Bool(_) => false,
            ExprKind::InterpolatedStringLit(parts) => parts.iter().any(|p| match p {
                crate::ast::ParsedInterpolationPart::Text(_) => false,
                crate::ast::ParsedInterpolationPart::Expr(e, _) => may_mention(e, name),
            }),
            // B-2026-08-29-50 — the RECEIVER is never one of `f.params`, so
            // `self` cannot be the parameter this is asking about. Without
            // this the catch-all below answered `true` and a method whose
            // non-param exit merely INTERPOLATES a field of self
            // (`return R { name: f"z{self.n}" }`) was declined, which left it
            // running two bodies on the hand-back path and — because codegen
            // reaches its own stand-down by a different route — one body
            // compiled against two interpreted on the dies-inside path. That
            // run-vs-build split predates this row and is measured on it.
            ExprKind::SelfValue => false,
            ExprKind::StructLiteral { fields, .. } => {
                fields.iter().any(|f| may_mention(&f.value, name))
            }
            ExprKind::Tuple(elems) => elems.iter().any(|el| may_mention(el, name)),
            ExprKind::Binary { left, right, .. } => {
                may_mention(left, name) || may_mention(right, name)
            }
            ExprKind::Unary { operand, .. } => may_mention(operand, name),
            ExprKind::FieldAccess { object, .. } => may_mention(object, name),
            _ => true,
        }
    }
    fn is_bare(e: &Expr, name: &str) -> bool {
        matches!(&e.kind, ExprKind::Identifier(n) if n == name)
    }
    /// The leaf tails of an escaping tail position, following exactly the
    /// branch structure `note_escaping_site` pushes escaping-ness down through.
    /// A branch arm with no tail expression contributes the branch expression
    /// itself, which `may_mention` then answers `true` for — declining rather
    /// than guessing what a tail-less arm does.
    fn leaf_tails<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
        match &e.kind {
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
                match &then_block.final_expr {
                    Some(t) => leaf_tails(t, out),
                    None => out.push(e),
                }
                match else_branch {
                    Some(x) => leaf_tails(x, out),
                    None => out.push(e),
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    leaf_tails(&arm.body, out);
                }
            }
            ExprKind::Block(b) => match &b.final_expr {
                Some(t) => leaf_tails(t, out),
                None => out.push(e),
            },
            _ => out.push(e),
        }
    }
    /// Condition 4 — collect every `return` operand as an additional EXIT
    /// leaf, so the `return` spelling of a conditional hand-back is analysed
    /// exactly like the block-tail one. Mirrors [`fn_returns_param`]'s
    /// traversal so the two agree on where a return site can appear.
    fn collect_return_leaves<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                leaf_tails(inner, out);
                collect_return_leaves(inner, out);
            }
            ExprKind::Return(None) => {}
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => collect_return_leaves_block(b, out),
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
                collect_return_leaves_block(then_block, out);
                if let Some(x) = else_branch.as_deref() {
                    collect_return_leaves(x, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    collect_return_leaves(&a.body, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => collect_return_leaves_block(body, out),
            _ => {}
        }
    }
    fn collect_return_leaves_block<'a>(b: &'a Block, out: &mut Vec<&'a Expr>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => collect_return_leaves(e, out),
                StmtKind::Let { value, .. } => collect_return_leaves(value, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            collect_return_leaves(fe, out);
        }
    }

    // Every EXIT leaf: the body tail's leaves, plus each `return` operand's.
    // A function whose exits are ALL `return` statements has no tail at all,
    // which is why the tail is optional here rather than required.
    let mut leaves = Vec::new();
    if let Some(tail) = f.body.final_expr.as_deref() {
        leaf_tails(tail, &mut leaves);
    }
    collect_return_leaves_block(&f.body, &mut leaves);
    // A single leaf is the unconditional shape — no branch, nothing to guard.
    if leaves.len() < 2 {
        return false;
    }
    let mut yields_bare = false;
    let mut yields_nothing = false;
    for leaf in leaves {
        if is_bare(leaf, name) {
            yields_bare = true;
        } else if may_mention(leaf, name) {
            // Condition 3 — an escape route the flag cannot clear.
            return false;
        } else {
            yields_nothing = true;
        }
    }
    yields_bare && yields_nothing
}

/// B-2026-08-28-62 — the THIRD escape route for a by-value parameter: `f` hands
/// it to ANOTHER CALL whose result is returned.
///
/// Kara passes a by-value argument under a caller-drops convention, and the
/// caller declines to drop only where it can see the value leaving. Two routes
/// were modelled: [`fn_returns_param`] (the param returned bare, or moved into a
/// returned aggregate literal) and `fn_moves_param_into_outliving_place`
/// (B-2026-08-26-9, stored into `self` or a `ref` param). `fn outer(y: R) -> …
/// { return src(y); }` is neither, so the caller fired `y`'s `Drop` body while
/// the value was still travelling out through `src`'s return — measured at two
/// bodies for one object on all three backends, generic and non-generic alike.
///
/// PROGRAM-AWARE because the question is genuinely interprocedural: passing the
/// param to a call proves nothing on its own — `fn outer(y: R) -> i64 { return
/// uses(y); }` consumes it — so the callee's OWN answer decides. That makes this
/// a separate entry point rather than a widening of `fn_returns_param`, whose
/// eight codegen ownership consumers are documented as turning a leak into
/// corruption if its answer moves; only the caller-side Drop-BODY walks ask this
/// one.
///
/// ONE LEVEL, and the argument must be the param BARE. Both are the
/// conservative direction this family runs on: a MISSED escape keeps today's
/// double body, a FALSE one suppresses the only body that runs. A two-hop chain
/// (`a` forwards to `b` forwards to a return) is therefore not recognized, and
/// neither is a param buried inside an aggregate ARGUMENT — the callee may
/// consume that aggregate whole.
pub fn fn_returns_param_via_call(program: &crate::Program, f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return false;
    };

    /// Does `e`, at a return site, hand `name` to a call that gives it back?
    /// Recurses through a returned aggregate LITERAL for the same reason
    /// `fn_returns_param`'s own `expr_is_ident` does — the value crosses the
    /// frame boundary inside it exactly as it would bare.
    fn yields_via_call(program: &crate::Program, e: &Expr, name: &str, self_name: &str) -> bool {
        match &e.kind {
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .any(|fi| yields_via_call(program, &fi.value, name, self_name)),
            ExprKind::Tuple(elems) => elems
                .iter()
                .any(|el| yields_via_call(program, el, name, self_name)),
            ExprKind::Call { callee, args, .. } => {
                let ExprKind::Identifier(g) = &callee.kind else {
                    return false;
                };
                // Self-recursion would ask the same question of the same body;
                // one level means one level.
                if g == self_name {
                    return false;
                }
                let Some(gf) = program.items.iter().find_map(|item| match item {
                    Item::Function(gf) if &gf.name == g => Some(gf),
                    _ => None,
                }) else {
                    return false;
                };
                args.iter().enumerate().any(|(j, a)| {
                    matches!(&a.value.kind, ExprKind::Identifier(n) if n == name)
                        && fn_returns_param(gf, j)
                })
            }
            _ => false,
        }
    }

    fn walk_expr(program: &crate::Program, e: &Expr, name: &str, self_name: &str) -> bool {
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                yields_via_call(program, inner, name, self_name)
                    || walk_expr(program, inner, name, self_name)
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(program, b, name, self_name),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                walk_block(program, then_block, name, self_name)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk_expr(program, x, name, self_name))
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                walk_block(program, then_block, name, self_name)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk_expr(program, x, name, self_name))
            }
            ExprKind::Match { arms, .. } => arms.iter().any(|a| {
                yields_via_call(program, &a.body, name, self_name)
                    || walk_expr(program, &a.body, name, self_name)
            }),
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(program, body, name, self_name),
            _ => false,
        }
    }

    fn walk_block(program: &crate::Program, b: &Block, name: &str, self_name: &str) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => walk_expr(program, e, name, self_name),
            _ => false,
        }) || b.final_expr.as_deref().is_some_and(|fe| {
            yields_via_call(program, fe, name, self_name) || walk_expr(program, fe, name, self_name)
        })
    }

    walk_block(program, &f.body, param_name, &f.name)
}

/// A PATH from an owned aggregate parameter to one of its parts — empty for
/// the whole param, one element per level in. See
/// [`fn_returns_param_part_paths`].
pub type ParamPath = Vec<ParamPart>;

/// One top-level PART of an owned aggregate parameter — a tuple element or a
/// struct field. See [`fn_returns_param_part_paths`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamPart {
    /// Tuple element `p.<n>`, or leaf `<n>` of a one-level tuple destructure.
    TupleIndex(usize),
    /// Struct field `p.<name>`, or the leaf a one-level struct pattern binds
    /// from field `<name>`.
    Field(String),
}

/// B-2026-08-28-2 — the PART-PRECISE sibling of [`fn_returns_param`]: WHICH
/// parts of owned aggregate parameter `arg_index` can reach a return site, each
/// as a PATH from the parameter inwards.
///
/// [`fn_returns_param`] answers a WHOLE-param question — it recognizes the
/// param used bare at a return site, or moved bare into a returned aggregate
/// literal, and nothing finer. That is the right question for its own callers
/// and it is deliberately left untouched here: eight codegen ownership sites
/// read it, several of them documented as turning a leak into corruption if
/// its answer moves.
///
/// The caller-side fresh-temp TUPLE walk needs a finer answer. It fires the
/// user `Drop` bodies of every element of a tuple-literal argument, on the
/// theory that the whole temp dies inside the call. When the callee extracts
/// one element and returns it, that element's owner is the caller's consumer
/// of the RESULT, and firing here runs its body a second time:
///
/// ```text
/// fn take(p: (R, i64)) -> R { let (r, n) = p; r }
/// let x = take((R { id: 41 }, 1));   // `drop 41` twice, all backends
/// ```
///
/// Suppressing the whole walk is NOT the fix. Measured on the two-dropper
/// shape `fn take(p: (R, R)) -> R { let (a, b) = p; a }`, blanket suppression
/// trades a double body on element 0 for a MISSING body on element 1 — one
/// soundness hole for another. Hence per-part.
///
/// Routes recognized from the param to a return site:
///   * a direct projection — `p.0`, `p.field`, `p.field.inner`;
///   * a destructure — `let (a, b) = p;`, `let S { x, y } = p;`, and the
///     nested chain of them — whose leaf binding is returned, including through
///     a `let` alias chain;
///   * either of those moved into a returned aggregate literal (`(r, 9)`,
///     `Holder { r, .. }`), matching `fn_returns_param`'s own aggregate rule.
///
/// DELIBERATELY UNDER-APPROXIMATE ON SHAPE. Any shape it cannot classify — an
/// element leaving through a container or a call — yields no part, leaving that
/// shape exactly as it behaves today. (A nested destructure and a
/// projection-of-a-projection were in that list until B-2026-08-28-23; they are
/// now classified, as paths.) A MISSED escape keeps the pre-existing double body, no
/// worse than before, whereas a FALSE escape would suppress the only body that
/// runs. When the shape is in doubt, report nothing.
///
/// CONSERVATIVE-TRUE ACROSS RETURN SITES, which is the opposite direction and
/// is called out separately because the paragraph above reads like a blanket
/// promise and is not one. The result is the UNION over every return site, so a
/// callee that yields a different part on each branch reports both:
///
/// ```text
/// fn take(w: W, k: bool) -> R { let W { a, b } = w; if k { a } else { b } }
/// ```
///
/// Only one of `a`/`b` escapes on any given run, so masking both loses the
/// body of whichever one died in the call. That is a real lost side effect,
/// and it is nonetheless the ESTABLISHED trade on this channel rather than a
/// regression against it: [`fn_returns_param`] answers true for the same mixed
/// path (`fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }`
/// called with `k = false` prints only the fresh value's body — measured, and
/// far older than this fn), and [`fn_returns_param_payload`] documents the
/// choice explicitly: a missed body, never a double drop, and never a memory
/// fault, since nothing on this channel frees.
///
/// Intersecting across return sites instead would swap that trade — the mixed
/// path would go back to the DOUBLE body this predicate exists to remove —
/// and would put the part channel out of step with both siblings. Tracked as
/// its own row so the whole family moves together if it ever moves.
///
/// Shadowing is tracked rather than ignored for the same reason: a `let` that
/// re-binds an alias name to something unrelated REMOVES it from the alias
/// set, so `let (a, b) = p; let a = other(); a` does not report element 0.
/// B-2026-08-28-23 — this reports the full PATH to each escaping part rather
/// than a single top-level one, so `fn take(w: W) -> R { w.inner.r }` yields
/// `[Field("inner"), Field("r")]` where the earlier top-level-only version
/// declined it and left the pre-existing double body in place.
///
/// A path is NOT interchangeable with its one-level prefix, which is why the
/// widening had to reach the callers rather than stopping here: masking `inner`
/// wholesale would take the body of any SIBLING field of `inner` that really
/// does die in the call — a false escape, the direction this analysis exists to
/// avoid. Each caller decides what it can express: the struct-field masks
/// resolve the whole path into a nested skip tree, and the tuple-element skip
/// list, being a flat index list, keeps only length-1 tuple paths and leaves a
/// deeper one at its pre-existing behaviour.
///
/// Carrying paths through the alias table also picks up a NESTED DESTRUCTURE
/// (`let W { inner, n } = w; let I { r } = inner; r`) for free: the second `let`
/// extends the first's path instead of failing a whole-param gate.
pub fn fn_returns_param_part_paths(f: &Function, arg_index: usize) -> Vec<ParamPath> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return Vec::new();
    };

    /// What `e` denotes relative to the param: `Some(path)` where an EMPTY
    /// path is the whole param and each element steps one level in, `None` =
    /// unrelated. A projection of an already-projected part extends the path
    /// rather than declining (B-2026-08-28-23); the length-1 filter in
    /// `fn_returns_param_parts` is what preserves the older answer for callers
    /// that cannot act on a nested one.
    fn denote(e: &Expr, aliases: &[(String, ParamPath)]) -> Option<ParamPath> {
        match &e.kind {
            ExprKind::Identifier(n) => aliases.iter().find(|(a, _)| a == n).map(|(_, p)| p.clone()),
            ExprKind::TupleIndex { object, index } => {
                let mut path = denote(object, aliases)?;
                path.push(ParamPart::TupleIndex(*index as usize));
                Some(path)
            }
            ExprKind::FieldAccess { object, field } => {
                let mut path = denote(object, aliases)?;
                path.push(ParamPart::Field(field.clone()));
                Some(path)
            }
            _ => None,
        }
    }

    fn set_alias(aliases: &mut Vec<(String, ParamPath)>, name: &str, p: ParamPath) {
        if let Some(slot) = aliases.iter_mut().find(|(a, _)| a == name) {
            slot.1 = p;
        } else {
            aliases.push((name.to_string(), p));
        }
    }

    fn clear_alias(aliases: &mut Vec<(String, ParamPath)>, name: &str) {
        aliases.retain(|(a, _)| a != name);
    }

    /// Record every name a `let` in this block makes denote the param or one
    /// of its parts, and un-record any alias the same `let` shadows.
    fn grow_block(b: &Block, aliases: &mut Vec<(String, ParamPath)>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Let { pattern, value, .. } => {
                    let d = denote(value, aliases);
                    match (&pattern.kind, &d) {
                        (PatternKind::Binding(n), Some(p)) => set_alias(aliases, n, p.clone()),
                        (PatternKind::Binding(n), None) => clear_alias(aliases, n),
                        // A destructure of the param OR of one of its parts:
                        // each leaf binding denotes the source's path extended
                        // by its own element / field (B-2026-08-28-23 — the
                        // whole-param gate here is what used to make a NESTED
                        // destructure unclassifiable).
                        (PatternKind::Tuple(pats), Some(base)) => {
                            for (i, p) in pats.iter().enumerate() {
                                if let PatternKind::Binding(n) = &p.kind {
                                    let mut path = base.clone();
                                    path.push(ParamPart::TupleIndex(i));
                                    set_alias(aliases, n, path);
                                }
                            }
                        }
                        (PatternKind::Struct { fields, .. }, Some(base)) => {
                            for fp in fields {
                                let mut path = base.clone();
                                path.push(ParamPart::Field(fp.name.clone()));
                                match &fp.pattern {
                                    // `W { r, n }` — shorthand binds the field
                                    // name itself.
                                    None => set_alias(aliases, &fp.name, path),
                                    // `W { r: inner, .. }` — renamed leaf.
                                    Some(p) => {
                                        if let PatternKind::Binding(n) = &p.kind {
                                            set_alias(aliases, n, path);
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    grow_expr(value, aliases);
                }
                StmtKind::Expr(e) => grow_expr(e, aliases),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            grow_expr(fe, aliases);
        }
    }

    fn grow_expr(e: &Expr, aliases: &mut Vec<(String, ParamPath)>) {
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => grow_block(b, aliases),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                grow_block(then_block, aliases);
                if let Some(x) = else_branch.as_deref() {
                    grow_expr(x, aliases);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                grow_block(then_block, aliases);
                if let Some(x) = else_branch.as_deref() {
                    grow_expr(x, aliases);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    grow_expr(&a.body, aliases);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => grow_block(body, aliases),
            ExprKind::Return(Some(inner)) => grow_expr(inner, aliases),
            _ => {}
        }
    }

    /// Parts handed across the frame boundary by a RETURNED expression —
    /// directly, or moved into a returned aggregate literal. A whole-param
    /// return contributes nothing: that is `fn_returns_param`'s answer, and
    /// its callers already act on it before this is consulted.
    fn yielded(e: &Expr, aliases: &[(String, ParamPath)], out: &mut Vec<ParamPath>) {
        match &e.kind {
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    yielded(&f.value, aliases, out);
                }
            }
            ExprKind::Tuple(elems) => {
                for el in elems {
                    yielded(el, aliases, out);
                }
            }
            _ => {
                // An EMPTY path is the whole param, which is
                // `fn_returns_param`'s answer and not this one's.
                if let Some(path) = denote(e, aliases) {
                    if !path.is_empty() && !out.contains(&path) {
                        out.push(path);
                    }
                }
            }
        }
    }

    fn scan_expr(e: &Expr, aliases: &[(String, ParamPath)], out: &mut Vec<ParamPath>) {
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                yielded(inner, aliases, out);
                scan_expr(inner, aliases, out);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => scan_block(b, aliases, out),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                scan_block(then_block, aliases, out);
                if let Some(x) = else_branch.as_deref() {
                    scan_expr(x, aliases, out);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                scan_block(then_block, aliases, out);
                if let Some(x) = else_branch.as_deref() {
                    scan_expr(x, aliases, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    yielded(&a.body, aliases, out);
                    scan_expr(&a.body, aliases, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => scan_block(body, aliases, out),
            _ => {}
        }
    }

    fn scan_block(b: &Block, aliases: &[(String, ParamPath)], out: &mut Vec<ParamPath>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => scan_expr(e, aliases, out),
                StmtKind::Let { value, .. } => scan_expr(value, aliases, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            yielded(fe, aliases, out);
            scan_expr(fe, aliases, out);
        }
    }

    let mut aliases: Vec<(String, ParamPath)> = vec![(param_name.clone(), Vec::new())];
    grow_block(&f.body, &mut aliases);
    let mut out = Vec::new();
    scan_block(&f.body, &aliases, &mut out);
    out
}

/// B-2026-08-09-15 — the PAYLOAD sibling of [`fn_returns_param`]: does `f`
/// return a value that a `match` / `if let` bound OUT of parameter `arg_index`?
///
/// `fn take(b: Box2) -> Res { match b { Box2.Full(r) => { return r; } … } }` —
/// `b` itself never reaches a return site, so `fn_returns_param` is false, yet
/// the value the caller handed over leaves the frame all the same. The caller
/// drops an owned enum arg's payload BODIES at the moved-from binding's
/// live-range end (the caller-retains convention this family runs on), which is
/// correct only when the payload dies inside the callee. When it comes back out,
/// the caller's consumer of the RESULT owns it and the arg-site fire is a second
/// body for one value — measured `drop 7, got 7, drop 7` where `--interp`
/// printed `got 7, drop 7`.
///
/// This is the same interprocedural question `fn_returns_param` already asks,
/// one level down: not "is the param returned" but "is something bound out of it
/// returned". Inverting the ownership model instead — making the callee own its
/// by-value params outright — was built and measured first, and it regressed the
/// shapes where the caller has no binding at all (a fresh `E.V(..)` ctor arg,
/// whose caller-side temp drop is the only owner) and reordered an own-`Drop`
/// enum's parent-body-then-payload sequence. Extending the existing rule keeps
/// both.
///
/// CONSERVATIVE-TRUE on a mixed-path callee, exactly like `fn_returns_param` and
/// like the arm-level `suppress_container_elem_bodies_for_var` it feeds: a
/// `match` whose OTHER arm returns a fresh value still answers true, so a run
/// that takes that arm loses the body side effect. That is the established trade
/// on this channel — a missed body, never a double drop, and never a memory
/// fault, since nothing here frees.
pub fn fn_returns_param_payload(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return false;
    };
    /// Does `e` hand `name` across the frame boundary — bare, or moved into a
    /// returned aggregate literal? A FIELD projection (`r.id`) deliberately does
    /// not count: the payload stays behind, only a copy of one field leaves.
    fn yields(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => n == name,
            ExprKind::StructLiteral { fields, .. } => fields.iter().any(|f| yields(&f.value, name)),
            ExprKind::Tuple(elems) => elems.iter().any(|el| yields(el, name)),
            ExprKind::Call { args, .. } => args.iter().any(|a| yields(&a.value, name)),
            _ => false,
        }
    }
    /// Every name that aliases one of `names` through a `let` in this block, so
    /// the `let k = r; return k;` spelling is recognized as the same escape.
    fn grow_aliases(b: &Block, names: &mut Vec<String>) {
        for st in &b.stmts {
            if let StmtKind::Let { pattern, value, .. } = &st.kind {
                if names.iter().any(|n| yields(value, n)) {
                    if let PatternKind::Binding(dest) = &pattern.kind {
                        if !names.iter().any(|n| n == dest) {
                            names.push(dest.clone());
                        }
                    }
                }
            }
        }
    }
    fn returns_any(e: &Expr, names: &[String]) -> bool {
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                names.iter().any(|n| yields(inner, n)) || returns_any(inner, names)
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => returns_any_block(b, names),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                returns_any_block(then_block, names)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| returns_any(x, names))
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                returns_any_block(then_block, names)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| returns_any(x, names))
            }
            ExprKind::Match { arms, .. } => arms
                .iter()
                .any(|a| names.iter().any(|n| yields(&a.body, n)) || returns_any(&a.body, names)),
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => returns_any_block(body, names),
            _ => false,
        }
    }
    fn returns_any_block(b: &Block, names: &[String]) -> bool {
        let mut names = names.to_vec();
        grow_aliases(b, &mut names);
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => returns_any(e, &names),
            StmtKind::Let { value, .. } => returns_any(value, &names),
            _ => false,
        }) || b
            .final_expr
            .as_deref()
            .is_some_and(|fe| names.iter().any(|n| yields(fe, n)) || returns_any(fe, &names))
    }
    /// B-2026-08-29-48 — the ROOTS of assignment targets inside `e` that receive
    /// one of `names`: `out = r`, `out.slot = r`, `outs[i] = r`.
    ///
    /// [`grow_aliases`] already follows a payload through a `let`
    /// (`let k = r; return k;`). It cannot follow one through an ASSIGNMENT,
    /// and the reason is scope rather than oversight: the destination is
    /// declared outside the arm and the `return` that carries it out sits
    /// outside the arm too, so both ends of the route are invisible from inside
    /// the arm body — which is all [`returns_any`] ever sees. The roots
    /// therefore come back out to the caller, which asks the whole FUNCTION
    /// body whether they leave.
    ///
    /// `MultiAssign` is deliberately not handled: [`crate::desugar`] rewrites
    /// every one into `let` temps plus single `Assign`s before any consumer of
    /// this predicate observes the program.
    fn place_root(e: &Expr) -> Option<&str> {
        match &e.kind {
            ExprKind::Identifier(n) => Some(n),
            ExprKind::FieldAccess { object, .. }
            | ExprKind::TupleIndex { object, .. }
            | ExprKind::Index { object, .. } => place_root(object),
            _ => None,
        }
    }
    fn assigned_roots(e: &Expr, names: &[String], out: &mut Vec<String>) {
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => assigned_roots_block(b, names, out),
            ExprKind::Return(Some(inner)) => assigned_roots(inner, names, out),
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
                assigned_roots_block(then_block, names, out);
                if let Some(x) = else_branch.as_deref() {
                    assigned_roots(x, names, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    assigned_roots(&a.body, names, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => assigned_roots_block(body, names, out),
            _ => {}
        }
    }
    fn assigned_roots_block(b: &Block, names: &[String], out: &mut Vec<String>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Assign { target, value } => {
                    if names.iter().any(|n| yields(value, n)) {
                        if let Some(root) = place_root(target) {
                            if !out.iter().any(|s| s == root) {
                                out.push(root.to_string());
                            }
                        }
                    }
                }
                StmtKind::Expr(e) => assigned_roots(e, names, out),
                StmtKind::Let { value, .. } => assigned_roots(value, names, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            assigned_roots(fe, names, out);
        }
    }
    /// Do any of `names` reach a return site by being ASSIGNED into a place
    /// whose root the function then returns? `fn_body` is the whole body, not
    /// the arm's, because that is where both the destination's declaration and
    /// the `return` live.
    fn escapes_by_assignment(body: &Expr, names: &[String], fn_body: &Block) -> bool {
        let mut roots: Vec<String> = Vec::new();
        assigned_roots(body, names, &mut roots);
        !roots.is_empty() && returns_any_block(fn_body, &roots)
    }
    fn escapes_by_assignment_block(body: &Block, names: &[String], fn_body: &Block) -> bool {
        let mut roots: Vec<String> = Vec::new();
        assigned_roots_block(body, names, &mut roots);
        !roots.is_empty() && returns_any_block(fn_body, &roots)
    }
    /// Walk for a `match` / `if let` / `while let` whose SCRUTINEE is the param,
    /// and ask whether the bindings it introduces leave the frame.
    fn walk(e: &Expr, param: &str, fn_body: &Block) -> bool {
        let scrutinee_is_param =
            |s: &Expr| matches!(&s.kind, ExprKind::Identifier(n) if n == param);
        match &e.kind {
            ExprKind::Match { scrutinee, arms } if scrutinee_is_param(scrutinee) => {
                arms.iter().any(|a| {
                    let names: Vec<String> = a.pattern.binding_names();
                    !names.is_empty()
                        && (names.iter().any(|n| yields(&a.body, n))
                            || returns_any(&a.body, &names)
                            || escapes_by_assignment(&a.body, &names, fn_body))
                })
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                ..
            } if scrutinee_is_param(value) => {
                let names: Vec<String> = pattern.binding_names();
                !names.is_empty()
                    && (returns_any_block(then_block, &names)
                        || escapes_by_assignment_block(then_block, &names, fn_body))
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } if scrutinee_is_param(value) => {
                let names: Vec<String> = pattern.binding_names();
                !names.is_empty()
                    && (returns_any_block(body, &names)
                        || escapes_by_assignment_block(body, &names, fn_body))
            }
            ExprKind::Match { scrutinee, arms } => {
                walk(scrutinee, param, fn_body)
                    || arms.iter().any(|a| walk(&a.body, param, fn_body))
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block_for(b, param, fn_body),
            ExprKind::Return(Some(inner)) => walk(inner, param, fn_body),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk(condition, param, fn_body)
                    || walk_block_for(then_block, param, fn_body)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk(x, param, fn_body))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                walk(value, param, fn_body)
                    || walk_block_for(then_block, param, fn_body)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk(x, param, fn_body))
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block_for(body, param, fn_body),
            _ => false,
        }
    }
    fn walk_block_for(b: &Block, param: &str, fn_body: &Block) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => walk(e, param, fn_body),
            StmtKind::Let { value, .. } => walk(value, param, fn_body),
            _ => false,
        }) || b
            .final_expr
            .as_deref()
            .is_some_and(|fe| walk(fe, param, fn_body))
    }
    walk_block_for(&f.body, param_name, &f.body)
}

/// B-2026-08-26-9 — the third sibling of [`fn_returns_param`] and
/// [`fn_returns_param_payload`]: does `f` MOVE by-value parameter `arg_index`
/// into a place that OUTLIVES the call?
///
/// The two existing predicates ask whether the argument leaves the frame
/// through the RETURN VALUE. This one asks whether it leaves through a
/// *reference the caller already holds* — `self`, or a `ref` / `mut ref`
/// parameter. `fn push(mut ref self, x: T) { self.xs.push(x); }` is the
/// canonical shape: `x` never reaches a return site, so both existing
/// predicates answer false, yet the value the caller handed over is alive in
/// the caller's own object when the call returns.
///
/// Without this the caller ran its fresh-temp arg drop anyway, so the value
/// was dropped once at the call and again when the container it now lives in
/// was drained. Measured on `PriorityQueue[Item]` with `Item: Drop`:
/// `drop 3, drop 1, built, pop 1, drop 1, pop 3, drop 3` from `karac build`
/// against `built, pop 1, drop 1, pop 3, drop 3` from `--interp`, plus
/// `31 byte(s) leaked in 8 allocation(s)` under LSan once `Item` also owned a
/// `String`. The same defect on a FREE function (`fn add(v: mut ref Vec[Item],
/// x: Item) { v.push(x); }`) double-drops on BOTH backends, so it is not an
/// A/B divergence there and no run-vs-build check would have found it.
///
/// **Roots that outlive the call**, and only these:
/// - `self` under a `ref self` / `mut ref self` receiver — the caller owns the
///   object. An OWNED `self` receiver (`fn consume(self, x: T)`) is excluded:
///   the receiver dies with the frame, so anything stored into it dies too and
///   the caller's temp drop is the only one.
/// - a parameter declared `ref T` / `mut ref T`.
///
/// A move into a purely local place is NOT a store site here even when that
/// local is later returned — that route belongs to `fn_returns_param`, which
/// already recognizes the param moved into a returned aggregate. A move into a
/// module-level binding is not modelled at all; no such shape has been
/// measured.
///
/// **CONSERVATIVE-TRUE**, the same direction and for the same reason as both
/// siblings: an argument-position occurrence of the bare param under a
/// qualifying receiver counts as a store, even for a method that only reads it
/// (`self.index.contains(x)`). Answering true too often means the caller skips
/// a drop that nothing else runs — a leak. Answering false too often means two
/// owners free one value — a double free. Only the first is recoverable, so
/// the predicate leans that way, exactly as `fn_returns_param_payload`'s
/// mixed-path note records for its own channel.
pub fn fn_moves_param_into_outliving_place(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    // A by-ref parameter is not owned by this frame, so the caller never
    // registered a temp drop for it and there is nothing to suppress.
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return false;
    }
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return false;
    };

    // The set of roots whose storage outlives the call. `self` joins it only
    // for a BORROWED receiver — see the doc comment.
    let mut roots: Vec<&str> = Vec::new();
    if matches!(f.self_param, Some(SelfParam::Ref) | Some(SelfParam::MutRef)) {
        roots.push("self");
    }
    for p in &f.params {
        if !matches!(
            p.ty.kind,
            crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
        ) {
            continue;
        }
        if let PatternKind::Binding(n) = &p.pattern.kind {
            roots.push(n.as_str());
        }
    }
    if roots.is_empty() {
        return false;
    }

    /// Is `e` a place expression whose ROOT is one of `roots`? Walks through
    /// field, index and tuple-index projections, so `self.buckets[i].inner`
    /// resolves to root `self`.
    fn place_root_outlives(e: &Expr, roots: &[&str]) -> bool {
        match &e.kind {
            ExprKind::SelfValue => roots.contains(&"self"),
            ExprKind::Identifier(n) => roots.contains(&n.as_str()),
            ExprKind::FieldAccess { object, .. }
            | ExprKind::TupleIndex { object, .. }
            | ExprKind::Index { object, .. } => place_root_outlives(object, roots),
            _ => false,
        }
    }

    /// Does `e` hand `name` over by value — bare, or nested inside an
    /// aggregate literal being built around it? Mirrors `fn_returns_param`'s
    /// `expr_is_ident` so the three predicates recognize the same move shapes.
    /// A FIELD projection (`x.id`) is deliberately not a move: it copies one
    /// field and leaves the value behind.
    fn moves(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => n == name,
            ExprKind::StructLiteral { fields, .. } => {
                fields.iter().any(|fi| moves(&fi.value, name))
            }
            ExprKind::Tuple(elems) => elems.iter().any(|el| moves(el, name)),
            ExprKind::Call { args, .. } => args.iter().any(|a| moves(&a.value, name)),
            _ => false,
        }
    }

    /// Is `e` the bare parameter `name` itself?
    fn is_bare(e: &Expr, name: &str) -> bool {
        matches!(&e.kind, ExprKind::Identifier(n) if n == name)
    }

    /// B-2026-08-29-49 — the param's PAYLOAD, bound out by a pattern, stored
    /// into an outliving place.
    ///
    /// `moves` matches the param by NAME, so a callee that destructures it
    /// first (`match b { Full(r) => sink.push(r) }`) stores `r` and never `b`,
    /// and the whole predicate answered false for a value that plainly
    /// escapes. That is one of the two holes behind the double body this row
    /// was filed on: with the predicate blind, even the FRESH-TEMP spelling —
    /// which does consult it — kept the caller's drop alongside the container's.
    ///
    /// Only when the scrutinee is the BARE param: `match other { .. }` binds
    /// nothing of ours, and a projection (`match b.inner { .. }`) copies a
    /// field rather than moving the param, the same line `moves` already draws
    /// for `x.id`.
    ///
    /// Conservative-true exactly like its parent, and for the same reason: an
    /// arm that stores licenses the stand-down for every arm, so a mixed enum
    /// whose other arm lets the payload die leaks that body rather than
    /// double-freeing it.
    fn stores_via_destructure<'a>(
        scrutinee: &Expr,
        arms: impl Iterator<Item = (&'a Pattern, &'a Expr)>,
        name: &str,
        roots: &[&str],
    ) -> bool {
        if !is_bare(scrutinee, name) {
            return false;
        }
        arms.into_iter()
            .any(|(pat, body)| pat.binding_names().iter().any(|b| stores(body, b, roots)))
    }

    /// One expression: is it a store of `name` into an outliving place?
    fn stores(e: &Expr, name: &str, roots: &[&str]) -> bool {
        match &e.kind {
            // `self.xs.push(x)`, `store.insert(k, x)`, `self.slots[i].set(x)`.
            ExprKind::MethodCall { object, args, .. } => {
                (place_root_outlives(object, roots) && args.iter().any(|a| moves(&a.value, name)))
                    || stores(object, name, roots)
                    || args.iter().any(|a| stores(&a.value, name, roots))
            }
            ExprKind::Call { args, .. } => args.iter().any(|a| stores(&a.value, name, roots)),
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) => {
                walk_block(b, name, roots)
            }
            // A `par` block's branches run concurrently but store into the same
            // places; the question is unchanged.
            ExprKind::Par(b) => walk_block(b, name, roots),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                stores(condition, name, roots)
                    || walk_block(then_block, name, roots)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| stores(x, name, roots))
            }
            ExprKind::IfLet {
                value,
                pattern,
                then_block,
                else_branch,
            } => {
                stores(value, name, roots)
                    || walk_block(then_block, name, roots)
                    || (is_bare(value, name)
                        && pattern
                            .binding_names()
                            .iter()
                            .any(|b| walk_block(then_block, b, roots)))
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| stores(x, name, roots))
            }
            ExprKind::Match { scrutinee, arms } => {
                stores(scrutinee, name, roots)
                    || arms.iter().any(|a| stores(&a.body, name, roots))
                    || stores_via_destructure(
                        scrutinee,
                        arms.iter().map(|a| (&a.pattern, &a.body)),
                        name,
                        roots,
                    )
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body, name, roots),
            _ => false,
        }
    }

    fn walk_block(b: &Block, name: &str, roots: &[&str]) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => stores(e, name, roots),
            // `self.slot = x`, `self.xs[i] = x`.
            StmtKind::Assign { target, value } => {
                (place_root_outlives(target, roots) && moves(value, name))
                    || stores(value, name, roots)
            }
            StmtKind::Let { value, .. } => stores(value, name, roots),
            _ => false,
        }) || b
            .final_expr
            .as_deref()
            .is_some_and(|fe| stores(fe, name, roots))
    }

    walk_block(&f.body, param_name, &roots)
}

/// The channel-endpoint type heads whose PARAMETERS may be named directly in
/// an effect verb — `with sends(tx)` on a `tx: Sender[T]` (B-2026-08-21-32).
///
/// design.md:6049 states the model normatively: "The `sends(ch)` and
/// `receives(ch)` effects that the effect system tracks attach to the *channel
/// value* — each channel is its own effect resource", and the seven
/// `Sender`/`Receiver` declarations at :6064-:6094 are all written that way.
/// Before this, every one of them was `'tx' is not an effect resource (it is a
/// variable)`, and the diagnostic's suggested remedy (`effect resource tx;`)
/// declared ONE GLOBAL resource named `tx` — the opposite of per-value
/// identity, so there was no spelling that got the documented behaviour.
pub const CHANNEL_ENDPOINT_TYPE_HEADS: &[&str] = &["Sender", "Receiver", "Channel"];

/// The single resource identity every value-rooted channel resource collapses
/// to, for now (B-2026-08-21-32).
///
/// THE COLLAPSE IS DELIBERATE AND IT IS THE SOUND DIRECTION. Per-value
/// identity — telling `sends(tx1)` apart from `sends(tx2)` — is what design.md
/// :6095's producer/consumer parallelization argument actually needs, and it
/// is a real type-system feature (mapping call-site arguments to resource
/// identities, i.e. alias reasoning). Until that exists, the alternative to
/// collapsing is to key the resource on the PARAMETER NAME, and that is
/// unsound in the dangerous direction: two functions whose channel params
/// happen to be named `tx` and `out` would look like disjoint resources, so
/// conflict analysis would report NO conflict between tasks that genuinely
/// share a channel.
///
/// Collapsing over-reports instead: every channel is the same resource, so
/// two channel-touching tasks always conflict. Sound, and pessimistic in the
/// direction that refuses to parallelize rather than the direction that
/// parallelizes something it should not.
///
/// A user's own `effect resource Channel;` merges with this name rather than
/// colliding, which is the same conservative direction.
pub const CHANNEL_RESOURCE_CANONICAL: &str = "Channel";

/// Names of the parameters whose declared type head is a channel endpoint.
/// Purely syntactic — it reads the declared type off the signature, so the
/// resolver (which runs before typecheck and has no types) and the effect
/// checker can both ask the same question and get the same answer.
pub fn channel_endpoint_param_names(params: &[Param]) -> Vec<String> {
    let mut out = Vec::new();
    for p in params {
        let Some(name) = p.name() else { continue };
        if type_head_is_channel_endpoint(&p.ty) {
            out.push(name.to_string());
        }
    }
    out
}

/// True when the type's root head (after stripping `ref` / `mut ref` / `weak`)
/// is a channel endpoint.
pub fn type_head_is_channel_endpoint(ty: &TypeExpr) -> bool {
    match &ty.kind {
        crate::ast::TypeKind::Path(p) => p
            .segments
            .last()
            .is_some_and(|s| CHANNEL_ENDPOINT_TYPE_HEADS.contains(&s.as_str())),
        crate::ast::TypeKind::Ref(inner)
        | crate::ast::TypeKind::MutRef(inner)
        | crate::ast::TypeKind::Weak(inner) => type_head_is_channel_endpoint(inner),
        _ => false,
    }
}
