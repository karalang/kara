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
| [independence-noalias-ilp.md](independence-noalias-ilp.md) | Scoped, **not yet run** (filed 2026-06-09). Measure-first gate deciding whether Tier-0 noalias→autovectorization is P0 (launch headline) or v1.x. |
| [windows-iocp-eventloop.md](windows-iocp-eventloop.md) | Groundwork / scoping done on macOS (2026-06-07). Runtime impl + validation need a Windows box — tracks phase-6 Slice 10 (IOCP integration) + cancel-sweep. |

## 🟡 Partial

| spike | status |
|---|---|
| [general-owned-temp-tracking.md](general-owned-temp-tracking.md) | Slices 1–3 + 5 landed (2026-06-06/07). **Slice 4** (scrutinee sub-frame leak) open — deferred to [phase-12](../implementation_checklist/phase-12-self-hosting.md); memory pressure, not corruption. |
| [ci-test-coverage.md](ci-test-coverage.md) | **Tier 1 landed + required** (2026-06-12) — `--features llvm` codegen E2E + self-host oracle + wasm clippy/archive gates in CI. **Tier 2** (memory_sanitizer / LSan) + **Tier 3** (wasm E2E, needs node + wasm-tools) open. |

## ✅ Done

| spike | status |
|---|---|
| [network-async-coroutine-transform.md](network-async-coroutine-transform.md) | **Decided — A2 chosen and shipped** (2026-05-30). Network-async coroutine transform landed, default-on; slices 2b→5 + 5c mechanism done. |
| [oversized-enum-payload.md](oversized-enum-payload.md) | **Boxing landed** (2026-06-07). Box-oversized representation for payloads wider than the seeded enum area; pack/unpack both shipped. |
| [pattern-arm-unbound-field-drop.md](pattern-arm-unbound-field-drop.md) | **Done** for all four match constructs (if-let / match / let-else / while-let), 2026-06-07; all three follow-ups resolved. |
| [self-hosting-llvm-c-ffi.md](self-hosting-llvm-c-ffi.md) | ✅ Resolved (2026-06-11). LLVM-C binding approach validated end-to-end; minimal proof runs green (`exit=42`). Unblocks the codegen leg of Phase 12. |
| [self-hosting-llvm-c-proof.md](self-hosting-llvm-c-proof.md) | ✅ Runs green — `exit=42` (2026-06-11). A Kāra program drives `libLLVM-18` to build/verify/emit a working Mach-O object under the stage-0 Rust `karac`. |
| [self-hosting-llvm-c-surface.md](self-hosting-llvm-c-surface.md) | Done — initial pass (2026-06-10). Resolves sub-question 1 (surface scope) of the FFI spike. |
