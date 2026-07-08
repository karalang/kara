# Slice 1 (de-gate) — DECIDED: fold-into-`llvm`, landed on `main` 2026-07-08

**Status:** approved (route (a), fold-into-`llvm`) and landed directly on `main`.
This is the LLJIT-productionization **Slice 1 (de-gate)** decision record. It is
intentionally scoped to *de-gate only* — it does **not** flip repl/test to
JIT-by-default (that is Slice 5). The owner signed off on the +29-crate tradeoff
below in exchange for automatic CI coverage of the JIT lane.

## What it does

Folds the always-JIT execution backend (`src/codegen/lljit.rs`, the
`karac_jit_runner` bin, `test_jit_dispatch`, the repl JIT client) out of the
throwaway `lljit_prototype` cargo feature and **into the shipped `llvm`
feature**:

- `Cargo.toml`: `llvm = ["inkwell", "dep:llvm-sys", "dep:karac-runtime",
  "dep:libc"]`; the `lljit_prototype` feature is removed. `karac_jit_runner`'s
  `required-features` → `["llvm"]`.
- All ~72 `#[cfg(feature = "lljit_prototype")]` sites → `#[cfg(feature =
  "llvm")]` (across `src/` and the JIT `tests/`).
- `build.rs`: the ELF `--export-dynamic-symbol=karac_*` gate keys on
  `CARGO_FEATURE_LLVM`.

So `--features llvm` — the feature CI and the shipped install already build —
now carries the JIT engine + `karac_jit_runner`, and links `karac-runtime` (for
in-process JIT symbol resolution) + `libc`.

**Behavior is unchanged for users:** `karac repl` / `test` / `run` still default
to the tree-walk interpreter. The JIT is opt-in via `KARAC_REPL_JIT=1` /
`KARAC_TEST_JIT=1`. The two env-default reads were flipped from opt-out
(`!= Ok("0")`, JIT-default) to opt-in (`== Ok("1")`, interpreter-default) so
that folding into `llvm` does **not** silently ship the Slice-5 default-flip.
Slice 5 restores the opt-out defaults once the JIT-by-default flip is signed off.

## Why fold into `llvm` (vs a separate `lljit` feature)

The spike's stated driver: the JIT lane had **zero CI coverage** under
`lljit_prototype` — CI only ever builds `--features llvm`. Folding in means the
existing `--features llvm` clippy / codegen-E2E / memory-sanitizer jobs now
compile, lint, and (with `KARAC_TEST_JIT=1`) exercise the JIT for free. That
invisibility is exactly what let the whole-lane Linux breakage
(`B-2026-07-07-5`) sit undetected. This draft makes CI's
`clippy --all --all-targets --features llvm -- -D warnings` cover the JIT code
+ JIT test files (verified clean, below).

## The tradeoff (the decision that was made)

Folding into `llvm` means **every `--features llvm` build now links the
`karac-runtime` async/TLS tree** (`tokio` / `hyper` / `rustls` / `ring` / `mio`
/ `socket2` / `h2`) + `libc`. Measured: the `--features llvm` dependency graph
grows **104 → 133 crates (+29)**. There is **no more minimal-AOT-only build**
(`--features llvm` without the runtime dep). This is the accepted cost. The
alternative that was *not* taken — a separate `lljit` feature that *implies*
`llvm`, with the shipped build switching to `--features lljit` — would have
preserved a minimal-AOT `--features llvm`, at the cost of a CI-invocation change
and losing the "existing `--features llvm` jobs cover it for free" property. The
fold-into-`llvm` route was chosen per the spike's CI-visibility rationale.

## Verification (Linux x86_64, on `main`)

| Check | Result |
|---|---|
| `cargo check` (default, interpreter-only) | ✅ unchanged |
| `cargo build --features llvm --bins` | ✅ builds `karac` **and** `karac_jit_runner` |
| `cargo test --features llvm` JIT suites | ✅ `lljit_prototype` 18/18, `lljit_e2e` 16/16, `repl_jit` 23/23 — now run under plain `--features llvm` |
| `KARAC_TEST_JIT=1 … --test codegen` | ✅ 2084/2084 (JIT parity preserved) |
| `cargo clippy --all --all-targets --features llvm -D warnings` | ✅ clean (now covers the JIT code + tests) |
| `cargo fmt --all --check` | ✅ clean |
| repl/test default | ✅ interpreter (JIT opt-in) — Slice 5 kept separate |

## Not done in this draft (intentional — follow-ups)

- **CI yaml:** ✅ DONE 2026-07-08 — the `codegen-e2e` job gained a
  `Codegen E2E via LLJIT (run==build parity)` step that re-runs the codegen
  suite with `KARAC_TEST_JIT=1`, so the JIT *execution* lane runs in CI (not
  just compile+clippy). Asserts run==build parity across all 2084 cases.
- **Doc sweep:** `CLAUDE.md` and several tracker docs still say
  `--features lljit_prototype` in build recipes. Those instructions become
  `--features llvm`. Left out to keep the diff focused on the mechanism.
- **Release-size measurement:** debug binaries aren't meaningful; a
  `--release` `karac` size delta (with the runtime tree folded in) should be
  captured before final sign-off if binary size is a gating concern.
- **Slice 5** (flip repl/test to JIT-default): restore the opt-out env defaults
  + remove the interpreter fallback, with a `--interp` escape hatch.
