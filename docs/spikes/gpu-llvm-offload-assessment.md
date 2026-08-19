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

> **Updated 2026-08-18 (later, same day).** Added **finding 6** and **Option 4**
> — whether a SPIRV-enabled LLVM could feed the wgpu launcher we already ship,
> keeping Metal and the browser. Measured answer: no, the two ends speak
> different SPIR-V dialects. Added **finding 7**, which counts how much of
> `gpu_wgsl.rs` a second backend could actually share (near half, not most) and
> concludes AGAINST a shared kernel IR. Marked **finding 4 historical**: the
> single-expression kernel-body floor it documents was lifted the same day. The
> architectural read now states plainly that none of this is a one-way door —
> the backends coexist, and the binding is per artifact.
>
> **Updated 2026-08-19: the recommended spike RAN.** Findings 8 and 9. A real
> Kāra kernel body reaches NVPTX unchanged (6/6 shapes, loops and `match`
> included), so the question gating every host-runtime option is answered
> yes. One new cost surfaced: the i64-carrier integer model, harmless on a
> CPU, would cost occupancy on a device.

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
(SPIRV *can* be enabled in a custom LLVM build, which looks like the way out of
this — finding 6 measures why it is not.)

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

**4. The shipped kernel body must be a single expression with NO `let`
bindings.** `src/gpu_wgsl.rs` (2,329 lines) handles exactly fourteen
`ExprKind`s — Binary, Block, Call, Cast, FieldAccess, Float, Identifier, If,
Index, Integer, MethodCall, Path, Return, StructLiteral, Unary — with zero
occurrences of `ExprKind::While`, `For`, `Loop`, or `Match`, and a
single-expression body contract on top. Probed with the release `karac check`,
one construct at a time:

| kernel body | result |
|---|---|
| `x * 2.0` | accepted |
| `if x > 0.0 { x } else { -x }` | accepted |
| `x.sqrt()` | accepted |
| `let y: f32 = x * 2.0; y + 1.0` | **rejected** |
| `while` loop with an accumulator | **rejected** |

Both rejections carry the same clean, correctly-spanned diagnostic
(`error[E_GPU_DISPATCH_KERNEL]: … a slice-0 GPU kernel body must be a single
expression (no locals)`), so this is a capability ceiling reported honestly,
not a diagnostics defect. **Correction:** an earlier draft of this finding said
"no loops or `match`" and understated it — the real limit also forbids `let`,
so every intermediate must be hand-inlined. Slice-0 documented its *signature*
contract (`fn k(x:T)->U` over `[T]`→`[U]`) but not this *body* restriction.
Filed as **B-2026-08-18-40**.

> **Finding 4 is now HISTORICAL (superseded 2026-08-18, same day).** The
> single-expression floor was lifted by B-2026-08-18-40 (locals, `while` +
> mutable locals + assignment, `for`-over-range, value `match`) and
> B-2026-08-18-49 (statement-form `if`, and value-`if` branches carrying
> locals). A scalar kernel is now a statement sequence, so the accumulator
> shape every reduction needs compiles and runs. What finding 4 says about the
> *mechanism* still stands and is the point that matters here: each construct
> costs a hand-written arm in a text emitter. Six increments in one day is
> evidence both that the emitter is tractable to extend AND that extending it
> is the recurring cost an IR-level backend would retire.

**5. No offload runtime, no CUDA, no GPU in this container.** `/usr/lib/llvm-18/lib`
carries only `libLLVMFrontendOffloading.a` — the *compiler-side* helper for
building offload entries — not `libomptarget`/`liboffload`, which ships
separately. No `nvcc`, no `nvidia-smi`, no `/usr/local/cuda`. **Any CG-5 work
done here can be validated only up to "emits valid PTX/HSA".** Execution
validation needs real hardware; note the owner's Mac is Metal-only, so a CUDA
box or CI runner is a prerequisite for closing CG-5 for real.

**6. LLVM's SPIR-V and wgpu's SPIR-V are DIFFERENT DIALECTS — the obvious
escape hatch does not exist.** The tempting fifth option is "build LLVM with
its SPIR-V backend, feed the wgpu launcher we already ship, keep Metal and the
browser." Both halves were checked, and they do not meet:

- *wgpu side, verified:* naga 29.0.3 does have a SPIR-V **frontend**
  (`naga/src/front/spv/`), reachable via `wgpu-core`'s `spirv = ["naga/spv-in"]`
  feature. So consuming SPIR-V is a feature flag away — this half is fine.
- *The dialect, verified:* that frontend accepts exactly five execution models
  — `Vertex`, `Fragment`, `GLCompute`, `TaskEXT`, `MeshEXT` — and returns
  `Error::UnsupportedExecutionModel` for everything else
  (`front/spv/mod.rs:1934-1939`, the `_ =>` arm). It also assumes the
  **Logical** memory model.
- *LLVM side:* the SPIRV backend is an OpenCL/compute target — `Kernel`
  execution model, `Physical32/Physical64` addressing. `Kernel` is precisely
  what that `_ =>` arm rejects.

So a hypothetical SPIRV-enabled LLVM would emit SPIR-V that naga refuses to
parse, and the cost of getting there (a custom pinned LLVM build, since SPIRV
is opt-in at LLVM build time and not in our `--targets-built`) buys nothing.
Bridging the dialects means an OpenCL-flavor→Vulkan-flavor translation layer —
addressing model, capabilities, entry-point decoration — which is a compiler
project of its own, not a build flag. Recorded because this option *reads* as
the obvious best of both worlds and is the one a reviewer will reach for first.

**7. A second kernel backend would share LESS of `gpu_wgsl.rs` than its shape
suggests — which is an argument FOR reusing `codegen`, not for a shared kernel
IR.** Counted over the 2,335 production lines of `src/gpu_wgsl.rs` (1,319
further lines are tests), splitting each top-level item by whether it produces
WGSL *text*:

| | lines |
|---|---|
| backend-**agnostic** (AST→`KStmt` walker, `Scope` rename/shadow machinery, `KStmt` model, helper call-graph walk, validation) | **618** |
| backend-**specific** (three emitters + their three expression lowerers, helper/struct printing) | **657** |

Near parity — so "write the hard part once, print it twice" does **not** hold
for the file as a whole.

It *does* hold for the scalar **statement** layer that B-2026-08-18-40/-49 grew:
406 agnostic lines against `emit_stmts`'s 119, a 3.4× ratio. That is the layer
where a shared IR would pay, and it is genuinely IR-shaped — `KStmt` is now
`Bind` / `Assign` / `While` / `ForRange` / `If` / `DeclareVar`, with every hard
part (the `i`→`i_k` rename that keeps `input[i]` correct, per-branch scoping,
the value-vs-statement fork, the diagnostics) on the agnostic side.

But the *expression* layer defeats it: the scalar, SoA and stencil emitters each
carry their own AST→WGSL-text lowering (`lower_expr` 79, `lower_soa_expr` 95,
`lower_stencil_expr` 119), and none of that is reusable by an LLVM backend.

**The conclusion runs opposite to the intuition.** A shared kernel IR feeding
both a WGSL printer and an LLVM builder would retire ~400 lines of duplication
and forfeit the "free coverage" argument — because an NVPTX path that goes
through `KStmt` no longer inherits `codegen`'s expression lowering, and `KStmt`
models strictly less of the language than `codegen` already handles. Reusing
`codegen` wholesale keeps the duplication but makes the second backend nearly
free *and* more capable than the first. **Prefer reuse over a shared IR**; the
duplication is asymmetric in our favour, since the bespoke emitter (WGSL) is the
one already written and the cheap one (LLVM) is the one still to come.

*Correction:* an earlier informal reading of this quoted the 3.4× statement-layer
ratio as if it described the file, concluding a second backend would cost "the
119-line half." Counting the whole file gives near parity. The narrow number was
real but not generalisable, and the generalisation inverted the recommendation.

**8. A REAL Kāra kernel body lowers to NVPTX unchanged — 6/6 shapes reached
PTX.** This is the question the recommendation below was written to answer, and
it needed no GPU. `examples/nvptx_kernel_spike.rs` compiles a `#[gpu]` kernel
through `codegen`'s ordinary lowering, lifts the emitted function out of the
module, links it into a fresh `nvptx64-nvidia-cuda` module under a hand-written
kernel wrapper (thread index, bounds check, load, call, store) plus
`nvvm.annotations`, verifies, and emits assembly:

| kernel shape | result |
|---|---|
| scalar map, locals, `for`-range accumulator, statement `if`, value `match`, wrapping accumulator | **all 6 verify and emit `.visible .entry`** |

**Control flow came through for free, as predicted.** `for n in 0..4 { acc = acc + x; }`
emits a genuine counted loop — condition, exit branch, `add.rn.f32`, counter
increment, back-edge:

```ptx
$L__BB0_1:
	setp.gt.s64 	%p1, %rd4, 3;
	@%p1 bra 	$L__BB0_3;
	add.rn.f32 	%f5, %f5, %f3;
	add.s64 	%rd4, %rd4, 1;
	bra.uni 	$L__BB0_1;
```

and a value `match` emits a real `setp`/`bra` chain. **Zero changes were needed
to the body lowering** — the transplant is the evidence: the function codegen
emitted for the native target dropped into an NVPTX module and compiled as-is.
Everything that did need writing sits in the WRAPPER, which is the part a real
backend generates anyway (address space 1 on the pointer params, the
`llvm.nvvm.read.ptx.sreg.*` intrinsics, the `nvvm.annotations` entry marker).

**Why this worked is not luck.** Finding 7's re-run shows every kernel shape
that is LEGAL after B-2026-08-19-1 lowers to device-clean IR — no `karac_*`
runtime calls, no unwind edges. The three shapes that still emit
`__karac_panic_site_*` (bare integer `+`, bare `/`, a `while` counter using bare
`+`) are exactly the three that fix made illegal. So the set of writable kernels
and the set of NVPTX-lowerable kernels now coincide **by construction** — the
effect gate proves no allocation/channels/host-I/O/explicit-panics, and the
trapping-arithmetic rule removed the last implicit panic sites.

**Method note:** the spike deliberately did NOT build a second `Codegen` against
an NVPTX target machine, which would mean threading a target through a large
stateful struct. Transplanting the emitted function is both cheaper and a
stronger result: it demonstrates the body lowering is *target-independent*
rather than merely *re-targetable*.

**9. The i64-carrier model would cost real performance on a device.** An `i32`
kernel's PTX uses 64-bit registers and ops throughout — `%rd`, `add.s64`,
`setp.gt.s64` — because codegen normalizes narrow integers to an i64 carrier
(finding 7's `compile_narrow_int_binop` model). That is invisible on a CPU,
where 64-bit ALU ops cost the same as 32-bit. It is **not** invisible on a GPU,
where 64-bit integer arithmetic is materially slower than 32-bit on every NVIDIA
generation, and doubles register pressure — which directly reduces occupancy.
Not a blocker and not a correctness issue, but any real NVPTX path wants narrow
integers carried at their declared width, and that is a change in `codegen`'s
integer model rather than in the GPU backend. Worth pricing before CG-5 is
scheduled, since discovering it after the backend exists would be the expensive
order.

## The architectural read

The decision splits into two *independent* axes, and conflating them is the
main way to get this wrong:

- **(A) How device code is produced** — WGSL text (today) vs LLVM IR → PTX/HSA.
- **(B) How a kernel is launched** — wgpu (today) vs a thin CUDA Driver API
  shim vs the LLVM Offload runtime.

**This is not a one-way door, and the options below should not be read as
"pick a lane."** The choice binds *per artifact*, not per language:

- **At the product level there is no exclusivity.** `karac` can ship both — WGSL
  by default, PTX under `--target cuda` — the same way it already ships native,
  wasm and wasm-threads backends side by side. CG-5 is specified as a *second*
  backend, never a replacement, and WGSL stays mandatory for the browser
  regardless of what else lands.
- **What genuinely is forced is per-target.** One artifact cannot span both:
  there is no PTX in a browser, and no `f64` in WGSL. So the binding happens at
  build time for a chosen device, which is a far weaker constraint than choosing
  once for the language.
- **The real recurring cost is therefore not "which backend" but "two
  emitters."** Finding 7 measures that cost and concludes it is worth paying —
  the second backend is the cheap one, provided it reuses `codegen` rather than
  routing through a shared kernel IR.

The one thing genuinely unavailable is a *single* artifact serving both worlds,
which is what Option 4's SPIR-V bridge would have bought (finding 6).

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
4). **That cost is no longer hypothetical: it was paid on 2026-08-18.** Locals,
`while` + mutable locals, `for`-over-range, value `match`, statement-form `if`
and value-`if`-with-locals took six increments across B-2026-08-18-40 and
-49, each a hand-written arm plus its own tests. The work went fine — the point
is that an IR-level backend would have needed none of it, and the next construct
(nested `if` inside a larger expression, still unsupported) is another such
increment. Over time that makes it the more capable kernel backend even on
hardware where wgpu works fine. WGSL remains mandatory for the browser/WebGPU target
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
no SPIR-V backend. See Option 4 for why *adding* one does not rescue this.

**Option 4 — build LLVM with its SPIR-V backend, keep the wgpu launcher.**
The version of Option 3 that a reviewer reaches for once finding 1 lands, and
the only option that would keep **Metal and the browser** while retiring the
hand-written text emitter. It is the most attractive option on paper and it
does not work, for a reason that is about dialects rather than effort
(finding 6):

- Getting there costs a **custom pinned LLVM build** — SPIRV is opt-in at LLVM
  build time and absent from our `--targets-built`. Bad but survivable for a
  language that already pins its toolchain.
- The fatal part is what comes out. LLVM's SPIRV backend is an OpenCL/compute
  target (`Kernel` execution model, physical addressing). naga's SPIR-V
  frontend accepts `Vertex`/`Fragment`/`GLCompute`/`TaskEXT`/`MeshEXT` and
  returns `UnsupportedExecutionModel` for anything else, assuming the Logical
  memory model. The two ends speak different dialects of the same format.

Rescuing it means writing an OpenCL-flavor→Vulkan-flavor translator
(addressing model, capabilities, entry-point decoration) — a compiler project
in its own right, and one whose output would then be re-translated by naga into
MSL/HLSL/SPIR-V per backend. **Not recommended**, and recorded at length
precisely because it reads like the obvious answer: anyone re-deriving this
should find the measurement rather than repeat the reasoning.

*If this option is ever revisited*, the thing to re-check first is whether
naga's frontend has gained `Kernel`/physical-addressing support (it had not as
of naga 29.0.3) — that single fact is what makes the difference between a build
flag and a compiler project.

## Recommendation

**Do not pick a host runtime yet.** The question that gates every option — and
that no amount of reading settles — is whether `codegen` can lower a *real Kāra
`#[gpu]` kernel body* to NVPTX, or whether the GPU address-space / calling-
convention constraints force a separate lowering path. That single result
decides whether Options 1 and 2 are contained slices or a second backend build,
and it is independent of which launch API wins.

> **THE SPIKE HAS RUN (2026-08-19) — see finding 8. Answer: yes, cleanly, with
> no body changes at all.** The paragraph below is the original proposal, kept
> for the record.

So the proposed next step is a **scoped spike, ~1 session**: take the existing
slice-0 kernel through `codegen`'s real expression lowering into an
`nvptx64-nvidia-cuda` module — reusing `create_wasm_target_machine`'s
second-target pattern, which already proves multi-target codegen works here —
and report (a) whether it verifies and emits, (b) what had to change
(address spaces on pointer params, kernel calling convention, the `tid` builtin,
`memcpy`/intrinsic lowering), and (c) whether loops and `match` come through for
free — a question that now has a **price tag on the other side of the ledger**, since the WGSL emitter got them by hand in six increments on 2026-08-18. **Ends at emitted PTX** — this container cannot execute it (finding 5).

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
