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
    fn_returns_param_with(None, f, arg_index)
}

/// [`fn_returns_param`] with the declarations in hand, so a USER enum's
/// variant constructor (`return Ho.Full(v)`) and the bare seeded spellings
/// (`Some(v)`, `Ok(v)`, `Err(v)`) count as carrying the param out, as the
/// `Option.Some(v)` spelling already does. B-2026-09-25-22: codegen's
/// monomorph call site asks this, and without it `takeit(mkh(r))` over
/// `fn mkh[T](v: T) -> Ho[T] { return Ho.Full(v) }` left the caller's `r`
/// running its `Drop` body beside the one the result carries. The
/// interpreter keeps the program-less form, which is why this is a sibling
/// rather than a change of meaning.
pub fn fn_returns_param_with(
    program: Option<&crate::Program>,
    f: &Function,
    arg_index: usize,
) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return false;
    };
    // B-2026-09-06-19 — the union sees a param handed back THROUGH a wrapping
    // local (`let p = P2 { r: r, .. }; return Box2 { r: p.r }`) exactly as it
    // sees the direct `return Box2 { r: r }`: the interpreter's method frame
    // reads this to decide which by-value params it owns at exit, and a
    // wrapped-then-returned `r` it owned there ran its body beside the
    // caller's result binding (`d72 C72 d72` under `--interp` alone).
    let wraps = param_wrap_aliases(None, f, param_name);
    let wraps: &[(String, ParamPath)] = &wraps;
    /// A return site "yields" the param when it IS the bare identifier, or
    /// (B-2026-08-02-23 leg 2) when it is an AGGREGATE LITERAL that moves the
    /// param into itself — `Holder { xs: v, tag: 9 }`, `(v, 9)`, or either
    /// nested. The value crosses the frame boundary inside the returned
    /// aggregate exactly as it does when returned bare, so the caller's
    /// consumer of the RESULT is the owner either way; recognizing only the
    /// bare form left the caller dropping an arg it had just been handed back
    /// inside a struct, firing the Drop body twice.
    fn expr_is_ident(
        e: &Expr,
        name: &str,
        wraps: &[(String, ParamPath)],
        program: Option<&crate::Program>,
    ) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => n == name || place_yields_wrapped_param(e, wraps),
            // B-2026-09-06-19 — a projection back out of a wrapping local.
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } => {
                place_yields_wrapped_param(e, wraps)
            }
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .any(|f| expr_is_ident(&f.value, name, wraps, program)),
            ExprKind::Tuple(elems) => elems
                .iter()
                .any(|el| expr_is_ident(el, name, wraps, program)),
            // B-2026-09-19-36 — an `Option`/`Result` CONSTRUCTOR carries the
            // param out exactly as the struct literal and tuple arms above do.
            // `return Ho { g: Option.Some(g) }` hands `g`'s box to the caller's
            // result binding just as `return Ho { g: g }` does, and recognizing
            // only the latter left the caller's argument still owning a box the
            // returned wrapper also held — `free(): double free detected in
            // tcache 2` where `--interp` was correct.
            //
            // `Option` and `Result` by name, rather than a general
            // variant-constructor test, because telling a user enum's
            // constructor from an associated function call needs the
            // declarations and this predicate has only the function (the
            // `program`-carrying sibling `yields_wrapped_named` does exactly
            // that, and still would not catch these two — they are BUILT-IN, so
            // `is_user_variant_ctor` finds no `EnumDef` for them).
            //
            // Naming them is not a shortcut here, it is COMPLETE for this shape:
            // `E_ENUM_NESTED_ENUM_PAYLOAD` rejects a user enum with a plain enum
            // payload outright (`enum W[T] { S(G1[T]), N }` does not compile), so
            // `Option` and `Result` are the only wrappers a plain generic enum
            // can reach. A `shared`/`par` inner enum is an RC pointer on a
            // different channel, and a `Vec` layer owns its elements itself.
            ExprKind::Call { callee, args } => {
                // B-2026-09-25-22 — only with the declarations in hand; see
                // `fn_returns_param_with`.
                if let Some(p) = program {
                    let bare_seeded = matches!(
                        &callee.kind,
                        ExprKind::Identifier(n) if matches!(n.as_str(), "Some" | "Ok" | "Err")
                            && !p.items.iter().any(|it| matches!(it, Item::Function(g) if &g.name == n))
                    );
                    if bare_seeded || is_user_variant_ctor(p, callee) {
                        return args
                            .iter()
                            .any(|a| expr_is_ident(&a.value, name, wraps, program));
                    }
                }
                let ExprKind::Path { segments, .. } = &callee.kind else {
                    return false;
                };
                let [head, _variant] = segments.as_slice() else {
                    return false;
                };
                if head != "Option" && head != "Result" {
                    return false;
                }
                args.iter()
                    .any(|a| expr_is_ident(&a.value, name, wraps, program))
            }
            _ => false,
        }
    }
    fn walk_expr(
        e: &Expr,
        name: &str,
        wraps: &[(String, ParamPath)],
        program: Option<&crate::Program>,
    ) -> bool {
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                expr_is_ident(inner, name, wraps, program) || walk_expr(inner, name, wraps, program)
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b, name, wraps, program),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk_expr(condition, name, wraps, program)
                    || walk_block(then_block, name, wraps, program)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk_expr(x, name, wraps, program))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                walk_expr(value, name, wraps, program)
                    || walk_block(then_block, name, wraps, program)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk_expr(x, name, wraps, program))
            }
            ExprKind::Match { scrutinee, arms } => {
                walk_expr(scrutinee, name, wraps, program)
                    || arms.iter().any(|a| {
                        // An arm TAIL that is the bare param is a return site
                        // when the match is itself a tail — conservative: any
                        // bare-param arm tail counts.
                        expr_is_ident(&a.body, name, wraps, program)
                            || walk_expr(&a.body, name, wraps, program)
                    })
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body, name, wraps, program),
            _ => false,
        }
    }
    fn walk_block(
        b: &Block,
        name: &str,
        wraps: &[(String, ParamPath)],
        program: Option<&crate::Program>,
    ) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => walk_expr(e, name, wraps, program),
            _ => false,
        }) || b.final_expr.as_deref().is_some_and(|fe| {
            expr_is_ident(fe, name, wraps, program) || walk_expr(fe, name, wraps, program)
        })
    }
    walk_block(&f.body, param_name, wraps, program)
}

/// B-2026-09-04-30 — for an OWNED-`self` method: can its RETURN VALUE carry any
/// part of the receiver back to the caller?
///
/// The receiver twin of [`fn_returns_param`]'s question, asked about the return
/// TYPE rather than by walking for a bare identifier. `self` is
/// `ExprKind::SelfValue`, not `ExprKind::Identifier("self")`, so none of the
/// name-keyed passthrough predicates above can see it — and a syntactic walk
/// would still have to chase `self.field`, a `Self`-typed forward, and an
/// aggregate that closes over any of them. The type answers all three at once:
/// if nothing the receiver owns can be spelled in the return type, nothing the
/// receiver owns crosses back, whatever the body does with it.
///
/// **Why the caller needs an answer at all.** A by-value receiver's field
/// `Drop` bodies are CALLER-retained, exactly as a by-value parameter's are
/// (design.md § Drop, "Interaction with move semantics": the destination takes
/// over, and codegen's destination for both is the caller's temp). The caller
/// runs them on its temp at statement end. When the callee hands the receiver
/// back out, the caller's RESULT binding is the owner instead and the temp
/// registration must stand down or the body fires twice — the passthrough
/// double-fire (`mk(3).me().ident()`) that made B-2026-08-01-5 disable owned-
/// `self` receiver bodies wholesale. Disabling them wholesale is what left a
/// TEMP receiver with no owner on any surface, since the callee never runs them
/// either; this predicate is the narrow gate that was missing.
///
/// `type_runs_user_drop` is the caller's own Drop table, passed in rather than
/// duplicated, so the interpreter and codegen reach the same verdict from the
/// same syntax and agree by construction (the rule the Arrow IPC twin and
/// `String.normalize` follow).
///
/// Conservative toward `true` (= can carry, so the caller stands down) at every
/// unrecognised shape. That direction costs a body that nobody runs — the
/// status quo for this whole family — where the other direction costs a double
/// free. Declined shapes, each because the receiver's payload can ride out
/// inside it without being named:
///
///  * any GENERIC path — `Result[R, String]`, `Option[R]`, `Vec[R]`;
///  * `Self`, which names the receiver's own type and so always can;
///  * a tuple, array, pointer, `ref`/`mut ref`, or `Fn` return.
pub fn owned_self_return_is_opaque_to_receiver(
    f: &Function,
    type_runs_user_drop: &mut dyn FnMut(&str) -> bool,
) -> bool {
    let Some(rt) = f.return_type.as_ref() else {
        // Unit return: there is no value to carry anything back in.
        return true;
    };
    let crate::ast::TypeKind::Path(p) = &rt.kind else {
        return false;
    };
    if p.generic_args.is_some() {
        return false;
    }
    match p.segments.last() {
        // `Self` is the receiver's own type by definition, so it can always
        // carry it — and it resolves to no entry in either backend's Drop
        // table, which would read as "clean" if it reached the lookup.
        Some(name) if name != "Self" => !type_runs_user_drop(name),
        _ => false,
    }
}

/// B-2026-09-06-38 — can `f`'s RETURN value carry its owned `self` receiver,
/// of type `receiver`, back to the caller WHOLE?
///
/// The ENUM-receiver sibling of [`owned_self_return_is_opaque_to_receiver`].
/// That gate asks whether the return could carry ANY Drop-bearing part of the
/// receiver, because a struct temp's caller-side registration is a walk over
/// its fields, and a field handed back would fire twice. An owned ENUM temp's
/// caller-side registration is the shell's OWN body alone — the match-arm
/// channel owns the payload (B-2026-08-01-6, B-2026-09-06-27) — so a payload
/// handed back (`fn m_r(self) -> R`) doubles nothing, and declining it under
/// the struct gate left the shell body with no owner on every surface. What
/// doubles the shell body is the WHOLE receiver riding out: the caller's
/// result binding then runs it too (`fn me(self) -> E`, `fn wrap(self) -> W`
/// with `struct W { e: E }`).
///
/// So this declines exactly the returns that can CONTAIN the receiver's type:
/// `Self`, the type itself, any generic path (`Option[E]`, `Vec[E]`), any
/// tuple / array / pointer / ref / fn return, and any declared type whose
/// fields or payloads reach the receiver's type transitively. A name this
/// pass cannot resolve to a declaration declines too — the conservative
/// direction of the whole family (a body nobody runs, never a double fire).
pub fn owned_self_return_cannot_carry_receiver(
    f: &Function,
    receiver: &str,
    items: &[Item],
) -> bool {
    let Some(rt) = f.return_type.as_ref() else {
        // Unit return: there is no value to carry anything back in.
        return true;
    };
    !type_expr_can_contain(rt, receiver, items, &mut Vec::new())
}

fn type_expr_can_contain(
    te: &TypeExpr,
    target: &str,
    items: &[Item],
    visited: &mut Vec<String>,
) -> bool {
    match &te.kind {
        crate::ast::TypeKind::Unit => false,
        crate::ast::TypeKind::Tuple(elems) => elems
            .iter()
            .any(|e| type_expr_can_contain(e, target, items, visited)),
        crate::ast::TypeKind::Path(p) => {
            let [name] = p.segments.as_slice() else {
                return true;
            };
            if name == target || name == "Self" {
                return true;
            }
            if let Some(args) = p.generic_args.as_ref() {
                // A generic path contains `target` when one of its ARGUMENTS
                // does (`Option[E]`, `Map[String, E]`, `Vec[W]` with
                // `W { e: E }`) — or, for a user-declared generic head, when
                // the declaration's own fields reach it (`S[T] { e: E, t: T }`;
                // the bare `T` field resolves to no declaration and so counts
                // as "can", the conservative direction). A builtin head
                // (`Vec[i64]`, `Option[String]`) has no declaration here and
                // carries nothing but its arguments — which is what keeps a
                // payload like `R { xs: Vec[i64] }` from reading as able to
                // hold the enum that carries it.
                let arg_can = args.iter().any(|a| match a {
                    GenericArg::Type(t) => type_expr_can_contain(t, target, items, visited),
                    GenericArg::Const(_) | GenericArg::Shape(_) => false,
                });
                let declared_here = items.iter().any(|it| {
                    matches!(it, Item::StructDef(s) if &s.name == name)
                        || matches!(it, Item::EnumDef(e) if &e.name == name)
                });
                return arg_can
                    || (declared_here && type_name_can_contain(name, target, items, visited));
            }
            if type_expr_cannot_carry_drop_body(te) {
                return false;
            }
            type_name_can_contain(name, target, items, visited)
        }
        _ => true,
    }
}

/// Does the declared struct or enum `name` reach `target` by value through
/// its fields / payloads? `visited` breaks recursive declarations.
fn type_name_can_contain(
    name: &str,
    target: &str,
    items: &[Item],
    visited: &mut Vec<String>,
) -> bool {
    if visited.iter().any(|v| v == name) {
        return false;
    }
    visited.push(name.to_string());
    for it in items {
        match it {
            Item::StructDef(s) if s.name == name => {
                return s
                    .fields
                    .iter()
                    .any(|f| type_expr_can_contain(&f.ty, target, items, visited));
            }
            Item::EnumDef(e) if e.name == name => {
                return e.variants.iter().any(|v| match &v.kind {
                    VariantKind::Unit => false,
                    VariantKind::Tuple(tys) => tys
                        .iter()
                        .any(|t| type_expr_can_contain(t, target, items, visited)),
                    VariantKind::Struct(fs) => fs
                        .iter()
                        .any(|f| type_expr_can_contain(&f.ty, target, items, visited)),
                });
            }
            _ => {}
        }
    }
    true
}

/// B-2026-09-06-42 — does `f` REBIND its owned `self` WHOLE, unconditionally:
/// a top-level `let <name> = self;` statement of the body?
///
/// The rebind transfers the receiver to the local: the local's own drop runs
/// the shell / struct body AND the field / payload bodies at its death, so the
/// caller's retained walk over a NAMED receiver (`let a = E.A(mk(1)); a.m_let()`)
/// must stand down on bodies (keeping its memory action, which frees the
/// caller's own copy). Measured before this on all four surfaces: `dE dR1 dE`
/// for an own-`Drop` enum, `dS dR2 dS dR2` for an own-`Drop` struct, `dR1 dR1`
/// for a struct with only Drop-bearing fields — each body twice.
///
/// Deliberately TOP-LEVEL ONLY. A rebind nested in a branch or loop moves the
/// receiver on some paths and leaves it with the caller on the rest, and the
/// callee frame registers nothing for an owned `self` (caller-retains), so an
/// unconditional stand-down would lose the body on the non-rebinding path —
/// the B-2026-08-28-22 class. Those spellings keep today's behaviour.
/// B-2026-09-06-39 — does `f` DESTRUCTURE a bare `self` in a `match` /
/// `if let` / `while let`?
///
/// The ENUM-receiver companion to [`fn_binds_self_part_out`], and it exists
/// because that predicate deliberately answers `false` here. Its
/// B-2026-09-06-15 note explains why: for a bare owned STRUCT receiver both
/// backends bind the arms as VIEWS (`bare_self_is_owned_struct_receiver` and
/// its interpreter twin), so `match self { H1 { e } => .. }` hands nothing out
/// and the caller's walk is the one body owner.
///
/// An ENUM receiver is the opposite, and the interpreter says so in as many
/// words: "A bare owned ENUM `self` keeps its transfer semantics: neither
/// registrar walks an enum receiver's bodies, so the arm channel is still
/// their only owner." So for an enum the arm DOES take the payload, and a
/// caller-side walk on top of it fires the body twice — measured, as
/// `dR2 x2 dE dR2`, when this predicate was missing and the enum disarm was
/// gated on `fn_binds_self_part_out` alone.
///
/// The two together are the real question at the disarm: is there an arm
/// channel to hand this receiver's payload to? `fn_binds_self_part_out` covers
/// the `let` transfer, this covers the match spelling, and neither alone is
/// sufficient. A WILDCARD arm counts (`E.A(_)`) — the arm channel fires the
/// payload there too, measured — so the test is the scrutinee, not the
/// pattern's bindings.
pub fn fn_matches_on_bare_self(f: &Function) -> bool {
    fn is_bare_self(e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::SelfValue)
    }
    fn walk_expr(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                is_bare_self(scrutinee)
                    || walk_expr(scrutinee)
                    || arms.iter().any(|a| walk_expr(&a.body))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                is_bare_self(value)
                    || walk_expr(value)
                    || walk_block(then_block)
                    || else_branch.as_deref().is_some_and(walk_expr)
            }
            ExprKind::WhileLet { value, body, .. } => {
                is_bare_self(value) || walk_expr(value) || walk_block(body)
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk_expr(condition)
                    || walk_block(then_block)
                    || else_branch.as_deref().is_some_and(walk_expr)
            }
            ExprKind::While { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body),
            _ => false,
        }
    }
    fn walk_block(b: &Block) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => walk_expr(value),
            StmtKind::Expr(e) => walk_expr(e),
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(body),
            _ => false,
        }) || b.final_expr.as_deref().is_some_and(walk_expr)
    }
    walk_block(&f.body)
}

pub fn fn_rebinds_self_whole(f: &Function) -> bool {
    f.body.stmts.iter().any(|st| {
        matches!(&st.kind, StmtKind::Let { pattern, value, .. }
            if matches!(pattern.kind, PatternKind::Binding(_))
                && matches!(value.kind, ExprKind::SelfValue))
    })
}

/// B-2026-09-06-45 — does `f` rebind its owned `self` WHOLE on SOME paths
/// only: a `let <name> = self;` nested inside a branch, arm or loop, with no
/// top-level one?
///
/// The complement of [`fn_rebinds_self_whole`], and the two are mutually
/// exclusive by construction: a body with a top-level rebind answers `false`
/// here and `true` there. Together they cover every whole-`self` transfer.
///
/// The nested spelling is the one that needs a per-path answer. On the path
/// that rebinds, the local owns the receiver and runs its bodies; on the path
/// that does not, nobody inside the callee does, and an owned `self` registers
/// nothing at entry (caller-retains). So the caller standing down
/// unconditionally — which is all [`fn_rebinds_self_whole`] asks for — would
/// LOSE the body on the non-rebinding path, the B-2026-08-28-22 class. Both
/// backends therefore pair this predicate with a callee-frame registration
/// guarded per path: the flag is armed at entry and cleared by the rebind
/// itself (codegen's `arm_conditional_store_flag`, whose `hands_over` reads a
/// bare `self`; the interpreter's `record_container_bodies_move_sources`).
/// Measured before that: `dE dR11 dE` for an own-`Drop` enum receiver on the
/// rebinding path and `dS1 dR31 dS1 dR31` for an own-`Drop` struct, each body
/// twice, while the non-rebinding path was already right.
///
/// A rebind inside a LOOP body counts as nested: the loop may run zero times,
/// so the transfer is per path exactly as a branch's is.
pub fn fn_conditionally_rebinds_self(f: &Function) -> bool {
    if fn_rebinds_self_whole(f) {
        return false;
    }
    fn is_self_rebind(st: &crate::ast::Stmt) -> bool {
        matches!(&st.kind, StmtKind::Let { pattern, value, .. }
            if matches!(pattern.kind, PatternKind::Binding(_))
                && matches!(value.kind, ExprKind::SelfValue))
    }
    fn walk_block(b: &Block) -> bool {
        b.stmts.iter().any(|st| {
            is_self_rebind(st)
                || match &st.kind {
                    StmtKind::Let { value, .. } | StmtKind::Assign { value, .. } => {
                        walk_expr(value)
                    }
                    StmtKind::LetElse {
                        value, else_block, ..
                    } => walk_expr(value) || walk_block(else_block),
                    StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(body),
                    StmtKind::Expr(e) => walk_expr(e),
                    _ => false,
                }
        }) || b.final_expr.as_deref().is_some_and(walk_expr)
    }
    fn walk_expr(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk_expr(condition)
                    || walk_block(then_block)
                    || else_branch.as_deref().is_some_and(walk_expr)
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                walk_expr(value)
                    || walk_block(then_block)
                    || else_branch.as_deref().is_some_and(walk_expr)
            }
            ExprKind::Match { scrutinee, arms } => {
                walk_expr(scrutinee) || arms.iter().any(|a| walk_expr(&a.body))
            }
            ExprKind::WhileLet { value, body, .. } => walk_expr(value) || walk_block(body),
            ExprKind::While { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body),
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b),
            ExprKind::Return(inner) => inner.as_deref().is_some_and(walk_expr),
            _ => false,
        }
    }
    // TOP-LEVEL statements are `fn_rebinds_self_whole`'s half and are already
    // ruled out above; only what they CONTAIN is this predicate's.
    f.body.stmts.iter().any(|st| match &st.kind {
        StmtKind::Let { value, .. } | StmtKind::Assign { value, .. } => walk_expr(value),
        StmtKind::LetElse {
            value, else_block, ..
        } => walk_expr(value) || walk_block(else_block),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => walk_block(body),
        StmtKind::Expr(e) => walk_expr(e),
        _ => false,
    }) || f.body.final_expr.as_deref().is_some_and(walk_expr)
}

/// B-2026-09-04-30 — does `f`'s body BIND A PART OF `self` OUT: a `let` (or
/// `let…else`) initialized from `self` or a `self`-rooted place, or a
/// `match` / `if let` / `while let` whose scrutinee is one?
///
/// The second half of the owned-`self` receiver gate, and the boundary between
/// this row and its neighbours. The caller runs a by-value receiver's field
/// bodies BECAUSE nobody else does — but the moment the callee binds a field,
/// a destructure leaf, or a payload out of `self`, THAT binding owns the part
/// and runs its body, and the caller's whole-struct walk fires a second time on
/// top. Measured, with the caller's walk registered unconditionally:
///
/// ```text
/// fn plain(self)  { println(f"  rd{self.a.id}") }          each body once   ✓
/// fn field(self)  { let x = self.a; .. }                   dR2, dR102 TWICE ✗
/// fn matchb(self) { match self.b { Ok(r) => .. } }         dR104 TWICE      ✗
/// fn s_call(self) { let HoRes { a, b } = self;
///                   match b { Ok(r) => eat(r), .. } }      dR103 TWICE      ✗
/// ```
///
/// Memory stays balanced through all of them (ASAN clean) — it is the BODY that
/// doubles, which is why only a stdout pin catches it.
///
/// So this predicate declines exactly the cells that belong to the neighbouring
/// rows rather than to this one: B-2026-09-03-7 (a by-value param destructured
/// inside a method), B-2026-09-04-29 (the `self` destructure's transfer), and
/// B-2026-09-02-24 (the callee's retained walk firing on a value an arm handed
/// out). Declining is the pre-existing behaviour for every one of them, so the
/// conservative direction here is also the no-regression direction.
///
/// A bare READ of `self` is not a bind-out: `self.a.id` in an interpolation
/// leaves the field where it is, which is what keeps the plain receiver — the
/// row's whole subject — admitted.
pub fn fn_binds_self_part_out(f: &Function) -> bool {
    /// B-2026-08-31-43 — a `match` / `if let` / `while let` whose scrutinee
    /// is a PROJECTION off `self` (`self.e`, `self.s.e`) is no longer a
    /// bind-out: both backends now treat an owned receiver's projection like
    /// any by-value parameter's, so its arms bind VIEWS of the caller-retained
    /// value and run no body of their own. Declining those here left a
    /// fresh-temp receiver (`H1 { .. }.take()`) with nobody running its bodies
    /// at all.
    ///
    /// B-2026-09-06-15 — and neither is a BARE `self` scrutinee, for the same
    /// reason one row later: both backends now bind a bare owned STRUCT
    /// receiver's arms as views too (codegen's
    /// `bare_self_is_owned_struct_receiver`, the interpreter's twin), so
    /// `match self { H1 { e } => .. }` hands nothing out and the caller's
    /// walk — this registrar, for a temp — is the one body owner. Declining
    /// it here was the fresh-temp half of that row (`dR32` and no `dE`: the
    /// enum shell's body ran nowhere). Only a `let` from bare `self`
    /// (`let x = self`, `let H1 { e } = self`) remains a bind-out: that is
    /// the transfer B-2026-09-04-29 built, and its leaves own their parts.
    /// The scrutinee predicate below is therefore consulted by the `let`
    /// arm of `walk_block` alone; a match scrutinee is only WALKED, for a
    /// bind-out nested inside it.
    fn scrutinee_binds_out(e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::SelfValue)
    }
    fn walk_expr(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                walk_expr(scrutinee) || arms.iter().any(|a| walk_expr(&a.body))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                walk_expr(value)
                    || walk_block(then_block)
                    || else_branch.as_deref().is_some_and(walk_expr)
            }
            ExprKind::WhileLet { value, body, .. } => walk_expr(value) || walk_block(body),
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk_expr(condition)
                    || walk_block(then_block)
                    || else_branch.as_deref().is_some_and(walk_expr)
            }
            ExprKind::While { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body),
            _ => false,
        }
    }
    fn walk_block(b: &Block) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            // B-2026-09-06-16 — a `let` from a PROJECTION off `self`
            // (`let e = self.e`) binds a VIEW of the caller-retained value on
            // both backends now, as `let e = h.e` always did, so it is no
            // longer a bind-out; a bare `let x = self` (transfer) still is.
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                scrutinee_binds_out(value) || walk_expr(value)
            }
            StmtKind::Expr(e) => walk_expr(e),
            _ => false,
        }) || b.final_expr.as_deref().is_some_and(walk_expr)
    }
    walk_block(&f.body)
}

/// B-2026-09-06-39 — do this function's bare-`self` match / `if let` /
/// `while let` arms only READ THROUGH their payload bindings, so that the arms
/// can bind VIEWS and the CALLER keep ownership of the payload's `Drop` bodies?
///
/// [`fn_matches_on_bare_self`] answers "is there an arm channel at all"; this
/// answers the follow-up question that decides WHO the payload belongs to. When
/// every bare-`self` arm only projects out of its binding (`r.id`, `r.0`), the
/// binding is design.md § Match Arm Binding Modes' "binding that is only read
/// [and] borrows from the already-owned value", nothing in the callee takes the
/// payload over, and the caller's walk is its one owner — which is what puts the
/// payload's body AFTER the shell's, the design.md § Part 8 order a local
/// scrutinee and a by-value param have always printed.
///
/// Three clauses, each load-bearing and each measured as a regression when it
/// was missing:
///
///  * `fn_matches_on_bare_self` — there has to BE an arm to decide about.
///  * `!fn_binds_self_part_out` — a callee that ALSO writes `let e = self;`
///    hands the whole receiver to that local, and the caller's disarm fires for
///    it whatever the arms do. Binding views underneath that lost the payload's
///    body outright (`enum_false` and `temp_false` in
///    `e2e_nested_self_rebind_runs_each_body_once`, `cond-false/local` in
///    `e2e_whole_self_rebind_in_owned_method_runs_each_body_once`: `dR2 dE`
///    became a bare `dE`).
///  * every binding is projection-only — [`expr_mentions_name_outside_field_projection`]
///    over each arm's guard and body. `fn m_r(self) -> R { match self { E.A(r)
///    => { return r; } .. } }` hands the payload to the caller's RESULT, and
///    with views on top of that the body fired twice (`dE dR3 y3 dR3`).
///
/// Deliberately CONSERVATIVE, through that third clause's own conservatism: a
/// bare mention in ANY other position counts as a take, including a call
/// argument and a method receiver. So `eat(r)` and `Some(r)` both decline, and
/// only the second of those has to — `Some(r)` really is a move (`opt/temp`
/// doubled without it), while `eat(r)` is caller-retains and would be safe. The
/// walk cannot tell them apart syntactically, and an over-approximation costs
/// the pre-existing mis-ORDER while an under-approximation costs a doubled
/// body, so it over-approximates. That residual is B-2026-09-16-26.
pub fn fn_bare_self_arms_bind_views(f: &Function) -> bool {
    fn names_of(p: &Pattern, out: &mut Vec<String>) {
        match &p.kind {
            PatternKind::Binding(n) => out.push(n.clone()),
            PatternKind::AtBinding { name, pattern, .. } => {
                out.push(name.clone());
                names_of(pattern, out);
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(sub) => names_of(sub, out),
                        None => out.push(f.name.clone()),
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. }
            | PatternKind::Tuple(patterns)
            | PatternKind::Or(patterns) => {
                for sub in patterns {
                    names_of(sub, out);
                }
            }
            _ => {}
        }
    }
    fn block_takes(b: &Block, name: &str) -> bool {
        b.stmts.iter().any(|st| {
            let mut found = false;
            crate::rc_elide::walk_stmt_children_pub(st, &mut |e| {
                if crate::deque_head::expr_mentions_name_outside_field_projection(e, name) {
                    found = true;
                }
            });
            found
        }) || b.final_expr.as_deref().is_some_and(|e| {
            crate::deque_head::expr_mentions_name_outside_field_projection(e, name)
        })
    }
    fn arm_reads_only(
        p: &Pattern,
        guard: Option<&Expr>,
        body: Option<&Expr>,
        blk: Option<&Block>,
    ) -> bool {
        let mut names = Vec::new();
        names_of(p, &mut names);
        names.iter().all(|n| {
            !guard.is_some_and(|g| {
                crate::deque_head::expr_mentions_name_outside_field_projection(g, n)
            }) && !body.is_some_and(|b| {
                crate::deque_head::expr_mentions_name_outside_field_projection(b, n)
            }) && !blk.is_some_and(|b| block_takes(b, n))
        })
    }
    fn is_bare_self(e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::SelfValue)
    }
    fn walk_expr(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                (!is_bare_self(scrutinee)
                    || arms
                        .iter()
                        .all(|a| arm_reads_only(&a.pattern, a.guard.as_ref(), Some(&a.body), None)))
                    && walk_expr(scrutinee)
                    && arms.iter().all(|a| walk_expr(&a.body))
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                (!is_bare_self(value) || arm_reads_only(pattern, None, None, Some(then_block)))
                    && walk_expr(value)
                    && walk_block(then_block)
                    && else_branch.as_deref().is_none_or(walk_expr)
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                (!is_bare_self(value) || arm_reads_only(pattern, None, None, Some(body)))
                    && walk_expr(value)
                    && walk_block(body)
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk_expr(condition)
                    && walk_block(then_block)
                    && else_branch.as_deref().is_none_or(walk_expr)
            }
            ExprKind::While { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body),
            _ => true,
        }
    }
    fn walk_block(b: &Block) -> bool {
        b.stmts.iter().all(|st| match &st.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => walk_expr(value),
            StmtKind::Expr(e) => walk_expr(e),
            _ => true,
        }) && b.final_expr.as_deref().is_none_or(walk_expr)
    }
    fn_matches_on_bare_self(f) && !fn_binds_self_part_out(f) && walk_block(&f.body)
}

/// `(target, callee key, [(arg index, bare-identifier arg)])` of one
/// `let target = callee(..)` candidate — see `RebindWalk::call_rebinds`.
type CallRebind = (String, String, Vec<(usize, String)>);

/// `(target, source)` of every candidate whole-value rebind, and how many
/// times each name is bound anywhere the walk reaches.
struct RebindWalk {
    rebinds: Vec<(String, String)>,
    /// B-2026-09-09-13 — `let MUT x = y`, which [`rebinds`] deliberately does
    /// NOT record: an alias that can be reassigned is not a stable alias, so
    /// every ALIASING predicate here is right to ignore it. An OWNERSHIP
    /// predicate needs the two sets TOGETHER (B-2026-09-09-17): a mutable
    /// rebind frees the displaced box when it is overwritten, an immutable one
    /// frees it at scope exit, and either way the local is an owner. See
    /// [`param_rebound_into_local`], the one predicate that reads this.
    mut_rebinds: Vec<(String, String)>,
    /// B-2026-09-06-19 — `let x = S { f: y, .. }` / `let x = (y, ..)`: `x` WRAPS
    /// `y` at `path`. A wrap is not a rebind (the types differ), but it is the
    /// other way a by-value param travels through a local on its way out:
    /// `let p = P2 { r: r, n: 1 }; return Box2 { r: p.r }` hands `r` back
    /// exactly as `let p = r; return p` does.
    wraps: Vec<(String, String, ParamPath)>,
    /// B-2026-09-26-27 — `let x = Some(y)` / `let x = Ho.Full(y)`: `x` WRAPS
    /// `y` inside an ENUM constructor, as `(x, y, callee, path)` where `path`
    /// is any struct / tuple nesting INSIDE the payload. Kept apart from
    /// [`Self::wraps`] because a user variant constructor is spelled exactly
    /// like an associated-function call, and only [`param_wrap_aliases`], which
    /// may hold the program, can tell the two apart.
    ctor_wraps: Vec<(String, String, Expr, ParamPath)>,
    /// `let x = y.f.g` / `let x = y.0`: `x` REBINDS the part of `y` at `path`.
    /// Read back against a wrap: `let q = p.r` after `let p = P2 { r: r, .. }`
    /// makes `q` the param itself again (an empty remaining path).
    proj_rebinds: Vec<(String, String, ParamPath)>,
    /// B-2026-09-06-9 — `(target, callee key, [(arg index, bare-identifier
    /// arg)])` of every `let x = g(..)` candidate; only the program-aware
    /// [`fn_whole_param_aliases`] reads these, the predicates never do.
    call_rebinds: Vec<CallRebind>,
    bound: std::collections::HashMap<String, usize>,
}
impl RebindWalk {
    fn bind(&mut self, pat: &Pattern) {
        for n in pat.binding_names() {
            *self.bound.entry(n).or_insert(0) += 1;
        }
    }
    /// Every bare identifier `y` inside the aggregate literal `e`, with the
    /// field / tuple-index path that reaches it, recorded as `(x, y, path)`.
    /// Only struct and tuple literals nest; anything else ends the walk.
    fn collect_wrap_sources(
        e: &Expr,
        path: &mut ParamPath,
        x: &str,
        out: &mut Vec<(String, String, ParamPath)>,
    ) {
        match &e.kind {
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    path.push(ParamPart::Field(f.name.clone()));
                    Self::collect_wrap_sources(&f.value, path, x, out);
                    path.pop();
                }
            }
            ExprKind::Tuple(elems) => {
                for (i, el) in elems.iter().enumerate() {
                    path.push(ParamPart::TupleIndex(i));
                    Self::collect_wrap_sources(el, path, x, out);
                    path.pop();
                }
            }
            ExprKind::Identifier(y) if !path.is_empty() => {
                out.push((x.to_string(), y.clone(), path.clone()));
            }
            _ => {}
        }
    }
    fn block(&mut self, b: &Block) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Let {
                    is_mut,
                    pattern,
                    value,
                    ..
                } => {
                    self.bind(pattern);
                    if let (false, PatternKind::Binding(x), ExprKind::Identifier(y)) =
                        (*is_mut, &pattern.kind, &value.kind)
                    {
                        self.rebinds.push((x.clone(), y.clone()));
                    }
                    if let (true, PatternKind::Binding(x), ExprKind::Identifier(y)) =
                        (*is_mut, &pattern.kind, &value.kind)
                    {
                        self.mut_rebinds.push((x.clone(), y.clone()));
                    }
                    if let (false, PatternKind::Binding(x)) = (*is_mut, &pattern.kind) {
                        let mut path: ParamPath = Vec::new();
                        Self::collect_wrap_sources(value, &mut path, x, &mut self.wraps);
                        if let ExprKind::Call { callee, args } = &value.kind {
                            for a in args {
                                let mut inner: Vec<(String, String, ParamPath)> = Vec::new();
                                let mut path: ParamPath = Vec::new();
                                if let ExprKind::Identifier(y) = &a.value.kind {
                                    inner.push((x.clone(), y.clone(), Vec::new()));
                                } else {
                                    Self::collect_wrap_sources(&a.value, &mut path, x, &mut inner);
                                }
                                for (x, y, path) in inner {
                                    self.ctor_wraps.push((x, y, (**callee).clone(), path));
                                }
                            }
                        }
                    }
                    if let (false, PatternKind::Binding(x)) = (*is_mut, &pattern.kind) {
                        if let Some((root, chain)) = place_chain_root_and_path(value) {
                            if !chain.is_empty() {
                                self.proj_rebinds.push((x.clone(), root, chain));
                            }
                        }
                    }
                    if let (false, PatternKind::Binding(x), ExprKind::Call { callee, args }) =
                        (*is_mut, &pattern.kind, &value.kind)
                    {
                        let key = match &callee.kind {
                            ExprKind::Identifier(g) => Some(g.clone()),
                            ExprKind::Path { segments, .. } => Some(segments.join(".")),
                            _ => None,
                        };
                        if let Some(key) = key {
                            let idents = args
                                .iter()
                                .enumerate()
                                .filter_map(|(i, a)| match &a.value.kind {
                                    ExprKind::Identifier(n) => Some((i, n.clone())),
                                    _ => None,
                                })
                                .collect();
                            self.call_rebinds.push((x.clone(), key, idents));
                        }
                    }
                    self.expr(value);
                }
                StmtKind::LetElse {
                    pattern,
                    value,
                    else_block,
                    ..
                } => {
                    self.bind(pattern);
                    self.expr(value);
                    self.block(else_block);
                }
                StmtKind::LetUninit { name, .. } => {
                    *self.bound.entry(name.clone()).or_insert(0) += 1;
                }
                StmtKind::Assign { value, .. } | StmtKind::CompoundAssign { value, .. } => {
                    self.expr(value)
                }
                StmtKind::Expr(e) => self.expr(e),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            self.expr(fe);
        }
    }
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.expr(condition);
                self.block(then_block);
                if let Some(x) = else_branch.as_deref() {
                    self.expr(x);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.bind(pattern);
                self.expr(value);
                self.block(then_block);
                if let Some(x) = else_branch.as_deref() {
                    self.expr(x);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for a in arms {
                    self.bind(&a.pattern);
                    if let Some(g) = &a.guard {
                        self.expr(g);
                    }
                    self.expr(&a.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.expr(condition);
                self.block(body);
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                self.bind(pattern);
                self.expr(value);
                self.block(body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.bind(pattern);
                self.expr(iterable);
                self.block(body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => self.block(body),
            ExprKind::Return(Some(inner)) => self.expr(inner),
            _ => {}
        }
    }
}

/// The rebind walk over `f`'s body, seeded with its parameter names.
fn rebind_walk(f: &Function) -> RebindWalk {
    let mut w = RebindWalk {
        rebinds: Vec::new(),
        mut_rebinds: Vec::new(),
        wraps: Vec::new(),
        ctor_wraps: Vec::new(),
        proj_rebinds: Vec::new(),
        call_rebinds: Vec::new(),
        bound: std::collections::HashMap::new(),
    };
    for p in &f.params {
        if let Some(n) = p.name() {
            *w.bound.entry(n.to_string()).or_insert(0) += 1;
        }
    }
    w.block(&f.body);
    w
}

/// B-2026-09-09-13, widened by B-2026-09-09-17 — is `param_name` rebound whole
/// into a local (`fn f(value: Option[Val]) { let mut vv = value; .. }`, or the
/// bare `let y = value;`), directly or through a chain of aliases?
///
/// This is an OWNERSHIP question, not an aliasing one, which is why it does not
/// go through [`param_whole_aliases`]: that closure admits only immutable
/// rebinds, because a name that can be reassigned is no longer a reliable alias
/// of the param. Ownership does not care which kind it is — either way the
/// local holds the param's heap, so a CALLER that also owns that heap frees it
/// twice.
///
/// B-2026-09-09-13 ASKED `mut_rebinds` ALONE, reasoning that "a mutable rebind
/// is precisely the shape at issue, since the reassignment is what frees the
/// displaced box". The reassignment is ONE way the local frees the box; SCOPE
/// EXIT is the other, and it needs no `mut`. `let y = x;` gives the local a
/// drop that runs at the end of the frame over a box the caller also
/// registered — the same two-owner shape with a different trigger. Reproduced
/// at f6f5818e with both earlier rows fixed, `KARAC_OPT_LEVEL=0`,
/// `KARAC_AUTO_PAR=0`, valgrind, three calls: `fn show(x: Option[K])` with
/// `let y = x;` frees 3 more blocks than it allocates, and the STRUCT payload
/// `Option[R2]` frees 12 more — 1 and 4 per call respectively, and the count
/// does not scale with the number of rebindings.
///
/// Both rebind sets are read against the SAME alias closure, so a chain mixing
/// the two (`let y = x; let mut z = y;`) is caught at either link.
///
/// Over-approximates on purpose. A false positive stands the caller down and
/// costs at most a leak; a false negative is a double free.
pub fn param_rebound_into_local(f: &Function, param_name: &str) -> bool {
    let w = rebind_walk(f);
    let aliases = close_rebind_aliases(&w, param_name);
    w.rebinds
        .iter()
        .chain(w.mut_rebinds.iter())
        .any(|(_, y)| aliases.iter().any(|a| a == y))
}

/// The transitive whole-rebind closure of `seed` over `w.rebinds`, admitting
/// only targets bound exactly once (see [`param_rebind_aliases`]).
fn close_rebind_aliases(w: &RebindWalk, seed: &str) -> Vec<String> {
    let mut aliases = vec![seed.to_string()];
    loop {
        let before = aliases.len();
        for (x, y) in &w.rebinds {
            if aliases.iter().any(|a| a == y)
                && !aliases.iter().any(|a| a == x)
                && w.bound.get(x.as_str()) == Some(&1)
            {
                aliases.push(x.clone());
            }
        }
        if aliases.len() == before {
            return aliases;
        }
    }
}

/// B-2026-09-06-9 — every name that IS one of `f`'s by-value parameters for
/// ownership purposes: each such parameter, its [`param_rebind_aliases`], and
/// — PROGRAM-AWARE, which is why the predicates do not share this — any
/// `let x = g(.., a, ..)` where `a` is already in the set and `g` returns that
/// parameter on every exit (`fn_always_returns_param`), transitively. A
/// callee is a free function or a `Type.assoc` fn; an unknown or instance
/// callee contributes nothing. Borrowed (`ref` / `mut ref`) parameters are
/// excluded; a destructuring parameter binds no whole name and contributes
/// nothing.
///
/// This is the set a `let w = keeps(x)` site consults to decide whether the
/// call is a whole rebind of a parameter the CALLER still fires (caller
/// retains: `x` is the param, a whole rebind of it, or such a call's result)
/// or hands over a PART bound out of one (`let (inner, y) = h.pe;
/// keep(inner)`), where the caller's walk already stands down through the
/// part channel (`fn_escaping_param_part_paths`) and the result binding is the
/// one owner. Measured: marking the part spelling a view as well ran ZERO
/// bodies. Both backends read this one set, so the split cannot drift.
pub fn fn_whole_param_aliases(
    program: &crate::Program,
    f: &Function,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for p in &f.params {
        if matches!(
            p.ty.kind,
            crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
        ) {
            continue;
        }
        if let Some(name) = p.name() {
            out.extend(param_whole_aliases(Some(program), f, name));
        }
    }
    out
}

/// Is `callee` the path `Enum.Variant` of a tuple variant declared in
/// `program`? A user enum constructor is spelled exactly like an associated
/// function call, and only the declarations can tell the two apart.
pub fn is_user_variant_ctor(program: &crate::Program, callee: &Expr) -> bool {
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    let [enum_name, variant] = segments.as_slice() else {
        return false;
    };
    program.items.iter().any(|item| match item {
        Item::EnumDef(def) if &def.name == enum_name => {
            def.variants.iter().any(|v| &v.name == variant)
        }
        _ => false,
    })
}

/// A callee key as a call site spells it — a bare free-function name, or
/// `Type.assoc` for an associated function — resolved to its AST. Instance
/// methods are never returned: their receiver shifts the argument indices, and
/// the method registrars own that leg.
fn resolve_free_or_assoc_fn<'p>(program: &'p crate::Program, key: &str) -> Option<&'p Function> {
    match key.split_once('.') {
        None => program.items.iter().find_map(|item| match item {
            Item::Function(g) if g.name == key => Some(g),
            _ => None,
        }),
        Some((ty, m)) => program.items.iter().find_map(|item| match item {
            Item::ImplBlock(b) => {
                let crate::ast::TypeKind::Path(pth) = &b.target_type.kind else {
                    return None;
                };
                if pth.segments.last().map(String::as_str) != Some(ty) {
                    return None;
                }
                b.items.iter().find_map(|ii| match ii {
                    ImplItem::Method(g) if g.name == m && g.self_param.is_none() => Some(&**g),
                    _ => None,
                })
            }
            _ => None,
        }),
    }
}

/// B-2026-09-06-12 — [`param_rebind_aliases`] plus, when a `program` is given,
/// the CALL spelling of the same rebind: `let x = g(.., a, ..)` where `a` is
/// already an alias and `g` hands that parameter back on every exit. This is
/// the per-parameter form the two alias-aware predicates consume, so a caller
/// that stands down for `fn f(r: R) -> R { let w = keeps(r); return w; }`
/// and the callee-side flip that registers for its conditional sibling read
/// one alias set.
///
/// ONE LEVEL, the direction this family runs on: `g` is judged by its OWN plain
/// aliases (`program = None` when asking it), so a chain of call-rebinds across
/// functions is not followed and mutual recursion cannot loop. `None` — the
/// answer a consumer with no program at hand gives — is exactly the pre-existing
/// plain-rebind set.
pub fn param_whole_aliases(
    program: Option<&crate::Program>,
    f: &Function,
    param_name: &str,
) -> Vec<String> {
    let w = rebind_walk(f);
    let mut aliases = close_rebind_aliases(&w, param_name);
    let Some(program) = program else {
        return aliases;
    };
    loop {
        let before = aliases.len();
        for (x, key, idents) in &w.call_rebinds {
            if aliases.iter().any(|a| a == x) || w.bound.get(x.as_str()) != Some(&1) {
                continue;
            }
            let Some(g) = resolve_free_or_assoc_fn(program, key) else {
                continue;
            };
            if idents.iter().any(|(i, n)| {
                aliases.iter().any(|a| a == n)
                    // B-2026-09-06-53 — a SCALAR argument does not make the
                    // call's result an alias of it. `let x = mk(i)` over
                    // `fn mk(i: i64) -> R` satisfies the predicate beside this
                    // (the parameter travels into the returned aggregate), and
                    // an `i64` owns nothing, so calling `x` an alias of `i` made
                    // every later question about `x` answer for a scalar: the
                    // interpreter then lost the `R`'s `Drop` body one hand-off
                    // later (`let y = keep(x)` printed nothing where both
                    // compiled backends printed one body), a run-vs-build split.
                    && !g
                        .params
                        .get(*i)
                        .is_some_and(|p| type_expr_is_owned_scalar(&p.ty))
                    // B-2026-09-06-58 — nor an alias THROUGH a callee that
                    // wraps the value in a `Drop`-bearing type of its own: the
                    // local holds a new object with a body of its own, not the
                    // param under another name.
                    && !fn_return_carries_own_drop_beyond_param(program, g, *i)
                    && fn_always_returns_param(None, g, *i)
            }) {
                aliases.push(x.clone());
            }
        }
        if aliases.len() == before {
            // A plain rebind of a call-alias (`let v = w;`) joins too.
            let closed = close_many(&w, &aliases);
            if closed.len() == aliases.len() {
                return aliases;
            }
            aliases = closed;
        }
    }
}

/// Close `seed` (already alias-closed individually) over the plain rebinds once
/// more, so a `let v = w;` after a call-rebind of `w` is picked up.
/// B-2026-09-06-19 — the locals that WRAP `param_name` (or one of its whole
/// aliases) inside an aggregate literal, each with the path at which the param
/// sits: `let p = P2 { r: r, n: 1 }` gives `(p, [Field(r)])`, a nested literal
/// a longer path, and a whole rebind of a wrapper (`let q = p`) or a wrap of a
/// wrapper (`let w = W { p: p }`) composes. Only once-bound, non-`mut` locals,
/// exactly as [`param_whole_aliases`] requires. Consumed by the hand-back
/// predicates so `return p`, `return p.r` and `return Box2 { r: p.r }` count
/// as yielding the param — the caller-side stand-down and the per-path
/// clearing at the return then follow the same path.
pub fn param_wrap_aliases(
    program: Option<&crate::Program>,
    f: &Function,
    param_name: &str,
) -> Vec<(String, ParamPath)> {
    param_wrap_aliases_ex(program, f, param_name, false)
}

/// [`param_wrap_aliases`], optionally counting ENUM-constructor wraps
/// (`let o = Some(r)`) as well — B-2026-09-26-27. Only the ALL-paths
/// predicates ask for them. A may-return (union) predicate that saw
/// `let o = Some(r); if c { return o } return None` as handing `r` back stood
/// the caller down on the `None` path too, where the callee keeps `o` as a
/// view of the caller's value and runs nothing, so the body was LOST (measured
/// on all four surfaces); the conditional hand-back's per-path flag keys on
/// the bare param and cannot see the wrapper.
pub fn param_wrap_aliases_ex(
    program: Option<&crate::Program>,
    f: &Function,
    param_name: &str,
    with_ctor: bool,
) -> Vec<(String, ParamPath)> {
    let w = rebind_walk(f);
    let whole = param_whole_aliases(program, f, param_name);
    // B-2026-09-26-27 — an enum constructor wraps its payload as a struct
    // literal wraps a field: `fn f(s: P) -> Option[P] { let o = Some(s);
    // return o }` hands `s` back exactly as `return Some(s)` does. Without it
    // the caller, never stood down, ran `s`'s `Drop` body beside the result's,
    // on all four surfaces. The seeded `Option`/`Result` constructors are known
    // by name; a user variant needs the program to be told from an associated
    // function. The path is the nesting INSIDE the payload: an enum cannot be
    // projected into, so only the wrapper itself (or a place reaching that
    // nesting through a later struct wrap) yields the param.
    let ctor_ok = |callee: &Expr| match &callee.kind {
        ExprKind::Identifier(n) => {
            matches!(n.as_str(), "Some" | "Ok" | "Err")
                && !program.is_some_and(|p| {
                    p.items
                        .iter()
                        .any(|it| matches!(it, Item::Function(g) if &g.name == n))
                })
        }
        ExprKind::Path { segments, .. } => {
            matches!(segments.as_slice(), [h, _] if h == "Option" || h == "Result")
                || program.is_some_and(|p| is_user_variant_ctor(p, callee))
        }
        _ => false,
    };
    let all_wraps: Vec<(String, String, ParamPath)> = w
        .wraps
        .iter()
        .cloned()
        .chain(
            w.ctor_wraps
                .iter()
                .filter(|(_, _, c, _)| with_ctor && ctor_ok(c))
                .map(|(x, y, _, path)| (x.clone(), y.clone(), path.clone())),
        )
        .collect();
    let mut out: Vec<(String, ParamPath)> = Vec::new();
    loop {
        let before = out.len();
        for (x, y, path) in &all_wraps {
            if w.bound.get(x.as_str()) != Some(&1) || out.iter().any(|(a, _)| a == x) {
                continue;
            }
            if whole.iter().any(|a| a == y) {
                out.push((x.clone(), path.clone()));
            } else if let Some((_, inner)) = out.iter().find(|(a, _)| a == y) {
                let mut full = path.clone();
                full.extend(inner.iter().cloned());
                out.push((x.clone(), full));
            }
        }
        for (x, y) in &w.rebinds {
            if w.bound.get(x.as_str()) != Some(&1) || out.iter().any(|(a, _)| a == x) {
                continue;
            }
            if let Some((_, inner)) = out.iter().find(|(a, _)| a == y) {
                out.push((x.clone(), inner.clone()));
            }
        }
        // B-2026-09-26-27 — a wrapper handed through a callee that returns it
        // on every exit (`let p = keep(o)` over `fn keep(o: Option[P]) ->
        // Option[P] { return o }`) is the same wrapper under a new name, as a
        // whole param through such a call is (`param_whole_aliases`).
        if let Some(program) = program {
            for (x, key, idents) in &w.call_rebinds {
                if w.bound.get(x.as_str()) != Some(&1) || out.iter().any(|(a, _)| a == x) {
                    continue;
                }
                let Some(g) = resolve_free_or_assoc_fn(program, key) else {
                    continue;
                };
                let hit = idents.iter().find_map(|(i, n)| {
                    let (_, inner) = out.iter().find(|(a, _)| a == n)?;
                    (!fn_return_carries_own_drop_beyond_param(program, g, *i)
                        && fn_always_returns_param(None, g, *i))
                    .then(|| inner.clone())
                });
                if let Some(inner) = hit {
                    out.push((x.clone(), inner));
                }
            }
        }
        // `let q = p.r`: a projection rebind that reaches the wrap path (or a
        // prefix of it) is the param, or a shallower wrapper of it, under a
        // new name. One that goes PAST the path is a part of the param and is
        // not followed.
        for (x, root, chain) in &w.proj_rebinds {
            if w.bound.get(x.as_str()) != Some(&1) || out.iter().any(|(a, _)| a == x) {
                continue;
            }
            if let Some((_, wpath)) = out.iter().find(|(a, _)| a == root) {
                if chain.len() <= wpath.len() && wpath.starts_with(chain) {
                    out.push((x.clone(), wpath[chain.len()..].to_vec()));
                }
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// `y.f.0.g` → `(y, [Field(f), TupleIndex(0), Field(g)])`; a bare `y` gives
/// an empty path. `None` for anything that is not a field / tuple-index chain
/// over an identifier.
fn place_chain_root_and_path(e: &Expr) -> Option<(String, ParamPath)> {
    let mut chain: ParamPath = Vec::new();
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::FieldAccess { object, field } => {
                chain.push(ParamPart::Field(field.clone()));
                cur = object;
            }
            ExprKind::TupleIndex { object, index } => {
                chain.push(ParamPart::TupleIndex(*index as usize));
                cur = object;
            }
            ExprKind::Identifier(n) => {
                chain.reverse();
                return Some((n.clone(), chain));
            }
            _ => return None,
        }
    }
}

/// Does the place `e` (a bare identifier, or a field / tuple-index chain)
/// yield a value that CONTAINS the wrapped param — the wrapper itself, or a
/// projection down to (a prefix of) the wrap path? A projection that goes
/// PAST the param (`p.r.id`) is a part of the param, not the param, so it
/// does not.
fn place_yields_wrapped_param(e: &Expr, wraps: &[(String, ParamPath)]) -> bool {
    let Some((root, chain)) = place_chain_root_and_path(e) else {
        return false;
    };
    wraps
        .iter()
        .any(|(a, path)| *a == root && chain.len() <= path.len() && path.starts_with(&chain))
}

fn close_many(w: &RebindWalk, seed: &[String]) -> Vec<String> {
    let mut aliases: Vec<String> = seed.to_vec();
    loop {
        let before = aliases.len();
        for (x, y) in &w.rebinds {
            if aliases.iter().any(|a| a == y)
                && !aliases.iter().any(|a| a == x)
                && w.bound.get(x.as_str()) == Some(&1)
            {
                aliases.push(x.clone());
            }
        }
        if aliases.len() == before {
            return aliases;
        }
    }
}

/// B-2026-09-05-13 — the names by-value parameter `param_name` is REBOUND to,
/// whole and unchanged, by a plain `let m = r;` — transitively, so `let m = r;
/// let n = m;` yields `[r, m, n]`. The parameter itself is always element 0.
///
/// The passthrough family is name-keyed: every predicate below asks whether a
/// return leaf IS the parameter's identifier, so `fn f(r: R) -> Option[R] {
/// let m = r; return Option.Some(m); }` answered "does not return `r`" while
/// `m` carried the caller-retained value out — and the caller, never stood
/// down, ran the `Drop` body a second time over the result binding
/// (measured on all four surfaces, unconditional and conditional alike — the
/// coverage limit B-2026-09-05-10 split out rather than reach for). This is the
/// one alias walk those predicates share, so they follow the same rebinds.
///
/// A rebind is admitted as an alias only when it is provably the SAME VALUE
/// under a new name on every path that can reach a leaf naming it:
///
///  * the pattern is a bare, IMMUTABLE binding and the initializer is a bare
///    identifier already in the set — a whole-value move, nothing built
///    around it. `mut` is declined because a later `m = other;` would leave
///    `m` naming a different value than the one the parameter brought in;
///  * the target name is BOUND EXACTLY ONCE in the body (params counted), by
///    any `let` / `let … else` / `if let` / `while let` / `for` / match-arm
///    pattern the leaf walkers can see. A name bound twice — a shadow, or the
///    same spelling in two branches — is declined outright, so a leaf that
///    names it can never be read as the parameter when it holds something
///    else. Closures are not entered, exactly as the leaf walkers do not enter
///    them: a binding inside one is not visible at any of `f`'s exits.
///
/// Declining keeps today's behaviour for that function, which is the
/// direction this whole family runs on: a missed alias is a body that keeps
/// firing where it always did, never a new silent loss.
///
/// CONSUMERS must follow the alias through the callee-side per-path flip as
/// well as through the admission predicate — a one-predicate alias fix was
/// measured to stand the caller down and then drop the parameter by NAME on
/// the non-escaping path while the value lived in the rebound local, losing
/// that body. Both backends hand the flip's registration from `r` to `m` at
/// the rebind site (codegen: `compile_let`'s param-view arm; interpreter:
/// `let_destructures_owned_param`), which is what keeps the caller's
/// stand-down and the callee's per-path drop naming the same binding on
/// every path.
pub fn param_rebind_aliases(f: &Function, param_name: &str) -> Vec<String> {
    close_rebind_aliases(&rebind_walk(f), param_name)
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
/// B-2026-09-06-53 — can a value of `ty` OWN anything a `Drop` body or a heap
/// free could reach: is it anything other than a primitive scalar?
///
/// The question the call-result VIEW classifiers have to ask before concluding
/// that a call's result is a view of one of its arguments. "The parameter is
/// handed back" ([`fn_always_returns_param`]) is true of a callee that merely
/// STORES the parameter into the aggregate it returns — `fn mkUses(i: i64) -> R
/// { return R { id: i, .. }; }` — and for an aggregate parameter that is the
/// right conclusion: the returned value carries the argument's heap, so the
/// argument's owner runs the body. For a SCALAR parameter it is not. An `i64`
/// owns nothing, has no body and no owner to defer to, so marking the result a
/// view left the `R` with no owner at all and its `Drop` body ran nowhere, on
/// every backend at every opt level, with valgrind clean because the MEMORY
/// side was never in doubt.
///
/// The same scalar false positive B-2026-09-06-26 and B-2026-09-06-41 fixed one
/// site over, where a scalar leaf of an owned-param destructure stood the
/// caller's walk down.
///
/// Bare primitive paths only. A generic parameter is spelled `T` here and
/// answers `false` (not a scalar) even when instantiated with one — the
/// direction that keeps today's behaviour rather than inventing a new one, and
/// the same coverage limit the neighbouring per-monomorph predicates carry.
pub fn type_expr_is_owned_scalar(ty: &TypeExpr) -> bool {
    let crate::ast::TypeKind::Path(p) = &ty.kind else {
        return false;
    };
    if p.generic_args.is_some() || p.segments.len() != 1 {
        return false;
    }
    matches!(
        p.segments[0].as_str(),
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "Unit"
    )
}

/// B-2026-09-06-58 — does the callee's RETURN type declare a user `Drop` of its
/// own that parameter `arg_index`'s type does not carry for it?
///
/// The second question a call-result VIEW classifier has to ask, beside
/// [`type_expr_is_owned_scalar`]. "The parameter is handed back" is also true
/// of a callee that WRAPS it in a different type — `fn mk2(i: i64, s: String)
/// -> R { return R { id: i, name: s }; }`, `fn wrapH(r: R) -> H { return H { r:
/// r, .. }; }` — and the view mark then defers the RESULT's body to whoever
/// owns the argument. That owner runs the argument's body, which is not the
/// result's: an `R` built around a `String` parameter, and an `H` built around
/// an `R` parameter, each ran their own `Drop` body NOWHERE, on every backend
/// at both opt levels, with valgrind clean.
///
/// SAME-TYPE hand-backs keep the view, which is the case the classifier exists
/// for: `fn keep(r: R) -> R { return r; }` returns the very value the argument
/// owns, so its body belongs to the argument's owner and running it at the
/// result binding as well would double it.
///
/// A return type with no user `Drop` of its own also keeps the view — `fn wrap(r:
/// R) -> W { W { r: r } }` for a body-less `W` hands nothing new to run, and its
/// field's body is the argument's.
///
/// Bare declared paths on both sides. A generic return (`Option[R]`) names
/// `Option`, which declares no `Drop`, so those keep today's behaviour — the
/// same per-monomorph coverage limit the neighbouring predicates carry.
/// B-2026-09-06-63 — does `f` WRAP argument `arg_index` in a DIFFERENT type
/// that declares its own `impl Drop`?
///
/// [`fn_return_carries_own_drop_beyond_param`] without its final clause, and
/// that omission is the whole point. That predicate refuses a parameter which
/// itself carries a `Drop`, because its consumer declines the VIEW outright and
/// doing so for such a parameter trades a lost body for a doubled one
/// (`dH2 dR43 dR43`) — the remainder it filed as this row. This one is asked by
/// a consumer that keeps the view and adds only the wrapper's OWN body, for
/// which a `Drop`-bearing parameter is exactly the case that needs serving.
///
/// The IDENTITY exclusion is kept and is load-bearing: `fn keeps(r: R) -> R`
/// hands the same object back, so its body is the parameter's own and the
/// caller already fires it. Admitting it doubles that body — measured
/// `v=9 dR9 dR9` while building this, on both backends.
///
/// Its OWN `Drop`, via `drop_method_keys`, rather than
/// `type_carries_user_drop`'s reachable-inside sense: the consumer registers
/// `<T>.drop`, which exists only for a type that declares one. A wrapper that
/// merely CONTAINS a `Drop` field has no body of its own to add, and its
/// field's body is the argument owner's.
pub fn fn_return_wraps_param_in_own_drop_type(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> Option<String> {
    let ret = f.return_type.as_ref()?;
    let crate::ast::TypeKind::Path(rp) = &ret.kind else {
        return None;
    };
    let [ret_name] = rp.segments.as_slice() else {
        return None;
    };
    if !program.drop_method_keys.contains_key(ret_name.as_str()) {
        return None;
    }
    let param = f.params.get(arg_index)?;
    let crate::ast::TypeKind::Path(pp) = &param.ty.kind else {
        return None;
    };
    if pp.segments.last() == Some(ret_name) {
        return None;
    }
    Some(ret_name.clone())
}

pub fn fn_return_carries_own_drop_beyond_param(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> bool {
    let Some(ret) = f.return_type.as_ref() else {
        return false;
    };
    let crate::ast::TypeKind::Path(rp) = &ret.kind else {
        return false;
    };
    let [ret_name] = rp.segments.as_slice() else {
        return false;
    };
    // The return type must carry a user `Drop` BODY — its own, or one reachable
    // inside it. `Option[R]` is the second form and is the one a bare
    // `drop_method_keys` lookup missed: `fn mkOptS(s: String) -> Option[R]` lost
    // the payload's body exactly as the bare `R` return did.
    if !type_carries_user_drop(program, ret, &mut Vec::new()) {
        return false;
    }
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let crate::ast::TypeKind::Path(pp) = &param.ty.kind else {
        return false;
    };
    if pp.segments.last() == Some(ret_name) {
        return false;
    }
    // The parameter must carry NO user `Drop` of its own, anywhere inside it.
    // Firing on one that does trades the lost body for a DOUBLED one: declining
    // the view gives the result binding full ownership, whose walk runs the
    // wrapped field's body beside the argument owner's — measured on
    // `fn wrapH(r: R) -> H`, where the `H`'s own body appeared (correctly) and
    // the `R`'s ran twice (`dH2 dR43 dR43`). That shape needs an ownership
    // split — the result's OWN body without its fields' — which is a wider
    // change than this row's, so it keeps today's behaviour and is filed on its
    // own.
    !type_carries_user_drop(program, &param.ty, &mut Vec::new())
}

/// Does `ty`, or anything reachable from it BY VALUE, declare a user `Drop`?
/// `visited` breaks recursive declarations; an unknown or non-path type answers
/// `true`, the direction that leaves a shape on today's behaviour.
fn type_carries_user_drop(
    program: &crate::Program,
    ty: &TypeExpr,
    visited: &mut Vec<String>,
) -> bool {
    let crate::ast::TypeKind::Path(p) = &ty.kind else {
        return true;
    };
    let Some(name) = p.segments.last() else {
        return true;
    };
    if let Some(args) = p.generic_args.as_ref() {
        if args.iter().any(|a| match a {
            crate::ast::GenericArg::Type(t) => type_carries_user_drop(program, t, visited),
            _ => false,
        }) {
            return true;
        }
    }
    if program.drop_method_keys.contains_key(name) {
        return true;
    }
    if type_expr_is_owned_scalar(ty)
        || matches!(
            name.as_str(),
            "String" | "Vec" | "Map" | "Set" | "Option" | "Result"
        )
    {
        // Builtin containers carry only what their arguments carry, which the
        // generic-argument walk above already answered.
        return false;
    }
    if visited.iter().any(|v| v == name) {
        return false;
    }
    visited.push(name.clone());
    for item in &program.items {
        match item {
            Item::StructDef(sd) if &sd.name == name => {
                return sd
                    .fields
                    .iter()
                    .any(|fl| type_carries_user_drop(program, &fl.ty, visited));
            }
            Item::EnumDef(ed) if &ed.name == name => {
                return ed.variants.iter().any(|v| match &v.kind {
                    VariantKind::Unit => false,
                    VariantKind::Tuple(tys) => tys
                        .iter()
                        .any(|t| type_carries_user_drop(program, t, visited)),
                    VariantKind::Struct(fs) => fs
                        .iter()
                        .any(|fl| type_carries_user_drop(program, &fl.ty, visited)),
                });
            }
            _ => {}
        }
    }
    true
}

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
/// B-2026-09-07-10 — what the ALL-PATHS walker needs in order to follow a
/// hand-back through one call: the program to resolve the inner callee in, and
/// this function's own name so self-recursion is not mistaken for a hop.
#[derive(Clone, Copy)]
struct ViaCtx<'a> {
    program: &'a crate::Program,
    self_name: &'a str,
}

pub fn fn_always_returns_param(
    program: Option<&crate::Program>,
    f: &Function,
    arg_index: usize,
) -> bool {
    fn_always_returns_param_ex(program, f, arg_index, false, false)
}

/// B-2026-09-26-15 — [`fn_always_returns_param`] with a payload-less `None`
/// admitted as the other kind of exit: every exit hands the param back (inside
/// `Some`, typically) or returns `None`, and at least one hands it back. So the
/// RESULT'S TAG says which happened — `Some` means the payload IS the param —
/// which is what lets a caller hand the param's memory to the result on exactly
/// the path that took it (`fn mid[T](v: T, c: bool) -> Option[T] { if c {
/// return Some(v) } return None }`).
pub fn fn_returns_param_or_none(
    program: Option<&crate::Program>,
    f: &Function,
    arg_index: usize,
) -> bool {
    fn_always_returns_param_ex(program, f, arg_index, false, true)
}

/// B-2026-09-07-10 — the ALL-PATHS form of [`fn_returns_param_via_call`]: does
/// every exit hand the param back, counting a hand-back that goes THROUGH a
/// callee which itself always returns it (`fn via(r: R) -> R { return f(r); }`)?
///
/// The caller's ADMISSION gate `call_arg_flows_into_return` learned the
/// one-hop route in B-2026-08-28-62 by ORing in `fn_returns_param_via_call`;
/// the STAND-DOWN gate beside it (`callee_takes_over_arg_drop_body`) never did,
/// so `via` was admitted by one and dropped by the other, and a named local
/// handed to it had two owners — the caller's binding and the result's.
///
/// It has to be the ALL-PATHS form rather than that ANY-path predicate for the
/// reason B-2026-09-06-61's fix records: the stand-down is the SUPPRESSING
/// direction, and a mixed-path callee's dies-inside leg registers bodies-only
/// "because the caller still owns the memory", so retracting the caller there
/// takes away that leg's only memory owner. Measured on the mixed spelling
/// (`fn mvia(r: R, c: bool) -> R { if c { return f(r); } return mk(99); }`):
/// the hand-back leg is the double free this closes, and the dies-inside leg is
/// clean today and stays clean, because this predicate declines the function
/// outright.
///
/// ONE HOP, and the inner callee is asked the ALL-PATHS question too, so the
/// chain cannot launder a mixed-path callee through a passthrough wrapper.
pub fn fn_always_returns_param_via_call(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> bool {
    fn_always_returns_param_ex(Some(program), f, arg_index, true, false)
}

fn fn_always_returns_param_ex(
    program: Option<&crate::Program>,
    f: &Function,
    arg_index: usize,
    allow_via_call: bool,
    none_ok: bool,
) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(name) = &param.pattern.kind else {
        return false;
    };
    // B-2026-09-05-13 — the param under every name it is rebound to whole
    // (`let m = r;`), so `return Option.Some(m)` reads as handing `r` back.
    // See `param_rebind_aliases` for what qualifies and why declining is safe.
    // B-2026-09-06-12 — and to a rebind THROUGH an always-returning callee
    // (`let w = keeps(r); return w`), when the caller can supply the program.
    let aliases = param_whole_aliases(program, f, name);
    // B-2026-09-26-27 — an ALL-paths question, so an enum-constructor wrap
    // counts (`let o = Some(r); return o`).
    let wraps = param_wrap_aliases_ex(program, f, name, true);
    let name: &[String] = &aliases;
    let wraps: &[(String, ParamPath)] = &wraps;

    /// The same test [`fn_returns_param`] applies at a return site: the bare
    /// identifier, or an aggregate literal that moves the param into itself.
    ///
    /// B-2026-09-05-10 — plus the param wrapped in an `Option`/`Result`
    /// constructor (`return Option.Some(r)`), the shape B-2026-08-31-46 taught
    /// the CONDITIONAL predicate to see. Widening this ALL-paths predicate is
    /// safe by its own semantics, where widening the union `fn_returns_param`
    /// was not: it answers true only when EVERY exit hands the param back, so a
    /// true answer stands the caller down only where the result binding
    /// provably owns the value on every path — there is no dies-inside path
    /// left to lose a body on. `option_result_ctor_payload` is the same shape
    /// test the conditional flip and both backends' tail walkers already share.
    fn yields(
        e: &Expr,
        name: &[String],
        wraps: &[(String, ParamPath)],
        program: Option<&crate::Program>,
        via: Option<ViaCtx>,
    ) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => {
                name.iter().any(|a| a == n) || place_yields_wrapped_param(e, wraps)
            }
            // B-2026-09-06-19 — a projection back out of a wrapping local.
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } => {
                place_yields_wrapped_param(e, wraps)
            }
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .any(|f| yields(&f.value, name, wraps, program, via)),
            ExprKind::Tuple(elems) => elems.iter().any(|el| yields(el, name, wraps, program, via)),
            // B-2026-09-25-16 — a user enum's variant constructor carries the
            // param out exactly as a struct literal does (`return E.A(a)`).
            ExprKind::Call { callee, args }
                if program.is_some_and(|p| is_user_variant_ctor(p, callee)) =>
            {
                args.iter()
                    .any(|a| yields(&a.value, name, wraps, program, via))
            }
            // B-2026-09-07-10 — the ONE-HOP route, when the caller asked for it:
            // `return f(r)` where `f` itself always returns that argument.
            ExprKind::Call { callee, args } if via.is_some() => {
                if let Some(p) = crate::ast::option_result_ctor_payload(e) {
                    if yields(p, name, wraps, program, via) {
                        return true;
                    }
                }
                let ViaCtx { program, self_name } = via.unwrap();
                let ExprKind::Identifier(g) = &callee.kind else {
                    return false;
                };
                // Self-recursion asks the same question of the same body.
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
                    (matches!(&a.value.kind, ExprKind::Identifier(n) if name.iter().any(|al| al == n))
                        // B-2026-09-26-27 — or a local that WRAPS the param
                        // (`let o = Some(r); return keep(o)`).
                        || matches!(&a.value.kind, ExprKind::Identifier(_))
                            && place_yields_wrapped_param(&a.value, wraps))
                        && fn_always_returns_param(Some(program), gf, j)
                })
            }
            _ => crate::ast::option_result_ctor_payload(e)
                .is_some_and(|p| yields(p, name, wraps, program, via)),
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

    let via = if allow_via_call {
        program.map(|p| ViaCtx {
            program: p,
            self_name: &f.name,
        })
    } else {
        None
    };
    let mut returns = Vec::new();
    return_operands_block(&f.body, &mut returns);
    // Is there a `return` that does NOT hand the param back? A bare `return;`
    // counts: it exits without yielding, so the param dies on that path.
    // B-2026-09-26-15 — a payload-less `None` exit, admitted only when asked.
    let is_none = |e: &Expr| {
        none_ok
            && match &e.kind {
                ExprKind::Identifier(n) => n == "None",
                ExprKind::Path { segments, .. } => {
                    segments.last().is_some_and(|s| s == "None")
                        && (segments.len() == 1 || segments[0] == "Option")
                }
                _ => false,
            }
    };
    let any_bad_return = returns
        .iter()
        .any(|o| !o.is_some_and(|x| yields(x, name, wraps, program, via) || is_none(x)));

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
        let any_good_return = returns
            .iter()
            .any(|o| o.is_some_and(|x| yields(x, name, wraps, program, via)));
        return f.return_type.is_some() && any_good_return && !any_bad_return;
    };
    let mut tails = Vec::new();
    leaf_tails(tail, &mut tails);
    if tails.is_empty()
        || !tails
            .iter()
            .all(|t| yields(t, name, wraps, program, via) || is_none(t))
    {
        return false;
    }
    // With `None` admitted, a body whose every exit is `None` hands nothing
    // back; require one exit that does.
    let any_good = tails.iter().any(|t| yields(t, name, wraps, program, via))
        || returns
            .iter()
            .any(|o| o.is_some_and(|x| yields(x, name, wraps, program, via)));
    !any_bad_return && any_good
}

/// B-2026-08-31-46 — is `e` an `Option`/`Result` CONSTRUCTOR around exactly one
/// operand? Returns that operand.
///
/// `return Option.Some(r)` hands `r` out of the frame exactly as `return r`
/// does, one constructor deeper. The conditional-move family recognised the
/// bare identifier and an aggregate LITERAL (`H { r: r }`, `(r, 9)`) but not
/// this, so a param escaping inside a returned `Some`/`Ok`/`Err` was claimed by
/// nobody: the caller's fresh-temp walk fired the body AND the result binding
/// fired it — two bodies on every compiled surface, one under `--interp` for
/// the method spelling only, and two on both backends for the named-binding
/// and free-function spellings.
///
/// ONE shape test, shared by the admission predicate
/// (`fn_conditionally_returns_param_bare`) and both backends' tail-source
/// walkers (`collect_aggregate_literal_sources`), so the three cannot drift on
/// what counts as a constructor. Mirrors `is_error_exit_value`'s idiom: a
/// qualified `Path` whose last segment is the variant, or the bare identifier
/// spelling.
///
/// Deliberately NOT folded into `fn_returns_param`: that predicate is the UNION
/// over return sites and is documented as turning a leak into corruption if its
/// answer moves. Recognising the wrap there would skip the caller-side drop on
/// EVERY call to such a callee — including the ones that take the non-escaping
/// path and print their body correctly today — trading one doubled body for two
/// lost ones and a leak. The conditional predicate is the right home because
/// per-path ownership is its whole job.
pub fn option_result_ctor_payload(e: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args } = &e.kind else {
        return None;
    };
    let is_ctor = match &callee.kind {
        ExprKind::Path { segments, .. } => segments
            .last()
            .is_some_and(|s| matches!(s.as_str(), "Some" | "Ok" | "Err")),
        ExprKind::Identifier(n) => matches!(n.as_str(), "Some" | "Ok" | "Err"),
        _ => false,
    };
    if !is_ctor {
        return None;
    }
    match args.as_slice() {
        [only] => Some(&only.value),
        _ => None,
    }
}

/// B-2026-09-13-13 — is `name` one of the built-in scalar type names, i.e. the
/// receiver of a DESUGARED operator call (`i64.add`, `f64.mul`, `bool.not`)?
///
/// The same head set [`type_expr_is_owned_scalar`] admits, minus `Unit`, which
/// has no operators. Kept here rather than reaching for
/// `codegen::param_own::is_primitive_type_name` because codegen containment
/// runs the other way: `ast` must not depend on the backend.
fn is_primitive_type_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}

/// B-2026-09-13-13 — the field names of the struct `ty` names whose own
/// declared type is an owned SCALAR, so a projection onto one of them
/// (`r.id` where `id: i64`) is a COPY READ and carries nothing out of the
/// frame.
///
/// Resolved from the PROGRAM's declarations rather than from a type
/// environment, which is what lets both backends ask the question and get the
/// same answer by construction — the same reason the hash, Arrow-IPC and
/// normalization twins share one implementation instead of two that agree by
/// convention. `fn_conditionally_returns_param_bare` is consulted by codegen
/// (four call legs) and by the interpreter (five sites); an oracle only one of
/// them could compute would be a run-vs-build divergence waiting to happen.
///
/// DELIBERATELY SYNTACTIC AND UNDER-APPROXIMATE. A generic head, a type alias,
/// a `shared` field, a tuple-typed parameter and any field whose own type is
/// not one of [`type_expr_is_owned_scalar`]'s heads all yield nothing, which
/// reproduces today's behaviour for that shape. Erring this way costs a missed
/// body; erring the other way would admit a leaf that really does carry the
/// param out, which is what condition 3 exists to refuse.
fn param_scalar_field_names(program: Option<&crate::Program>, ty: &TypeExpr) -> Vec<String> {
    let Some(p) = program else {
        return Vec::new();
    };
    let crate::ast::TypeKind::Path(path) = &ty.kind else {
        return Vec::new();
    };
    if path.generic_args.is_some() || path.segments.len() != 1 {
        return Vec::new();
    }
    let head = path.segments[0].as_str();
    for it in &p.items {
        let Item::StructDef(s) = it else {
            continue;
        };
        if s.name != head {
            continue;
        }
        // A `shared struct` is reference-semantics: a projection off one is a
        // different question (the RC channel's), so decline the whole type
        // rather than field by field.
        if s.is_shared || s.is_par {
            return Vec::new();
        }
        return s
            .fields
            .iter()
            .filter(|f| type_expr_is_owned_scalar(&f.ty))
            .map(|f| f.name.clone())
            .collect();
    }
    Vec::new()
}

/// B-2026-09-23-44 — a type every part of which is named concretely and
/// runs no user `Drop`: a scalar, `String`, a `Vec` / `Option` / `Result` /
/// tuple / fixed `Array` of such, or a non-generic, non-`shared`,
/// non-`par` struct with no `Drop` impl whose fields all are. Anything
/// else — a type parameter, a user enum, an RC type, a body anywhere
/// inside — is `false`, which keeps the `let`-bound expansion off it: a
/// body under a `Vec` is lost on the dies-inside exit by the tail spelling
/// too, and expanding would carry `--interp` into that gap with it.
/// B-2026-09-24-14 — also the payload gate for the codegen escaping-param
/// entry copy (`optres_escaping_param_entry_copied`), which is why it is
/// public rather than nested in its first consumer.
pub fn concrete_plain_type(
    program: Option<&crate::Program>,
    te: &crate::ast::TypeExpr,
    seen: &mut Vec<String>,
) -> bool {
    match &te.kind {
        crate::ast::TypeKind::Tuple(ts) => ts.iter().all(|t| concrete_plain_type(program, t, seen)),
        crate::ast::TypeKind::Array { element, .. } => concrete_plain_type(program, element, seen),
        crate::ast::TypeKind::Path(q) if q.segments.len() == 1 => {
            let n = q.segments[0].as_str();
            let args_ok = |seen: &mut Vec<String>| {
                q.generic_args.as_ref().is_some_and(|args| {
                    args.iter().all(|a| {
                        matches!(a, crate::ast::GenericArg::Type(t)
                            if concrete_plain_type(program, t, seen))
                    })
                })
            };
            match n {
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64"
                | "bool" | "char" | "String" | "isize" | "usize" => q.generic_args.is_none(),
                "Vec" | "Option" | "Result" => args_ok(seen),
                _ => {
                    if q.generic_args.is_some() {
                        return false;
                    }
                    if seen.iter().any(|x| x == n) {
                        return true;
                    }
                    let Some(sd) = program.and_then(|p| {
                        p.items.iter().find_map(|it| match it {
                            Item::StructDef(s) if s.name == n => Some(s),
                            _ => None,
                        })
                    }) else {
                        return false;
                    };
                    if sd.is_shared
                        || sd.is_par
                        || sd.generic_params.is_some()
                        || program.is_some_and(|p| p.drop_method_keys.contains_key(n))
                    {
                        return false;
                    }
                    seen.push(n.to_string());
                    sd.fields
                        .iter()
                        .all(|fd| concrete_plain_type(program, &fd.ty, seen))
                }
            }
        }
        _ => false,
    }
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
///   3. No leaf tail mentions the parameter in any OTHER way. This used to
///      rule out the aggregate-literal route `if k { H { r: r } } else { .. }`
///      as well, on a measurement (`drop 41` / `41` / `drop 41`, a double body
///      plus a read of the dropped value) taken when the flag was cleared only
///      for a bare identifier. Both backends' return-site retractions have
///      since become aggregate-aware and per path (B-2026-08-28-65,
///      B-2026-08-31-35/-46), so B-2026-09-02-4 admits that route as a
///      hand-over (`yields_wrapped` below); what condition 3 still declines is
///      a leaf that mentions the param without handing it out (`consume(r)`,
///      `r.id`), which no flag clears.
///   4. HISTORICAL, and no longer a condition: this used to read "no `return`
///      statement anywhere in the body mentions the parameter", on the grounds
///      that a `return`-borne escape was outside the flag's reach. It is not —
///      `collect_return_leaves` now folds every `return` operand into the leaf
///      set below, so the `return` spelling of a conditional hand-back is
///      analysed exactly like the block-tail one. Kept here as a pointer
///      because B-2026-08-30-23's plan was written against the old text and
///      proposed extending the per-path flag to reach these shapes; the leaves
///      already reach them, and what actually declined that row's reproducer
///      was condition 3 refusing a leaf that merely CONTAINED a call.
///
/// Paired with `!`[`fn_moves_param_into_outliving_place`] at the call site: a
/// param stored into `self` or a `ref` param outlives the frame by a route with
/// no tail at all.
///
/// The conservative direction is unchanged from the rest of this family — a
/// shape this predicate declines keeps today's missed body, which is a
/// leak-of-side-effect, never a double drop and never a memory fault.
pub fn fn_conditionally_returns_param_bare(
    program: Option<&crate::Program>,
    f: &Function,
    arg_index: usize,
) -> bool {
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
    // B-2026-09-05-13 — the param under every name it is rebound to whole
    // (`let m = r;`): a leaf `Option.Some(m)` yields it, and a leaf that
    // mentions `m` any other way declines exactly as one mentioning `r` does.
    // The per-path flip this predicate admits follows the same rebind at the
    // callee side, so the caller's stand-down and the callee's drop keep
    // naming one binding per path — see `param_rebind_aliases`.
    // B-2026-09-06-12 — and through an always-returning callee; see
    // `param_whole_aliases`.
    let aliases = param_whole_aliases(program, f, param_name);
    let wraps = param_wrap_aliases(program, f, param_name);
    // B-2026-09-06-19 — a wrapping local MENTIONS the param wherever it
    // appears (the conservative side of this predicate), and yields it where
    // `place_yields_wrapped_param` says so.
    let mut mention_names: Vec<String> = aliases.clone();
    mention_names.extend(wraps.iter().map(|(a, _)| a.clone()));
    let name: &[String] = &mention_names;
    let wraps: &[(String, ParamPath)] = &wraps;
    // B-2026-09-13-13 — the param type's own scalar fields, so a leaf that
    // mentions the param only by reading one of them is an exit rather than an
    // escape. Empty for every shape the syntactic resolution cannot settle,
    // which reproduces today's answer.
    let copy_fields = param_scalar_field_names(program, &param.ty);

    /// May `e` mention `name`? Conservative in the DECLINING direction: any
    /// shape not explicitly recognized answers `true`, which fails condition 3
    /// and leaves that function on today's behaviour. Adding a shape here can
    /// only ever admit more programs, never silently widen an escape route.
    fn may_mention(e: &Expr, name: &[String]) -> bool {
        match &e.kind {
            ExprKind::Identifier(n) => name.iter().any(|a| a == n),
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
            // B-2026-09-23-15 — an ARRAY literal mentions the parameter exactly
            // when one of its elements does, like the tuple above. Without this
            // arm the catch-all answered `true` for the canonical non-escaping
            // exit of an `Array` hand-back (`return [mk(8), mk(9)]`), so no
            // array param was ever admitted and its element bodies were lost on
            // every surface on the path where it died inside the callee.
            ExprKind::ArrayLiteral(elems) => elems.iter().any(|el| may_mention(el, name)),
            // B-2026-09-23-25 — and a PREFIX collection literal, which is also
            // what `vec![..]` desugars to, likewise. Without these two arms an
            // `Array[Vec[i64], 2]` hand-back whose other exit builds
            // `[vec![8, 8], vec![9]]` was declined, kept the static scope-exit
            // drop, and double-freed on the exit that hands the param back.
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                items.iter().any(|el| may_mention(el, name))
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                may_mention(value, name) || may_mention(count, name)
            }
            ExprKind::Binary { left, right, .. } => {
                may_mention(left, name) || may_mention(right, name)
            }
            ExprKind::Unary { operand, .. } => may_mention(operand, name),
            ExprKind::FieldAccess { object, .. } => may_mention(object, name),
            // B-2026-08-30-23 — a CALL mentions the parameter exactly when its
            // callee expression or one of its arguments does. Without these two
            // arms the catch-all answered `true` for every leaf containing a
            // call, and condition 3 then declined the whole function.
            //
            // That is what kept this row's own reproducer out of the mechanism
            // built for it. `fn pick(a: R, k: bool) -> R { if k { return R { id:
            // 98, s: mk(98) }; } return a; }` differs from an admitted program
            // only by the `mk(98)` in the arm that does NOT hand `a` back — a
            // call that cannot mention `a`, in the one leaf whose whole job is
            // to not mention it. Measured: replacing `mk(98)` with a string
            // literal makes both directions correct on all four lanes with no
            // compiler change at all, which is what localised the defect here
            // rather than in the per-path flag the row's plan proposed
            // extending.
            //
            // Faithful rather than permissive: `consume(a)` and `a.take()` both
            // recurse to the bare identifier and still decline, so no escape
            // route is admitted that the flag cannot clear. Every other shape
            // keeps the catch-all.
            ExprKind::Call { callee, args } => {
                may_mention(callee, name) || args.iter().any(|a| may_mention(&a.value, name))
            }
            ExprKind::MethodCall { object, args, .. } => {
                may_mention(object, name) || args.iter().any(|a| may_mention(&a.value, name))
            }
            // B-2026-08-31-46 — a bare PATH (`Option.None`, `Sig.B`, `H.zero`)
            // names a type, variant or associated item and can never name a
            // local binding, so it cannot mention the parameter. Without this
            // arm the catch-all declined every callee whose NON-escaping exit
            // was a qualified unit variant — which is the canonical partner of
            // a `return Option.Some(r)` exit — so the constructor widening
            // above never admitted the row's own reproducer.
            ExprKind::Path { .. } => false,
            _ => true,
        }
    }
    fn is_bare(e: &Expr, name: &[String]) -> bool {
        matches!(&e.kind, ExprKind::Identifier(n) if name.iter().any(|a| a == n))
    }
    /// B-2026-09-13-13 — are ALL of `e`'s mentions of the param COPY READS off
    /// it (`r.id` where `id: i64`), rather than routes the value can leave by?
    ///
    /// Condition 3 declines any leaf that mentions the param at all, on the
    /// stated grounds that "no per-path flag clears a leaf that READS the
    /// param". That is true of `consume(r)` and `r.take()` and NOT of a scalar
    /// field read: `return R { id: 90 + r.id }` reads a word out of `r` and
    /// leaves `r` to die inside the callee exactly as a constant leaf would, so
    /// the flag has nothing to clear and the per-path registration is safe.
    ///
    /// The cost of getting this wrong was a body count wrong in BOTH directions
    /// at once, and the two halves hid each other. Declining the function left
    /// no per-path owner at the callee, and the two caller legs then disagreed
    /// about what to do with that: the ASSOCIATED and METHOD legs gate on this
    /// predicate, so `false` read as "nobody else can own this argument" and
    /// they hung the full `karac_drop_<T>` wrapper on the caller's temp —
    /// `dR1 k:1 dR1` on every compiled surface against `--interp`'s correct
    /// `k:1 dR1`. The FREE leg gates on the `fn_returns_param` UNION instead,
    /// so it stood down on BOTH paths and the dies-inside path lost its body on
    /// all four surfaces — agreed, and therefore invisible to the A/B rule,
    /// which is why the row recorded the free position as "correct here".
    ///
    /// So the fix is the CALLEE side, not either caller's gate. The 2026-09-14
    /// attempt on this row widened `escapes_frame` to the union on the
    /// associated and method legs and was reverted: it fixed the hand-back path
    /// and turned the dies-inside path into a fresh divergence, because
    /// standing the caller down is only sound once the callee registers the
    /// per-path owner. Admitting the copy read is what makes the callee
    /// register it, and both caller legs then land on the same answer.
    ///
    /// A BARE mention is not a read, and a NON-scalar field is not either
    /// (`return R { s: r.s }` moves the string out) — `copy_fields` is keyed on
    /// the param type's scalar fields precisely so the two cannot be confused.
    /// A `Call` or `MethodCall` that mentions the param is declined whole:
    /// `consume(r)` hands it away and `r.take()` may, so they keep condition
    /// 3's answer. Every shape not listed declines, which is this family's
    /// standing direction — a missed body, never a double drop.
    fn mentions_only_as_copy_read(e: &Expr, name: &[String], copy_fields: &[String]) -> bool {
        match &e.kind {
            // THE one admitted shape. Does not recurse into `object`: this arm
            // IS the mention, and it carries nothing away.
            ExprKind::FieldAccess { object, field } => {
                if matches!(&object.kind, ExprKind::Identifier(n) if name.iter().any(|a| a == n)) {
                    return copy_fields.iter().any(|f| f == field);
                }
                mentions_only_as_copy_read(object, name, copy_fields)
            }
            ExprKind::Identifier(n) => !name.iter().any(|a| a == n),
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::Bool(_)
            | ExprKind::SelfValue
            | ExprKind::Path { .. } => true,
            ExprKind::InterpolatedStringLit(parts) => parts.iter().all(|p| match p {
                crate::ast::ParsedInterpolationPart::Text(_) => true,
                crate::ast::ParsedInterpolationPart::Expr(e, _) => {
                    mentions_only_as_copy_read(e, name, copy_fields)
                }
            }),
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .all(|f| mentions_only_as_copy_read(&f.value, name, copy_fields)),
            ExprKind::Tuple(elems) => elems
                .iter()
                .all(|el| mentions_only_as_copy_read(el, name, copy_fields)),
            ExprKind::Binary { left, right, .. } => {
                mentions_only_as_copy_read(left, name, copy_fields)
                    && mentions_only_as_copy_read(right, name, copy_fields)
            }
            ExprKind::Unary { operand, .. } => {
                mentions_only_as_copy_read(operand, name, copy_fields)
            }
            // A DESUGARED SCALAR OPERATOR, which is what `90 + r.id` actually
            // is by the time any predicate sees it: the parser lowers every
            // binary operator to a `Call` on a qualified path
            // (`i64.add(90, r.id)`), so the `Binary` arm above is not the arm
            // this row's leaf takes. Measured — the declining leaf printed as
            // `Call { callee: Path { segments: ["i64", "add"] }, .. }`, which
            // is why "a leaf that READS the param" and "a leaf containing a
            // call" are the same shape here and the `Call` arm below declined
            // it.
            //
            // Recursing into the arguments is sound because the receiver is a
            // PRIMITIVE type: `i64.add` cannot take an `R`, so no argument of
            // such a call can be the param itself, and each one is checked on
            // its own terms anyway. A call on any other path keeps the refusal.
            ExprKind::Call { callee, args }
                if matches!(
                    &callee.kind,
                    ExprKind::Path { segments, .. }
                        if segments.len() == 2 && is_primitive_type_name(&segments[0])
                ) =>
            {
                args.iter()
                    .all(|a| mentions_only_as_copy_read(&a.value, name, copy_fields))
            }
            // A call that does not mention the param at all is fine; one that
            // does keeps condition 3's refusal, whatever it does with it.
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } => !may_mention(e, name),
            _ => false,
        }
    }
    /// B-2026-09-02-4 — does the leaf hand the param out: bare, inside an
    /// `Option`/`Result` constructor, or moved into a returned AGGREGATE
    /// LITERAL (`Box2 { r: r }`, `(r, 2)`, nested)? The same shapes
    /// `fn_always_returns_param`'s `yields` and both backends' tail-source
    /// walkers (`collect_aggregate_literal_sources`) recognise, so a leaf
    /// admitted here is one the per-path flag is cleared for at the return.
    /// Condition 3's measurement against the aggregate route predates the
    /// flag-aware aggregate retraction (B-2026-08-28-65, B-2026-08-31-46);
    /// with it in place the wrap is a hand-over like any other, and declining
    /// it left `fn f(r: R, k: bool) -> Box2 { if k { return Box2 { r: mk() };
    /// } return Box2 { r: r }; }` with no owner on the dies-inside path for a
    /// fresh temp and two on the hand-back path for a named one.
    fn yields_wrapped_named(
        e: &Expr,
        name: &[String],
        wraps: &[(String, ParamPath)],
        program: Option<&crate::Program>,
        f_self_name: &str,
    ) -> bool {
        match &e.kind {
            ExprKind::Identifier(_) => is_bare(e, name),
            // B-2026-09-06-19 — a projection back out of a wrapping local.
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } => {
                place_yields_wrapped_param(e, wraps)
            }
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .any(|f| yields_wrapped_named(&f.value, name, wraps, program, f_self_name)),
            ExprKind::Tuple(elems) => elems
                .iter()
                .any(|el| yields_wrapped_named(el, name, wraps, program, f_self_name)),
            ExprKind::Call { callee, args }
                if program.is_some_and(|p| is_user_variant_ctor(p, callee)) =>
            {
                args.iter()
                    .any(|a| yields_wrapped_named(&a.value, name, wraps, program, f_self_name))
            }
            // B-2026-09-07-15 — the ONE-HOP hand-back leaf
            // (`if c { return f(r); } return mk(99);`). Without it `may_mention`
            // answers `true` for this leaf — correctly, it does mention `r` —
            // and condition 3 declines the whole function, so a MIXED-path
            // callee that hands its param back through one call was admitted by
            // no predicate at all: the caller kept its cleanup and the result
            // binding took a second one.
            //
            // The inner callee is asked the ALL-paths question
            // (`fn_always_returns_param`), the same test
            // `fn_always_returns_param_via_call` uses, so a chain cannot
            // launder a mixed-path callee through a passthrough wrapper. One
            // hop, and the argument must be the param (or one of its whole
            // aliases) BARE — the conservative direction this family runs on.
            ExprKind::Call { callee, args } => {
                if let (Some(p), ExprKind::Identifier(g)) = (program, &callee.kind) {
                    if g.as_str() != f_self_name {
                        if let Some(gf) = p.items.iter().find_map(|item| match item {
                            Item::Function(gf) if &gf.name == g => Some(gf),
                            _ => None,
                        }) {
                            if args.iter().enumerate().any(|(j, a)| {
                                is_bare(&a.value, name) && fn_always_returns_param(Some(p), gf, j)
                            }) {
                                return true;
                            }
                        }
                    }
                }
                option_result_ctor_payload(e)
                    .is_some_and(|pl| yields_wrapped_named(pl, name, wraps, program, f_self_name))
            }
            _ => option_result_ctor_payload(e)
                .is_some_and(|p| yields_wrapped_named(p, name, wraps, program, f_self_name)),
        }
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
            // B-2026-09-12-26 — a `return` reached AS A TAIL contributes its
            // OPERAND's leaves, not the `Return` node itself.
            //
            // A function whose exits are all `return` statements still has a
            // `final_expr` — the last `return` is an EXPRESSION in tail
            // position — so this walker was handed the `Return` node and its
            // catch-all pushed it whole. `may_mention` does not recognise
            // `ExprKind::Return` and answers `true` for it by design, so
            // condition 3 read that leaf as "mentions the param by a route no
            // flag clears" and declined the WHOLE function. Every exit of the
            // form `if c { return r } return mk(9)` was therefore invisible to
            // this predicate, while the same program with a TRAILING SEMICOLON
            // (`… return mk(9);`) was admitted — that makes the last `return` a
            // statement, leaves `final_expr` empty, and routes every exit
            // through `collect_return_leaves` alone. One character, and it is
            // why the nearest fixture cells (`method-return-both-exits-*`, all
            // of which carry the semicolon) covered none of this.
            //
            // What that cost is asymmetric across call positions, which is why
            // it read as an associated-vs-free defect rather than a shape one.
            // A FREE callee's caller declines its argument registration on
            // `call_arg_flows_into_return` — the `fn_returns_param` UNION,
            // which does see the `return` — so the miss showed only as a lost
            // body on the dies-inside path (agreed by both backends, so no A/B
            // gate reported it). The ASSOCIATED and METHOD legs gate on this
            // predicate instead, so a `false` here left `escapes_frame` false,
            // the caller registered the FULL `karac_drop_<T>` wrapper for the
            // argument temp, and its body ran beside the result binding's:
            // `dR1 / k:1 / dR1` at -O0, -O0 autopar and -O2 autopar against
            // `--interp`'s `k:1 / dR1`.
            //
            // Not a widening of what counts as a hand-back: `collect_return_-
            // leaves` has folded `return` operands into the leaf set since
            // condition 4, and this makes the TAIL spelling reach the same
            // leaves it already does for a `return` in statement position. The
            // duplicate leaf the two walkers now both contribute is harmless —
            // they classify it identically — and an UNCONDITIONAL `return r`
            // still yields no `yields_nothing` leaf and is still declined.
            ExprKind::Return(Some(inner)) => leaf_tails(inner, out),
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
    // B-2026-09-23-18 — a leaf naming a local that the body binds exactly once,
    // at its top level, by an immutable `let x = <if / if let / match>` stands
    // for that branch expression's own leaves. `let r = if c { a } else { mk() };
    // …; r` hands `a` back on one path exactly as the tail `if c { a } else
    // { mk() }` does, and the `let` initializer is already an escaping site on
    // both backends, so the arm tail that moves `a` into `r` clears `a`'s flag
    // the same way. Unexpanded, the single leaf `r` read as "never returns the
    // param": the caller kept its argument AND `r` handed the same value back,
    // two owners on the path that took the arm — both element bodies twice
    // under `--interp`, a double free on every compiled surface.
    //
    // ONLY for a parameter whose conditional return both backends can own
    // callee-side: a plain (non-generic, non-`shared`) user struct, or a fixed
    // `Array`. For any other type the flip stands the caller down with nothing
    // registered in the callee, so the dies-inside path LOSES its body — the
    // gap the tail spelling already has for an `Option[R]` parameter (`if c { a }
    // else { None }` runs no body at `c = false` on every surface) — and a
    // generic parameter flipped only the interpreter, splitting the backends.
    // B-2026-09-23-26 — and an `Option` / `Result` whose payload is such a
    // struct WITH a user `Drop`: both backends now register that payload's body
    // callee-side under the per-path flag (the `Option` / `Result` arm of the
    // conditional-return registration and `cond_returned_param_drop_names`).
    // Unexpanded, `let r: Option[R] = if c { a } else { None }; …; r` kept the
    // caller's argument armed while `r` handed it back: a segfault on every
    // compiled surface and the body twice under `--interp`.
    let droppable_struct = |te: &crate::ast::TypeExpr| {
        matches!(&te.kind, crate::ast::TypeKind::Path(q)
        if q.segments.len() == 1
            && q.generic_args.is_none()
            && program.is_some_and(|p| {
                p.drop_method_keys.contains_key(&q.segments[0])
                    && p.items.iter().any(|it| {
                        matches!(it, Item::StructDef(s)
                            if s.name == q.segments[0] && !s.is_shared && !s.is_par)
                    })
            }))
    };
    let expandable_param = match &param.ty.kind {
        crate::ast::TypeKind::Array { .. } => true,
        crate::ast::TypeKind::Path(path)
            if path.segments.len() == 1
                && matches!(path.segments[0].as_str(), "Option" | "Result") =>
        {
            path.generic_args.as_ref().is_some_and(|args| {
                args.iter()
                    .any(|a| matches!(a, crate::ast::GenericArg::Type(t) if droppable_struct(t)))
                    // B-2026-09-23-44 — or whose payloads are all CONCRETE
                    // plain types (`String`, `Vec[i64]`, a non-generic
                    // non-`shared` struct, …). Both backends leave such a
                    // param's MEMORY with the caller on every exit, and the
                    // caller already stands its result binding down for it
                    // on the syntactic union; what this `let`-bound spelling
                    // lacked was only the leaf, so `r` handed the argument's
                    // buffer back while the argument kept it — a double free
                    // on every compiled surface. A generic payload, or one
                    // with a `Drop` body anywhere inside, stays out.
                    || (f.generic_params.is_none()
                        && args.iter().all(|a| matches!(a,
                            crate::ast::GenericArg::Type(t)
                                if concrete_plain_type(program, t, &mut Vec::new()))))
            })
        }
        crate::ast::TypeKind::Path(path) if path.segments.len() == 1 => {
            path.segments[0] == "Array"
                || (path.generic_args.is_none()
                    && program.is_some_and(|p| {
                        p.items.iter().any(|it| {
                            matches!(it, Item::StructDef(s)
                                if s.name == path.segments[0] && !s.is_shared && !s.is_par)
                        })
                    }))
        }
        _ => false,
    };
    let bound = rebind_walk(f).bound;
    let branch_let_init = |x: &str| -> Option<&Expr> {
        if !expandable_param {
            return None;
        }
        f.body.stmts.iter().find_map(|st| match &st.kind {
            StmtKind::Let {
                is_mut: false,
                pattern,
                value,
                ..
            } if matches!(&pattern.kind, PatternKind::Binding(n) if n == x)
                && matches!(
                    value.kind,
                    ExprKind::If { .. } | ExprKind::IfLet { .. } | ExprKind::Match { .. }
                ) =>
            {
                Some(value)
            }
            _ => None,
        })
    };
    let leaves: Vec<&Expr> = leaves
        .into_iter()
        .flat_map(|leaf| {
            if let ExprKind::Identifier(x) = &leaf.kind {
                if !name.iter().any(|a| a == x) && bound.get(x.as_str()) == Some(&1) {
                    if let Some(init) = branch_let_init(x) {
                        let mut sub = Vec::new();
                        leaf_tails(init, &mut sub);
                        return sub;
                    }
                }
            }
            vec![leaf]
        })
        .collect();
    // A single leaf is the unconditional shape — no branch, nothing to guard.
    if leaves.len() < 2 {
        return false;
    }
    let mut yields_bare = false;
    let mut yields_nothing = false;
    for leaf in leaves {
        // B-2026-08-31-46 — a leaf that is the param wrapped in an
        // `Option`/`Result` constructor yields it too: the value crosses the
        // frame boundary inside the ctor exactly as it does bare, and the
        // per-path flag clears it through the same source walk.
        // B-2026-09-02-4 — and the param moved into a returned aggregate
        // literal; see `yields_wrapped`.
        if yields_wrapped_named(leaf, name, wraps, program, f.name.as_str()) {
            yields_bare = true;
        } else if may_mention(leaf, name) {
            // Condition 3 — an escape route the flag cannot clear, UNLESS every
            // mention is a scalar-field COPY READ (B-2026-09-13-13). A read
            // leaves the param to die inside the callee exactly as a constant
            // leaf would, so this counts as a non-yielding exit rather than a
            // refusal; see `mentions_only_as_copy_read` for what that cost.
            if !mentions_only_as_copy_read(leaf, name, &copy_fields) {
                return false;
            }
            yields_nothing = true;
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
    fn yields_via_call(
        program: &crate::Program,
        e: &Expr,
        name: &str,
        wr: &[String],
        self_name: &str,
    ) -> bool {
        match &e.kind {
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .any(|fi| yields_via_call(program, &fi.value, name, wr, self_name)),
            ExprKind::Tuple(elems) => elems
                .iter()
                .any(|el| yields_via_call(program, el, name, wr, self_name)),
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
                    (matches!(&a.value.kind, ExprKind::Identifier(n) if n == name)
                        // B-2026-09-26-27 — or a local that wraps the param
                        // (`let o = Some(r); return keep(o)`), or the param
                        // wrapped in place (`return keep(Some(r))`).
                        || matches!(&a.value.kind, ExprKind::Identifier(n) if wr.iter().any(|w| w == n))
                        || crate::ast::option_result_ctor_payload(&a.value).is_some_and(
                            |p| matches!(&p.kind, ExprKind::Identifier(n) if wr.iter().any(|w| w == n)),
                        ))
                        && fn_returns_param(gf, j)
                })
            }
            _ => false,
        }
    }

    fn walk_expr(
        program: &crate::Program,
        e: &Expr,
        name: &str,
        wr: &[String],
        self_name: &str,
    ) -> bool {
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                yields_via_call(program, inner, name, wr, self_name)
                    || walk_expr(program, inner, name, wr, self_name)
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(program, b, name, wr, self_name),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                walk_block(program, then_block, name, wr, self_name)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk_expr(program, x, name, wr, self_name))
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                walk_block(program, then_block, name, wr, self_name)
                    || else_branch
                        .as_deref()
                        .is_some_and(|x| walk_expr(program, x, name, wr, self_name))
            }
            ExprKind::Match { arms, .. } => arms.iter().any(|a| {
                yields_via_call(program, &a.body, name, wr, self_name)
                    || walk_expr(program, &a.body, name, wr, self_name)
            }),
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(program, body, name, wr, self_name),
            _ => false,
        }
    }

    fn walk_block(
        program: &crate::Program,
        b: &Block,
        name: &str,
        wr: &[String],
        self_name: &str,
    ) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => walk_expr(program, e, name, wr, self_name),
            _ => false,
        }) || b.final_expr.as_deref().is_some_and(|fe| {
            yields_via_call(program, fe, name, wr, self_name)
                || walk_expr(program, fe, name, wr, self_name)
        })
    }

    // B-2026-09-26-27 — the locals that wrap the param, at any depth, asked
    // only where the param leaves on EVERY exit: this predicate is a MAY
    // answer, and a wrapper returned on some paths only would stand the
    // caller down where the callee runs nothing (see `param_wrap_aliases_ex`).
    // The param itself joins, for the in-place wrap `keep(Some(r))`.
    let wr: Vec<String> = if fn_always_returns_param_via_call(program, f, arg_index) {
        std::iter::once(param_name.to_string())
            .chain(
                param_wrap_aliases_ex(Some(program), f, param_name, true)
                    .into_iter()
                    .map(|(n, _)| n),
            )
            .collect()
    } else {
        Vec::new()
    };
    walk_block(program, &f.body, param_name, &wr, &f.name)
}

/// A PATH from an owned aggregate parameter to one of its parts — empty for
/// the whole param, one element per level in. See
/// [`fn_returns_param_part_paths`].
pub type ParamPath = Vec<ParamPart>;

/// One top-level PART of an owned aggregate parameter — a tuple element or a
/// struct field. See [`fn_returns_param_part_paths`].
/// `Ord` is derived (B-2026-09-19-34) so a set of paths can be sorted into a
/// canonical order before it is handed to a consumer. It is a total order over
/// plain data with no semantic content — `TupleIndex` sorts before `Field`
/// because it is declared first — and nothing reads it as a ranking.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
    returned_param_part_paths_impl(f, arg_index, None)
}

/// B-2026-09-05-36 — [`fn_returns_param_part_paths`] plus the routes only a
/// PROGRAM can classify: a part handed BARE to a free function that takes
/// that parameter over (returns it, or stores it under a root outliving its
/// call — [`callee_takes_param_over`]), or pushed directly under one of `f`'s
/// own outliving roots (`v.push(r)` with `v: mut ref Vec[R]`). Statement
/// position included, which the return-site walk never sees: `let (r, k) =
/// t; stash(r, v); k` hands `t.0` to `v` and only `k` back.
///
/// Reported on the same channel as a returned part because the consumer's
/// question is the same — which parts must the caller-side walk SKIP because
/// some other owner runs their body — and both channels' consumers union it
/// with the tuple-arm predicate the same way. `wrap(r)` on `let (r, k) = t`
/// ran `dR5 r5 dR5` on every surface before; a call to an unknown callee still
/// reports nothing, the under-approximating direction this channel keeps.
pub fn fn_escaping_param_part_paths(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> Vec<ParamPath> {
    returned_param_part_paths_impl(f, arg_index, Some(program))
}

/// Shared by the two part-path scanners (`returned_param_part_paths_impl`,
/// `escaping_field_payload_paths_impl`) since B-2026-09-06-29, so the
/// destructure-alias rule cannot be spelled twice.
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

/// A destructure of `base` (the param or one of its parts): each leaf
/// binding denotes the source's path extended by its own element / field.
/// Shared by the `let` / `let … else` statements and by every pattern
/// construct (`match`, `if let`, `while let`) — B-2026-09-02-24 taught the
/// `match` arm this, and B-2026-09-05-34 found `if let (r, k) = t { r }`
/// still opaque: `r` was an unknown name, the handed-back element never
/// registered as escaping, and the caller ran its `Drop` body a SECOND time
/// on top of the result's owner (`dR3 r3 dR3` against the interpreter's
/// `r3 dR3`). One walk for every destructuring position, so the `if let`
/// spelling cannot fall behind the `match` spelling again.
fn alias_destructure(pattern: &Pattern, base: &ParamPath, aliases: &mut Vec<(String, ParamPath)>) {
    match &pattern.kind {
        PatternKind::Tuple(pats) => {
            for (i, p) in pats.iter().enumerate() {
                if let PatternKind::Binding(n) = &p.kind {
                    let mut path = base.clone();
                    path.push(ParamPart::TupleIndex(i));
                    set_alias(aliases, n, path);
                }
            }
        }
        PatternKind::Struct { fields, .. } => {
            for fp in fields {
                let mut path = base.clone();
                path.push(ParamPart::Field(fp.name.clone()));
                match &fp.pattern {
                    // `W { r, n }` — shorthand binds the field name itself.
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
}

fn returned_param_part_paths_impl(
    f: &Function,
    arg_index: usize,
    program: Option<&crate::Program>,
) -> Vec<ParamPath> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    // The roots whose storage outlives the call, as
    // `fn_moves_param_into_outliving_place` computes them; only consulted on
    // the program-aware path.
    let mut roots: Vec<&str> = Vec::new();
    if program.is_some() {
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
    }
    let cx = PartScanCx {
        program,
        roots: &roots,
        top_level: true,
    };
    // B-2026-09-05-17 — cycle guard for the forwarding route, which asks
    // this same question of the callee: a recursive forward answers empty,
    // the channel's under-approximating direction.
    thread_local! {
        static PART_PATHS_IN_FLIGHT: std::cell::RefCell<Vec<(String, usize)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let key = (f.name.clone(), arg_index);
    if program.is_some() && PART_PATHS_IN_FLIGHT.with(|v| v.borrow().contains(&key)) {
        return Vec::new();
    }
    if program.is_some() {
        PART_PATHS_IN_FLIGHT.with(|v| v.borrow_mut().push(key.clone()));
    }
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
                        (_, Some(base)) => alias_destructure(pattern, base, aliases),
                        _ => {}
                    }
                    grow_expr(value, aliases);
                }
                // B-2026-09-05-34 — `let PAT = SCRUT else { … }` destructures
                // exactly as a `let` does; the else block diverges, so its
                // aliases cannot reach a return of this frame, but it is
                // walked for the same reason every other block is.
                StmtKind::LetElse {
                    pattern,
                    value,
                    else_block,
                    ..
                } => {
                    if let Some(base) = denote(value, aliases) {
                        alias_destructure(pattern, &base, aliases);
                    }
                    grow_expr(value, aliases);
                    grow_block(else_block, aliases);
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
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                // B-2026-09-05-34 — the `if let` twin of the `match` arm
                // below: the pattern destructures the scrutinee, so its leaf
                // bindings denote the scrutinee's path extended by their
                // position. See `alias_destructure`.
                if let Some(base) = denote(value, aliases) {
                    alias_destructure(pattern, &base, aliases);
                }
                grow_expr(value, aliases);
                grow_block(then_block, aliases);
                if let Some(x) = else_branch.as_deref() {
                    grow_expr(x, aliases);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // B-2026-09-02-24 — a match arm destructures the scrutinee
                // exactly as a `let` does, so its leaf bindings denote the
                // scrutinee's path extended by their own position. Recording
                // them here (mirroring the `let`-destructure arms in
                // `grow_block`) is what lets `yielded` recognize
                // `match t { (r, k) => r }` as handing back `t.0`; without it the
                // arm binding was an unknown name, the returned element was never
                // seen to escape, and the caller ran that element's `Drop` body a
                // SECOND time on top of the result's owner. The enum-variant
                // PAYLOAD spelling is `fn_returns_param_payload`'s separate answer
                // (`ParamPart` cannot name an enum payload).
                let base = denote(scrutinee, aliases);
                for a in arms {
                    if let Some(base) = &base {
                        alias_destructure(&a.pattern, base, aliases);
                    }
                    grow_expr(&a.body, aliases);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                // B-2026-09-05-34 — the loop form, same destructure.
                if let Some(base) = denote(value, aliases) {
                    alias_destructure(pattern, &base, aliases);
                }
                grow_expr(value, aliases);
                grow_block(body, aliases);
            }
            ExprKind::While { body, .. }
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
            // B-2026-09-03-4 — a CONSTRUCTOR wrap (`Option.Some(r)`,
            // `Result.Ok(r)`, `E.A(r)`): the callee is a path, not a plain
            // identifier, and the operand crosses the frame boundary inside the
            // constructed value exactly as it does inside a struct literal.
            // `let (r, k) = t; Option.Some(r)` ran `r`'s body twice on every
            // surface — the caller's element walk beside the result's owner —
            // while the `match` spelling was right through the tuple-arm
            // predicate, whose `payload_yields` already counts a constructor
            // call. A call to a plain IDENTIFIER is a free function's business
            // and stays with the program-aware `taken_over`.
            ExprKind::Call { callee, args, .. }
                if !matches!(&callee.kind, ExprKind::Identifier(_)) =>
            {
                for a in args {
                    yielded(&a.value, aliases, out);
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

    /// B-2026-09-05-36 — a part handed to a call that takes it over, or
    /// pushed under an outliving root. Program-aware path only.
    fn taken_over(
        e: &Expr,
        aliases: &[(String, ParamPath)],
        cx: PartScanCx<'_>,
        out: &mut Vec<ParamPath>,
    ) {
        fn push_part(out: &mut Vec<ParamPath>, path: ParamPath) {
            if !path.is_empty() && !out.contains(&path) {
                out.push(path);
            }
        }
        let note = |a: &Expr, out: &mut Vec<ParamPath>| {
            if let Some(path) = denote(a, aliases) {
                push_part(out, path);
            }
        };
        match &e.kind {
            ExprKind::Call { callee, args, .. } => {
                let Some(program) = cx.program else { return };
                let ExprKind::Identifier(g) = &callee.kind else {
                    return;
                };
                let Some(gf) = program.items.iter().find_map(|item| match item {
                    Item::Function(gf) if &gf.name == g => Some(gf),
                    _ => None,
                }) else {
                    return;
                };
                for (j, a) in args.iter().enumerate() {
                    if !matches!(&a.value.kind, ExprKind::Identifier(_)) {
                        continue;
                    }
                    if callee_takes_param_over(program, gf, j) {
                        note(&a.value, out);
                        continue;
                    }
                    // B-2026-09-05-17 — the FORWARDING route: the callee hands
                    // back not the value but a PART of it (`fn fwd(g: Cd) -> R
                    // { return cEsc(g); }` over `fn cEsc(h: Cd) -> R { let Cd
                    // { r, z } = h; r }`), so `fwd` hands back the same part
                    // under this argument's prefix. Composed from the callee's
                    // own answer, which is what the whole-param channel's
                    // `fn_returns_param_via_call` does one level up, and gated
                    // to the body's top level (see `PartScanCx::top_level`).
                    if cx.top_level {
                        if let Some(prefix) = denote(&a.value, aliases) {
                            for q in fn_escaping_param_part_paths(program, gf, j) {
                                let mut path = prefix.clone();
                                path.extend(q);
                                push_part(out, path);
                            }
                        }
                    }
                }
            }
            ExprKind::MethodCall { object, args, .. }
                if cx.program.is_some()
                    && outliving_store::place_root_outlives(object, cx.roots) =>
            {
                for a in args {
                    if matches!(&a.value.kind, ExprKind::Identifier(_)) {
                        note(&a.value, out);
                    }
                }
            }
            _ => {}
        }
    }

    fn scan_expr(
        e: &Expr,
        aliases: &[(String, ParamPath)],
        cx: PartScanCx<'_>,
        out: &mut Vec<ParamPath>,
    ) {
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                yielded(inner, aliases, out);
                scan_expr(inner, aliases, cx, out);
            }
            ExprKind::Call { args, .. } => {
                taken_over(e, aliases, cx, out);
                for a in args {
                    scan_expr(&a.value, aliases, cx, out);
                }
            }
            // B-2026-09-05-17 — a call nested in a returned aggregate literal
            // (`return W { r: cEsc(g), n: 1 }`) forwards exactly as a bare one.
            ExprKind::StructLiteral { fields, .. } => {
                for fi in fields {
                    scan_expr(&fi.value, aliases, cx, out);
                }
            }
            ExprKind::Tuple(elems) => {
                for el in elems {
                    scan_expr(el, aliases, cx, out);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                taken_over(e, aliases, cx, out);
                scan_expr(object, aliases, cx, out);
                for a in args {
                    scan_expr(&a.value, aliases, cx, out);
                }
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => scan_block(b, aliases, cx, out),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                let cx = PartScanCx {
                    top_level: false,
                    ..cx
                };
                scan_block(then_block, aliases, cx, out);
                if let Some(x) = else_branch.as_deref() {
                    scan_expr(x, aliases, cx, out);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                let cx = PartScanCx {
                    top_level: false,
                    ..cx
                };
                scan_block(then_block, aliases, cx, out);
                if let Some(x) = else_branch.as_deref() {
                    scan_expr(x, aliases, cx, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                let cx = PartScanCx {
                    top_level: false,
                    ..cx
                };
                for a in arms {
                    yielded(&a.body, aliases, out);
                    scan_expr(&a.body, aliases, cx, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => scan_block(
                body,
                aliases,
                PartScanCx {
                    top_level: false,
                    ..cx
                },
                out,
            ),
            _ => {}
        }
    }

    fn scan_block(
        b: &Block,
        aliases: &[(String, ParamPath)],
        cx: PartScanCx<'_>,
        out: &mut Vec<ParamPath>,
    ) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => scan_expr(e, aliases, cx, out),
                StmtKind::Let { value, .. } => scan_expr(value, aliases, cx, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            yielded(fe, aliases, out);
            scan_expr(fe, aliases, cx, out);
        }
    }

    let mut aliases: Vec<(String, ParamPath)> = vec![(param_name.clone(), Vec::new())];
    grow_block(&f.body, &mut aliases);
    let mut out = Vec::new();
    scan_block(&f.body, &aliases, cx, &mut out);
    if program.is_some() {
        PART_PATHS_IN_FLIGHT.with(|v| {
            let mut v = v.borrow_mut();
            if let Some(pos) = v.iter().rposition(|k| k == &key) {
                v.remove(pos);
            }
        });
    }
    out
}

/// What the program-aware part scan carries (B-2026-09-05-36).
#[derive(Clone, Copy)]
struct PartScanCx<'a> {
    program: Option<&'a crate::Program>,
    roots: &'a [&'a str],
    /// B-2026-09-05-17 — still on the function body's own statement list,
    /// i.e. not inside an `if` / `match` / loop. The forwarding route below
    /// is admitted only here, so a CONDITIONAL forward keeps the channel's
    /// under-approximating answer (`fn f(g: Cd, c: bool) -> R { if c { return
    /// cEsc(g); } mk(99) }` reports nothing and the not-forwarded path keeps
    /// its body) rather than trading this row's double for a lost body.
    top_level: bool,
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
    !escaping_param_payload_variants_impl(f, arg_index, CallYieldRule::Any).is_empty()
}

/// B-2026-09-05-35 — the PROGRAM-AWARE, per-VARIANT form of
/// [`fn_returns_param_payload`]: which variants of by-value enum parameter
/// `arg_index` have a payload that `f` hands out of its frame?
///
/// ```text
/// fn e_call(b: E) -> i64   { match b { E.A(r) => consume(r), E.B(k) => k } }   // [] — `k: i64` carries no body (B-2026-09-06-26; was ["B"])
/// fn e_ret(b: E) -> R      { match b { E.A(r) => r,          E.B(k) => mk(k) } } // ["A"]
/// fn e_fwd(b: E) -> R      { match b { E.A(r) => wrap(r),    E.B(k) => mk(k) } } // ["A"]
/// fn any(b: E) -> E        { match b { x => x } }                                 // ["*"]
/// ```
///
/// Two over-approximations of the whole-param predicate are what this
/// replaces for its caller-side consumers. Its `yields` counts a binding
/// passed to ANY call as leaving (`consume(r)`, whose callee returns `x.id`),
/// and a MIXED-arm callee answers for the whole parameter, so `E.B(k) => k`
/// handing back an `i64` stood the caller's payload walk down for the `E.A`
/// arm too. Both backends consult the same predicate for an enum argument, so
/// both lost `r`'s body on both cells — agreed-and-wrong, invisible to A/B.
/// Here a call is counted only when its callee takes the parameter over
/// ([`callee_takes_param_over`]; an unknown callee still counts), and each
/// arm reports under ITS variant: a `TupleVariant` / `Struct` pattern by its
/// path's last segment, an `Or` by each alternative, and a whole-value
/// `Binding` / `AtBinding` under `"*"`, which every consumer reads as "all".
/// The interpreter asks with the argument's runtime variant; codegen masks
/// the escaping variants out of the payload-bodies walker and lets the tag
/// switch decide at run time. The whole-param form keeps the `Any` rule for
/// the callee-side consumers it was measured on.
/// B-2026-09-06-17 — the PROJECTION-ROOTED sibling of
/// [`fn_escaping_param_payload_variants`]: which enum-typed PARTS of by-value
/// parameter `arg_index` (`h.e`, `h.s.e`, `t.0`) have a payload that `f` hands
/// out of its frame?
///
/// ```text
/// fn out(h: H1) -> R   { match h.e { E.A(r) => return r, E.B => mk(0) } }   // [["e"]]
/// fn out2(h: H2) -> R  { match h.s.e { E.A(r) => return r, .. } }           // [["s","e"]]
/// fn read(h: H1) -> i64 { match h.e { E.A(r) => r.id, .. } }               // []
/// ```
///
/// The whole-param scanner keys its scrutinee on the bare parameter name, and
/// the part-path scanner denotes returned PLACES, so a payload bound out of a
/// projected enum by a `match` / `if let` / `while let` arm and handed back
/// reached neither: the caller's retained walk over the argument ran that
/// payload's body a second time, for a named local and a fresh temp alike, on
/// every surface. Conservative-any-variant, like [`fn_returns_param_payload`]:
/// a path is reported when ANY arm hands its payload out, and the consumer
/// masks the whole field's payload bodies — a missed body on a run that takes
/// another arm, never a double.
pub fn fn_escaping_param_field_payload_paths(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> Vec<ParamPath> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return Vec::new();
    };
    // B-2026-09-06-25 — a BORROWED param hands nothing out of the caller's
    // value: a payload the body forwards from `h.e` under `h: ref H` /
    // `mut ref H` is the stopgap COPY (design.md § "A projection off a borrow
    // is an implicit copy"), and the caller's own field walk still owns the
    // original's body. Without this gate the interpreter's named-arg consumer
    // disarmed the CALLER's payload walk for a `ref` argument, so
    // `let r2 = match h.e { E.A(r) => r, .. }` through `ref h` printed
    // `dE 5 dR5` — the caller's struct dying with an empty shell — against
    // `dE dR5 5 dR5` on every compiled backend. The owned-`self` sibling below
    // already carries the same mode gate.
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return Vec::new();
    }
    escaping_field_payload_paths_impl(f, param_name, false, CallYieldRule::ReturnsIt(program))
}

/// [`fn_escaping_param_field_payload_paths`] for an OWNED `self` receiver
/// (`fn out(self) -> R { match self.e { E.A(r) => return r, .. } }`), which
/// has no index in `f.params` at the AST level. Empty for a borrowed receiver:
/// a projection off a borrow is a copy, and the copy's binding is its own owner.
pub fn fn_escaping_self_field_payload_paths(
    program: &crate::Program,
    f: &Function,
) -> Vec<ParamPath> {
    if !matches!(f.self_param, Some(SelfParam::Owned)) {
        return Vec::new();
    }
    escaping_field_payload_paths_impl(f, "self", true, CallYieldRule::ReturnsIt(program))
}

fn escaping_field_payload_paths_impl(
    f: &Function,
    root: &str,
    root_is_self: bool,
    rule: CallYieldRule<'_>,
) -> Vec<ParamPath> {
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
    let program = match rule {
        CallYieldRule::ReturnsIt(p) => Some(p),
        CallYieldRule::Any => None,
    };
    let stored = |body: &Expr, names: &[String]| {
        names.iter().any(|n| {
            outliving_store::stores(body, n, &roots)
                || program.is_some_and(|p| stored_via_call(body, n, p))
        })
    };
    let stored_block = |body: &Block, names: &[String]| {
        names.iter().any(|n| {
            outliving_store::walk_block(body, n, &roots)
                || program.is_some_and(|p| stored_via_call_block(body, n, p))
        })
    };
    /// The path a scrutinee denotes off `root`: `FieldAccess` / `TupleIndex`
    /// hops down to the root identifier (or `self`), OR to a name a
    /// destructure has made an alias of one of the root's parts. An EMPTY
    /// path is the whole parameter — a legal BASE for a destructure to alias
    /// from, never a reported path (the callers check `is_empty`).
    ///
    /// B-2026-09-06-29 — the alias half is what reaches a TWO-LEVEL
    /// destructure. `match h { H1 { e } => match e { E.A(r) => return r } }`
    /// hands `h.e`'s payload back exactly as `match h.e { .. }` does, but its
    /// inner scrutinee is the bare leaf `e`, which the projection-only walk
    /// declined; the caller's retained walk over the argument then ran the
    /// payload's body a second time under the result's owner, on every
    /// surface. The part-path scanner has carried this alias table since
    /// B-2026-08-28-23; the shared helpers below are its.
    fn denote(
        e: &Expr,
        root: &str,
        root_is_self: bool,
        aliases: &[(String, ParamPath)],
    ) -> Option<ParamPath> {
        let mut path: Vec<ParamPart> = Vec::new();
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, field } => {
                    path.push(ParamPart::Field(field.clone()));
                    cur = object;
                }
                ExprKind::TupleIndex { object, index } => {
                    path.push(ParamPart::TupleIndex(*index as usize));
                    cur = object;
                }
                ExprKind::Identifier(n) => {
                    let base: ParamPath = if !root_is_self && n == root {
                        Vec::new()
                    } else {
                        aliases.iter().find(|(a, _)| a == n)?.1.clone()
                    };
                    path.reverse();
                    let mut full = base;
                    full.extend(path);
                    return Some(full);
                }
                ExprKind::SelfValue if root_is_self => {
                    path.reverse();
                    return Some(path);
                }
                _ => return None,
            }
        }
    }
    fn push(out: &mut Vec<ParamPath>, path: ParamPath) {
        if !out.contains(&path) {
            out.push(path);
        }
    }
    struct Cx<'a> {
        root: &'a str,
        root_is_self: bool,
        fn_body: &'a Block,
        rule: CallYieldRule<'a>,
        stored: &'a dyn Fn(&Expr, &[String]) -> bool,
        stored_block: &'a dyn Fn(&Block, &[String]) -> bool,
    }
    /// Does a `match` arm / `if let` / `while let` over a denoted base hand
    /// one of its bindings out? `block` is the arm's scope when the construct
    /// binds into a block rather than an arm expression.
    fn arm_hands_out(
        cx: &Cx<'_>,
        names: &[String],
        body: Option<&Expr>,
        block: Option<&Block>,
    ) -> bool {
        if names.is_empty() {
            return false;
        }
        match (body, block) {
            (Some(body), _) => {
                names.iter().any(|n| payload_yields(body, n, cx.rule))
                    || payload_returns_any(body, names, cx.rule)
                    || payload_escapes_by_assignment(body, names, cx.fn_body, cx.rule)
                    || (cx.stored)(body, names)
            }
            (None, Some(block)) => {
                payload_returns_any_block(block, names, cx.rule)
                    || payload_escapes_by_assignment_block(block, names, cx.fn_body, cx.rule)
                    || (cx.stored_block)(block, names)
            }
            (None, None) => false,
        }
    }
    fn walk(
        e: &Expr,
        cx: &Cx<'_>,
        aliases: &mut Vec<(String, ParamPath)>,
        out: &mut Vec<ParamPath>,
    ) {
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                let base = denote(scrutinee, cx.root, cx.root_is_self, aliases);
                for a in arms {
                    if let Some(base) = &base {
                        // A destructuring arm makes each leaf an alias of the
                        // base's part, for the arm body that follows.
                        alias_destructure(&a.pattern, base, aliases);
                        if !base.is_empty()
                            && !matches!(a.pattern.kind, PatternKind::Tuple(_))
                            && arm_hands_out(cx, &a.pattern.binding_names(), Some(&a.body), None)
                        {
                            push(out, base.clone());
                        }
                    }
                    walk(&a.body, cx, aliases, out);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                if let Some(base) = denote(value, cx.root, cx.root_is_self, aliases) {
                    alias_destructure(pattern, &base, aliases);
                    if !base.is_empty()
                        && !matches!(pattern.kind, PatternKind::Tuple(_))
                        && arm_hands_out(cx, &pattern.binding_names(), None, Some(then_block))
                    {
                        push(out, base);
                    }
                }
                walk_block_for(then_block, cx, aliases, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, cx, aliases, out);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                if let Some(base) = denote(value, cx.root, cx.root_is_self, aliases) {
                    alias_destructure(pattern, &base, aliases);
                    if !base.is_empty()
                        && !matches!(pattern.kind, PatternKind::Tuple(_))
                        && arm_hands_out(cx, &pattern.binding_names(), None, Some(body))
                    {
                        push(out, base);
                    }
                }
                walk_block_for(body, cx, aliases, out);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block_for(b, cx, aliases, out),
            ExprKind::Return(Some(inner)) => walk(inner, cx, aliases, out),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk(condition, cx, aliases, out);
                walk_block_for(then_block, cx, aliases, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, cx, aliases, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block_for(body, cx, aliases, out),
            _ => {}
        }
    }
    fn walk_block_for(
        b: &Block,
        cx: &Cx<'_>,
        aliases: &mut Vec<(String, ParamPath)>,
        out: &mut Vec<ParamPath>,
    ) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => walk(e, cx, aliases, out),
                StmtKind::Let { pattern, value, .. } => {
                    walk(value, cx, aliases, out);
                    // `let H1 { e } = h;` / `let k = e;` — the leaf (or the
                    // rebind) denotes the source's part from here on; a `let`
                    // of an unrelated value un-records a shadowed alias.
                    let d = denote(value, cx.root, cx.root_is_self, aliases);
                    match (&pattern.kind, d) {
                        (PatternKind::Binding(n), Some(p)) => set_alias(aliases, n, p),
                        (PatternKind::Binding(n), None) => clear_alias(aliases, n),
                        (_, Some(base)) => alias_destructure(pattern, &base, aliases),
                        _ => {}
                    }
                }
                StmtKind::LetElse { pattern, value, .. } => {
                    walk(value, cx, aliases, out);
                    if let Some(base) = denote(value, cx.root, cx.root_is_self, aliases) {
                        alias_destructure(pattern, &base, aliases);
                    }
                }
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            walk(fe, cx, aliases, out);
        }
    }
    let cx = Cx {
        root,
        root_is_self,
        fn_body: &f.body,
        rule,
        stored: &stored,
        stored_block: &stored_block,
    };
    let mut out = Vec::new();
    let mut aliases: Vec<(String, ParamPath)> = Vec::new();
    walk_block_for(&f.body, &cx, &mut aliases, &mut out);
    out
}

pub fn fn_escaping_param_payload_variants(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> Vec<String> {
    escaping_param_payload_variants_impl(f, arg_index, CallYieldRule::ReturnsIt(program))
}

/// [`fn_escaping_param_payload_variants`] asked of one variant (`Some`) or of
/// any (`None`). `"*"` matches every variant.
pub fn fn_returns_param_payload_of(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
    variant: Option<&str>,
) -> bool {
    let vs = fn_escaping_param_payload_variants(program, f, arg_index);
    match variant {
        None => !vs.is_empty(),
        Some(v) => vs.iter().any(|x| x == "*" || x == v),
    }
}

/// The one name an arm pattern binds when it binds an `Option`/`Result`
/// payload WHOLE (`Some(t)` / `Ok(t)`), and only for the variant asked about.
/// A destructuring pattern (`Some((a, b))`) binds the PARTS and is the
/// tuple-arm channel's business, not this one's.
///
/// Lifted out of [`fn_escaping_param_payload_part_paths`] by B-2026-09-14-7 so
/// its CONSUMED-INSIDE complement asks the same question of the same arms: the
/// two channels mask the same caller walk from opposite directions, so a
/// divergence between their notions of "the arm binds the payload whole" would
/// show up as a body run twice or not at all.
fn optres_whole_payload_binding<'p>(p: &'p Pattern, variant: Option<&str>) -> Option<&'p str> {
    let PatternKind::TupleVariant { path, patterns } = &p.kind else {
        return None;
    };
    let last = path.last()?;
    if variant.is_some_and(|v| last != v) {
        return None;
    }
    match patterns.as_slice() {
        [sub] => match &sub.kind {
            PatternKind::Binding(n) => Some(n.as_str()),
            _ => None,
        },
        _ => None,
    }
}

/// The path `e` denotes off `root`, where an EMPTY path is the whole binding.
/// `None` for anything that is not a projection chain bottoming out at `root`.
/// Lifted alongside [`optres_whole_payload_binding`], for its reason.
fn optres_part_denote(e: &Expr, root: &str) -> Option<ParamPath> {
    let mut path: Vec<ParamPart> = Vec::new();
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::FieldAccess { object, field } => {
                path.push(ParamPart::Field(field.clone()));
                cur = object;
            }
            ExprKind::TupleIndex { object, index } => {
                path.push(ParamPart::TupleIndex(*index as usize));
                cur = object;
            }
            ExprKind::Identifier(n) if n == root => {
                path.reverse();
                return Some(path);
            }
            _ => return None,
        }
    }
}

/// B-2026-09-13-5 — the PART-PRECISE sibling of
/// [`fn_escaping_param_payload_variants`]: which paths INSIDE the payload of
/// by-value `Option` / `Result` parameter `arg_index` does `f` hand out of its
/// frame, in an arm that binds the payload WHOLE and returns only a
/// PROJECTION of it?
///
/// ```text
/// fn eat(o: Option[(R, i64)]) -> R { match o { Some(t) => return t.0, .. } }   // [[#0]]
/// fn eat(o: Option[Holder]) -> R   { match o { Some(t) => return t.r, .. } }   // [["r"]]
/// fn eat(o: Option[(R, i64)]) -> R { match o { Some(t) => return t, .. } }     // []
/// ```
///
/// The whole-payload predicate answers the third spelling and stands the
/// argument's payload walk down entirely. It counts a payload as escaping only
/// when the ARM BINDING ITSELF leaves the frame, so the first two slipped past
/// it and the caller's fresh-temp walk ran the handed-back part's body a second
/// time under the result's owner — `dR5 got:5 dR5 end` against a due
/// `got:5 dR5 end`, on the interpreter only, the compiled backends being
/// correct on that cell. The nested-destructure spelling
/// (`Some((a, b)) => return a`) binds the part directly and was already right,
/// which is the tell that the projection is the missing hop and not the move.
///
/// A WHOLE-binding escape reports nothing here, deliberately: that arm belongs
/// to the whole-payload predicate, and answering on both channels would mask
/// one walk twice.
///
/// DELIBERATELY UNDER-APPROXIMATE, the direction every predicate on this
/// channel keeps: a direct projection chain off the arm binding at an explicit
/// `return`, and nothing else. An alias, a store, a forwarding call, a TAIL
/// position or any shape this cannot classify yields no path and leaves that
/// spelling exactly as it behaves today. A MISSED escape keeps the pre-existing
/// double body; a FALSE one would suppress the only body that runs.
///
/// The LEAF's own ownership is NOT decided here and cannot be — this is the
/// AST, with no types. `t.0.id` reports `[#0, "id"]` and the consumer declines
/// it, because a scalar leaf owns nothing and masking it would hand the
/// parent's own body a hole to read. That is the `value_leaf_can_own` gate the
/// sibling part channels already apply to their own paths.
pub fn fn_escaping_param_payload_part_paths(
    f: &Function,
    arg_index: usize,
    variant: Option<&str>,
) -> Vec<ParamPath> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    // A projection off a BORROW is an implicit copy, so nothing of the
    // caller's value leaves the frame and the caller's own walk stays the only
    // owner — `callee_param_is_borrow`'s reason, asked here so the predicate
    // cannot be wired to a borrowed slot by a future consumer.
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return Vec::new();
    }
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return Vec::new();
    };

    /// Every yield site inside `e`, asked what it denotes off `root`.
    ///
    /// `top` is "still on the arm body's own statement list", i.e. not inside a
    /// nested `if` / `match` / loop, and a path is RECORDED only there. That is
    /// [`PartScanCx`]'s rule and it is load-bearing rather than tidy: this
    /// channel is consulted with the argument's RUNTIME variant but has no idea
    /// which branch a run takes, so a conditional escape
    /// (`Some(t) => { if k { return t.r; } return mk(1); }` called with
    /// `k = false`) would mask the body of a part that really did die in the
    /// call. Measured: recording it loses `dR5` outright on the `false` run,
    /// where the interpreter is correct today. The whole-part sibling accepts
    /// that trade by long-standing convention; a NEW channel does not have to
    /// inherit it, and the shape keeps its pre-existing (doubled) answer
    /// instead of gaining a lost body.
    ///
    /// `tail` is "this expression's value is what the function returns", which
    /// is how the arm-tail spelling (`Some(t) => { t.0 }`, no `return`) is
    /// reached: it is a yield site exactly when the `match` itself sits in the
    /// function's tail position.
    ///
    /// `whole` is set by a `return <root>` / a tail `<root>` at ANY depth, which
    /// takes the arm out of this channel entirely — conservative in the
    /// declining direction, since the whole-payload predicate answers that arm.
    fn returns_in(
        e: &Expr,
        root: &str,
        out: &mut Vec<ParamPath>,
        whole: &mut bool,
        top: bool,
        tail: bool,
    ) {
        let mut record = |e: &Expr, top: bool| match optres_part_denote(e, root) {
            Some(p) if p.is_empty() => *whole = true,
            Some(p) if top && !out.contains(&p) => {
                out.push(p);
            }
            _ => {}
        };
        if tail {
            record(e, top);
        }
        match &e.kind {
            ExprKind::Return(Some(inner)) => {
                record(inner, top);
                returns_in(inner, root, out, whole, false, false);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => returns_in_block(b, root, out, whole, top, tail),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                returns_in_block(then_block, root, out, whole, false, tail);
                if let Some(x) = else_branch.as_deref() {
                    returns_in(x, root, out, whole, false, tail);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    returns_in(&a.body, root, out, whole, false, tail);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                returns_in_block(then_block, root, out, whole, false, tail);
                if let Some(x) = else_branch.as_deref() {
                    returns_in(x, root, out, whole, false, tail);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => {
                returns_in_block(body, root, out, whole, false, false)
            }
            _ => {}
        }
    }
    fn returns_in_block(
        b: &Block,
        root: &str,
        out: &mut Vec<ParamPath>,
        whole: &mut bool,
        top: bool,
        tail: bool,
    ) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => returns_in(e, root, out, whole, top, false),
                StmtKind::Let { value, .. } => returns_in(value, root, out, whole, false, false),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            returns_in(fe, root, out, whole, top, tail);
        }
    }

    /// Find every `match` / `if let` / `while let` whose SCRUTINEE is the bare
    /// parameter, and collect its whole-payload arms' yielded projections.
    /// `tail` is threaded so that a `match` in the function's tail position
    /// treats each arm's own tail as a yield site.
    fn scan(
        e: &Expr,
        param: &str,
        variant: Option<&str>,
        out: &mut Vec<ParamPath>,
        whole: &mut bool,
        tail: bool,
    ) {
        let is_param = |s: &Expr| matches!(&s.kind, ExprKind::Identifier(n) if n == param);
        match &e.kind {
            ExprKind::Match { scrutinee, arms } if is_param(scrutinee) => {
                for a in arms {
                    if let Some(bind) = optres_whole_payload_binding(&a.pattern, variant) {
                        returns_in(&a.body, bind, out, whole, true, tail);
                    }
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                ..
            } if is_param(value) => {
                if let Some(bind) = optres_whole_payload_binding(pattern, variant) {
                    returns_in_block(then_block, bind, out, whole, true, tail);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } if is_param(value) => {
                if let Some(bind) = optres_whole_payload_binding(pattern, variant) {
                    // A loop body's tail is not the function's value.
                    returns_in_block(body, bind, out, whole, true, false);
                }
            }
            _ => {}
        }
        // Nested positions: the construct above may sit anywhere in the body.
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => scan_block(b, param, variant, out, whole, tail),
            ExprKind::Return(Some(inner)) => scan(inner, param, variant, out, whole, true),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                scan_block(then_block, param, variant, out, whole, tail);
                if let Some(x) = else_branch.as_deref() {
                    scan(x, param, variant, out, whole, tail);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                scan_block(then_block, param, variant, out, whole, tail);
                if let Some(x) = else_branch.as_deref() {
                    scan(x, param, variant, out, whole, tail);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    scan(&a.body, param, variant, out, whole, tail);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => {
                scan_block(body, param, variant, out, whole, false)
            }
            _ => {}
        }
    }
    fn scan_block(
        b: &Block,
        param: &str,
        variant: Option<&str>,
        out: &mut Vec<ParamPath>,
        whole: &mut bool,
        tail: bool,
    ) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => scan(e, param, variant, out, whole, false),
                StmtKind::Let { value, .. } => scan(value, param, variant, out, whole, false),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            scan(fe, param, variant, out, whole, tail);
        }
    }

    let mut out: Vec<ParamPath> = Vec::new();
    let mut whole = false;
    scan_block(&f.body, param_name, variant, &mut out, &mut whole, true);
    if whole {
        return Vec::new();
    }
    out
}

/// B-2026-09-14-7 — the CONSUMED-INSIDE complement of
/// [`fn_escaping_param_payload_part_paths`]: which paths INSIDE the payload of
/// by-value `Option` / `Result` parameter `arg_index` does `f` move into a
/// LOCAL that lives and dies inside the frame, in an arm that binds the
/// payload WHOLE?
///
/// ```text
/// fn eat(o: Option[(R, i64)]) { match o { Some(t) => { let x = t.0; println("mid"); } .. } }
/// // -> [("x", [#0])]
/// ```
///
/// The escaping sibling answers "a new owner outside the frame holds this
/// part". This one answers "an owner INSIDE the frame holds it", and the
/// caller has to stand down for both: the part is gone from the argument
/// either way, and the only difference is where its body runs. Without it the
/// interpreter gave the local no slot at all (`let_reads_param_view_field`
/// classified it a view of the caller's value) and the caller's fresh-temp
/// walk ran the body AFTER the call returned — `mid dR5 end` against the three
/// compiled surfaces' `dR5 mid end`, a pure ordering divergence with the count
/// right everywhere. design.md § 866 settles which side is correct: "a value
/// whose last use is mid-scope is dropped at that use", and `x` is never read
/// after its `let`.
///
/// Returns the local's NAME beside the path, because the two consumers need
/// different halves of one answer and MUST NOT compute it separately. The
/// callee-side gate (`let_destructures_owned_param`) asks by NAME whether this
/// `let` takes a slot of its own; the caller-side mask
/// (`mask_optres_payload_consumed_parts`) removes the PATH from the value its
/// walk sees. One predicate, consulted from both ends of the same call, is
/// what keeps them from drifting into a lost body (both stand down) or a
/// doubled one (neither does).
///
/// DELIBERATELY UNDER-APPROXIMATE, the direction every predicate on this
/// channel keeps. The local must be:
///
///   * bound by a plain `let <x> = <projection off the arm binding>;` on the
///     arm body's OWN statement list — not inside a nested `if` / loop, whose
///     branch this predicate cannot see (the escaping sibling's `top` rule,
///     and for its reason: this is consulted with the argument's runtime
///     variant but has no idea which branch a run takes); and
///   * used NOWHERE in the function except as the direct base of a field or
///     tuple-index read. A bare mention in any other position — `return x`,
///     `takes(x)`, `let y = x`, `x.method()`, an assignment target — could
///     transfer the value on, which is a DIFFERENT owner's business, so the
///     shape keeps its pre-existing (late) answer instead of gaining a lost
///     body.
///
/// A MISSED consumption keeps today's ordering defect; a FALSE one would
/// suppress the only body that runs. That asymmetry is why the escape check
/// is a whole-function walk rather than an arm-local one.
pub fn fn_consumed_param_payload_part_paths(
    f: &Function,
    arg_index: usize,
    variant: Option<&str>,
) -> Vec<(String, ParamPath)> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    // A projection off a BORROW is an implicit copy, so the caller's walk
    // stays the only owner and nothing here may stand it down — the escaping
    // sibling's guard, asked for its reason.
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return Vec::new();
    }
    // `Option` / `Result` HEAD only. The escaping sibling needs no such gate
    // because both of its ends are already keyed on that head; this channel's
    // callee end is a bare name lookup, so a user enum with a single-binding
    // tuple variant (`E.A(t)`) would stand a slot up here against a caller
    // that never masks — the doubled body this predicate exists to avoid.
    let head_is_optres = match &param.ty.kind {
        crate::ast::TypeKind::Path(p) => p
            .segments
            .last()
            .is_some_and(|seg| seg == "Option" || seg == "Result"),
        _ => false,
    };
    if !head_is_optres {
        return Vec::new();
    }
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return Vec::new();
    };

    /// The `let <x> = <projection off `bind`>;` statements on `b`'s OWN list.
    fn consumed_lets_in(b: &Block, bind: &str, out: &mut Vec<(String, ParamPath)>) {
        for st in &b.stmts {
            let StmtKind::Let { pattern, value, .. } = &st.kind else {
                continue;
            };
            let PatternKind::Binding(x) = &pattern.kind else {
                continue;
            };
            let Some(path) = optres_part_denote(value, bind) else {
                continue;
            };
            // An EMPTY path is a whole-payload rebind, which the whole-payload
            // predicate answers; answering on both channels would mask one
            // walk twice.
            if path.is_empty() {
                continue;
            }
            if !out.iter().any(|(n, p)| n == x && *p == path) {
                out.push((x.clone(), path));
            }
        }
    }

    /// Every `match` / `if let` / `while let` whose SCRUTINEE is the bare
    /// parameter, asked for its whole-payload arms' consumed `let`s. Mirrors
    /// the escaping sibling's `scan` shape so the two channels see the same
    /// set of arms.
    fn scan(e: &Expr, param: &str, variant: Option<&str>, out: &mut Vec<(String, ParamPath)>) {
        let is_param = |s: &Expr| matches!(&s.kind, ExprKind::Identifier(n) if n == param);
        match &e.kind {
            ExprKind::Match { scrutinee, arms } if is_param(scrutinee) => {
                for a in arms {
                    if let Some(bind) = optres_whole_payload_binding(&a.pattern, variant) {
                        if let ExprKind::Block(b) = &a.body.kind {
                            consumed_lets_in(b, bind, out);
                        }
                    }
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                ..
            } if is_param(value) => {
                if let Some(bind) = optres_whole_payload_binding(pattern, variant) {
                    consumed_lets_in(then_block, bind, out);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } if is_param(value) => {
                if let Some(bind) = optres_whole_payload_binding(pattern, variant) {
                    consumed_lets_in(body, bind, out);
                }
            }
            _ => {}
        }
        // Nested positions: the construct above may sit anywhere in the body.
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => scan_block(b, param, variant, out),
            ExprKind::Return(Some(inner)) => scan(inner, param, variant, out),
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                scan_block(then_block, param, variant, out);
                if let Some(x) = else_branch.as_deref() {
                    scan(x, param, variant, out);
                }
            }
            ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                scan_block(then_block, param, variant, out);
                if let Some(x) = else_branch.as_deref() {
                    scan(x, param, variant, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    scan(&a.body, param, variant, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => scan_block(body, param, variant, out),
            _ => {}
        }
    }
    fn scan_block(
        b: &Block,
        param: &str,
        variant: Option<&str>,
        out: &mut Vec<(String, ParamPath)>,
    ) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => scan(e, param, variant, out),
                StmtKind::Let { value, .. } => scan(value, param, variant, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            scan(fe, param, variant, out);
        }
    }

    let mut out: Vec<(String, ParamPath)> = Vec::new();
    scan_block(&f.body, param_name, variant, &mut out);
    // The escape check, over the WHOLE function body rather than the arm: a
    // local that leaves the frame by any route keeps its pre-existing answer.
    out.retain(|(x, _)| {
        !crate::deque_head::block_mentions_name_outside_field_projection(&f.body, x)
    });
    out
}

/// The variant names a pattern over an enum scrutinee commits to: a
/// `TupleVariant` / `Struct` path's last segment, each alternative of an
/// `Or`, `"*"` for a whole-value binding, nothing for a wildcard / literal
/// (which bind no payload).
fn pattern_variant_names(p: &Pattern) -> Vec<String> {
    match &p.kind {
        PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
            path.last().cloned().into_iter().collect()
        }
        PatternKind::Binding(_) => vec!["*".to_string()],
        PatternKind::AtBinding { pattern, .. } => {
            let mut v = pattern_variant_names(pattern);
            if v.is_empty() {
                v.push("*".to_string());
            }
            v
        }
        PatternKind::Or(alts) => alts.iter().flat_map(pattern_variant_names).collect(),
        _ => Vec::new(),
    }
}

fn escaping_param_payload_variants_impl(
    f: &Function,
    arg_index: usize,
    rule: CallYieldRule<'_>,
) -> Vec<String> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return Vec::new();
    };
    fn note(out: &mut Vec<String>, pattern: &Pattern) {
        for v in pattern_variant_names(pattern) {
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    // B-2026-09-05-35 — the STORE routes, as the tuple-arm predicate has
    // them: a payload binding pushed under a borrowed `self` / `ref` param
    // (`v.push(r)`) or handed to a callee that stores it (`stash(r, v)`,
    // program-aware). The whole-param `Any` rule never saw either and was
    // right on such arms only when a SIBLING arm's hand-back happened to
    // stand the whole argument down; asked per variant, the store has to be
    // seen on its own.
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
    let program = match rule {
        CallYieldRule::ReturnsIt(p) => Some(p),
        CallYieldRule::Any => None,
    };
    let stored = |body: &Expr, names: &[String]| {
        names.iter().any(|n| {
            outliving_store::stores(body, n, &roots)
                || program.is_some_and(|p| stored_via_call(body, n, p))
        })
    };
    let stored_block = |body: &Block, names: &[String]| {
        names.iter().any(|n| {
            outliving_store::walk_block(body, n, &roots)
                || program.is_some_and(|p| stored_via_call_block(body, n, p))
        })
    };
    /// Walk for a `match` / `if let` / `while let` whose SCRUTINEE is the param,
    /// and record, per arm, the variants whose bindings leave the frame.
    ///
    /// B-2026-09-05-28 / -30 — a TUPLE pattern over the param is NOT this
    /// predicate's to answer, and is skipped here. Its bindings name ELEMENTS,
    /// so the escape is per element, which [`fn_returns_param_part_paths`]
    /// and [`fn_returns_param_tuple_arm_elems`] report one index at a time;
    /// standing the whole argument down from here instead lost the body of
    /// every element that DID die in the call — `fn pf(t: (R, i64)) -> i64 {
    /// match t { (r, k) => { k } } }` handed back only `k`, and the interpreter
    /// never ran `r`'s body. Codegen already asked this predicate only of a
    /// payload-carrying ENUM parameter (`callee_returns_enum_arg_payload`),
    /// which is why it was correct on the same cells.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        e: &Expr,
        param: &str,
        fn_body: &Block,
        rule: CallYieldRule<'_>,
        stored: &dyn Fn(&Expr, &[String]) -> bool,
        stored_block: &dyn Fn(&Block, &[String]) -> bool,
        out: &mut Vec<String>,
    ) {
        // B-2026-09-24-9 — or a call to a free function that hands the param
        // back on EVERY exit (`match id(a) { .. }`, `fn id(a: Option[R]) ->
        // Option[R] { a }`): its arms destructure the param itself. Unlike
        // `match a`, whose arm bindings are views of a caller-retained value,
        // the call's result is a fresh temp in this frame, and both backends
        // make its arm bindings the payload's OWNERS whether they hand it out
        // or only read it. So every arm that binds a payload reports its
        // variant, and the caller stands its own walk down for it. Program-
        // aware only; the `Any` rule cannot see the callee.
        let scrutinee_is_param = |s: &Expr| match &s.kind {
            ExprKind::Identifier(n) => n == param,
            ExprKind::Call { callee, args } => {
                let CallYieldRule::ReturnsIt(program) = rule else {
                    return false;
                };
                let ExprKind::Identifier(g) = &callee.kind else {
                    return false;
                };
                let Some(gf) = program.items.iter().find_map(|item| match item {
                    Item::Function(gf) if &gf.name == g => Some(gf),
                    _ => None,
                }) else {
                    return false;
                };
                args.iter().enumerate().any(|(j, a)| {
                    matches!(&a.value.kind, ExprKind::Identifier(n) if n == param)
                        && fn_always_returns_param(Some(program), gf, j)
                })
            }
            _ => false,
        };
        match &e.kind {
            ExprKind::Match { scrutinee, arms } if scrutinee_is_param(scrutinee) => {
                let via_handback_call = !matches!(scrutinee.kind, ExprKind::Identifier(_));
                for a in arms {
                    if matches!(a.pattern.kind, PatternKind::Tuple(_)) {
                        walk(&a.body, param, fn_body, rule, stored, stored_block, out);
                        continue;
                    }
                    let names = payload_names_that_can_carry_a_body(
                        &a.pattern,
                        a.pattern.binding_names(),
                        rule,
                    );
                    // A whole-value pattern (`o => ..`, and a bare `None`,
                    // which parses as one) binds no PAYLOAD, so it is not
                    // owned the way a variant arm's binding is.
                    let binds_payload = !matches!(
                        a.pattern.kind,
                        PatternKind::Binding(_) | PatternKind::Wildcard
                    );
                    if !names.is_empty()
                        && ((via_handback_call && binds_payload)
                            || names.iter().any(|n| payload_yields(&a.body, n, rule))
                            || payload_returns_any(&a.body, &names, rule)
                            || payload_escapes_by_assignment(&a.body, &names, fn_body, rule)
                            || stored(&a.body, &names))
                    {
                        note(out, &a.pattern);
                    }
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                ..
            } if scrutinee_is_param(value) && !matches!(pattern.kind, PatternKind::Tuple(_)) => {
                let names =
                    payload_names_that_can_carry_a_body(pattern, pattern.binding_names(), rule);
                // B-2026-09-24-9 — the `if let` twin of the `match` arm above.
                let via_handback_call = !matches!(value.kind, ExprKind::Identifier(_))
                    && !matches!(
                        pattern.kind,
                        PatternKind::Binding(_) | PatternKind::Wildcard
                    );
                if !names.is_empty()
                    && (via_handback_call
                        || payload_returns_any_block(then_block, &names, rule)
                        || payload_escapes_by_assignment_block(then_block, &names, fn_body, rule)
                        || stored_block(then_block, &names))
                {
                    note(out, pattern);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } if scrutinee_is_param(value) && !matches!(pattern.kind, PatternKind::Tuple(_)) => {
                let names =
                    payload_names_that_can_carry_a_body(pattern, pattern.binding_names(), rule);
                if !names.is_empty()
                    && (payload_returns_any_block(body, &names, rule)
                        || payload_escapes_by_assignment_block(body, &names, fn_body, rule)
                        || stored_block(body, &names))
                {
                    note(out, pattern);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                walk(scrutinee, param, fn_body, rule, stored, stored_block, out);
                for a in arms {
                    walk(&a.body, param, fn_body, rule, stored, stored_block, out);
                }
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => {
                walk_block_for(b, param, fn_body, rule, stored, stored_block, out)
            }
            ExprKind::Return(Some(inner)) => {
                walk(inner, param, fn_body, rule, stored, stored_block, out)
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk(condition, param, fn_body, rule, stored, stored_block, out);
                walk_block_for(then_block, param, fn_body, rule, stored, stored_block, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, param, fn_body, rule, stored, stored_block, out);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                walk(value, param, fn_body, rule, stored, stored_block, out);
                walk_block_for(then_block, param, fn_body, rule, stored, stored_block, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, param, fn_body, rule, stored, stored_block, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => {
                walk_block_for(body, param, fn_body, rule, stored, stored_block, out)
            }
            _ => {}
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn walk_block_for(
        b: &Block,
        param: &str,
        fn_body: &Block,
        rule: CallYieldRule<'_>,
        stored: &dyn Fn(&Expr, &[String]) -> bool,
        stored_block: &dyn Fn(&Block, &[String]) -> bool,
        out: &mut Vec<String>,
    ) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => walk(e, param, fn_body, rule, stored, stored_block, out),
                StmtKind::Let { value, .. } => {
                    walk(value, param, fn_body, rule, stored, stored_block, out)
                }
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            walk(fe, param, fn_body, rule, stored, stored_block, out);
        }
    }
    let mut out = Vec::new();
    walk_block_for(
        &f.body,
        param_name,
        &f.body,
        rule,
        &stored,
        &stored_block,
        &mut out,
    );
    out
}

/// B-2026-09-05-28 / -30 — the per-ELEMENT sibling of
/// [`fn_returns_param_payload`] for a TUPLE pattern over by-value parameter
/// `arg_index`: which top-level element indices does `f` hand across the frame
/// boundary through a `match` / `if let` / `while let` arm that destructures
/// the param as a tuple?
///
/// ```text
/// fn pf(t: (R, i64)) -> i64 { match t { (r, k) => { k } } }        // [1]
/// fn p4(t: (R, i64)) -> R   { match t { (r, k) => { r } } }        // [0]
/// fn pw(t: (R, i64)) -> R   { match t { (r, k) => { wrap(r) } } }  // [0] — `wrap` returns its param
/// fn pc(t: (R, i64)) -> i64 { match t { (r, k) => { consume(r) } } } // []  — `consume` does not
/// ```
///
/// The whole-param predicate used to answer these arms too, and its answer was
/// a whole-argument stand-down: `pf` hands back `k`, so the caller ran NO
/// element body and `r`'s was lost; `pc` passed `r` to a call, which the
/// whole-param `yields` counts as an escape unconditionally, so the same. The
/// compiled backends were right on every cell because they never asked it of a
/// tuple. The bare / alias / aggregate-literal spellings here overlap
/// [`fn_returns_param_part_paths`] (which reports them as length-1
/// `TupleIndex` paths) — the two are unioned by the interpreter's
/// `callee_escaping_tuple_elems`, and this one exists for the shape the part
/// channel cannot classify: an element FORWARDED through a call.
///
/// PROGRAM-AWARE for that call, the way [`fn_returns_param_via_call`] is and
/// for its reason: passing an element to a call proves nothing on its own, so
/// the callee's own answer decides. ONE LEVEL, and the argument must be the
/// element BARE; an unknown callee (a constructor path, a method, a name that
/// resolves to nothing) counts as an escape, which is the conservative
/// direction this family runs on — a missed escape doubles a body, a false one
/// loses the only body that runs. A NESTED sub-pattern (`((r, j), k)`) escapes
/// as its whole element when any of its bindings does, since a flat index list
/// cannot name anything deeper.
pub fn fn_returns_param_tuple_arm_elems(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> Vec<usize> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return Vec::new();
    };
    // The roots whose storage outlives the call, as
    // `fn_moves_param_into_outliving_place` computes them: a borrowed `self`
    // and every `ref` / `mut ref` parameter. An element stored under one of
    // them (`v.push(r)`) is alive in the caller's own object when the call
    // returns, so its body belongs to that object's drain, not to the walk.
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
    let cx = TupleArmCx {
        program,
        fn_body: &f.body,
        roots: &roots,
    };
    fn walk(e: &Expr, param: &str, cx: TupleArmCx<'_>, out: &mut Vec<usize>) {
        let scrutinee_is_param =
            |s: &Expr| matches!(&s.kind, ExprKind::Identifier(n) if n == param);
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                walk(scrutinee, param, cx, out);
                for a in arms {
                    if scrutinee_is_param(scrutinee) {
                        if let PatternKind::Tuple(pats) = &a.pattern.kind {
                            tuple_arm_escaping_elems(pats, &a.body, cx, out);
                        }
                    }
                    walk(&a.body, param, cx, out);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                if scrutinee_is_param(value) {
                    if let PatternKind::Tuple(pats) = &pattern.kind {
                        tuple_arm_escaping_elems_block(pats, then_block, cx, out);
                    }
                }
                walk(value, param, cx, out);
                walk_block(then_block, param, cx, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, param, cx, out);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                if scrutinee_is_param(value) {
                    if let PatternKind::Tuple(pats) = &pattern.kind {
                        tuple_arm_escaping_elems_block(pats, body, cx, out);
                    }
                }
                walk(value, param, cx, out);
                walk_block(body, param, cx, out);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b, param, cx, out),
            ExprKind::Return(Some(inner)) => walk(inner, param, cx, out),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk(condition, param, cx, out);
                walk_block(then_block, param, cx, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, param, cx, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body, param, cx, out),
            _ => {}
        }
    }
    fn walk_block(b: &Block, param: &str, cx: TupleArmCx<'_>, out: &mut Vec<usize>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => walk(e, param, cx, out),
                StmtKind::Let { value, .. } => walk(value, param, cx, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            walk(fe, param, cx, out);
        }
    }
    let mut out = Vec::new();
    walk_block(&f.body, param_name, cx, &mut out);
    out.sort_unstable();
    out
}

/// B-2026-09-14-18 — lifted out of [`fn_returns_param_tuple_arm_elems`]
/// so the PAYLOAD-destructure sibling below asks the same four escape
/// routes of the same arm shape. Two copies of this judgement would drift,
/// and the two predicates have to agree: one answers a tuple PARAMETER's
/// arm, the other a tuple PAYLOAD's, and a part is handed out under exactly
/// the same conditions in both.
fn note_elem(out: &mut Vec<usize>, i: usize) {
    if !out.contains(&i) {
        out.push(i);
    }
}
/// The arm's bindings, ELEMENT by element: does element `i`'s binding set
/// leave the frame through `body`? Four routes: handed back (bare, via an
/// alias, inside a returned aggregate, or through a call that returns
/// it), assigned into a place the function then returns, stored under an
/// outliving root directly (`v.push(r)`), or handed to a callee that
/// stores it (`stash(r, v)`). The last is what a STATEMENT-position call
/// needs: the return-site walks only look at tails and `return`s.
pub(crate) fn tuple_arm_escaping_elems(
    pats: &[Pattern],
    body: &Expr,
    cx: TupleArmCx<'_>,
    out: &mut Vec<usize>,
) {
    let rule = CallYieldRule::ReturnsIt(cx.program);
    for (i, p) in pats.iter().enumerate() {
        let names: Vec<String> = p.binding_names();
        if !names.is_empty()
            && (names.iter().any(|n| payload_yields(body, n, rule))
                || payload_returns_any(body, &names, rule)
                || payload_escapes_by_assignment(body, &names, cx.fn_body, rule)
                || names.iter().any(|n| {
                    outliving_store::stores(body, n, cx.roots)
                        || stored_via_call(body, n, cx.program)
                }))
        {
            note_elem(out, i);
        }
    }
}
pub(crate) fn tuple_arm_escaping_elems_block(
    pats: &[Pattern],
    body: &Block,
    cx: TupleArmCx<'_>,
    out: &mut Vec<usize>,
) {
    let rule = CallYieldRule::ReturnsIt(cx.program);
    for (i, p) in pats.iter().enumerate() {
        let names: Vec<String> = p.binding_names();
        if !names.is_empty()
            && (payload_returns_any_block(body, &names, rule)
                || payload_escapes_by_assignment_block(body, &names, cx.fn_body, rule)
                || names.iter().any(|n| {
                    outliving_store::walk_block(body, n, cx.roots)
                        || stored_via_call_block(body, n, cx.program)
                }))
        {
            note_elem(out, i);
        }
    }
}

/// B-2026-09-14-18 — [`fn_returns_param_tuple_arm_elems`] one level deeper:
/// which ELEMENTS of a by-value `Option`/`Result` parameter's TUPLE PAYLOAD
/// does `f` hand out of its frame, when the arm destructures that payload
/// element-wise?
///
/// ```text
/// fn eat(o: Option[(R, i64)]) -> i64 { match o { Some((a, b)) => return b, .. } }  // [1]
/// fn eat(o: Option[(R, R)]) -> R     { match o { Some((a, b)) => return a, .. } }  // [0]
/// fn eat(o: Option[(R, R)]) -> i64   { match o { Some((a, b)) => a.id + b.id, .. } } // []
/// ```
///
/// THE GAP THIS FILLS. `fn_returns_param_payload_of` answers the same arms with
/// one bit — "Some escapes" — and the caller then stands the WHOLE payload
/// down, so the sibling part that stayed behind had its owed `Drop` body run by
/// nobody. Measured `got:9 end` against a due `dR5 got:9 end`, and
/// `got:5 dR5 end` against `dR6 got:5 dR5 end`, on ALL FOUR surfaces — which is
/// why no comparison between the backends could see it and why it sat open
/// through four sessions of this family's work.
///
/// `fn_escaping_param_payload_part_paths` is the PROJECTION sibling
/// (`Some(t) => return t.0`) and deliberately reports nothing for a
/// destructure; this reports nothing for a whole-payload binding. The two
/// channels partition the arms rather than overlapping, so a caller may apply
/// both masks without running one twice.
///
/// Escape is the same four routes the tuple-PARAMETER predicate uses, through
/// the same lifted helpers, so the two cannot drift: handed back, assigned into
/// a returned place, stored under an outliving root, or handed to a callee that
/// stores it.
pub fn fn_escaping_param_payload_destructured_elems(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
    variant: Option<&str>,
) -> Vec<usize> {
    let Some(param) = f.params.get(arg_index) else {
        return Vec::new();
    };
    // A borrow hands nothing of the caller's value out, so the caller's own
    // walk stays the only owner — the same first test every sibling on this
    // channel applies.
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return Vec::new();
    }
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return Vec::new();
    };
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
    let cx = TupleArmCx {
        program,
        fn_body: &f.body,
        roots: &roots,
    };
    /// The tuple sub-patterns of an arm that destructures the payload of the
    /// variant asked about, or `None` for every other arm shape.
    ///
    /// `Some(t)` (whole binding) and `Some(K.A(r))` (a nested non-tuple
    /// sub-pattern) both answer `None`: neither has elements this predicate can
    /// index, and reporting on them would collide with the channel that does
    /// own them.
    fn payload_tuple_pats<'p>(
        pattern: &'p Pattern,
        variant: Option<&str>,
    ) -> Option<&'p [Pattern]> {
        let PatternKind::TupleVariant { path, patterns } = &pattern.kind else {
            return None;
        };
        let v = path.last()?.as_str();
        if !matches!(v, "Some" | "Ok" | "Err") {
            return None;
        }
        if variant.is_some_and(|want| want != v) {
            return None;
        }
        let [only] = patterns.as_slice() else {
            return None;
        };
        match &only.kind {
            PatternKind::Tuple(elems) => Some(elems.as_slice()),
            _ => None,
        }
    }
    fn walk(
        e: &Expr,
        param: &str,
        variant: Option<&str>,
        cx: TupleArmCx<'_>,
        out: &mut Vec<usize>,
    ) {
        let scrutinee_is_param =
            |s: &Expr| matches!(&s.kind, ExprKind::Identifier(n) if n == param);
        match &e.kind {
            ExprKind::Match { scrutinee, arms } => {
                walk(scrutinee, param, variant, cx, out);
                for a in arms {
                    if scrutinee_is_param(scrutinee) {
                        if let Some(pats) = payload_tuple_pats(&a.pattern, variant) {
                            tuple_arm_escaping_elems(pats, &a.body, cx, out);
                        }
                    }
                    walk(&a.body, param, variant, cx, out);
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                if scrutinee_is_param(value) {
                    if let Some(pats) = payload_tuple_pats(pattern, variant) {
                        tuple_arm_escaping_elems_block(pats, then_block, cx, out);
                    }
                }
                walk(value, param, variant, cx, out);
                walk_block(then_block, param, variant, cx, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, param, variant, cx, out);
                }
            }
            ExprKind::WhileLet {
                pattern,
                value,
                body,
                ..
            } => {
                if scrutinee_is_param(value) {
                    if let Some(pats) = payload_tuple_pats(pattern, variant) {
                        tuple_arm_escaping_elems_block(pats, body, cx, out);
                    }
                }
                walk(value, param, variant, cx, out);
                walk_block(body, param, variant, cx, out);
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b, param, variant, cx, out),
            ExprKind::Return(Some(inner)) => walk(inner, param, variant, cx, out),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                walk(condition, param, variant, cx, out);
                walk_block(then_block, param, variant, cx, out);
                if let Some(x) = else_branch.as_deref() {
                    walk(x, param, variant, cx, out);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                walk(condition, param, variant, cx, out);
                walk_block(body, param, variant, cx, out);
            }
            ExprKind::For { iterable, body, .. } => {
                walk(iterable, param, variant, cx, out);
                walk_block(body, param, variant, cx, out);
            }
            ExprKind::Loop { body, .. } => walk_block(body, param, variant, cx, out),
            ExprKind::LabeledBlock { body, .. } => walk_block(body, param, variant, cx, out),
            _ => {}
        }
    }
    fn walk_block(
        b: &Block,
        param: &str,
        variant: Option<&str>,
        cx: TupleArmCx<'_>,
        out: &mut Vec<usize>,
    ) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Expr(e) => walk(e, param, variant, cx, out),
                StmtKind::Let { value, .. } => walk(value, param, variant, cx, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            walk(fe, param, variant, cx, out);
        }
    }
    let mut out = Vec::new();
    walk_block(&f.body, param_name, variant, cx, &mut out);
    out.sort_unstable();
    out
}

/// What [`fn_returns_param_tuple_arm_elems`]'s walks carry: the program (for
/// the callee lookups), the whole function body (for the assignment route),
/// and the outliving roots (for the store routes).
#[derive(Clone, Copy)]
pub(crate) struct TupleArmCx<'a> {
    program: &'a crate::Program,
    fn_body: &'a Block,
    roots: &'a [&'a str],
}

/// B-2026-09-05-28 — is `name` handed BARE, anywhere inside `e`, to a free
/// function that moves that parameter into a place outliving ITS call
/// ([`fn_moves_param_into_outliving_place`])? `stash(r, v)` with
/// `fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }` — the element is alive in
/// `v` when the arm ends, so no walk may run its body. Statement position
/// included, which is what the return-site walks cannot see. ONE LEVEL, like
/// every interprocedural question in this family.
fn stored_via_call(e: &Expr, name: &str, program: &crate::Program) -> bool {
    match &e.kind {
        ExprKind::Call { callee, args, .. } => {
            let direct = match &callee.kind {
                ExprKind::Identifier(g) => program
                    .items
                    .iter()
                    .find_map(|item| match item {
                        Item::Function(gf) if &gf.name == g => Some(gf),
                        _ => None,
                    })
                    .is_some_and(|gf| {
                        args.iter().enumerate().any(|(j, a)| {
                            matches!(&a.value.kind, ExprKind::Identifier(n) if n == name)
                                && fn_moves_param_into_outliving_place(gf, j)
                        })
                    }),
                _ => false,
            };
            direct
                || args
                    .iter()
                    .any(|a| stored_via_call(&a.value, name, program))
        }
        ExprKind::MethodCall { object, args, .. } => {
            stored_via_call(object, name, program)
                || args
                    .iter()
                    .any(|a| stored_via_call(&a.value, name, program))
        }
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => stored_via_call_block(b, name, program),
        ExprKind::Return(Some(inner)) => stored_via_call(inner, name, program),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            stored_via_call(condition, name, program)
                || stored_via_call_block(then_block, name, program)
                || else_branch
                    .as_deref()
                    .is_some_and(|x| stored_via_call(x, name, program))
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            stored_via_call(value, name, program)
                || stored_via_call_block(then_block, name, program)
                || else_branch
                    .as_deref()
                    .is_some_and(|x| stored_via_call(x, name, program))
        }
        ExprKind::Match { scrutinee, arms } => {
            stored_via_call(scrutinee, name, program)
                || arms.iter().any(|a| stored_via_call(&a.body, name, program))
        }
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::For { body, .. }
        | ExprKind::Loop { body, .. }
        | ExprKind::LabeledBlock { body, .. } => stored_via_call_block(body, name, program),
        _ => false,
    }
}

fn stored_via_call_block(b: &Block, name: &str, program: &crate::Program) -> bool {
    b.stmts.iter().any(|st| match &st.kind {
        StmtKind::Expr(e) => stored_via_call(e, name, program),
        StmtKind::Let { value, .. } => stored_via_call(value, name, program),
        StmtKind::Assign { value, .. } => stored_via_call(value, name, program),
        _ => false,
    }) || b
        .final_expr
        .as_deref()
        .is_some_and(|fe| stored_via_call(fe, name, program))
}

/// B-2026-09-05-28 / -36 — does free function `gf` take by-value parameter
/// `j` OVER from its caller: hand it back (bare, always, conditionally, as a
/// payload or element bound out of it, or through a further call), or move it
/// into a home that outlives the call? The one interprocedural question every
/// "handed to a call" arm in this family asks, so it lives in one place. A
/// returned FIELD projection (`fn consume(x: R) -> i64 { x.id }`) is not
/// counted: only a copy of one field leaves, the value itself dies in the
/// callee.
fn callee_takes_param_over(program: &crate::Program, gf: &Function, j: usize) -> bool {
    // B-2026-09-05-35 — CYCLE GUARD. Every interprocedural arm of this family
    // routes through here, and a recursive callee (`fn len(l: L) -> i64 {
    // match l { L.Cons(_, rest) => 1 + len(rest), .. } }`) asks this question
    // of itself from inside its own answer: `fn_escaping_param_payload_variants
    // (len)` reaches `len(rest)`, which asks `callee_takes_param_over(len, 0)`,
    // which walks `len`'s arms again — measured as a stack overflow in
    // `e2e_direct_recursive_shared_enum_single_field`. A question already in
    // flight answers `true`, the conservative direction on both channels: the
    // caller stands down and a body is at worst missed, never doubled. Mutual
    // recursion (`g` → `h` → `g`) passes through here too, so one guard covers
    // every route.
    thread_local! {
        static IN_FLIGHT: std::cell::RefCell<Vec<(String, usize)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    let key = (gf.name.clone(), j);
    let re_entered = IN_FLIGHT.with(|f| f.borrow().contains(&key));
    if re_entered {
        return true;
    }
    IN_FLIGHT.with(|f| f.borrow_mut().push(key.clone()));
    let answer = callee_takes_param_over_inner(program, gf, j);
    IN_FLIGHT.with(|f| {
        let mut v = f.borrow_mut();
        if let Some(pos) = v.iter().rposition(|k| k == &key) {
            v.remove(pos);
        }
    });
    answer
}

fn callee_takes_param_over_inner(program: &crate::Program, gf: &Function, j: usize) -> bool {
    fn_returns_param(gf, j)
        || fn_always_returns_param(Some(program), gf, j)
        || fn_conditionally_returns_param_bare(Some(program), gf, j)
        || fn_returns_param_payload_of(program, gf, j, None)
        || fn_returns_param_via_call(program, gf, j)
        || fn_moves_param_into_outliving_place(gf, j)
        || fn_moves_param_into_outliving_place_via_call(program, gf, j)
        || !fn_returns_param_tuple_arm_elems(program, gf, j).is_empty()
}

/// B-2026-09-06-13 — is by-value parameter `arg_index` handed BARE, at a
/// statement-level site both backends disarm, to a callee that returns THAT
/// parameter on some exits and not others (`fn_conditionally_returns_param_bare`)
/// — and moved nowhere else?
///
/// `fn s_cond(r: R, k: bool) { let w: R = keepc(r, k); println(..) }` over
/// `fn keepc(r: R, k: bool) -> R { if k { return r; } return mk(99); }`:
/// `keepc`'s frame owns `r`'s body per path (the return flip), so exactly one
/// body is produced inside `s_cond`'s dynamic extent on every path — in `keepc`
/// when the value dies there, in `w` when it is handed back — and the OUTER
/// caller's fire after the call was the second one on every path (`sc 1 dR1
/// dR1` / `dR2 sc 99 dR99 dR2`, all four surfaces). A hand-over to such a
/// callee is a CONDITIONAL STORE in every respect that matters: the caller
/// stands down ([`fn_moves_param_into_outliving_place_via_call`] reports it),
/// and where the hand-over is itself nested in a branch the frame registers
/// B-2026-08-30-28's bodies-only per-path drop, cleared at the handing
/// statement by `arm_conditional_store_flag` /
/// `disarm_cond_store_param_on_handover` — which is why the admitted sites are
/// exactly the statement shapes those two recognize: a `let` / assignment RHS,
/// a statement-position call, a `return` operand, at any block depth.
///
/// Deliberately NOT an always-returning callee (`keeps(r)`): there the result
/// binding is a VIEW and the caller keeps firing (B-2026-09-06-9), the other
/// convention, and admitting it here would stand the caller down beside a
/// view — a lost body. Any OTHER move of the parameter (a bare `return r`, a
/// store, a hand-over nested inside another expression) declines the whole
/// function, the recoverable direction: today's double stays a double rather
/// than becoming a silent loss.
pub fn fn_conditionally_hands_param_to_flip_callee(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return false;
    }
    let PatternKind::Binding(name) = &param.pattern.kind else {
        return false;
    };
    // B-2026-09-07-8 — under every name the param is rebound to WHOLE
    // (`let q = a; f(q, c);`), which is the same act as handing `a` over: a
    // rebind of a by-value param is a view onto the same object, and this
    // predicate's whole question is whether some other frame takes the body
    // per path. Without the aliases the rebound spelling answered `false`
    // here, the caller registered a full owner for its temp, and the callee's
    // per-path body fired alongside it — `dR1 g dR1`, two bodies for one
    // object, agreed by every backend and so invisible to the A/B gate and to
    // every sanitizer. See `param_whole_aliases` for what qualifies.
    let alias_names = param_whole_aliases(Some(program), f, name);
    struct Tally {
        clearable: usize,
        other: bool,
    }
    /// Is `e` a direct call handing `name` (or one of its whole aliases) bare
    /// to a flip callee? The index of that argument, if so.
    fn flip_call(e: &Expr, name: &[String], program: &crate::Program) -> Option<usize> {
        let ExprKind::Call { callee, args } = &e.kind else {
            return None;
        };
        let key = match &callee.kind {
            ExprKind::Identifier(g) => g.clone(),
            ExprKind::Path { segments, .. } => segments.join("."),
            _ => return None,
        };
        let gf = resolve_free_or_assoc_fn(program, &key)?;
        args.iter().enumerate().find_map(|(j, a)| {
            (matches!(&a.value.kind, ExprKind::Identifier(n) if name.iter().any(|al| al == n))
                && fn_conditionally_returns_param_bare(Some(program), gf, j))
            .then_some(j)
        })
    }
    fn classify(e: &Expr, name: &[String], program: &crate::Program, t: &mut Tally) {
        if let Some(j) = flip_call(e, name, program) {
            t.clearable += 1;
            if let ExprKind::Call { args, .. } = &e.kind {
                if args.iter().enumerate().any(|(k, a)| {
                    k != j && name.iter().any(|al| outliving_store::moves(&a.value, al))
                }) {
                    t.other = true;
                }
            }
            return;
        }
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => walk_block(b, name, program, t),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                if name.iter().any(|al| outliving_store::moves(condition, al)) {
                    t.other = true;
                }
                walk_block(then_block, name, program, t);
                if let Some(x) = else_branch.as_deref() {
                    classify(x, name, program, t);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                if name.iter().any(|al| outliving_store::moves(value, al)) {
                    t.other = true;
                }
                walk_block(then_block, name, program, t);
                if let Some(x) = else_branch.as_deref() {
                    classify(x, name, program, t);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                if name.iter().any(|al| outliving_store::moves(scrutinee, al)) {
                    t.other = true;
                }
                for a in arms {
                    classify(&a.body, name, program, t);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                if name.iter().any(|al| outliving_store::moves(condition, al)) {
                    t.other = true;
                }
                walk_block(body, name, program, t);
            }
            ExprKind::WhileLet { value, body, .. } => {
                if name.iter().any(|al| outliving_store::moves(value, al)) {
                    t.other = true;
                }
                walk_block(body, name, program, t);
            }
            ExprKind::For { iterable, body, .. } => {
                if name.iter().any(|al| outliving_store::moves(iterable, al)) {
                    t.other = true;
                }
                walk_block(body, name, program, t);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                walk_block(body, name, program, t)
            }
            ExprKind::Return(Some(inner)) => classify(inner, name, program, t),
            _ => {
                if name.iter().any(|al| outliving_store::moves(e, al)) {
                    t.other = true;
                }
            }
        }
    }
    fn walk_block(b: &Block, name: &[String], program: &crate::Program, t: &mut Tally) {
        for st in &b.stmts {
            match &st.kind {
                // B-2026-09-07-8 — the rebind that CREATES an alias is not an
                // "other move" of the parameter. `let q = a;` reaches the
                // catch-all below as a bare identifier and sets `other`, which
                // disqualified the very function whose alias set it seeded:
                // `fn g(a: R, c: bool) { let q = a; f(q, c); }` answered false
                // where the un-rebound `f(a, c)` answered true, so the caller
                // registered a full owner for its temp and the callee's
                // per-path body fired alongside it — `dR1 g dR1`, two bodies
                // for one object on every backend at once.
                StmtKind::Let { pattern, value, .. }
                    if matches!(&pattern.kind, PatternKind::Binding(b)
                        if name.iter().any(|al| al == b))
                        && matches!(&value.kind, ExprKind::Identifier(src)
                            if name.iter().any(|al| al == src)) => {}
                StmtKind::Let { value, .. } | StmtKind::Assign { value, .. } => {
                    classify(value, name, program, t)
                }
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    classify(value, name, program, t);
                    walk_block(else_block, name, program, t);
                }
                StmtKind::Expr(e) => classify(e, name, program, t),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            classify(fe, name, program, t);
        }
    }
    let mut t = Tally {
        clearable: 0,
        other: false,
    };
    walk_block(&f.body, &alias_names, program, &mut t);
    t.clearable > 0 && !t.other
}

/// B-2026-09-05-36 — the program-aware sibling of
/// [`fn_moves_param_into_outliving_place`]: is by-value parameter `arg_index`
/// handed BARE to a free function that moves that parameter into a place
/// outliving ITS call? `fn b_stash(x: R, v: mut ref Vec[R]) { stash(x, v) }`
/// with `fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }`: `x` is alive in the
/// caller's `v` when `b_stash` returns, exactly as if `b_stash` had pushed it
/// itself, yet the intraprocedural predicate treats the call as opaque and the
/// caller's temp drop ran alongside the container's drain — two bodies for
/// one value on all four surfaces.
///
/// A separate entry point rather than a widening, for the reason
/// [`fn_returns_param_via_call`] gives: the intraprocedural predicate's
/// callee-side consumers (`compile_function`'s conditional-store
/// registration, the interpreter's twin) were measured on its current answer,
/// and only the CALLER-side stand-down asks this one. ONE LEVEL, argument
/// bare, statement position included; an unresolvable callee counts as
/// nothing here, the part channel's conservative direction.
pub fn fn_moves_param_into_outliving_place_via_call(
    program: &crate::Program,
    f: &Function,
    arg_index: usize,
) -> bool {
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
    stored_via_call_block(&f.body, param_name, program)
        // B-2026-09-06-13 — or handed to a callee that returns it on some
        // exits: that frame owns the body per path, so the caller stands down
        // exactly as for a store.
        || fn_conditionally_hands_param_to_flip_callee(program, f, arg_index)
}

/// How the payload-escape walks below treat a CALL that takes the tracked
/// binding as an argument (B-2026-09-05-28).
#[derive(Clone, Copy)]
enum CallYieldRule<'p> {
    /// Every such call counts as an escape — [`fn_returns_param_payload`]'s
    /// reading, kept verbatim for the enum-payload consumers it was measured
    /// on.
    Any,
    /// Only a call whose callee is a KNOWN free function that hands that
    /// parameter back (or moves it somewhere outliving the call) counts; a
    /// consuming callee (`fn consume(x: R) -> i64 { x.id }`) does not. An
    /// unknown callee still counts — the conservative direction.
    ReturnsIt(&'p crate::Program),
}

/// Does `e` hand `name` across the frame boundary — bare, moved into a
/// returned aggregate literal, or handed to a call per `rule`? A FIELD
/// projection (`r.id`) deliberately does not count: the payload stays behind,
/// only a copy of one field leaves.
/// B-2026-09-06-26 — of the names an arm's pattern binds, the ones whose value
/// could carry a user `Drop` body at all.
///
/// The escape predicates above answer a BODIES question: does the caller's
/// retained walk over the argument have to stand down because the value it
/// would fire for has left the frame? A leaf whose DECLARED type is a scalar
/// primitive or a `String` (`H2 { e, n }` with `n: i64`) cannot carry a body wherever it
/// goes, so its leaving proves nothing about the argument — yet the scanner
/// counted it, and for a plain-STRUCT pattern there is one "variant" (the
/// struct itself), so `return n` stood the whole argument down and the
/// enum field's `dE dR` ran nowhere under `--interp` (the compiled backends
/// never ask this predicate of a struct argument, which is why they were
/// right on the same cells). Program-aware only: the `Any` rule has no
/// declarations to read and keeps every name, its callee-side consumers'
/// measured behaviour. For an enum this narrows the -35 per-variant answer
/// the same way: `E.B(k) => k` with `k: i64` no longer reports `B`, whose
/// payload has no body to mask.
fn payload_names_that_can_carry_a_body(
    pattern: &Pattern,
    names: Vec<String>,
    rule: CallYieldRule<'_>,
) -> Vec<String> {
    let CallYieldRule::ReturnsIt(program) = rule else {
        return names;
    };
    names
        .into_iter()
        .filter(|n| pattern_leaf_may_carry_body(program, pattern, n))
        .collect()
}

/// The DECLARED type of the field / payload position `name` binds under
/// `pattern`, looked up in the program's struct and enum declarations; `true`
/// (may carry) whenever the shape or the declaration cannot be resolved —
/// the conservative direction this family runs on.
fn pattern_leaf_may_carry_body(program: &crate::Program, pattern: &Pattern, name: &str) -> bool {
    let declared: Option<&TypeExpr> = match &pattern.kind {
        PatternKind::Struct { path, fields, .. } => {
            let Some(last) = path.last() else {
                return true;
            };
            let Some(field) = fields.iter().find_map(|f| match &f.pattern {
                None if f.name == name => Some(f.name.as_str()),
                Some(sub) if matches!(&sub.kind, PatternKind::Binding(b) if b == name) => {
                    Some(f.name.as_str())
                }
                _ => None,
            }) else {
                return true;
            };
            let plain = program.items.iter().find_map(|it| match it {
                Item::StructDef(s) if &s.name == last => {
                    s.fields.iter().find(|f| f.name == field).map(|f| &f.ty)
                }
                _ => None,
            });
            plain.or_else(|| {
                program.items.iter().find_map(|it| match it {
                    Item::EnumDef(e) => {
                        e.variants
                            .iter()
                            .find(|v| &v.name == last)
                            .and_then(|v| match &v.kind {
                                VariantKind::Struct(fs) => {
                                    fs.iter().find(|f| f.name == field).map(|f| &f.ty)
                                }
                                _ => None,
                            })
                    }
                    _ => None,
                })
            })
        }
        PatternKind::TupleVariant { path, patterns } => {
            let Some(last) = path.last() else {
                return true;
            };
            let Some(pos) = patterns
                .iter()
                .position(|p| matches!(&p.kind, PatternKind::Binding(b) if b == name))
            else {
                return true;
            };
            program.items.iter().find_map(|it| match it {
                Item::EnumDef(e) => {
                    e.variants
                        .iter()
                        .find(|v| &v.name == last)
                        .and_then(|v| match &v.kind {
                            VariantKind::Tuple(ts) => ts.get(pos),
                            _ => None,
                        })
                }
                _ => None,
            })
        }
        _ => None,
    };
    !declared.is_some_and(type_expr_cannot_carry_drop_body)
}

/// A type that can own nothing a user `Drop` body observes: a bare scalar
/// primitive, unit, or `String` (heap, but no user `Drop` and nothing inside
/// that could declare one). Every other shape — a user type, a generic
/// container, a tuple — may carry one and keeps the conservative answer.
fn type_expr_cannot_carry_drop_body(te: &TypeExpr) -> bool {
    match &te.kind {
        crate::ast::TypeKind::Unit => true,
        crate::ast::TypeKind::Path(p) if p.generic_args.is_none() => {
            matches!(p.segments.as_slice(), [n] if matches!(
                n.as_str(),
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32"
                    | "u64" | "u128" | "usize" | "f32" | "f64" | "bool" | "char" | "String"
            ))
        }
        _ => false,
    }
}

fn payload_yields(e: &Expr, name: &str, rule: CallYieldRule<'_>) -> bool {
    match &e.kind {
        ExprKind::Identifier(n) => n == name,
        ExprKind::StructLiteral { fields, .. } => {
            fields.iter().any(|f| payload_yields(&f.value, name, rule))
        }
        ExprKind::Tuple(elems) => elems.iter().any(|el| payload_yields(el, name, rule)),
        ExprKind::Call { callee, args, .. } => args.iter().enumerate().any(|(j, a)| {
            if !payload_yields(&a.value, name, rule) {
                return false;
            }
            // B-2026-09-06-26 — a LOWERED PRIMITIVE OPERATOR. By the time the
            // interpreter runs, `lower_program` has rewritten `r.id + n` into
            // `i64.add(r.id, n)`: a `Call` whose callee is a `Path`, which the
            // resolution below cannot name and so counted as "unknown callee,
            // keeps the escape". An intrinsic returns a fresh scalar and takes
            // nothing over, under either rule.
            if crate::consume_class::is_lowered_primitive_operator(callee) {
                return false;
            }
            let CallYieldRule::ReturnsIt(program) = rule else {
                return true;
            };
            // The element BARE, to a resolvable free function: its own
            // ownership answer decides. Anything else keeps the escape.
            if !matches!(&a.value.kind, ExprKind::Identifier(n) if n == name) {
                return true;
            }
            let ExprKind::Identifier(g) = &callee.kind else {
                return true;
            };
            let Some(gf) = program.items.iter().find_map(|item| match item {
                Item::Function(gf) if &gf.name == g => Some(gf),
                _ => None,
            }) else {
                return true;
            };
            callee_takes_param_over(program, gf, j)
        }),
        _ => false,
    }
}

/// Every name that aliases one of `names` through a `let` in this block, so
/// the `let k = r; return k;` spelling is recognized as the same escape.
fn payload_grow_aliases(b: &Block, names: &mut Vec<String>, rule: CallYieldRule<'_>) {
    for st in &b.stmts {
        if let StmtKind::Let { pattern, value, .. } = &st.kind {
            if names.iter().any(|n| payload_yields(value, n, rule)) {
                if let PatternKind::Binding(dest) = &pattern.kind {
                    if !names.iter().any(|n| n == dest) {
                        names.push(dest.clone());
                    }
                }
            }
        }
    }
}

fn payload_returns_any(e: &Expr, names: &[String], rule: CallYieldRule<'_>) -> bool {
    match &e.kind {
        ExprKind::Return(Some(inner)) => {
            names.iter().any(|n| payload_yields(inner, n, rule))
                || payload_returns_any(inner, names, rule)
        }
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => payload_returns_any_block(b, names, rule),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            payload_returns_any_block(then_block, names, rule)
                || else_branch
                    .as_deref()
                    .is_some_and(|x| payload_returns_any(x, names, rule))
        }
        ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => {
            payload_returns_any_block(then_block, names, rule)
                || else_branch
                    .as_deref()
                    .is_some_and(|x| payload_returns_any(x, names, rule))
        }
        ExprKind::Match { arms, .. } => arms.iter().any(|a| {
            names.iter().any(|n| payload_yields(&a.body, n, rule))
                || payload_returns_any(&a.body, names, rule)
        }),
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::For { body, .. }
        | ExprKind::Loop { body, .. }
        | ExprKind::LabeledBlock { body, .. } => payload_returns_any_block(body, names, rule),
        _ => false,
    }
}

fn payload_returns_any_block(b: &Block, names: &[String], rule: CallYieldRule<'_>) -> bool {
    let mut names = names.to_vec();
    payload_grow_aliases(b, &mut names, rule);
    b.stmts.iter().any(|st| match &st.kind {
        StmtKind::Expr(e) => payload_returns_any(e, &names, rule),
        StmtKind::Let { value, .. } => payload_returns_any(value, &names, rule),
        _ => false,
    }) || b.final_expr.as_deref().is_some_and(|fe| {
        names.iter().any(|n| payload_yields(fe, n, rule)) || payload_returns_any(fe, &names, rule)
    })
}

/// B-2026-08-29-48 — the ROOTS of assignment targets inside `e` that receive
/// one of `names`: `out = r`, `out.slot = r`, `outs[i] = r`.
///
/// [`payload_grow_aliases`] already follows a payload through a `let`
/// (`let k = r; return k;`). It cannot follow one through an ASSIGNMENT,
/// and the reason is scope rather than oversight: the destination is
/// declared outside the arm and the `return` that carries it out sits
/// outside the arm too, so both ends of the route are invisible from inside
/// the arm body — which is all [`payload_returns_any`] ever sees. The roots
/// therefore come back out to the caller, which asks the whole FUNCTION
/// body whether they leave.
///
/// `MultiAssign` is deliberately not handled: [`crate::desugar`] rewrites
/// every one into `let` temps plus single `Assign`s before any consumer of
/// this predicate observes the program.
fn payload_place_root(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(n) => Some(n),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => payload_place_root(object),
        _ => None,
    }
}

fn payload_assigned_roots(
    e: &Expr,
    names: &[String],
    out: &mut Vec<String>,
    rule: CallYieldRule<'_>,
) {
    match &e.kind {
        ExprKind::Block(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => payload_assigned_roots_block(b, names, out, rule),
        ExprKind::Return(Some(inner)) => payload_assigned_roots(inner, names, out, rule),
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
            payload_assigned_roots_block(then_block, names, out, rule);
            if let Some(x) = else_branch.as_deref() {
                payload_assigned_roots(x, names, out, rule);
            }
        }
        ExprKind::Match { arms, .. } => {
            for a in arms {
                payload_assigned_roots(&a.body, names, out, rule);
            }
        }
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::For { body, .. }
        | ExprKind::Loop { body, .. }
        | ExprKind::LabeledBlock { body, .. } => {
            payload_assigned_roots_block(body, names, out, rule)
        }
        _ => {}
    }
}

fn payload_assigned_roots_block(
    b: &Block,
    names: &[String],
    out: &mut Vec<String>,
    rule: CallYieldRule<'_>,
) {
    for st in &b.stmts {
        match &st.kind {
            StmtKind::Assign { target, value } => {
                if names.iter().any(|n| payload_yields(value, n, rule)) {
                    if let Some(root) = payload_place_root(target) {
                        if !out.iter().any(|s| s == root) {
                            out.push(root.to_string());
                        }
                    }
                }
            }
            StmtKind::Expr(e) => payload_assigned_roots(e, names, out, rule),
            StmtKind::Let { value, .. } => payload_assigned_roots(value, names, out, rule),
            _ => {}
        }
    }
    if let Some(fe) = b.final_expr.as_deref() {
        payload_assigned_roots(fe, names, out, rule);
    }
}

/// Do any of `names` reach a return site by being ASSIGNED into a place
/// whose root the function then returns? `fn_body` is the whole body, not
/// the arm's, because that is where both the destination's declaration and
/// the `return` live.
fn payload_escapes_by_assignment(
    body: &Expr,
    names: &[String],
    fn_body: &Block,
    rule: CallYieldRule<'_>,
) -> bool {
    let mut roots: Vec<String> = Vec::new();
    payload_assigned_roots(body, names, &mut roots, rule);
    !roots.is_empty() && payload_returns_any_block(fn_body, &roots, rule)
}

fn payload_escapes_by_assignment_block(
    body: &Block,
    names: &[String],
    fn_body: &Block,
    rule: CallYieldRule<'_>,
) -> bool {
    let mut roots: Vec<String> = Vec::new();
    payload_assigned_roots_block(body, names, &mut roots, rule);
    !roots.is_empty() && payload_returns_any_block(fn_body, &roots, rule)
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
/// B-2026-08-29-49 / B-2026-08-30-28 — the shared syntax helpers behind the
/// three "does this callee store the param somewhere that outlives the call"
/// questions.
///
/// They were nested inside [`fn_moves_param_into_outliving_place`] until
/// B-2026-08-30-28 needed the SAME recognition for a second question (does
/// EVERY path store it, not just some path). Hoisting rather than duplicating
/// is what keeps the may- and must-analyses agreeing on which syntactic shapes
/// count as a store: a shape added to `stores` is admitted by both at once, so
/// the pair can never drift into disagreeing about what a store IS -- only
/// about how many paths take one.
mod outliving_store {
    use super::*;
    /// Is `e` a place expression whose ROOT is one of `roots`? Walks through
    /// field, index and tuple-index projections, so `self.buckets[i].inner`
    /// resolves to root `self`.
    pub(super) fn place_root_outlives(e: &Expr, roots: &[&str]) -> bool {
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
    pub(super) fn moves(e: &Expr, name: &str) -> bool {
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
    pub(super) fn is_bare(e: &Expr, name: &str) -> bool {
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
    pub(super) fn stores_via_destructure<'a>(
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
    pub(super) fn stores(e: &Expr, name: &str, roots: &[&str]) -> bool {
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

    /// B-2026-08-30-28 / -33 — does `name` leave the frame by a route neither
    /// backend can DISARM the per-path flag on?
    ///
    /// Renamed from `escapes_by_non_store_route` when -33 added the missing
    /// disarms: the store stopped being the only exit that clears the flag, so
    /// a name built around "non-store" no longer described the question. What
    /// matters is not which exit the value takes but whether both backends
    /// disarm at it — a `return`, a `let` and an assignment now do, at
    /// statement level, in `arm_conditional_store_flag` and
    /// `disarm_cond_store_param_on_handover`.
    ///
    /// The conditional-store registration hands the parameter's `Drop` body to
    /// a runtime flag that is cleared at the STORE. Any OTHER way the value can
    /// leave — returned, bound into a local that is then returned, handed to a
    /// call whose result escapes — is a path where the flag stays armed while
    /// some other frame has taken the value over, and the body runs TWICE. That
    /// is the unrecoverable direction, so the registration declines whenever
    /// such a route exists rather than trying to enumerate and clear them all.
    ///
    /// Measured, which is why this exists rather than being assumed safe: with
    /// the registration ungated, `fn f(sink: mut ref Vec[R], r: R) -> Option[W]
    /// { if .. { let w = W { r: r }; return Some(w); } if .. { sink.push(r); }
    /// return None; }` ran the body TWICE for the returned object on all four
    /// surfaces — `drop 301 ` with the string already moved out, then the real
    /// one. The bare-return and return-through-a-call spellings each doubled on
    /// at least one backend too.
    ///
    /// CONSERVATIVE-DECLINE: an unrecognized shape answers `true` and the
    /// parameter keeps today's behaviour (its body lost on the non-storing
    /// path). That is the same trade `fn_conditionally_returns_param_bare`'s
    /// condition 3 makes, and for the identical reason — a lost body is
    /// recoverable, a double is not.
    pub(super) fn escapes_by_unclearable_route(b: &Block, name: &str, roots: &[&str]) -> bool {
        fn in_expr(e: &Expr, name: &str, roots: &[&str]) -> bool {
            match &e.kind {
                // A `return` whose operand hands the value over, at any depth
                // (`return r`, `return Some(r)`, `return mk(r)`).
                // B-2026-08-30-33 — a `return` whose operand HANDS THE VALUE
                // OVER is now disarmed on both backends
                // (`arm_conditional_store_flag` /
                // `disarm_cond_store_param_on_handover`), so it is no longer an
                // unclearable route. Anything deeper still is.
                ExprKind::Return(Some(inner)) => {
                    if moves(inner, name) {
                        return false;
                    }
                    in_expr(inner, name, roots)
                }
                ExprKind::Return(None) => false,
                // THE store itself is the admitted route; its arguments are not
                // an escape. Anything else about the call still is.
                ExprKind::MethodCall { object, args, .. } => {
                    if place_root_outlives(object, roots)
                        && args.iter().any(|a| moves(&a.value, name))
                    {
                        return false;
                    }
                    args.iter()
                        .any(|a| moves(&a.value, name) || in_expr(&a.value, name, roots))
                        || in_expr(object, name, roots)
                }
                ExprKind::Call { args, .. } => args
                    .iter()
                    .any(|a| moves(&a.value, name) || in_expr(&a.value, name, roots)),
                ExprKind::Block(bb)
                | ExprKind::Unsafe(bb)
                | ExprKind::Try(bb)
                | ExprKind::Seq(bb)
                | ExprKind::Par(bb) => escapes_by_unclearable_route(bb, name, roots),
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
                    escapes_by_unclearable_route(then_block, name, roots)
                        || else_branch
                            .as_deref()
                            .is_some_and(|x| in_expr(x, name, roots))
                }
                ExprKind::Match { arms, .. } => arms.iter().any(|a| in_expr(&a.body, name, roots)),
                ExprKind::While { body, .. }
                | ExprKind::WhileLet { body, .. }
                | ExprKind::For { body, .. }
                | ExprKind::Loop { body, .. }
                | ExprKind::LabeledBlock { body, .. } => {
                    escapes_by_unclearable_route(body, name, roots)
                }
                _ => false,
            }
        }
        let stmt_escapes = b.stmts.iter().any(|st| match &st.kind {
            // B-2026-08-30-33 — `let w = W { r: r };` hands the value to the
            // local, and both backends disarm at that statement, so the local
            // is now the sole owner rather than a second one. Still declined
            // when the hand-over is nested somewhere the statement-level hook
            // does not match.
            StmtKind::Let { value, .. } => {
                if moves(value, name) {
                    return false;
                }
                in_expr(value, name, roots)
            }
            StmtKind::Assign { target, value } => {
                if place_root_outlives(target, roots) && moves(value, name) {
                    return false;
                }
                if moves(value, name) {
                    return false;
                }
                in_expr(value, name, roots)
            }
            StmtKind::Expr(e) => in_expr(e, name, roots),
            _ => false,
        });
        stmt_escapes
            || b.final_expr
                .as_deref()
                .is_some_and(|fe| moves(fe, name) || in_expr(fe, name, roots))
    }

    pub(super) fn walk_block(b: &Block, name: &str, roots: &[&str]) -> bool {
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
}

/// B-2026-09-03-7 — does `f` move parameter `arg_index` into a LOCAL aggregate?
///
/// `fn take(r: R) -> i64 { let s = S1 { r: r }; let x = s.r; return 7; }` hands
/// the parameter to a local struct, which owns it from there; a field moved
/// back out lands in another local and dies in this frame. That is the ONE
/// shape where the compiled backends keep the body inside the callee rather
/// than firing it after the call returns — measured across a six-cell method
/// matrix, where the other five (bare death, a rebind, a move into another
/// call, and both destructure spellings) all fire caller-side:
///
/// ```text
/// fn m1(ref self, r: R)        { .. }                     compiled: end / dR
/// fn m2(ref self, r: R)        { let m = r; .. }          compiled: end / dR
/// fn m3(ref self, r: R)        { let m = r; read(m); .. } compiled: end / dR
/// fn m4(ref self, r: R) -> i64 { let s = S1 { r: r };
///                                let x = s.r; return 7; } compiled: dR / v=7
/// ```
///
/// Distinct from [`fn_moves_param_into_outliving_place`], which asks about a
/// place that OUTLIVES the frame (`self`, a `ref` param). This one is about a
/// place that does not: the value still dies here, just under a local's
/// ownership rather than the argument temp's.
pub fn fn_moves_param_into_local_aggregate(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(name) = &param.pattern.kind else {
        return false;
    };
    /// Is the param handed to this aggregate literal, at any nesting depth?
    fn aggregate_takes(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::StructLiteral { fields, .. } => fields
                .iter()
                .any(|f| is_named(&f.value, name) || aggregate_takes(&f.value, name)),
            ExprKind::Tuple(elems) => elems
                .iter()
                .any(|el| is_named(el, name) || aggregate_takes(el, name)),
            _ => false,
        }
    }
    fn is_named(e: &Expr, name: &str) -> bool {
        matches!(&e.kind, ExprKind::Identifier(n) if n == name)
    }
    fn walk_block(b: &Block, name: &str) -> bool {
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                aggregate_takes(value, name)
            }
            StmtKind::Expr(e) => walk_expr(e, name),
            _ => false,
        }) || b.final_expr.as_deref().is_some_and(|e| walk_expr(e, name))
    }
    fn walk_expr(e: &Expr, name: &str) -> bool {
        match &e.kind {
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) => {
                walk_block(b, name)
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                walk_block(then_block, name)
                    || else_branch.as_deref().is_some_and(|e| walk_expr(e, name))
            }
            ExprKind::Match { arms, .. } => arms.iter().any(|a| walk_expr(&a.body, name)),
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => walk_block(body, name),
            _ => false,
        }
    }
    walk_block(&f.body, name)
}

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

    outliving_store::walk_block(&f.body, param_name, &roots)
}

/// B-2026-09-24-16 — does `f` move by-value parameter `arg_index` into a
/// container held by one of its own LOCALS, on every path?
///
/// `fn st(a: R) -> Vec[R] { let mut v: Vec[R] = Vec.new(); v.push(a); v }`
/// hands the parameter to `v`, whose drain runs its `Drop` body -- where `v`
/// dies in this frame, or wherever the returned `Vec` ends up. The caller's
/// own walk over the argument is then a second body for one value (`d1 k1 d1`
/// on all four surfaces). [`fn_moves_param_into_outliving_place`] answers the
/// same question for a container the CALLER already holds (`self`, a `ref`
/// parameter), and this is its sibling for one the callee creates: the caller
/// stands down in both, for the same reason.
///
/// A MUST-analysis, deliberately narrow, because this feeds only the
/// suppressing direction: a false `true` would lose a body on a path that did
/// not push. So the push has to be a TOP-LEVEL statement of the body, into a
/// binding a top-level `let` declared before it, and every statement ahead of
/// it has to be straight-line -- nothing that can `return`, `break` out or
/// `?`-propagate before the push is reached. Anything else answers `false`,
/// which keeps the pre-existing behaviour for that shape.
pub fn fn_moves_param_into_local_container(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return false;
    }
    let PatternKind::Binding(name) = &param.pattern.kind else {
        return false;
    };
    /// Can evaluating `e` leave the function, or skip what follows it?
    fn straight(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(..)
            | ExprKind::ByteLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::InterpolatedStringLit(..)
            | ExprKind::ByteStringLit(..)
            | ExprKind::Bool(..)
            | ExprKind::Identifier(..)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue => true,
            ExprKind::Binary { left, right, .. } => straight(left) && straight(right),
            ExprKind::Unary { operand, .. } => straight(operand),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                straight(object)
            }
            ExprKind::Call { callee, args } => {
                straight(callee) && args.iter().all(|a| straight(&a.value))
            }
            ExprKind::MethodCall { object, args, .. } => {
                straight(object) && args.iter().all(|a| straight(&a.value))
            }
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => elems.iter().all(straight),
            // B-2026-09-22-14 — `let mut v: Vec[T] = []` reaches here as this
            // prefix form, rewritten from the bare literal by its annotation.
            ExprKind::PrefixCollectionLiteral { items, .. } => items.iter().all(straight),
            ExprKind::StructLiteral { fields, .. } => fields.iter().all(|fi| straight(&fi.value)),
            _ => false,
        }
    }
    let mut locals: Vec<&str> = Vec::new();
    for st in &f.body.stmts {
        match &st.kind {
            StmtKind::Expr(e) => {
                if let ExprKind::MethodCall {
                    object,
                    method,
                    args,
                    ..
                } = &e.kind
                {
                    if matches!(
                        method.as_str(),
                        "push" | "push_back" | "push_front" | "insert"
                    ) && matches!(&object.kind, ExprKind::Identifier(v) if locals.contains(&v.as_str()))
                        && args
                            .iter()
                            .any(|a| outliving_store::is_bare(&a.value, name))
                    {
                        return args.iter().all(|a| straight(&a.value));
                    }
                }
                if !straight(e) {
                    return false;
                }
            }
            StmtKind::Let { pattern, value, .. } => {
                if !straight(value) {
                    return false;
                }
                if let PatternKind::Binding(n) = &pattern.kind {
                    if n == name {
                        return false;
                    }
                    locals.push(n.as_str());
                }
            }
            _ => return false,
        }
    }
    false
}

/// B-2026-09-25-10 — either conditional store: into a place the caller holds,
/// or into a container one of `f`'s own locals holds. Every site that registers
/// or honours the conditional-store flag asks this, so the two routes cannot be
/// admitted on one side of the handover and not the other.
pub fn fn_conditionally_stores_param(f: &Function, arg_index: usize) -> bool {
    fn_conditionally_moves_param_into_outliving_place(f, arg_index)
        || fn_conditionally_moves_param_into_local_container(f, arg_index)
}

/// B-2026-09-25-10 — the caller's side of the local-container handover: the
/// callee takes the value over on every path (the MUST predicate) or per path
/// under the conditional-store flag.
pub fn fn_moves_param_into_local_container_any(f: &Function, arg_index: usize) -> bool {
    fn_moves_param_into_local_container(f, arg_index)
        || fn_conditionally_moves_param_into_local_container(f, arg_index)
}

/// B-2026-09-25-10 — the CONDITIONAL sibling of
/// [`fn_moves_param_into_local_container`]: does `f` move by-value parameter
/// `arg_index` into a container one of its own locals holds on SOME path, but
/// not provably on every one?
///
/// `if c { v.push(a) }` hands the parameter to `v` on the path that pushes and
/// lets it die in this frame on the other. The MUST predicate answers `false`
/// for it, so the caller kept its walk while the container drained the same
/// value on the pushing path: the body ran twice for a struct and the element
/// memory was freed twice for a caller-retained `Array`. This is what the
/// conditional-STORE registration ([`fn_conditionally_moves_param_into_outliving_place`])
/// answers for a container the caller holds, restated for one the callee
/// creates, and it is consulted in the same places: the callee takes the value
/// under a per-path flag the storing statement clears, and the caller stands
/// down on exactly the same answer.
///
/// The roots are every name a `let` binds anywhere in the body, other than the
/// parameter itself. The store must be a direct statement of some block (the
/// flag is cleared in the storing statement's own basic block), and the value
/// must have no other way out, the same `escapes_by_unclearable_route` gate the
/// outliving sibling applies.
pub fn fn_conditionally_moves_param_into_local_container(f: &Function, arg_index: usize) -> bool {
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    if matches!(
        param.ty.kind,
        crate::ast::TypeKind::Ref(_) | crate::ast::TypeKind::MutRef(_)
    ) {
        return false;
    }
    let PatternKind::Binding(name) = &param.pattern.kind else {
        return false;
    };
    if fn_moves_param_into_local_container(f, arg_index) {
        return false;
    }
    fn locals_in_block<'a>(b: &'a Block, out: &mut Vec<&'a str>) {
        for st in &b.stmts {
            match &st.kind {
                StmtKind::Let { pattern, value, .. } => {
                    if let PatternKind::Binding(n) = &pattern.kind {
                        out.push(n.as_str());
                    }
                    locals_in_expr(value, out);
                }
                StmtKind::Expr(e) => locals_in_expr(e, out),
                _ => {}
            }
        }
        if let Some(fe) = b.final_expr.as_deref() {
            locals_in_expr(fe, out);
        }
    }
    fn locals_in_expr<'a>(e: &'a Expr, out: &mut Vec<&'a str>) {
        match &e.kind {
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => locals_in_block(b, out),
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
                locals_in_block(then_block, out);
                if let Some(x) = else_branch.as_deref() {
                    locals_in_expr(x, out);
                }
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    locals_in_expr(&a.body, out);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. }
            | ExprKind::LabeledBlock { body, .. } => locals_in_block(body, out),
            _ => {}
        }
    }
    let mut roots: Vec<&str> = Vec::new();
    locals_in_block(&f.body, &mut roots);
    roots.retain(|n| *n != name.as_str());
    if roots.is_empty() {
        return false;
    }
    // The SAME store shape the MUST twin recognises: a `push` / `insert` of the
    // bare param onto a container a let-bound local names. Not
    // `outliving_store::walk_block`, whose roots-relative answer also counts a
    // FIELD assignment into a local (`o.h = h`) — a struct field, not a
    // container, and one the caller keeps firing for, so claiming it here
    // stood the caller down and left the body with no owner.
    fn pushes(e: &Expr, name: &str, roots: &[&str]) -> bool {
        if let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &e.kind
        {
            if matches!(
                method.as_str(),
                "push" | "push_back" | "push_front" | "insert"
            ) && matches!(&object.kind, ExprKind::Identifier(v) if roots.contains(&v.as_str()))
                && args
                    .iter()
                    .any(|a| outliving_store::is_bare(&a.value, name))
            {
                return true;
            }
        }
        let mut hit = false;
        crate::rc_elide::walk_children_pub(&e.kind, &mut |c| {
            if !hit && pushes(c, name, roots) {
                hit = true;
            }
        });
        hit
    }
    let mut stores = false;
    for st in &f.body.stmts {
        crate::rc_elide::walk_stmt_children_pub(st, &mut |c| {
            if !stores && pushes(c, name, &roots) {
                stores = true;
            }
        });
    }
    if let Some(fe) = f.body.final_expr.as_deref() {
        stores = stores || pushes(fe, name, &roots);
    }
    stores && !outliving_store::escapes_by_unclearable_route(&f.body, name, &roots)
}

/// B-2026-08-30-28 — the MUST half of [`fn_moves_param_into_outliving_place`]:
/// does EVERY path through `f` store the parameter into a place that outlives
/// the call?
///
/// Its sibling is a MAY-analysis (`any` at every branch), which is the right
/// question for "may the caller stand down" and the wrong one for "who runs the
/// `Drop` body". A conditional store has two paths and needs two answers, and a
/// per-callee predicate that reports only "some path stores" cannot give them:
/// the callee registered no drop for the param at all, so on the path that did
/// NOT store, the value died with nobody to run its body.
///
/// BOTH ERROR DIRECTIONS ARE SAFE HERE, which is why the analysis can stay
/// simple:
///
///  * Answering `false` for a store that is really unconditional makes the
///    callee register the guarded body drop; the flag is then cleared at the
///    store, so the body still fires exactly once. A wasted alloca, not a bug.
///  * Answering `true` for a store that is really conditional declines the
///    registration and leaves TODAY's behaviour on that shape — the body lost
///    on the non-storing path. Not a regression, just an unfixed case.
///
/// Neither direction can double a body, because the registration is guarded and
/// the caller stands down either way. That is the whole reason this is allowed
/// to be a cheap syntactic walk instead of a dataflow pass.
///
/// A LOOP never counts: its body may execute zero times, so a store inside one
/// is conditional by construction.
pub fn fn_always_moves_param_into_outliving_place(f: &Function, arg_index: usize) -> bool {
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

    /// One expression: does taking it ALWAYS store `name`?
    fn always_stores(e: &Expr, name: &str, roots: &[&str]) -> bool {
        match &e.kind {
            // The store itself, executed unconditionally by whoever reaches it.
            ExprKind::MethodCall { object, args, .. } => {
                outliving_store::place_root_outlives(object, roots)
                    && args.iter().any(|a| outliving_store::moves(&a.value, name))
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) | ExprKind::Seq(b) => {
                always_walk_block(b, name, roots)
            }
            // A `par` block's branches all run; the question is unchanged.
            ExprKind::Par(b) => always_walk_block(b, name, roots),
            // Both sides must store, and a missing `else` is a path that does not.
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
                always_walk_block(then_block, name, roots)
                    && else_branch
                        .as_deref()
                        .is_some_and(|x| always_stores(x, name, roots))
            }
            ExprKind::Match { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|a| always_stores(&a.body, name, roots))
            }
            // A loop body may run zero times.
            _ => false,
        }
    }

    fn always_walk_block(b: &Block, name: &str, roots: &[&str]) -> bool {
        // Statements are sequential, so ONE that always stores makes the whole
        // block always store.
        b.stmts.iter().any(|st| match &st.kind {
            StmtKind::Expr(e) => always_stores(e, name, roots),
            StmtKind::Assign { target, value } => {
                outliving_store::place_root_outlives(target, roots)
                    && outliving_store::moves(value, name)
            }
            _ => false,
        }) || b
            .final_expr
            .as_deref()
            .is_some_and(|fe| always_stores(fe, name, roots))
    }

    always_walk_block(&f.body, param_name, &roots)
}

/// B-2026-08-30-28 — the param is stored into an outliving place on SOME path
/// but not on EVERY path.
///
/// This is the shape whose `Drop` body had no owner at all: the caller stands
/// down because [`fn_moves_param_into_outliving_place`] sees a store, and the
/// callee registered nothing because that same predicate said the value leaves.
/// On the path where the store does not happen, the value simply died.
///
/// Read by `compile_function`'s parameter loop (and the interpreter's twin) to
/// register a BODIES-ONLY drop guarded by B-2026-08-28-51's per-path flag —
/// exactly what `fn_conditionally_returns_param_bare` already does for the
/// conditionally RETURNED param, which is the same defect one escape route
/// over.
pub fn fn_conditionally_moves_param_into_outliving_place(f: &Function, arg_index: usize) -> bool {
    if !fn_moves_param_into_outliving_place(f, arg_index)
        || fn_always_moves_param_into_outliving_place(f, arg_index)
    {
        return false;
    }
    // The value must have NO other way out. See
    // `outliving_store::escapes_by_unclearable_route` for the three measured
    // doubles this declines, and why declining is the recoverable direction.
    let Some(param) = f.params.get(arg_index) else {
        return false;
    };
    let PatternKind::Binding(param_name) = &param.pattern.kind else {
        return false;
    };
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
    !outliving_store::escapes_by_unclearable_route(&f.body, param_name, &roots)
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

/// B-2026-09-17-34 — the struct FIELD NAMES a function moves out of the
/// whole-payload binding of a `match` / `if let` over by-value parameter
/// `param_name` (`fn eat(o: Option[Hd]) { match o { Some(t) => { let x = t.r; … } } }`
/// answers `["r"]`).
///
/// A CALLER question, and one the signature cannot answer. A by-value boxed
/// `Option`/`Result` payload's interior is registered by the CALLER
/// (`__optbox_arg_tmp{i}`, B-2026-08-05-7) and drains in the caller's frame
/// after the call returns, so when the callee's arm moves a field out — making
/// its local the owner of that field's body AND memory — the caller's interior
/// walk frees a buffer the callee has already freed. The caller has to mask
/// exactly those fields out of the walker it registers, and only the callee's
/// body says which they are.
///
/// TOP-LEVEL `let`s of the arm body only, deliberately. That is precisely the
/// set codegen's own move-out disarm fires on, so the two cannot disagree about
/// which fields moved — and a disagreement here is a double free in one
/// direction and a leak in the other. A move nested inside a further block or
/// branch is not reported, and the caller then keeps the unmasked walker it has
/// today.
///
/// UNDER-APPROXIMATES ON PURPOSE, the opposite direction from
/// [`param_rebound_into_local`]. That predicate stands the caller DOWN, where a
/// false positive costs a leak; this one narrows what the caller frees, so a
/// false positive is a LEAK of a field nobody owns while a false negative is
/// the double free we already have. Report a field only when the shape is
/// exactly the one codegen disarms.
pub fn param_payload_moved_out_fields(f: &Function, param_name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_payload_moved_fields_in_block(&f.body, param_name, &mut out);
    out
}

/// The whole-payload binding of `pattern`, when it is a single-field tuple
/// variant binding a bare name (`Some(t)` / `Ok(t)` / `Err(t)`). A destructure
/// or a wildcard binds no such name and answers `None` — the first has its own
/// per-part machinery, the second leaves every field the box's.
fn whole_payload_binding(pattern: &Pattern) -> Option<&str> {
    let PatternKind::TupleVariant { patterns, .. } = &pattern.kind else {
        return None;
    };
    let [sub] = patterns.as_slice() else {
        return None;
    };
    let PatternKind::Binding(name) = &sub.kind else {
        return None;
    };
    Some(name.as_str())
}

/// `let <x> = <binding>.<field>;` at the top level of `body`, collecting each
/// `<field>`. One hop only: a deeper chain (`let x = t.h.r`) names a leaf
/// inside a hop, which is a different mask and has its own path-shaped record.
fn collect_moved_fields_of_binding(body: &Expr, binding: &str, out: &mut Vec<String>) {
    let ExprKind::Block(block) = &body.kind else {
        return;
    };
    for stmt in &block.stmts {
        let StmtKind::Let { value, .. } = &stmt.kind else {
            continue;
        };
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            continue;
        };
        let ExprKind::Identifier(root) = &object.kind else {
            continue;
        };
        if root == binding && !out.iter().any(|f| f == field) {
            out.push(field.clone());
        }
    }
}

fn collect_payload_moved_fields_in_expr(e: &Expr, param_name: &str, out: &mut Vec<String>) {
    match &e.kind {
        ExprKind::Match { scrutinee, arms } => {
            if matches!(&scrutinee.kind, ExprKind::Identifier(n) if n == param_name) {
                for arm in arms {
                    if let Some(binding) = whole_payload_binding(&arm.pattern) {
                        collect_moved_fields_of_binding(&arm.body, binding, out);
                    }
                }
            }
            collect_payload_moved_fields_in_expr(scrutinee, param_name, out);
            for arm in arms {
                collect_payload_moved_fields_in_expr(&arm.body, param_name, out);
            }
        }
        ExprKind::IfLet {
            pattern,
            value,
            then_block,
            else_branch,
        } => {
            if matches!(&value.kind, ExprKind::Identifier(n) if n == param_name) {
                if let Some(binding) = whole_payload_binding(pattern) {
                    collect_moved_fields_in_block_of_binding(then_block, binding, out);
                }
            }
            collect_payload_moved_fields_in_block(then_block, param_name, out);
            if let Some(eb) = else_branch {
                collect_payload_moved_fields_in_expr(eb, param_name, out);
            }
        }
        ExprKind::Block(b) => collect_payload_moved_fields_in_block(b, param_name, out),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            collect_payload_moved_fields_in_block(then_block, param_name, out);
            if let Some(eb) = else_branch {
                collect_payload_moved_fields_in_expr(eb, param_name, out);
            }
        }
        _ => {}
    }
}

/// [`collect_moved_fields_of_binding`] over a [`Block`] rather than a block
/// EXPRESSION — the `if let` arm's body shape.
fn collect_moved_fields_in_block_of_binding(b: &Block, binding: &str, out: &mut Vec<String>) {
    for stmt in &b.stmts {
        let StmtKind::Let { value, .. } = &stmt.kind else {
            continue;
        };
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            continue;
        };
        let ExprKind::Identifier(root) = &object.kind else {
            continue;
        };
        if root == binding && !out.iter().any(|f| f == field) {
            out.push(field.clone());
        }
    }
}

fn collect_payload_moved_fields_in_block(b: &Block, param_name: &str, out: &mut Vec<String>) {
    for stmt in &b.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => {
                collect_payload_moved_fields_in_expr(value, param_name, out)
            }
            StmtKind::Expr(e) => collect_payload_moved_fields_in_expr(e, param_name, out),
            _ => {}
        }
    }
    if let Some(fe) = &b.final_expr {
        collect_payload_moved_fields_in_expr(fe, param_name, out);
    }
}
