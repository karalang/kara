# 67 — SIMD strategy for the data spine and stdlib hot paths

**Status:** Graduated to canonical 2026-05-13. Archived.

**Where the content lives now:**
- `design.md § Portable SIMD — Vector[T, N]` — WASM SIMD-128 lowering, trait surface table, Tensor↔Vector interop API, split-borrow note (§5.1, §5.3, §4.1).
- `design.md § Portable SIMD — Vector[T, N] > Multiversioning` — `cpu-baseline` knob + per-arch baselines + `#[target_feature]` / `#[multiversion]` attribute syntax (§4.2, §4.2.1).
- `design.md § Standard Library Layers > Internal SIMD usage policy` — stdlib-internals SIMD policy (§4.3).
- `design.md § Feature 5: Auto-Concurrency > Composition with SIMD` — auto-par × SIMD composition note (§5.2).
- `design.md § AOT — karac build` — `cpu-baseline` replaces `--target-cpu=native` as the default story.
- `deferred.md § Hand-Vectorized Data-Spine Commitment` — kernel list, BLAS classification, speedup targets, bit-exactness scope, `std.simd.math` sub-surface (§3, §3.1, §5.4).
- `deferred.md § std.embeddings` — updated to six functions, `cosine_similarity_matrix` added (§3.1.1).
- `deferred.md § Tensor Element-Wise Math and Clamp` — perf-contract paragraph added, cross-references the spine commitment.
- `phase-10-targets.md` — new bullet: WASM SIMD-128 lowering tracker entry.
- `phase-8-stdlib-floor.md` — new bullets: hand-vectorized data-spine kernels, `std.simd.math`, `cpu-baseline` knob + `#[multiversion]` attribute; existing `std.embeddings` bullet updated to six functions.

- **§1 (audit)** and **§2 (problem statement)** are framing — nothing to decide.
- **§3 (data-spine kernel scope)** — **resolved:** option C (hand-write the 8 kernel families) as a *ceiling*, narrowed kernel-by-kernel after §6.4 auto-vec measurement.
- **§3.1.1 (BLAS-3 cosine path)** — **resolved:** option γ (add `cosine_similarity_matrix(queries, corpus) -> Tensor[f32, [Q, N]]` as a sixth `std.embeddings` function). v66 `std.embeddings` entry needs a +1 function update at graduation time.
- **§4.1 (Tensor↔Vector API)** — **resolved:** approve as proposed (`chunks_simd` + `chunks_simd_mut` + `load_simd` + `store_simd`; `Vector` + `Slice` tail idiom; non-contiguous panics; last-axis row-major chunking).
- **§4.2 (multiversioning)** — **resolved:** P+R hybrid (`cpu-baseline = "v3"` default + opt-in `#[multiversion]` for spine kernels). Extended with per-architecture baseline mapping (see §4.2.1 added below) covering aarch64 alongside x86_64.
- **§4.3 (stdlib-internals SIMD policy)** — **resolved:** one-paragraph addition to `design.md § Standard Library` approved verbatim.
- **§5 (settled documentation graduations)** — pre-approved on read-through; bundle into the graduation pass.
- **§6.4 (measure-first sequencing detail)** — folded into §3 resolution; no separate decision needed.

**Graduation policy.** All resolved sections graduate to canonical docs (`design.md`, `deferred.md`, the v66 `std.embeddings` entry, `phase-10-targets.md`, `phase-11-stdlib-longtail.md`) **at once** when triggered — not piecemeal. v67 stays in `brainstorming/` until the graduation pass; on graduation it moves to `brainstorming/archive/v67_simd_strategy.md` and the two `deferred.md` entries that cross-reference it (MLIR + Heterogeneous Compute) get their cross-references updated to the archive path. Graduation is now unblocked; pending only the explicit "graduate now" trigger from the user.

**Trigger:** Conversation 2026-05-13 — comparison of Kāra against Mojo led to the question of where SIMD fits in Kāra's plan. Audit of `design.md`, `deferred.md`, `roadmap.md`, `phase-7-codegen.md`, `phase-8-stdlib-floor.md`, `phase-10-targets.md`, `phase-11-stdlib-longtail.md`, and `book/ch15-data-layout.md` showed that the **language-surface SIMD story is already well-designed** (`Vector[T, N]` portable type, auto-fallback, GPU mapping, repr(simd) layout, `#[require_simd]`). The gaps are not in the language surface — they are in **how the v1 ML/data spine and backend-first stdlib hot paths actually achieve SIMD performance.**

This brainstorm decides:
- Whether to commit a designated set of stdlib kernels to **hand-written `Vector[T, N]` implementations** rather than relying on LLVM auto-vectorization (§3).
- The **`Tensor[T, S]` ↔ `Vector[T, N]` interop API** shape (§4.1).
- The **CPU-feature multiversioning policy** — single binary with `cpu-baseline` knob, function multiversioning, or runtime detection (§4.2).
- Whether stdlib internals may use `Vector[T, N]` and feature-gated paths for hot operations like JSON parsing, UTF-8 validation, and regex prefilter (§4.3).
- Documentation gaps that are uncontroversial and can graduate directly (§5).

Per stored tier definitions: **v1 = P0 + P1**. P0 = load-bearing architectural commit; P1 = ships at v1, sequenced after the P0 spine. P2 = post-v1 but will ship. P3 = library/framework, may or may not.

Framing claim: the v1 ML/data narrative (v66 graduation — `std.embeddings`, `std.autograd`, lazy DataFrame, Tensor element-wise math, statistical methods) **commits Kāra to numerical performance at launch**. Every one of those items currently relies on LLVM auto-vectorization with no documented fallback if LLVM fails to vectorize. That is a `v1 ships reality not promises` risk — the kind that a curious adopter's first benchmark exposes.

---

## Problem 1 — What's already settled (audit summary, not decisions)

For completeness, the SIMD surface that already exists in the canonical design:

- **`Vector[T, N]` portable SIMD type** — `design.md § Portable SIMD — Vector[T, N]` (~line 12400). Phase 7+, P1. Three-tier auto-fallback (native → wider-lane masked → scalar). `repr(simd)` layout, FFI-stable for power-of-two N. Same type unifies CPU SIMD and GPU `vec<N, T>`.
- **Auto-vectorization as Layer 3** — `design.md:194` lists vectorization as compiler-choice / implementation freedom.
- **SoA-friendly layout primitives** — `#[layout(soa)]`, `Column[T]` Arrow bitmap (explicitly noted SIMD-friendly at `design.md:2057`), `Tensor` dense-only (`design.md:2080`).
- **`f16`/`bf16` reduced-precision primitives** — Phase 7+, native on AVX-512FP16, ARM FP16, TPU, Apple ANE.
- **Reserved `engine` profile** — `design.md:9966` reserves the name for "SIMD auto-optimization" (placeholder, not implemented).
- **`#[require_simd]` attribute and `--simd-report=verbose`** — hard-error and diagnostic affordances on top of the fallback rule.
- **Trait surface anticipates SIMD** — `roadmap.md:434` notes that Add/Mul restriction-lift includes "associated Output + heterogeneous Rhs … for mixed-type arithmetic (SIMD, decimal, duration)."

**Nothing in §1 needs revisiting.** The language surface for explicit SIMD is locked.

---

## Problem 2 — Where the gaps are (the actual problem)

The v66 graduation put a substantial numerical stdlib on the v1 plate. Each of these is committed at v1 P1 today, with no documented fallback if LLVM auto-vectorization underperforms:

- `std.embeddings.cosine_similarity_batched` over `Tensor[f32, [N, D]]` — canonical SIMD workload (FMA-heavy inner loop). Rust's `simsimd` gets 4–10× over scalar on this.
- `std.embeddings.dot_batched`, `top_k`, `l2_normalize` — same shape.
- `std.autograd` operator overloads (`+`, `*`, matmul, reductions) on `Var[T, S]` + activation/loss backwards — backward kernels are matmul + element-wise, the dominant ML inner loop.
- Statistical methods on `Column[T: Numeric]` — `mean`, `std`, `var`, `median`, `sum`, `corr`. Sum/mean/var are textbook reduction patterns.
- Lazy DataFrame Option A — filter, projection, aggregation kernels. DuckDB/Polars beat pandas largely on SIMD-ized analytical inner loops.
- Tensor element-wise math (`exp`, `log`, `sqrt`, `relu`, `softmax`, `clip`, `where`) — `deferred.md:1348` explicitly says these "require LLVM auto-vectorization (Phase 7) to perform well."

LLVM auto-vectorization is fragile under well-known conditions. The `parallax_perf.md` H4 hypothesis (indirect calls to `karac_par_run` likely blocking auto-vec on the par-fan-out body) sketches one such path in a Kāra-specific context — currently *not probed*, deprioritized as <2% out-of-band cost at the H1-dominated bench shape, but structurally consistent with the standard failure conditions: bounds-check elision, alias analysis, no early-exit, no function calls in the loop, recognized reduction shape — any one of these failing kills the vectorizer silently.

The reader-attention cost of "trust LLVM" is hidden until launch-day benchmarks expose it. By then, the narrative is set.

---

## Problem 3 — Hand-vectorized kernels for the v1 data spine (resolved 2026-05-13)

**Resolved 2026-05-13:** Option **C** — hand-write the 8 kernel families enumerated in §3.1 as a *ceiling commitment*, with kernel-by-kernel narrowing driven by the §6.4 measurement once Phase 7's LLVM backend can compile a representative kernel. The `deferred.md` entry per §5.4 records C as the committed list; rows that LLVM auto-vec already handles within ~80% of hand-written SIMD drop into a "trust auto-vec, bench number documented" footnote at implementation time.

Rationale: the v66 graduation puts `std.embeddings`, `std.autograd`, Tensor element-wise math, and statistical methods on the v1 plate. Shipping those rows as "hopefully LLVM vectorized it" directly violates `feedback_v1_ship_reality_not_promises`. Option A is the only choice that fails that test; B leaves autograd and statistical methods exposed; D is months of work for diminishing returns past the spine.

**Original decision framing (kept for historical record):** Designate a small, named set of stdlib kernels to be hand-written against `Vector[T, N]` rather than left to auto-vectorization. Yes/no, and if yes — which.

### 3.1 Proposed spine

The minimum kernel set that converts the v66 data narrative from "hopefully fast" to "measurably fast":

| Kernel | Surface | BLAS class | Bound by | Expected speedup vs scalar | Why it matters |
|---|---|---|---|---|---|
| `embeddings.cosine_similarity` (single) | `std.embeddings` | BLAS-1 | Memory | 2–4× | The flagship "AI-adjacent" demo; first benchmark a RAG-curious adopter runs. |
| `embeddings.cosine_similarity_batched` (query × N corpus) | `std.embeddings` | BLAS-2 | Memory | 2–4× | RAG single-query path; SGEMV-shaped. See §3.1.1 on the Q×N BLAS-3 path. |
| `embeddings.dot_batched` (matrix × matrix) | `std.embeddings` | **BLAS-3** | **Compute** | **5–10×** | The decisive BLAS-3 RAG / re-ranking surface. Q×N cosine reduces to this via normalize → dot. |
| `embeddings.l2_normalize` (in-place + non-mutating) | `std.embeddings` | BLAS-1 | Memory | 2–4× | Hot prelude to almost every embeddings op. |
| `embeddings.top_k` | `std.embeddings` | BLAS-1 + reduction | Memory | 2–3× | Common end-of-pipeline; vectorizable via SIMD select / mask. |
| Tensor element-wise `+`, `-`, `*`, `/` | `Tensor` ops | BLAS-1 | Memory | 2–4× | Universal in autograd and Column math. |
| Tensor reductions: `sum`, `mean`, `min`, `max` | `Tensor` / `Column` / `Var` | BLAS-1 + reduction | Memory | 2–4× | Statistical methods, autograd reductions, loss aggregation. |
| Activations: `relu`, `sigmoid`, `tanh` | `std.autograd` | BLAS-1 | Memory (relu) / **Mixed** (sigmoid, tanh) | 2–3× / **4–8×** | Forward path of every neural-network workload. |
| `softmax` | `std.autograd` | BLAS-1 + reduction + transcendental | **Mixed** | **4–6×** | Final classification layer; transcendental-heavy. |
| `exp`, `log`, `sqrt` element-wise | `std.math` / `Tensor` | BLAS-1 (transcendental) | **Compute (per element)** | **4–8×** | Underpins softmax, GeLU, cross-entropy, normalization. |

Roughly 8 distinct kernel families, each with a small number of type instantiations (`f32`, `f64` minimum; `f16`/`bf16` once those land). Realistic engineering: 1-2 weeks for a competent SIMD author, building on the existing `Vector[T, N]` codegen and a chunked-iteration helper.

**Reading the speedup column.** Memory-bound kernels (most of the spine) cap at 2–4× because RAM bandwidth is the ceiling — scalar code doesn't saturate it, hand-vec gets closer but can't exceed it. Compute-bound kernels (BLAS-3 batched ops, transcendentals) hit 5–10× because the ALU is the ceiling and SIMD widens it 8–16×. The big wins on a benchmark slide come from the bold rows; the modest rows are still load-bearing because they're called constantly and scalar bandwidth-starvation is real.

### 3.1.1 — Note on the BLAS-3 cosine path (resolved 2026-05-13)

**Resolved 2026-05-13:** Option **γ** — add `cosine_similarity_matrix(queries, corpus) -> Tensor[f32, [Q, N]]` as a sixth `std.embeddings` function. γ over α because doc-only composition buries the BLAS-3 win behind a recipe nobody reads; γ over β because rank-dispatch overload is implicit-behavior that conflicts with Kāra's general explicit-over-magic preference (the resolver restriction on user `impl Add` and the explicit ownership-tier syntax both reflect this). v66's "five functions over `Tensor[f32, ...]`" framing becomes "six functions" at graduation time — trivial size bump.

The v66 `cosine_similarity_batched(query, corpus)` API is `[D] × [N, D] → [N]` — SGEMV-shaped, BLAS-2. The 5–10× BLAS-3 win lives on the `[Q, D] × [N, D] → [Q, N]` shape (Q queries against N corpus), which the current API does *not* expose directly. Three options were considered:

- **(α) Compose via `dot_batched`.** Document the recipe in `std.embeddings` docs: normalize query and corpus, then call `dot_batched(query_norm, corpus_norm)`. Zero API change; user has to know the pattern. The BLAS-3 win is accessible but discoverability is poor.
- **(β) Widen `cosine_similarity_batched` to accept `Tensor[f32, [Q, D]]` for the query side.** Backwards-compatible if dispatched on the query rank (`[D]` → SGEMV path, `[Q, D]` → SGEMM path). Adds one function-overload-shaped surface to v66.
- **(γ) Add a new `cosine_similarity_matrix(queries, corpus) -> Tensor[f32, [Q, N]]`.** Cleaner naming, no rank-dispatch ambiguity, slight API growth (6 functions instead of 5). Most analogous to NumPy / scikit-learn conventions.

The §3 spine commitment is correct under all three: `dot_batched` is the load-bearing BLAS-3 kernel either way. The choice between α/β/γ is a v66 API question, not a v67 SIMD-strategy question. Resolved to γ above.

**Transcendentals need a separate implementation surface.** `Vector[f32, N].exp()` does not fall out of `Vector[T, N]` arithmetic, and LLVM auto-vec will *not* substitute scalar `expf` calls with vectorized exp — that's a known auto-vec dead end. The fix is to ship SIMD-friendly polynomial approximations (Schraudolph's exp, Sleef-class for `log`/`sin`/`cos`, Padé approximants where needed). This implies a new sub-surface — provisional name `std.simd.math` — with `Vector[f32, N].exp()`, `.log()`, `.sqrt()`, `.tanh()`, `.sigmoid()` as the entry points. Tier: P1 (v1), Phase 11. Without this, every transcendental row in the spine table degrades to "auto-vec maybe, scalar usually" — the 4–8× speedup vanishes.

**What this does NOT commit to:**
- Hand-writing all 40+ Tensor element-wise functions (only the spine).
- Hand-writing `std.linalg` (LAPACK delegation; `deferred.md` already locks this).
- Hand-writing `std.fft` (Cooley-Tukey or library delegation).
- Outperforming hand-tuned LAPACK / MKL. Perf targets are **per-kernel and tied to the speedup column** — BLAS-3 rows target NumPy parity (NumPy itself calls into OpenBLAS/MKL; matching is the goal, not beating); BLAS-1 memory-bound rows target NumPy ±20%; transcendental rows target ~2× of NumPy (NumPy's `exp` calls into libm which is already vectorized on most platforms — closing this gap depends on `std.simd.math` quality).

### 3.2 Alternatives considered

- **(A) Trust LLVM entirely, document the risk in `deferred.md`.** Cheapest. Highest exposure on benchmark day.
- **(B) Hand-write the embeddings spine only (4 kernels), trust LLVM for Tensor element-wise.** Middle ground. Embeddings is the most marketed-adjacent surface; element-wise is more diffuse so the per-op risk is smaller individually but compounds at the autograd level.
- **(C) Proposed: hand-write the 8 kernel families in §3.1.**
- **(D) Hand-write everything in `std.embeddings` + `std.autograd` + Tensor ops.** Largest cost (months). Diminishing returns past the spine.

**Recommendation: (C).** It's a bounded, named scope. It converts the v66 narrative from a hope to a number. It is the minimum that survives a benchmark-day adversarial reading of the launch.

### 3.3 Tier

**P1 (v1), Phase 11.** Implementation lands alongside the `std.embeddings` / `std.autograd` graduation work — same engineering surface, same author, same review. Not P0 because nothing else gates on it; not P2 because the v66 narrative ships at v1 and the perf claim ships with it.

### 3.4 Carry cost

Each hand-vectorized kernel is ~30–80 lines of `Vector[T, N]` code per element-type instantiation, plus a scalar fallback for `#[cfg(not(simd_target))]`-equivalent paths (whatever shape that takes in Kāra — see §4.2). Test surface: golden numerical tests against scalar-loop reference + bit-exact reproducibility tests. The cost compounds across `f32`/`f64`/`f16`/`bf16` once mixed-precision lands; macros (or the existing typeclass dispatch) keep duplication contained.

**Honest risk:** ABI/numerics drift between SIMD and scalar paths (associativity in reductions, rounding in transcendentals). Standard answer: lock the order of summation in the SIMD path, document it in `data.md`, and treat user-observable bit-exactness as a guarantee **for a given execution path** — same target, same compile flags, same hardware feature level. Cross-path bit-exactness (SIMD vs scalar fallback; AVX-2 baseline vs an AVX-512 multiversioned variant) is *not* promised, because the reduction order differs by construction. Polars and NumPy make the same scoped commitment — reproducibility within an execution path, not equivalence across paths.

---

## Problem 4 — Three policy / API decisions that are smaller but precondition the kernels

### 4.1 `Tensor[T, S]` ↔ `Vector[T, N]` interop API (resolved 2026-05-13)

**Resolved 2026-05-13:** approve the proposed four signatures (`chunks_simd`, `chunks_simd_mut`, `load_simd`, `store_simd`) verbatim. Tail idiom: `(Iter[Vector[T, N]], Slice[T])` — most explicit, matches existing stdlib iterator patterns. Strided handling: panic on non-contiguous, caller must `.contiguous()` first (NumPy/PyTorch convention). Multi-dim: chunk the last axis (row-major); axis-stride-aware SIMD deferred to v1.5. Gather/scatter for non-contiguous SIMD deferred.

**Implementation note for the graduation pass:** `chunks_simd` returning both an iterator and a tail Slice over the same Tensor is a *split borrow* — Kāra's ownership system already handles this via `Slice[T]`'s borrow representation (Slice is a borrow form, not an owned thing), but the implementation must verify the split point is not reachable through both handles simultaneously — same shape as Rust's `split_at_mut`. Worth a one-line note in `design.md` adjacent to the API.

**Original decision framing (kept for historical record):** the API shape for iterating over a Tensor as a stream of `Vector[T, N]` chunks (and storing back). Without this, the hand-vectorized kernels in §3 cannot be written portably — the author has to drop to FFI.

**Proposed shape:**

```kara
// Iterator interface — yields successive Vector[T, N] chunks of a contiguous Tensor view.
// Returns (head: Iter[Vector[T, N]], tail: Slice[T]) — tail handles the non-multiple remainder.
fn chunks_simd[T, S, const N: i64](t: ref Tensor[T, S]) -> (Iter[Vector[T, N]], Slice[T])

// Mutable counterpart for in-place ops.
fn chunks_simd_mut[T, S, const N: i64](t: mut ref Tensor[T, S]) -> (Iter[mut ref Vector[T, N]], mut Slice[T])

// Single-vector load/store at a given offset, with mask for the tail.
fn load_simd[T, const N: i64](t: ref Tensor[T, [?]], offset: i64, mask: Mask[N]) -> Vector[T, N]
fn store_simd[T, const N: i64](t: mut ref Tensor[T, [?]], offset: i64, mask: Mask[N], v: Vector[T, N])
```

**Open sub-questions:**
- **Idiom for the tail.** Three established patterns: (a) iterator yields `Vector` chunks then a `Slice` for the remainder (proposed); (b) iterator yields `(Vector, Mask)` with the final chunk masked; (c) loop runs to the multiple, then a scalar tail loop (most explicit, also most boilerplate). Proposal: ship (a) at v1, add masked-load/store ops in v1.5 once we have user feedback on what reads best.
- **Strided iteration.** `Tensor` views can be non-contiguous (transpose, slice with step). Initial cut: `chunks_simd` requires `t.is_contiguous()` and panics otherwise — caller must `.contiguous()` first (already standard in NumPy / PyTorch). Non-contiguous SIMD via gather/scatter is deferred.
- **Multi-dim.** `Tensor[T, [M, N]]` — chunk the last axis (row-major). Higher-dim SIMD (axis-stride-aware) is deferred.

**Tier:** P1 (v1), Phase 11. Same window as the §3 kernels — they author against this API.

### 4.2 CPU-feature multiversioning policy (resolved 2026-05-13)

**Resolved 2026-05-13:** approve the **P+R hybrid** — `cpu-baseline = "v3"` default (single binary, no runtime dispatch for the common case), with opt-in `#[multiversion(baseline, "avx2", "avx512f")]` (and ARM analogues) for the §3 spine kernels. The proposed `#[target_feature(...)]` and `#[multiversion(...)]` attribute syntax is approved. ARM coverage is added in §4.2.1 below; without it the multiversioning story would have been x86-only, which is wrong given Apple Silicon and AWS Graviton are major dev / server targets. The honest ABI/inlining flag in this section stays.

**Original decision framing (kept for historical record):** what CPU feature level does `karac build --target x86_64-linux` assume, and how does the stdlib's hand-vectorized kernels handle host hardware variance?

Three live options:

- **(P) Single lowest-common-denominator baseline.** `karac.toml` knob: `cpu-baseline = "x86-64-v3"` (default = `"x86-64-v2"`?). Maps to LLVM's named feature levels (SSE4.2 / AVX / AVX2 / AVX-512). What Go does up to 1.21 (now Go also has GODEBUG-style runtime dispatch, mirroring P below).
- **R) Runtime feature dispatch via function multiversioning.** `#[target_feature(avx2)]` / `#[target_feature(avx512f)]` attributes on duplicate kernel implementations; a dispatcher selects at first call. What `glibc`, `simsimd`, `sleef` do. Adds slight per-call indirection (mitigated by ifunc-style first-call patching).
- **N) `--target-cpu=native` only.** What `design.md:10470` currently mentions. Fine for self-hosted deploys, terrible for distribution.

**Recommendation: P + R, hybrid.** Default behavior: `cpu-baseline = "x86-64-v3"` for v1 (covers ~95% of x86 hardware deployed in 2026; excludes pre-Haswell), single binary, no runtime dispatch. **Opt-in to multiversioning** via `#[target_feature(...)]` attribute on functions that benefit (the §3 spine kernels) — those get an AVX-512 variant alongside the AVX2 baseline, with a dispatcher.

Syntax (proposal):

```kara
#[target_feature("avx512f", "avx512bw")]
fn cosine_similarity_avx512(a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32 { ... }

#[multiversion(baseline, "avx2", "avx512f")]
fn cosine_similarity(a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32 {
    // Compiler synthesizes the dispatcher; body is the baseline implementation.
    ...
}
```

`#[multiversion]` is sugar over multiple `#[target_feature]` variants of the same function. The dispatcher is a runtime function-pointer set on first call (ifunc on Linux; manual pointer-swap on macOS/Windows).

**Tier:** the attribute syntax is P1 (v1) — it ships at the same time as the hand-vectorized kernels in §3, because those kernels need it. The `cpu-baseline` knob in `karac.toml` is also P1.

**Out of v1 scope:** ARM SVE (variable-length), function-multiversioning across non-CPU dimensions (e.g., GPU). Both clean future extensions.

**Honest open question:** ABI / inlining interaction. Function-multiversioning canonically defeats inlining of the multiversioned function (since the call site doesn't know which variant). Mitigated by ensuring the multiversioned function is *itself* the hot kernel — the caller is cold, the callee is hot, so inlining at the call site is low-value anyway. Worth a footnote in the design once specced.

### 4.2.1 — Per-architecture baseline mapping for `cpu-baseline`

The `cpu-baseline` knob is **target-agnostic at the surface**; the actual feature implications are per-architecture. One knob value picks the corresponding tier on whichever target the build is for:

| Knob value | x86_64 (`-march=...`) | aarch64 (`-march=...`) |
|---|---|---|
| `"v1"` | `x86-64` (SSE2 baseline) | `armv8-a` (NEON baseline) |
| `"v2"` | `x86-64-v2` (SSE3/SSSE3/SSE4.1/SSE4.2/POPCNT) | `armv8.2-a` (FP16, dotprod) |
| `"v3"` (default) | `x86-64-v3` (AVX/AVX2/BMI/FMA) | `armv8.4-a` (extended FP16) |
| `"v4"` | `x86-64-v4` (AVX-512F/BW/CD/DQ/VL) | `armv8.6-a` (BF16, I8MM) |

Default `"v3"` covers ~95% of x86 hardware deployed in 2026 (excludes pre-Haswell) and aligns with the recent Linux distro shift (RHEL 9, Ubuntu 23.10+) toward v3-class baselines. On aarch64, `armv8.4-a` is the corresponding sweet spot — Apple M1 and later are ARMv8.4+; AWS Graviton 3 is ARMv8.4-A; Graviton 4 is ARMv9-A (a strict superset). Default users on M-series Macs and modern Graviton get v3 baseline automatically.

**Why v4 on aarch64 maps to ARMv8.6-A, not ARMv9-A.** ARMv9 made SVE2 mandatory, but Kāra's `Vector[T, N]` is *fixed-length by construction* and lowers to NEON, not SVE. Variable-length SVE programming as a SIMD model is explicitly out of v1 scope per the line above. ARMv8.6-A's BF16 and I8MM extensions deliver the practical "modern feature-rich" payoff (Tensor Core-style INT8 matmul, BF16 arithmetic) without entering SVE territory. SVE / SVE2 as a separate programming model is a clean post-v1 extension.

**Opt-in multiversioning attribute names per architecture.** `#[target_feature("avx512f")]` / `#[target_feature("avx512bw")]` for x86; `#[target_feature("sve2")]` / `#[target_feature("i8mm")]` / `#[target_feature("bf16")]` for aarch64. Same `#[multiversion(...)]` sugar wraps either set.

### 4.3 Stdlib-internals SIMD policy for backend hot paths (resolved 2026-05-13)

**Resolved 2026-05-13:** approve the one-paragraph addition to `design.md § Standard Library` verbatim. The bullet list of *what this unlocks* (simdjson, simdutf, HTTP header tokenization, Hyperscan, AES-NI) remains framed as opportunity, not commitment — individual kernels follow per-workload as benchmarks justify.

**Original decision framing (kept for historical record):** explicit policy that stdlib internals may use `Vector[T, N]` and `#[target_feature]` paths for hot operations — even though the user-facing API is scalar.

The backend-first persona's hot paths are not in `std.embeddings` — they are in JSON parsing, UTF-8 validation, HTTP header tokenization, hashing, regex prefilter, base64. These are where Kāra gets benchmarked against Go and Rust on day one. None of them have an explicit SIMD plan today.

**Recommendation: ship as policy, not features.** A short section in `design.md § Standard Library` stating:

> Stdlib implementations may use `Vector[T, N]` and `#[target_feature]` paths internally where doing so provides a measurable improvement on representative workloads. The user-facing API surface remains scalar. Multiversioned variants follow §4.2.

**What this unlocks (incrementally, post-v1 as needed — no commitment to all of these at launch):**

- simdjson-class `std.json` parsing (3+ GB/s achievable).
- simdutf-class UTF-8 validation in `std.text` / string boundary checks.
- SWAR-accelerated HTTP header tokenization in `std.http`.
- Hyperscan-style literal-set prefilter in `std.regex`.
- AES-NI / SHA-NI in `std.crypto` (already implicit since `std.crypto` delegates to a vetted C library — but worth confirming the FFI ABI lets the C library use those features without Kāra-side gating).

**Tier:** policy lands at v1 P1 (a one-paragraph design.md addition). Individual kernels follow per-workload, prioritized by benchmarking.

---

## Problem 5 — Settled items that can graduate to canonical directly

These have no open design questions. They are documentation gaps or explicit commitments missing from the canonical record. **Each can graduate without further brainstorm discussion** — listed here for traceability.

### 5.1 WebAssembly SIMD-128 lowering — explicit commitment

`design.md § Portable SIMD` describes the auto-fallback chain in terms of "native instruction → wider-lane masked → scalar," with examples for x86 / ARM / GPU. WebAssembly SIMD-128 is implicit in the chain but not named. Phase 10's WASM section doesn't call it out either.

**Graduation target:** one line in `design.md § Portable SIMD — Vector[T, N]` adding wasm-simd-128 to the lowering tier examples; matching line in `phase-10-targets.md`. Browser-playground perf benchmarks depend on this being committed.

### 5.2 SIMD × auto-parallelization composition note

The runtime composes two independent dimensions: auto-par across cores (`src/concurrency.rs`) and SIMD within a lane (`Vector[T, N]`). The composition order, expected speedup model, and whether `par for` loops use SIMD chunks per parallel chunk is undocumented.

**Graduation target:** short subsection in `design.md § Feature 5 — Auto-Concurrency`, three paragraphs: (a) composition is multiplicative on workloads that admit both — auto-par splits the iteration space across cores, SIMD chunks each thread's slice; (b) the runtime does not move SIMD lanes across threads — threads cooperate at chunk boundaries only; (c) `#[require_simd]` and auto-par are orthogonal — a `#[require_simd]` kernel inside a `par for` body must still vectorize, which means the body must remain auto-vec friendly (no early exit, no function calls). Cross-reference from `book/ch15-data-layout.md` and from any concurrency book chapter.

### 5.3 `Vector[T, N]` trait surface enumeration

The Portable SIMD section in `design.md` defines layout, fallback, and arithmetic at a high level. The full trait surface (horizontal sum, horizontal max, lane shuffle, masked load/store, gather/scatter, lane-by-lane comparison producing `Mask[N]`, blend/select) is not enumerated.

**Graduation target:** trait-surface table inline in `design.md § Portable SIMD`. Borrow the Rust `std::simd` surface as the v1 baseline; mark gather/scatter as "P1 if cheap, P2 if not."

### 5.4 `deferred.md` entry: hand-vectorized data-spine commitment

Once §3 is decided, add a `deferred.md`-style entry in the v1 P1 section recording the commitment, the kernel list, the **per-kernel speedup targets from §3.1's table** (BLAS-3 rows → NumPy parity; BLAS-1 memory-bound rows → NumPy ±20%; transcendental rows → ~2× of NumPy contingent on `std.simd.math`), the bit-exactness guarantee, and the `std.simd.math` sub-surface commitment for transcendentals. Even though this ships at v1, the entry serves as the design record for "why these kernels exist as hand-written code rather than scalar loops."

---

## Problem 6 — Meta observations

### 6.1 None of this changes the language surface

Every decision in §3, §4, §5 is either a stdlib-internals commitment, a build-system knob, or a documentation addition. **No new keywords. No new syntax beyond the `#[multiversion]` attribute (§4.2), which is itself sugar over `#[target_feature]`.** This is a stdlib-and-policy brainstorm, not a language-surface one.

### 6.2 The Mojo comparison is not a competitive frame

The trigger conversation compared Kāra to Mojo. The honest read: Mojo is in a different room (AI/ML accelerators on Modular's commercial stack); Kāra is in the backend/general-purpose room. The §3 hand-vectorized spine does not move Kāra into Mojo's room — it protects Kāra's v1 ML/data narrative *under its own framing* (general-purpose with quiet data bonus, v66) from a benchmark-day reality check. The competitive comparison that matters is Go and Rust on backend workloads, where §4.3 (stdlib-internals SIMD) is the load-bearing axis.

Note on heterogeneous compute. v67 is the **CPU half** of an existing heterogeneous-compute capability surface — `#[gpu]` functions (Phase 10), `Vector[T, N]` unifying CPU SIMD and GPU vectors (`design.md § Portable SIMD`), `f16`/`bf16` primitives targeting NVIDIA Tensor Cores / Apple ANE / Google TPU (`design.md § f16 / bf16`), and `GpuTensor[T, Shape]` with explicit `.on(gpu)` / `.to_cpu()` boundary ops (`phase-11-stdlib-longtail.md:897`) — all already canonical. Per v66 § Problem 1, this capability ships at v1 but is **not** a positioning axis; "heterogeneous compute" as a launch pitch is rejected against, because the lead persona is general-purpose backend and the data/GPU work is the quiet bonus. Feature gaps beyond CPU+GPU (NPU/TPU/FPGA backends, unified-memory abstraction, kernel fusion across CPU↔GPU boundaries) are post-v1 work and belong in their own brainstorms or as new `deferred.md` entries — not in v67.

### 6.3 The compute-vs-memory frame is the lens every reviewer applies

The BLAS-1/2/3 taxonomy and the compute-bound-vs-memory-bound axis (arithmetic intensity, the roofline model) is the standard performance-engineering vocabulary in HPC and ML compiler circles. **Every Kāra perf claim will get audited against it.** A reviewer reading "Kāra hand-vectorizes embeddings" expects 5–10× on the BLAS-3 batched ops and 2–4× on the BLAS-1 reductions; pre-committing to those numbers in v67 (per the §3.1 table) lets the launch-day benchmark survive an adversarial reading because the reviewer's mental model and Kāra's stated targets line up. The opposite failure mode — claiming "8× faster across the board" — invites the obvious follow-up ("on which kernels? memory-bound or compute-bound?") that no marketing answer survives. Framing the v67 commitments in BLAS / intensity vocabulary by construction is defensive in the right way.

### 6.4 Measure first

Per `feedback_measure_first_empirical`, the right next step before fully committing §3 scope is to **benchmark LLVM auto-vectorization on a representative kernel** (e.g., `cosine_similarity_batched` on `Tensor[f32, [10_000, 768]]`) once the Phase 7 LLVM backend can compile it. If LLVM auto-vec already hits 80% of hand-written SIMD on the proposed spine, the §3 scope shrinks to "the 2-3 kernels LLVM misses." If it hits 20%, §3 as written is the floor. The decision in §3 should be conditional on this measurement, not pre-baked.

---

## Open questions for the user

~~1. **§3 — Hand-vectorized data spine.**~~ **Resolved 2026-05-13: option C + measure-first sequencing.**
~~1a. **§3.1.1 — BLAS-3 cosine path.**~~ **Resolved 2026-05-13: option γ.**
~~2. **§4.1 — Tensor↔Vector API.**~~ **Resolved 2026-05-13: approved as proposed.**
~~3. **§4.2 — Multiversioning.**~~ **Resolved 2026-05-13: P+R hybrid with proposed `#[multiversion]` syntax; per-arch baseline mapping added as §4.2.1.**
~~4. **§4.3 — Stdlib-internals SIMD policy.**~~ **Resolved 2026-05-13: approved verbatim.**
5. **§5 — Graduation trigger.** All v67 resolutions plus the four §5 documentation graduations are ready to land in `design.md` / `deferred.md` / `phase-10-targets.md` / `phase-11-stdlib-longtail.md` / the v66 `std.embeddings` entry in **one pass**. Pending only the explicit "graduate now" trigger.
~~6. **§6.4 — Sequence.**~~ **Resolved 2026-05-13: folded into §3 — measurement narrows the kernel list once Phase 7 LLVM backend can compile a representative kernel.**

---

## Cross-references

- `design.md § Portable SIMD — Vector[T, N]` (~line 12400) — existing language surface.
- `design.md § Layer 3` (line 194) — auto-vectorization as compiler choice.
- `design.md § Numerical Types (Tensor, Column, DataFrame)` (~line 1925) — Tensor / Column / DataFrame.
- `deferred.md § std.embeddings`, `§ std.autograd`, `§ Lazy DataFrame Query Planner` — v66 graduation entries that this brainstorm partially de-risks.
- `deferred.md § Tensor Element-Wise Math` (~line 1346) — "vectorize via LLVM" entry that §3 reframes.
- `roadmap.md § Phase 11 Stdlib Long-Tail` — `std.embeddings`, `std.autograd`, statistical methods.
- `phase-11-stdlib-longtail.md` — checklist entries that need updating once §3 graduates.
- `brainstorming/archive/v66_general_purpose_with_data_bonus.md` — the graduation that put the v1 ML/data narrative on the plate.
- Stored memory `feedback_v1_ship_reality_not_promises` — frames why §3 matters.
- Stored memory `feedback_measure_first_empirical` — frames §6.3 sequencing.
- `docs/investigations/parallax_perf.md` — documents one real auto-vec failure case.
