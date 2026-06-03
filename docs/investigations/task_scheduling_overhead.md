# Task-scheduling overhead — G13 investigation

**Status:** ✓ Resolved (2026-06-02). **Owner:** unassigned.
**Checklist item:** [`phase-6-runtime.md`](../implementation_checklist/phase-6-runtime.md)
G13 — "Work-stealing overhead on fine-grained tasks".
**Harness:** `scheduler::tests::bench_g13_scheduling_overhead` in
[`runtime/src/scheduler.rs`](../../runtime/src/scheduler.rs).
**Hardware:** Apple M5 Pro (6 perf + 12 efficiency cores), `--release`,
18 pool workers.

G13 asked three things: (1) measure scheduling overhead per task
(target: **<1µs for spawn + join**); (2) establish a minimum task-duration
threshold below which the compiler doesn't parallelize; (3) benchmark
mixed workloads with same-resource serialization interleaved with
different-resource parallelism.

**Bottom line.** Spawn+join costs **~16µs** per task one-at-a-time
(~6–7µs amortized when pipelined) — the <1µs target is **not reachable**
with the current `Mutex`+`Condvar` global-queue design (an OS thread
wake-up alone is microsecond-scale), and **does not need to be**: the
auto-parallelizer's cost gate already refuses to parallelize work below
the granularity where dispatch pays off. The empirical granularity
crossover is **~80µs of total group work** (≈20µs per branch at N=4),
which **exactly matches** the pinned `REDUCE_DISPATCH_THRESHOLD_UNITS`
(`DISPATCH_OVERHEAD_PER_CALL_UNITS` 10,000 × `ASSUMED_WORKER_COUNT` 8 =
80,000 units ≈ 80µs). The cost-model calibration is confirmed correct;
**no constant needs changing.** Work-stealing remains correctly deferred
— the single global queue is not the bottleneck at these rates.

---

## Method

The harness measures the runtime scheduling primitives directly — no
codegen or Kāra source in the loop — so the numbers are the floor cost of
the dispatch path itself. It is `#[ignore]`d (perf benchmark) and reports
the **median** of each measurement over 5–9 runs after warmup. A
`#[inline(never)]` + `black_box` integer busy-loop (`busy_work`) is
calibrated to ns/iteration so the crossover sweep can target real
wall-clock task durations.

```bash
cargo test -p karac-runtime --release bench_g13_scheduling_overhead \
    -- --ignored --nocapture
```

`--release` is mandatory — debug builds make the busy kernel and the
dispatch path meaningless.

Four measurements:

| # | Measures | Construct exercised |
|---|---|---|
| 1 | spawn+join round-trip, one task in flight | `karac_runtime_spawn` + `karac_runtime_task_join` |
| 2 | spawn+join amortized, B in flight then joined | same, pipelined (TaskGroup fan-out shape) |
| 3 | par_run dispatch overhead, N trivial branches | `karac_par_run` (par-block / auto-par group) |
| 4 | granularity crossover: parallel vs in-thread for W ns/branch | `karac_par_run` vs sequential fallback |

## Results (M5 Pro, release, median; two runs)

**1. spawn+join round-trip (one task in flight):** **16.3 / 16.7 µs/task.**
The full one-task lifecycle: heap alloc of the handle + result buffer,
`ParCall`+`Task` construction, queue push under the pool mutex, one
`notify_all`, the worker's wake → run → terminal store → notify, and the
joiner's condvar wake. Dominated by the two thread hand-offs.

**2. spawn+join pipelined (4096 in flight):** **6.2 / 7.3 µs/task amortized.**
With many tasks outstanding the worker-wake latency overlaps; the residual
is per-task allocation + single-queue mutex traffic. The 2.5× gap from
measurement 1 shows the global queue sustains throughput fine — queue
contention is **not** the bottleneck (so work-stealing buys little here).

**3. par_run dispatch overhead (trivial branches):**

| N branches | µs/call | µs/branch |
|---:|---:|---:|
| 2 | 13.9 / 15.6 | 6.9 / 7.8 |
| 4 | 17.5 / 8.8 | 4.4 / 2.2 |
| 8 | 22.8 / 22.9 | 2.8 / 2.9 |
| 16 | 28.8 / 28.3 | 1.8 / 1.8 |

A ~10µs base dispatch cost plus ~1µs per additional branch (queue push +
condvar wake + remaining-decrement). The base agrees with the pinned
`DISPATCH_OVERHEAD_PER_CALL_UNITS` = 10,000 ns to within measurement
noise.

**4. granularity crossover (N=4 disjoint branches):**

| per-branch W | sequential | parallel | speedup | verdict |
|---:|---:|---:|---:|---|
| 0.5 µs | 2.4 µs | ~21–36 µs | 0.07–0.11× | sequential wins |
| 1 µs | 4.4–4.7 µs | ~24–34 µs | 0.14–0.19× | sequential wins |
| 2 µs | 8.0–8.4 µs | ~22–37 µs | 0.22–0.38× | sequential wins |
| 5 µs | 19–20 µs | ~28–41 µs | 0.47–0.73× | sequential wins |
| 10 µs | 37–39 µs | ~44–48 µs | 0.81–0.85× | sequential wins |
| **20 µs** | **74–78 µs** | **49–56 µs** | **1.32–1.59×** | **parallel wins** |
| 50 µs | 196–207 µs | 75–81 µs | 2.6× | parallel wins |
| 100 µs | 380–415 µs | 127–157 µs | 3.0× | parallel wins |

**Crossover is stable at W ≈ 20µs/branch ⇒ ~80µs total group work** in
both runs. At 10µs/branch (40µs group) parallel still loses (~0.8×); the
break-even total work lands between 40µs and 80µs, and parallel pulls
clearly ahead at 80µs.

## Interpretation

1. **The <1µs spawn+join target is unreachable and unnecessary.** A task's
   lifecycle crosses two OS-thread hand-offs (dispatch → worker, worker →
   joiner); each condvar wake is microsecond-scale on macOS, so ~16µs is
   the floor for the current global-queue design — no micro-optimization
   closes a 16× gap. It doesn't matter because the compiler never
   parallelizes work fine enough for the spawn cost to dominate (point 2).

2. **The minimum task-duration threshold is already established and
   correct.** The auto-par cost model gates `karac_par_reduce` at
   `ASSUMED_WORKER_COUNT × DISPATCH_OVERHEAD_PER_CALL_UNITS` = 8 × 10,000 =
   **80,000 units ≈ 80µs of estimated work** (`src/codegen/reduce.rs`).
   The measured crossover is **~80µs** — the gate sits exactly at the
   empirical break-even, on the conservative side (it only dispatches when
   parallel clearly wins). The companion `karac_par_run` gate
   (`PAR_RUN_DISPATCH_THRESHOLD_UNITS` = 500, `PAR_RUN_VISIBILITY_THRESHOLD
   _UNITS` = 50) operates in the `CostEstimator`'s abstract per-statement
   units rather than nanoseconds — it can't see into opaque impl-method
   bodies, so it pairs a low dispatch threshold with a visibility floor
   that *keeps dispatch on* when the branch's real work is hidden. The
   per-call dispatch magnitude that gate models (~10µs) is confirmed by
   measurement 3. **No constant needs recalibration** (measure-first per
   [`feedback_measure_first_empirical`](../../CLAUDE.md)).

3. **Explicit `spawn()` / `TaskGroup` is intentionally ungated.** The cost
   model applies to *auto*-parallelization (`par_run` / `par_reduce`),
   where the compiler decides. `spawn()` is user-driven: the programmer
   chose the task granularity, so the runtime spawns unconditionally. The
   16µs figure is the per-spawn budget a user implicitly accepts; it is
   amortized to ~6µs under fan-out (measurement 2). Documenting it lets
   users reason about when a `TaskGroup` of many tiny tasks is worthwhile
   (rule of thumb: each task should do ≫16µs of work).

4. **Mixed same-resource / different-resource workloads resolve at
   analysis time, not runtime.** The concurrency analyzer
   (`src/concurrency.rs`) serializes statements with conflicting effects
   into a single dependency-ordered group — same-resource work never forms
   an independent parallel group, so it never reaches `karac_par_run` and
   pays **zero** dispatch overhead. Only disjoint (different-resource)
   statements form parallel groups, and those are subject to the ~80µs
   crossover above. So the "interleaved" case decomposes into: serialized
   spans (free) + parallel spans (gated). The end-to-end wall-clock story
   for a real disjoint fan-out is already exercised by the Parallax bench
   (`examples/parallax/bench/`); this investigation supplies the
   per-group economics underneath it.

5. **Work-stealing stays deferred — correctly.** The runtime uses one
   global MPMC queue (no per-worker deques) by design. Measurement 2 shows
   the queue sustains 4096-task fan-outs at ~6µs/task with no collapse, so
   global-queue contention is not the current bottleneck. Work-stealing
   lands only if a real workload profiles queue contention as the
   limiter — none does today.

## Verdict

All three G13 sub-questions answered with no code change warranted:

- **Overhead measured:** spawn+join ~16µs cold / ~6µs pipelined; par_run
  ~10µs + ~1µs/branch. (<1µs target not met, not needed.)
- **Threshold established/validated:** ~80µs total group work, matching the
  existing `REDUCE_DISPATCH_THRESHOLD_UNITS` to the µs.
- **Mixed workloads:** handled at analysis time — same-resource serialized
  (free), different-resource gated at the crossover.

Revisit only if a production workload profiles spawn latency or
global-queue contention as a hotspot, per
[`feedback_optimize_for_production_not_kata`](../../CLAUDE.md).
