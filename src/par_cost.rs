//! Auto-par **cost model** — plain data, no LLVM.
//!
//! Extracted from `src/codegen/reduce.rs` (B-2026-07-29-33) so that BOTH
//! codegen and the analysis/query side can ask the same question and get the
//! same answer.
//!
//! ## Why this module exists
//!
//! `codegen/reduce.rs` is behind `#[cfg(feature = "llvm")]`, but
//! `karac query concurrency` runs without that feature — so the query
//! physically could not reach the gates that decide whether a recognized
//! reduction actually fans out. It therefore reported the *analysis* decision
//! and disclaimed the rest (`"cost_gate":"deferred_to_codegen"`,
//! B-2026-07-29-29), which is honest but incomplete: the query could not
//! answer "did this loop fan out."
//!
//! The alternative — reimplementing the gates on the analysis side — was
//! rejected outright. A second copy of a *calibrated* cost model drifts from
//! the first, and a drifted copy makes the query confidently wrong, which is
//! worse than the disclaimer it replaces. One definition, two callers.
//!
//! ## Contents
//!
//! Everything here is a pure AST walk over `crate::ast` types — no `inkwell`,
//! no LLVM values, nothing `self`-bound to a codegen context. That property is
//! what made the move possible and is worth preserving: it keeps the
//! codegen-containment invariant (`CLAUDE.md` § Architecture) intact, because
//! the analysis side consumes plain data rather than reaching into codegen.
//!
//! - The calibrated constants (dispatch overhead, assumed worker count, the
//!   dispatch threshold, the variable-K floor).
//! - `const_eval_iter_count` / `const_eval_int_literal` — literal trip counts.
//! - `CostEstimator` / `estimate_body_cost_units` — per-iteration body cost.
//! - `MemoryBoundDetector` / `body_is_memory_bound` — the memory-bandwidth gate.
//! - [`fanout_verdict`] — the gate *sequence*, which is the part both callers
//!   need to agree on.

use crate::ast::{
    BinOp, Block, CompoundOp, Expr, ExprKind, Function, Item, PatternKind, Program, Stmt, StmtKind,
};
use std::collections::HashMap;

/// Why a recognized loop reduction does or does not become a parallel
/// fan-out. Mirrors the gate order in `codegen/reduce.rs`; see
/// [`fanout_verdict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanoutVerdict {
    /// Dispatched to `karac_par_reduce` across the worker pool.
    Fanout,
    /// Body is memory-bandwidth-bound: splitting it does not reduce wall
    /// time but does pay dispatch overhead plus extra user-CPU.
    DeclinedMemoryBound,
    /// Statically known trip count × per-iteration cost falls below the
    /// dispatch threshold — sequential wins.
    DeclinedBelowCostThreshold,
    /// Variable trip count, a body cheaper than one nested loop's worth of
    /// work, AND a bound that references a function parameter — the
    /// reusable-helper shape of B-2026-07-23-25.
    DeclinedVariableKParamBound,
}

impl FanoutVerdict {
    /// True only for [`FanoutVerdict::Fanout`].
    pub fn is_fanout(self) -> bool {
        matches!(self, FanoutVerdict::Fanout)
    }

    /// Stable machine-readable tag for `karac query concurrency`.
    pub fn tag(self) -> &'static str {
        match self {
            FanoutVerdict::Fanout => "fanout",
            FanoutVerdict::DeclinedMemoryBound => "declined_memory_bound",
            FanoutVerdict::DeclinedBelowCostThreshold => "declined_below_cost_threshold",
            FanoutVerdict::DeclinedVariableKParamBound => "declined_variable_k_param_bound",
        }
    }

    /// One-line explanation, suitable for a diagnostic or a query `reason`.
    pub fn reason(self) -> &'static str {
        match self {
            FanoutVerdict::Fanout => "dispatched across the worker pool",
            FanoutVerdict::DeclinedMemoryBound => concat!(
                "body is memory-bandwidth-bound: at least one index/field read and no ",
                "substantial call, so splitting it pays dispatch cost without reducing wall time"
            ),
            FanoutVerdict::DeclinedBelowCostThreshold => concat!(
                "statically known trip count x per-iteration cost is below the dispatch ",
                "threshold; sequential execution wins"
            ),
            FanoutVerdict::DeclinedVariableKParamBound => concat!(
                "variable trip count with a cheap body and a parameter-referencing bound: ",
                "the reusable-helper shape where per-call dispatch cost is unrecoverable"
            ),
        }
    }
}

/// Run the fan-out gate sequence for one recognized loop, in the order both
/// `codegen/reduce.rs` and `codegen/disjoint_par.rs` apply it: memory-bound,
/// then the const-K cost gate, then the variable-K parameter-bound floor.
///
/// The per-iteration estimate is computed up front, for every loop. It used to
/// be deferred until after the memory-bound gate so a rejected loop never paid
/// for the walk, but the carve-out below needs the estimate in order to decide
/// that gate, so there is nothing left to defer. The walk is a bounded AST
/// traversal of one loop body — the saving was small and is not worth
/// reintroducing a second, cheaper-but-different notion of "how much work".
///
/// `program` threads free-function bodies into the estimator so a call into a
/// known callee folds the callee's real cost instead of the opaque
/// `CALL_COST_UNITS` constant. Pass `None` to estimate without inlining.
///
/// `bound_references_param` is supplied by the caller because the two callers
/// resolve "is this a parameter of the enclosing function" differently —
/// codegen from its current-function context, the query side from the AST.
///
/// ## The nested-loop carve-out on the memory-bound gate
///
/// `body_is_memory_bound` asks "does the body read memory and make no
/// substantial CALL", and it is right for the scalar reduction it was
/// calibrated on (kata-153's `let x = nums[i]; if x < m { m = x; }`). It has no
/// notion of a body whose work is a runtime-bounded nested LOOP, so it
/// classifies a convolution as memory-streaming. The gate is therefore skipped
/// when the body already scores at least one nested loop's worth of work —
/// reusing [`VARIABLE_K_PER_ITER_FLOOR_UNITS`], the constant the cost model
/// already uses to mean exactly that, rather than inventing a second threshold.
///
/// Measured on both fan-out shapes, 4 cores, medians:
///
/// - **Indexed writes.** Prism's Lanczos vertical pass — a ~7-tap
///   `Vector[f64,2]` FMA per output pixel — the gate cost 21% of wall time
///   (1600x1200 -> 800x600, 9 runs: 43.8 ms declined vs 34.5 ms fanned out,
///   user-CPU flat, so the parallel work was not being wasted). It also made
///   converting that kernel to the natural loop form a REGRESSION against the
///   hand-rolled band fan-out it replaced.
/// - **Reductions.** The reduction twin of that kernel — a 7-tap convolution
///   whose body also has one top-level index read — measured 172.2 ms declined
///   vs 107.5 ms fanned out (11 runs), a **1.6x** loss to the gate. Three
///   interleaved runs put the ratio between 1.45x and 1.60x.
///
/// The carve-out cannot reach kata-153's shape: a flat body scores far below
/// [`VARIABLE_K_PER_ITER_FLOOR_UNITS`], so it stays `DeclinedMemoryBound`.
///
/// ## What the gate does and does not still cover
///
/// [`MemoryBoundDetector`] deliberately does **not** descend into nested loops
/// (`While` / `For` / `Loop` fall through its `_` arm), so a body whose loads
/// all live inside an inner loop was never classified memory-bound in the first
/// place — the carve-out only changes bodies that ALSO read memory at the top
/// level. Making the detector nest-aware was measured and rejected: it
/// reclassifies genuinely-winning loops as memory-bound (a 32 MiB strided sum
/// measured 73.2 ms sequential vs 34.6 ms fanned out, 2.12x; at 128 MiB,
/// 156.2 ms vs 104.1 ms, 1.50x — more cores extract more bandwidth than one).
pub fn fanout_verdict(
    body: &Block,
    end_expr: &Expr,
    lo_expr: Option<&Expr>,
    program: Option<&Program>,
    bound_references_param: bool,
) -> FanoutVerdict {
    fanout_verdict_with_cost(body, end_expr, lo_expr, program, bound_references_param).0
}

/// [`fanout_verdict`], also returning the per-iteration body cost estimate it
/// computed.
///
/// Codegen needs that number twice: once to decide the gate and once to stamp
/// the descriptor's `per_iter_cost_units` field for the runtime-side gate.
/// Handing it back avoids estimating the body a second time, and — more
/// importantly — stops codegen from open-coding its own copy of the gate
/// sequence to keep the estimate in scope. A second copy of a calibrated cost
/// model drifts from the first, which is the failure this module exists to
/// prevent (see the module docs).
pub fn fanout_verdict_with_cost(
    body: &Block,
    end_expr: &Expr,
    lo_expr: Option<&Expr>,
    program: Option<&Program>,
    bound_references_param: bool,
) -> (FanoutVerdict, u64) {
    let per_iter_cost = match program {
        Some(prog) => CostEstimator::new(prog).estimate_body(body),
        None => estimate_body_cost_units(body),
    };
    let substantial = per_iter_cost >= VARIABLE_K_PER_ITER_FLOOR_UNITS;
    if !substantial && body_is_memory_bound(body) {
        return (FanoutVerdict::DeclinedMemoryBound, per_iter_cost);
    }
    if let Some(k) = const_eval_iter_count(end_expr, lo_expr) {
        if k.saturating_mul(per_iter_cost) < REDUCE_DISPATCH_THRESHOLD_UNITS {
            return (FanoutVerdict::DeclinedBelowCostThreshold, per_iter_cost);
        }
    } else if per_iter_cost < VARIABLE_K_PER_ITER_FLOOR_UNITS && bound_references_param {
        return (FanoutVerdict::DeclinedVariableKParamBound, per_iter_cost);
    }
    (FanoutVerdict::Fanout, per_iter_cost)
}

/// Per-call overhead of dispatching to `karac_par_reduce`, in
/// "1 unit ≈ 1 ns." Calibrated against the kata-7 bench: the pool-share
/// refactor (slice 3b.7) measured dispatch latency at ~10µs per call
/// for N=18 workers including Box alloc + queue push + N Condvar wakes
/// + the final N-way combine. Round-up to 10,000 units (10µs).
pub(crate) const DISPATCH_OVERHEAD_PER_CALL_UNITS: u64 = 10_000;

/// Worker count we assume at compile time for the threshold math. Real
/// runtime worker count is `available_parallelism()` (typically 4–18 on
/// developer machines), but we don't have that at codegen time — and
/// even if we did, baking it into the binary would defeat the
/// portability of the artifact. Median modern CPU is 8 cores; use that
/// as the assumed N. Slight under-estimate on big.LITTLE machines
/// (M5 Pro has 18 cores) lowers the threshold a bit, which is the safer
/// direction (more loops cross the gate at small K).
pub(crate) const ASSUMED_WORKER_COUNT: u64 = 8;

/// Threshold for the cost-model gate. Total work (K × per-iter cost) must
/// exceed this for the par_reduce dispatch to win. With the calibration
/// above, this is 80,000 unit-iterations ≈ 80µs of estimated work — at
/// that scale, the ~10µs dispatch overhead amortizes to roughly 12% of
/// runtime, leaving most of the work for parallel speedup.
pub(crate) const REDUCE_DISPATCH_THRESHOLD_UNITS: u64 =
    DISPATCH_OVERHEAD_PER_CALL_UNITS * ASSUMED_WORKER_COUNT;

/// Minimum per-iteration body cost for a VARIABLE-trip-count reduction to be
/// lowered to `par_reduce` (B-2026-07-23-25). The compile-time `K × per_iter`
/// gate above only applies when the trip count is a compile-time constant;
/// variable-K loops previously bypassed it entirely, relying on a runtime-side
/// gate. But that runtime gate still pays the descriptor-setup + dispatch cost
/// on EVERY call, which is catastrophic when the loop is a tiny fine-grained
/// hot-path HELPER invoked millions of times (the `pow10` / `num_digits` shape
/// — `while i < n { r = r * 10 }`, per-iter cost ≈ 4 — inside an O(n²) sort
/// comparator: ~1000x slowdown on the default build, output still correct). For
/// a body doing less than one function-call's worth of work per iteration
/// (`CALL_COST_UNITS`), the spawn+join overhead is unrecoverable regardless of
/// the unknowable trip count, so keep it sequential. Substantial-body
/// variable-K loops (a real per-iter nested loop) clear this floor and stay
/// eligible; the runtime gate handles their rare small-actual-K case.
///
/// Set to `RUNTIME_NESTED_LOOP_MULTIPLIER` (64): the estimator scores any body
/// containing a runtime-bounded nested loop at ≥ 64 × its inner cost, while a
/// body of pure scalar ops / O(1) calls stays well below (the `pow10` body
/// `r = r * 10; i = i + 1` scores 11; `num_digits` similar). So the floor
/// cleanly means "the per-iteration work must be at least one nested loop's
/// worth" — coarse enough that dispatch overhead can amortize even when the
/// trip count is unknown.
pub(crate) const VARIABLE_K_PER_ITER_FLOOR_UNITS: u64 = RUNTIME_NESTED_LOOP_MULTIPLIER;

/// Try to const-evaluate the loop's iteration count = `end - lo` to a
/// literal. Returns `None` for any non-literal shape on either bound
/// (Identifier, expression involving captures, etc.) so the cost-model
/// gate conservatively assumes "large enough to parallelize." Pre- and
/// post-lowering both leave integer literals untouched, so this is
/// shape-agnostic across the pipeline. `lo_expr = None` means "no lo
/// in the source" (treated as 0 — the slice 3b / 3b.4 shape).
pub(crate) fn const_eval_iter_count(end_expr: &Expr, lo_expr: Option<&Expr>) -> Option<u64> {
    let end_lit = const_eval_int_literal(end_expr)?;
    let lo_lit = match lo_expr {
        Some(e) => const_eval_int_literal(e)?,
        None => 0,
    };
    let count = end_lit.checked_sub(lo_lit)?;
    if count >= 0 {
        Some(count as u64)
    } else {
        None
    }
}

/// Pull a signed-int literal out of an Expr. Returns `None` for any non-
/// literal shape — including negative literals that the parser already
/// represents as a Unary{Neg, Integer(n)} rather than Integer(-n); v1's
/// reduction range bounds rarely use negatives so the literal arm is
/// sufficient. Pre- and post-lowering both leave Integer(n) untouched.
pub(crate) fn const_eval_int_literal(expr: &Expr) -> Option<i64> {
    if let ExprKind::Integer(n, _) = expr.kind {
        Some(n)
    } else {
        None
    }
}

/// Codegen-time per-iter body-cost estimator. Walks the AST with weights
/// chosen to bias toward the actual code shape: arithmetic / comparison
/// / cast each cost a small constant; function and method calls fall
/// back to `CALL_COST_UNITS` for opaque callees but recursively estimate
/// the callee's body when it's a known free function in this program
/// (up to `INLINE_DEPTH_CAP` levels deep). Control-flow takes the
/// max-arm path (conservative for cost, so the gate over-counts and
/// thus over-parallelizes — acceptable bias for v1). A nested loop with a
/// compile-time-evaluable range (`for i in 0..16`) uses its exact trip
/// count; a runtime-bounded loop (`while`, `for x in v.iter()`, runtime
/// range, `loop`) uses `RUNTIME_NESTED_LOOP_MULTIPLIER` since the trip
/// count is unknown at codegen time.
///
/// The inlining-aware path (slice: cost-gate fn-call body cost,
/// 2026-05-20) addresses the constant-10 underestimate surfaced by the
/// post-3b.10 re-bench sweep: `for _ in 0..K { sum += f(big_input); }`
/// shapes scored as `K * 10` cost units regardless of what `f` did,
/// so K=10 outer reductions over heavy callees (kata-121's max_profit,
/// kata-153's find_min) failed the cost gate and ran sequentially. By
/// recursing into resolvable callees the gate now reflects the callee's
/// structural cost (number of stmts, branches, inner loops) rather than
/// a constant.
pub(crate) struct CostEstimator<'a> {
    /// Free-function bodies keyed by source name. Built once from
    /// `Program.items` at construction; method bodies are not included
    /// at v1 (`MethodCall` and 2+-segment `Path` calls keep the
    /// `CALL_COST_UNITS` fallback — adding receiver-type-resolved
    /// method lookup needs typechecker info threaded in, deferred).
    fn_bodies: HashMap<String, &'a Function>,
    /// Current inlining recursion depth. Bounded by `INLINE_DEPTH_CAP`
    /// to prevent unbounded recursion on indirect-recursive call graphs
    /// (`A → B → A`) without needing a visited-set: the depth alone is
    /// a safe upper bound because each recursive call increments it.
    depth: u32,
}

impl<'a> CostEstimator<'a> {
    /// Recursion cap for body inlining. Three levels = the caller, one
    /// callee, one grand-callee — enough to estimate a `sum += f(...)`
    /// shape that hides a real-work-doing loop inside `f`, without
    /// blowing up on deep call chains. Past the cap, calls fall back
    /// to `CALL_COST_UNITS` so the estimator always terminates.
    const INLINE_DEPTH_CAP: u32 = 3;

    pub(crate) fn new(program: &'a Program) -> Self {
        let mut fn_bodies = HashMap::new();
        for item in &program.items {
            if let Item::Function(f) = item {
                fn_bodies.insert(f.name.clone(), f);
            }
        }
        Self {
            fn_bodies,
            depth: 0,
        }
    }

    /// Body-cost entry point. Per-iter cost in "1 unit ≈ 1 ns" —
    /// matches the calibration unit of `DISPATCH_OVERHEAD_PER_CALL_UNITS`
    /// so threshold math stays apples-to-apples.
    pub(crate) fn estimate_body(&mut self, body: &Block) -> u64 {
        let mut total: u64 = 0;
        for stmt in &body.stmts {
            total = total.saturating_add(self.estimate_stmt(stmt));
        }
        if let Some(e) = &body.final_expr {
            total = total.saturating_add(self.estimate_expr(e));
        }
        // Bound at 1 so a trivially-empty body (no stmts, no final expr
        // — analyzer rejects this earlier but the helper stays safe)
        // doesn't gate out every loop at K * 0 = 0 < threshold.
        total.max(1)
    }

    pub(crate) fn estimate_stmt(&mut self, stmt: &Stmt) -> u64 {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                1u64.saturating_add(self.estimate_expr(value))
            }
            StmtKind::Assign { target, value } => 1u64
                .saturating_add(self.estimate_expr(target))
                .saturating_add(self.estimate_expr(value)),
            StmtKind::CompoundAssign { target, value, .. } => 2u64
                .saturating_add(self.estimate_expr(target))
                .saturating_add(self.estimate_expr(value)),
            StmtKind::Expr(e) => self.estimate_expr(e),
            StmtKind::LetUninit { .. } => 1,
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                // Defer bodies run at scope exit, not per-iter — but in
                // the worker-fn the worker scope IS the iter scope (one
                // alloca frame), so count once. Conservative; the
                // slice-3b worker-fn synth pushes one cleanup frame per
                // call anyway.
                self.estimate_body(body)
            }
        }
    }

    /// Resolve a Call's callee identifier to a free-fn body cost when
    /// possible. Returns `CALL_COST_UNITS` when the callee shape isn't
    /// a known free-fn name, or when the recursion depth cap is hit.
    /// Caller is responsible for adding arg costs separately — this
    /// returns the body-walk cost only (mirrors the prior CALL_COST_UNITS
    /// semantics, which represented the callee body opaquely).
    pub(crate) fn call_body_cost(&mut self, callee: &Expr) -> u64 {
        if self.depth >= Self::INLINE_DEPTH_CAP {
            return CALL_COST_UNITS;
        }
        let name = match &callee.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            ExprKind::Path { segments, .. } if segments.len() == 1 => Some(segments[0].clone()),
            _ => None,
        };
        let Some(name) = name else {
            return CALL_COST_UNITS;
        };
        let Some(f) = self.fn_bodies.get(&name).copied() else {
            return CALL_COST_UNITS;
        };
        self.depth += 1;
        let cost = self.estimate_body(&f.body);
        self.depth -= 1;
        cost
    }

    pub(crate) fn estimate_expr(&mut self, expr: &Expr) -> u64 {
        match &expr.kind {
            // Free: leaf literals + identifier loads. SSA-promoted alloca
            // reads compile to a single load that the LLVM backend almost
            // always folds into the consuming instruction.
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::Bool(_)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType => 0,

            // Arithmetic / bitwise / comparison: 1 unit each plus operand costs.
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => 1u64
                .saturating_add(self.estimate_expr(left))
                .saturating_add(self.estimate_expr(right)),
            ExprKind::NilCoalesce { left, right } => 1u64
                .saturating_add(self.estimate_expr(left))
                .saturating_add(self.estimate_expr(right)),
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                1u64.saturating_add(self.estimate_expr(operand))
            }
            ExprKind::Cast { expr: inner, .. } => 1u64.saturating_add(self.estimate_expr(inner)),

            // Indexing: 2 units (GEP + load + bounds check) plus operand costs.
            ExprKind::Index { object, index } => 2u64
                .saturating_add(self.estimate_expr(object))
                .saturating_add(self.estimate_expr(index)),
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                1u64.saturating_add(self.estimate_expr(object))
            }

            // Calls: try to inline the callee's body cost when the callee
            // is a known free fn within the recursion depth cap; else
            // fall back to `CALL_COST_UNITS`. Args + callee-expr eval
            // costs added separately.
            ExprKind::Call { callee, args } => {
                let mut c: u64 = self.call_body_cost(callee);
                c = c.saturating_add(self.estimate_expr(callee));
                for arg in args {
                    c = c.saturating_add(self.estimate_expr(&arg.value));
                }
                c
            }
            ExprKind::MethodCall { object, args, .. } => {
                // Method receiver type resolution isn't threaded into
                // the estimator at v1 — keep the opaque CALL_COST_UNITS
                // fallback. Adding receiver-type-aware method lookup
                // requires the typechecker's method_callee_types table.
                let mut c: u64 = CALL_COST_UNITS;
                c = c.saturating_add(self.estimate_expr(object));
                for arg in args {
                    c = c.saturating_add(self.estimate_expr(&arg.value));
                }
                c
            }
            ExprKind::OptionalChain { object, args, .. } => {
                let mut c: u64 = CALL_COST_UNITS;
                c = c.saturating_add(self.estimate_expr(object));
                if let Some(args) = args {
                    for arg in args {
                        c = c.saturating_add(self.estimate_expr(&arg.value));
                    }
                }
                c
            }

            // Control-flow: walk arms, take the max (conservative cost).
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                let cond = self.estimate_expr(condition);
                let then_cost = self.estimate_body(then_block);
                let else_cost = else_branch
                    .as_ref()
                    .map(|e| self.estimate_expr(e))
                    .unwrap_or(0);
                cond.saturating_add(then_cost.max(else_cost))
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                let v = self.estimate_expr(value);
                let then_cost = self.estimate_body(then_block);
                let else_cost = else_branch
                    .as_ref()
                    .map(|e| self.estimate_expr(e))
                    .unwrap_or(0);
                v.saturating_add(then_cost.max(else_cost))
            }
            ExprKind::Match { scrutinee, arms } => {
                let s = self.estimate_expr(scrutinee);
                let arm_max = arms
                    .iter()
                    .map(|a| self.estimate_expr(&a.body))
                    .max()
                    .unwrap_or(0);
                s.saturating_add(arm_max)
            }

            // Inner loops: trip count drives the cost. A compile-time-
            // evaluable `for i in lo..hi` uses its EXACT count (no over- or
            // under-estimate); every runtime-bounded loop (while, while-let,
            // `for x in v.iter()`, runtime/step_by ranges, bare loop) uses
            // `RUNTIME_NESTED_LOOP_MULTIPLIER` — the flat-16 it replaces was
            // orders of magnitude low for real scans (see the const's doc).
            ExprKind::While {
                condition, body, ..
            } => {
                let c = self.estimate_expr(condition);
                let b = self.estimate_body(body);
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(c.saturating_add(b))
            }
            ExprKind::WhileLet { value, body, .. } => {
                let v = self.estimate_expr(value);
                let b = self.estimate_body(body);
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(v.saturating_add(b))
            }
            ExprKind::For { iterable, body, .. } => {
                let it = self.estimate_expr(iterable);
                let b = self.estimate_body(body);
                // `for i in lo..hi` with literal bounds → exact trip count.
                // (Half-open only; an inclusive `..=` const range is rare in
                // a hot inner loop and falls through to the runtime path.)
                if let ExprKind::Range {
                    start,
                    end: Some(end),
                    inclusive: false,
                } = &iterable.kind
                {
                    if let Some(count) = const_eval_iter_count(end, start.as_deref()) {
                        return count.saturating_mul(b.max(1)).saturating_add(it);
                    }
                }
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(it.saturating_add(b))
            }
            ExprKind::Loop { body, .. } => {
                RUNTIME_NESTED_LOOP_MULTIPLIER.saturating_mul(self.estimate_body(body))
            }

            // Blocks and other shape-passthrough nodes: cost of the contained block.
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b) => self.estimate_body(b),
            ExprKind::Par(b) => self.estimate_body(b),
            ExprKind::Lock { body, .. } => self.estimate_body(body),
            ExprKind::LabeledBlock { body, .. } => self.estimate_body(body),

            // Composite literals — cost is sum of element costs.
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
                let mut c: u64 = 0;
                for e in elems {
                    c = c.saturating_add(self.estimate_expr(e));
                }
                c
            }
            ExprKind::RepeatLiteral { value, count, .. } => 1u64
                .saturating_add(self.estimate_expr(value))
                .saturating_add(self.estimate_expr(count)),
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                let mut c: u64 = 1;
                for e in items {
                    c = c.saturating_add(self.estimate_expr(e));
                }
                c
            }
            ExprKind::MapLiteral(entries) => {
                let mut c: u64 = 1;
                for (k, v) in entries {
                    c = c.saturating_add(self.estimate_expr(k));
                    c = c.saturating_add(self.estimate_expr(v));
                }
                c
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                let mut c: u64 = 1;
                for f in fields {
                    c = c.saturating_add(self.estimate_expr(&f.value));
                }
                if let Some(s) = spread {
                    c = c.saturating_add(self.estimate_expr(s));
                }
                c
            }
            ExprKind::Range { start, end, .. } => {
                let mut c: u64 = 0;
                if let Some(s) = start {
                    c = c.saturating_add(self.estimate_expr(s));
                }
                if let Some(e) = end {
                    c = c.saturating_add(self.estimate_expr(e));
                }
                c
            }
            ExprKind::Closure { body, .. } => self.estimate_expr(body),
            ExprKind::Providers { bindings, body } => {
                let mut c: u64 = 0;
                for b in bindings {
                    c = c.saturating_add(self.estimate_expr(&b.value));
                }
                c.saturating_add(self.estimate_body(body))
            }
            ExprKind::Return(Some(inner)) => self.estimate_expr(inner),
            ExprKind::Break { value: Some(v), .. } => self.estimate_expr(v),
            ExprKind::InterpolatedStringLit(parts) => {
                let mut c: u64 = 1;
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(inner, _) = part {
                        c = c.saturating_add(self.estimate_expr(inner));
                    }
                }
                c
            }

            // Pure control-edge shapes.
            ExprKind::Continue { .. }
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => 0,
        }
    }
}

/// Free-fn wrapper kept for backward compatibility with internal call
/// sites that don't need the inlining-aware path. Internally builds an
/// estimator with an empty `fn_bodies` map — semantically equivalent
/// to "every call is opaque, return CALL_COST_UNITS" — so it matches
/// the pre-slice behavior on its own.
pub(crate) fn estimate_body_cost_units(body: &Block) -> u64 {
    let mut est = CostEstimator {
        fn_bodies: HashMap::new(),
        depth: 0,
    };
    est.estimate_body(body)
}

/// Function-call cost — function-call ABI alone is on the order of 5–20
/// ns (PLT + arg marshalling + branch); add ~10 units for the callee
/// body when the callee is opaque (Method call, multi-segment Path,
/// past the recursion-depth cap). When the callee is a resolvable free
/// fn within the cap, the body's actual structural cost replaces this
/// constant — see `CostEstimator::call_body_cost`.
pub(crate) const CALL_COST_UNITS: u64 = 10;

/// Trip-count multiplier for a loop whose bound is *runtime* (not a
/// compile-time-evaluable range): `while i < s.len()`, `for x in v.iter()`,
/// `for j in (a..=b).step_by(k)`, `loop { ... }`. The flat
/// `NESTED_LOOP_MULTIPLIER = 16` underestimated these by orders of
/// magnitude — a `while i < hn` over a 2M-element slice runs millions of
/// times, not 16 — so a doubly-nested runtime scan (`str_str`'s
/// `while i { while j { s[i+j] == n[j] } }`, kata-28) scored ≈30k cost
/// units (`16² × body × K=10`) and fell under the 80k dispatch threshold,
/// declining a real ~11× parallel win to a serial run. 64 is calibrated
/// so a doubly-nested runtime loop crosses the gate (`64² × body × K`)
/// while a *single* runtime loop at small K stays conservatively serial
/// (kata-1 hash_map's lone `for i in 0..n` at K=10 ≈ 64 × body × 10 stays
/// well under threshold) — over-firing genuinely light bodies is the cost
/// we keep bounded, since the existing gate philosophy already biases
/// toward over-counting (control-flow takes the max arm). Compile-time-
/// evaluable ranges (`for i in 0..16`) bypass this entirely and use their
/// exact count (see the `For`/`While`/`Loop` arms in `estimate_expr`).
/// Surfaced + calibrated by the 2026-06-13 `for _` auto-par re-bench sweep
/// (phase-7-codegen.md); the calibration follow-up the closed
/// "function-call body-cost estimation" slice deferred "when needed".
pub(crate) const RUNTIME_NESTED_LOOP_MULTIPLIER: u64 = 64;

/// The memory-bandwidth gate: true when the body has at least one
/// Index/FieldAccess and no substantial function/method call.
///
/// The cost gates elsewhere in this module are compute-units-aware but not
/// bandwidth-aware. For a body that is mostly memory-streaming
/// (`let x = nums[i]; if x < m { m = x; }`) the compute-unit estimate looks
/// parallelizable — ~10M units against an 80k threshold at N=2M — while the
/// wall clock is pinned to memory bandwidth. Measured on kata-153 before this
/// gate existed: User-CPU 3.5 ms -> 11.8 ms for **no** wall improvement, and
/// the binary grew 49 KiB -> 311.9 KiB just to link `par_reduce`.
///
/// "Substantial" excludes two things, both load-bearing. Lowered primitive-op
/// calls (`Call(Path([type, op_method]), ..)`) are intrinsic operator
/// dispatches, not real callees — counting them would defeat the gate for
/// every body, since post-lowering every body has arithmetic. Trivial accessor
/// methods (`len`, `is_empty`, `as_slice`, `as_str`, `as_bytes`) are shape
/// queries on the collection, not compute.
///
/// See [`fanout_verdict`] for the one carve-out that lets a nested-loop body
/// past this gate, and for why the detector's blindness to nested loops is
/// kept rather than repaired.
pub(crate) fn body_is_memory_bound(body: &Block) -> bool {
    let mut detector = MemoryBoundDetector {
        memory_count: 0,
        substantial_call: false,
    };
    detector.visit_body(body);
    detector.memory_count > 0 && !detector.substantial_call
}

pub(crate) struct MemoryBoundDetector {
    memory_count: u32,
    substantial_call: bool,
}

impl MemoryBoundDetector {
    pub(crate) fn visit_body(&mut self, body: &Block) {
        for stmt in &body.stmts {
            self.visit_stmt(stmt);
        }
        if let Some(e) = &body.final_expr {
            self.visit_expr(e);
        }
    }

    pub(crate) fn visit_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => self.visit_expr(value),
            StmtKind::Assign { target, value } => {
                self.visit_expr(target);
                self.visit_expr(value);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.visit_expr(target);
                self.visit_expr(value);
            }
            StmtKind::Expr(e) => self.visit_expr(e),
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.visit_body(body),
        }
    }

    pub(crate) fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Index { object, index } => {
                self.memory_count = self.memory_count.saturating_add(1);
                self.visit_expr(object);
                self.visit_expr(index);
            }
            ExprKind::FieldAccess { object, .. } => {
                self.memory_count = self.memory_count.saturating_add(1);
                self.visit_expr(object);
            }
            ExprKind::Call { callee, args } => {
                // The lowering pass rewrites every primitive binop /
                // comparison into a `Call(Path([type, op_method]), [a, b])`
                // shape (e.g. `x < m` → `Call(Path(["i64", "lt"]), [x, m])`).
                // These are intrinsic operator dispatches, not real
                // function calls — counting them as `substantial_call`
                // would defeat the memory-bound gate for every body that
                // has any arithmetic or comparison post-lowering (which
                // is every kata's body). Filter those out before tagging
                // the call as substantial.
                if !is_lowered_primitive_op_call(callee) {
                    self.substantial_call = true;
                }
                self.visit_expr(callee);
                for arg in args {
                    self.visit_expr(&arg.value);
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if !is_trivial_accessor_method(method) {
                    self.substantial_call = true;
                }
                self.visit_expr(object);
                for arg in args {
                    self.visit_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.visit_expr(left);
                self.visit_expr(right);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.visit_expr(operand);
            }
            ExprKind::Cast { expr: inner, .. } => self.visit_expr(inner),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.visit_expr(condition);
                self.visit_body(then_block);
                if let Some(e) = else_branch {
                    self.visit_expr(e);
                }
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.visit_body(b),
            // Other shapes (literals, identifiers, paths, etc.) contribute
            // no memory access or call signal.
            //
            // `While` / `For` / `Loop` land here too, so reads inside a NESTED
            // loop are invisible to this walk and a body whose loads all live
            // in an inner loop is never classified memory-bound. That is not an
            // oversight left standing by accident — making the walk nest-aware
            // was implemented, measured, and rejected, because it reclassifies
            // loops that measurably win from fanning out. The measurements and
            // the reasoning are on [`fanout_verdict`]; read them before
            // "fixing" this.
            _ => {}
        }
    }
}

pub(crate) fn is_trivial_accessor_method(method: &str) -> bool {
    matches!(
        method,
        "len" | "is_empty" | "as_slice" | "as_str" | "as_bytes"
    )
}

/// Recognize the lowering-pass-emitted shape for a primitive operator
/// dispatch — `Call(Path([type, op_method]), [a, b])` where `op_method`
/// is one of the standard arithmetic / comparison / bitwise / shift
/// methods. These are intrinsic op calls and should not count as
/// "substantial" callees for the memory-bound gate.
pub(crate) fn is_lowered_primitive_op_call(callee: &Expr) -> bool {
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    if segments.len() != 2 {
        return false;
    }
    matches!(
        segments[1].as_str(),
        // Arithmetic
        "add" | "sub" | "mul" | "div" | "rem" | "neg"
        // Comparison
        | "eq" | "ne" | "lt" | "le" | "gt" | "ge"
        // Bitwise
        | "bitor" | "bitand" | "bitxor" | "bitnot"
        // Shifts
        | "shl" | "shr"
        // Min/Max — added by the combined Min/Max slice (2026-05-20)
        | "min" | "max"
    )
}

/// Extract the canonical shape of a recognized reduction loop. Returns
/// `Some(LoopShape)` when the loop matches one of v1's supported shapes
/// (for-range with `lo == 0`, or while with an explicit `k = k + 1`
/// induction step preceded by `let mut k: T = 0;`), `None` otherwise.
/// Decouples the shape-parsing complexity from the lowering caller so
/// future shapes (non-zero `lo`, larger step constants, while_let,
/// loop with break, etc.) extend by adding match arms here without
/// changing the lowering body.
pub(crate) fn extract_loop_shape(
    parent_body: &Block,
    stmt_index: usize,
    expr: &Expr,
) -> Option<LoopShape> {
    match &expr.kind {
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            // `for k in ..` binds `k`; `for _ in ..` discards it. The
            // wildcard case is just as parallelizable — the body never
            // reads the loop variable, so the reduction is independent
            // of iteration order. Synthesize a sentinel name (with
            // chars illegal in a source identifier, so it can never
            // collide with a captured outer variable) for the unused
            // per-worker loop-index alloca. Other pattern kinds (tuple,
            // struct destructure, etc.) aren't loop-counter shapes and
            // fall through to sequential codegen.
            let loop_var = match &pattern.kind {
                PatternKind::Binding(name) => name.clone(),
                PatternKind::Wildcard => "<reduce-wildcard-idx>".to_string(),
                _ => return None,
            };
            let ExprKind::Range {
                start,
                end,
                inclusive: false,
            } = &iterable.kind
            else {
                return None;
            };
            let end_expr = end.as_ref()?;
            // Slice 3b.3: any `lo` expression of the accumulator
            // type is supported by adding it to the worker's chunk-
            // local index. `None` / `Integer(0)` normalize to
            // `lo_expr = None` (no shift math — the worker's local
            // index already matches the source-level k).
            let lo_expr = match start.as_deref() {
                None => None,
                Some(s) if matches!(s.kind, ExprKind::Integer(0, _)) => None,
                Some(s) => Some(s.clone()),
            };
            Some(LoopShape {
                loop_var,
                end_expr: (**end_expr).clone(),
                body: body.clone(),
                lo_expr,
            })
        }
        ExprKind::While {
            condition, body, ..
        } => {
            // Pull `loop_var` and `end_expr` out of the condition.
            // Accepts both `Binary { Lt, Ident(k), end }` (pre-
            // lowering) and `Call(Path([T, "lt"]), [Ident(k), end])`
            // (post-lowering). The body must contain exactly one step-
            // 1 increment of the loop var as its terminal stmt; the
            // recognizer (slice 1) already accepted the loop as an
            // induction-step + reduction pair, so we can be opinionated
            // about the shape here.
            let (loop_var, end_expr) = parse_lt_condition(condition)?;

            // The body's last stmt must be `loop_var = loop_var + 1`
            // (or `loop_var += 1`, either pre- or post-lowered). Strip
            // it so the worker's loop scaffolding handles the
            // increment via the back-edge — same shape as the for-loop
            // path, no need to re-think the worker fn synth.
            let stripped_body = strip_terminal_step_one_increment(body, &loop_var)?;

            // The immediately preceding stmt must be `let mut k: T =
            // <anything>;`. Slices 3b.9 + 3b.10 normalize the init:
            //   - `Integer(0)`: `lo_expr = None` (no shift math).
            //   - Non-zero int literal: `lo_expr = Some(literal)` —
            //     re-compile the literal in the par_reduce setup;
            //     it's a constant, no side effects, free.
            //   - Anything else: `lo_expr = Some(Identifier(k))` —
            //     load from the parent's k alloca (the let-stmt
            //     already evaluated the init expression and stored
            //     the result; reading it back guarantees single
            //     evaluation regardless of side effects in the init
            //     expression).
            // Adjacent let + while (no intervening stmts) means
            // nothing modifies k between the init and the dispatch.
            if stmt_index == 0 {
                return None;
            }
            let init_expr = preceding_stmt_init(parent_body, stmt_index, &loop_var)?;
            let lo_expr = match &init_expr.kind {
                ExprKind::Integer(0, _) => None,
                ExprKind::Integer(_, _) => Some(init_expr),
                _ => Some(Expr {
                    kind: ExprKind::Identifier(loop_var.clone()),
                    span: init_expr.span,
                }),
            };

            Some(LoopShape {
                loop_var,
                end_expr,
                body: stripped_body,
                lo_expr,
            })
        }
        _ => None,
    }
}

/// Canonical shape of a recognized reduction loop. Built by
/// `extract_loop_shape` from either the `for k in lo..hi` shape
/// (slices 3b + 3b.3) or the `while k < hi { ...; k = k + 1; }` shape
/// (slice 3b.4) and consumed by the lowering path. `body` is the source
/// body with the while-shape's terminal increment already stripped — so
/// the worker fn synth treats both shapes identically and always emits
/// its own back-edge `k += 1`. `lo_expr` is `None` when the source's
/// start bound is absent or `Integer(0)` (the common case — no shift
/// math at all in the worker); `Some(expr)` otherwise (slice 3b.3 — the
/// expr is compiled in the parent, passed through env-struct field 0,
/// and added to the worker's chunk-local start/end). The while-shape
/// always sets `lo_expr = None` since its loop-var init is gated to
/// literal 0 by `preceding_stmt_inits_to_zero`.
pub(crate) struct LoopShape {
    /// Read only by the fan-out *lowering* in `codegen/reduce.rs` (to rebind
    /// the loop variable per worker), which is `#[cfg(feature = "llvm")]`. The
    /// query side consumes the other three fields to run the gates, so without
    /// that feature this field is genuinely dead — hence the narrow cfg'd
    /// allow rather than a blanket one.
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub(crate) loop_var: String,
    pub(crate) end_expr: Expr,
    pub(crate) body: Block,
    pub(crate) lo_expr: Option<Expr>,
}

/// Match a less-than condition into `(loop_var_name, end_expr)`.
/// Accepts both pre-lowering `Binary { Lt, Ident(k), end }` and post-
/// lowering `Call(Path([type, "lt"]), [Ident(k), end])` — the codegen
/// pipeline runs `src/lowering.rs` before reaching us, so the post-
/// lowering shape is the common case, but `compile_to_ir` tests that
/// skip lowering need the pre-lowering arm too.
pub(crate) fn parse_lt_condition(condition: &Expr) -> Option<(String, Expr)> {
    match &condition.kind {
        ExprKind::Binary {
            op: BinOp::Lt,
            left,
            right,
        } => {
            let ExprKind::Identifier(name) = &left.kind else {
                return None;
            };
            Some((name.clone(), (**right).clone()))
        }
        ExprKind::Call { callee, args } => {
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 || segments[1] != "lt" || args.len() != 2 {
                return None;
            }
            let ExprKind::Identifier(name) = &args[0].value.kind else {
                return None;
            };
            Some((name.clone(), args[1].value.clone()))
        }
        _ => None,
    }
}

/// If the last stmt of `body` is `loop_var = loop_var + 1` or
/// `loop_var += 1` (in either pre- or post-lowered form), return a
/// fresh `Block` with that stmt removed. Returns `None` if the terminal
/// shape doesn't match — the recognizer (slice 1) only emits a
/// `LoopReduction` when the body has at most one induction step, so a
/// loop tagged as a reduction whose body's terminal stmt isn't the
/// step must have a non-canonical layout we don't handle in v1.
///
/// Also returns `None` when the loop variable is written anywhere else
/// in the body (defense-in-depth — the analyzer already rejects that
/// shape, but the codegen check costs nothing and pins the invariant).
pub(crate) fn strip_terminal_step_one_increment(body: &Block, loop_var: &str) -> Option<Block> {
    let last = body.stmts.last()?;
    if !is_step_one_increment_stmt(last, loop_var) {
        return None;
    }
    // Verify no other stmt in the body writes the loop variable. A
    // body-internal `k = <expr>` in the middle would shift the worker
    // fn out of the simple chunk-local-counter model.
    for (idx, s) in body.stmts.iter().enumerate() {
        if idx + 1 == body.stmts.len() {
            break;
        }
        if stmt_writes_loop_var(s, loop_var) {
            return None;
        }
    }
    let mut stripped = body.clone();
    stripped.stmts.pop();
    Some(stripped)
}

/// True iff `stmt` is `loop_var = loop_var + 1` or `loop_var += 1`,
/// in either pre-lowering or post-lowering form. The constant `1` is
/// matched by value (any int suffix accepted; the recognizer already
/// gates on int suffix at the analyzer level).
pub(crate) fn is_step_one_increment_stmt(stmt: &Stmt, loop_var: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { target, value } => {
            if !is_named_identifier(target, loop_var) {
                return false;
            }
            // Pre-lowering: Binary { Add, Ident(loop_var), Int(1) }.
            // Lowered: Call(Path([T, "add"]), [Ident(loop_var), Int(1)]).
            match &value.kind {
                ExprKind::Binary {
                    op: BinOp::Add,
                    left,
                    right,
                } => is_loop_var_plus_one(left, right, loop_var),
                ExprKind::Call { callee, args } => {
                    let ExprKind::Path { segments, .. } = &callee.kind else {
                        return false;
                    };
                    if segments.len() != 2 || segments[1] != "add" || args.len() != 2 {
                        return false;
                    }
                    is_loop_var_plus_one(&args[0].value, &args[1].value, loop_var)
                }
                _ => false,
            }
        }
        StmtKind::CompoundAssign {
            target,
            op: CompoundOp::Add,
            value,
        } => is_named_identifier(target, loop_var) && is_int_literal_one(value),
        _ => false,
    }
}

/// Whether a stmt writes (Assign / CompoundAssign target = identifier)
/// the named loop variable. Used to defense-in-depth the
/// `strip_terminal_step_one_increment` body scan.
pub(crate) fn stmt_writes_loop_var(stmt: &Stmt, loop_var: &str) -> bool {
    match &stmt.kind {
        StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
            is_named_identifier(target, loop_var)
        }
        _ => false,
    }
}

/// If `parent_body.stmts[stmt_index - 1]` is `let mut loop_var: T =
/// <anything>;`, return the init expression. Caller decides how to
/// translate the init into the worker's chunk-local shift:
///   - `Integer(0)` → `lo_expr = None` (no shift math, current path).
///   - Non-zero int literal → `lo_expr = Some(literal)` (slice 3b.9 —
///     re-compile literal in the parent's par_reduce setup, free).
///   - Anything else → `lo_expr = Some(Identifier(loop_var))` (slice
///     3b.10 — load from the parent's already-initialized k alloca
///     instead of re-evaluating the init expression, which would
///     double-evaluate side effects).
///
/// Returns `None` if the preceding stmt isn't a let-mut binding of the
/// loop var. Caller guarantees `stmt_index > 0`.
pub(crate) fn preceding_stmt_init(
    parent_body: &Block,
    stmt_index: usize,
    loop_var: &str,
) -> Option<Expr> {
    let prev = &parent_body.stmts[stmt_index - 1];
    let StmtKind::Let {
        pattern,
        value,
        is_mut: true,
        ..
    } = &prev.kind
    else {
        return None;
    };
    let PatternKind::Binding(name) = &pattern.kind else {
        return None;
    };
    if name != loop_var {
        return None;
    }
    Some(value.clone())
}

pub(crate) fn is_loop_var_plus_one(left: &Expr, right: &Expr, loop_var: &str) -> bool {
    let left_is_var = matches!(&left.kind, ExprKind::Identifier(n) if n == loop_var);
    let right_is_var = matches!(&right.kind, ExprKind::Identifier(n) if n == loop_var);
    let left_is_one = is_int_literal_one(left);
    let right_is_one = is_int_literal_one(right);
    (left_is_var && right_is_one) || (right_is_var && left_is_one)
}

pub(crate) fn is_int_literal_one(expr: &Expr) -> bool {
    matches!(expr.kind, ExprKind::Integer(1, _))
}

pub(crate) fn is_named_identifier(expr: &Expr, name: &str) -> bool {
    matches!(&expr.kind, ExprKind::Identifier(n) if n == name)
}
