// src/ast.rs

//! Abstract Syntax Tree definitions for the Kāra language.
//! Every node carries a `Span` for source location tracking.

use crate::token::Span;

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

/// Side-table populated by the cli pipeline from `EffectCheckResult`. Maps
/// each callable's canonical name to whether its effect set carries a
/// `sends(Network)` or `receives(Network)` verb-resource pair — the only
/// effects that route through the network event loop's non-blocking
/// park-and-yield path at v1. Other suspending effects (`Receiver.recv` via
/// `suspends`, custom user `suspends`, future channel waits) stay
/// thread-blocking and are NOT marked. Consumed by the state-machine
/// transform codegen (phase 6 line 26) to identify which functions need
/// the transform, and by codegen at network-effect call sites (phase 6
/// line 17 sub-item 6) to identify which call boundaries lower to "register
/// fd + park + yield" instead of a synchronous call. `Polymorphic` and
/// `PolymorphicWithFixed` declared-effect callees are conservatively marked
/// `true` because a monomorphization may bind their effect parameter to a
/// network-bearing effect; the transform itself reads the resolved
/// monomorphized effect set when deciding to apply.
pub type CalleeNetworkYieldEffectTable = std::collections::HashMap<String, bool>;

/// One yield-point entry within a network-boundary function: the call site
/// where execution suspends pending I/O readiness, paired with the
/// resolved callee key (`Identifier(name) → name`, two-segment
/// `Type.method` Path → joined, `MethodCall` resolved via
/// `TypeCheckResult.method_callee_types`). The state-machine transform
/// (phase 6 line 26) consumes the per-function vector to size the state
/// struct (one tag per yield point), and the codegen lowering pass
/// consumes the per-yield-point callee key to identify which network
/// runtime FFI helper to call at each yield site.
#[derive(Debug, Clone)]
pub struct YieldPoint {
    /// Resolved callee key — same shape as `CalleeNetworkYieldEffectTable`
    /// keys (`name`, `Type.method`). The state-machine transform looks
    /// this up to determine the parking convention at the call boundary.
    pub callee: String,
    /// Span of the call expression (the `MethodCall` or `Call` node, not
    /// the callee identifier). Used to thread debugger metadata through
    /// the state-machine transform — `WaitTarget.NetworkIo` per the
    /// debugger contract carries this span so `list_tasks()` can show
    /// the source-level yield site, identical to a thread-blocking
    /// syscall's stack frame.
    pub span: Span,
    /// V1 conservative over-approximation of the locals that the
    /// state-machine transform must preserve across this suspension —
    /// every binding lexically in scope at the yield site (function
    /// parameters + every `let` / `let-else` / `for`-loop / pattern
    /// binding introduced earlier in source order that hasn't gone out
    /// of scope). Names are listed in introduction order — params first
    /// (left-to-right), then per-block let-binding sequence. The
    /// captures-union packed-across-non-overlapping-live-ranges
    /// optimization (per design.md § State-Machine Transform) is a later
    /// refinement; v1 codegen packs every entry unconditionally.
    /// Closures are NOT descended into during the walk — a yield point
    /// inside a closure body is the closure's own state machine, not
    /// the enclosing function's. Empty when slice 3 hasn't run (e.g.
    /// before phase 6 line 26 slice 3's pipeline pass).
    pub captured_locals: Vec<String>,
}

/// Side-table populated by the cli pipeline after `EffectCheckResult` and
/// `CalleeNetworkYieldEffectTable` are available. Maps each
/// network-boundary function's canonical name to the ordered list of
/// yield points within its body (in source-traversal order). Functions
/// without any yield-point calls — even network-boundary ones reaching the
/// classification through their own emitted `sends(Network)` /
/// `receives(Network)` effect at the FFI primitive layer rather than via
/// a sub-call — have no entry. Consumed by:
///   - the state-machine transform codegen (phase 6 line 26) — one state
///     per entry sizes the function's poll-function switch arm count;
///   - the live-range pass that computes the captured-locals union per
///     yield point — needs the yield-point spans to define the
///     suspension-boundary set.
pub type YieldPointsTable = std::collections::HashMap<String, Vec<YieldPoint>>;

/// One field in a network-boundary function's state struct: a binding
/// from the function body (parameter, `let`, pattern binding) that the
/// state-machine transform must preserve across at least one of the
/// function's yield points. Slice 4 of phase 6 line 26: the v1 layout
/// is the union of every yield point's captured-locals set, in
/// source-introduction order, with no overlap optimization.
#[derive(Debug, Clone)]
pub struct StateStructField {
    /// Source-level binding name (matches the names in
    /// `YieldPoint.captured_locals`).
    pub name: String,
    /// Surface type name as recorded by the typechecker's
    /// `pattern_binding_types` map at this binding's pattern span.
    /// `None` when the typechecker did not record a name there: at v1
    /// this covers primitive-typed bindings (`i64`/`bool`/`u8`/...),
    /// anonymous-tuple shapes the recorder skips, and bindings whose
    /// pattern span was not threaded into `pattern_binding_types`
    /// (e.g. `let-uninit` and slice-pattern rest bindings — neither
    /// passes through `bind_pattern_types`). Codegen consults this
    /// name plus the sibling `pattern_binding_inner_types` table to
    /// materialize the LLVM shape; `None` entries fall through to the
    /// existing primitive-sizing path.
    pub type_name: Option<String>,
}

/// State-struct layout synthesized per network-boundary function. The
/// `fields` list is the union of every yield point's captured-locals
/// set within the function body, in source-introduction order
/// (parameters first left-to-right, then per-block let-binding sequence;
/// the first occurrence of a name across yield points fixes its
/// position). Slice 4 of phase 6 line 26 produces this conservative
/// over-approximation layout; a later slice may refine to per-yield
/// non-overlapping live ranges per design.md § State-Machine Transform.
#[derive(Debug, Clone)]
pub struct StateStructLayout {
    pub fields: Vec<StateStructField>,
}

/// Side-table populated by the cli pipeline after `Program.yield_points`
/// is built. Maps each network-boundary function with at least one
/// concrete yield point in its body to a `StateStructLayout`. Functions
/// classified network-boundary by Polymorphic declared-effect candidacy
/// without any actual sub-call yield points (FFI-primitive-emitting
/// shape) have no entry — matches `YieldPointsTable`'s presence rule.
/// Consumed by the state-machine transform codegen to size and lower
/// the state struct one-per-function-instantiation.
pub type StateStructLayoutTable = std::collections::HashMap<String, StateStructLayout>;

/// Side-table populated by the lowering pass from the typechecker's
/// `expr_types` map. Maps each `MethodCall` expression's span to the
/// canonical `Type.method` callee key — the same shape used in
/// `CalleeEffectfulTable`. Codegen consults this table at method-call
/// sites in par branches so the cooperative cancel-check narrowing
/// applies to instance methods, not just free-function / `Type.assoc`
/// calls.
pub type MethodCalleeTypesTable = std::collections::HashMap<(usize, usize), String>;

/// Side-table populated by the lowering pass from the typechecker's
/// `method_unwrap_inner_types` map. Maps each `unwrap`/`expect`/`is_*`
/// `MethodCall` expression's span to the inner `T` (for `Option[T]`) or
/// success-`T` (for `Result[T, E]`) `TypeExpr`. Codegen consults this
/// table in the `compile_method_call` arm for those methods to know
/// the LLVM shape of the value to reconstitute from the Option/Result
/// payload words.
pub type MethodUnwrapInnerTypesTable = std::collections::HashMap<(usize, usize), TypeExpr>;

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
    /// Set by the cli pipeline after effectcheck; empty otherwise. Identifies
    /// callees that route through the network event loop's park-and-yield
    /// path (i.e., carry `sends(Network)` / `receives(Network)`). Foundation
    /// for the state-machine transform (phase 6 line 26) and codegen
    /// lowering at yield points (phase 6 line 17 sub-item 6).
    pub callee_network_yield_effect: CalleeNetworkYieldEffectTable,
    /// Set by the cli pipeline after `callee_network_yield_effect` is
    /// populated; empty otherwise. For each network-boundary function (one
    /// where `callee_network_yield_effect.get(name) == Some(&true)`),
    /// lists the call sites whose callee is itself in
    /// `callee_network_yield_effect`. These are the suspension points the
    /// state-machine transform codegen lowers to "register fd + park +
    /// yield" code (phase 6 line 17 sub-item 6); their count drives the
    /// state struct's tag arity (phase 6 line 26).
    pub yield_points: YieldPointsTable,
    /// Set by the cli pipeline after `yield_points` is populated; empty
    /// otherwise. For each network-boundary function with at least one
    /// concrete yield point, the per-function state-struct layout (union
    /// of captured-locals across yield points, in source-introduction
    /// order, paired with their typechecker-recorded surface type
    /// names where available). Drives the state-machine transform's
    /// poll-function state-struct shape (phase 6 line 26).
    pub state_struct_layouts: StateStructLayoutTable,
    /// Set by the lowering pass from `TypeCheckResult.expr_types`; empty otherwise.
    pub method_callee_types: MethodCalleeTypesTable,
    /// Set by the lowering pass from
    /// `TypeCheckResult.method_unwrap_inner_types`; empty otherwise.
    pub method_unwrap_inner_types: MethodUnwrapInnerTypesTable,
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

mod exprs;
mod items;
mod patterns;
mod stmts;
mod types;
pub use exprs::*;
pub use items::*;
pub use patterns::*;
pub use stmts::*;
pub use types::*;
