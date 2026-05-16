// src/ast.rs

//! Abstract Syntax Tree definitions for the Kāra language.
//! Every node carries a `Span` for source location tracking.

use crate::token::{FloatSuffix, IntSuffix, Span};

/// Three-level visibility per `design.md § Three-level visibility`.
/// Items carry `is_pub: bool` and `is_private: bool`; this enum is the
/// single-value view used by the resolver / typechecker when enforcing
/// cross-module access rules (CR-24 slice 6). Exactly one of the two
/// bools may be true; both false means `Default` (project-internal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Marked `pub` — visible to end users and all project files.
    Pub,
    /// No visibility keyword — project-internal (visible to all files in
    /// the package, not to external consumers).
    Default,
    /// Marked `private` — visible only within the same directory.
    Private,
}

impl Visibility {
    /// Build a Visibility from the two transitional booleans. Callers that
    /// violate the "at most one true" invariant get `Pub` as the safe fallback
    /// — parser validation should have rejected the combination earlier.
    pub fn from_flags(is_pub: bool, is_private: bool) -> Self {
        if is_pub {
            Visibility::Pub
        } else if is_private {
            Visibility::Private
        } else {
            Visibility::Default
        }
    }

    pub fn is_pub(self) -> bool {
        matches!(self, Visibility::Pub)
    }

    pub fn is_private(self) -> bool {
        matches!(self, Visibility::Private)
    }
}

// ── Program ──────────────────────────────────────────────────────

/// Side-table populated by `lowering::lower_program` from the typechecker's
/// `TypeCheckResult.question_conversions`. Maps each `?` expression's span
/// (offset, length as a `(usize, usize)` tuple) to the fully-qualified name
/// of the target error type when a `From`-based conversion must run before
/// propagation. Used by codegen to emit `Target.from(e)` ahead of the early
/// return; see `src/codegen.rs:compile_question`.
pub type QuestionConversionTable = std::collections::HashMap<(usize, usize), String>;

/// Side-table populated by the cli pipeline from `EffectCheckResult`. Maps
/// each callable's canonical name (free fn `name`, assoc/method `Type.method`)
/// to whether its inferred or declared effects include any of the four
/// "side-effect-bearing" verbs — `reads`, `writes`, `sends`, `receives`.
/// Read by codegen at par-branch call sites: a callee marked `false` skips
/// the cooperative cancel-check atomic load; absent or `true` callees fall
/// back to the conservative "always fire" behavior. See design.md
/// § Effect-boundary cooperative cancellation.
pub type CalleeEffectfulTable = std::collections::HashMap<String, bool>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map. Maps each `MethodCall` expression's span to the
/// canonical `Type.method` callee key — the same shape used in
/// `CalleeEffectfulTable`. Codegen consults this table at method-call
/// sites in par branches so the cooperative cancel-check narrowing
/// applies to instance methods, not just free-function / `Type.assoc`
/// calls.
pub type MethodCalleeTypesTable = std::collections::HashMap<(usize, usize), String>;

/// Side-table populated by the lowering pass from the typechecker's
/// `pattern_binding_types` map. Maps each pattern-binding's span (offset,
/// length) to the canonical surface type name (e.g. `"MyError"`). Used by
/// codegen at match-arm bind sites: when binding a tuple-variant payload
/// to a name whose surface type is a struct, codegen reconstitutes the
/// struct value from the i64 payload word so subsequent `.field` access
/// dispatches through the right struct shape.
pub type PatternBindingTypesTable = std::collections::HashMap<(usize, usize), String>;

/// Sibling to `PatternBindingTypesTable`: maps each pattern-binding's span
/// `(offset, length)` to the inner element `TypeExpr` for `Vec[T]` /
/// `Slice[T]` bindings only. Populated by the lowering pass from the
/// typechecker's `pattern_binding_inner_types` map. Consumed by codegen at
/// `bind_pattern_values` to register `vec_elem_types` / `slice_elem_types`
/// under the binding's variable name, so direct method dispatch on a
/// pattern-bound collection payload (`xs.len()` / `xs[0]` / `xs.push(...)`)
/// routes through the right element-typed path. PB sibling slice
/// (2026-05-09).
pub type PatternBindingInnerTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

/// Borrow form for a pattern binding under a `ref` / `mut ref` scrutinee.
/// `Ref` corresponds to a `ref T` scrutinee mode; `MutRef` to `mut ref T`.
/// Owned bindings have no entry in `PatternBindingBorrowModesTable` —
/// presence-as-signal lets the codegen short-circuit in the common case.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PatternBindingBorrow {
    Ref,
    MutRef,
}

/// Per-pattern-binding borrow mode populated by the typechecker's
/// `check_pattern_against` walk and forwarded by the lowering pass for
/// codegen to consult. Codegen consumes this at every leaf binding site
/// (plain `Binding`, struct shorthand fields, slice rest bindings,
/// `@`-bindings) to wrap the binding in a "ref shim" — an extra alloca
/// holding a pointer to the value alloca, registered in `ref_params` —
/// so call sites that take a `ref T` / `mut ref T` parameter receive
/// the right ABI shape rather than the raw value. Mirrors the
/// typechecker's `ScrutineeMode::wrap_binding_ty` rule for the codegen
/// surface — design.md § Match Arm Binding Modes.
pub type PatternBindingBorrowModesTable =
    std::collections::HashMap<(usize, usize), PatternBindingBorrow>;

#[derive(Debug, Clone, Default)]
pub struct Program {
    pub items: Vec<Item>,
    /// Joined `//!` doc-comment text at the top of the source file.
    /// Lines from a single run of `//!` are concatenated with `\n`.
    /// `None` when the file has no leading `//!` lines.
    pub module_doc_comment: Option<String>,
    /// Set by the lowering pass; empty before lowering runs.
    pub question_conversions: QuestionConversionTable,
    /// Set by the cli pipeline after effectcheck; empty otherwise.
    pub callee_effectful: CalleeEffectfulTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`; empty otherwise.
    pub method_callee_types: MethodCalleeTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.pattern_binding_types`.
    pub pattern_binding_types: PatternBindingTypesTable,
    /// Set by the lowering pass from `TypeCheckResult.pattern_binding_inner_types`.
    /// PB sibling slice (2026-05-09).
    pub pattern_binding_inner_types: PatternBindingInnerTypesTable,
    /// Set by the lowering pass from
    /// `TypeCheckResult.pattern_binding_borrow_modes`. Consumed by codegen
    /// to apply the ref-binding shim at match-arm leaf bindings under a
    /// `ref` / `mut ref` scrutinee. Empty entries mean owned bindings.
    pub pattern_binding_borrow_modes: PatternBindingBorrowModesTable,
}

// ── Top-level Items ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Item {
    Function(Function),
    StructDef(StructDef),
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
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub params: Vec<Param>,
    pub self_param: Option<SelfParam>,
    pub return_type: Option<TypeExpr>,
    pub effects: Option<EffectList>,
    pub requires: Vec<Expr>,
    pub ensures: Vec<EnsuresClause>,
    pub where_clause: Option<WhereClause>,
    pub body: Block,
    /// True iff this item came from baked stdlib source (CR-202 slice 3b).
    /// The parser writes `false`; the bake step in `prelude.rs` flips it
    /// to `true` after parsing. The resolver's `#[compiler_builtin]` gate
    /// (`E0237`) bypasses items where this is `true` so stdlib source can
    /// use the attribute even when the resolver's session-wide
    /// `is_stdlib_source` flag is unset (e.g. when the bake AST is
    /// spliced into a user-mode program tree).
    pub stdlib_origin: bool,
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
impl_vis!(EnumDef);
impl_vis!(TraitDef);
impl_vis!(ConstDecl);
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
    pub no_rc: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub where_clause: Option<WhereClause>,
    pub fields: Vec<StructField>,
    pub invariants: Vec<Expr>,
    /// See [`Function::stdlib_origin`]. CR-202 slice 3b.
    pub stdlib_origin: bool,
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
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub where_clause: Option<WhereClause>,
    pub variants: Vec<Variant>,
    /// See [`Function::stdlib_origin`]. CR-202 slice 3b.
    pub stdlib_origin: bool,
}

#[derive(Debug, Clone)]
pub struct Variant {
    pub span: Span,
    /// Joined contents of `///` doc comments preceding the variant.
    /// `None` when no doc comments were attached. CommonMark.
    pub doc_comment: Option<String>,
    pub name: String,
    pub kind: VariantKind,
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
}

#[derive(Debug, Clone)]
pub enum TraitItem {
    Method(Box<TraitMethod>),
    AssocType(AssocTypeDecl),
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
}

#[derive(Debug, Clone)]
pub struct AssocTypeDecl {
    pub span: Span,
    pub name: String,
    pub bounds: Vec<TraitBound>,
}

#[derive(Debug, Clone)]
pub struct TraitMethod {
    pub span: Span,
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
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub effects: Option<EffectList>,
    pub requires: Vec<Expr>,
    pub ensures: Vec<EnsuresClause>,
    pub where_clause: Option<WhereClause>,
    pub body: Option<Block>,
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
}

#[derive(Debug, Clone)]
pub enum ImplItem {
    Method(Box<Function>),
    AssocType(AssocTypeBinding),
}

#[derive(Debug, Clone)]
pub struct AssocTypeBinding {
    pub span: Span,
    pub name: String,
    pub ty: TypeExpr,
}

// ── Effect Declarations ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct EffectResourceDecl {
    pub span: Span,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub provider_trait: Option<String>,
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
    /// Joined contents of `///` doc comments preceding the const.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub ty: TypeExpr,
    pub value: Expr,
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
    /// Joined contents of `///` doc comments preceding the alias.
    pub doc_comment: Option<String>,
    pub is_pub: bool,
    pub is_private: bool,
    pub name: String,
    pub generic_params: Option<GenericParams>,
    pub ty: TypeExpr,
    pub refinement: Option<Expr>,
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
}

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

// ── Expressions ──────────────────────────────────────────────────

/// A part of a parsed f-string — static text or a fully-parsed expression.
/// Replaces `token::InterpolationPart::Expr(raw_string)` after the parser
/// sub-parses each interpolation hole at parse time.
#[derive(Debug, Clone)]
pub enum ParsedInterpolationPart {
    Text(String),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    // Literals
    Integer(i64, Option<IntSuffix>),
    Float(f64, Option<FloatSuffix>),
    CharLit(char),
    StringLit(String),
    MultiStringLit(String),
    InterpolatedStringLit(Vec<ParsedInterpolationPart>),
    Bool(bool),

    // Identifiers
    Identifier(String),
    Path {
        segments: Vec<String>,
        /// Mixed type / const generic arguments at the expression position.
        /// Const generics slice 1b (2026-05-11) widened this from
        /// `Vec<TypeExpr>` to `Vec<GenericArg>` so call-site expressions
        /// like `make_arr[i64, 4]()` carry the `4` literal through to the
        /// codegen mango key.
        generic_args: Option<Vec<GenericArg>>,
    },
    SelfValue,
    SelfType,

    // Operators
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },

    // Postfix
    Question(Box<Expr>),
    OptionalChain {
        object: Box<Expr>,
        field_or_method: String,
        args: Option<Vec<CallArg>>, // None for field, Some for method
    },

    // Infix
    NilCoalesce {
        left: Box<Expr>,
        right: Box<Expr>,
    },

    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
    },
    MethodCall {
        object: Box<Expr>,
        method: String,
        turbofish: Option<Vec<TypeExpr>>,
        args: Vec<CallArg>,
    },
    FieldAccess {
        object: Box<Expr>,
        field: String,
    },
    TupleIndex {
        object: Box<Expr>,
        index: u64,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },

    // Compound expressions
    Block(Block),
    If {
        condition: Box<Expr>,
        then_block: Block,
        else_branch: Option<Box<Expr>>,
    },
    IfLet {
        pattern: Pattern,
        value: Box<Expr>,
        then_block: Block,
        else_branch: Option<Box<Expr>>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    While {
        label: Option<String>,
        condition: Box<Expr>,
        body: Block,
    },
    WhileLet {
        label: Option<String>,
        pattern: Pattern,
        value: Box<Expr>,
        body: Block,
    },
    For {
        label: Option<String>,
        pattern: Pattern,
        iterable: Box<Expr>,
        body: Block,
    },
    Loop {
        label: Option<String>,
        body: Block,
    },
    /// Labeled block expression — `label: { ... }` (design.md § Loops >
    /// Labeled blocks; syntax.md §5.3). The block becomes a `break` target
    /// (with optional value); `continue label` referring to a labeled block
    /// is rejected by the resolver. The block's type is the LUB of all
    /// reachable `break label expr` value sites and the tail expression.
    /// Unlabeled blocks continue to use `ExprKind::Block` — the
    /// `LabeledBlock` variant is added rather than mutating `Block` so
    /// existing AST consumers (which heavily destructure `Block`) keep
    /// working unchanged.
    LabeledBlock {
        label: String,
        /// Source span of the label identifier (the `IDENT` before the
        /// colon). Threaded through for diagnostic span fidelity —
        /// `error[E_CONTINUE_LABEL_BLOCK]` points its secondary span at
        /// the label binding using this.
        label_span: Span,
        body: Block,
    },
    Closure {
        params: Vec<ClosureParam>,
        /// Explicit per-closure borrow-mode override (Rule 2½).
        /// `None` = bare `|...|` (captures by ownership; default).
        /// `Some(Ref)` / `Some(MutRef)` = explicit prefix (`ref |...|` / `mut ref |...|`).
        capture_mode: Option<CaptureMode>,
        /// Span of the explicit prefix tokens (`mut ref` / `ref` / `own` /
        /// `move`) when present. `None` for bare `|...|` closures. Lets
        /// diagnostics target the prefix region precisely — used by N0507
        /// (UnusedMutCaptureNote) to attach a machine-applicable
        /// `mut ref` → `ref` rewrite without disturbing the closure body.
        prefix_span: Option<Span>,
        body: Box<Expr>,
    },
    Return(Option<Box<Expr>>),
    Break {
        label: Option<String>,
        value: Option<Box<Expr>>,
    },
    Continue {
        label: Option<String>,
    },

    // Composite literals
    Tuple(Vec<Expr>),
    ArrayLiteral(Vec<Expr>),
    /// `TypeName[e1, e2, ...]` — prefix collection literal.
    /// `type_name` is one of `Vec`, `Array`, `Set`, `Map`.
    /// `Array[e1, e2, e3]` produces a fixed-size array; `Vec[...]` produces a growable vec.
    PrefixCollectionLiteral {
        type_name: String,
        items: Vec<Expr>,
    },
    /// `[value; count]` (bare) or `Vec[value; count]` / `Array[value; count]`
    /// (prefix). Equivalent to a literal with `count` copies of `value`. Bare
    /// form defaults to `Vec[T]` in synthesis mode and coerces to `Array[T, N]`
    /// in check mode against an Array-typed expected. `Array[v; n]` requires
    /// `count` to be a compile-time integer literal. Restricted to `Vec` /
    /// `Array` only; repeating into `Set` / `Map` is rejected.
    RepeatLiteral {
        /// `None` → bare `[v; n]`; `Some("Vec")` / `Some("Array")` → prefix form.
        type_name: Option<String>,
        value: Box<Expr>,
        count: Box<Expr>,
    },
    MapLiteral(Vec<(Expr, Expr)>),
    StructLiteral {
        path: Vec<String>,
        fields: Vec<FieldInit>,
        spread: Option<Box<Expr>>,
    },

    // Pipe
    Pipe {
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `_` placeholder in pipe argument position — replaced by left-hand value during desugaring
    PipePlaceholder,

    // Cast
    Cast {
        expr: Box<Expr>,
        ty: TypeExpr,
    },

    /// `offset_of[T](field.path)` — compile-time byte offset of a field
    /// (or nested field path) from the start of a value of type `T`.
    /// Parser special form because the second argument is a field-name
    /// path, not a value expression. The typechecker walks `field_path`
    /// against `T`'s declared fields, validating each segment and
    /// emitting `E_OFFSET_OF_OPAQUE_TYPE` / `E_OFFSET_OF_GENERIC_PARAM`
    /// / `E_OFFSET_OF_UNKNOWN_FIELD` / `E_OFFSET_OF_PRIVATE_FIELD` /
    /// `E_OFFSET_OF_ENUM_VARIANT` as appropriate. The codegen lowers
    /// to inkwell's `TargetData::offset_of_element` (chained for
    /// nested paths). Returns `usize`. See `design.md § Field Offsets`.
    OffsetOf {
        ty: TypeExpr,
        field_path: Vec<String>,
    },

    // Range — start and/or end may be absent for half-open forms.
    // `a..b`   → start=Some, end=Some, inclusive=false  → Range[T]
    // `a..=b`  → start=Some, end=Some, inclusive=true   → RangeInclusive[T]
    // `a..`    → start=Some, end=None, inclusive=false  → RangeFrom[T]
    // `..b`    → start=None, end=Some, inclusive=false  → RangeTo[T]
    // `..=b`   → start=None, end=Some, inclusive=true   → RangeToInclusive[T]
    // `..`     → start=None, end=None, inclusive=false  → RangeFull
    Range {
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },

    // Unsafe
    Unsafe(Block),

    /// `try { ... }` — try block. The body may use `?` to short-circuit
    /// out of the block; the block itself produces a `Result`-shaped
    /// value. Parsed at v1; the typechecker pipeline (?-retargeting
    /// against the block, error-type unification, From-chain coercion)
    /// lands in P1. See design.md § Error Handling > Try Blocks.
    Try(Block),

    // Sequential block (suppresses auto-parallelism)
    Seq(Block),

    // Parallel block (explicit fork-join)
    Par(Block),

    // Lock block
    Lock {
        mutex: String,
        alias: Option<String>,
        body: Block,
    },

    // `providers { R => p, ... } in { body }` — multi-provider bootstrapping
    // (design.md § `providers { } in { }` Block).
    Providers {
        bindings: Vec<ProviderBinding>,
        body: Block,
    },

    // Error recovery placeholder
    Error,
}

#[derive(Debug, Clone)]
pub struct ProviderBinding {
    pub resource: String,
    pub resource_span: Span,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // Comparison
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    // Logical
    And,
    Or,
    // Bitwise
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    // Range
    Range,
    RangeInclusive,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnaryOp {
    Neg,    // -
    Not,    // !
    BitNot, // ~
    Deref,  // *
}

// ── Closures ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ClosureParam {
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

/// Discriminator for active label-stack entries — distinguishes labeled
/// loops (which accept both `break label` and `continue label`) from
/// labeled blocks (which accept `break label` only). Carried alongside
/// the label name in the parser's and resolver's label stacks; the
/// resolver consults this when validating `continue label` targets.
/// See design.md § Loops > "Labeled blocks".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    Loop,
    Block,
}

/// Explicit closure capture-mode prefix (design.md § Closure Behavior, Rule 2½).
/// Bare `|...|` (no prefix) runs per-capture-path inference; the three variants
/// here pin every captured path to the declared mode. `Own` is Kāra's spelling
/// of capture-by-value; the Rust idiom `move` is rejected with a redirect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Own,
    Ref,
    MutRef,
}

// ── Call Arguments ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CallArg {
    pub label: Option<String>,
    /// Call-site mutation marker (`mut <expr>`). Required for fresh bindings
    /// passed to `mut ref T` / `mut Slice[T]` parameters; rejected elsewhere.
    /// See design.md Feature 4 Part 1½: Call-site Mutation Markers.
    pub mut_marker: bool,
    pub value: Expr,
    pub span: Span,
}

// ── Struct Literal Fields ────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub value: Expr,
    pub shorthand: bool, // true for `Point { x }` (name == value identifier)
    pub span: Span,
}

mod patterns;
mod stmts;
pub use patterns::*;
pub use stmts::*;
