# Spike: codegen auto-vectorization (kata #59 gap) — RESOLVED: not vectorization

**Status:** ✅ **RESOLVED 2026-07-04 — auto-vectorization RULED OUT.** The kata-#59 1.46× Rust gap
is **not** a vectorization gap. Rigorous S0 refuted it: the loop that "didn't vectorize" is a
weighted integer reduction (`s += a[i]·(i+1)`) that **no compiler vectorizes** — clang -O2, clang
-O3, and rustc -O all emit 0 vector ops for it, same as karac. The original "karac 0 SIMD vs Rust
2589 SIMD" reading was a **whole-binary confound** (Rust statically links a SIMD-heavy std; the
2589 was mostly memcpy/format/panic machinery, not the kata loop). The real cause is Kāra's
**default safety checks** — overflow-checked arithmetic + bounds-checked indexing — the exact
tradeoff already resolved in [overflow-check-elision.md](overflow-check-elision.md): the lever is
the `wrapping_*` / scoped opt-out, **not** a bigger prover or a vectorizer change. No codegen
change is warranted from this spike.

**Question this spike gated:** kata [#59 Spiral Matrix II](../../../kara-katas/leetcode/1-100/59-spiral-matrix-ii/)
runs 1.46× behind Rust on a generate-and-checksum workload. First guess (in the kata README) was
nested `Vec[Vec]` indexing; the first cut of *this* spike guessed auto-vectorization. Both were
wrong. What is the gap actually?

## Method

Decomposed the workload by rewriting it to remove one suspected cost at a time (nested→flat,
per-iter-alloc→reused-buffer, checked→wrapping, checked-index→`get_unchecked`), timing each with
`hyperfine` on M5 Pro and disassembling hot loops with `otool -tvV`. Every variant prints the
identical sink, so they are the same computation. Cross-checked the vectorization question against
`clang -O2/-O3` and `rustc -O` on the *identical* kernel.

## Findings

**1. The ratio is invariant to representation and allocation (~1.5× throughout).** Nested→flat is
~2.2× on *both* sides; removing per-iter allocation drops both similarly. Neither moves the
Kāra:Rust ratio. → not nesting, not allocation.

**2. Vectorization is not the cause — nobody vectorizes the kata's loop.** The checksum is
`s += g[i]·(i+1)`, a weighted integer reduction. Vector-op count of *that specific function*:

| kernel | karac (`default<O2>`) | clang -O2 | clang -O3 | rustc -O |
|---|---|---|---|---|
| `s += a[i]` (add-reduction) | **14** | vec | vec | vec |
| `s += a[i]·2` (constant-scaled) | **18** | vec | vec | vec |
| `s += a[i]·(i+1)` (induction-weighted) | 0 | 0 | 0 | 0 |
| `s += a[i]·b[i]` (dot product) | 0 | 0 | 0 | 0 |

karac vectorizes exactly what LLVM vectorizes (simple and constant-scaled reductions) and, like
every LLVM front-end, does **not** vectorize the induction-weighted / dual-varying multiply-reduce
at -O2/-O3. So karac's vectorizer works fine; the kata loop simply isn't a vectorizable shape for
anyone. The earlier "0 vs 2589" was a whole-binary count dominated by Rust std.

**3. The gap is Kāra's default safety checks.** Isolating the pure checksum reduction with clean
(wrapping, unchecked) arithmetic put karac at **1.13×** of Rust — the reduction codegen is near
parity. Running the *full* reused-buffer workload with everything wrapping + unchecked reads:

| variant (flat, reused buffer, K=180k) | wall | vs Rust |
|---|---|---|
| kāra, **checked** (default) | 39.0 ms | 1.48× |
| kāra, **wrapping + unchecked reads** | **27.4 ms** | **1.04× — parity** |
| rust (release = wrapping, bounds mostly elided) | 26.3 ms | — |

Removing the overflow checks (wrapping arithmetic) and the read bounds checks lands karac on Rust.
Split roughly evenly between the fill loop's checks and the checksum's. That is the whole gap.

## Decision

**No change from this spike.** The finding reduces to the already-decided
[overflow-check-elision.md](overflow-check-elision.md) conclusion: karac is *safe by default*
(traps on overflow, bounds-checks) and therefore trails *release*-Rust (unchecked arithmetic) by
the cost of the checks — while matching *checked*-Rust. The lever, if the ~1.3–1.5× on
check-dense integer kernels matters, is the existing `wrapping_{add,sub,mul}` opt-out (and a
possible scoped `#[wrapping]` / `unchecked_index`), **not** a prover and **not** a vectorizer
change — building an auto-vectorization pass would do nothing here, because the hot loop is
unvectorizable for LLVM regardless of language.

The kata #59 README has been corrected to state the safety-check cause (it carried the
nested-indexing guess, then the vectorization guess — both now refuted in-repo).

## Lesson

Two wrong hypotheses (nested indexing → vectorization) died before the real, mundane cause
(default safety checks). Two process notes for the next perf investigation:

- **Never count vector ops over the whole binary** — statically-linked std/runtime dominates. Count
  them in the *specific function* (`otool … | awk '/_fn:/{…}'`), and confirm against clang/rustc on
  the *same* kernel before blaming the front-end.
- **Decompose by removing one variable at a time** (representation, allocation, checks) and watch
  the *ratio*, not the absolute time. The ratio staying flat across nested/flat/reused is what
  killed the nesting and allocation stories; wrapping+unchecked reaching parity is what identified
  the real one.

## Cross-references

- Root cause + prior decision: [overflow-check-elision.md](overflow-check-elision.md).
- Motivating measurement: [kata #59 § Benchmarks](../../../kara-katas/leetcode/1-100/59-spiral-matrix-ii/README.md).
- `noalias`/independence levers (also gate vectorization where a loop *is* vectorizable): [independence-noalias-ilp.md](independence-noalias-ilp.md).
