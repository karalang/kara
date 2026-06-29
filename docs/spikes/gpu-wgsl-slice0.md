# Spike: GPU WGSL codegen — slice-0 (smallest end-to-end dispatch)

**Status:** ⬜ OPEN — **scoping/sketch only, not an approved plan.** This spike defines the smallest provable increment of the GPU compute backend (one `#[gpu]` kernel → generated WGSL → wgpu dispatch → result read back) so the tracker's "weeks not days" estimate has a concrete first step to judge against. It also documents a **standing strategic tension**: [roadmap.md § Phase 10 > GPU compute shaders](../roadmap.md) (2026-06-10 resequence note) puts GPU codegen in the *"built once, directly in Kāra"* bucket — i.e. **don't build it in the Rust `karac`, build it in the self-hosted compiler**. This spike scopes the *Rust-now* alternative and the trade-off, so the decision can be made with a real first increment in view rather than in the abstract. **No code has been written.** Building slice-0a means adding `wgpu` to the runtime crate and reversing that roadmap decision — gate on an explicit go.

## Question

GPU codegen (WGSL lowering + wgpu device/buffer/dispatch) is entirely unbuilt — there is **zero** `wgpu`/`WGSL`/`naga`/`NVPTX` anywhere in the Rust compiler. The front-end contract is *done* and enforced ([`phase-10-targets.md`](../implementation_checklist/phase-10-targets.md) Track A: FE-1–4 + residuals + SL-1), so `#[gpu]` kernels are guaranteed GpuSafe and effect-clean. **What is the smallest end-to-end increment that proves the execution spine, and where does the real cost sit?**

## Strategic context (read first)

The roadmap's three-bucket model for compiler-internal work at the self-hosting pivot:

- **Stdlib in Kāra** (`*.kara`) — reused verbatim by the self-hosted `karac`.
- **Already built in Rust** (typechecker, the FE-1–4 GPU front-end, existing codegen) — *ported* to Kāra; the Rust version is the spec + differential oracle + near-line-for-line translation source. Design/debug effort is sunk, not discarded.
- **Not yet built** (GPU codegen, `f16` lowering, …) — *"built once, directly in Kāra. This is the only bucket the pivot saves work on."*

GPU codegen is classified in the third bucket. The bet: building the WGSL/wgpu backend in Rust *then* porting it = two builds; building it directly in Kāra after self-hosting = one build. A nuance specific to **codegen** (vs a pure-logic pass): the Rust backend is bound to `inkwell`/`wgpu`, so its port is *less* mechanical than a logic pass — the backend-interfacing parts get re-expressed against the Kāra compiler's own backend layer, not copied line-for-line. That is why codegen is bucket-3 (build-once-in-Kāra) rather than bucket-2 (mechanical port).

**Why this spike exists anyway.** If self-hosting is far out, the calculus shifts: a Rust GPU backend buys a working, dogfoodable GPU path (the Metal-on-macOS "non-negotiable" in [roadmap.md](../roadmap.md)) *years* earlier, plus — once it exists — a **reference oracle** that makes the eventual Kāra port safer and faster. The trade is "build once in Rust + port later" vs "build once in Kāra, but only after self-hosting lands, with no reference implementation in the meantime." That is a timeline-and-priorities call for the project lead, not something to flip silently. This spike makes the first increment concrete so the call is informed.

## Slice-0 kernel — element-wise map

The honest "hello GPU compute": an embarrassingly-parallel element-wise map — no reduction, no shared/workgroup memory, no control flow.

```
#[gpu]
fn double(x: f32) -> f32 { x * 2.0 }
```

Dispatched over a buffer of `f32`, **one GPU invocation per element**. Semantic contract for slice-0: a kernel `fn k(x: T) -> U` dispatched over a `[T]` buffer produces a `[U]` buffer.

> **Semantic choice to confirm.** [design.md § GPU Subset Constraints](../design.md#gpu-subset-constraints) shows `gpu.dispatch(dot, a, b)` where the kernel takes *whole arrays* and `dot` even reduces to a scalar. **Reductions need workgroup memory and are explicitly NOT slice-0.** The per-element-map form is the cleanest floor; the whole-array / reduction / multi-buffer forms are later increments. Confirm the per-element-map contract before 0c wires `gpu.dispatch`.

## What the codegen emits (WGSL)

```wgsl
@group(0) @binding(0) var<storage, read>       input:  array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= arrayLength(&input)) { return; }
    output[i] = input[i] * 2.0;        // ← the ONLY kernel-specific line
}
```

Everything except `input[i] * 2.0` is fixed boilerplate. So **WGSL codegen slice-0 = emit the wrapper + lower one kernel body**, where the kernel param `x` maps to `input[i]`. The body emitter is a small AST→WGSL-text walk over the trivial subset: integer/float literals, binary arithmetic ops, identifiers (param → indexed buffer load), and the return expression (→ `output[i] = …`). Order-of-magnitude ~150 lines.

**Architecture placement — respects the containment invariant.** The WGSL emitter is a **plain-string module** (`src/gpu_wgsl.rs`), *not* part of `src/codegen.rs`. WGSL is text — no `inkwell`/LLVM types — so the [codegen-containment invariant](../../CLAUDE.md) (only `codegen.rs` imports `inkwell`) holds. `codegen.rs` consumes the generated WGSL string as plain data and bakes it into the binary as a constant, then emits a call to a runtime symbol. This mirrors how every other analysis pass feeds codegen via plain-data hints.

## Runtime spine — where the real unknowns are

The wgpu calls live in **`karac-runtime`** behind a C symbol, e.g.:

```
karac_runtime_gpu_dispatch_f32(wgsl_ptr, wgsl_len, in_ptr, n_elems) -> out_ptr
```

`codegen.rs` emits a call to that extern with the compile-time-baked WGSL string and the input buffer — the **same pattern as every existing runtime extern** (`karac_runtime_*`). The runtime body:

```
1. instance → request_adapter().await → request_device().await   // ASYNC — block_on via the tokio already in the native archive
2. create_shader_module(wgsl)
3. input buffer  (STORAGE | COPY_DST) + queue.write_buffer
   output buffer (STORAGE | COPY_SRC)
   staging buffer (MAP_READ | COPY_DST)
4. compute pipeline + bind group
5. encoder: begin_compute_pass → set_pipeline/set_bind_group → dispatch_workgroups(ceil(n/64))
   copy_buffer_to_buffer(output → staging)
6. queue.submit; staging.slice().map_async(Read); device.poll(Wait); read bytes back
```

**This is where the "weeks" actually is — not the codegen.** The unknowns:

- **Async device init.** `request_adapter`/`request_device` are async. The native runtime archive already links tokio, so `block_on` is available — but the GPU init/teardown lifecycle (per-call vs cached device) needs a decision.
- **The `wgpu` dependency.** Adding `wgpu` to `karac-runtime` triggers the [3-place runtime-archive rebuild dance](../../CLAUDE.md) (lean/full/installed) and a **wasm story** — wgpu does not build the same on `wasm32` (the browser path is WebGPU via JS glue, a separate lowering). Slice-0 is **native-only**; the wasm/browser GPU path is explicitly out of scope.
- **Buffer lifecycle / mapping.** `map_async` + `poll(Wait)` is the readback handshake; getting ownership and the staging-buffer copy right is the fiddly part.

## Increment breakdown — so "weeks" has structure

| Slice | Proves | De-risks | Rough size |
|---|---|---|---|
| **0a** — runtime spine with *hand-written* WGSL | wgpu plumbing works: Metal on the dev Mac doubles a buffer end-to-end | the genuine unknowns — async device init, buffer mapping, the `wgpu` dependency + archive rebuild | a few days |
| **0b** — WGSL codegen for the `double` shape | AST→WGSL for the trivial subset, replacing the hand-written shader | the codegen surface (the *easy* part) | 1–2 days |
| **0c** — wire `gpu.dispatch` (SL-2) to invoke it | end-to-end from Kāra source; honest `gpu.dispatch` typing | the call-site intrinsic typing (also lands SL-2 for real) | 1–2 days |

**Do 0a first.** It front-loads the real risk (wgpu) before any codegen investment. If 0a runs on Metal, the full-backend estimate (control flow, structs, `Array[T,N]`, layout-group buffer coalescing, reductions, the real per-buffer effect inference) is grounded. If 0a fights, that is learned cheaply, before building the emitter.

## Mapping to the tracker

- **0a + 0b** = [`phase-10-targets.md`](../implementation_checklist/phase-10-targets.md) **CG-1** (WGSL codegen) + **CG-2** (wgpu integration).
- **0c** = **CG-3** (`gpu.dispatch` runtime) + **SL-2** (`gpu.dispatch` typing surface — which slice-0c lands *honestly* rather than as a stub-that-errors).
- Explicitly **out of slice-0**: **CG-4** (layout groups → coalesced GPU buffers), **CG-5** (CUDA/NVPTX), **CG-6** (`KARAC_GPU`/`KARAC_GPU_BACKEND` selection), reductions/workgroup memory, the browser/wasm WebGPU path, and the design's per-buffer `reads/writes(GpuBuffer[buf])` input-vs-output *inference* (slice-0c attributes a conservative effect; the precise parameterization is CG-4-coupled).

The FE-1–4 front-end (done) already guarantees the kernel is GpuSafe + effect-clean, so the emitter can assume a clean subset — that prior work pays off directly here.

## Definition of done (this spike)

This spike is **resolved** when one of:

1. **0a runs green** — a hand-written WGSL compute shader doubles an `f32` buffer on the dev Mac's Metal backend through `karac-runtime`, validating the wgpu spine — *and* the project lead confirms proceeding to 0b/0c (reversing the build-in-self-hosted default for the GPU backend); **or**
2. **Decision recorded to hold** — GPU codegen stays bucket-3 (built in the self-hosted compiler); this spike is closed as "scoped, deferred to self-hosting," and the slice-0 plan here becomes the build-order reference for the Kāra implementation.

## Open questions / decision needed

- **Strategic:** build the GPU backend in Rust now (this spike's 0a→0c, then the full backend), or hold for the self-hosted compiler per the roadmap? Timeline-dependent; project lead's call.
- **Semantic:** confirm the per-element-map dispatch contract (`fn k(x: T) -> U` over `[T]` → `[U]`) as the slice-0 floor, vs the design's whole-array forms.
- **Lifecycle:** per-dispatch device init (simple, slow) vs a cached device handle in the runtime (faster, needs lifecycle management) — defer to 0a findings.
