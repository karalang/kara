# CI test-coverage tiers — `--features llvm` E2E + wasm in CI

> **Status:** 🟡 partial (2026-06-12). **Tier 1 landed** — the `codegen-e2e`
> job (`.github/workflows/ci.yml`) runs the `--features llvm` codegen E2E +
> interpreter + typechecker + self-host oracle on every push/PR, and the `wasm`
> job runs both wasm clippy arms + both wasm staticlib builds. Both are
> **required status checks** on `main` (admin-bypassable, so direct-push-to-main
> still works). **Tier 2** (memory_sanitizer) and **Tier 3** (wasm E2E +
> component) are open. Supersedes the local `bugs.md` B-2026-06-12-2 follow-on.

## Why this exists

For most of the project's life CI ran `cargo test --all` — **no `--features
llvm`** — so the load-bearing real-binary correctness surface (codegen E2E,
the memory-sanitizer leak/UAF gate, the self-hosting differential oracle) and
the whole wasm surface ran **zero times** in CI. Two consequences bit in quick
succession (2026-06-11/12), each a wasm-only bug that drifted in unnoticed
because no CI job exercised it:

- a `clashing_extern_declarations` `malloc` clash (the wasm clippy gate was red
  for an unknown duration), and
- a `size_t`-width signature trap in `karac_alloc_or_panic` that made three wasm
  E2E fail — long mis-attributed to a "local env failure."

With concurrent sessions shipping to `main`, the per-session "I ran the full
`--features llvm` suite before shipping" discipline is the **one gate local
work can't guarantee across sessions**. CI is the shared backstop.

## The tiers

Split by cost / dependency weight, lightest first. Each tier is an independent,
verifiable add — a runner that has LLVM does not need node, etc.

### Tier 1 — codegen E2E + oracle ✅ (2026-06-12)

`codegen-e2e` job, one `ubuntu-latest` runner:

1. Install LLVM 18.1 via **apt** (`llvm-18 llvm-18-dev`), derive
   `LLVM_SYS_181_PREFIX` + `LD_LIBRARY_PATH` from `llvm-config-18`. *Not* the
   `install-llvm-action` — its prebuilt for `"18.1"` landed at a tree whose
   `llvm-config` the `llvm-sys` build script couldn't find. inkwell's
   `llvm18-1-prefer-dynamic` needs `libLLVM-18.so` at build **and** run time;
   the `LD_LIBRARY_PATH` line is what makes the test binaries load it.
2. Build the native runtime staticlib (lean→full) so the E2E binaries link
   instead of vacuously skipping.
3. `cargo test --features llvm --lib --test codegen --test interpreter --test
   typechecker --test selfhost_lexer`.

**Earned its keep on run 1.** Two Linux-surfaced failures, both genuinely
informative and neither a real codegen defect:

- A determinism test (`test_enum_variant_name_collision_..._deterministic`)
  diverged at iter 12 on Linux, mac-stable. Diffing the two captured IR strings
  showed the *only* difference was `@KARAC_SPAWN_SITES_ENABLED = i1 true|false`
  — an env-driven flag (`KARAC_RUNTIME_DEBUG_METADATA`). Root cause was a
  **parallel-test env-var race**: this was the one spawn-site test that read the
  flag without holding `SPAWN_SITE_ENV_LOCK` while peers flipped it. Production
  codegen reads the env once at startup and is deterministic — the bootstrap
  fixpoint is unaffected. Fix: acquire the lock. **This is the value
  proposition — a second platform catching a flaky test mac-only testing
  structurally cannot.**
- `e2e_vec_binary_stays_lean_no_heavy_runtime_floor` asserts a `<150 KB` floor
  calibrated on macOS arm64; the ELF/x86_64 baseline is ~328 KB. Gated the byte
  assertion to `cfg(target_os = "macos")`; kept a build-success check on every
  platform.

### Tier 2 — memory_sanitizer ⬜ (easy)

Add `tests/memory_sanitizer.rs` to the `codegen-e2e` test invocation (or a
sibling step). Needs an ASAN-capable `cc` (Linux's clang/gcc both qualify) and
the same native archive. On Linux, LeakSanitizer **works** (it does not on
macOS), so this leg *adds* leak coverage the local mac runs can't provide — a
real reason to do it, not just parity. Low effort once Tier 1's LLVM + archive
infra exists.

### Tier 3 — wasm E2E + component ⬜ (the heavy leg)

The `--features llvm` wasm E2E in `tests/cli.rs` — what would have caught the
`karac_alloc_or_panic` `size_t` trap (invisible to clippy; only manifests at
wasm *runtime*). Needs, on top of Tier 1's LLVM:

- **node** (preinstalled on `ubuntu-latest`) for the `node:wasi` run path,
- **`wasm-tools`** (`cargo install wasm-tools`) for `--bindings component`
  componentization,
- the **wasm archives** built per the CLAUDE.md § "Four archives" recipe
  (`libkarac_runtime_wasm*.a`), and
- the wasm targets (`rustup target add wasm32-wasip1 wasm32-wasip1-threads`).

Then `cargo test --features llvm --test cli wasm`. Watch for node:WASI version
drift as a flake source — keep this leg **non-required** until it has a stable
green history, so a node-version hiccup never blocks `main`.

## Notes for whoever picks up Tier 2/3

- The `codegen-e2e` job is the template — copy its LLVM-install + archive steps.
- Keep new legs **non-required** (branch protection) until they have a green
  streak; required-but-flaky teaches everyone to ignore red.
- The four-archive recipe and the ASAN/node/wasm-tools prereqs are in
  `CLAUDE.md` § "Codegen E2E + memory_sanitizer require the runtime library".
