# Design spikes — index

At-a-glance status for every spike in this directory. **The source of truth is each
file's own `**Status:**` line** — this index just surfaces it. When a spike's status
changes, update its `Status:` line first, then the one row here.

**Filenames are stable IDs.** Spikes are cross-referenced by ~70 inbound links from
`design.md`, the phase trackers, and source comments in `src/` — do **not** rename a
spike file or move it into a subdirectory to signal "done." Renaming breaks those links
(the ones in `.rs` comments rot silently). Status is tracked here and in the file, not
in the path.

Legend: ✅ done · 🟡 partial · ⬜ open

## ⬜ Open

| spike | status |
|---|---|
| [windows-iocp-eventloop.md](windows-iocp-eventloop.md) | Groundwork / scoping done on macOS (2026-06-07). Runtime impl + validation need a Windows box — tracks phase-6 Slice 10 (IOCP integration) + cancel-sweep. |
| [small-string-optimization.md](small-string-optimization.md) | 🟡 **SCOPED 2026-06-12 — campaign teed up, not started.** The corpus-wide allocation lever: inline short strings in the `{ptr,len,cap}` struct → no `malloc` for short lexemes (`malloc` is the #1 self-time leaf after the string-match dispatch lever shipped, Rust gap now 2.74×). Central constraint: String shares `vec_struct_type` with Vec → in-struct tag, don't split the type. Highest blast radius in the String subsystem; staged for a **fresh dedicated session** (layout → accessors → inline construction → runtime/FFI decode). Full cold-start handoff in the spike. |

## 🟡 Partial

| spike | status |
|---|---|
| [reduce-elementwise-trait-unification.md](reduce-elementwise-trait-unification.md) | 🟡 **S0–S1a LANDED 2026-06-30 (`bcaff37d`, `73af27b0`); S1b/c + S2–S6 open.** Unifies the three copy-pasted reduce/element-wise/ordering impls (Tensor, Column, `Stats.*`) behind one internal `ContainerAccess`/`ElemKind`/`ReduceOp`-parameterized kernel (S0–S5), then layers **user-extensible** `Reduce`/`ElementwiseMap`/`ElementwiseOrd` surface traits (S6, gated on a default-method-body + generic-trait-method spike). S0 = shared `ReduceOp` vocab + interpreter twin (`src/reduce_kernel.rs`); S1a = codegen `ContainerAccess`/`emit_reduce_fold` (`src/codegen/kernel.rs`) with Stats+Tensor `sum`/`prod`/`mean` migrated. Byte-identical (oracle 1935/0, par 127/0). Closes the two open `std.stats` long-tail items (non-f64 elems + trait unification) and lands Column `median`/`quantile` codegen as S4 fallout. S0–S5 is the committed spine; S6 may split off. |
| [gpu-wgsl-slice0.md](gpu-wgsl-slice0.md) | 🟡 **slice-0a PROVEN on Metal (2026-06-29).** Runtime wgpu spine works end-to-end — a hand-written WGSL `x*2.0` shader doubles an `f32` buffer through `karac-runtime` (`--features gpu`, `runtime/src/gpu.rs`). `wgpu 29.0.3` behind an opt-in `gpu` feature; production/wasm archives untouched. Remaining: **0b** (WGSL codegen from the `#[gpu]` AST) + **0c** (wire `gpu.dispatch`) — gated on explicit go. Maps to CG-1/2/3 + SL-2; standing build-in-self-hosted tension still applies. |
| [general-owned-temp-tracking.md](general-owned-temp-tracking.md) | Slices 1–3 + 5 landed (2026-06-06/07). **Slice 4** (scrutinee sub-frame leak) open — deferred to [phase-12](../implementation_checklist/phase-12-self-hosting.md); memory pressure, not corruption. |
| [ci-test-coverage.md](ci-test-coverage.md) | **Tier 1 landed + required**, **Tier 2 landed non-required** (2026-06-12) — `--features llvm` codegen E2E + oracle + wasm gates required; the Linux-LSan memory-sanitizer job found 11 real leaks on run 1 (→13, all fixed — leak gate CLOSED, CI-verified; durable record = `tests/memory_sanitizer.rs` + phase-12 #14–#22). **Tier 3** (wasm E2E, needs node + wasm-tools) open. |

## ✅ Done

| spike | status |
|---|---|
| [per-layout-monomorphization.md](per-layout-monomorphization.md) | 🟩 **COMPLETE 2026-06-20 — slices 1–6 landed (axis scaffolding + forward arg-layout mono + SoA returns + multi-buffer `ref`/`mut ref` borrow forms + origin-only `soa_layouts` cutover + the Slipstream full-SoA proof).** SoA `layout` is a monomorphization axis that crosses call boundaries: by-value AND by-ref SoA `Vec[E]` cross regardless of param name, a builds-and-returns helper is monomorphized to return the receiving binding's layout, multiple SoA buffers of one type flow through shared by-ref helpers as distinct monomorphs, and a binding's physical layout is a per-binding `LayoutId` value carrier (not a name-keyed `soa_layouts` lookup). **Slice 6** converts `examples/slipstream/src/sim.kara`'s carried LBM grid to a `layout` block — the native oracle's milestone checksums are byte-identical AoS↔SoA and the browser flagship runs on SoA in real headless Chrome — and fixed five more cross-function gaps it surfaced (`with_capacity` SoA constructor, returned-local base-symbol clash, SoA reassignment `grid = substep(grid)`, tail-CALL SoA-return propagation, and SoA carried across a coroutine suspend). Tracks B-2026-06-19-14 (now `fixed`), design.md Feature 1 / P1.5 (Phase 11). Follow-ons (separate features): whole-element SoA index-store `grid[i] = E{..}`; branch-leaf/multi-return SoA returns. |
| [network-async-coroutine-transform.md](network-async-coroutine-transform.md) | **Decided — A2 chosen and shipped** (2026-05-30). Network-async coroutine transform landed, default-on; slices 2b→5 + 5c mechanism done. |
| [oversized-enum-payload.md](oversized-enum-payload.md) | **Boxing landed** (2026-06-07). Box-oversized representation for payloads wider than the seeded enum area; pack/unpack both shipped. |
| [pattern-arm-unbound-field-drop.md](pattern-arm-unbound-field-drop.md) | **Done** for all four match constructs (if-let / match / let-else / while-let), 2026-06-07; all three follow-ups resolved. |
| [self-hosting-llvm-c-ffi.md](self-hosting-llvm-c-ffi.md) | ✅ Resolved (2026-06-11). LLVM-C binding approach validated end-to-end; minimal proof runs green (`exit=42`). Unblocks the codegen leg of Phase 12. |
| [self-hosting-llvm-c-proof.md](self-hosting-llvm-c-proof.md) | ✅ Runs green — `exit=42` (2026-06-11). A Kāra program drives `libLLVM-18` to build/verify/emit a working Mach-O object under the stage-0 Rust `karac`. |
| [self-hosting-llvm-c-surface.md](self-hosting-llvm-c-surface.md) | Done — initial pass (2026-06-10). Resolves sub-question 1 (surface scope) of the FFI spike. |
| [independence-noalias-ilp.md](independence-noalias-ilp.md) | ✅ **RAN — resolved 2026-06-12. Tier-0 *aliasing* = v1.x, not P0.** The autovec enabler was non-trapping arithmetic (`wrapping_*`, shipped), not alias info; the aliasing half measured ≈0 (Kāra at Rust parity, memory-bound). Alias-scope metadata filed deferred in `roadmap.md`; real-world-lever follow-on → [selfhost-lexer-profile.md](selfhost-lexer-profile.md). |
| [selfhost-lexer-profile.md](selfhost-lexer-profile.md) | ✅ **RAN — resolved 2026-06-12.** Profiled the self-hosted lexer on 441 KB of real Kāra. **#1 hotspot = string-literal `match` dispatch lowered to a linear `memcmp` chain (46% self-time)** — a surprise; allocation was a strong #2 (38%). Self-host lexer = **4.6× the Rust lexer's instruction count** (token output bit-identical). Filed two real codegen levers (string-match dispatch + allocation reduction) at the top of `roadmap.md` § Codegen Optimization; **confirmed the three SIMD-class levers (alias-scope / NT-stores / fusion) are *not* the real-world answer** and stay deferred. Surfaced `B-2026-06-12-9` (`?`-in-`main` miscompile) + `B-2026-06-12-10` (suspected leak). |
