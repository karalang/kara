# `Vec.sort_by` vs Rust's `sort_by` — where the gap actually is

**Status:** measured, and the adaptivity half **landed** (B-2026-08-10-9).
The gap reproduces. The cause is **not** codegen quality — karac's lowering
is at parity with rustc for the same algorithm — it is the **algorithm**: a
fixed-32-run bottom-up merge sort with no adaptivity, versus Rust's
driftsort. No contained merge-kernel tweak is a reliable win; the measured
options are all listed below with the data that rejects them. The fix that
shipped is a natural-run merge sort — see "What landed". The remaining
shuffled-uniform gap needs a stable quicksort and is tracked separately.

## The claim under test

B-2026-08-10-9 filed an observation from kata #253: `Vec[(i64,i64)].sort_by`
runs ~2x slower than Rust's `sort_by` on the same data, with the row's own
stated next step being *"compare the lowered sort against what Rust's adaptive
sort does on shuffled-uniform input."* The row explicitly made no claim about
which algorithm karac lowers to.

The row's framing (`surface: codegen`) implies a lowering-quality problem.
That is the hypothesis this spike falsifies.

## Method

- Host: `x86_64` Intel Xeon @ 2.80 GHz, 4 vCPU, 16 GB, **shared cloud
  container** — the same class of host as the original measurement, *not* the
  canonical Apple-silicon bench host. See "Caveats".
- Workload: `n = 150_000` elements of `(i64, i64)`, comparator
  `|x, y| x.0.cmp(y.0)`, i.e. the row's kernel.
- karac timings are **pure sort**: measured as the 1-round/25-round slope
  (which removes process startup and the input build), then with a
  clone-only build subtracted (which removes the per-round `base.clone()`,
  measured at 1.13–1.23 ms and flat across patterns).
- Rust timings are best-of-11 `Instant` around the sort alone.
- Every run asserts the output is sorted, so no variant is vacuously fast.
- Six input patterns, because a sort measured only on shuffled-uniform data
  hides exactly the adaptivity differences that dominate real workloads.

Harness lives in the session scratchpad (`sortbench/`); it is a handful of
self-contained `.rs` and `.kara` files, reproducible from the numbers here.

## Result 1 — it reproduces

End-to-end on the row's kernel (clone + sort, per round):

| | per round |
|---|---|
| karac | 15.1 ms |
| Rust `sort_by` | 8.7 ms |
| **ratio** | **1.74x** |

The row reported 2.1x isolated on its host. Same phenomenon.

## Result 2 — the decisive control: codegen is at parity

Hand-writing karac's *exact* algorithm in Rust (insertion-sort RUN=32 base,
bottom-up ping-pong merge, `cmp <= 0` takes left, raw pointers so there are no
bounds checks) **and** karac's exact comparator lowering (user body yields an
`Ordering` tag, codegen subtracts 1, merge tests `<= 0`):

| pattern | karac | rustc, same algorithm | karac / rustc | Rust `sort_by` |
|---|---|---|---|---|
| random | 14.91 ms | 15.6–16.2 ms | **0.92–0.95x** | 7.0–7.6 ms |
| sorted | 4.91 ms | 3.74–3.78 ms | 1.30x | 0.09 ms |
| reverse | 6.65 ms | 5.09–5.14 ms | 1.30x | 0.17 ms |
| few-unique | 6.43 ms | 5.34–5.40 ms | 1.19x | 1.51 ms |
| sawtooth | 4.99 ms | 4.22–4.30 ms | 1.17x | 4.38 ms |

**On the row's own case (random), karac is 5–8% _faster_ than rustc compiling
the identical algorithm.** Across the other patterns karac trails by 17–30% —
a real but modest lowering gap, nowhere near 2x.

So the ~2x is not recoverable by improving codegen. Confirmed independently:
the emitted binary references no `karac_vec_sort_by`, so the inlined mono path
(`emit_sort_by_mono`) is what runs, as intended.

## Result 3 — the gap is much worse than 2x, and the row understates it

Comparing karac against Rust `sort_by` per pattern:

| pattern | karac / driftsort |
|---|---|
| random | 2.1x |
| **sorted** | **54x** |
| **reverse** | **39x** |
| few-unique | 4.3x |
| sawtooth | 1.14x |

The row was filed from a shuffled-uniform kata, which is the **narrowest**
gap of the five (sawtooth aside). karac's sort is completely non-adaptive: it
performs `ceil(log2(n/32))` = 13 full merge passes over 2.4 MB regardless of
input order. driftsort detects the existing run and does one scan.

Already-sorted and reverse-sorted inputs are not exotic — they are what you
get re-sorting a maintained list, sorting by a second key, or sorting output
of a previous sort. This is the larger exposure.

## Result 4 — where the time goes

Phase split of the algorithm (random input, rustc mirror):

| | |
|---|---|
| phase 1, insertion runs | 2.56 ms |
| phase 2, 13 merge passes | 10.73 ms |
| *13 bare `memcpy` passes (pure data-movement floor)* | *2.21 ms* |

Phase 2 costs ~5x its own memory traffic, so the merge is **latency- and
branch-bound, not bandwidth-bound**. Blocking for cache would therefore
recover at most ~1 ms of 13.

## Result 5 — rejected contained tweaks, with the data

All measured in the rustc mirror across patterns (best of 3 sweeps of 15
reps; the sweeps agreed to within ~3%, so these differences are real):

| pattern | A (today) | A+fastpath | cached | cached+FP | branchless | bl+FP | driftsort |
|---|---|---|---|---|---|---|---|
| random | 13.27 | 16.06 | 13.71 | **12.90** | **10.36** | 10.72 | 7.00 |
| sorted | 3.11 | 2.93 | 2.86 | 2.95 | 5.14 | 2.93 | 0.09 |
| reverse | 4.84 | 4.85 | 6.06 | 4.89 | 6.57 | 4.87 | 0.17 |
| few-unique | 4.41 | 5.48 | 4.83 | 4.39 | **7.94** | 8.08 | 1.49 |
| sawtooth | 3.37 | 4.21 | 3.25 | 3.23 | **6.70** | 6.16 | 4.35 |
| nearly-sorted | 3.51 | 4.44 | 3.78 | 3.60 | **7.03** | 6.80 | 5.05 |

- **Branchless merge** (select the value, add `zext(cond)` to the cursors) is
  a *trade, not a win*: 1.28x faster on random, but **1.8–2.0x slower** on
  few-unique, sawtooth and nearly-sorted, where the branch predictor already
  succeeds and the branchless form instead serialises on a loop-carried
  dependency (load → compare → cursor → next load address). Rejected.
  - Measurement note: a branchless merge must be written against a *direct*
    predicate. Routing it through the 3-way `cmp` (`-1/0/+1` then `<= 0`)
    reintroduces branches inside the comparator and produces the worst of
    both — an early version of this table read 16.2 ms on random (slower than
    branchy) for exactly that reason.
- **Ordered-run bulk-copy fast path** (`cmp(src[mid-1], src[mid]) <= 0` →
  `memcpy` the pair; plus a strict `src[hi-1] < src[lo]` block-swap, which is
  stable because strictness rules out ties across the boundary) is nearly
  free on random and helps ordered inputs — but only ~2x on sorted, because
  the 13 passes still happen, just as `memcpy`s.
- **Cached merge heads** (keep both run heads in registers, reload only the
  side that advanced) looked like an 18% win in isolation but shrinks to
  ~3–5% in the full sweep, and is slightly negative on nearly-sorted.
- **RUN tuning** (8/16/32/64) is flat within ~3%. Not a lever.

Nothing here is worth rewriting a load-bearing sort's IR for.

## Result 6 — algorithm dominates comparator inlining

The mono path exists (B-2026-07-30-2) on the premise that Rust wins by
monomorphizing the comparator into the sort. That premise is only half right:

| | inlined comparator | indirect (thunk) comparator |
|---|---|---|
| bottom-up merge sort | 15.9–16.2 ms | 24.4–25.4 ms |
| driftsort | **7.2 ms** | 10.4–10.8 ms |

Inlining is worth ~1.5x. The algorithm is worth ~2.2x. They compose (3.5x
end to end). **driftsort with an indirect call still beats an inlined merge
sort by 1.5x** — so inlining alone was never going to close this.

Both matter, and the target configuration is the one karac does not have:
adaptive stable algorithm *with* an inlined comparator, ~7 ms against today's
~15 ms.

## What the fix is

Replace the fixed-width bottom-up merge in `emit_sort_by_mono`
(`src/codegen/vec_method.rs`) with a **natural-run merge sort**:

1. **Phase 1** — walk the array detecting each maximal ascending run (and
   strictly-descending runs, reversed in place, which keeps it stable);
   extend any run shorter than `MIN_RUN = 32` by insertion sort. Record run
   boundaries in a scratch `i64` array.
2. **Phase 2** — merge adjacent runs pairwise from that boundary list until
   one run remains, keeping the existing ping-pong buffers and the existing
   `cmp <= 0` tie-break.

Properties:
- **Pure win, no trade.** On random input every natural run is ~2 elements,
  so phase 1 degenerates to today's 32-element insertion runs and phase 2 to
  today's 13 passes — same cost, plus one O(n) scan. On sorted input there is
  one run and phase 2 does nothing: the 54x and 39x cases collapse to a
  single scan.
- **Stability is preserved**, and the "IR mirrors the runtime's
  `sort_fixed_width` so both backends order equal elements identically"
  invariant is *automatically* preserved — any two stable sorts agree on
  output, so the two backends may use different stable algorithms. The
  runtime does not have to change in lockstep.
- **Panic-freedom is preserved** — one more null-checked `malloc` for the
  boundary array, no `unwrap`/`assert`, so the ~262 KiB DWARF symbolizer
  stays dead-strippable (see "Lean large-N sort entry").

This does *not* close the random-data case, which needs driftsort's other
half: a **stable quicksort** to build long runs when natural runs are short.
Partitioning beats merging there because the comparison is against a pivot
held in a register, so there is no dependent-load chain. That is a separate,
larger piece of work; the run-detection step above is the cheap half and is
strictly independent of it.

## What landed

The natural-run merge sort above, in `emit_sort_by_mono`. Measured on the
same host, karac pure sort (clone subtracted), before vs after:

| pattern | before | after | change | driftsort |
|---|---|---|---|---|
| random | 14.91 ms | 14.60 ms | unchanged | 7.0 ms |
| **sorted** | 4.91 ms | **0.14 ms** | **35x** | 0.09 ms |
| **reverse** | 6.65 ms | **0.19 ms** | **35x** | 0.17 ms |
| few-unique | 6.43 ms | 6.11 ms | 1.05x | 1.51 ms |
| sawtooth | 4.99 ms | **3.15 ms** | 1.58x | 4.38 ms |

The shape is exactly what the design predicted: **no change on shuffled
input** (natural runs there are ~2 elements, so `RUN` padding reproduces the
old 32-element runs and the old pass count) and an order of magnitude on
ordered input. Sorted and reverse-sorted now land within 1.1–1.6x of
driftsort instead of 39–54x behind it, and sawtooth is now *faster* than
driftsort. Few-unique barely moves, because random keys drawn from a small
alphabet still produce short natural runs — that case rides on the
stable-quicksort half.

Verification:
- Full `--features llvm` suite: 102 targets, 13,381 passed, 0 failed —
  including `codegen` 2882/0 and `memory_sanitizer` 1028/0. On Linux the
  latter runs LeakSanitizer, so it is the authoritative gate for the two
  extra allocations this change introduces.
- `valgrind --leak-check=full` at `KARAC_OPT_LEVEL=0` over a program hitting
  all three new paths (ordinary merge, the `nr == 1` skip, the descending
  reversal): 20 allocs / 20 frees, no errors.
- Sortedness + stability + permutation checked over 98 (pattern, size)
  combinations, AOT against `--interp` as oracle.
- Two regression tests pinned in `tests/codegen.rs`. Both have teeth against
  the specific way this can go wrong: with a non-strict (`>=`) descending
  extension, `natural_run_sort_keeps_descending_runs_stable` prints 33 before
  32 and 40 before 39, because a reversed run containing equal keys inverts
  them.

## The stable-quicksort half (B-2026-08-10-19): designed, validated, NOT shipped

Prototyped in the same Rust mirror against the *shipped* natural-run baseline.
Design: keep phase 1's run detection; when a run is short, build a long run by
stable 3-way quicksorting a span, then let phase 2's pairwise run merge run
unchanged over the (now far fewer) runs.

Measured at span = 16 K, against shipped:

| pattern | shipped | + stable qsort | |
|---|---|---|---|
| random | 13.8 ms | 11.6 ms | 1.2x |
| few-unique | 4.8 ms | 2.4 ms | **1.9x** |
| sorted | 0.10 | 0.10 | — |
| reverse | 0.18 | 0.18 | — |
| sawtooth | 1.88 | 1.93 | — |
| nearly-sorted | 3.01 | 2.97 | — |

A clean win with no regressions — but it leaves random at **1.9x driftsort
(6.0 ms)**, so it does *not* close B-2026-08-10-19. Weighed against ~600 lines
of IR in a load-bearing sort, that is a poor trade, so it is recorded here
rather than shipped. The few-unique 1.9x is the strongest argument for
building it anyway: low-cardinality sort keys (status, category, day) are
common.

**Why it stalls at 1.9x.** The 3-way branchless scatter costs 3 stores per
element per level, plus a full copy-back — ~2 loads + 4 stores per element per
level. driftsort partitions *into* scratch and alternates buffer roles across
levels (no copy-back) using a 2-way partition: ~1 load + 1 store. Matching it
means carrying a per-range buffer-parity flag on the stack and normalising at
the end — i.e. porting driftsort proper, a bigger project than this row.

### Three traps, all of which bit during prototyping

1. **Pivot choice is a correctness-of-performance issue, not a tuning knob.**
   A middle-element pivot is *catastrophic* on periodic data: sawtooth
   (`i % 1000`) regressed **44x–100x** (124 ms and 290 ms against a 2.8 ms
   baseline) because the middle of a period-1000 block is an unrepresentative
   small key, giving a ~5/95 split and O(n²) behaviour. **Median-of-3 does not
   fix it** — sampling positions 0/mid/last of a periodic sequence returns
   three correlated small keys. A fixed-seed xorshift random pivot does fix
   it (and keeps the binary reproducible, since only *performance* depends on
   the seed, never the output).
2. **Never quicksort a block just because the run at this position is short.**
   Doing so destroys order that already exists: `nearly-sorted` (1% random
   spikes in an ascending sequence) regressed **2.4x**, because each spike
   triggered quicksorting 4096 mostly-ordered elements. The correct policy
   accumulates only *consecutive* short runs, and probes with a cap (32) so
   discarding a long run costs O(32) rather than O(run).
3. **"Push the smaller, iterate on the larger" bounds RECURSION depth, not
   explicit-stack depth.** With an explicit stack a lopsided chain pushes one
   entry per iteration and never pops, so the stack grows to O(span/base) —
   this overflowed a 96-entry stack on sawtooth. The sound bound comes from a
   different argument: every stacked range is disjoint and strictly larger
   than the base cutoff, so the stack cannot exceed `span/base` entries.
   3-way partitioning is also what guarantees *progress* (the equal block is
   at least the pivot and is excluded from both sides), which a 2-way
   partition does not.

## Caveats

- **Single host, x86_64 shared container.** Absolute numbers drift a few
  percent between sweeps and more between runs of different binaries; ratios
  within a sweep are stable. None of this is from the canonical
  Apple-silicon bench host, and the branch-prediction-sensitive results
  (branchless especially) are the most likely to move on a different
  microarchitecture.
- Rust's `sort_by` is driftsort as of the toolchain in this container; the
  comparison is against that, not against stable-sort-in-general.
- The karac/rustc parity comparison holds the algorithm *and* the comparator
  shape fixed. It does not claim karac's codegen is at parity in general —
  only that it is not the cause of this gap.
