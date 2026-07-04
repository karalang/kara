# Spike: GPU WGSL codegen — slice-0 (smallest end-to-end dispatch)

**Status:** ✅ **slice-0 COMPLETE — 0a+0b+0c PROVEN end-to-end on Metal (0a 2026-06-29; 0b+0c 2026-07-03).** A `#[gpu]` element-wise-map kernel now runs on the real GPU straight from Kāra source: `karac build` on `#[gpu] fn double(x: f32) -> f32 { x * 2.0 }` + `gpu.dispatch(double, buf)` compiles, links the Metal-backed runtime, and the binary doubles the buffer on this Mac's GPU (`[1,2,3,4]f32 → 2,4,6,8`), byte-identical to `karac run`'s CPU fallback. **0a** = the wgpu spine (`runtime/src/gpu.rs`, `cargo test -p karac-runtime --features gpu`). **0b** = the AST→WGSL emitter (`src/gpu_wgsl.rs`, a plain-string module — no `inkwell`, so codegen-containment holds). **0c** = `gpu.dispatch` wired through all four surfaces: resolver registers `gpu` as a magic module (like `process`/`ast`); the typechecker validates the `#[gpu] fn(f32)->f32` kernel + `Vec[f32]` buffer (honest `E_GPU_DISPATCH_*` diagnostics, SL-2) and bakes the WGSL into a `gpu_dispatch_wgsl` side table (typechecker → lowering → Program → codegen, so the `ast`-importing emit stays out of `codegen.rs`); codegen calls the runtime `karac_runtime_gpu_f32_map` symbol with the baked shader + input buffer and wraps the result as an owned `Vec[f32]`; the interpreter runs the kernel element-wise on the CPU for run==build parity. **Isolation held:** `wgpu` stays behind the opt-in `gpu` feature (a 14 MB archive) — the default/lean/wasm archives pull no `wgpu`/`naga`, and a non-GPU binary links byte-for-byte as before (the Metal frameworks + gpu archive are added only when the emitted object references `karac_runtime_gpu_*`). The gpu archive (`libkarac_runtime_gpu.a`, built `cargo rustc -p karac-runtime --release --features gpu --crate-type staticlib` + copied to the `_gpu` name) is now **auto-selected** (2026-07-03 follow-on): when the emitted object references `karac_runtime_gpu_*`, the linker resolves that archive (a superset of the full archive) instead of min/full — so `gpu.dispatch` links with no manual `KARAC_RUNTIME` (still honored verbatim as the dev override). A GPU program built without the archive present fails with an actionable "build `libkarac_runtime_gpu.a`" error; a non-GPU program links the normal min/full archive byte-for-byte as before. The archive stays opt-in (heavy wgpu) — only GPU-dispatching programs link it. The per-element-map contract (`fn k(x:T)->U` over `[T]`→`[U]`, `T=U=f32`) is the confirmed slice-0 floor; reductions / whole-array / multi-buffer forms + the Linux-Vulkan and wasm-WebGPU link stories are later increments. Original scoping sketch preserved below. The **standing strategic tension** — [roadmap.md § Phase 10 > GPU compute shaders](../roadmap.md) puts GPU codegen in the *"built once, directly in Kāra"* bucket — is resolved for slice-0 by the explicit go: the Rust backend buys a dogfoodable GPU path now + a reference oracle for the eventual Kāra port.

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
| **0a** ✅ **DONE (2026-06-29)** — runtime spine with *hand-written* WGSL | wgpu plumbing works: Metal on the dev Mac doubles a buffer end-to-end | the genuine unknowns — async device init, buffer mapping, the `wgpu` dependency + archive rebuild | *took ~½ day* |
| **0b** ✅ **DONE (2026-07-03)** — WGSL codegen for the `double` shape | AST→WGSL for the trivial subset, replacing the hand-written shader | the codegen surface (the *easy* part) | *took ~½ day* |
| **0c** ✅ **DONE (2026-07-03)** — wire `gpu.dispatch` (SL-2) to invoke it | end-to-end from Kāra source; honest `gpu.dispatch` typing | the call-site intrinsic typing (also lands SL-2 for real) | *took ~½ day* |

**Do 0a first.** It front-loads the real risk (wgpu) before any codegen investment. If 0a runs on Metal, the full-backend estimate (control flow, structs, `Array[T,N]`, layout-group buffer coalescing, reductions, the real per-buffer effect inference) is grounded. If 0a fights, that is learned cheaply, before building the emitter.

## Slice-0a findings (2026-06-29)

The spine ran on the first real attempt — the risk was lower than feared. Concrete notes for 0b/0c:

- **Dependency isolation works cleanly.** `wgpu = { optional = true }` + `pollster` behind a new `gpu` feature (*not* in `default`). A default `cargo build -p karac-runtime` pulls neither — the lean/full/wasm archive floors are untouched. The [4-archive rebuild dance](../../CLAUDE.md) is a *non-issue until 0c* (when a `karac_runtime_gpu_*` C symbol actually ships into an archive); 0a lives purely behind the test-only feature.
- **`wgpu 29.0.3` API specifics** (it churns between majors — pin/verify on bump): `Instance::new(InstanceDescriptor::new_without_display_handle())` (by value, no `Default`); `request_adapter`/`request_device` return `Result` (→ `.ok()?`); `ComputePipelineDescriptor` needs `entry_point: Some("main")` + `compilation_options`/`cache`; `device.poll(wgpu::PollType::wait_indefinitely())` (the `Wait` variant is now a struct variant).
- **Async** handled with `pollster::block_on` — no tokio-runtime-handle juggling needed for the GPU path. Revisit if 0c must share the program's tokio reactor.
- **Readback** = staging buffer (`MAP_READ | COPY_DST`) + `map_async` + `poll(Wait)` + an `mpsc` channel to await the callback. Works; the per-dispatch device init is the obvious next perf lever (cache the device — deferred, noted in Open questions).
- **Verified on Metal** (`runtime/src/gpu.rs` test, green and *not* the no-adapter skip). The `None`-on-no-adapter path means CI without a GPU skips gracefully rather than failing.

**0b is now unblocked** with the spine proven: generate the boilerplate WGSL + lower the `double` body (`x * 2.0`, `x` ↦ `input[i]`) from the `#[gpu]` AST in a plain-string `src/gpu_wgsl.rs`, and feed it to `dispatch_f32_map` instead of the hand-written constant.

## Slice-0b + 0c findings (2026-07-03)

Both landed in ~one session — smaller than the 1–2 days each budgeted. Concrete notes for the next GPU increment:

- **0b was purely additive.** `src/gpu_wgsl.rs::emit_kernel(&Function) -> Result<String, WgslError>` walks the trivial GpuSafe subset (numeric literals, binary `+ - * / %`, unary `-`, the single parameter → `input[i]`, tail-expr / `return` → `output[i] = …`) and emits the fixed `@group(0)` binding boilerplate. Everything outside the subset is a structured `WgslError`, so 0c gates cleanly rather than emitting invalid WGSL. No existing code touched — 13 unit tests. WGSL is text, so the module imports no `inkwell`; **codegen-containment held with zero friction.**

- **`gpu.dispatch` fits the existing magic-module machinery exactly.** `ast.expr(…)` / `compiler.error(…)` / `process.exit(…)` already parse as method calls on a lowercase module identifier the resolver registers as `SymbolKind::Module`. Registering `gpu` the same way (one line) let the receiver resolve; the typechecker + interpreter already gate on `object == Identifier(module)` in `infer_method_call` / `eval_method_call`, and effect + ownership tolerate a module-receiver method call with **no new intercept** (verified: the three `E_GPU_DISPATCH_*` diagnostics + the run path both work without touching those phases). This collapsed the feared 6-phase change into: resolver (1 line) + typechecker (validate + bake) + codegen (call) + interpreter (CPU map).

- **The WGSL is baked in the typechecker, not codegen.** Codegen's `compile_method_call` has no `program` handle, and `gpu_wgsl` imports `ast` — emitting there would break containment. So the typechecker (which validates the kernel anyway) calls `emit_kernel` and stashes the shader in a `gpu_dispatch_wgsl` side table keyed on the kernel-arg span, threaded typechecker → lowering → `Program` → codegen exactly like `stats_elem_types`. Codegen reads the string as a plain-data hint. **This is the containment-preserving pattern for any future AST-shape-dependent codegen hint.**

- **The interpreter gives run==build parity for free.** A `#[gpu]` kernel is an ordinary Kāra function, so `karac run` (no GPU) computes the element-wise map on the CPU by calling the kernel per element. Identical result to the Metal path for `x*2.0` in f32 — the kata/book A/B discipline holds.

- **Buffer ABI:** the input `Vec[f32]`'s `{data,len}` is read via a spill-alloca + scalar `struct_gep` (NOT aggregate-load `extractvalue`, which nulls the pointer field under arm64-Linux ASan — the same trap `src/codegen/stats.rs` documents). The result buffer is `malloc`'d in the runtime (via `karac_alloc_or_panic`, the collection allocator) so the owned `Vec[f32]` frees it with the matching `free`; `len == cap == n` since element-wise maps preserve length.

- **The real 0c cost was the native link line, not the codegen.** wgpu's Metal backend needs macOS system frameworks (`Metal`/`Foundation`/`QuartzCore`/`CoreGraphics` + `-lobjc`) the default compute `cc` line omits — the first link failed on `_sel_getName`. Fixed by appending them **only** when the emitted object references `karac_runtime_gpu_*` (`object_references_gpu`, mirroring the TLS-symbol probe), so non-GPU binaries are byte-for-byte unchanged. The gpu archive is 14 MB (wgpu + naga + objc2) — concrete confirmation of why it cannot go in the default archive.

- **Next-increment pointers:** (1) ✅ **automatic gpu-archive selection landed (2026-07-03)** — `resolve_runtime_path`'s `gpu` arm resolves `libkarac_runtime_gpu.a` when the object references `karac_runtime_gpu_*` (`object_references_gpu` / `symbol_listing_references_gpu`), no `KARAC_RUNTIME` needed; (2) the Linux-Vulkan + wasm-WebGPU link/story arms of the framework block; (3) non-f32 element types (the emitter's `scalar_name`/`require_f32` already factor the scalar mapping); (4) multi-parameter / whole-array / reduction dispatch (needs workgroup memory + multi-buffer binding — the emitter rejects >1 param today); (5) a CI-friendly gated E2E that builds the gpu archive (heavy) or a compile-to-object assertion that the shader constant + `karac_runtime_gpu_f32_map` reference are emitted.

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
