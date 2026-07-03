# Shallow-depth parallel reduction — design note

**Status:** IMPLEMENTED (see § Outcome). **Author-context:** follow-on to
B-2026-07-03-14 (LeetCode #52 N-Queens II). **Scope:** runtime fork-depth cap +
compiler-guard relaxation + tests.

## Problem

B-2026-07-03-14 fixed a crash (SIGBUS at depth) where auto-par parallelized a `+`
reduction whose per-iteration delta **recurses into its own function** — a
backtracking counter, `if legal { total = total + count(...deeper...) }`. Each
recursion level opened a fresh parallel region; nesting was bounded only by stack
depth, so it exhausted the stack.

The fix (`concurrency.rs::recognize_reductions`, via
`call_graph::block_calls_function`) **declines all parallelization** of such a
loop. Correct and safe — but conservative. A backtracking search is
*embarrassingly parallel at the top*: the outermost loop's `n` branches are
independent subtrees. Declining outright leaves that on the table; the N-Queens
counter runs fully sequential when the first-row fan-out could saturate the pool.

**Goal:** parallelize the **outermost** level of a recursive reduction and run all
deeper levels sequentially, with a hard cap so nesting can never explode.

## Current pipeline (anchors)

1. **Recognition** — `src/concurrency.rs::recognize_reductions` → `LoopReduction`.
   The B-14 guard declines here when `block_calls_function(body, &func.name)`.
2. **Codegen** — `src/codegen/reduce.rs::try_emit_reduction_lowering`
   (called from `src/codegen/stmts.rs:696`). Lowers `for k in 0..hi` / `while k <
   hi` integer reductions into a `KaracReduceDescriptor` (`init_slot` /
   `worker_fn` / `combine_fn` fn-pointers) + a `karac_par_reduce` call. Gates:
   compile-time cost (`REDUCE_DISPATCH_THRESHOLD_UNITS`), memory-bound, early-exit.
3. **Runtime** — `runtime/src/lib.rs::karac_par_reduce` →
   `karac_par_reduce_pooled` (line ~1866). Splits `iter_total` across
   `n_workers`, folds per-worker slots, serial-combines. **Already has a
   single-worker sequential fast path** (line ~1903): `if n_workers == 1 ||
   gate_skip { (desc.worker_fn)(out_slot, 0, iter_total, ctx, &dummy); return; }`.

## Root cause of the explosion

`runtime/src/lib.rs` line ~1131 ("Nested par + work-helping"): *"A pool worker can
call `karac_par_run` … bounded only by stack depth, not by pool size."* So a
`worker_fn` that transitively calls `karac_par_reduce` again re-enters the pool
from a worker thread, recursively, with no depth bound → stack overflow.

## Proposed design

Two coordinated changes. The **runtime cap is the load-bearing safety
mechanism**; the compiler change just stops suppressing the now-safe lowering.

### 1. Runtime: thread-local fork-depth cap (the safety mechanism)

Add a thread-local depth counter, mirroring the proven `CONTRACT_PREDICATE_DEPTH`
idiom (`runtime/src/lib.rs:521`):

```rust
thread_local! { static PAR_REDUCE_DEPTH: Cell<u32> = const { Cell::new(0) }; }
```

In `karac_par_reduce_pooled`:

- **On entry**, read `PAR_REDUCE_DEPTH`. If `depth >= FORK_DEPTH_CAP`, take the
  **existing single-worker fast path** (run `worker_fn` inline over the full range
  into `out_slot`, return). No new code path — reuse line ~1903's shape.
- **When parallelizing** (depth below cap), bracket **each worker's `worker_fn`
  invocation** with `depth+1 … depth-1`, set on the *worker thread* (the pooled
  per-worker task closure, and the single-worker fast path). Because the counter
  is thread-local and the increment wraps the user body, any nested
  `karac_par_reduce` reached transitively from `worker_fn` observes `depth ≥ 1` on
  that worker thread and runs sequentially.

`FORK_DEPTH_CAP` is a small constant, default **1**, overridable via
`KARAC_PAR_MAX_FORK_DEPTH` (mirrors `KARAC_PAR_WORKERS`, read once per call like
`resolve_pool_workers`). `karac_par_run` (branch-parallel `par {}`) can adopt the
same guard in a follow-up; this note scopes the reduction path.

### 2. Compiler: relax the B-14 recognition decline

With the runtime bounding nesting, a recursive reduction is safe to lower — the
runtime parallelizes only the top level per-call. So relax
`recognize_reductions`: instead of declining when `block_calls_function` is true,
recognize + lower it as normal (the existing cost/shape gates in
`try_emit_reduction_lowering` still apply). The `call_graph::block_calls_function`
helper stays (useful, tested); it just no longer forces a decline.

Net: the compiler emits the reduction lowering; the runtime decides per-call
whether this invocation is the top level (parallelize) or nested (sequential).

## Why cap = 1

With a pool of `W` workers and a top-level fan-out `n ≥ W` (N-Queens: `n` columns),
cap = 1 already saturates the pool at the outermost level; every deeper level runs
inline. Cap = 2 would parallelize `n × n` sub-branches, but the pool is already
full at level 1, so it only adds dispatch overhead and re-opens (bounded) nesting.
Cap = 1 is the sweet spot: maximal useful parallelism, zero nested dispatch. The
env knob exists for measurement, not because a higher default is expected to win.

## Correctness argument

- **Result invariance.** `ReductionOp` is associative + commutative (already
  required for recognition). The result is independent of how iterations are
  partitioned across workers and of combine order. Running some subtrees
  sequentially is just a *coarser partition* of the same iteration space — still a
  valid partition, same fold. The single-worker fast path already proves inline
  execution is observably identical (it backs `n_workers == 1` and the cost gate).
- **A nested sequential reduction** computes its `i64` inline and returns it up
  through the ordinary call return (`count` → `total += …`); it never touches the
  outer reduction's slot buffer. No slot-machinery interaction.
- **Bounded nesting.** Live parallel frames ≤ `FORK_DEPTH_CAP × pool_workers`, a
  constant. No unbounded recursion of dispatch → no stack blowup. The exact class
  B-14 crashed on is structurally impossible.
- **Thread-safety.** Depth is thread-local (same rationale as
  `CONTRACT_PREDICATE_DEPTH`: tasks run on multiple scheduler threads, each tracks
  its own). No shared mutable state added; saturating add/sub so an unbalanced
  path can't under/overflow.

## Risks / open questions

1. **Depth-set placement in the pooled path.** The increment must wrap `worker_fn`
   on the *worker* thread, not the caller. Needs care in the `dispatch_and_wait` /
   `execute_task` task closure (the worker slot loop below line ~1942) — a guard
   struct (`Drop` decrements) is the safe form so a panicking `worker_fn` still
   restores depth. Pool workers are reused, so a leaked increment would wrongly
   serialize that worker's next task.
2. **Interaction with the cost gate.** A recursive reduction's `per_iter_cost`
   folds the recursive call in as `CALL_COST_UNITS` (opaque), so total work will
   read high and pass the threshold — desired (we want the top level to
   parallelize). No change needed, but worth a test at small `n`.
3. **`karac_par_run` (par branches)** has the same unbounded-nesting property.
   Out of scope here; the same thread-local guard generalizes if we later see a
   branch-parallel recursion crash.
4. **Transitive/mutual recursion** (`count` → helper → `count`): the *compiler*
   B-14 guard only caught direct self-recursion, but the *runtime* depth cap is
   agnostic to how the nested `karac_par_reduce` was reached — so it covers
   transitive recursion for free. This design strictly widens safety.

## Test plan

- **Runtime unit** (`runtime/src/scheduler.rs` test module, alongside the existing
  `karac_par_run` nesting tests): a `worker_fn` that re-enters `karac_par_reduce`
  N deep asserts bounded live-thread count and a correct fold.
- **par_codegen E2E**: the N-Queens counter (`fn count(...) -> i64` swept to n=13)
  under default auto-par — must (a) not crash, (b) match the sequential sink, (c)
  measurably use >1 worker at the top (via `KARAC_PAR_WORKERS` timing or a
  worker-touch counter).
- **Concurrency unit**: update `test_reduction_rejects_recursive_self_call_in_body`
  — after relaxation the reduction IS recognized again; assert lowering proceeds
  and the runtime cap (not the compiler) is what bounds it.
- **A/B**: kata #52 `bitmask_count.kara` / `symmetry.kara` stay byte-identical
  across run / build / auto-par, now with the auto-par build actually parallel.
- **Bench**: kata #52 `bench/nqueens2_bench.kara` under default auto-par (drop the
  `KARAC_AUTO_PAR=0`) — expect a wall-clock drop toward `wall/W`, capped by the
  serial tail and the n=13-dominates load imbalance.

## Outcome (as built)

Landed as designed, with two findings the plan didn't anticipate:

1. **Env-read storm (the load-bearing perf fix).** A recursive reduction enters
   `karac_par_reduce` at *every* recursion node (millions, for a backtracking
   search). The first cut read `std::env::var` for the cap on each call and, worse,
   still fell into the pooled path's `resolve_pool_workers` env read before the
   depth check — turning the search into a syscall storm (`sys` 24 s, wall *worse*
   than sequential). Fixes: (a) cache the cap in a `OnceLock`; (b) check the cap
   **first**, at the top of `karac_par_reduce`, before any pool/cost work, so a
   nested call's whole cost is a thread-local read + one `worker_fn` call; (c) raise
   the depth in the single-worker fast path too, so a 1-worker pool (or a
   single-iteration top loop) doesn't leave depth at 0 and re-query the pool per
   node. After these, nested/inline overhead is negligible.

2. **Load balance is the workload's, not the mechanism's.** With cap = 1 the
   *outermost* reduction is the one that parallelizes, so the speedup tracks that
   loop's balance. A single `count(13)` (13 balanced first-row branches) gets
   **9.46× on this host** (141.6 ms → 15.0 ms, `sys` ~2 ms). The kata's n-sweep
   bench, whose outer loop is `for n in 9..=13` with n = 13 ≈ 80 % of the work in
   one branch, barely speeds up — correct and safe, but a poor showcase. The kata
   bench therefore stays `KARAC_AUTO_PAR=0`; the win shows on a single large count.

**Shipped:** `PAR_REDUCE_DEPTH` thread-local + `ParReduceDepthGuard` +
cached `resolve_max_fork_depth` + top-of-`karac_par_reduce` cap check + guarded
single-worker path (`runtime/src/lib.rs`); B-14 decline removed from
`recognize_reductions` and `call_graph::block_calls_function` deleted. Tests:
runtime `test_par_reduce_fork_depth_cap_bounds_recursive_nesting`, concurrency
`test_reduction_recognized_for_recursive_self_call_in_body`, par_codegen
`test_e2e_recursive_reduction_nqueens_count_bounded_by_fork_depth_cap`.

**Not done (as scoped):** `karac_par_run` (the `par {}` branch path) keeps its own
unbounded-nesting property; the same thread-local guard generalizes to it in a
fast-follow if a branch-parallel recursion ever surfaces it. `FORK_DEPTH_CAP`
stayed at default 1 with the `KARAC_PAR_MAX_FORK_DEPTH` knob.
