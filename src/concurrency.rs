// src/concurrency.rs

//! Concurrency analysis pass for the Kāra language.
//!
//! Analyzes function bodies to identify which statements can safely run in
//! parallel by building a dual-analysis dependency graph:
//! 1. **Data dependency**: if statement B reads a variable that A defines, B depends on A
//! 2. **Effect conflict**: if A and B have conflicting effects on the same resource, they
//!    must serialize
//!
//! Only when BOTH analyses find no dependency can statements be parallelized.

use crate::ast::*;
use crate::effectchecker::{DeclaredEffects, EffectCheckResult, EffectSet};
use crate::index_disjoint::{
    prove_disjoint_indexed_writes, DisjointDecline, DisjointWriteProof, TargetFootprint,
};
use crate::resolver::SpanKey;
use crate::typechecker::TypeCheckResult;
use std::collections::{HashMap, HashSet};

mod conflicts;
mod effects_collect;
mod hazards;
mod predicates;
mod reads;
mod reduction_shapes;
mod reductions;
mod var_extract;

use predicates::*;
pub(crate) use reduction_shapes::*;

// ── Result Types ───────────────────────────────────────────────

/// The full result of concurrency analysis across all functions.
#[derive(Debug, Clone)]
pub struct ConcurrencyAnalysis {
    /// Per-function parallelization decisions.
    pub function_decisions: HashMap<String, FunctionConcurrency>,
    /// Phase-8 stdlib-floor § Compiler queries channel sub-item 2.
    /// Empty in v1; future P1.6 catalogue entry (auto-concurrency
    /// fork threshold) pushes `CompilerQuery` values here.
    pub queries: Vec<crate::queries::CompilerQuery>,
}

/// Parallelization analysis for a single function.
#[derive(Debug, Clone)]
pub struct FunctionConcurrency {
    /// Groups of statement indices that can run in parallel.
    pub parallel_groups: Vec<ParallelGroup>,
    /// Total statements analyzed.
    pub total_statements: usize,
    /// Source span of each top-level body statement, indexed by the same
    /// ordinal used in `parallel_groups[].statement_indices` and
    /// `serialization_points[].statement_indices` (so `statement_spans[i]`
    /// locates statement `i`). Length is always `total_statements`. The
    /// ordinal stays the stable key for agents/diffs; this array makes the
    /// machine surface self-locating for IDE/LSP decoration and human
    /// reports without re-deriving positions by counting statements. See
    /// phase-5-diagnostics.md "Self-locating query output".
    pub statement_spans: Vec<crate::token::Span>,
    /// Top-level loops in the function body whose only loop-carried write
    /// is a reduction over an outer-scope accumulator with an op in the
    /// associative + commutative allow-list. Codegen consumes this list
    /// to lower the loop as a fan-out + reduce: each worker processes a
    /// contiguous slice of the iteration space into a per-thread partial,
    /// then a final serial pass combines the partials with the same op.
    /// See `docs/implementation_checklist/phase-7-codegen.md` — "Auto-par
    /// reduction recognition" — for the policy and slicing plan.
    pub loop_reductions: Vec<LoopReduction>,
    /// Loops the author annotated `#[par_order_free]` that did NOT fan out,
    /// each with the obligation that stopped it. See [`DeclinedParLoop`].
    pub declined_par_loops: Vec<DeclinedParLoop>,
    /// Loops whose body writes a collection at a computed index, with the
    /// per-iteration disjointness proof (or the obligation that failed). The
    /// third compute-fan-out shape; see [`DisjointWriteLoop`].
    pub disjoint_write_loops: Vec<DisjointWriteLoop>,
    /// The statement pairs that *can't* run in parallel, and why — the
    /// inverse of `parallel_groups`. Each records the conflicting
    /// statement indices, a human reason, the resource at issue (empty
    /// for a data/ordering conflict), and — for an effect conflict — the
    /// callees whose effect on that resource forced the serialization
    /// (`blocking_callees`). Inverting `blocking_callees` across all
    /// functions answers "which callers does function `f` block, and on
    /// what resource" — the Cartographer attribution view.
    pub serialization_points: Vec<SerializationPoint>,
    /// Independent statement pairs the contiguous-only grouper could not
    /// co-group *only because they are non-adjacent in source order* — a
    /// legal reorder (permitted by the data + effect dependency graph)
    /// would make them adjacent and let them parallelize. Each names the two
    /// ordinals and which one can slide. This is the deterministic "a better
    /// order exists" signal for the agent-driven reorder loop (option 1):
    /// the agent acts on a sound dependency signal instead of guessing, then
    /// re-runs `check` / `query` to confirm it helped and broke nothing. See
    /// phase-5-diagnostics.md "Contiguous-greedy grouping is suboptimal".
    pub reorder_opportunities: Vec<ReorderOpportunity>,
}

/// A pair of independent statements left unparallelized only by source
/// ordering, surfaced by [`ConcurrencyChecker::find_reorder_opportunities`].
/// See [`FunctionConcurrency::reorder_opportunities`].
#[derive(Debug, Clone)]
pub struct ReorderOpportunity {
    /// The two independent statement ordinals, ascending. Index into
    /// `statement_spans` to locate them.
    pub statement_indices: Vec<usize>,
    /// The ordinal (one of `statement_indices`) that can legally slide
    /// adjacent to its partner — every statement it passes over is
    /// dependency-independent of it, so the move preserves data + effect
    /// ordering. The advisory reports the move but does not apply it.
    pub movable_statement: usize,
    /// Human-readable explanation, e.g. ``statements 0 and 2 are
    /// independent but separated by statement 1; moving statement 2 adjacent
    /// would let them parallelize``.
    pub reason: String,
}

/// One reason two statements in a function body can't run in parallel —
/// the inverse of a [`ParallelGroup`]. See
/// [`FunctionConcurrency::serialization_points`].
#[derive(Debug, Clone)]
pub struct SerializationPoint {
    /// The two conflicting statement indices, ascending.
    pub statement_indices: Vec<usize>,
    /// Human-readable cause, e.g. `"writes(AuditLog) conflicts with
    /// writes(AuditLog)"`, `"data dependency on `x`"`, `"explicit seq
    /// ordering"`.
    pub reason: String,
    /// The resource at issue for an effect conflict (e.g. `"AuditLog"`);
    /// empty for a data-dependency / write-write / ordering conflict.
    pub resource: String,
    /// For an effect conflict: the callee keys (`fn` / `Type.method`)
    /// whose effect on `resource` caused the conflict. Empty for
    /// non-effect conflicts. Sorted + deduped.
    pub blocking_callees: Vec<String>,
    /// Structured, machine-readable counterpart to `reason`: *which axis*
    /// forced this serialization. Lets a consumer branch on the conflict
    /// class without parsing the prose `reason` — a data dependency and an
    /// effect conflict imply different fixes (break the dataflow vs split
    /// the resource), and the human string alone hides the distinction
    /// when two pairs read byte-identical on the effect surface. See
    /// phase-5-diagnostics.md "Per-statement exclusion-reason attribution".
    pub cause: SerializationCause,
}

/// Structured attribution of *which axis* serialized a statement pair —
/// the discriminated counterpart to [`SerializationPoint::reason`].
#[derive(Debug, Clone)]
pub enum SerializationCause {
    /// One of the two statements is inside a `seq {}` block — explicit
    /// user-requested ordering, not a discovered dependency.
    SeqOrdering,
    /// A local-binding dependency between the two statements. `vars` lists
    /// the bindings at issue (sorted, deduped); `kind` records the
    /// dependency direction.
    DataDependency {
        kind: DataDepKind,
        vars: Vec<String>,
    },
    /// A `with _` polymorphic-effect call whose effects are unknown at
    /// analysis time, forcing a conservative serialization.
    PolymorphicEffect,
    /// A resource-level effect conflict: both `verbs` act on `resource`.
    EffectConflict {
        resource: String,
        verbs: (EffectVerbKind, EffectVerbKind),
    },
}

/// Direction of a [`SerializationCause::DataDependency`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataDepKind {
    /// Read-after-write: the later statement reads a binding the earlier
    /// one writes — a true (flow) dependency.
    Raw,
    /// Write-after-read: the later statement writes a binding the earlier
    /// one reads — an anti-dependency.
    War,
    /// Both statements write the same binding — an output dependency.
    WriteWrite,
}

impl DataDepKind {
    /// Lowercase wire tag used in the structured query output.
    pub fn as_str(&self) -> &'static str {
        match self {
            DataDepKind::Raw => "raw",
            DataDepKind::War => "war",
            DataDepKind::WriteWrite => "ww",
        }
    }
}

/// An associative + commutative reduction operator recognized at v1.
/// Int-only allow-list per the roadmap entry; float `+`/`*` are deferred
/// to v1.x behind an `#[fp_reassoc]` opt-in because IEEE-754 addition is
/// not associative and per-thread combine order would break determinism.
///
/// `Collect` is a different reduction kind from the scalar ops: it
/// represents a Vec/String/Buffer accumulator that *collects* per-iter
/// contributions via `acc.push(x)` rather than scalar-folding. The
/// analyzer only recognizes `Collect` when the enclosing loop carries
/// the `#[par_order_free]` attribute (see
/// [`crate::ast::Attribute::is_par_order_free`]).
///
/// What that opt-in means, precisely (B-2026-07-29-30): today's lowering
/// PRESERVES iteration order on both Collect paths — the partials-concat
/// path because chunks are statically assigned and contiguous with no
/// work-stealing, so worker order is iteration order; the tabulate path
/// because each element is written straight into its final slot. The
/// attribute is not a warning that your output will be scrambled. It is
/// the user's promise that their output does not DEPEND on order, which
/// is what reserves the freedom to reorder later (work-stealing deques
/// are an open option in `runtime/src/scheduler.rs`) without silently
/// breaking programs already in the field. Requiring it is also what
/// keeps "auto-par never changes what your program prints" an
/// unconditional invariant.
///
/// See `phase-7-codegen.md` collect-style reduction entry for the full
/// design + slice plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReductionOp {
    Add,
    Mul,
    BitOr,
    BitAnd,
    BitXor,
    Min,
    Max,
    Collect,
}

impl ReductionOp {
    /// Source-level glyph for the op, used in `--concurrency-report`
    /// output and in diagnostic messages.
    pub fn symbol(&self) -> &'static str {
        match self {
            ReductionOp::Add => "+",
            ReductionOp::Mul => "*",
            ReductionOp::BitOr => "|",
            ReductionOp::BitAnd => "&",
            ReductionOp::BitXor => "^",
            ReductionOp::Min => "min",
            ReductionOp::Max => "max",
            ReductionOp::Collect => "collect",
        }
    }

    fn from_bin_op(op: &BinOp) -> Option<Self> {
        match op {
            BinOp::Add => Some(ReductionOp::Add),
            BinOp::Mul => Some(ReductionOp::Mul),
            BinOp::BitOr => Some(ReductionOp::BitOr),
            BinOp::BitAnd => Some(ReductionOp::BitAnd),
            BinOp::BitXor => Some(ReductionOp::BitXor),
            _ => None,
        }
    }

    fn from_compound_op(op: &CompoundOp) -> Option<Self> {
        match op {
            CompoundOp::Add => Some(ReductionOp::Add),
            CompoundOp::Mul => Some(ReductionOp::Mul),
            CompoundOp::BitOr => Some(ReductionOp::BitOr),
            CompoundOp::BitAnd => Some(ReductionOp::BitAnd),
            CompoundOp::BitXor => Some(ReductionOp::BitXor),
            _ => None,
        }
    }
}

/// A loop body recognized as a reduction over a single accumulator.
/// `stmt_index` identifies the top-level loop statement in the
/// enclosing function's body; `loop_line` is the loop expression's
/// 1-indexed source line, suitable for the report's user-facing text.
#[derive(Debug, Clone)]
pub struct LoopReduction {
    pub accumulator: String,
    pub op: ReductionOp,
    pub stmt_index: usize,
    pub loop_line: usize,
    /// Collect-only: the body pushes EXACTLY one element per iteration,
    /// unconditionally, and mentions the accumulator nowhere else. This
    /// licenses the tabulate lowering — output length is exactly
    /// `iter_total` and iteration `i` owns output slot `i`, so workers
    /// write elements in place into one presized shared buffer (no
    /// per-worker partial Vecs, no combine memcpy). The gate must be
    /// exact: an extra or skipped push under tabulate overflows a
    /// worker's chunk view and the push grow-path would free an interior
    /// pointer. See `collect_is_tabulate_shape`.
    pub collect_tabulate: bool,
    /// SEQUENTIAL tabulate (no `#[par_order_free]`): the same
    /// tabulate-shape guarantee, lowered inline — reserve the exact
    /// capacity once, store each element in place, bump `len` after the
    /// loop. No parallel dispatch, no reordering license needed; the
    /// win is removing the per-iteration push grow-branch + realloc
    /// call, which is what blocks LLVM's loop vectorizer on the
    /// canonical `out.push(f(v[i]))` map loop (see the phase-10
    /// CPU-codegen-gap entry, 2026-07-16 forensics). Only ever true
    /// with `op == Collect && collect_tabulate`.
    pub seq: bool,
    /// Source-type display of the SCALAR accumulator, resolved from the
    /// typed AST (`expr_types` at an identifier use of the accumulator in
    /// the loop body). `None` when the analysis ran without a
    /// `TypeCheckResult`, when no typed use was found, or for
    /// `Collect`/`seq` entries (a `Vec` accumulator takes its own lowering
    /// and never consults the scalar type gate). Drives the query side of
    /// the non-integer-accumulator fan-out gate
    /// (`par_cost::accumulator_type_fans_out`, B-2026-07-31-14).
    pub accumulator_type: Option<String>,
}

/// A loop the author ANNOTATED `#[par_order_free]` that did not fan out, and the
/// obligation that stopped it.
///
/// The attribute is an explicit opt-in — B-2026-07-29-30 renamed it precisely to
/// make it "the caller's promise not to depend on order" — so a loop carrying it
/// and lowering sequentially is a decision the compiler owes the author an
/// explanation for. Before B-2026-08-15-19 there was none: the loop did not
/// appear in `loop_reductions` (it is not a reduction), it did not appear in
/// `disjoint_write_loops` (it is not that shape either), and no diagnostic was
/// emitted. `karac query concurrency` reported nothing at all about it.
///
/// Deliberately a SEPARATE list rather than a `declined` flag on
/// [`LoopReduction`]: codegen consumes `loop_reductions` to choose a lowering,
/// and putting non-reductions in it would invite exactly the confusion this row
/// was filed about. These records are reporting-only.
///
/// Unannotated loops are not recorded. Every loop in a program that is not a
/// reduction would qualify, which is noise, not an explanation — the signal is
/// that the AUTHOR asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclinedParLoop {
    /// Index of the top-level statement holding the loop.
    pub stmt_index: usize,
    /// Source line of the loop header.
    pub loop_line: usize,
    /// The obligation that failed, phrased as what the fan-out needed and did
    /// not get. Static strings attached at the decline site, so they cannot
    /// drift from the condition that produced them.
    pub reason: &'static str,
}

/// A loop whose body writes a collection at a computed index — the third
/// compute-fan-out shape, alongside parallel `let`-groups and associative
/// reductions (`design.md § 8876`).
///
/// One record per candidate loop, **whether or not the proof discharged**: the
/// declined case is the whole point of the surface. `karac query concurrency`
/// renders these so the answer to "why isn't my loop parallel" is a compiler
/// explanation naming the failed obligation, which is what the slice design
/// accepted *in place of* a `par for` keyword.
///
/// ## This is a footprint proof, not a fan-out decision
///
/// `decline == None` means: every iteration of `loop_var` writes each target
/// only inside its own contiguous range, so no two iterations can touch the
/// same slot. It does **not** mean the loop fans out — the fan-out lowering,
/// its cost gate, and the differential harness that gates enabling it are
/// separate sub-slices. There is deliberately no `fanned_out` field here yet;
/// adding one before the lowering exists would repeat exactly the
/// over-promise B-2026-07-29-29 was filed for.
#[derive(Debug, Clone)]
pub struct DisjointWriteLoop {
    /// The loop's index within its own block (same convention as
    /// [`LoopReduction::stmt_index`]).
    pub stmt_index: usize,
    /// 1-indexed source line of the loop expression. Reporting only — the
    /// codegen lookup keys on [`Self::loop_span`], which `loop_line` is NOT a
    /// safe substitute for; see that field.
    pub loop_line: usize,
    /// Byte span of the loop expression, and the **exact** key codegen matches
    /// this tag by.
    ///
    /// `(stmt_index, loop_line)` — what the reduction tags use — is not unique:
    /// a nested loop written on its parent's line (`for y in 0..h { for x in
    /// 0..w { … } }`) has BOTH the same statement index (0, within the outer
    /// loop's own body block) and the same line. Codegen compiles the outer
    /// loop's body inside the fan-out worker, hits the inner loop there, and
    /// matches the outer loop's tag against it — emitting a second fan-out over
    /// a loop nothing proved disjoint.
    ///
    /// That is a miscompile, not an inefficiency. The outer proof says
    /// "iteration `y` writes only `[y*S, (y+1)*S)`"; it says nothing about two
    /// `x` values being distinct. `for y in 0..h { for x in 0..w { out[y*w] =
    /// f(x); } }` proves for `y` and is a same-slot race for `x`. A span is
    /// unique per expression, so matching on it closes the class.
    pub loop_span: crate::token::Span,
    /// The candidate parallel dimension.
    pub loop_var: String,
    /// `None` when the proof discharged; otherwise the obligation that failed.
    pub decline: Option<DisjointDecline>,
    /// Per-target footprints. Empty on a decline.
    pub targets: Vec<TargetFootprint>,
    /// Prose for the query `reason` field and the concurrency report.
    pub reason: String,
}

impl DisjointWriteLoop {
    /// True when every iteration's write footprint was proven disjoint.
    pub fn proven(&self) -> bool {
        self.decline.is_none()
    }

    /// Stable machine tag: `"proven"`, or the declining obligation's name.
    pub fn tag(&self) -> &'static str {
        match self.decline {
            None => "proven",
            Some(d) => d.tag(),
        }
    }
}

/// A set of statements that can safely run in parallel.
#[derive(Debug, Clone)]
pub struct ParallelGroup {
    /// Indices of statements in this parallel group.
    pub statement_indices: Vec<usize>,
    /// Why these can be parallelized.
    pub reason: String,
    /// True if the group is too cheap to justify thread dispatch
    /// (pure arithmetic, simple variable access, no I/O or function calls with effects).
    /// Codegen should run trivial groups inline instead of spawning tasks.
    pub is_trivial: bool,
    /// Names of *captured* (pre-existing) locals that some stmt in this
    /// group mutates without introducing them as a fresh let-binding —
    /// e.g., `v.push(3)` mutates the captured `v`, `cap = max` mutates
    /// the captured `cap`. The auto-par codegen captures locals by
    /// value into the per-branch env struct, so these mutations live
    /// on the branch's local copy and are lost at join time. Codegen
    /// (`compute_return_slots_checked`) consults this set: if any name
    /// in it is read outside the group, the par-group is dropped and
    /// the stmts run sequentially. Names freshly introduced by
    /// `let`/`let-uninit`/`let-else` patterns within the group itself
    /// are excluded — those flow through the return-slot mechanism
    /// already.
    pub captured_mutations: HashSet<String>,
    /// The subset of `captured_mutations` naming HEAP-OWNING CONTAINER
    /// locals (`Vec` / `String` / `Map` / `Set` / sorted variants). A lost
    /// branch-local mutation of one of these is never a dead write even when
    /// no later statement reads the name: the parent's scope-exit drop reads
    /// the container header, and the branch's realloc'd buffer + pushed
    /// elements are orphaned (B-2026-07-15-2 — the write-only single-push
    /// `Vec[shared]` leak). Codegen falls back to sequential whenever this
    /// set is non-empty, independent of the outside-reads check.
    pub captured_container_mutations: HashSet<String>,
}

// ── Internal: Per-statement metadata ───────────────────────────

/// Metadata extracted from a single statement for dependency analysis.
#[derive(Debug, Clone, Default)]
struct StmtInfo {
    /// Variables defined (written) by this statement.
    defines: HashSet<String>,
    /// Names freshly introduced by `let`/`let-uninit`/`let-else`
    /// patterns in this statement (subset of `defines`). The complement
    /// `defines − let_introduced` is the set of *captured* names this
    /// statement mutates — needed by the auto-par codegen to decide
    /// whether a multi-stmt group can safely run in parallel given
    /// that captures are bit-copied into per-branch envs.
    let_introduced: HashSet<String>,
    /// Variables read by this statement.
    reads: HashSet<String>,
    /// Bare names of functions this statement calls (free-fn callee names
    /// and method names, transitively through the statement's expression
    /// tree). Drives the SELF-RECURSION par gate (B-2026-07-15-4): a group
    /// whose statement calls the enclosing function is a recursive
    /// divide-and-conquer — spawning it costs ~70µs per dispatch and O(nodes)
    /// dispatches per top-level call (each recursion level re-spawns), which
    /// no bounded top-level win can amortize without a work-stealing
    /// sequential-cutoff scheduler. Measured 175x wall-time regression on a
    /// 15-node tree build at 20k reps before the gate.
    called_fn_names: HashSet<String>,
    /// Effects produced by this statement (from called functions).
    effects: Vec<StmtEffect>,
    /// Whether this statement (transitively) calls a function with polymorphic
    /// effects (`with _`). Its effects are unknown at analysis time, so it must
    /// serialize conservatively against any other stmt with visible effects.
    calls_polymorphic: bool,
    /// Whether this statement is inside a seq {} block.
    is_seq: bool,
    /// Whether this statement may exit the enclosing function abnormally
    /// (an `if` body / loop body / match arm reachable through this stmt
    /// contains `return`, `break`, or `continue`). Such statements
    /// cannot share a parallel group with siblings — par branches are
    /// emitted as standalone `void` LLVM functions and a raw `ret X`
    /// from inside the branch produces invalid IR ("return instr that
    /// returns non-void in Function of void return type").
    has_early_exit: bool,
    /// Whether this statement performs a channel operation (`Channel.new()`
    /// / `Sender.send` / `Receiver.recv` / `Receiver.try_recv`). Such
    /// statements are kept out of auto-par groups — channels are explicit
    /// communication primitives whose ordering auto-par must not disturb.
    /// See `stmt_has_channel_op` and the `find_parallel_groups` guards.
    has_channel_op: bool,
    /// Whether this statement *syntactically* performs console output
    /// (`println` / `print` / `eprintln` / `eprint`). Used only by the
    /// reorder-opportunity advisory to exclude such statements as movers —
    /// relocating a console write reorders observable output, which the
    /// effect surface (console output is resourceless) would not flag. A
    /// best-effort local check, not interprocedural. See
    /// `stmt_has_console_output`.
    has_console_output: bool,
    /// Whether this statement is a direct, pure `sleep_ms(...)` timer-park
    /// call — the ONLY `suspends` form the auto-parallelizer overlaps (A2b).
    /// `suspends` is an execution verb (placement, not conflict — design.md
    /// :5907), but at the effect level a timer wait and a channel `recv` are
    /// indistinguishable (both seed a bare `suspends`), and a channel recv is
    /// NOT independent — it has a happens-before with its producer, so
    /// relocating it into a `__par_branch` worker deadlocks. So the boundary
    /// gate keeps *every* `suspends` stmt serial (conservative default) and
    /// exempts only the ones proven to be a standalone timer park here. See
    /// `stmt_is_timer_suspend` and the `find_parallel_groups` boundary guards.
    is_timer_suspend: bool,
    /// A2b-2: whether this statement is a network-boundary call the auto-par
    /// fan-out can safely overlap — a direct free-function `Call` (or its
    /// `let`) whose arguments move in NO owned heap/`Drop` binding, so the
    /// coroutine-owned-param double-drop (the `__par_branch` suppression-scope
    /// gap) cannot fire. Like `is_timer_suspend`, it exempts the statement from
    /// the `effects_mark_coroutine_boundary` gate — the conflict model then
    /// keeps same-resource network calls (`sends`/`receives` on `Network`)
    /// serial and overlaps only independent ones (e.g. two `reads(Network)`
    /// fetches). Fail-closed: proven purely from AST shape (no type info in
    /// this pass), so it admits literal/const-arg calls only; variable-arg
    /// fan-out (Copy/borrow args) awaits threading ownership info through and
    /// is the A2b-2 follow-up. Set in `analyze_stmt` as `stmt_fanout_args_safe`
    /// (arg-safety) AND a `Network`-resource-effect check.
    is_safe_network_fanout: bool,
    /// A2b-2 Phase 1: whether this statement is an *ephemeral* network
    /// fan-out — a safe network fan-out (`is_safe_network_fanout`) whose
    /// callee declares NO borrow parameter (`ref`/`mut ref`/`mut Slice`). No
    /// borrow param means the callee cannot be handed a shared connection
    /// object; it must open its own connection internally (the
    /// `http_get(url: String)` shape), so two such calls touch disjoint,
    /// freshly-created OS connection state. That is what makes it *sound to
    /// relax the `Network`-resource conflict* between two of them
    /// (`(Sends,Sends)`/`(Receives,Receives)` on `Network`) in
    /// `statements_conflict`, letting `http_get("a"); http_get("b")` fan out
    /// with their real `sends`/`receives` effects. A call that borrows an
    /// argument (`send_on(ref conn, ...)`) is deliberately excluded: two ops
    /// on the same borrowed connection would race if overlapped, and this
    /// pass has no connection-identity info to tell same-conn from
    /// different-conn apart — that is the Phase 2 parameterized-`Network`
    /// follow-up (docs/spikes/network-resource-granularity.md). Any *other*
    /// shared resource a callee touches (a pool checkout `writes(Pool)`, a DB
    /// `writes(Db)`) still surfaces as a non-`Network` effect and still
    /// serializes — the relaxation only ever skips `Network`↔`Network` pairs.
    /// Set in `analyze_stmt` as `is_safe_network_fanout` AND
    /// `stmt_callee_has_no_borrow_params`.
    is_ephemeral_network_fanout: bool,
    /// A2b-2 Phase 2 Slice 2: for a method-call network fan-out CANDIDATE
    /// (`obj.method(args)` touching `Network`, borrowed `ref`/`mut ref self`,
    /// plain-identifier receiver that is neither a `ref` param nor a `shared`
    /// (RC) type, args fan-out-safe), the receiver ROOT identifier; `None`
    /// otherwise. Two such statements with DIFFERENT roots have provably
    /// distinct, non-aliasing receivers — distinct connections — so
    /// `statements_conflict` relaxes their `Network`↔`Network` conflict. Same
    /// root is already serialized by the write-write data dependency (a
    /// `mut ref self` method defines its receiver), and a shared-type / ref-param
    /// receiver (which could alias under a different name) is excluded here.
    /// Requires type info (`method_callee_types`); `None` without it
    /// (fail-closed). Computed in `analyze_stmt` via `classify_method_fanout`.
    method_fanout_receiver_root: Option<String>,
    /// Whether this statement is a constant-cost initializer — a
    /// `let`/`assign` of a literal or bare identifier, or a `let
    /// uninit`. These are O(1) and run in ~zero time. Used by the
    /// cost-model gate in `find_parallel_groups`: a parallel group
    /// where N−1 of N stmts are constant-init has zero structural
    /// parallelism (one branch does all the work, others idle) and is
    /// marked trivial so codegen skips the `karac_par_run` dispatch.
    /// Without this, the auto-parallelizer pays per-spawn cost (~70μs
    /// on macOS) for groups that can produce no speedup — the
    /// dominant hot-path overhead surfaced by the kata 6 zigzag bench
    /// (2.5× slowdown vs sequential codegen, 2026-05-17).
    is_constant_init: bool,
}

/// Human label for an effect verb, used in serialization-point reasons.
fn effect_verb_label(v: &EffectVerbKind) -> &str {
    match v {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(s) => s.as_str(),
    }
}

/// An effect associated with a statement.
/// The console resources (`Stdout` / `Stderr`), whose writes the runtime
/// serializes for us (B-2026-08-23-8).
///
/// Console writes carry a real effect — a public function that prints must
/// DECLARE `writes(Stdout)`, which is the whole point of seeding them — but
/// they must not change any CONCURRENCY decision, because they never used to
/// participate in one: before the seeding they reached this pass as no effect
/// at all, and the auto-par design leans on that. `karac_par_run` captures
/// each branch's output and replays it in source order at the join, so
/// parallel prints are byte-identical to sequential ones.
///
/// Two sites consult this, and BOTH are required — missing either would let a
/// declaration-side seed silently change generated code:
///   * `conflicts.rs::two_effects_conflict` — so two prints still fan out
///     rather than serializing (the reversal of B-2026-06-13-18's blanket
///     suppression that `find_parallel_groups` documents).
///   * the cost model's `all_pure` below — so a group of nothing but prints is
///     still TRIVIAL and codegen declines it, instead of paying ~70μs of spawn
///     per dispatch to run two `println`s in parallel.
pub(crate) fn is_console_resource(resource: &str) -> bool {
    resource == "Stdout" || resource == "Stderr"
}

#[derive(Debug, Clone)]
struct StmtEffect {
    verb: EffectVerbKind,
    resource: String,
    /// The callee whose effect this is — the function/method key
    /// (`fn` name or `Type.method`) that contributed this effect to the
    /// statement, or `None` for an effect the statement performs
    /// directly. Used to attribute a serialization point to the specific
    /// callee responsible (`SerializationPoint::blocking_callees`).
    source_callee: Option<String>,
    /// A2b-2 Phase 2 Slice 3 (parameterized resources): the **partition key**
    /// for a parameterized resource (`writes(Db[id])`), when it resolves to a
    /// compile-time LITERAL at this call site — the callee's declared param
    /// substituted with the actual argument (`update(5)` on `writes(Db[id])` →
    /// `Some("5")`). `None` for an unparameterized resource OR a param that does
    /// not reduce to a literal here (a variable arg — fail-closed to "unproven",
    /// so it conservatively conflicts). Two same-resource effects with distinct
    /// `Some` keys touch DIFFERENT partitions and never conflict
    /// (`design.md § Parameterized Resources`, proven-disjoint case).
    key: Option<String>,
}

/// True iff a statement's effect set marks it as a **coroutine network-boundary
/// call** — one that the A2 coroutine transform (`build_state_struct_layouts`,
/// keyed off `sends(Network)`/`receives(Network)`) compiles into a dispatcher-
/// driven LLVM coroutine, or a `suspends` park (e.g. `Receiver.recv`). Such a
/// statement must not be auto-parallelized: a coroutine owns + drops its
/// by-value params at completion while auto-par captures are shared-with-write-
/// back (the parent keeps drop ownership), so lifting the call into a
/// `__par_branch` worker double-drops any owned user-`Drop` arg (an fd
/// double-close for a `WebSocket`), and the ramp+wait belongs to the async
/// dispatcher, not the `karac_par_run` pool. See `find_parallel_groups`.
///
/// **`suspends` stays gated, except a standalone timer park (A2b, 2026-06-10).**
/// At the effect level a channel `recv`, a network park, and `sleep_ms` all
/// seed a bare `suspends` — indistinguishable here. A channel recv is NOT
/// independent (it has a happens-before with its producer; relocating it into a
/// `__par_branch` worker deadlocks — regression-pinned by
/// `e2e_auto_par_channel_consumer_terminates`), so the conservative default is
/// to keep every `suspends` stmt serial. The `find_parallel_groups` boundary
/// guards then exempt only the stmts `stmt_is_timer_suspend` proves to be a
/// standalone `sleep_ms` call — the one `suspends` form known to be independent
/// (a bare timer wait, no by-value `Drop` params). The harder *network*
/// coroutine fan-out (design.md:9044 `http_get` — true double-drop, wants
/// dispatcher routing) stays gated pending A2b-2.
fn effects_mark_coroutine_boundary(effects: &[StmtEffect]) -> bool {
    effects.iter().any(|e| {
        matches!(e.verb, EffectVerbKind::Suspends)
            || (matches!(e.verb, EffectVerbKind::Sends | EffectVerbKind::Receives)
                && e.resource == "Network")
    })
}

/// True iff `stmt` is a direct, pure `sleep_ms(...)` timer-park call — the only
/// `suspends` form the auto-parallelizer overlaps (A2b). `find_parallel_groups`
/// exempts such statements from the `effects_mark_coroutine_boundary` gate so
/// two independent timer waits overlap via the `karac_par_run` thread-block
/// path, exactly like `blocks` (A1). It is deliberately conservative: the stmt
/// must be exactly a `sleep_ms` call whose args contain no further call or
/// method (which could itself suspend or touch a channel) — anything richer
/// stays serial. A `sleep_ms` wrapper fn (`fn nap() { sleep_ms(..) }`) does NOT
/// qualify (the call site sees only the wrapper's propagated `suspends`, not
/// that it is timer-pure); supporting wrappers would need provenance on the
/// effect and is left to A2b-2.
fn stmt_is_timer_suspend(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => expr_is_pure_sleep_ms_call(value),
        _ => false,
    }
}

/// `sleep_ms(<call-free args>)` — a `Call` to the bare `sleep_ms` path whose
/// every argument is itself free of any nested call/method.
fn expr_is_pure_sleep_ms_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            // A bare free-function callee is either an `Identifier` or a
            // single-segment `Path`, depending on parse context.
            let is_sleep_ms = match &callee.kind {
                ExprKind::Identifier(name) => name == "sleep_ms",
                ExprKind::Path { segments, .. } => segments.len() == 1 && segments[0] == "sleep_ms",
                _ => false,
            };
            is_sleep_ms && args.iter().all(|a| expr_is_call_free(&a.value))
        }
        _ => false,
    }
}

/// True iff `expr` contains no `Call` and no `MethodCall` anywhere — used to
/// confirm a `sleep_ms` argument cannot itself suspend or touch a channel.
fn expr_is_call_free(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Closure { .. } => false,
        ExprKind::Binary { left, right, .. } => expr_is_call_free(left) && expr_is_call_free(right),
        ExprKind::Unary { operand, .. } => expr_is_call_free(operand),
        ExprKind::Index { object, index } => expr_is_call_free(object) && expr_is_call_free(index),
        ExprKind::FieldAccess { object, .. } => expr_is_call_free(object),
        ExprKind::Cast { expr, .. } => expr_is_call_free(expr),
        // Literals, paths, and other leaf forms carry no call.
        _ => true,
    }
}

/// A2b-2 (arg-safety half): true iff `stmt` is a direct free-function `Call`
/// (or `let x = Call(...)`) whose every argument is BOTH call-free (no nested
/// call/method that could itself suspend or touch a channel) AND binding-free
/// (references no name, so nothing owned is moved into the coroutine). The
/// coroutine-owned-param double-drop
/// (docs/spikes/network-async-coroutine-transform.md; the `__par_branch`
/// suppression-scope gap in `call_dispatch.rs`) fires ONLY when a coroutine
/// call moves an owned parent `Drop`/heap binding into itself via an
/// `Identifier` argument; a literal / const-expression argument names no
/// binding, so the caller's drop-suppression has nothing to cancel and the
/// value drops exactly once (inside the coroutine). Deliberately conservative:
/// it admits the flagship two-`http_get("...")`-to-different-hosts shape and
/// leaves variable-arg fan-out (Copy/borrow args — safe, but indistinguishable
/// from an owned move without type info this pass does not carry) to the A2b-2
/// follow-up. This is only the ARG-safety half; `analyze_stmt` combines it with
/// a Network-resource-effect check so the exemption fires for network calls
/// only (a non-network user `with suspends` fn stays serial), and the conflict
/// model still serializes same-resource network calls
/// (`(Sends,Sends)`/`(Receives,Receives)` on `Network`) — the exemption ONLY
/// lifts the blanket coroutine-boundary EXCLUSION so two *independent*
/// (disjoint-resource) network calls can group.
fn stmt_fanout_args_safe(
    stmt: &Stmt,
    function_bodies: &HashMap<String, &Function>,
    method_bodies: &HashMap<String, &Function>,
) -> bool {
    let value = match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => value,
        _ => return false,
    };
    let ExprKind::Call { callee, args } = &value.kind else {
        return false;
    };
    // Resolve the callee's params. Admitted callee shapes: a bare free function
    // (`Identifier` / 1-segment `Path`) OR a 2-segment ASSOCIATED-function path
    // (`Type.connect(...)`, no `self` receiver — A2b-2 Phase 2 Slice 1). Neither
    // has a receiver to move into the coroutine or to share between two calls,
    // so both fit the double-drop reasoning below. A 2-segment path that is a
    // METHOD (has `self` — its receiver IS a connection, e.g. `stream.read`) or
    // is unresolvable (extern, associated-vs-method unknown) is rejected via
    // `resolve_assoc_callee`, and a computed callee is outside the shape.
    //
    // For a free function the params may be absent (extern) — a literal argument
    // is still safe, so the shape is admitted with `None` params (`param_is_borrow`
    // is `false` on `None`, so any `Identifier` argument then fails, leaving only
    // literal-arg extern calls). When present, the params tell us which positions
    // BORROW their argument (`ref`/`mut ref`/`mut Slice` — not moved): an
    // `Identifier` at a borrow position moves no owned binding into the coroutine,
    // so it is fan-out-safe even though it names a binding (verified
    // double-free-clean by
    // `tests/memory_sanitizer.rs::asan_par_ref_string_arg_network_call_no_double_free`).
    let callee_params: Option<&[Param]> = match &callee.kind {
        ExprKind::Identifier(n) => function_bodies.get(n).map(|f| f.params.as_slice()),
        ExprKind::Path { segments, .. } if segments.len() == 1 => function_bodies
            .get(&segments[0])
            .map(|f| f.params.as_slice()),
        ExprKind::Path { segments, .. } if segments.len() == 2 => {
            match resolve_assoc_callee(segments, method_bodies) {
                Some(f) => Some(f.params.as_slice()),
                None => return false,
            }
        }
        _ => return false,
    };
    args.iter().enumerate().all(|(i, a)| {
        expr_is_call_free(&a.value)
            && (expr_is_binding_free(&a.value)
                || (matches!(a.value.kind, ExprKind::Identifier(_))
                    && param_is_borrow(callee_params, i)))
    })
}

/// True iff the callee's parameter at position `i` is a borrow form
/// (`ref T` / `mut ref T` / `mut Slice[T]`) — an argument passed there is
/// borrowed, never moved, so an owned binding at that position is not
/// double-dropped when the call is lifted into a par branch. `None` params
/// (callee body not in this program) → `false` (fail-closed).
fn param_is_borrow(params: Option<&[Param]>, i: usize) -> bool {
    params.and_then(|ps| ps.get(i)).is_some_and(|p| {
        matches!(
            p.ty.kind,
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
        )
    })
}

/// True iff the callee's parameter at position `i` is a MUTABLE borrow
/// (`mut ref T` / `mut Slice[T]` — NOT the read-only `ref T`). An argument
/// passed there is written through the callee, so if its place-root is a
/// loop-invariant binding the loop's iterations are not independent
/// (B-2026-07-23-20). A read-only `ref T` shares immutable data and is
/// parallel-safe, so it is deliberately excluded.
fn param_is_mut_borrow(params: Option<&[Param]>, i: usize) -> bool {
    params
        .and_then(|ps| ps.get(i))
        .is_some_and(|p| matches!(p.ty.kind, TypeKind::MutRef(_) | TypeKind::MutSlice(_)))
}

/// The root binding name of a place expression — the identifier at the base of
/// an `Index` / `FieldAccess` / `TupleIndex` chain (`a` for `a`, `a[i]`,
/// `a.f.g`, `a[i].f`), or the canonical `"self"` for a `self`-rooted place.
/// `None` for a non-place root (a literal, a call result, an arithmetic expr).
fn place_root(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::SelfValue => Some("self".to_string()),
        ExprKind::Index { object, .. }
        | ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. } => place_root(object),
        _ => None,
    }
}

/// A2b-2 Phase 2 Slice 3: resolve a parameterized-resource key expression
/// (`Db[<param>]`) to a compile-time-LITERAL partition key at a call site, or
/// `None` if it does not reduce to a literal here. The declared key `param` is
/// relative to the callee's `params`: a bare identifier names a callee
/// parameter, substituted with the actual `args` at the same position; a
/// literal in the declaration itself is taken verbatim. A non-literal resolved
/// argument (a variable) yields `None` — deliberately "unproven", so two such
/// calls conservatively conflict. Integer keys normalize to their numeric value
/// (so `5` and `5u64` are the same partition), keeping distinctness sound.
fn resolve_param_key(param: &Expr, params: &[Param], args: &[CallArg]) -> Option<String> {
    match &param.kind {
        ExprKind::Identifier(pname) => {
            let idx = params
                .iter()
                .position(|p| matches!(&p.pattern.kind, PatternKind::Binding(n) if n == pname))?;
            literal_key(&args.get(idx)?.value)
        }
        _ => literal_key(param),
    }
}

/// The compile-time-literal partition key of `expr` (its normalized value), or
/// `None` if `expr` is not an integer/string literal.
fn literal_key(expr: &Expr) -> Option<String> {
    match &expr.kind {
        ExprKind::Integer(n, _) => Some(n.to_string()),
        ExprKind::StringLit(s) => Some(s.clone()),
        _ => None,
    }
}

/// A2b-2 Phase 2 (Slice 1): resolve a 2-segment `Type.method` callee to its
/// body IFF it is an ASSOCIATED function — one with NO `self` receiver
/// (`self_param.is_none()`) — present in this program's `method_bodies`. These
/// are the receiver-less connection *openers* (`TcpStream.connect`,
/// `TlsStream.connect`): structurally identical to a free function, since there
/// is no receiver to move into the coroutine or to share between two calls. A
/// 2-segment path that resolves to a METHOD (`self_param.is_some()` — its
/// receiver IS a live connection/listener, e.g. `stream.read`, `listener.accept`)
/// is deliberately NOT admitted (returns `None`): overlapping two ops on one
/// shared receiver would race. An unresolvable callee (extern — associated
/// vs. method is unknown) also returns `None`, fail-closed. Returns `None` for a
/// non-2-segment path so callers can branch on it uniformly.
fn resolve_assoc_callee<'a>(
    segments: &[String],
    method_bodies: &HashMap<String, &'a Function>,
) -> Option<&'a Function> {
    if segments.len() != 2 {
        return None;
    }
    let key = format!("{}.{}", segments[0], segments[1]);
    method_bodies
        .get(&key)
        .copied()
        .filter(|f| f.self_param.is_none())
}

/// A2b-2 Phase 1 companion to [`stmt_fanout_args_safe`]: true iff the
/// statement's callee (resolved by the same free-fn / associated-fn rule) is in
/// this program and declares NO borrow parameter (`ref`/`mut ref`/`mut Slice`).
/// Combined with `is_safe_network_fanout` in `analyze_stmt` it yields
/// `is_ephemeral_network_fanout` — see that field's doc for why a borrow-free
/// callee proves two network calls use disjoint, freshly-opened connections
/// and may overlap. Fail-closed: a computed callee, a non-`Call` statement, or
/// an extern callee (body not in this program, so its param modes are unknown)
/// all return `false`.
fn stmt_callee_has_no_borrow_params(
    stmt: &Stmt,
    function_bodies: &HashMap<String, &Function>,
    method_bodies: &HashMap<String, &Function>,
) -> bool {
    let value = match &stmt.kind {
        StmtKind::Let { value, .. } | StmtKind::Expr(value) => value,
        _ => return false,
    };
    let ExprKind::Call { callee, .. } = &value.kind else {
        return false;
    };
    // Mirror `stmt_fanout_args_safe`'s callee resolution: a bare free function
    // or a 2-segment ASSOCIATED-function path (`Type.connect`, no `self`). Unlike
    // that function, this one needs the params to exist — an extern callee (body
    // absent) is fail-closed `false`, since its param modes are unknown.
    let params: &[Param] = match &callee.kind {
        ExprKind::Identifier(n) => match function_bodies.get(n) {
            Some(f) => &f.params,
            None => return false,
        },
        ExprKind::Path { segments, .. } if segments.len() == 1 => {
            match function_bodies.get(&segments[0]) {
                Some(f) => &f.params,
                None => return false,
            }
        }
        ExprKind::Path { segments, .. } if segments.len() == 2 => {
            match resolve_assoc_callee(segments, method_bodies) {
                Some(f) => &f.params,
                None => return false,
            }
        }
        _ => return false,
    };
    !params.iter().any(|p| {
        matches!(
            p.ty.kind,
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
        )
    })
}

/// True iff `expr` references no binding — used by `stmt_fanout_args_safe`
/// to prove a network call's arguments move no owned parent binding into the
/// coroutine. FAIL-CLOSED: only pure-value literals and arithmetic/cast over
/// them are binding-free; ANY `Identifier`/`Path`, and every richer form
/// (struct/array/map literal, interpolated string, index, field access, call,
/// closure, …) that could carry a name, disqualifies. This pass has no type
/// info, so it cannot tell an owned heap binding from a Copy scalar and
/// conservatively excludes all names.
fn expr_is_binding_free(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(..)
        | ExprKind::ByteLit(..)
        | ExprKind::ByteStringLit(..)
        | ExprKind::StringLit(..)
        | ExprKind::MultiStringLit(..)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(..) => true,
        ExprKind::Binary { left, right, .. } => {
            expr_is_binding_free(left) && expr_is_binding_free(right)
        }
        ExprKind::Unary { operand, .. } => expr_is_binding_free(operand),
        ExprKind::Cast { expr, .. } => expr_is_binding_free(expr),
        // Identifier / Path / interpolated string / struct-array-map literals /
        // index / field access / call / closure / everything else: fail closed.
        _ => false,
    }
}

/// Sparse statement-conflict graph.
///
/// Replaces the former dense `Vec<Vec<bool>>` adjacency matrix (which was
/// `O(n²)` memory — a 49K-statement function alone allocated ~2.4 GB of
/// bools — and was filled by an all-pairs `O(n²)` scan). Two statements can
/// only conflict if they share a *binding* (dataflow), a *resource*
/// (effect), a *polymorphic-effect* linkage, or a `seq` ordering — see
/// [`ConcurrencyChecker::statements_conflict`]. So an inverted index over
/// those keys enumerates every real edge in ~`O(edges)` work, with no
/// quadratic allocation or all-pairs conflict check. See
/// phase-5-diagnostics.md.
struct ConflictGraph {
    /// `neighbors[i]` = the set of statements that conflict with statement `i`.
    neighbors: Vec<HashSet<usize>>,
}

impl ConflictGraph {
    /// Do statements `i` and `j` conflict (must serialize)? Symmetric.
    fn conflicts(&self, i: usize, j: usize) -> bool {
        self.neighbors[i].contains(&j)
    }
}

// ── Checker ────────────────────────────────────────────────────

pub struct ConcurrencyChecker<'a> {
    program: &'a Program,
    effects: &'a EffectCheckResult,
    /// Function bodies collected from the program, keyed by function name.
    function_bodies: HashMap<String, &'a Function>,
    /// Impl method bodies: "TypeName.method" -> &Function.
    method_bodies: HashMap<String, &'a Function>,
    /// Bodies of `let`-bound closures, keyed by the BINDING name
    /// (`let mut append = |s| { buf.push_str(s); }` -> "append").
    ///
    /// B-2026-08-08-17. Calling such a binding mutates whatever the closure
    /// CAPTURED, and captures are invisible at the call site: `append(s)` has
    /// no `mut` marker and no declared `mut ref` parameter, so the `Call` arm
    /// of [`Self::collect_expr_inner_writes`] recorded no write at all. The
    /// auto-parallelizer then saw `append(s)` and `println(buf.len())` as two
    /// independent reads, split them across branches, and captured `buf` into
    /// the par env BY VALUE — so the closure's write landed in the real
    /// binding while the sibling branch printed a stale snapshot. Exactly the
    /// kata-22 failure the `Call` arm's own comment describes for `mut ref`
    /// params, one level over: through a capture instead of an argument.
    ///
    /// Keyed program-wide by binding name, like `function_bodies`. A same-named
    /// closure in another function can therefore contribute its writes here
    /// too; that only ever ADDS serialization, which is the safe direction.
    closure_bodies: HashMap<String, &'a Expr>,
    /// Closure binding names whose bodies are currently being expanded by
    /// [`Self::collect_expr_inner_writes`], so a cycle (`a` calls `b` calls
    /// `a`) terminates. Re-entering a name already on the stack is skipped:
    /// the outer expansion of that same closure has already collected its
    /// body's writes, so nothing is lost.
    closure_expansion_stack: std::cell::RefCell<Vec<String>>,
    /// Type info (when available). Its `method_callee_types` map (receiver type
    /// name per method-call span) drives method-receiver classification for
    /// A2b-2 Phase 2 Slice 2 (method-call network fan-out). `None` disables it.
    types: Option<&'a TypeCheckResult>,
    /// Names of `shared struct` / `shared enum` (RC) types declared in this
    /// program. A2b-2 Phase 2 Slice 2: a method receiver of a shared type can
    /// ALIAS another binding (`let b = a` clones the RC handle), so two method
    /// calls on distinct-named shared receivers may still hit the same object —
    /// they are excluded from method-call fan-out.
    shared_type_names: HashSet<String>,
    /// Transitive closure of user types whose drop is OBSERVABLE — a user
    /// `impl Drop` body runs when a value of the type (or one reachable
    /// through its declared fields / enum payloads / tuples / arrays /
    /// generic args) is displaced or scope-dropped. Seeded from
    /// `program.drop_method_keys` and closed over `StructDef` fields and
    /// `EnumDef` variant payloads at construction. Drives the
    /// never-a-dead-write widening in `collect_container_locals`
    /// (B-2026-07-31-41): a lost branch-local mutation of such a binding is
    /// never dead, because the parent's scope-exit fire reads the value the
    /// branch never published — wrapper types (`Box2.Full(Res{..})`) diverge
    /// exactly like the direct `Res` binding did. Non-owning type formers
    /// (refs, slices, weak, raw pointers, fn types) do not propagate.
    drop_observable_type_names: HashSet<String>,
}

impl<'a> ConcurrencyChecker<'a> {
    pub fn new(
        program: &'a Program,
        effects: &'a EffectCheckResult,
        types: Option<&'a TypeCheckResult>,
    ) -> Self {
        let shared_type_names = program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::StructDef(s) if s.is_shared => Some(s.name.clone()),
                Item::EnumDef(e) if e.is_shared => Some(e.name.clone()),
                _ => None,
            })
            .collect();
        let drop_observable_type_names = Self::compute_drop_observable_types(program);
        let mut checker = ConcurrencyChecker {
            program,
            effects,
            function_bodies: HashMap::new(),
            method_bodies: HashMap::new(),
            closure_bodies: HashMap::new(),
            closure_expansion_stack: std::cell::RefCell::new(Vec::new()),
            types,
            shared_type_names,
            drop_observable_type_names,
        };
        checker.collect_functions();
        checker
    }

    /// Build the transitive drop-observable closure — see the field doc on
    /// [`Self::drop_observable_type_names`]. Fixpoint over declared items:
    /// seed with every `impl Drop` type, then admit any struct whose field
    /// (or enum whose variant payload) MENTIONS a member through an owning
    /// type former (path head, generic args, tuples, arrays). Terminates:
    /// each round either adds a declared type name or stops, and the name
    /// universe is finite.
    fn compute_drop_observable_types(program: &Program) -> HashSet<String> {
        fn te_mentions(te: &TypeExpr, set: &HashSet<String>) -> bool {
            match &te.kind {
                TypeKind::Path(p) => {
                    p.segments.last().is_some_and(|s| set.contains(s))
                        || p.generic_args.iter().flatten().any(|a| match a {
                            GenericArg::Type(t) => te_mentions(t, set),
                            _ => false,
                        })
                }
                TypeKind::Tuple(elems) => elems.iter().any(|t| te_mentions(t, set)),
                TypeKind::Array { element, .. } => te_mentions(element, set),
                // Non-owning formers: a borrow/view/weak/raw-pointer/fn
                // field never runs the pointee's drop, so it does not make
                // the enclosing type drop-observable.
                _ => false,
            }
        }
        let mut set: HashSet<String> = program.drop_method_keys.keys().cloned().collect();
        loop {
            let mut changed = false;
            for item in &program.items {
                match item {
                    Item::StructDef(s) if !set.contains(&s.name) => {
                        if s.fields.iter().any(|f| te_mentions(&f.ty, &set)) {
                            set.insert(s.name.clone());
                            changed = true;
                        }
                    }
                    Item::EnumDef(e) if !set.contains(&e.name) => {
                        let hit = e.variants.iter().any(|v| match &v.kind {
                            VariantKind::Tuple(tes) => tes.iter().any(|t| te_mentions(t, &set)),
                            VariantKind::Struct(fields) => {
                                fields.iter().any(|f| te_mentions(&f.ty, &set))
                            }
                            VariantKind::Unit => false,
                        });
                        if hit {
                            set.insert(e.name.clone());
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
            if !changed {
                break;
            }
        }
        set
    }

    /// Record every `let <name> = <closure>` in `block`, at any depth, into
    /// `out`. B-2026-08-08-17 — see the field doc on `closure_bodies`.
    ///
    /// Walks nested blocks so a closure defined inside an `if` / loop body is
    /// found too; a call to it can still be grouped against a read of what it
    /// captures. Re-binding the same name in two scopes keeps the FIRST body
    /// here, which is fine for the purpose: this map only ever adds writes, and
    /// missing a re-bound sibling costs serialization, never soundness.
    fn collect_closure_bindings(block: &'a Block, out: &mut HashMap<String, &'a Expr>) {
        for stmt in &block.stmts {
            if let StmtKind::Let { pattern, value, .. } = &stmt.kind {
                if let (PatternKind::Binding(name), ExprKind::Closure { body, .. }) =
                    (&pattern.kind, &value.kind)
                {
                    out.entry(name.clone()).or_insert(body);
                }
            }
            // Explicit walk rather than `rc_elide::walk_stmt_children_pub`:
            // that helper hands out `&Expr` with an anonymous lifetime, and the
            // map stores `&'a Expr`.
            match &stmt.kind {
                StmtKind::Let { value, .. } => Self::collect_closure_bindings_in_expr(value, out),
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    Self::collect_closure_bindings_in_expr(value, out);
                    Self::collect_closure_bindings(else_block, out);
                }
                StmtKind::Expr(e) => Self::collect_closure_bindings_in_expr(e, out),
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    Self::collect_closure_bindings(body, out)
                }
                StmtKind::Assign { target, value } => {
                    Self::collect_closure_bindings_in_expr(target, out);
                    Self::collect_closure_bindings_in_expr(value, out);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    Self::collect_closure_bindings_in_expr(target, out);
                    Self::collect_closure_bindings_in_expr(value, out);
                }
                StmtKind::MultiAssign { targets, values } => {
                    for t in targets {
                        Self::collect_closure_bindings_in_expr(t, out);
                    }
                    for v in values {
                        Self::collect_closure_bindings_in_expr(v, out);
                    }
                }
                StmtKind::LetUninit { .. } => {}
            }
        }
        if let Some(tail) = &block.final_expr {
            Self::collect_closure_bindings_in_expr(tail, out);
        }
    }

    fn collect_closure_bindings_in_expr(expr: &'a Expr, out: &mut HashMap<String, &'a Expr>) {
        match &expr.kind {
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) | ExprKind::Par(b) => {
                Self::collect_closure_bindings(b, out)
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                Self::collect_closure_bindings_in_expr(condition, out);
                Self::collect_closure_bindings(then_block, out);
                if let Some(e) = else_branch {
                    Self::collect_closure_bindings_in_expr(e, out);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                Self::collect_closure_bindings_in_expr(value, out);
                Self::collect_closure_bindings(then_block, out);
                if let Some(e) = else_branch {
                    Self::collect_closure_bindings_in_expr(e, out);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                Self::collect_closure_bindings_in_expr(condition, out);
                Self::collect_closure_bindings(body, out);
            }
            ExprKind::WhileLet { value, body, .. } => {
                Self::collect_closure_bindings_in_expr(value, out);
                Self::collect_closure_bindings(body, out);
            }
            ExprKind::Loop { body, .. } | ExprKind::For { body, .. } => {
                Self::collect_closure_bindings(body, out)
            }
            ExprKind::Match { scrutinee, arms } => {
                Self::collect_closure_bindings_in_expr(scrutinee, out);
                for arm in arms {
                    Self::collect_closure_bindings_in_expr(&arm.body, out);
                }
            }
            _ => {}
        }
    }

    fn collect_functions(&mut self) {
        // Closure bindings first, so the `Call` arm can resolve a callee that
        // names a closure defined anywhere in the program.
        let mut closures: HashMap<String, &'a Expr> = HashMap::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => Self::collect_closure_bindings(&f.body, &mut closures),
                Item::ImplBlock(imp) => {
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            Self::collect_closure_bindings(&m.body, &mut closures);
                        }
                    }
                }
                _ => {}
            }
        }
        self.closure_bodies = closures;
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    self.function_bodies.insert(f.name.clone(), f);
                }
                Item::ImplBlock(imp) => {
                    let type_name = match &imp.target_type.kind {
                        TypeKind::Path(p) => match p.segments.last().cloned() {
                            Some(name) => name,
                            None => continue,
                        },
                        _ => continue,
                    };
                    for item in &imp.items {
                        if let ImplItem::Method(method) = item {
                            let key = format!("{}.{}", type_name, method.name);
                            self.method_bodies.insert(key, method);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn analyze(self) -> ConcurrencyAnalysis {
        let mut decisions = HashMap::new();

        for item in &self.program.items {
            if let Item::Function(f) = item {
                let fc = self.analyze_function(f);
                decisions.insert(f.name.clone(), fc);
            }
        }

        // Also analyze impl methods
        for item in &self.program.items {
            if let Item::ImplBlock(imp) = item {
                let type_name = match &imp.target_type.kind {
                    TypeKind::Path(p) => match p.segments.last().cloned() {
                        Some(name) => name,
                        None => continue,
                    },
                    _ => continue,
                };
                for impl_item in &imp.items {
                    if let ImplItem::Method(method) = impl_item {
                        let key = format!("{}.{}", type_name, method.name);
                        let fc = self.analyze_function(method);
                        decisions.insert(key, fc);
                    }
                }
            }
        }

        ConcurrencyAnalysis {
            function_decisions: decisions,
            queries: Vec::new(),
        }
    }

    fn analyze_function(&self, func: &Function) -> FunctionConcurrency {
        let stmts = &func.body.stmts;
        let total_statements = stmts.len();

        // B-2026-07-16-10: a function containing any user `defer` / `errdefer`
        // is not auto-parallelized — the par_run whole-function lowering does
        // not preserve LIFO-at-scope-exit defer semantics (it emits function-
        // scope defers FIFO-inline). Return an empty decision so codegen falls
        // back to the sequential lowering, which drains defers correctly. See
        // `block_has_user_defer` for the full rationale. (Explicit `par {}` is
        // a separate codegen path and is unaffected.)
        // B-2026-08-14-24 — `stmts.is_empty()` is NOT "the body is empty". A
        // block's TAIL EXPRESSION is part of the body, so a function written as
        // `fn f(..) { if go { for .. } }` has zero STATEMENTS and one tail `if`,
        // and this early return handed back an all-empty decision before any
        // lane ran. That is the `total_statements: 0` a `karac query
        // concurrency` reported for a function that plainly contains a loop, and
        // why its loop was ABSENT from `--concurrency-report` rather than listed
        // with a decline reason — indistinguishable, to an author, from a
        // function with no loop in it.
        //
        // `total_statements` itself keeps counting statements: the parallel-group
        // lane indexes into `stmts`, and a tail expression is not one of those.
        // Only the "nothing here at all" test is corrected.
        if (total_statements == 0 && func.body.final_expr.is_none())
            || block_has_user_defer(&func.body)
        {
            return FunctionConcurrency {
                parallel_groups: Vec::new(),
                total_statements,
                statement_spans: Vec::new(),
                loop_reductions: Vec::new(),
                declined_par_loops: Vec::new(),
                disjoint_write_loops: Vec::new(),
                serialization_points: Vec::new(),
                reorder_opportunities: Vec::new(),
            };
        }

        // The enclosing fn's `ref`/`mut ref` parameter names — a method-call
        // receiver rooted at one may be caller-aliased, so it is excluded from
        // method-call network fan-out (Slice 2). `mut Slice` params are borrows
        // too but never name a method receiver of interest; included for parity
        // with `param_is_borrow`.
        let mut ref_params: HashSet<String> = HashSet::new();
        for p in &func.params {
            if matches!(
                p.ty.kind,
                TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
            ) {
                self.collect_pattern_bindings(&p.pattern, &mut ref_params);
            }
        }

        // Step 1: Extract metadata for each statement
        let stmt_infos: Vec<StmtInfo> = stmts
            .iter()
            .map(|s| self.analyze_stmt(s, false, &ref_params))
            .collect();

        // Step 2: Build the conflict graph + the serialization-point list
        // (the inverse of the parallel groups: for every conflicting pair,
        // *why* they can't parallelize + which callee's effect is to blame).
        // Uses a sparse inverted index rather than a dense O(n²) matrix — see
        // `build_conflict_graph` / [`ConflictGraph`].
        let (graph, serialization_points) = self.build_conflict_graph(&stmt_infos);

        // Step 3: Find maximal independent sets (greedy graph coloring approach)
        // We group statements that have no edges between them.
        // Names of locals whose declared/recorded type is a heap-owning
        // container — feeds `captured_container_mutations` (B-2026-07-15-2).
        let container_locals = self.collect_container_locals(&func.body);
        // B-2026-07-16-19: per-stmt consuming reads of move-hazard locals.
        // A statement that MOVES heap ownership out of a binding it captured
        // (a `match r { Some(w) => .. }` on an `Option[String]`, a bare owned
        // heap arg to a consuming callee, `let y = s;`) must not enter a par
        // group: the branch env bit-copies the binding, the branch's move
        // machinery suppresses/frees only its LOCAL copy, and the parent's
        // scope-exit cleanup still fires on the original — a double-free the
        // stmt-vs-stmt conflict graph cannot see (the hazard is stmt-vs-
        // scope-exit, not stmt-vs-stmt).
        let move_hazards = self.collect_move_hazard_locals(&func.body);
        let consuming_hazard_reads: Vec<HashSet<String>> = stmts
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut set = self.stmt_consuming_hazard_reads(s, &move_hazards, true);
                // Names the stmt itself introduces are its own to consume —
                // the branch-local move machinery is complete for those.
                for n in &stmt_infos[i].let_introduced {
                    set.remove(n);
                }
                set
            })
            .collect();
        // B-2026-07-22-9 producer guard uses a NARROWER set: wrapper-combinator
        // consumption (`a.unwrap_or(..)`) of a published slot is round-trip-safe
        // (B-2026-07-17-4), so it must not de-parallelize the PRODUCER of that
        // binding — only genuine MOVES do. (The consumer of such a call is still
        // gated by `consuming_hazard_reads` above.)
        let moving_hazard_reads: Vec<HashSet<String>> = stmts
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut set = self.stmt_consuming_hazard_reads(s, &move_hazards, false);
                for n in &stmt_infos[i].let_introduced {
                    set.remove(n);
                }
                set
            })
            .collect();
        let parallel_groups = self.find_parallel_groups(
            &stmt_infos,
            &graph,
            total_statements,
            &container_locals,
            &consuming_hazard_reads,
            &moving_hazard_reads,
            &func.name,
        );

        // Step 4: Recognize reductions in top-level loops. Independent of
        // the parallel-group / dependency machinery — a reduction loop
        // has a loop-carried dependency that the parallel-group analysis
        // correctly serializes, but the loop's iteration space can still
        // be split across workers when the op is associative + commutative.
        let (loop_reductions, declined_par_loops) = self.recognize_reductions(func);

        // Step 4b: Recognize loops over provably-disjoint indexed writes — the
        // third compute-fan-out shape. Independent of both the parallel-group
        // machinery and the reduction classifier: `out[f(i)] = ...` has no
        // accumulator, so the reduction shapes never see it.
        let disjoint_write_loops = self.recognize_disjoint_write_loops(func);

        // Step 5: Flag parallelism left on the table purely by source
        // ordering — independent statements the contiguous-only grouper
        // could not co-group because they are non-adjacent, but a legal
        // reorder would. Advisory only; consumes the same dependency graph.
        let reorder_opportunities = self.find_reorder_opportunities(
            &stmt_infos,
            &graph,
            total_statements,
            &parallel_groups,
        );

        let statement_spans = stmts.iter().map(|s| s.span).collect();

        FunctionConcurrency {
            parallel_groups,
            total_statements,
            statement_spans,
            loop_reductions,
            declined_par_loops,
            disjoint_write_loops,
            serialization_points,
            reorder_opportunities,
        }
    }

    /// Build the sparse [`ConflictGraph`] plus the ordered
    /// serialization-point list for a function body.
    ///
    /// Instead of the former dense `O(n²)` all-pairs scan, this enumerates
    /// only *candidate* pairs — pairs that share a binding, a resource, a
    /// polymorphic-effect linkage, or a `seq` ordering — via inverted
    /// indices, since [`Self::statements_conflict`] can only return `true`
    /// for such pairs. Every candidate is then run through the exact same
    /// `statements_conflict` / `conflict_detail` predicates, so the produced
    /// edge set and serialization points are identical to the old dense
    /// build (the serialization points are re-sorted into the old
    /// outer-`i` / inner-`j` emission order for byte-stable diagnostics).
    fn build_conflict_graph(&self, infos: &[StmtInfo]) -> (ConflictGraph, Vec<SerializationPoint>) {
        let n = infos.len();

        // Inverted indices. Only pairs colliding on one of these keys can
        // ever conflict, so they bound the candidate set.
        let mut var_definers: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut var_readers: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut resource_stmts: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut seq_stmts: Vec<usize> = Vec::new();
        let mut poly_stmts: Vec<usize> = Vec::new();
        let mut effectful_stmts: Vec<usize> = Vec::new();

        for (i, info) in infos.iter().enumerate() {
            if info.is_seq {
                seq_stmts.push(i);
            }
            if info.calls_polymorphic {
                poly_stmts.push(i);
            }
            if !info.effects.is_empty() {
                effectful_stmts.push(i);
            }
            for v in &info.defines {
                var_definers.entry(v.as_str()).or_default().push(i);
            }
            for v in &info.reads {
                var_readers.entry(v.as_str()).or_default().push(i);
            }
            for e in &info.effects {
                resource_stmts
                    .entry(e.resource.as_str())
                    .or_default()
                    .push(i);
            }
        }

        // Candidate unordered pairs, stored `(lo, hi)` with `lo < hi`.
        let mut candidates: HashSet<(usize, usize)> = HashSet::new();
        let mut add = |a: usize, b: usize| {
            if a != b {
                candidates.insert((a.min(b), a.max(b)));
            }
        };

        // A `seq` statement force-serializes against *every* other statement.
        for &s in &seq_stmts {
            for other in 0..n {
                add(s, other);
            }
        }

        // Dataflow: a conflict via binding `v` requires at least one *definer*
        // of `v` (two pure readers never conflict). So pair each definer with
        // every other definer and every reader of the same binding.
        for (v, definers) in &var_definers {
            for a in 0..definers.len() {
                for b in (a + 1)..definers.len() {
                    add(definers[a], definers[b]);
                }
            }
            if let Some(readers) = var_readers.get(v) {
                for &d in definers {
                    for &r in readers {
                        add(d, r);
                    }
                }
            }
        }

        // Polymorphic calls have unknown effects: each conflicts with any
        // other polymorphic *or* effect-bearing statement.
        for a in 0..poly_stmts.len() {
            for b in (a + 1)..poly_stmts.len() {
                add(poly_stmts[a], poly_stmts[b]);
            }
        }
        for &p in &poly_stmts {
            for &e in &effectful_stmts {
                add(p, e);
            }
        }

        // Effect conflicts only arise between statements touching the *same*
        // resource (`two_effects_conflict` short-circuits on differing
        // resources).
        for stmts in resource_stmts.values() {
            for a in 0..stmts.len() {
                for b in (a + 1)..stmts.len() {
                    add(stmts[a], stmts[b]);
                }
            }
        }

        // Confirm each candidate against the exact predicate and record edges.
        let mut neighbors = vec![HashSet::new(); n];
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for &(lo, hi) in &candidates {
            if self.statements_conflict(&infos[lo], &infos[hi]) {
                neighbors[lo].insert(hi);
                neighbors[hi].insert(lo);
                edges.push((lo, hi));
            }
        }

        // Reproduce the old emission order (outer index ascending, inner index
        // ascending) so serialization-point diagnostics stay byte-stable.
        edges.sort_unstable_by_key(|&(lo, hi)| (hi, lo));
        let mut serialization_points: Vec<SerializationPoint> = Vec::new();
        for (lo, hi) in edges {
            if let Some(mut sp) = self.conflict_detail(&infos[lo], &infos[hi]) {
                sp.statement_indices = vec![lo, hi];
                serialization_points.push(sp);
            }
        }

        (ConflictGraph { neighbors }, serialization_points)
    }

    /// Analyze a single statement to extract defines, reads, and effects.
    /// `ref_params` is the set of `ref`/`mut ref` parameter names of the
    /// enclosing function (a receiver rooted at one may be caller-aliased, so
    /// it is excluded from method-call network fan-out — Slice 2).
    fn analyze_stmt(&self, stmt: &Stmt, is_seq: bool, ref_params: &HashSet<String>) -> StmtInfo {
        let mut info = StmtInfo {
            defines: HashSet::new(),
            let_introduced: HashSet::new(),
            reads: HashSet::new(),
            called_fn_names: HashSet::new(),
            effects: Vec::new(),
            calls_polymorphic: false,
            is_seq,
            has_early_exit: stmt_has_early_exit(stmt),
            has_channel_op: stmt_has_channel_op(stmt),
            has_console_output: stmt_has_console_output(stmt),
            is_timer_suspend: stmt_is_timer_suspend(stmt),
            // Set below, once `effects` is populated — it needs the effect set
            // to confirm the call touches the `Network` resource.
            is_safe_network_fanout: false,
            is_ephemeral_network_fanout: false,
            method_fanout_receiver_root: None,
            is_constant_init: stmt_is_constant_init(stmt),
        };

        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { pattern, value, .. } => {
                // A `let` whose binding is a channel end (`Sender`/`Receiver`)
                // — even when produced by a plain call whose RETURN is a
                // channel end, e.g. `let rx = after(ms)` — must stay
                // sequential: a `__par_branch` writeback would duplicate the
                // end's `DropChannelEnd`. `stmt_has_channel_op` only sees a
                // syntactic channel op, so catch the returns-a-channel-end
                // shape here. See `pattern_binds_channel_end`.
                if self.pattern_binds_channel_end(pattern) {
                    info.has_channel_op = true;
                }
                // The pattern defines variables
                self.collect_pattern_bindings(pattern, &mut info.defines);
                self.collect_pattern_bindings(pattern, &mut info.let_introduced);
                // The value expression may read variables and call functions
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
                // The RHS may also WRITE outer state as a side effect — a
                // `mut ref self` / `mut ref T` call mutates its receiver / a
                // `mut`-passed argument. Record those writes so a later stmt
                // that reads (or writes) the same place serializes against
                // this one. Without this, `let then_block = self.parse_block()`
                // recorded no write on `self`, so three sequential
                // cursor-advancing `self.parse_*()` calls looked independent
                // and the auto-parallelizer raced them (B-2026-07-09-12).
                // Mirrors the `StmtKind::Expr` arm's inner-write collection.
                self.collect_expr_inner_writes(value, &mut info.defines);
            }
            StmtKind::LetUninit { name, .. } => {
                info.defines.insert(name.clone());
                info.let_introduced.insert(name.clone());
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                // Channel-end binding guard — see the `StmtKind::Let` arm.
                if self.pattern_binds_channel_end(pattern) {
                    info.has_channel_op = true;
                }
                self.collect_pattern_bindings(pattern, &mut info.defines);
                self.collect_pattern_bindings(pattern, &mut info.let_introduced);
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
                // RHS side-effect writes (mut-ref receiver / mut arg) — see the
                // `StmtKind::Let` arm (B-2026-07-09-12).
                self.collect_expr_inner_writes(value, &mut info.defines);
                self.collect_block_reads(else_block, &mut info.reads);
                self.collect_block_effects(else_block, &mut info);
                self.collect_block_inner_writes(else_block, &mut info.defines);
            }
            StmtKind::Assign { target, value } => {
                // The target is being written to
                self.collect_assign_target_defines(target, &mut info.defines);
                // But the target may also read (e.g. array[idx] = val reads idx)
                self.collect_assign_target_reads(target, &mut info.reads);
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.collect_assign_target_defines(target, &mut info.defines);
                self.collect_assign_target_reads(target, &mut info.reads);
                // Compound assign also reads the target
                self.collect_expr_reads(target, &mut info.reads);
                self.collect_expr_reads(value, &mut info.reads);
                self.collect_expr_effects(value, &mut info);
            }
            StmtKind::Expr(expr) => {
                self.collect_expr_reads(expr, &mut info.reads);
                self.collect_expr_effects(expr, &mut info);
                // Nested Assigns (e.g. inside a `for v in nums.iter() {
                // if v > cap { cap = v; } }`) write to outer-scope
                // names — record them in `info.defines` so subsequent
                // stmts that read those names create a data dependency
                // and serialize against this stmt. Without this, a
                // for-loop body's `cap = v` is invisible to
                // `statements_conflict` and the analyzer groups stmts
                // that should be sequential.
                self.collect_expr_inner_writes(expr, &mut info.defines);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.collect_block_reads(body, &mut info.reads);
                self.collect_block_effects(body, &mut info);
                self.collect_block_inner_writes(body, &mut info.defines);
            }
        }

        // A2b-2: a network call (touches the `Network` resource) whose args
        // move in no owned binding is exempt from the coroutine-boundary gate.
        // Both halves are required: the arg-safety proves no double-drop, and
        // the Network-resource check keeps a non-network user `with suspends`
        // fn serial (its independence isn't established here). Now that
        // `info.effects` is populated, combine them.
        info.is_safe_network_fanout =
            stmt_fanout_args_safe(stmt, &self.function_bodies, &self.method_bodies)
                && info.effects.iter().any(|e| e.resource == "Network");

        // A2b-2 Phase 1: an ephemeral network fan-out is a safe network fan-out
        // whose callee borrows nothing — so it cannot share a connection object
        // with a sibling call, and its `Network` ops touch a freshly-opened,
        // private connection. Two of them may overlap; `statements_conflict`
        // relaxes the `Network`↔`Network` conflict for such pairs.
        info.is_ephemeral_network_fanout = info.is_safe_network_fanout
            && stmt_callee_has_no_borrow_params(stmt, &self.function_bodies, &self.method_bodies);

        // A2b-2 Phase 2 Slice 2: method-call network fan-out. A method call
        // `obj.method(args)` touching `Network` with a borrowed receiver of a
        // distinct-provable (non-shared, non-ref-param) local is a candidate;
        // record its receiver root for the distinct-receiver conflict relaxation.
        if info.effects.iter().any(|e| e.resource == "Network") {
            info.method_fanout_receiver_root = self.classify_method_fanout(stmt, ref_params);
        }

        info
    }

    /// A2b-2 Phase 2 Slice 2: classify a statement as a method-call network
    /// fan-out CANDIDATE, returning its receiver ROOT identifier if admissible.
    /// Requires (all fail-closed to `None`): the statement is `obj.method(args)`
    /// whose (1) receiver `obj` is a plain identifier that is NOT a `ref`/`mut ref`
    /// parameter of the enclosing fn (a ref param may be caller-aliased); (2)
    /// receiver type — from `method_callee_types` — is NOT a `shared` (RC) type
    /// (which can alias via `let b = a`); (3) resolved method BORROWS its receiver
    /// (`ref self`/`mut ref self`, never a consuming `own self` that would move it
    /// into the coroutine and double-drop); and (4) args are fan-out-safe (same
    /// rule as `stmt_fanout_args_safe`). The Network-resource check is applied by
    /// the caller. Needs type info; `None` without it.
    fn classify_method_fanout(&self, stmt: &Stmt, ref_params: &HashSet<String>) -> Option<String> {
        let value = match &stmt.kind {
            StmtKind::Let { value, .. } | StmtKind::Expr(value) => value,
            _ => return None,
        };
        let ExprKind::MethodCall {
            object,
            args,
            method,
            ..
        } = &value.kind
        else {
            return None;
        };
        // (1) Receiver is a plain local identifier, not a ref/mut-ref param.
        let ExprKind::Identifier(root) = &object.kind else {
            return None;
        };
        if ref_params.contains(root) {
            return None;
        }
        // (2) Receiver type is known (from `method_callee_types`, keyed by the
        // method-call span — which equals the receiver span) and NOT shared.
        // The stored value is the full `"TypeName.method"` key.
        let key = self
            .types?
            .method_callee_types
            .get(&SpanKey::from_span(&value.span))?;
        let recv_ty = key.rsplit_once('.').map(|(t, _)| t)?;
        if self.shared_type_names.contains(recv_ty) {
            return None;
        }
        let _ = method; // method identity is carried by `key`; kept for clarity
                        // (3) Resolved method borrows its receiver (not consuming).
        let func = self.method_bodies.get(key)?;
        if !matches!(
            func.self_param,
            Some(SelfParam::Ref) | Some(SelfParam::MutRef)
        ) {
            return None;
        }
        // (4) Args fan-out-safe: literal / const, or an identifier at a borrow
        // parameter position (mirrors `stmt_fanout_args_safe`).
        let callee_params = Some(func.params.as_slice());
        let args_safe = args.iter().enumerate().all(|(i, a)| {
            expr_is_call_free(&a.value)
                && (expr_is_binding_free(&a.value)
                    || (matches!(a.value.kind, ExprKind::Identifier(_))
                        && param_is_borrow(callee_params, i)))
        });
        if !args_safe {
            return None;
        }
        Some(root.clone())
    }

    #[allow(clippy::too_many_arguments)] // B-2026-07-22-9 fix threads a second (producer-only) hazard set
    fn find_parallel_groups(
        &self,
        infos: &[StmtInfo],
        graph: &ConflictGraph,
        n: usize,
        container_locals: &HashSet<String>,
        consuming_hazard_reads: &[HashSet<String>],
        moving_hazard_reads: &[HashSet<String>],
        enclosing_fn: &str,
    ) -> Vec<ParallelGroup> {
        let mut groups: Vec<ParallelGroup> = Vec::new();
        let mut assigned = vec![false; n];

        // B-2026-07-22-9: a statement that PRODUCES a move-hazard binding
        // later consumed by a MOVE cannot be auto-parallelized either — the
        // dual of the consumer guard below (B-2026-07-16-19). When the
        // producer runs in a `__par_branch` worker, its owned heap value is
        // written back to the parent frame via the par-return slot (a
        // bit-copy of the {tag,ptr,len,cap} header); a later sequential
        // `let c = a` / owned-arg move of that binding then double-frees —
        // the move's source-null and the writeback copy don't compose, so
        // both the moved-into binding's scope-exit drop and the residual
        // writeback copy free the same buffer. De-parallelizing only the
        // CONSUMER (line ~3763) is insufficient: the consumer already runs
        // in the sequential tail there, yet the PRODUCER stays grouped and
        // the double-free still fires. The proven-broken repro is a
        // String-payload enum temp describe() sibling (which seeds the
        // group) beside a `let a = mk_nums()` Vec-payload producer whose `a`
        // is then `let c = a`-moved; either alone is clean (no group forms),
        // only their coexistence parallelizes the producer. Sequential is
        // always correct; auto-par is only an optimization.
        let mut hazard_producer = vec![false; n];
        for i in 0..n {
            let produces_consumed_hazard = infos[i].let_introduced.iter().any(|name| {
                // Consumed-by-MOVE by any LATER statement (the writeback +
                // move race is strictly forward: the producer's value is
                // handed to the parent, then a subsequent stmt moves it).
                // Uses `moving_hazard_reads`, not `consuming_hazard_reads`:
                // wrapper-combinator consumption of a published slot is safe
                // (B-2026-07-17-4) and must not de-parallelize the producer.
                ((i + 1)..n).any(|j| moving_hazard_reads[j].contains(name))
            });
            hazard_producer[i] = produces_consumed_hazard;
        }

        // For each unassigned statement, try to build a maximal parallel group
        for start in 0..n {
            if assigned[start] {
                continue;
            }

            // B-2026-07-22-9 seed guard: see `hazard_producer` above.
            if hazard_producer[start] {
                assigned[start] = true;
                continue;
            }

            // A statement that may exit the function early (contains `return`,
            // `break`, or `continue`) cannot share a par group with any
            // sibling — the par branch's `void` LLVM signature can't carry
            // the inner `ret X` and module verification fails.
            if infos[start].has_early_exit {
                assigned[start] = true;
                continue;
            }

            // A statement that calls a coroutine network-boundary fn — or any
            // `suspends` park that is not a standalone `sleep_ms` timer wait —
            // must NOT be auto-parallelized into a `__par_branch` worker: a
            // coroutine owns + drops its by-value params while auto-par captures
            // are shared-with-write-back (a `__par_branch`-lifted call would
            // double-drop an owned user-`Drop` arg), and a channel `recv` has a
            // happens-before with its producer that a fan-out would deadlock.
            // A direct `sleep_ms` timer park is the one independent `suspends`
            // form, so it is exempted and overlaps like `blocks` (A2b). See
            // `effects_mark_coroutine_boundary` / `stmt_is_timer_suspend`.
            if effects_mark_coroutine_boundary(&infos[start].effects)
                && !infos[start].is_timer_suspend
                && !infos[start].is_safe_network_fanout
                && infos[start].method_fanout_receiver_root.is_none()
            {
                assigned[start] = true;
                continue;
            }

            // A channel operation (`Channel.new` / `send` / `recv` /
            // `try_recv`) is never auto-parallelized — channels are explicit
            // communication primitives whose send-before-recv ordering a
            // par fan-out would break (and whose channel-end bindings would
            // be isolated into a branch's captured scope). Mirrors the
            // early-exit / coroutine-boundary seed guards. See
            // `stmt_has_channel_op`.
            if infos[start].has_channel_op {
                assigned[start] = true;
                continue;
            }

            // B-2026-07-16-19: a statement that CONSUMES a move-hazard local
            // captured from outside itself (moves heap ownership out of a
            // `match`/`if let` payload, an owned call arg, a bare-RHS alias)
            // must not run in a par-branch worker: the branch's move
            // machinery suppresses/frees only the branch's bit-copied env
            // alloca, while the parent's scope-exit cleanup still fires on
            // the original — a double-free the stmt-vs-stmt conflict graph
            // cannot model (the conflicting "read" is the parent's implicit
            // scope-exit drop, not a sibling statement). Sequential is
            // always correct; auto-par is only an optimization.
            if !consuming_hazard_reads[start].is_empty() {
                assigned[start] = true;
                continue;
            }

            // Console-output statements (`println` / `print` / `eprintln` / a
            // `Stdout`/`Stderr` write) are NOT suppressed here. They carry no
            // resource effect, so the conflict gate treats them as independent
            // and they fan out — but the runtime captures each branch's output
            // and replays it in branch (= source) order at the join
            // (`karac_par_run`'s ordered-output capture), so observable output
            // is byte-identical to sequential execution. This reverses
            // B-2026-06-13-18's blanket suppression, which traded away the
            // parallelism of logging-bearing independent work (the Parallax
            // demo's per-fetch trace) to avoid the race the buffering now
            // eliminates. See phase-6-runtime.md "Auto-par ordered output".

            let mut group_indices = vec![start];
            assigned[start] = true;

            // Try to add subsequent unassigned statements to this group.
            //
            // **Contiguous-only invariant.** A parallel group must be a
            // contiguous run of statements: code before the group runs
            // sequentially, the group fans out at one point through
            // `karac_par_run`, then code after the group runs
            // sequentially. Non-contiguous groups violate this — they
            // imply two interleaved fan-outs that the single-fan-out
            // runtime cannot express, and the codegen's
            // `i = max_idx + 1` step would skip past the second
            // group's stmts entirely. So when a candidate isn't
            // independent of the in-progress group, we **break**, not
            // continue — the group ends here and any later eligible
            // candidate becomes the seed of its own group.
            for candidate in (start + 1)..n {
                if assigned[candidate] {
                    break;
                }

                // Same rule applied to candidates: an early-exit stmt
                // ends the par group at its sibling boundary.
                if infos[candidate].has_early_exit {
                    break;
                }

                // A coroutine network-boundary statement (or a non-timer
                // `suspends` park) is never auto-parallelized — it must not join
                // a group seeded by a pure sibling either (see the seed-side
                // guard above). A direct `sleep_ms` timer wait is exempt and may
                // join. End the group at any other boundary.
                if effects_mark_coroutine_boundary(&infos[candidate].effects)
                    && !infos[candidate].is_timer_suspend
                    && !infos[candidate].is_safe_network_fanout
                    && infos[candidate].method_fanout_receiver_root.is_none()
                {
                    break;
                }

                // A channel-op statement ends the group at its sibling
                // boundary too (seed-side guard's candidate mirror).
                if infos[candidate].has_channel_op {
                    break;
                }

                // Consuming read of a move-hazard capture ends the group too
                // (seed-side guard's candidate mirror, B-2026-07-16-19).
                if !consuming_hazard_reads[candidate].is_empty() {
                    break;
                }

                // A move-hazard PRODUCER whose binding is consumed-by-move
                // later ends the group too (seed-side guard's candidate
                // mirror, B-2026-07-22-9).
                if hazard_producer[candidate] {
                    break;
                }

                // Console-output statements may JOIN a group (no candidate-side
                // break): the runtime's ordered-output capture preserves their
                // program-order observability across the fan-out. See the
                // seed-side note above and phase-6-runtime.md.

                // Check if candidate is independent of ALL statements already in the group
                let independent = group_indices
                    .iter()
                    .all(|&g| !graph.conflicts(candidate, g));

                if independent {
                    group_indices.push(candidate);
                    assigned[candidate] = true;
                } else {
                    break;
                }
            }

            // SELF-RECURSION gate (B-2026-07-15-4): a group whose statement
            // calls the enclosing function is a recursive divide-and-conquer
            // (`let left = build(..); let right = build(..)`). Auto-par
            // spawns per call with no sequential cutoff, so EVERY recursion
            // level re-dispatches (~70µs each, O(nodes) dispatches per
            // top-level call) — measured 175x wall-time regression on a
            // 15-node tree build at 20k reps, sys-time-dominated, identical
            // output. Until a work-stealing scheduler with a lazy sequential
            // cutoff exists, these groups run sequentially. Direct
            // self-calls only (bare fn name / method name); mutual recursion
            // through a helper is a documented residual.
            let is_self_recursive = group_indices
                .iter()
                .any(|&i| infos[i].called_fn_names.contains(enclosing_fn));

            // Only emit groups with more than 1 statement (parallelism requires >= 2)
            if group_indices.len() > 1 && !is_self_recursive {
                let reason = self.describe_group_reason(infos, &group_indices);
                // A group is trivial when running it in parallel can produce
                // no measurable speedup, so the `karac_par_run` spawn cost
                // (~70μs per dispatch on macOS) is pure overhead. Two cases:
                //
                // 1. All stmts are pure (no effects, no polymorphic calls) —
                //    the codegen could eliminate them, no point parallelizing.
                // 2. At most one stmt does meaningful work — the rest are
                //    constant-init lets/assigns that produce ~zero work for
                //    a par branch. The structural parallelism is zero (one
                //    branch holds all the work, the others idle through a
                //    join). Surfaced by the kata 6 zigzag bench 2026-05-17,
                //    where `convert_off` was forking three par groups per
                //    call (each shaped "one big loop + N let-binds"), adding
                //    2.2s of system-call time over 10K calls for no speedup.
                // Console writes do not count against purity here — see
                // `is_console_resource`. A statement whose only effect is a
                // print did no measurable work before B-2026-08-23-8 seeded
                // that effect, and parallelizing it still buys nothing.
                let all_pure = group_indices.iter().all(|&i| {
                    infos[i]
                        .effects
                        .iter()
                        .all(|e| is_console_resource(&e.resource))
                        && !infos[i].calls_polymorphic
                });
                let non_constant_count = group_indices
                    .iter()
                    .filter(|&&i| !infos[i].is_constant_init)
                    .count();
                let is_trivial = all_pure || non_constant_count <= 1;
                // Union of (defines − let_introduced) across the group's
                // stmts. Names in this set name *captured* locals that
                // some branch will mutate without introducing them as a
                // fresh binding — the codegen needs this to bail when
                // those mutations would otherwise be lost across the
                // par-run join.
                let mut captured_mutations: HashSet<String> = HashSet::new();
                for &i in &group_indices {
                    for name in infos[i].defines.difference(&infos[i].let_introduced) {
                        captured_mutations.insert(name.clone());
                    }
                }
                let captured_container_mutations = captured_mutations
                    .intersection(container_locals)
                    .cloned()
                    .collect();
                groups.push(ParallelGroup {
                    statement_indices: group_indices,
                    reason,
                    is_trivial,
                    captured_mutations,
                    captured_container_mutations,
                });
            }
        }

        groups
    }

    /// Flag parallelism the *contiguous-only* grouper leaves on the table:
    /// pairs of mutually-independent statements that did not co-group only
    /// because they are non-adjacent in source order, where a legal reorder
    /// (one permitted by the data + effect dependency graph) would make them
    /// adjacent. This is the deterministic "a better order exists" advisory
    /// for the agent-driven reorder loop (phase-5-diagnostics.md option 1):
    /// instead of *guessing* that a reorder helps, the agent reads a sound
    /// dependency signal, applies it, and re-runs `check` / `query` to
    /// confirm. No transformation happens here.
    ///
    /// A pair `(i, j)`, `i < j`, is reported when:
    /// - they are independent (`!graph.conflicts(i, j)`) — they *could* run in
    ///   parallel;
    /// - they are non-adjacent (`j > i + 1`) — adjacency is what the grouper
    ///   already exploits, so only a gap represents missed parallelism;
    /// - at least one of them is currently **serial** (not in a multi-stmt
    ///   parallel group) — so acting on it adds parallelism rather than just
    ///   reshuffling two already-parallel statements;
    /// - both are parallel-eligible (the same seed guards `find_parallel_groups`
    ///   applies: not an early-exit / channel-op / non-timer coroutine boundary
    ///   / `seq` statement, and not a syntactic console write — see
    ///   [`reorder_eligible`]); and
    /// - a legal slide exists: either `j` moves left past every intervening
    ///   statement (each independent of `j`) or `i` moves right past them
    ///   (each independent of `i`). Each pairwise adjacent swap is between
    ///   independent statements, so the whole slide preserves data + effect
    ///   ordering.
    ///
    /// Soundness scope: the slide is proven safe against data + resource-effect
    /// dependencies (the conflict graph). Observable console-output ordering
    /// is resourceless and only filtered syntactically (`has_console_output`);
    /// output emitted transitively inside a callee is not modeled — the
    /// agent's verification loop is the backstop, as for any source reorder.
    fn find_reorder_opportunities(
        &self,
        infos: &[StmtInfo],
        graph: &ConflictGraph,
        n: usize,
        groups: &[ParallelGroup],
    ) -> Vec<ReorderOpportunity> {
        // A statement is "serial" unless it sits in an emitted (multi-stmt)
        // parallel group.
        let mut grouped = vec![false; n];
        for g in groups {
            for &idx in &g.statement_indices {
                grouped[idx] = true;
            }
        }

        let mut out = Vec::new();
        for i in 0..n {
            if !reorder_eligible(&infos[i]) {
                continue;
            }
            // `j > i + 1`: adjacent independents are already the grouper's job.
            for j in (i + 2)..n {
                if !reorder_eligible(&infos[j]) {
                    continue;
                }
                // Must be independent to ever parallelize.
                if graph.conflicts(i, j) {
                    continue;
                }
                // Both already parallel → reshuffling them adds nothing.
                if grouped[i] && grouped[j] {
                    continue;
                }
                // A legal slide makes them adjacent. `j` slides left past
                // (i, j) iff each intervening stmt is independent of `j`;
                // symmetrically for `i` sliding right.
                let between = (i + 1)..j;
                let j_slides_left = between.clone().all(|k| !graph.conflicts(j, k));
                let i_slides_right = between.clone().all(|k| !graph.conflicts(i, k));
                let movable = if j_slides_left {
                    j
                } else if i_slides_right {
                    i
                } else {
                    continue;
                };
                let stationary = if movable == j { i } else { j };
                let reason = format!(
                    "statements {i} and {j} are independent but separated by \
                     {} intervening statement{}; moving statement {movable} adjacent \
                     to statement {stationary} would let them parallelize",
                    j - i - 1,
                    if j - i - 1 == 1 { "" } else { "s" },
                );
                out.push(ReorderOpportunity {
                    statement_indices: vec![i, j],
                    movable_statement: movable,
                    reason,
                });
            }
        }
        out
    }

    /// Generate a human-readable reason for why a group of statements can be parallelized.
    fn describe_group_reason(&self, infos: &[StmtInfo], indices: &[usize]) -> String {
        let all_pure = indices.iter().all(|&i| infos[i].effects.is_empty());
        if all_pure {
            return "pure computations".to_string();
        }

        // Check if they all read different resources
        let mut all_resources: Vec<&str> = Vec::new();
        let mut has_reads_only = true;
        for &i in indices {
            for eff in &infos[i].effects {
                if !matches!(eff.verb, EffectVerbKind::Reads) {
                    has_reads_only = false;
                }
                all_resources.push(&eff.resource);
            }
        }

        if has_reads_only {
            // Check if same or different resources
            let unique: HashSet<&&str> = all_resources.iter().collect();
            if unique.len() > 1 {
                return "independent reads on different resources".to_string();
            }
            return "concurrent reads on same resource".to_string();
        }

        // Check if effects are on different resources
        let unique_resources: HashSet<&str> = all_resources.iter().copied().collect();
        if unique_resources.len() == all_resources.len() && unique_resources.len() > 1 {
            return "independent effects on different resources".to_string();
        }

        "no data or effect dependencies".to_string()
    }
}
