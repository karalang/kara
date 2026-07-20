# Data-spine auto-vec measurement (v67 §3)

**Date:** 2026-07-20
**Purpose:** The hand-vectorized data-spine commitment (`phase-11-stdlib-longtail.md`
line 96 / `deferred.md § Hand-Vectorized Data-Spine Commitment`) says the final
per-kernel scope "narrows kernel-by-kernel after the auto-vec measurement on a
representative kernel" — rows where LLVM auto-vec hits ~80% of hand-written SIMD
drop into a "trust auto-vec" footnote; rows where it hits ~20% stay hand-written.
This is that measurement, run now that every prerequisite ships (`Vector[T, N]`
lowering + transcendentals/rounding/shifts/bitcast, and `#[multiversion]`).

## Method

Representative kernels: `std.embeddings.dot` and `cosine_similarity`,
monomorphized at the common embedding width **D = 768** (`Tensor[f32, [768]]`).
Wrapped in `pub fn` taking `ref Tensor` params (params are opaque to the
optimizer, and `pub` keeps the body emitted), built with the default `-O2`
pipeline, and disassembled with `llvm-objdump -d`.

```kara
import std.embeddings.{dot, cosine_similarity};
pub fn measure_dot(a: ref Tensor[f32, [768]], b: ref Tensor[f32, [768]]) -> f32 {
    dot(a, b)
}
```

## Result

The `dot` symbol's floating-point instruction mix at `-O2`:

| instruction | count | meaning |
|---|---|---|
| `vmulps` (`%ymm`) | 4 | **packed** 8-wide multiply, 4× unrolled — the element-wise product |
| `vaddps` | 0 | — |
| `vmulss` | 1 | scalar remainder multiply |
| `vaddss` | 9 | **scalar** sequential adds — the sum reduction |

Plus ~142 `%ymm` references (the packed loads/stores).

**Reading:** exactly the split the commitment predicts.

1. **Element-wise multiply → already auto-vectorized.** `zip_with(b, |x, y| x * y)`
   lowers to `vmulps` on 256-bit `%ymm` vectors, 4× unrolled. LLVM does this
   for free; no hand-vectorization would beat it. → **trust auto-vec.**

2. **f32 sum-reduction → scalar (the dead-end).** `.sum()` over f32 lowers to a
   sequential chain of scalar `vaddss`. LLVM will **not** reassociate f32 adds
   under the default (non-fast-math) pipeline — reassociation changes rounding,
   so it is disallowed without `reassoc`/`-ffast-math`. This is the classic
   auto-vec dead-end and the reason the reduction kernels are on the hand-vec
   list. → **needs hand-vectorization.**

3. **Two-pass shape wastes an intermediate.** `zip_with(...).sum()` materializes
   a full `[768]` products tensor (the packed `%ymm` stores) and then reads it
   back for the scalar sum — an extra O(D) alloc + write + read. A **fused**
   multiply-accumulate (one pass, a vector accumulator) removes both the
   intermediate and the scalar reduction.

The `dot`/`l2_norm`/`cosine_similarity` single-pair kernels, and the batched
variants (`dot_batched`, `cosine_similarity_batched`, `cosine_similarity_matrix`)
that repeat the same `zip_with(...).sum()` inner loop, all inherit this scalar
reduction. Correctness of the measured binary was confirmed (`dot(ones,ones)` =
768, `cosine(ones,ones)` = 1).

## Scoped remaining work + a design fork

The win is a **fused, vector-accumulator reduction** for f32. Two ways to get
there — this is an owner/design decision because each carries a distinct cost:

- **Option A — hand-write the kernels against `Vector[f32, 8]`.** Matches the
  commitment's literal framing. A fused multiply-accumulate loop
  (`acc = acc + a_chunk * b_chunk`) over `Vector[f32, 8]` + `v.reduce_sum()`
  (both already lower) + a scalar remainder loop, optionally `#[multiversion]`'d
  for an AVX-512 (`Vector[f32, 16]`) variant. **Prerequisite gap:** there is no
  lightweight `Tensor[f32, [D]] → Slice[f32]` window today (`Vector.from_slice`
  needs a `Slice[T]`; `Tensor.slice(axis, …)` returns a sub-*Tensor*). So Option
  A needs a new `Tensor.as_slice()` / contiguous-window primitive first. Per-kernel.

- **Option C — vectorize the f32 reduction in codegen.** Make `Tensor.sum()` /
  the `Reduce.sum` lowering emit a `reassoc` vector reduction
  (`@llvm.vector.reduce.fadd` with `reassoc`, or a manual `Vector` accumulator
  loop). **Corpus-wide** — every f32 reduction (`sum`/`mean`, and thus every
  kernel above) vectorizes with no per-kernel rewrite. **Cost:** it is a
  semantic choice — reassociation changes low-order bits, so `karac run`
  (interpreter, ordered f64) and `karac build` (reassoc f32) would diverge in the
  last bits, exactly like the shipped `v.exp()`/`v.ln()` polynomials. Acceptable
  for ML/embedding reductions (SIMD reductions are the universal norm there), but
  it is a documented behavior change.

Option C is higher-leverage and matches the existing exp/ln divergence
precedent; Option A matches the literal "hand-write against `Vector[T, N]`"
wording but needs new Tensor plumbing. Recommend C unless the reassociation
divergence is unwanted, in which case A.
