# Spike: LLJIT productionization — collapse the `run`/`build` divergence tax

**Status:** OPEN — active epic, self-hosting (Phase 12) paused behind it.
**Decision date:** 2026-07-06. **Owner call:** pause self-hosting; productionize LLJIT to the default execution backend *first*, then move `karac run` onto it and strip run-leniency.

**Progress (2026-07-07 — Slice 2 proof gate GREEN on Linux; the JIT lane was 0% functional on Linux before this):**
The whole engine (W1–W5) had only ever been exercised on macOS arm64, and *every prior "green" claim was platform-blind*. On the first Linux run, the codegen-E2E-via-JIT proof gate came back **1416 failed / 649 passing** — the JIT produced empty output for every program that touches the runtime. Two root causes, both now fixed:

1. **JIT symbol resolution on ELF (`B-2026-07-07-5`, fix `199098e`).** ORC's process-symbol-search generator resolves the statically-linked `karac_*` runtime FFI via `dlsym(RTLD_DEFAULT, …)`, which on ELF only sees symbols in the executable's `.dynsym`. Rust exports nothing by default (measured **0 / 507** `karac_runtime_*` symbols in `.dynsym`), so the JIT failed to materialize `main` (`Symbols not found: [karac_runtime_scheduler_start_dispatcher, …]`). macOS's Mach-O `dlsym` finds main-image symbols without an export flag, which is exactly why this never surfaced. Fix: a `build.rs` exporting `--export-dynamic-symbol=karac_*` for the JIT-hosting + integration-test binaries on ELF targets (scoped glob keeps `.dynsym` lean); the in-process test binary also force-links the runtime + defines the `KARAC_SPAWN_SITES` stand-ins; plus an explicit stdio flush in the runner one-shot exit path.
2. **Borrow-return codegen UB (`B-2026-07-07-4`, fix `fddfb9a`).** With symbols resolving, 5 residual failures were a real miscompile the JIT *exposed*: `fn f(u: ref String) -> ref String { u }` lowered the returned ref through the owned-value move-out path, GEP-ing a `String`'s `cap` field (offset 16) off an 8-byte `ref` pointer slot and storing out of bounds. `-O0` tolerates it; the LLJIT (~O2) weaponizes the UB into a segfault — the canonical "AOT oracle masks a latent codegen bug" case this epic exists to kill. Fix: guard the move-null to inline-owned slots.

A third fix followed from validating the wider JIT surface on Linux: **`B-2026-07-07-6`** — the same UB class as #2 at a different site (a REPL snapshot cap-suppress applied to a replayed i64 binding's slot) crashed the JIT runner at PC=0 on a cross-type REPL rebind; guarded to inline-struct slots.

**Result:** codegen E2E via JIT on Linux **1416 → 0 failed (2084/2084)**; AOT baseline unchanged (2084/2084). **Every JIT suite is green on Linux:** `lljit_prototype` 15/15, `lljit_e2e` 16/16, `karac_jit_runner_repl` 3/3, `repl_jit` 23/23, and the `karac test` JIT batch runner (`tests/cli.rs`) 491/491. **Slice 2 (the proof gate) is met on Linux, and the repl/test JIT command paths are de-risked.** This is the first time the JIT lane has ever been exercised — and passed — off macOS.

---

## Decision & rationale

**Do this before Phase 12 self-hosting.** The single largest recurring bug class in this repo is `karac run` (tree-walk interpreter) diverging from `karac build` (AOT LLVM): **23% of all fixed bugs (67 / 284) are explicit run-vs-build splits**, and ~half the ledger touches the theme (value_compare missing arms, decl-order vs `karac_cmp`, numeric-coercion split, representation/lowering gaps). By `design.md` § Specification Layers these are **guaranteed-semantics** — "the program's meaning" — so maintaining two implementations of them is a spec-divergence generator, not a code smell.

The roadmap already chose the cure (Core Strategy #2, locked 2026-05-05 / brainstorm v62): **LLVM is the single execution backend** — `karac repl`/`test` run on always-JIT LLJIT (same lowering as `build`, JIT'd lazily), tree-walk interpreter retained as dev/debug only. One lowering invoked two ways (AOT + JIT) collapses the divergence class *by construction*. This spike finishes that migration and extends it to `karac run`.

**Why before self-hosting:** the self-hosted compiler is the largest Kāra codebase that will ever be written. Porting it while `run` and `build` still diverge pays the tax thousands more times and risks the self-hosted compiler miscompiling itself in ways the tree-walk oracle masks. Collapse the tax first.

Related: [`docs/diagnostic-fix-audit.md`](../diagnostic-fix-audit.md) (the other flagship-hardening axis), bug-ledger run/build splits (`grep 'run-vs-build\|works under.*run' docs/bug-ledger.jsonl`).

---

## Corrected current state — the tracker was wrong

`phase-7-codegen.md` L696 marked **"Always-JIT backend DONE 2026-06-03, JIT-default flip landed."** **That is false.** Ground truth (2026-07-06):

- `Cargo.toml`: `default = []`; `lljit_prototype` is **off by default**, commented *"promoted to default + integrated into `karac repl`/`karac test` once W6 closes."*
- Every JIT dispatch path in `src/repl.rs` is `#[cfg(feature = "lljit_prototype")]`; the `karac_jit_runner` bin has `required-features = ["lljit_prototype"]`; the REPL's own error text says *"ensure karac was installed with --features lljit_prototype."*
- **Therefore, in the shipped/default `karac`, `repl` / `test` / `run` ALL run on the tree-walk interpreter.** The "flip to default" never happened. (Classic unreliable-`[x]` per the repo's checklist-scoping caution.)

**What IS genuinely done** (real, green, but behind `--features lljit_prototype`): the engine, W1–W5. `src/codegen/lljit.rs::LLJITEngine` (~140 LOC over `llvm-sys::orc2`); `ResourceTracker` RAII; the process-symbol-search generator (`LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess` — the fix for arm64-Mach-O; MCJIT is not viable on Apple Silicon, hangs at PC=0 on any external call); error/threading hardening (`OnceLock` native-target init, malformed-IR → `Err`). 15/15 `tests/lljit_prototype.rs` + 14/14 `tests/lljit_e2e.rs` green. Test harness `jit_run_program(src) -> Option<String>` mirrors the AOT `run_program`.

**What is NOT done:** the default flip; full codegen-E2E-via-JIT parity; DWARF/coro on the JIT lane; four follow-ons; and `karac run` on the JIT at all (the original plan was repl/test only — `run` is added by this decision).

---

## Remaining scope — ordered slices (sequencing is load-bearing)

Prerequisites (1–5) must land before the user-facing flip (6).

1. **De-gate foundation.** Promote the `lljit_prototype` deps (`dep:llvm-sys`/`orc2`, `dep:karac-runtime`, `dep:libc`) into the default (or a shipped) feature set; build & ship `karac_jit_runner`; keep the runtime `karac_*` symbols force-linked so the JIT process-symbol generator resolves them. Handle LLVM link + binary-size impact.
2. **Codegen-E2E-via-JIT parity (the proof gate). ✅ MET ON LINUX 2026-07-07 (2084/2084 via `KARAC_TEST_JIT=1`).** The suite already had a `KARAC_TEST_JIT=1` dispatch path (`tests/codegen.rs::jit_dispatch` → `karac_jit_runner` subprocess); running it on Linux surfaced the two root causes above (`B-2026-07-07-5` symbol export, `B-2026-07-07-4` borrow-return UB) and both are fixed. **Residual for full closure:** (a) verify on macOS arm64 (the fixes are ELF-scoped no-ops there, but re-run to confirm); (b) decide whether to make `KARAC_TEST_JIT` the *default* codegen-suite dispatch (it is still env-gated — the parity is proven, but the AOT lane is still the default oracle) vs keeping both lanes; (c) the `lljit_prototype` in-process direct-engine suite is green but small — the bulk parity evidence is the 2084-case codegen suite.
3. **DWARF + coro passes through `LLJITEngine`.** Route the Level-2 DWARF crash-location emission and the coroutine-transform pass through the JIT lane (the remaining "wrapping done" dependency per W5).
4. **Close the four open follow-ons:** L626 ambient-method codegen, L632 unified provider dispatch, L640 harness flake, L642 REPL aggregate snapshots.
5. **Flip `repl` + `test` to JIT-by-default.** Remove the `cfg(lljit_prototype)` gates in `cmd_repl` / `cmd_test`; keep a `--interp` escape hatch for compiler-internal/dev use. **Linux de-risking done 2026-07-07 (gated on slice 1 for the actual gate-removal):** the `karac test` JIT batch-runner path is green on Linux (`tests/cli.rs` 491/491 under `--features lljit_prototype`, JIT-default) and the repl JIT path is now **23/23** (`B-2026-07-07-6`, the cross-type REPL rebind runner crash, was root-caused and fixed this session — a String cap-suppress applied to the replayed i64 binding's slot; same class as `B-2026-07-07-4`).
6. **`karac run` → JIT + strip leniency  ← the pickup point, gated on 1–5.** Route `cmd_run` through the JIT (same as `cmd_test`). Make the typecheck gate **strict**: remove the phase-10 run-leniency entirely — the `TypeErrorKind::is_run_fatal()` partition and the after-fatal-gate typecheck ordering both go away, so `run` rejects hard type errors like `build`/`check`. Net simplification.
7. **Gates.** Full suite green via JIT on macOS arm64 **and** Linux; Linux/LSan leak gate; DWARF crash-diagnostics verified; publish the cold-start bench. **Linux status 2026-07-07 — ALL JIT suites green:** codegen E2E 2084/2084 (AOT baseline also 2084/2084), `karac test` batch 491/491, `lljit_prototype` 15/15, `lljit_e2e` 16/16, `karac_jit_runner_repl` 3/3, `repl_jit` 23/23. fmt + clippy (`--features lljit_prototype --all-targets`) clean. **Not yet done:** macOS re-verify (the fixes are ELF-no-ops there but confirm), Linux/LSan run of the JIT lane, DWARF-on-JIT verification, cold-start bench.

---

## Session handoff (2026-07-07) — what's done, what's next, decisions owner must make

**Done + pushed this session** (commits `199098e`, `fddfb9a`, `e21d41f`): the Linux JIT unblock (the lane was 0% functional on Linux — see the Progress note at the top), Slice 2 proof gate green on Linux, and broad Linux JIT validation (`karac test` batch + repl + engine suites). This is pure de-risking — **no user-facing behavior changed**; the JIT is still behind `--features lljit_prototype` and the tree-walk interpreter is still the default `run`/`repl`/`test` backend.

**Next up, in order:**

- **Slice 1 (de-gate) — DECISION NEEDED before executing.** Ground truth confirmed this session: CI's shipped build is `cargo … --features llvm` and runs **zero** `lljit_prototype` tests (`.github/workflows/ci.yml`), so the JIT lane has *no CI coverage today* and "shipping" it means it must ride the `llvm` feature. The mechanical recipe is clear — rename the `lljit_prototype` gate to a shipped feature and have `llvm` enable it (`llvm = [… , "lljit"]`, `lljit = ["dep:llvm-sys", "dep:karac-runtime", "dep:libc"]`), flip the ~72 `#[cfg(feature = "lljit_prototype")]` sites, `karac_jit_runner`'s `required-features`, and `build.rs`'s `CARGO_FEATURE_*` check. **The decision is the tradeoff, not the mechanics:** folding into `llvm` forces the tokio/hyper/rustls `karac-runtime` dep + the JIT build cost onto *every* AOT/`karac build`/CI-`llvm` build (no more minimal-AOT-only build), and newly subjects the JIT code to CI's `clippy --features llvm -D warnings`. Left for owner sign-off rather than pushed blind — it changes the shipped binary's composition and every `--features llvm` build's footprint, and can't be validated on macOS/CI from here. (Once blessed: also add a CI job that runs the codegen suite with `KARAC_TEST_JIT=1` so the JIT lane stops being CI-invisible — that invisibility is exactly what let the whole-lane Linux breakage sit undetected.)
- **Slice 3 (DWARF + coro on the JIT lane).** Coro already runs on the JIT lane (`run_coro_passes` in `LLJITEngine::parse_ir_into_tsm`); the open piece is Level-2 DWARF crash-location emission through the JIT. Engineering slice, no decision.
- **Slice 4 (four follow-ons L626/L632/L640/L642).** Non-blocking; independent of the flip.
- **Slice 5 (flip repl/test to JIT-default).** Gated on slice 1 (the gates to remove are `cfg(lljit_prototype)`). Linux-de-risked this session and now fully green on Linux (`karac test` batch 491/491, `repl_jit` 23/23 after fixing `B-2026-07-07-6`) — the flip is a mechanical gate-removal once slice 1 lands.
- **Slice 6 (`karac run` → JIT + strip run-leniency) — DECISION NEEDED (already flagged in Gotchas).** The leniency strip is a *visible behavior change* (programs that "ran" via lenient tree-walk get rejected) and requires the `examples/` + `kara-katas` + `examples/mend` sweep the Gotchas call for. Do not strip blind.

**Owner re-sequencing question** (roadmap Core Strategy #5) is still open and unactioned per the original spike note.

---

## Gotchas — do not rediscover these

- **Per-module JITDylib collision (W2 finding).** Every karac module emits the same Debugger-Contract globals (`KARAC_SPAWN_SITES*`, `kara.string_table`, `karac_jit_template_manifest`); a single JD rejects the duplicates. For `run`/`test` each program is one module → one JD, fine. For REPL, use cell-shadowing via tracker-cycle (works today) or per-module JD isolation (needs `ExecutionSession::lookup`, below the LLJIT C API). Pinned by `lljit_w2_finding_same_jd_collides_on_runtime_globals`.
- **Apple Silicon requires orc2/LLJIT, not MCJIT** — MCJIT hangs at PC=0 on any external (libc/runtime) call on arm64-Mach-O. Non-negotiable on the primary dev platform.
- **ELF (Linux) requires `--export-dynamic-symbol` for the runtime FFI (2026-07-07 finding).** The process-symbol-search generator is a `dlsym(RTLD_DEFAULT, …)` lookup; on ELF that only sees the executable's `.dynsym`, which Rust leaves empty for statically-linked symbols. Every JIT-hosting binary — `karac_jit_runner`, and any in-process-JIT test/`karac run` binary — must export the `karac_*` surface (handled by `build.rs`) **and** actually reference those symbols so the linker keeps them (`__preserve_no_mangle_symbols` force-link; the subprocess runner and `tests/codegen.rs`/`tests/lljit_prototype.rs` already do). Symptom when missing: `Symbols not found: [karac_runtime_*]` and empty output. macOS never needs this (Mach-O `dlsym` finds main-image symbols), so it is invisible until the suite runs on Linux — do not trust a macOS-only green.
- **The LLJIT compiles at ~`-O2` (its builder default), so it exposes UB the AOT lane's opt level masks.** `B-2026-07-07-4` (borrow-return out-of-bounds move-null) ran clean at `llc -O0` and segfaulted at `llc -O2`; the JIT crashed, the AOT test oracle didn't. When triaging a JIT-only "empty output"/crash whose symbols all resolve, reach for `llc -O0` vs `-O2` on the *same emitted IR* to localize an optimizer-weaponized UB before assuming a JIT-engine bug.
- **The tree-walker STAYS — it's the `comptime` engine.** `src/comptime.rs` runs `crate::interpreter::Interpreter` over the typed AST for every `comptime { ... }` block, on every compile; comptime is a v1 differentiator. Demote it from `karac run`, do **not** delete it. Keep the interpreter test suite — its effectful/runtime-only paths (I/O, channels, `par`) lose `karac run` as their exerciser and would otherwise rot; the tests are the dev-tool guard. Expose `karac run --interp` for compiler-internal work.
- **Leniency removal is a visible behavior change.** Programs that "ran" under lenient tree-walk (hard type error → `warning` → placeholder `0`/`""` → *silent wrong output, exit 0*) will now be rejected. That un-masking is the point — the leniency already caused a real incident (`B-2026-06-13-15`, the self-host lexer cast). Sweep `examples/`, `kara-katas`, and `examples/mend` for anything relying on it before flipping slice 6.

## Acceptance criteria

Default `karac run` / `repl` / `test` execute via the LLVM JIT lowering; `run == build` by construction; the full codegen E2E suite passes via the JIT path; the tree-walk interpreter is retained and green for `comptime` + `--interp`; leak + DWARF + cross-platform (macOS arm64 + Linux) gates pass; published REPL cold-start number.

## Open re-sequencing question (needs owner confirm — not yet actioned)

`roadmap.md` Core Strategy #5 sets execution order **8 → 9 → 10 → 12 (self-host) → 11**. This spike inserts LLJIT-productionization *before* Phase 12. If confirmed, update roadmap #5 and the Phase Dependency Graph accordingly. Flagged here; the roadmap edit is deliberately left for owner sign-off.
