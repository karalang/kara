//! Bounds-check and overflow-check elision state.
//!
//! The analysis caches behind the two "can this check be skipped?" questions
//! codegen asks per index and per arithmetic op: the BCE fact stack and its
//! length-pin / descending / converging / interprocedural skip tables
//! (`bce_length_pin.rs`, `bce_interproc.rs`, `control_flow_bce.rs`), the
//! binary-search guard machinery, and the overflow-check elision arming flags
//! (`accum_overflow.rs`). Extracted from `Codegen` as cluster 6 of the
//! state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.
//!
//! Every field here is a *cache of a proof*, not a fact about the program
//! being compiled, and the caches are one-directional: losing one costs a
//! redundant runtime check, never correctness. Only a *wrong* entry is
//! dangerous. That asymmetry is what makes the cluster cheap to reason
//! about — but it is emphatically not a uniform lifetime, and the four
//! that live here must not be confused:
//!
//! - **Lexical scope** — `asserted_index_bounds` and `binsearch_guard_stack`
//!   are push/pop stacks mirroring the dominating guard's source scope;
//!   `len_alias` is snapshot/restored across shadowing (`shadow.rs`).
//! - **Per function** — the length-pin and descending/converging skip
//!   tables, rebuilt at `compile_function` (`functions.rs`).
//! - **Per module** — `interproc_conv_skips` (the interprocedural
//!   precondition pass runs once over the whole module) and
//!   `binsearch_assume_emitted`, which is sticky-on and gates one extra
//!   `default<O1>` pipeline run for the module.
//! - **Process** — `elide_proven_index_add_overflow`, an env kill switch
//!   read once in the constructor and never written again.
//!
//! Resetting a per-module or process field per function would silently
//! disarm the optimization it guards, so a future "clear the BCE state"
//! helper must take the scope it is clearing, not clear the struct.

use std::collections::HashMap;

use super::bce_length_pin;
use super::state::AssertedIndexBound;

/// Bounds-check / overflow-check elision caches. No `'ctx` lifetime: every
/// field is plain analysis data keyed by source-level names and `SpanKey`s,
/// with no LLVM value in the cluster.
pub(crate) struct BceState {
    /// Local bindings that alias `vec_var.len()` — populated at let-sites of
    /// the form `let n = v.len()` where `v` is a Vec identifier in scope.
    /// Consulted by the bounds-check-elision pass when parsing while-guard
    /// predicates of form `idx < n`: resolving `n` back to `v.len()` lets
    /// the elision recognize `idx < v.len()` and skip the upper-half of
    /// `compile_vec_index`'s bounds check on a matching `v[idx]` site.
    /// Cleared / replaced as bindings shadow; the simple HashMap shape is
    /// load-bearing because tracked Vec names don't shadow each other in
    /// practice — refine to scope-keyed if a counter-example surfaces.
    pub(crate) len_alias: HashMap<String, String>,
    /// Asserted bounds in the current emission scope — facts established
    /// by a dominating `while`-guard or `for`-range that the bounds-check
    /// emission can rely on. Each entry asserts one half of a Vec-index
    /// safety fact; `compile_vec_index` consults this stack at the
    /// indexing site and elides the matching half of the bounds check.
    /// The stack discipline (push on body-entry, pop on body-exit) maps
    /// directly onto the source-level lexical scope of the guard.
    pub(crate) asserted_index_bounds: Vec<AssertedIndexBound>,
    /// Vec-length pins for the current function (bce_length_pin.rs): each maps a
    /// fill loop's key `SpanKey` (the `while` condition, or the `for`-range end)
    /// to the `(bound, vec_var)` fact that the counted fill establishes
    /// (`vec_var.len() >= bound`, both invariant from the fill loop onward).
    /// Populated at `compile_function`; a pin is moved into `vec_len_pins` once
    /// its fill loop finishes emitting, so it goes live exactly for the code
    /// lexically after the fill loop.
    pub(crate) pending_vec_len_pins:
        HashMap<crate::resolver::SpanKey, bce_length_pin::VecLengthPin>,
    /// Active length pins: `(bound, vec_var)` pairs. `bound` is a normalised
    /// pure-arithmetic `BoundTerm` (a bare var like `cols`, or `cols + 1`, …).
    /// Consulted by `resolve_len_origin`: a `while idx < BOUND` guard whose RHS
    /// normalises to a pinned `bound` resolves back to `vec_var` and asserts
    /// `idx < vec_var.len()` — the rolling-DP `dp[c]` / `dp[c - 1]` bounds-check
    /// elision (kata #62). A `Vec` (not a map) because the key is a `BoundTerm`
    /// and there are only a handful of pins per function.
    pub(crate) vec_len_pins: Vec<(bce_length_pin::BoundTerm, String)>,
    /// Descending-loop bounds-check skips for the current function
    /// (bce_length_pin.rs, B-2026-07-17-1): each maps an inner descending
    /// loop's condition `SpanKey` to the `(idx_var, vec_vars)` whose upper
    /// bound check that loop's body may skip (`idx_var < vec_var.len()` proven
    /// transitively via a length pin + the enclosing counter's bound). Consumed
    /// in `compile_while`, which pushes the matching `UpperBound` facts onto
    /// `asserted_index_bounds` for the inner loop body. Populated at
    /// `compile_function` (whole-body analysis, so it is stable across the
    /// function).
    pub(crate) descending_skips: HashMap<crate::resolver::SpanKey, bce_length_pin::DescendingSkip>,
    /// Converging two-pointer bounds-check skips for the current function
    /// (bce_length_pin.rs, B-2026-08-04-8): each maps an inner converging
    /// loop's condition `SpanKey` to the `(base_var, idx_vars, vec_vars)`
    /// whose SUM-index upper check that loop's body may skip
    /// (`base + idx < vec.len()` proven from a length pin, the enclosing
    /// counter's bound, and the guard that bounds both converging indices by
    /// `hi`'s init). Consumed in `compile_while`, which pushes the matching
    /// `UpperBoundSum` facts. Populated at `compile_function`.
    pub(crate) converging_skips: HashMap<crate::resolver::SpanKey, bce_length_pin::ConvergingSkip>,
    /// Converging skips a free function earns from an interprocedural bounds
    /// PRECONDITION every one of its call sites discharges (`bce_interproc.rs`,
    /// B-2026-08-05-6): the row-helper shape, where the length pin, the
    /// enclosing counter and the linear base all sit in the caller. Keyed by
    /// function name, then by the same inner-loop condition `SpanKey` the
    /// intra-function map uses — the records are identical, so `compile_while`
    /// cannot tell the two provenances apart and needed no change. Computed
    /// once per program in `compile_program`, merged into `converging_skips` at
    /// `compile_function`.
    pub(crate) interproc_conv_skips:
        HashMap<String, HashMap<crate::resolver::SpanKey, bce_length_pin::ConvergingSkip>>,
    /// Stack of `(lo, hi)` variable-name pairs from dominating strict
    /// `while lo < hi` guards (innermost last). When a `let mid = lo +
    /// (hi - lo) / 2` (or `(lo + hi) / 2`) binding is compiled under such
    /// a guard, codegen emits `assume(mid >= lo)` + `assume(mid < hi)` —
    /// the relational midpoint facts that let LLVM fold the `nums[mid]`
    /// bounds check (which interval-based CVP/LVI cannot, because the
    /// `mid = extractvalue(sadd.with.overflow …)` value is opaque to its
    /// range analysis). Both facts are LOCALLY sound from the midpoint
    /// form + the dominating `lo < hi` (so `hi - lo >= 1`): `(hi-lo)/2`
    /// lands in `[0, hi-lo-1]`, hence `lo <= mid <= hi-1 < hi`. Emitted at
    /// the binding site, where `lo`/`hi` still hold the values `mid` was
    /// derived from, so later mutation of `lo`/`hi` cannot invalidate them.
    /// See `docs/investigations/bce_monotonic_assume.md` § midpoint idiom.
    pub(crate) binsearch_guard_stack: Vec<(String, String)>,
    /// Set when `try_emit_binsearch_midpoint_assumes` emits at least one
    /// midpoint `llvm.assume`. CVP only consumes these once the bounds
    /// check and the assume are co-resident post-inline, which the first
    /// `default<Ox>` run doesn't achieve in one shot (the callee is
    /// optimized, then inlined; the fold needs CVP to re-run over the
    /// inlined-and-simplified form). When set, the driver runs ONE extra
    /// `default<O1>` pass to complete the fold — gated, so modules with no
    /// binary search pay nothing. Validated in `opt`: a second pipeline
    /// run folds the otherwise-surviving `mid < len` check (3 → 0).
    pub(crate) binsearch_assume_emitted: bool,
    /// Assignment statements whose `acc = acc + 1` RHS may skip its overflow
    /// check, keyed by the STATEMENT's span (B-2026-07-26-1). Populated per
    /// block by `accum_overflow::check_free_accumulator_sites`, which only
    /// admits sites where the trap is provably dead — see that module for the
    /// bound proof. Empty for essentially every block, so the lookup is a
    /// hash miss on the common path.
    pub(crate) check_free_accum_sites: std::collections::HashSet<crate::resolver::SpanKey>,
    /// One-shot latch consumed by the `BinOp::Add` arm in `compile_binop_typed`
    /// to emit a plain `add` instead of the trapping
    /// `llvm.sadd.with.overflow` sequence.
    ///
    /// TWO producers arm it, each responsible for its own proof, and each
    /// narrow enough that exactly one `add` can consume the arming:
    ///
    /// 1. `accum_overflow` (B-2026-07-26-1) sets it immediately before
    ///    compiling a recognized accumulator RHS. That RHS is exactly
    ///    `<ident> + <literal>`, so there is no nested add to leak onto.
    /// 2. `compile_proven_index_expr` (B-2026-08-05-21) sets it around the
    ///    index expression of a `v[base + i]` whose bounds BCE has already
    ///    proven. That index is exactly `<ident> + <ident>`, so likewise one
    ///    add — and that producer save/restores rather than clearing, so it
    ///    can neither consume nor destroy an arming belonging to (1).
    pub(crate) elide_next_add_overflow_check: bool,
    /// `KARAC_BCE_OVF_SKIP=0` disables producer (2) above — the
    /// proven-in-bounds index-add overflow elision (B-2026-08-05-21). Escape
    /// hatch and A/B lever, mirroring the `KARAC_BCE_*_SKIP` family. Note the
    /// BCE kill switches disable it transitively as well: with the bounds
    /// facts gone, `index_bounds_already_proven` stops returning the
    /// `(true, true)` this rides on.
    pub(crate) elide_proven_index_add_overflow: bool,
}
