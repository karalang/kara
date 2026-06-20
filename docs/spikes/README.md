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
| [per-layout-monomorphization.md](per-layout-monomorphization.md) | ⬜ **SCOPED 2026-06-20 — ADR, not started.** Make SoA `layout` a monomorphization axis so it crosses call boundaries (returns + differing-name/multi-buffer kernels). Layered on the generic-mono engine: keep `layout <name>` as the layout *origin*, add layout-flow inference (forward arg layouts, backward return layout from the receiving binding) + per-layout monomorph keying/mangling. Supersedes the name-keyed cross-fn slices. Tracks B-2026-06-19-14, design.md Feature 1 / P1.5 (Phase 11). 6-slice plan, each gated on the codegen suite + LSan. |

## 🟡 Partial

| spike | status |
|---|---|
| [general-owned-temp-tracking.md](general-owned-temp-tracking.md) | Slices 1–3 + 5 landed (2026-06-06/07). **Slice 4** (scrutinee sub-frame leak) open — deferred to [phase-12](../implementation_checklist/phase-12-self-hosting.md); memory pressure, not corruption. |
| [ci-test-coverage.md](ci-test-coverage.md) | **Tier 1 landed + required**, **Tier 2 landed non-required** (2026-06-12) — `--features llvm` codegen E2E + oracle + wasm gates required; the Linux-LSan memory-sanitizer job found 11 real leaks on run 1 (→13, all fixed — leak gate CLOSED, CI-verified; durable record = `tests/memory_sanitizer.rs` + phase-12 #14–#22). **Tier 3** (wasm E2E, needs node + wasm-tools) open. |

## ✅ Done

| spike | status |
|---|---|
| [network-async-coroutine-transform.md](network-async-coroutine-transform.md) | **Decided — A2 chosen and shipped** (2026-05-30). Network-async coroutine transform landed, default-on; slices 2b→5 + 5c mechanism done. |
| [oversized-enum-payload.md](oversized-enum-payload.md) | **Boxing landed** (2026-06-07). Box-oversized representation for payloads wider than the seeded enum area; pack/unpack both shipped. |
| [pattern-arm-unbound-field-drop.md](pattern-arm-unbound-field-drop.md) | **Done** for all four match constructs (if-let / match / let-else / while-let), 2026-06-07; all three follow-ups resolved. |
| [self-hosting-llvm-c-ffi.md](self-hosting-llvm-c-ffi.md) | ✅ Resolved (2026-06-11). LLVM-C binding approach validated end-to-end; minimal proof runs green (`exit=42`). Unblocks the codegen leg of Phase 12. |
| [self-hosting-llvm-c-proof.md](self-hosting-llvm-c-proof.md) | ✅ Runs green — `exit=42` (2026-06-11). A Kāra program drives `libLLVM-18` to build/verify/emit a working Mach-O object under the stage-0 Rust `karac`. |
| [self-hosting-llvm-c-surface.md](self-hosting-llvm-c-surface.md) | Done — initial pass (2026-06-10). Resolves sub-question 1 (surface scope) of the FFI spike. |
| [independence-noalias-ilp.md](independence-noalias-ilp.md) | ✅ **RAN — resolved 2026-06-12. Tier-0 *aliasing* = v1.x, not P0.** The autovec enabler was non-trapping arithmetic (`wrapping_*`, shipped), not alias info; the aliasing half measured ≈0 (Kāra at Rust parity, memory-bound). Alias-scope metadata filed deferred in `roadmap.md`; real-world-lever follow-on → [selfhost-lexer-profile.md](selfhost-lexer-profile.md). |
| [selfhost-lexer-profile.md](selfhost-lexer-profile.md) | ✅ **RAN — resolved 2026-06-12.** Profiled the self-hosted lexer on 441 KB of real Kāra. **#1 hotspot = string-literal `match` dispatch lowered to a linear `memcmp` chain (46% self-time)** — a surprise; allocation was a strong #2 (38%). Self-host lexer = **4.6× the Rust lexer's instruction count** (token output bit-identical). Filed two real codegen levers (string-match dispatch + allocation reduction) at the top of `roadmap.md` § Codegen Optimization; **confirmed the three SIMD-class levers (alias-scope / NT-stores / fusion) are *not* the real-world answer** and stay deferred. Surfaced `B-2026-06-12-9` (`?`-in-`main` miscompile) + `B-2026-06-12-10` (suspected leak). |
