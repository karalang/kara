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

## Resolution (shipped) — the minimal-C variant

Landed the **lightest form of Option C**: rather than emitting
`@llvm.vector.reduce.fadd` or a manual `Vector` accumulator, codegen now tags
the *existing* scalar reduction fold (`fadd` for `sum`/`mean`, `fmul` for
`prod`) with the `reassoc` fast-math flag (`tag_reduce_reassoc`,
`src/codegen/kernel.rs`), applied at both fold emission sites (`emit_reduce_fold`
and the validity-gated `emit_reduce_fold_gated`). That single permission is all
LLVM's loop vectorizer needs to recognize the reduction and rewrite the scalar
chain into packed adds + a horizontal sum — no new intrinsic, no Tensor
`as_slice` plumbing, and it is **corpus-wide** (every f32/f64 `Tensor.sum` /
`mean` / `prod`, `Column.sum` / `prod`, and `Stats.sum`, plus the `dot` /
`cosine` kernels above, all inherit it).

**Re-measurement** (same `dot(ones, ones)` binary, `<dot>` disassembly): the
reduction went from `vaddps` 0 / `vaddss` 9 (the dead-end above) to **`vaddps` 9
/ `vaddss` 2** (the 2 residual scalars are the horizontal-reduce tail +
remainder). A `Tensor[f32, [1024]].sum()` over a `ref` param (const-fold
defeated) shows the same 9 packed adds end-to-end. Correctness unchanged
(`dot(ones,ones)` = 768; `Tensor[f32,[1024]].sum()` of ones = 1024).

**Divergence scope, as predicted.** Only the interpreter-vs-`build` low-order
bits of a *non-exact* float reduction move (the shipped-`v.exp()`/`v.ln()`
class); the AOT binary is deterministic run-to-run (fixed lane count, fixed
horizontal-sum tree — distinct from the `#[fp_reassoc]`-gated *parallel* float
reduction, which is gated for cross-thread nondeterminism). Exactly-representable
inputs (small integers held in f32) stay bit-identical across backends. Guarded
by `test_ir_float_tensor_reduce_carries_reassoc` (the `reassoc` flag is emitted
for float, absent for int) and `test_e2e_float_tensor_reduce_exact_value_bit_identical`
(a 1024-wide vectorized reduction over exact values stays byte-identical to the
interpreter twin) in `tests/codegen.rs`.

### Follow-on — fusing the two-pass shape (point 3 above)

The reassoc win vectorized the reduction but left the **wasted intermediate**
(spike point 3): `a.zip_with(b, |x, y| x * y).sum()` still materialized a full
`[D]` products tensor and read it back. That is now fused. Codegen recognizes a
`<tensor>.zip_with(other, |a, b| ..).<reduce>()` / `<tensor>.map(|x| ..)
.<reduce>()` chain (reduce ∈ `sum`/`mean`/`prod`) with an inline closure over a
tensor-binding base and emits ONE accumulating loop
(`emit_fused_map_reduce`, `src/codegen/kernel.rs`, hooked in
`try_emit_fused_map_reduce`, `src/codegen/tensor.rs`) — the products never
materialize. Corpus-wide, generalizes to user code, and needs no
`Tensor→Slice` primitive (Option A's prerequisite gap). Empty policy, seed,
`reassoc` tagging, and the `mean` divide all mirror `emit_reduce_fold`, so it is
bit-identical to the materialize-then-reduce path it replaces.

**Re-measurement** (`dot(ones, ones)` at D=1024, AOT): heap allocs for the
`dot` body drop from **3 → 2** (the two inputs remain; the intermediate
products tensor is gone), with the loop still packed (`vmulps` + `vaddps` in one
pass, no `vfmadd` — the mul and add stay separate packed ops, which is the main
win; FMA contraction is a possible further step, deliberately not taken to keep
the divergence surface at just `reassoc`).

**Stdlib kernels rewritten to the chained form** (`runtime/stdlib/embeddings
.kara`) so they inherit the fusion: `dot`, `l2_norm`, `cosine_similarity`, and
`dot_batched`'s per-row `query.zip_with(row, ..).sum()`. **Residual:**
`cosine_similarity_batched` and `cosine_similarity_matrix` keep the let-bound
`zip_with` method form — their per-row bodies reuse an iter_axis **row-view**
across two combines, which trips an ownership false-positive on the chained form
(B-2026-07-20-6), and delegating to `dot`/`l2_norm` is out because the module is
deliberately self-contained (gated imports splice only the named fn). Their
inner loops do not fuse until B-2026-07-20-6 is fixed. Guarded by
`test_ir_chained_zip_reduce_fuses` (fusion fires: `fmr.*` blocks present),
`test_ir_letbound_zip_reduce_does_not_fuse` (the let-bound form stays on the
materialize path), and `test_e2e_chained_map_zip_reduce_parity` in
`tests/codegen.rs`; the four `test_e2e_embeddings_*` E2E tests cover the
rewritten kernels.
