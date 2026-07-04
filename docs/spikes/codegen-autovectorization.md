# Spike: codegen auto-vectorization (scalar-only loops vs rustc/clang SIMD)

**Status:** ⬜ **PROFILED 2026-07-04 — confirmed finding, not yet root-caused in karac source, no
fix attempted.** On a tight numeric-loop workload karac emits **fully scalar** machine code
(0 SIMD instructions) where `rustc -O` / `clang -O3` auto-vectorize the identical loops
(thousands of NEON ops). Measured ~1.5× runtime gap that is stable across representation and
allocation changes — a broad codegen lever if karac can be made to vectorize, not a
kata-specific quirk. Next step is to inspect karac's LLVM pass pipeline / opt level (does it
run the loop + SLP vectorizers at all?); the fix could be as small as a pass-manager/opt-level
setting or as involved as reshaping the emitted IR. **Do not assume the mechanism — confirm it
in `src/codegen.rs`'s optimization setup first.**

**Question this spike gates:** Kata [#59 Spiral Matrix II](../../../kara-katas/leetcode/1-100/59-spiral-matrix-ii/)
runs 1.46× behind Rust on a nested-`Vec[Vec[i64]]` generate-and-checksum workload — heavier
than the corpus norm (most compute katas sit at 1.0–1.1× of Rust). Is the gap something
specific to that kata (nested indexing, allocation), or a general karac codegen limitation?
And if general, is closing it a small pipeline fix or a deep IR-reshaping project?

## Method

Decomposed the kata-59 workload (K=180k iters, each generates an n×n spiral matrix for
rotating n=12..20 and folds a position-weighted checksum over every cell) by rewriting it to
remove one suspected cost at a time, timing each on M5 Pro with `hyperfine --warmup 5`, and
disassembling the hot loops with `otool -tvV`. All variants print the identical sink
(1,100,752,800,000), so they are the same computation.

## Findings

**The ~1.5× ratio is invariant to representation and allocation:**

| variant | Kāra | Rust | ratio |
|---|---|---|---|
| nested `Vec[Vec[i64]]` (`n+1` allocs/iter) | 114.0 ms | 78.2 ms | 1.46× |
| flat `Vec[i64]`, index `i·n+j` (1 alloc/iter) | 51.3 ms | 33.7 ms | 1.52× |
| flat, reused buffer (0 allocs in the loop) | 39.0 ms | 26.3 ms | 1.48× |

Going nested → flat is ~2.2× on **both** sides (nesting is expensive for everyone — double
indirection + `n` extra allocations), and removing per-iter allocation drops both by a similar
absolute amount. Neither moves the **Kāra:Rust ratio**, which stays ~1.5×.

**The cause is auto-vectorization.** Disassembling the flat reused-buffer binaries:

- `otool -tvV rust_binary | grep -cE '\bq[0-9]+\b|\.2d|\.16b'` → **2589** vector instructions.
- Same over the karac binary → **0**.

Rust emits NEON SIMD for the checksum reduction (`ldr q0, …` + vector multiply-add) and the
fill; karac emits a purely scalar loop. The karac loop body also carries, per element, a
bounds-check (`cmp x, #0x190` [=400, the buffer len] + `b.hs <trap>`) and an
overflow-checked index multiply (`mul` + `cmp x, x, asr #63`) — both are per-element
conditional traps, exactly the shape that blocks LLVM's vectorizer.

**But removing those checks individually did NOT enable vectorization:**

- Running linear index (no `i·n` multiply, so no per-element overflow check): **0** vector ops,
  40.7 ms — unchanged.
- `unsafe { g.get_unchecked(idx) }` in the checksum (no bounds check on the read): **0** vector
  ops, 41.2 ms — unchanged.

So while the checks are *a* reason a naive vectorizer would bail, removing them one at a time
doesn't flip karac to vectorized output. That points less at "one check blocks it" and more at
**karac's LLVM pipeline not running (or not succeeding at) auto-vectorization on these loops at
all** — the whole binary has zero vector instructions across four different variants. The fill
loop additionally has a checked `v = v + 1` loop-carried increment (a trap-carrying recurrence)
that would block the fill even if reads were clean, but that does not explain the checksum
reduction staying scalar under `get_unchecked`.

## What is NOT yet known (do this first)

The mechanism is unconfirmed. Before any fix, inspect karac's codegen optimization setup in
`src/codegen.rs` (the LLVM `PassManager` / `PassBuilderOptions` / target-machine opt level):

1. **Does karac run the loop-vectorizer + SLP-vectorizer passes at all?** If AOT codegen builds
   at a low opt level or a custom pass list that omits them, that alone explains 0 SIMD, and the
   fix is a pipeline change (potentially a few lines) with corpus-wide payoff.
2. **If the passes run, what makes them bail?** Candidates in priority order: the per-element
   bounds-check traps (needs bounds-check elision to feed the vectorizer clean IR — related to
   [overflow-check-elision.md](overflow-check-elision.md), which found LLVM already elides the
   *provable* checks but did not look at the vectorization angle); the checked-arithmetic
   loop-carried recurrence in fill loops; or reduction IR that isn't in a vectorizer-recognized
   form. `-mllvm -pass-remarks-analysis=loop-vectorize` (or the inkwell equivalent) on a minimal
   kernel would report exactly why it bails.
3. **Target features.** Confirm the target machine is told the host supports NEON/the right
   feature set — a missing `+neon`/CPU string can silently disable vectorization even with the
   passes enabled.

## Decision

None yet — this spike records the finding and the investigation plan, not a verdict. The
finding is strong (0 vs 2589 SIMD, ratio invariant across 5 variants) and the payoff is broad
(any vectorizable numeric kernel in the corpus), so it is worth the pipeline inspection in step
1 before deciding scope. If step 1 shows the vectorizer passes simply aren't in the AOT
pipeline, this becomes a high-ROI slice; if they run and bail on the checked-IR shape, it
converges with the overflow/bounds-check-elision work and is a larger project. Explicitly **not**
assumed: that this is "just bounds checks" (removing them didn't help) or "just nested
indexing" (the kata-59 README's original guess, refuted here).

## Proposed slices (if greenlit after step-1 inspection)

1. **S0 — pipeline audit.** Read `src/codegen.rs`'s opt setup; run a *minimal* known-vectorizable
   kernel (`for i in 0..n { s += a[i] }` over a flat `Vec[i64]`) and check the emitted asm for
   vector ops. Confirms whether the passes run. Cheap, decides everything downstream.
2. **S1 (if passes absent) — enable loop + SLP vectorizers** in the AOT pass pipeline; A/B the
   corpus's numeric katas (#54, #59, the stats/tensor kernels) for speedup + zero output/ASAN
   regressions.
3. **S1′ (if passes present but bail) — feed clean IR:** scope bounds-check elision *specifically
   as a vectorization enabler* (narrower than a general prover — only needs the in-bounds proof
   for counted loops over a known-length `Vec`), and re-measure.

## Cross-references

- Motivating measurement + full decomposition table: [kata #59 § Benchmarks](../../../kara-katas/leetcode/1-100/59-spiral-matrix-ii/README.md).
- Related check-elision work (LLVM already elides *provable* overflow checks, but the
  vectorization-blocking angle was not examined): [overflow-check-elision.md](overflow-check-elision.md).
- Independence/`noalias` levers that also gate vectorization: [independence-noalias-ilp.md](independence-noalias-ilp.md).
