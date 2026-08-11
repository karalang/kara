# `Vec.sort_by` vs Rust's `sort_by` — where the gap actually is

**Status:** two fixes landed. The remaining ~1.30x on shuffled-uniform input
has now had five directions measured against it and none closes it
(B-2026-08-10-20); treat it as the cost of the algorithm, not a pending fix.

**Read the sections in order — later ones correct earlier ones.** This
document was written as the investigation ran, and its conclusions changed
twice as better measurements arrived:

1. *Results 1–6, "What the fix is", "What landed"* — the gap is the
   **algorithm**, not codegen quality (karac's lowering is 5–8% ahead of
   rustc on the identical algorithm). Fixed by a natural-run merge sort:
   35x on sorted and reverse-sorted input.
2. *"The real answer: branch mispredictions"* — the remaining shuffled-input
   gap was **entirely branch mispredicts**, not the algorithm. Fixed by a
   per-pass adaptive branchless merge kernel: 1.29x on random.
3. *"After the fix"* and *"The bounds-check hoist"* — the gap has now flipped
   to **instructions and latency**, and the second of those **withdraws** the
   first's conclusion that the partition direction is ruled out. Instruction
   budgeting was the wrong metric.
4. *"The calibrated kernel measurement"* and *"The budget check"* — said a
   2-way partition level is 1.87x cheaper per element than a merge pass and
   projected 1.24–1.43x. **Both are superseded by the last section**, which
   built the thing: it is never faster than the merge on shuffled input at any
   configuration. The budget was measured on one huge range and so captured
   the kernel's asymptotic cost, not its cost at the range sizes a recursion
   actually produces.

Everything rejected along the way is listed with the data that rejected it.

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

## The stable-quicksort half (B-2026-08-10-19): built, measured, NOT merged

> **Outcome, added after implementing it in IR.** The run-builder below was
> built in full and is correct, but it does **not** move shuffled-uniform
> input — the case the row is about. It is preserved unmerged on branch
> `claude/kara-open-bugs-list-6r5wke` (`93b438d`). Measured against the
> shipped natural-run sort: random 14.6 → 13.6–14.4 ms (noise-level),
> few-unique 6.11 → 3.88 (1.58x), everything else neutral. It also adds a
> fourth `malloc`/`free` to *every* sort, for a win only large few-unique
> sorts see; 1145 lines for that is not a trade worth making here.
>
> **Why, and the Rust prototype below OBSCURED this** — do not re-derive the
> plan from the prototype numbers. Building a 16 K run by quicksort takes
> `log2(span/16)` = 10 partition levels to replace `log2(span/32)` = 9 merge
> passes: **the pass count does not drop, it slightly rises.** The entire bet
> was that a partition level is cheaper *per element* than a merge pass, and
> in IR it is not — the branchless three-way scatter costs 3 stores per
> element. Writing the "less" bucket in place to eliminate the largest
> copy-back was also tried and changed nothing, which rules out the copy-back
> and points at the scatter stores.
>
> **So driftsort's advantage is not "quicksort instead of merge."** It is a
> partition cheap enough per level to beat a merge pass: 2-way, into
> **alternating** buffers (~1 load + 1 store, no copy-back, buffer parity
> carried per range on the stack and normalised at the end). A 2-way
> partition needs its own progress guarantee, since only 3-way gets one free
> from the equal block. Reusable from `93b438d`: the insertion-sort helper,
> the bounded explicit stack, the capped non-reversing probe, and the
> consecutive-short-run policy. Only the partition needs replacing.

### The prototype (kept for the design and the traps)

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

## The real answer: branch mispredictions (B-2026-08-10-19, FIXED)

Everything above reasons about the gap from *timings*. The measurement that
should have come first is `cachegrind --branch-sim`, which counts the work
instead of guessing at it. Cost of ONE 150k shuffled-uniform sort, taken as
the difference between the benchmark and a clone-only twin so the input
build and the clone cancel out, with both programs generating input
identically:

| | karac | driftsort | ratio |
|---|---|---|---|
| instructions | 52.79 M | 48.56 M | 1.09x |
| data references | 18.82 M | 30.59 M | **0.62x** |
| D1 misses | 1.091 M | 1.193 M | 0.91x |
| LL misses | 38.4 k | 37.6 k | 1.02x |
| conditional branches | 8.18 M | 10.30 M | 0.79x |
| **mispredicts** | **1.061 M** | **0.233 M** | **4.56x** |

karac executes **fewer instructions, fewer memory operations and fewer
branches** than driftsort, with an indistinguishable cache profile — and
loses. The 0.83 M excess mispredicts at ~20 cycles each is 5.9 ms, and the
measured gap was 14.82 − 8.93 = 5.9 ms. The whole gap is one number.

The same counters across patterns make it inescapable:

| pattern | instructions | mispredicts | rate | time |
|---|---|---|---|---|
| random | 52.79 M | 1.061 M | **12.96%** | 14.82 ms |
| few-unique | 50.79 M | 0.221 M | 2.75% | 6.51 ms |
| sawtooth | 23.90 M | 0.073 M | 1.86% | 3.55 ms |

random and few-unique run **essentially the same instruction count** and
differ 2.3x in wall clock.

This retires the diagnosis this row was filed with. "It needs driftsort's
other half, a stable quicksort" was the wrong conclusion drawn from the
right observation — partitioning *is* better than merging on shuffled data,
but not because of pass counts or memory traffic. It is better because a
partition compares against a pivot in a register and can be written
branchlessly. What was actually needed was branchlessness, which a merge can
also have.

### What landed: a per-pass adaptive merge kernel

Phase 2 now emits **two merge kernels with identical output** and picks one
per pass. The branchless one selects the value and advances both cursors by
`zext` of the `cmp <= 0` predicate, so the CPU never speculates.

Unconditional branchlessness is still the bad trade the table in Result 5
says it is — on ordered-ish input it replaces speculation that was working
with a serial `load -> compare -> cursor -> next-load` dependency chain. So
the kernel choice has to answer exactly one question: *would the hardware
predictor do well here?* The cheapest honest way to answer it is to simulate
one. The first `PROBE = 256` outputs of each pass run the branchy kernel
while feeding each take through a 16-entry, 1-bit-per-entry predictor
indexed by 4 bits of global take history — six integer ops on one register.
Above a 40% simulated miss rate the pass commits to the branchless kernel.

Three design points that are load-bearing, all found by measurement:

- **A switch-rate counter is the wrong signal.** It reads 1.0 on perfect
  alternation — which is exactly what merging two identical runs produces on
  sawtooth input, where the hardware mispredicts 1.86%. It would route the
  best-predicted case straight to the branchless kernel.
- **The history width is not a round number.** A sawtooth merge at pass `k`
  has period `2^k` (alternation, then AABB, then AAAABBBB). An n-bit history
  predicts a period-`p` sequence perfectly exactly when `p <= 2^n`. A first
  version used 2 bits and a 1-in-3 threshold; it read 25% on AAAABBBB, close
  enough to misroute, and measured **8% and 6% regressions** on few-unique
  and sawtooth. 4 bits covers periods to 8 and degrades to one miss per
  period beyond that (6% at period 16).
- **The probe needs a length gate.** A pass emits about `len` outputs, so on
  a 64-element sort the 256-output budget covers *all* of phase 2 and the
  simulation stops being amortised — an estimated ~5% tax on exactly the
  small-Vec sorts that are the common case. Below `4 * PROBE` the budget is
  set to zero, which the strict threshold resolves to the branchy kernel, so
  a short sort executes precisely the code it did before this change.

### Result

Interleaved A/B, both binaries measured in one process so host drift hits
both equally, karac pure sort with the clone subtracted:

| pattern | baseline | adaptive | change |
|---|---|---|---|
| random | 14.22 ms | **11.05 ms** | **1.29x** |
| sorted | — | — | unchanged (see below) |
| reverse | — | — | unchanged (see below) |
| few-unique | 6.13 ms | 5.96 ms | 1.03x |
| sawtooth | 3.38 ms | 3.04 ms | 1.11x |

Three sweeps put random at 1.28x, 1.32x and 1.29x, and few-unique and
sawtooth between 0.98x and 1.11x — i.e. at or slightly above parity, with
the spread being host noise rather than a real gain on those two.

Sorted and reverse-sorted are a single natural run, so phase 2 is skipped
and the kernel choice never happens. Their timings sit at the harness noise
floor (the clone dominates, and the subtraction amplifies the noise —
successive sweeps read 4.30x and 0.84x on the same pair of binaries), so
they are settled by instruction count instead, which is deterministic:
**±1 instruction out of 3.2 M and 4.0 M**. The path is untouched.

The routing is confirmed by counters, not inferred from timings. With the
4-bit predictor, few-unique and sawtooth report mispredict counts **identical
to baseline** — 0.221 M and 0.073 M, to the digit — meaning every one of
their passes stayed branchy, while random collapses 1.061 M -> 0.192 M. The
branchless kernel costs 1.40x the instructions on random (52.79 M -> 74.07 M)
and wins anyway, which is the same point the diagnosis makes, from the other
direction.

Verification:
- Full `--features llvm` suite green, including `codegen` and
  `memory_sanitizer` (LSan on Linux).
- Sortedness + stability + permutation over 98 (pattern, size) combinations,
  AOT against `--interp` as oracle.
- Element-type coverage, since the branchless kernel `select`s a whole
  element rather than a key: bare `i64`, a mixed-width `(i64, i32, i64)`
  tuple and an all-int-field named struct, 4096 shuffled elements each,
  agreeing across AOT, LLJIT and the interpreter.
- `valgrind --leak-check=full` at `KARAC_OPT_LEVEL=0` over a program hitting
  the probe kernel, both merge kernels, the `nr == 1` skip and the descending
  reversal: 28 allocs / 28 frees, 0 bytes in use at exit, 0 errors. The
  change adds no allocation — the probe state is six allocas.
- Two regression tests in `tests/codegen.rs`. The behavioural one has teeth:
  weakening the branchless tie-break to `cmp < 0` fails it. **All 79
  pre-existing sort tests still pass with that break in place** — every other
  sort test in the file is too small or too ordered to leave the branchy
  path, which is why the second, structural test pins the kernels' presence
  in the IR. The two kernels produce identical output by construction, so no
  behavioural test can observe which one ran.

### What is left

random is now ~1.28-1.33x driftsort, down from ~1.66x. The residual has a
known cause: the branchless merge trades mispredicts for a serial
`load -> compare -> cursor -> next-load` chain of ~7 cycles per element,
because the next load address depends on the comparison. A partition loop
has no such chain — its read pointer advances unconditionally, so loads run
ahead freely. That, and not pass counts, is driftsort's remaining edge.
Refiled as B-2026-08-10-20.


## After the fix: the gap flipped from mispredicts to instructions (B-2026-08-10-20)

The section above closes with "a partition loop has no dependency chain, that
is driftsort's remaining edge" and hands that to B-2026-08-10-20. Measuring it
before building it says the direction is **not worth taking**, and the reason
is that the adaptive kernel already spent the payoff it was counting on.

Instruction and mispredict counts for one 150k shuffled sort, all differenced
against a clone-only twin on the same host:

| build | instructions | mispredicts | time |
|---|---|---|---|
| natural-run, branchy merge (pre-19) | 52.79 M | 1.061 M | 14.22 ms |
| **adaptive kernel (shipped)** | **74.07 M** | **0.192 M** | **11.05 ms** |
| 3-way quicksort run-builder (`93b438d`) | 87.02 M | 0.485 M | ~14 ms (wash) |
| driftsort | 48.56 M | 0.233 M | 8.2–8.9 ms |

Read the first two rows against the last one and the whole picture inverts:

- **Before B-2026-08-10-19**, karac ran 1.09x driftsort's instructions and
  4.56x its mispredicts. It was mispredict-bound.
- **After**, karac runs **1.53x** driftsort's instructions and **0.82x** its
  mispredicts — it now mispredicts *less often than driftsort does*. It is
  instruction-bound.

The branchless kernel bought its 1.29x by spending instructions to buy
mispredicts: +21.3 M instructions for −0.87 M mispredicts. That was a good
trade at ~20 cycles per mispredict, and it is a trade that can only be made
once.

### Why that kills the partition direction

The 3-way quicksort row in the table is the same trade made badly: **+34.2 M
instructions to save 0.58 M mispredicts**, which cancel almost exactly — a
complete, quantitative explanation of the wall-clock wash that the original
prototype could not account for. It also settles the contradiction flagged in
that section (removing the copy-back "changed nothing"): `memcpy` is only
9.57 M instructions, 10.6% of the program, so the copy-back was never where
the cost was.

Where it actually was, from the emitted loop: the branchless 3-way scatter
writes the element to **all three buckets on every iteration**. For a 16-byte
element that is 6 stores per element per level, plus three cursor updates —
about 47 instructions per element per partition level.

Budget the 2-way alternating-buffer rewrite against that, using measured
per-element instruction costs (adaptive build: 74.07 M total, less ~0.9 M for
run detection and ~9.6 M for insertion padding, over 1.95 M merge elements):

| | instructions/element |
|---|---|
| branchless merge element | ~33 |
| 3-way partition element (measured) | ~47 |
| tight 2-way partition element (estimated) | ~24 |

Ten partition levels at 24 replaces nine merge passes at 33: 240 vs 297, a
saving of ~57 instructions per element, or **~8.6 M — about 12%**. That is
the *best case* for roughly a thousand lines of IR in a load-bearing sort,
and it no longer has a mispredict win stacked on top of it, because there are
only 0.192 M mispredicts left to remove.

**So the row's stated fix is worth ~12% at best.** Not the 1.3x it is filed
for.

### What is actually worth doing instead

The same accounting names cheaper levers with comparable payoff, because the
target is now instructions per merge element rather than the algorithm:

- **Hoist the two per-element bounds checks.** The branchless kernel tests
  `a < mid` and `b < hi` on every element. Running `min(mid - a, hi - b)`
  iterations with no bounds test and re-checking only at the block boundary
  removes ~5 instructions per element — **~9.8 M, about 13%** — which is
  *more* than the full 2-way partition rewrite, for a fraction of the work.
  This is the recommended next step.
- Fewer instructions in the element move itself. A `(i64, i64)` element costs
  2 loads, 2 selects and 2 stores because it is selected as a whole struct.

### RUN is still not a lever, re-measured

Worth re-testing after the kernel change, since the original "flat within 3%"
finding was taken under the branchy merge and a cheaper phase 2 should in
principle favour a smaller insertion base. It does not — swept with a
temporary `KARAC_SORT_RUN` override so one build covers every point:

| pattern | RUN=4 | RUN=8 | RUN=16 | RUN=32 | RUN=64 |
|---|---|---|---|---|---|
| random | 11.05 | 11.06 | 10.59 | 10.79 | 11.20 |
| few-unique | 5.26 | 5.44 | 5.14 | 5.67 | 6.14 |
| sawtooth | 3.00 | 2.93 | 2.99 | 2.89 | 2.86 |

Flat within ~3% on random across a **16x** range of RUN, which means the
insertion cost per element and the extra-pass cost stay balanced over the
whole range. Only RUN=64 is clearly worse, and only on few-unique. Not a
lever, under either kernel.


## The bounds-check hoist: measured, NOT merged — and it corrects the section above

The section above recommends hoisting the branchless kernel's two per-element
bounds checks and budgets it at ~13%. It was built and measured. **It does not
pay, and measuring it invalidates the reasoning that produced the ~12% figure
used to rule out the partition direction.** Read this section before acting on
that one.

### The change

`p2.bl.chk.a` / `p2.bl.chk.b` test `a < mid` and `b < hi` on every element.
Only one cursor moves per step, so `min(mid - a, hi - b)` steps are safe with
no test at all: after j steps `a <= a0 + j` and `b <= b0 + j`, and
`j < min(...)` bounds both. The kernel becomes

    blk:   ra = mid - a;  rb = hi - b;  rem = min(ra, rb)
           if rem < 1 -> drain
           kend = k + rem
    ubody: <branchless take>;  if k+1 < kend -> ubody else -> blk

with the loop test riding on `k`, which already increments every step, so no
separate counter is needed. The bounds-checked loop is then dead and was
deleted outright — keeping it as a small-`rem` fallback measured **worse**
(see below).

On shuffled input the safe count shrinks geometrically (L, ~L/2, ~L/4), so a
2L-element merge needs only ~log2(L) blocks. The collapsed-`min` case that
would make blocks expensive belongs to wildly unequal runs, which is
partially-ordered input — routed to the branchy kernel and never reaching
this code.

### The result

Interleaved A/B against main, three sweeps, plus deterministic counters:

| pattern | wall clock | instructions | cond branches | mispredicts |
|---|---|---|---|---|
| random | **+3–4%** | 74.05 → 67.33 M (**−9.1%**) | 7.33 → 5.61 M (−23%) | 0.192 → 0.226 M |
| few-unique | **−2–3%** | 48.85 → 47.69 M (−2.4%) | 8.05 → 7.27 M | unchanged |
| sawtooth | neutral | 22.53 → 21.90 M (−2.8%) | 3.94 → 3.43 M | unchanged |
| sorted / reverse | unchanged | unchanged | — | — |

Net across the five patterns is roughly zero, so it was not merged. The
few-unique regression is reproducible across all three sweeps and has **no
signature in any counter available here** — fewer instructions, fewer
branches, unchanged D1/LLd/I1 misses, unchanged mispredicts — which points at
µop-cache or alignment effects that cachegrind does not model, and which any
unrelated change could flip either way.

An earlier version that kept the checked loop as a `rem < 8` fallback was
strictly worse: the duplicated take cost **+2.4% instructions on few-unique
and +1.8% on sawtooth — patterns that never execute this kernel** — purely
through register pressure in the shared function, for a 3–5% wall-clock
regression. Worth remembering generally: in a function this large, a second
copy of a hot loop is not free for the paths that never run it.

### Why this invalidates the "~12%, therefore ruled out" argument

The headline number is that **−9.1% of instructions and −23% of branches
bought +3.5% of wall clock.** The branchless merge is therefore *not*
throughput-bound on instruction count — it is **latency-bound on its
loop-carried dependency chain** (`load -> compare -> cursor -> next load
address`, ~7 cycles). Removing instructions from around that chain does not
shorten it.

That is exactly the metric the previous section used to dismiss the partition
direction, and it is the wrong one:

- Instruction budgeting **over-predicts** changes that only delete
  instructions. This change is the proof: budgeted at ~13%, delivered ~3.5%.
- It **under-predicts** changes that shorten the critical path — and the
  partition direction is precisely such a change. Its claim was never "fewer
  instructions" (a 2-way partition element is ~24 against a merge element's
  ~33, a modest edge); its claim is that a partition loop has **no
  loop-carried dependency on the load address**, because the read pointer
  advances unconditionally. Iterations overlap; merge iterations cannot.

So the "~12% best case, therefore not worth it" conclusion is **withdrawn**.
It costed the partition direction on the one axis where partitioning does not
compete, and ignored the axis it was proposed for. The direction is
**unresolved**, not ruled out.

What still stands from that analysis, unaffected:

- The gap flipped from mispredicts to instructions/latency: karac now runs
  1.53x driftsort's instructions and 0.82x its mispredicts.
- The 3-way prototype's failure is fully explained by its instruction count
  (+34.2 M against −0.58 M mispredicts), and its ~47 instructions per element
  per level came from writing the element to all three buckets. That
  explanation does **not** generalise to a tight 2-way loop.

### The measurement that would actually decide it

Achieved **cycles per element per partition level** for a 2-way branchless
partition in this codegen — not instructions, and not extrapolated from the
3-way prototype, whose 6-stores-per-element scatter contaminates the figure.
If a partition level runs near 2–3 cycles/element against the merge's ~7,
ten levels replacing nine passes is a large win in time while being roughly
neutral in instructions. If it runs at 6+, the direction is dead for real.
Nothing measured so far distinguishes those two worlds.


## The calibrated kernel measurement: the partition direction is ALIVE

The previous section says the deciding question is achieved **cycles per
element per partition level**, and that nobody had measured it. Measured now.
**A 2-way branchless partition level is 1.66–1.74x cheaper per element than a
branchless merge pass**, and the direction is worth building.

### Calibration first — the step that makes this mirror trustworthy

A mirror already misled this investigation once: the Rust prototype of the
3-way quicksort projected 1.2x and the IR delivered 0x. So this mirror is
**calibrated against a kernel karac already emits** before its number for the
kernel karac lacks is read at all. If it cannot reproduce the merge, its
partition figure means nothing.

The calibration target was measured, not estimated. A karac program builds
150k shuffled pairs and pre-sorts each 64-element block **once, in setup**, so
every natural run is exactly 64: phase 1 then does detection only, no
insertion padding, and the per-round time is almost purely phase 2's 12 merge
passes. karac achieves **3.944 ns/element/pass**.

The first mirror attempt **failed that gate** at 6.411 ns — 1.63x off. The
cause is worth knowing, because it is a trap for anyone writing a C or Rust
mirror of this kernel: in clang, how the element select is written swings the
merge by 1.9x.

| merge formulation | ns/element/pass |
|---|---|
| direct predicate, struct-valued ternary `dst[k] = t ? x : y` | 7.727 |
| karac's Ordering-tag lowering, struct ternary | 5.477 |
| direct predicate, mask arithmetic | 5.108 |
| direct predicate, per-field ternary | 4.374 |
| **direct predicate, address select `dst[k] = *(t ? &src[i] : &src[j])`** | **4.033** |
| *karac, measured* | *3.944* |

Only the address-select form reproduces karac (within 2.3%). Note what that
says about karac: **its emitted struct `select` is already at the best of the
five formulations**, comfortably ahead of what the obvious C spelling gets.

### The result

With the merge written in the calibrated form, both kernels in one program,
three runs (each also asserting its own correctness — the merge must sort, the
partition must bucket *and* stay stable):

| kernel | ns/element/level | notes |
|---|---|---|
| branchless merge pass | 4.06 – 4.11 | calibrated against karac's 3.944 |
| **2-way partition, 2-pass** | **2.34 – 2.47** | count pass + scatter; 2 loads, 1 store |
| 2-way partition, 1-pass 2-buffer | 1.90 | 1 load, 2 stores; needs a concatenation |

**Ratio: 1.66–1.74x.** The 2-pass form counts `nlt` first, then scatters with
`dst[is_lt ? p : q] = x`, so both passes read `src[i]` with `i` advancing
unconditionally — no loop-carried dependency on the load address, which is
exactly the property the merge lacks. Deeper levels also work on blocks that
fit in cache, where every merge pass streams the full 2.4 MB.

The two inner loops, so this is reproducible on another host without the
scratchpad (element is `struct { long k, v; }`, 150k uniform keys, `cc -O2`):

```c
// merge pass — the calibration kernel, address-select form
while (i < mid && j < hi) {
    int take_a = (src[i].k <= src[j].k);
    dst[k] = *(take_a ? &src[i] : &src[j]);
    i += take_a; j += !take_a; k++;
}

// partition level — the kernel under test, count then scatter
int nlt = 0;
for (int i = lo; i < hi; i++) nlt += (cur[i].k < pivot);
int p = lo, q = lo + nlt;
for (int i = lo; i < hi; i++) {
    pair x = cur[i];
    int is_lt = (x.k < pivot);
    oth[is_lt ? p : q] = x;          // one store, index selected
    p += is_lt; q += !is_lt;
}
```

Drive the merge over 12 passes from run width 64, and the partition over 10
levels tracking real (uneven) block boundaries with each block's value range
supplying its midpoint pivot — a fixed `n / 2^level` block size silently
desynchronises from the splits the partition actually produces.

### Projection onto the real sort

Using karac's own measured per-pass cost (0.5916 ms/pass at 150k) and the
observed phase split (total ~10.87 ms = phase 1 ~3.18 + phase 2 ~7.69):

| | current | quicksort route |
|---|---|---|
| phase 1 | 3.18 ms (insertion base 32) | ~1.74 ms (base 16, half the shifts) |
| run building | — | 3.51 ms (10 partition levels) |
| merging | 7.69 ms (13 passes) | 2.37 ms (4 passes) |
| **total** | **10.87 ms** | **~7.6 ms** |

That is **~1.43x**, which would put karac at ~7.6 ms against driftsort's
8.2–8.9 on this host — i.e. at or slightly ahead of it, closing the row.

### The budget the IR has to hit — and why the last attempt missed

This is a projection from a mirror, and the 3-way attempt is proof that a
sound algorithm can still lose to a fat emitted loop. So the projection comes
with a **checkable per-element budget**, and the two now reconcile exactly:

- The mirror's 2-pass partition is ~11–13 machine instructions per element per
  level.
- The 3-way attempt emitted **~47**, because it wrote the element to all three
  buckets every iteration. At ~4 instructions/cycle that is ~4.2 ns/element —
  *worse than a merge pass at 3.94*, which is precisely the wall-clock wash it
  measured. The model is now consistent end to end.

**So the requirement is: emit ≤ ~15 instructions per element per partition
level.** That is checkable cheaply and early, long before the whole
run-builder exists — build the partition loop alone, run one level under
`cachegrind`, divide Ir by elements. If it comes out near 12, continue; if it
comes out near 40, stop, because the 3-way already showed how that ends.

Remaining risks, unchanged: this is the x86 shared container and not the
canonical Apple-silicon bench host, and a mirror is not IR. What has changed
is that the direction now has a measured payoff and a falsifiable budget
rather than an argument.

## The budget check, in karac's own emitted code: PASS

The previous section set a falsifiable target — **≤ ~15 instructions per
element per partition level** — and said to check it early, on the partition
loop alone, before building anything on top. Done. **14.38. It passes.**

### Method

A temporary scaffold (`KARAC_SORT_PART=K`, since removed) replaced the mono
sort body with K rounds of exactly the 2-way partition the run-builder would
use — same `emit_sort_by_inline_compare`, same element loads and stores, same
alloca machinery — alternating buffers with no copy-back between levels. The
output is not sorted; only the instruction count is meaningful, and for a
branchless loop that count does not depend on the data, so unbalanced splits
cannot distort it.

Ir is linear in K, so the slope cancels allocation, the copy-back and every
fixed overhead:

| levels | Ir | cond branches | mispredicts |
|---|---|---|---|
| 0 | 2,882,005 | 142,757 | 6,254 |
| 1 | 5,217,249 | 208,488 | 6,302 |
| 2 | 7,214,176 | 261,610 | 6,315 |
| 4 | 11,545,547 | 380,358 | 6,357 |
| 8 | 20,208,365 | 617,867 | 6,323 |

Fit: `Ir = 2,941,517 + 2,157,317 * levels`, residuals ≤0.4% at K=2,4,8.

### Result

| | per element per level |
|---|---|
| **instructions** | **14.38** |
| branches | 0.39 |
| **mispredicts** | **0.0000** |

Zero mispredicts and 0.39 branches confirm the emitted loop really is
branchless and that LLVM unrolled it — the take never becomes a branch. The
figure is also structurally believable: the count pass reduces to a key load,
a compare and an add (~3), and the scatter to two loads, a compare, a cmov,
two stores, two adds and a GEP (~9), with loop control amortised by unrolling.

### Against the merge it would replace

Measured the same way, on the same host, from karac's own binaries:

| | instructions/element | vs merge |
|---|---|---|
| merge pass | 26.93 | — |
| **2-way partition level** | **14.38** | **1.87x cheaper** |
| 3-way partition level (abandoned attempt) | ~47 | 1.75x *more expensive* |

That last row is the retrospective the whole row needed: the 3-way loop cost
**more than the merge pass it was replacing**, so ten levels replacing nine
passes could only ever have been a loss. Its wall-clock wash was not bad luck
or a subtle microarchitectural effect — it was arithmetic, and this is the
number that would have predicted it in an afternoon.

### Wall clock, and an honest bracket

Timed by the 25-round slope method, even level counts only (odd counts pay a
copy-back):

| levels | ms/round |
|---|---|
| 0 (clone only) | 1.123 |
| 2 | 2.803 |
| 4 | 3.335 |
| 8 | 4.947 |

**3.0–3.2 ns/element/level**, against the merge's measured 3.944
ns/element/pass. That is a 1.27x time advantage where the instruction count
says 1.87x and the C mirror said 1.70x — and the gap is explained by a known
limitation of the probe: **it partitions the full 150k array at every level**,
so it never sees the cache locality a real recursive quicksort gets once
blocks drop below L2. The mirror modelled recursive blocking; the probe
deliberately does not.

So the whole-sort projection is a bracket, not a point:

| | phase 1 | run building | merging | total | vs today |
|---|---|---|---|---|---|
| today | 3.18 ms | — | 7.69 ms (13 passes) | **10.87 ms** | — |
| conservative (probe, no blocking) | 1.74 | 4.65 (10 x 3.1 ns) | 2.37 (4 passes) | **8.76 ms** | 1.24x |
| with blocking (mirror, 2.34 ns) | 1.74 | 3.51 | 2.37 | **7.62 ms** | 1.43x |

Against driftsort's 8.2–8.9 ms on this host that is **parity at worst, and
ahead at best**. Both ends of the bracket are worth having; the difference
between them is entirely whether the implementation recurses into
cache-resident blocks, which it should.

**Verdict: GO.** The kernel clears its budget in karac's real codegen, is
provably branchless, and is 1.87x cheaper per element than the merge pass it
replaces. What remains is the run-builder around it — pivot selection, the
bounded stack, the short-run policy — all of which already exist in `93b438d`
and none of which touch the partition loop this measured.

## Built it. It does not pay on the target case — and the budget check is why

The budget check said GO. The run-builder was then written, verified and
measured, and **on shuffled-uniform input it is never faster than the merge**,
at any configuration. It was not merged. This section records the result and,
more usefully, why the budget check pointed the wrong way.

### What was built

A stable 2-way quicksort run-builder, ~500 lines of emitter:

- `__vec_<m>_qs_<id>(data, scratch, lo, hi)` — count-then-scatter partition
  into alternating buffers, borrowing **phase 2's own scratch at matching
  indices**, so unlike the 3-way attempt it needs *no allocation at all*.
- Progress without a 3-way equal block: the split predicate is
  `cmp(x, pivot) < t` with `t` loop-invariant, and the partition simply
  re-runs with `t = 1` when `nlt == 0`. The retry cannot return 0 (the pivot
  is `<= pivot`), so the left half is then the block of elements equal to the
  pivot — already sorted and stable, no recursion needed. Two passes restore
  buffer parity, so nothing downstream knows a retry happened. **One kernel,
  not two.**
- Bounded explicit stack (`span / (base + 1)` by disjointness), fixed-seed
  xorshift pivot, insertion base case, and a phase-1 policy that accumulates
  only *consecutive* short runs so a long run ends the accumulation and is
  recorded untouched.

It is correct: 98/98 pattern × size combinations, element-type coverage
(`i64`, mixed-width tuple, named struct), agreeing across AOT, LLJIT and the
interpreter.

### The result

| pattern | main | + run-builder | instructions | mispredicts |
|---|---|---|---|---|
| random | 11.11 ms | 11.73 ms (**0.95x**) | 74.05 → 82.18 M (**+11%**) | 0.192 → 0.364 M |
| **few-unique** | 5.39 ms | **4.15 ms (1.30x)** | 48.85 → **34.25 M (−29.9%)** | 0.221 → **0.020 M** |
| sawtooth | 2.82 ms | 3.22 ms (0.87x) | 22.53 → 23.46 M (+4.1%) | unchanged |

And a sweep of the configuration space, to rule out a tuning miss:

| config | random | few-unique | sawtooth |
|---|---|---|---|
| span 512 | 12.39 | 5.90 | 3.11 |
| span 2048 | 12.27 | 5.49 | 3.15 |
| span 8192 | 12.04 | 4.84 | 3.34 |
| span 16384 | 12.33 | 4.51 | 3.31 |
| span 65536 | 11.85 | 7.21 | 3.11 |
| span 16384, base 32 | 11.81 | 4.70 | 3.30 |
| span 16384, base 64 | **11.17** | **4.38** | 3.14 |
| *main* | *11.11* | *5.39* | *2.82* |

**Nothing beats main on random.** The best configuration reaches parity.
Few-unique wins 1.23–1.30x throughout; sawtooth loses 10–18% throughout.

### Why the budget check misled — the lesson worth keeping

The probe measured **14.38 instructions per element per partition level** and
that number was correct. It was measured the wrong way round: the probe
partitioned the **full 150k array at every level**, so every loop had a huge
trip count, unrolled well, and amortised its per-range setup to nothing. It
measured the kernel's *asymptotic* cost.

A real quicksort spends most of its work on *small* ranges. Recursing from
16384 down to a base of 16 means roughly `2 * span / base` ranges, most of
them near the base size, and each one pays a fixed cost — pivot xorshift plus
a 64-bit `urem`, stack push/pop, block dispatch, two helper calls — over very
few elements, with loops too short to unroll usefully. The merge has no such
problem: every pass runs one long loop per run pair, so it sits near *its*
asymptotic cost already.

So the comparison "14.38 vs 26.93" was never between comparable things. The
correct budget would have measured the partition **at the range sizes the
recursion actually produces** — a geometric mix from `span` down to `base`,
not one range of `n`. Concretely, the probe should have partitioned
`n / 2^d`-sized blocks at level `d`, exactly as the C mirror did, rather than
the whole array each time.

That also explains, retrospectively, why the C mirror was closer to right
(1.70x, and it *did* model recursive blocking) than the karac probe (1.87x on
instructions) — and why even the mirror was optimistic: it modelled recursion
but still in C, without the per-range helper calls and `urem` the IR pays.

### Standing conclusion

Five directions have now been measured against the shuffled-uniform residual —
merge-kernel tweaks, RUN tuning, the bounds-check hoist, a 3-way quicksort
run-builder, and a 2-way one — and none of them closes it. The remaining
~1.30x should be treated as the cost of the algorithm karac has, not as a bug
with a pending fix.

The one unexploited finding is **few-unique**, where the run-builder is a
genuine 1.23–1.30x with a 30% instruction reduction and a 10x mispredict
reduction. Anything that pursues it has to earn back the sawtooth regression
first, which is code-growth rather than algorithm — sawtooth never executes
the quicksort at all, yet its instruction count rises 4.1% with identical
branch and mispredict counts.

Those two halves have different futures, so they were split into two ledger
rows rather than left as one:

- **B-2026-08-10-20** — the shuffled-uniform residual, `wontfix`. Real,
  reproducible, five directions measured, no action left. It stays readable
  (the `wontfix` section of `bug-ledger.md` renders titles in full) so nobody
  re-derives the dead ends, but it is out of the open queue.
- **B-2026-08-11-10** — the few-unique gap, `open`. karac 5.39 ms vs
  driftsort's 3.6–4.1, with a measured route to 4.15 (parity) that is blocked
  only by the sawtooth regression. That one is work, and it is filed as work.

The distinction is worth stating because it is easy to get wrong in the other
direction: a row kept open out of reluctance to admit a dead end costs the
queue its meaning, and a finding closed along with its dead-end parent gets
lost.

## The driftsort baseline was wrong on few-unique — the route is not to parity (B-2026-08-11-10)

Re-measured on 2026-08-11 with a rebuilt harness, now checked into
[`sortbench/`](sortbench/) so it stops being lost. The rebuild was validated
before it was believed, on two host-independent numbers this document already
records:

| | 2026-08-10 | 2026-08-11 rebuild |
|---|---|---|
| karac few-unique, instructions | 48.85 M | **48.8 M** |
| karac sawtooth, instructions | 22.53 M | **22.5 M** |
| clone-only slope | 1.13–1.23 ms | **1.13–1.24 ms** |

Three significant figures on both instruction counts, from an independently
written harness. Whatever follows is not a harness artifact.

### The correction

The driftsort side does not hold up. Measured on the same host, in the same
session, against an input generator mirrored line-for-line from the Kāra one
and with `black_box` guarding the Rust timing loop:

| pattern | karac | driftsort (this doc, 2026-08-10) | **driftsort (re-measured)** |
|---|---|---|---|
| random | 10.92 ms / 72.3 M | 8.2–8.9 ms | **8.65 ms** / 48.2 M |
| **few-unique** | 5.93 ms / 48.8 M | 3.6–4.1 ms | **1.81 ms** / **14.2 M** |
| sawtooth | 3.58 ms / 22.5 M | 6.0–6.5 ms | **4.50 ms** / 31.6 M |

Random lands inside the recorded range, which is the control: it is the pattern
where the generator matters least, and it says the two measurements are of the
same thing. Few-unique does not — driftsort is **1.81 ms, not 3.6–4.1**, and the
14.2 M instruction count corroborates it on a metric that does not move between
hosts. The earlier figure appears never to have been cross-checked against
instructions; nothing else in this document depends on it.

`black_box` is why the first attempt at this re-measurement was also wrong, in
the other direction: without it, random measures 6.77 ms rather than 8.65 and
flatters Rust by 28%. It was only the mismatch against this document's own
8.2–8.9 that caught it.

### What that does to B-2026-08-11-10

The row was filed as a 1.3–1.5x gap with a **measured route to parity**. Both
halves change:

- **The gap is bigger than filed: 3.3x wall, 3.4x instructions**, not 1.3–1.5x.
  So the row is worth *more* than it says, not less.
- **The route does not reach parity.** The run-builder takes few-unique to
  34.25 M / 4.15 ms. Against driftsort's 14.2 M / 1.81 ms that is still
  **2.4x on instructions and 2.3x on wall clock** — the two ratios agree, which
  is what you want before believing either. It is a real 1.43x improvement and
  it is not parity, and the row's plan was built on it being parity.

Which makes both options the row proposes worth less than they looked. Option
(a) (pay down the sawtooth regression) and option (b) (gate on a cardinality
signal) are both ways to *keep* a 1.43x that leaves 2.4x on the table — and (a)
in particular spends its effort protecting sawtooth, which is **the pattern
karac already wins** (22.5 M vs driftsort's 31.6 M; 3.58 ms vs 4.50). Regressing
your best pattern 10–18% to bank a fraction of your worst is a worse trade than
the row assumed.

### What the target actually is

14.2 M instructions for 150 k elements is ~95 per element, against karac's ~325.
driftsort is not doing a merge sort with a better constant on this input; at 8
distinct keys it is resolving the array in a couple of passes. Anything aimed at
this gap should be measured against **14.2 M**, and a design that cannot
plausibly reach the low 20s of millions is not worth building — that is the
lesson of the budget check two sections up, applied before the code rather than
after.

Not attempted here, and deliberately: the run-builder was not rebuilt. It was
reverted and is not in git history, so pursuing option (a) means writing ~500
lines of emitter first. Doing that to chase a target the same measurement says
it cannot hit is the mistake this document already made once.


## Where shuffled-uniform stands after Direction 7 (B-2026-08-11-28)

Direction 7 landed (`3d77cc6`) and B-2026-08-11-28 was filed independently
against the shuffled-uniform residual, from the B-2026-08-10-9 lineage rather
than the -19/-20 one. Re-measured on current `main`, instructions, which is the
metric this document trusts:

| pattern | karac before D7 | karac after D7 | driftsort | |
|---|---|---|---|---|
| few-unique | 48.8 M | **13.2 M** | 14.2 M | karac now AHEAD |
| sawtooth | 22.5 M | 23.4 M | 31.6 M | karac ahead |
| **random** | 72.3 M | **72.1 M** | 48.2 M | **1.50x, unchanged** |

Two things follow, and they point the same way.

**Direction 7 did exactly what the arithmetic predicted, on exactly the pattern
it predicted.** Few-unique fell 3.7x and passed the 14.2 M target set when the
driftsort baseline was corrected. Random did not move at all — 72.3 → 72.1 M is
noise. A full-array partition collapses the array when there are 8 distinct
keys; with 150 k distinct keys it still leaves ~log2(n) passes to do, so the
pass-count reduction that carried few-unique has nothing to give here.

**And the direction B-2026-08-11-28 proposes is the one Direction 6 retired.**
That row's stated next step is "compare the emitted merge inner loop against
driftsort's ... any remaining gap is in the merge itself". That is the
cost-per-merge-output family, and § Direction 6 refuted the whole family by
arithmetic rather than by trying its members: `n · log2(n/RUN)` outputs is
fixed, so no per-output constant reaches the target. Seven directions have now
been measured against this residual and the only one with the required property
— reducing full passes — is the one that landed and does not apply to shuffled
input.

### A note on the ratio, because three rows now quote three numbers

B-2026-08-10-20 says 1.30x, B-2026-08-11-28 says 1.64x, and a fresh wall-clock
pass in one sitting produced 1.26x and then 1.71x from the same binaries. None
of these disagree about the code; they disagree about the host. The slope method
(subprocess spawns) and best-of-N in-process timing respond differently to
contention on a shared container, and driftsort's random figure alone moved
8.65 → 5.80 ms between two measurements an hour apart while karac's moved
10.92 → 9.90.

**Quote instructions for this residual, not wall clock.** 72.1 M vs 48.2 M
is reproducible to three significant figures across sessions and hosts; the
wall-clock ratio is not stable to two.

## Caveats

- **Single host, x86_64 shared container.** Absolute numbers drift a few
  percent between sweeps and more between runs of different binaries; ratios
  within a sweep are stable. None of this is from the canonical
  Apple-silicon bench host, and the branch-prediction-sensitive results
  (branchless especially) are the most likely to move on a different
  microarchitecture.
- Rust's `sort_by` is driftsort as of the toolchain in this container; the
  comparison is against that, not against stable-sort-in-general.
- **The driftsort baselines in the early sections are not directly comparable
  to the later ones.** The `7.0 ms` random figure quoted from Result 1
  onwards came from a Rust program whose input generator differed from the
  Kāra one (two LCG steps per element, different key ranges and second
  field). Regenerating it to match karac's input exactly puts driftsort at
  **8.2–8.9 ms** on random, 3.6–4.1 on few-unique and 6.0–6.5 on sawtooth.
  Every karac-vs-karac A/B is unaffected — both sides always ran the same
  input — but the karac-vs-driftsort *ratios* quoted before the cachegrind
  section are pessimistic by roughly the amount that correction implies.
- The karac/rustc parity comparison holds the algorithm *and* the comparator
  shape fixed. It does not claim karac's codegen is at parity in general —
  only that it is not the cause of this gap.

## Direction 6 — the galloping merge: measured, refuted, and it retires the whole merge-side family (B-2026-08-11-10)

Galloping is *the* textbook answer to few-unique stable sorting — timsort's,
and therefore Java's and CPython's. karac's merge does not have it. Anyone
picking this row up would try it, so it was budget-checked first, per this
document's own standing lesson, and the answer is **no** — with a reason that
generalises well past galloping itself.

### The mirror, and why a mirror

[`sortbench/mirror.rs`](sortbench/mirror.rs) is a faithful Rust
reimplementation of `emit_sort_by_mono`'s algorithm — natural-run detection
with `RUN`-padding, then the pairwise ping-pong merge — plus a galloping
variant of the same merge, switchable at the command line.

The point of a mirror rather than a kernel probe is § "Why the budget check
misled": that probe partitioned the full 150k array at every level, so it
measured the kernel's *asymptotic* cost while the real sort spends most of its
work on small ranges. A mirror runs the whole algorithm on the real input, so
every pass sees the run-size distribution the real sort sees.

**It was validated before it was believed.** Against the harness's own
host-independent checksums, re-measured live in the same session
(`ir.sh`: few-unique 48.8 M, sawtooth 22.5 M, random 72.3 M):

| | karac | mirror `plain` | |
|---|---|---|---|
| few-unique | 48.8 M | 50.5 M | +3.5% |
| sawtooth | 22.5 M | 24.0 M | +6.7% |

That is the ~10% band this document already records for a hand-written Rust
mirror of this algorithm. It did **not** start there: the first draft indexed
slices normally and came out at 68.2 M on few-unique, 40% high, purely on
Rust's bounds checks — which karac's emitted merge does not have (raw GEPs off
the data and scratch pointers). A mirror that pays them is measuring a
different loop. The unchecked version is the one below.

### The result

| pattern | instructions plain → gallop | | wall clock plain → gallop | |
|---|---|---|---|---|
| **few-unique** | 50.5 → **40.1 M** | **1.26x** | 5.472 → **5.908 ms** | **0.93x** |
| sawtooth | 24.0 → 20.4 M | 1.18x | 3.877 → 3.563 ms | 1.09x |
| random | 50.8 → 61.4 M | 0.83x | 13.222 → 14.044 ms | 0.94x |
| nearly-sorted | 32.0 → 23.6 M | 1.35x | 4.264 → 4.077 ms | 1.05x |
| sorted / reverse | 2.8 / 3.4 M | 1.00x | — | — |

**On the target pattern it is a wall-clock regression while removing 26% of the
instructions.** That is the same signature as the bounds-check hoist above, and
the same cause: the merge is latency-bound on its loop-carried chain, and a
gallop's binary search has a *worse* chain than the streaming merge it
replaces — each probe address depends on the previous comparison's result.

The mechanism was verified to fire, not assumed to:

```
few_unique    galloped=1619133  elemwise=142975  copies=35145  avg_copy=46
random        galloped=693      elemwise=1895848
```

92% of few-unique's merge outputs are bulk-copied, and the adaptive threshold
correctly switches itself off on random (0.04%). This is a real null result,
not a dud implementation. A `MIN_GALLOP` sweep confirms it is not a tuning
miss either — few-unique is flat at **40.3 / 40.3 / 40.3 / 40.1 / 40.1 / 41.1 M**
for thresholds 1 / 2 / 3 / 4 / 7 / 12, while threshold 1 costs random 27%.

### Why — and why this kills every merge-side direction, not just this one

**Every phase-2 pass writes all `n` elements.** With `RUN = 32` and
`n = 150 000` that is `ceil(log2(150000/32))` = 13 passes ≈ **1.8 M merge
outputs, and that volume does not depend on key cardinality at all.**
Galloping makes an output *cheaper* — a `memcpy` instead of a compare-and-move
— but cannot make one fewer of them.

Worse, it can only make the *top* passes cheaper. With 8 distinct keys a run of
length L holds ~8 equal blocks of L/8, so a gallop pays only once L/8 clears
the threshold. The bottom passes contribute exactly as many outputs as the top
ones and are untouched. The measured average bulk copy is **46 elements**, not
the hundreds the idea imagines.

Now put driftsort's 14.2 M next to that: it is ~95 instructions per *element*.
Spread over 13 passes that would be **7.9 instructions per merge output** —
less than a single compare-and-move costs. Driftsort is therefore not running a
cheaper merge. It is running about **three passes**. The gap is pass COUNT, not
per-pass cost.

And the pass count cannot be bought by raising the insertion base, because
phase 1 is O(n · RUN):

| RUN | passes | plain | gallop |
|---|---|---|---|
| **32** | **13** | **50.5 M** | 40.1 M |
| 64 | 12 | 58.2 M | 46.9 M |
| 128 | 11 | 76.8 M | 65.7 M |
| 256 | 10 | 116.0 M | 106.0 M |
| 1024 | 8 | 362.4 M | 355.7 M |

One pass saved costs more than the pass. `RUN = 32` is already at the optimum.
Note also that the gallop column tracks plain at a roughly constant ~10 M
saving across the whole sweep — exactly what "galloping only reaches the top
few passes" predicts.

### Standing conclusion, updated

Six directions have now been measured. Four of them — merge-kernel tweaks, RUN
tuning, the bounds-check hoist, and galloping — optimise the **cost per merge
output**, and the arithmetic above says that family cannot reach 14.2 M at any
per-output cost, because `n · log2(n/RUN)` outputs is fixed. The other two
(the 3-way and 2-way quicksort run-builders) *were* pass-count reducers, but
both bounded the partition to a `span` and merged above it, so they removed
only `log2(span/RUN)` of the passes — which is why the better of them landed
at 34.25 M rather than near the target.

**The one property a future attempt must have is a reduction in the number of
full passes over the array, and bounding the partition to a span forfeits most
of it.** That is a much narrower target than "try something faster", and it is
what this measurement buys.

Not attempted here, deliberately: a full-array stable partition. It is the only
shape left that satisfies the constraint, but it is a ~500-line emitter whose
bounded cousin has already been built and reverted once, and this row is
severity `low` — karac is still 7x faster than driftsort on sorted and
reverse-sorted input and 1.85x on sawtooth. The measurement is the deliverable;
the build is a separate decision with better information than it had before.

## Direction 7 — the full-array stable partition: measured, and it works

*2026-08-11. Budget-checked in the mirror before any emitter, same as
Direction 6. Harness: `sortbench/mirror.rs`, mode `part`.*

This is the shape § Direction 6 left standing, and the first of seven
directions that improves the target pattern **without charging any other
pattern for it**.

    pattern         instructions              wall clock
                    merge  -> partition       merge  -> partition
    few-unique      49.7M  -> 19.8M   2.5x    6.377  -> 3.640 ms   1.75x
    sawtooth        23.8M  -> 23.8M   1.00x   4.015  -> 4.398 ms
    random          50.0M  -> 50.0M   1.00x   13.804 -> 12.273 ms
    nearly-sorted   31.5M  -> 31.6M   1.00x   5.048  -> 5.203 ms
    sorted           3.1M  ->  3.1M   1.00x   2.157  -> 2.218 ms
    reverse          3.7M  ->  3.7M   1.00x   2.225  -> 1.974 ms

Instructions on the five non-target patterns are unchanged **to three
significant figures**, because on those inputs the algorithm *is* today's
algorithm — see the gates below. Read the wall-clock column on those rows as
noise and nothing else: it swings ±11% (random 13.804 → 12.273) against an
identical instruction count, which is this shared 4-vCPU container's floor. The
few-unique row is the one that matters, and there instructions and wall clock
**agree in direction** — precisely what galloping failed to do.

Across eight pivot seeds few-unique lands at 18.4 / 18.9 / 18.9 / 19.6 / 19.8 /
19.9 / 21.1 / 22.4 M, so the honest figure is 2.2x–2.7x and the table above
quotes the median. Against driftsort's 14.2 M the residual gap falls from
**3.4x to ~1.4x**.

### Why this one reduces passes when the bounded run-builder could not

Same kernel as the reverted B-2026-08-10-20 attempt — a stable 2-way
count-then-scatter partition into alternating buffers, borrowing phase 2's own
scratch, so still no extra allocation. The inversion is *which end of the
recursion it occupies*:

    bounded (reverted)   merge ABOVE a span, partition BELOW it
                         -> removes log2(span/RUN) passes off the bottom,
                            and pays a partition's fixed per-range cost on
                            ranges near its base size
    full-array (this)    partition from n DOWN to a span, merge below
                         -> removes the top passes, and every partition it
                            performs is on a large range

And it can **stop early**, which no merge can: when every element of a range
compares equal to the pivot, that range is sorted *and* stable and is finished.
That is the entire mechanism, and the instrumentation shows it firing exactly as
the theory predicts — on few-unique, `partitions=12, all_equal_exits=8,
merged=0`. Eight all-equal exits for eight distinct keys, and **not one merge
pass over the array**.

Two details the kernel needs, neither optional:

- **The pivot must be randomised, not a fixed-position median-of-3.** Sampling
  `lo`, `lo+len/2`, `hi-1` is degenerate on periodic input: on the sawtooth
  (`i % 1000` repeated 150 times) those positions hold 0, 0 and 999, the median
  is 0, and 0 is the range *minimum*, so each level peels off only the 150
  copies of the minimum. Measured **2328.7 M instructions**, ~100x the merge.
  Median-of-3-random samples fixes it. (The reverted emitter already specified
  "pivot xorshift plus a 64-bit urem" — this is why.)
- **One counting pass tallies both `< pivot` and `<= pivot`.** That lets the
  split predicate be chosen without a second pass, and folds the old `t=1`
  retry into the same pass: `nlt == 0` means the left block is exactly the
  elements equal to the pivot, already sorted and stable, needing no recursion.

### The two gates, each placed where it costs nothing

An ungated partition is unshippable — it is 11–13x *worse* on sorted and
reverse (3.1 M → 40.2 M, 3.7 M → 39.7 M) and 1.3–2.7x worse on random,
sawtooth and nearly-sorted. Getting the gate free was most of the work, and two
plausible placements both lose:

    probe before phase 1   a full counting pass is ~1.3 M instructions, which is
                           42% of sorted's entire 3.1 M. sorted -> 4.4 M,
                           reverse -> 5.0 M. These are the patterns karac beats
                           driftsort 7x on.
    probe after phase 1    free for sorted/reverse, but few-unique goes 19.4 M
                           -> 33.9 M: phase 1's RUN=32 insertion padding costs
                           ~14.5 M on an input whose natural runs are ~2 long,
                           and the partition then discards that work. 33.9 M is
                           no better than the 34.25 M run-builder already
                           rejected.

So the entry decision must precede phase 1 **and** be O(1) in n. 256 random
samples settle both halves of it:

- **distinct keys among the sample** estimates cardinality — 8 true keys reads
  8, 1000 reads 231, all-distinct reads 256;
- **fraction of sampled adjacent pairs already in order** estimates what phase 1
  would find — sorted 100%, nearly-sorted 99%, shuffled ~57–61%.

Both are needed. Cardinality alone would partition an input that is *already
sorted over few keys*, which phase 1 resolves in a single run.

Inside the recursion the gate is exact and genuinely free: `part_once`'s
counting pass has already computed `neq = nle - nlt`, the number of elements
tying with a random pivot, so `len/neq` is an unbiased cardinality estimate
**available before a single element is written**. A range that fails it
abandons having moved no data, and the caller merges it exactly as today. So
the decision is per-range, not a global mode: a mixed input partitions the part
that pays and merges the part that does not.

### Where the gate threshold actually belongs — measured, not assumed

Sweeping cardinality with the partition forced on (`GATE=100000`), so the
crossover is a property of the algorithms rather than of the gate:

    distinct keys      2     8    32    64   128   256   512
    merge          38.7M 49.7M 53.0M 52.0M 51.0M 50.5M 50.5M
    partition       7.6M 22.4M 30.7M 36.8M 60.6M 64.8M 67.0M
    ratio           5.1x  2.2x  1.7x  1.4x  0.84x 0.78x 0.75x

The crossover is between 64 and 128 distinct keys. The analytic prediction is
that partitioning wins while `2·log2(d) < log2(n/RUN) = 12.2` — two passes per
level against the merge's one — i.e. `d < 68`. Measurement and arithmetic
agree, which is the corroboration worth having before trusting either.

`GATE = 64` therefore sits just on the winning side of a crossover it was not
tuned to. The estimator undercounts at high cardinality (128 true keys read as
108, 256 as 157) by ordinary coupon-collector shrinkage, which pushes the
effective trigger down to a true `d ≲ 66` — conservative in the right
direction.

### What this leaves

The budget check this row demanded is now positive: the design reaches
18.4–22.4 M against a stated bar of "the low 20s of millions", it holds every
other pattern at parity, and both of its gates are free. That is the
information the emitter decision was waiting on.

### Built — and the gap is closed

*2026-08-11. `emit_sort_by_mono` in `src/codegen/vec_method.rs`.*

The mirror said go, so it was built. Measured with the harness in
`sortbench/`, before and after on the same container, baseline reproducing this
document's own recorded checksum (48.8 M / 22.5 M) exactly:

    pattern         instructions           driftsort   was        now
    few-unique      48.8M -> 13.2M  3.70x     14.2M    3.44x behind   1.08x AHEAD
    sawtooth        22.5M -> 23.4M  0.96x     31.6M    1.40x ahead    1.35x ahead
    random          72.3M -> 72.1M  1.00x     48.2M    1.50x behind   unchanged
    nearly-sorted   31.5M -> 32.9M  0.96x
    sorted           1.0M ->  1.0M  1.00x
    reverse          1.8M ->  1.8M  1.00x

Wall clock on the target, same session, same slope method: **4.760 ms →
1.742 ms (2.73x)**, against driftsort's 1.514 ms best / 1.764 ms bulk average.
So on the pattern this row filed as 3.3x behind, karac now runs **fewer
instructions than driftsort** and lands at wall-clock parity.

Read wall clock on the other five rows as noise. They swing several percent in
both directions against *identical* instruction counts, and `sorted` /
`reverse` sit at 0.03–0.4 ms, which is under the slope method's floor. The
instruction column is the deterministic one, and it says those patterns did not
change.

**The 4% on sawtooth and nearly-sorted is real, and it is not what it looks
like.** It is not the probe: `sorted` and `reverse` call the probe on every
sort and did not move off 1.0 M and 1.8 M, which bounds it under 0.05 M. It is
not the partition running: neither pattern ever passes the gate. It is not the
call-graph change either — the obvious suspect was that `qpart` calling back
into the merge gives it a second caller, but deleting that call (a deliberately
incorrect build, valid to measure because these two patterns never reach
`qpart`) made both *worse*, 24.4 M and 34.4 M. What is left is codegen jitter:
perturbing the module around a 60-block function moves these two patterns a few
percent in either direction, and the shipped arrangement is the better end of
the range that was measured.

#### What the emitter does

`__vec_<m>_qpart_<id>(data, scratch, lo, hi, in_a, depth)`, mutually recursive
with the merge sort, borrowing phase 2's scratch so it allocates nothing:

- **Two gates, each where it is free.** `__vec_<m>_sprobe_<id>` samples 512
  elements once at entry and answers both halves of the question — ties with a
  random pivot estimate cardinality, ordered adjacent pairs estimate what phase
  1 would find. Inside the recursion, `neq = nle - nlt` is already computed by
  the partition's own counting pass, so every range re-decides for nothing and
  a mixed input partitions the part that pays.
- **One counting pass tallies both `< pivot` and `<= pivot`**, which chooses
  the split predicate without a second pass and folds the old `t = 1` retry
  into it.
- **Count-then-scatter** with two cursors over one in-order walk — the whole
  stability argument.
- **`allow_part` on the merge sort's signature.** Without it an abandoned range
  is re-probed, accepted by the sampling estimate that the exact tie count just
  rejected, and handed straight back: same range, same pivot, forever.
- **Depth limit 64**, which is what keeps the worst case O(n log n) after
  introducing a randomised pivot.

Verification: a 756-case sweep (6 patterns × 14 sizes × 9 cardinalities,
straddling the probe floor and the gate) asserting sorted *and* stable, with
the oracle itself validated by poisoning it; `i64`, all-int-struct and heap
`String` element types; the full `--features llvm` suite; ASAN at the default
level and the `-O0` leg. Note that `should_use_mono_vec_sort_by_for` admits
only `i64` and all-int structs, so no heap-owning element ever reaches this
path — the memory-safety class here is bounds, not ownership.
