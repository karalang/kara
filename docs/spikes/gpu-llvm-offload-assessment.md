# CG-5 assessment: LLVM Offload / NVPTX as the second GPU backend

**Status:** ASSESSMENT ONLY — no implementation, no decision taken. Prompted by
[*GPU Offload in Rust: Portable, Safe, and Fast*](https://arxiv.org/abs/2608.13759)
(arXiv 2608.13759, 2026-08) and its [Phoronix writeup](https://www.phoronix.com/news/LLVM-Offload-Rust-Performance),
which report a rustc/LLVM-Offload GPU path benchmarking competitive with native
CUDA and HIP on RAJAPerf. Question for us: does that change how we should build
**CG-5**, the one unbuilt item in the GPU story (`--target cuda`, roadmap.md
Phase 10)?

The measured findings below are the point of this document. The options section
prices what they imply; the recommendation is a scoped next spike, not a
commitment.

> **Sourcing caveat.** `phoronix.com`, `arxiv.org`, `news.ycombinator.com` and
> `rust-lang.github.io` are all blocked by this container's egress proxy. The
> paper's claims here come from search-result summaries and its abstract — the
> full text and the benchmark tables were **not** read. Re-verify the numbers
> before quoting them anywhere load-bearing.

## What the paper does (as best established)

A zero-overhead multi-vendor GPU compilation framework built natively into
rustc and the LLVM backends, dispatching through **LLVM's Offload
infrastructure** rather than a vendor DSL. It leans on Rust's type system,
ownership, and strict-aliasing (`noalias`) guarantees to manage and optimize
host↔device transfers, and lets most kernels avoid raw pointers. Evaluated by
porting a subset of RAJAPerf to Rust and comparing against the C++ HIP/CUDA
originals on MI250X / H100 / RTX A2000, measuring kernel time, transfer volume,
total runtime, fast-math impact, and register usage. Reported result:
competitive with native HIP and CUDA, winning some and losing others.

Two things follow for us, and they pull in opposite directions:

- **Supportive:** the "safety tax on GPU code" assumption does not show up in
  their numbers. A safe front-end over LLVM device codegen is performance-viable.
  Our front-end guarantees are *stronger* than the ones they exploit (below).
- **Competitive:** if rustc ships first-class multi-vendor offload, "memory-safe
  GPU compute" stops being a differentiator on its own. Ours has to rest on the
  effect-system guarantees and the diagnostics story, not on safety per se.

## Measured findings (this container, 2026-08-18)

Everything in this section was run, not inferred.

**1. Device codegen needs no new LLVM.** The LLVM we already link has both GPU
backends compiled in:

```
$ llvm-config-18 --targets-built
AArch64 AMDGPU ARM AVR BPF Hexagon Lanai LoongArch Mips MSP430 NVPTX
PowerPC RISCV Sparc SystemZ VE WebAssembly X86 XCore M68k Xtensa
```

`inkwell` 0.9 exposes `Target::initialize_nvptx` and
`Target::initialize_amd_gpu` alongside the `initialize_native` /
`initialize_webassembly` we already call. **Note: no SPIRV** in that list — so
"emit SPIR-V and keep feeding wgpu" is not available from this toolchain.

**2. Both GPU backends emit real device code through our stock toolchain.**
`examples/nvptx_probe.rs` (added by this assessment; run
`cargo run --release --features llvm --example nvptx_probe [amdgcn]`) builds the
slice-0 kernel — `out[i] = in[i] * 2.0` over the thread index — by hand through
inkwell and emits assembly. NVPTX, `sm_70`:

```ptx
.visible .entry double_kernel(
	.param .u64 double_kernel_param_0,
	.param .u64 double_kernel_param_1
)
{
	mov.u32 	%r1, %tid.x;
	mul.wide.s32 	%rd3, %r1, 4;
	ld.global.f32 	%f1, [%rd4];
	add.rn.f32 	%f2, %f1, %f1;
	st.global.f32 	[%rd5], %f2;
	ret;
}
```

A proper `.visible .entry` kernel (the `nvvm.annotations` metadata took), with
the GPU optimizer live — it strength-reduced `x * 2.0` into `add.rn.f32
%f1, %f1`. The `amdgcn` arm emits a complete gfx90a HSA code object with kernel
metadata (kernarg layout, `.wavefront_size: 64`, sgpr/vgpr counts). **So the
device-codegen half of CG-5 is not the expensive half.**

**3. The runtime boundary is seven symbols, and all of them take WGSL *text*.**
`runtime/src/gpu.rs` exports `karac_runtime_gpu_{map, map_multi, dispatch_soa,
upload_soa, dispatch_resident, download_soa, free_soa}`. Every dispatching one
begins `(wgsl_ptr: *const u8, wgsl_len: usize, …)`. The pipeline is:

```
#[gpu] fn (AST)
  → typechecker: GpuSafe validation + gpu_wgsl::emit_kernel → WGSL *string*
  → TypeCheckResult.gpu_dispatch_wgsl (SpanKey → String)
  → lowering → Program.gpu_dispatch_wgsl
  → codegen bakes the string as a constant, calls karac_runtime_gpu_*
  → runtime: wgpu/naga compiles the WGSL *at run time*, dispatches
```

That is a fundamentally different model from PTX/HSA, where device images are
**precompiled and embedded in the host binary**, then registered at load. A
second backend therefore cannot reuse these seven symbols — it is a sibling
runtime path, not a swap.

**4. The shipped WGSL kernel subset has no loops and no `match`.**
`src/gpu_wgsl.rs` (2,329 lines) handles exactly fourteen `ExprKind`s: Binary,
Block, Call, Cast, FieldAccess, Float, Identifier, If, Index, Integer,
MethodCall, Path, Return, StructLiteral, Unary. Zero occurrences of
`ExprKind::While`, `For`, `Loop`, or `Match`. Kernels today are straight-line
code plus `if`. This is a real limitation of the *shipped* path that the
slice-0 spike doc does not call out, and it matters to the option analysis
below.

**5. No offload runtime, no CUDA, no GPU in this container.** `/usr/lib/llvm-18/lib`
carries only `libLLVMFrontendOffloading.a` — the *compiler-side* helper for
building offload entries — not `libomptarget`/`liboffload`, which ships
separately. No `nvcc`, no `nvidia-smi`, no `/usr/local/cuda`. **Any CG-5 work
done here can be validated only up to "emits valid PTX/HSA".** Execution
validation needs real hardware; note the owner's Mac is Metal-only, so a CUDA
box or CI runner is a prerequisite for closing CG-5 for real.

## The architectural read

The decision splits into two *independent* axes, and conflating them is the
main way to get this wrong:

- **(A) How device code is produced** — WGSL text (today) vs LLVM IR → PTX/HSA.
- **(B) How a kernel is launched** — wgpu (today) vs a thin CUDA Driver API
  shim vs the LLVM Offload runtime.

The paper's contribution is mostly **(B)** plus rustc integration. For us the
interesting leverage is **(A)**, for a reason specific to Kāra:

**Our front-end already establishes the preconditions that make LLVM-IR kernel
lowering tractable.** The `#[gpu]` effect gate
(`src/effectchecker/gpu_effect_gate.rs`) rejects `allocates(Heap)`, `sends` /
`receives`, host reads/writes, and explicit panics across the *entire
transitive call graph* from every `#[gpu]` root; `GpuSafe`
(`src/typechecker/gpu_safe.rs`) rejects `String`, `shared` RC handles, and every
other heap-bearing type in kernel signatures and bindings. So a kernel body is
*statically guaranteed* to be arithmetic and control flow over flat scalars and
structs — no allocator calls, no panic landing pads, no runtime symbols to
resolve on the device. That is exactly the property that otherwise makes
"lower a general-purpose language to a GPU target" hard, and we get it from
checks that already ship.

The paper reaches for `noalias` to earn transfer optimizations. We have
ownership *and* declared effects — a strictly richer source of the same
information, and one already consumed by the layout/SoA machinery
(`dispatch_soa`, the resident-buffer path in
[`gpu-slip-4b-resident-buffers.md`](gpu-slip-4b-resident-buffers.md)).

**The unobvious payoff of axis (A):** an LLVM-IR kernel path would inherit
codegen's *full* expression coverage — loops, `match`, the lot — instead of
requiring each construct to be re-implemented in the WGSL text emitter (finding
4). Over time that makes it the more capable kernel backend even on hardware
where wgpu works fine. WGSL remains mandatory for the browser/WebGPU target
regardless, so this is "second backend", never "replacement".

## Options

**Option 0 — status quo.** wgpu/WGSL only; CG-5 stays unbuilt. Cost: zero.
Accepts: no NVIDIA-specific performance path, no CUDA library interop
(cuBLAS/cuDNN), kernels stay straight-line + `if` until someone extends the
WGSL emitter construct by construct, and — per the phase-10 tracker's CG-5
entry — **no f64 on the GPU at all**, since WGSL has no `f64` and NVPTX is our
only f64-capable device target. That last one is a motivation for CG-5 that
predates this paper and is independent of it; GPU-LBM-1 resolved to f32, so it
is not currently on a critical path, but it is the constraint most likely to
force the issue later.

**Option 1 — NVPTX + a thin CUDA Driver API shim** (what roadmap.md currently
specs). Device side is now *proven* cheap (finding 2). Host side is a contained
runtime shim — `cuInit` / `cuModuleLoadData` / `cuLaunchKernel` / async memcpy —
behind a new opt-in archive, structurally identical to the wgpu archive we
already auto-select on `karac_runtime_gpu_*` (`src/codegen/driver.rs`). Adds a
CUDA-toolkit-or-driver dependency for users of that target only, which the
roadmap already anticipates (`E0793: CUDA toolkit not found`). NVIDIA only.

**Option 2 — LLVM Offload (libomptarget).** One integration buys the host
runtime for **both** NVIDIA and AMD, which Option 1 does not. Costs: a runtime
component we must ship or require (not present by default, cf. finding 5); an
embedding format — offload entries / fat binary bundles — we would have to match
exactly; and an ABI that is OpenMP-oriented and still under active churn, which
is a poor fit for a language that pins its toolchain. It also concentrates the
"portable GPU" promise in a dependency we do not control, where today wgpu
already gives us Metal + Vulkan + DX12 + WebGPU portability.

**Option 3 — decouple (A) from (B): LLVM-IR kernels, existing wgpu launch.**
Not available. wgpu consumes WGSL or SPIR-V, and finding 1 shows this LLVM has
no SPIR-V backend.

## Recommendation

**Do not pick a host runtime yet.** The question that gates every option — and
that no amount of reading settles — is whether `codegen` can lower a *real Kāra
`#[gpu]` kernel body* to NVPTX, or whether the GPU address-space / calling-
convention constraints force a separate lowering path. That single result
decides whether Options 1 and 2 are contained slices or a second backend build,
and it is independent of which launch API wins.

So the proposed next step is a **scoped spike, ~1 session**: take the existing
slice-0 kernel through `codegen`'s real expression lowering into an
`nvptx64-nvidia-cuda` module — reusing `create_wasm_target_machine`'s
second-target pattern, which already proves multi-target codegen works here —
and report (a) whether it verifies and emits, (b) what had to change
(address spaces on pointer params, kernel calling convention, the `tid` builtin,
`memcpy`/intrinsic lowering), and (c) whether loops and `match` come through for
free. **Ends at emitted PTX** — this container cannot execute it (finding 5).

Only after that does the Option 1 vs 2 choice have a real cost attached. My
current lean, stated as a lean and not a finding: **Option 1**, because it is
the smaller dependency, matches the archive/opt-in pattern we already run, and
keeps portability where it already works (wgpu) rather than re-buying it from
libomptarget. Option 2 becomes interesting if AMD compute becomes a first-class
requirement.

## Open questions for the owner

1. **Is CG-5 actually wanted at v1?** The wgpu path already runs on NVIDIA
   hardware through Vulkan. CUDA buys vendor-library interop and last-mile
   performance, not basic NVIDIA support. If neither is a v1 requirement,
   Option 0 is defensible and this whole thread can wait.
2. **Bucket-3 tension.** roadmap.md classifies residual GPU codegen as "built
   once, directly in Kāra" after self-hosting; the wgpu path was pulled into
   Rust by explicit owner directive (2026-07-10). An LLVM-IR kernel path is
   *more* backend-coupled than the WGSL emitter (which is plain string
   generation and ports cleanly), so it is the harder thing to port later. That
   argues either for doing it in Rust deliberately with eyes open, or for
   leaving it to the self-hosted compiler.
3. **Where would it be validated?** Already recorded as a hard blocker on the
   tracker's CG-5/CG-7 prereq line — a discrete-NVIDIA Linux host, which the
   M5 Pro cannot provide and this container demonstrably cannot either
   (finding 5). Nothing in this assessment changes that; the proposed spike is
   deliberately scoped to end at emitted PTX so it can run *before* the
   hardware exists, and de-risk the backend question while CG-7 waits on the
   same machine.

## Artifacts

- `examples/nvptx_probe.rs` — the finding-2 probe, kept as executable evidence
  that our stock LLVM emits both PTX and AMDGPU code objects. Registered with
  `required-features = ["llvm"]`.
