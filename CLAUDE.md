# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                            # Build the compiler (no LLVM backend)
cargo test                             # Run non-codegen tests (lexer, parser, resolver, typechecker, effect, ownership, interpreter)
cargo test --features llvm             # Run ALL tests including codegen E2E + memory_sanitizer (ASAN)
cargo test lexer                       # Run a single test file (e.g., tests/lexer.rs)
cargo test -- test_name                # Run a single test by name
cargo clippy --all --all-targets -- -D warnings  # Lint (must be clean before declaring work done)
cargo fmt --all                        # Format all files
cargo fmt --all -- --check             # Verify formatted (must be clean before declaring work done — peer to clippy)
```

**`cargo fmt --all -- --check` is a hard pre-commit gate, peer to clippy.** Both must clear before any commit lands. **First action of any new coding session or slice:** run `cargo fmt --all -- --check`. If it fails, fix with `cargo fmt --all` and land as a standalone `chore: cargo fmt cleanup` commit *before* starting feature work. Don't pull fmt drift into a feature commit; don't surgically revert drift to keep a commit scoped — both patterns push cleanup to CI and let drift accumulate in the meantime.

**Use `--all-targets`, not `--tests`, on the clippy gate.** `--tests` only builds the test target (cfg(test)), so any lint that fires only in production cfg slips through. The runtime crate has cfg-gated type definitions (e.g. `KARAC_SPAWN_SITES` is `extern KaracSpawnSiteEntry` in production but a `SpawnSiteEntryStandIn` wrapper under cfg(test)) — clippy lints on those code paths only fire in the cfg where they're real, and CI runs `cargo clippy --all -- -D warnings` (no `--tests`). `--all-targets` builds lib + bins + tests + examples + benches, each in its own cfg, so it covers both surfaces.

**Codegen and memory-sanitizer tests are gated on `--features llvm`.** Plain `cargo test` will skip `tests/codegen.rs`, `tests/par_codegen.rs`, and `tests/memory_sanitizer.rs` entirely (the modules are `#[cfg(feature = "llvm")]`). Always use `--features llvm` when verifying codegen-related work; otherwise you will miss real regressions.

**Codegen E2E + memory_sanitizer require the runtime library.** One-time setup on a fresh checkout:

```bash
# Lean archive first (rustls-free, native net kept) — built into the canonical name, then renamed.
cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
# Full archive (TLS on) overwrites the canonical name — must run SECOND.
cargo rustc -p karac-runtime --release --crate-type staticlib   # target/release/libkarac_runtime.a
# WASM archive (phase-10 `--target=wasm_wasi`) — separate target dir, no clobber risk.
cargo rustc -p karac-runtime --release --target wasm32-wasip1 --no-default-features --crate-type staticlib
cp target/wasm32-wasip1/release/libkarac_runtime.a target/release/libkarac_runtime_wasm.a
# Threaded WASM archive (phase-10 `--features wasm-threads`) — separate target dir too.
# Prereq: `rustup target add wasm32-wasip1-threads` (its sysroot is the only one whose
# wasi-libc is built with atomics — required for the --shared-memory link).
cargo rustc -p karac-runtime --release --target wasm32-wasip1-threads --no-default-features --features wasm-threads --crate-type staticlib
cp target/wasm32-wasip1-threads/release/libkarac_runtime.a target/release/libkarac_runtime_wasm_threads.a
# GPU archive (OPTIONAL — only for building programs that call `gpu.dispatch`;
# carries the heavy wgpu/Metal backend). Emits the canonical name, so build it
# LAST and rename immediately (like the lean archive). Skip unless doing GPU work.
cargo rustc -p karac-runtime --release --features gpu --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_gpu.a
# Re-run the plain full build afterward so the canonical name is the non-GPU archive again:
cargo rustc -p karac-runtime --release --crate-type staticlib
```

**Four archives: lean-then-full (+ wasm + wasm-threads).** `karac` links the **lean** `libkarac_runtime_min.a` for any program that references no TLS-only runtime symbol (`karac_runtime_tls_*` / `_serve_https` / `_http_client_*` / `_http_builder_*` / `_ws_accept_tls`), and the **full** `libkarac_runtime.a` otherwise; it falls back to the full archive when the lean one is absent, so building only the full archive is always correct (just no size win). The lean archive omits the `rustls`/`ring` tree (gated behind the runtime's `tls` feature; the default set is `["tls", "net"]`, and lean keeps `net`), recovering ~65 KiB on every compute/auto-par binary — see phase-7-codegen.md § "Phase 4". The **wasm** archive (`--no-default-features`, i.e. no `net` either) compiles out the whole tokio/hyper/mio/socket2 + native-scheduler/event-loop surface — none of those deps build on wasm32 — and instead carries the **sequential cooperative scheduler** (`runtime/src/seq_scheduler.rs` + `seq_par_run`, phase-10 "WASM concurrency lowering — sequential default"): `spawn()`/`TaskGroup`/`par {}` work on wasm, single-threaded, FIFO-deterministic. It is what `karac build --target=wasm_wasi` links; without it, wasm builds fail at link with a pointer to this recipe. The **wasm-threads** archive (`--target wasm32-wasip1-threads --no-default-features --features wasm-threads`) is the threaded sibling: the native pool substrate compiled for wasm (std threads are real there — pthreads over the wasi-threads ABI, futex atomics over shared memory) plus `runtime/src/wasm_threads_scheduler.rs` for the spawn/TaskGroup externs (exactly one scheduler exports `karac_runtime_*` per archive — `scheduler.rs` under `net`, `seq_scheduler.rs` on sequential wasm, `wasm_threads_scheduler.rs` under `wasm-threads`). It is the second leg of `karac build --target=wasm_browser --features wasm-threads`'s dual artifact (`<stem>.threads.wasm`); without it those builds fail at link with the recipe. Wasm-cfg clippy must be run per-target — CI's native clippy never sees either wasm arm: `cargo clippy -p karac-runtime --target wasm32-wasip1 --no-default-features` and `cargo clippy -p karac-runtime --target wasm32-wasip1-threads --no-default-features --features wasm-threads`. All commands must use `cargo rustc … --crate-type staticlib`, NOT `cargo build`. Build order matters: `--no-default-features` and the default build both emit `target/release/libkarac_runtime.a`, so build lean first and copy it to `libkarac_runtime_min.a` *before* the full build overwrites the canonical name. (`KARAC_FORCE_FULL_RUNTIME=1` forces the full archive for any program — an escape hatch if symbol detection ever misfires. `KARAC_RUNTIME=<path>` overrides resolution entirely and is honored **verbatim** — the named file is the linked file, with no lean-sibling substitution — so tests that build a feature-gated archive, e.g. `tests/park_and_wake.rs`'s `test-helpers` build, link exactly what they built.) A **fifth, optional `gpu` archive** (`libkarac_runtime_gpu.a`, `--features gpu`, spike slice-0) sits outside the lean/full axis: it is a superset of the full archive plus the wgpu/Metal backend, **auto-selected** only when the emitted object references `karac_runtime_gpu_*` (a `gpu.dispatch` program). Non-GPU builds never see it; a GPU build without it fails at link with an actionable "build `libkarac_runtime_gpu.a`" message. It stays opt-in because wgpu + naga + objc2 add ~4.5 MB to the archive and a heavy dep tree — see `docs/spikes/gpu-wgsl-slice0.md`.

**Use `cargo rustc … --crate-type staticlib`, NOT `cargo build -p karac-runtime --release`.** The runtime's `[lib] crate-type` is `["staticlib", "rlib"]` (the `rlib` exists only for the opt-in `lljit_prototype` test path). Under `lto = "fat"`, emitting both artifacts in one `cargo build` defeats the staticlib's cross-module DCE — std's panic/alloc-error default hooks stay reachable and the ~57 KiB DWARF backtrace symbolizer survives `-dead_strip` into *every* AOT binary (measured: auto-par floor 295.7 KiB → 417.7 KiB, +41%). `cargo rustc --crate-type staticlib` builds only the staticlib, so LTO strips the symbolizer. See the comment at `runtime/Cargo.toml`'s `crate-type` line for the full rationale.

Without this, the E2E tests (including all `tests/memory_sanitizer.rs` cases) skip with a stderr notice rather than exercise real binaries — they pass vacuously. `tests/memory_sanitizer.rs` additionally requires a `cc` that supports `-fsanitize=address`; if missing (or if `KARAC_SKIP_ASAN_TESTS=1` is set), it skips gracefully.

**Leak detection: the Linux-CI `memory-sanitizer` job is the authoritative gate, not local macOS.** `-fsanitize=address` runs **LeakSanitizer on Linux but NOT on macOS** — so a local `cargo test --features llvm --test memory_sanitizer` on a Mac catches use-after-free / double-free only, and **silently misses leaks**. The CI `memory-sanitizer` job (ubuntu, [`docs/spikes/ci-test-coverage.md`](docs/spikes/ci-test-coverage.md) Tier 2) runs the same suite *with* LSan, so it is the comprehensive, automatic leak gate for the whole codegen-ownership class — strictly better than the older manual one-at-a-time `leaks --atExit` spot-check. Practical rule: **do not conclude "no leak" from a green local Mac asan run**; consult the Linux CI job (or run the suite under a Linux/LSan toolchain) before trusting a leak-class fix. CI now also runs the `--features llvm` codegen E2E + self-host oracle (`codegen-e2e`) and the wasm clippy/archive gates (`wasm`) — see the spike for the full tier map; the once-true "CI runs no `--features llvm`" assumption is obsolete.

**Embedded-component wasm tests additionally need `wasm-tools`** (`cargo install wasm-tools` or `brew install wasm-tools`): `--bindings component` — the `wasm_wasi` default — shells out to it for componentization (phase-10 embedded-WIT migration; design.md § Component Model emission). Without it, the embedded-component E2E tests in `tests/cli.rs` skip with a stderr notice (same vacuous-pass caveat as the archives). The wasi preview1 adapter is vendored in karac itself (`wasi-preview1-component-adapter-provider` crate) — no extra setup.

## Branch management

**Two environments, two workflows — pick by where the session runs.** The worktree rules in the rest of this section govern the **local multi-worktree checkout** (the primary machine, where sibling worktrees run parallel slices and the primary's clean `git status` is load-bearing). They do **not** apply to an **ephemeral cloud container** (Claude Code on the web / a fresh clone discarded when the session ends): there are no sibling worktrees, no parallel slices, and nothing to isolate from, so `EnterWorktree` + a feature branch buys nothing but ceremony. In a cloud container, **work directly on `main`** — commit straight to `main`, no feature branch, no PR unless explicitly asked (owner-authorized 2026-07-07, overriding the mandatory-worktree default below for this environment only).

The one discipline that carries over regardless of environment: **`main` advances from other sources mid-session** — other cloud sessions, teammates, bots. This repo has seen a sibling commit (`docs(mend)`) land on remote `main` *during a single task*. So working "directly on `main`" is not "ignore the remote":

- **Before starting a slice AND before every push**, sync local `main` to the remote: `git fetch origin main`, then `git reset --hard origin/main` if you have no local-only commits yet, or `git rebase origin/main` to replay your commits on top if you do.
- **Push with `git push origin main`**; retry transient network errors with 2s/4s/8s/16s backoff. A push rejected as **non-fast-forward** means the remote advanced after your last fetch — `git fetch` + `git rebase origin/main`, then retry. **Never** `push --force` / `update-ref` past commits you did not create (the silent-rewind footgun in failure mode 2 below); rebase onto them instead.
- **Deleting the working branch**, when one exists, is a UI/one-click step for the owner — the cloud git gateway returns `403` on ref deletion and the GitHub MCP server exposes no delete-branch tool, so it cannot be done from inside the session. Delete the *local* branch after confirming it is fully contained in `main` (`git merge-base --is-ancestor <branch> main`), and flag the remote ref for the owner to remove.

**On the local checkout, all dev work in karac-rust happens in an isolated worktree, not on the primary `main` checkout.** Every implementation slice — feature, fix, refactor, even single-line bug fixes — starts with `EnterWorktree` (which honors `.claude/settings.local.json`'s `worktree.baseRef: "head"`, so the new worktree picks up local-but-unpushed `main` commits). Commit inside the worktree, then `git rebase main` from the worktree and `git merge --ff-only <branch>` from the primary to integrate. Direct commits to `main` from the primary checkout are reserved for pure recovery operations (the `update-ref` failure-mode dance below) — never for normal feature/fix work, even if "it's just two lines."

Why mandatory rather than judgment-call: the primary worktree's role is review, cross-referencing, and integration. Mixing in-progress work there contaminates `git status`, blocks parallel slices, and skips the rebase-loud-fail signal that catches stale fork-points (the same signal that prevents the silent-rewind footgun in failure mode 2 below). Worktree isolation makes "what's on main" and "what I'm currently doing" structurally separate, which is what every other rule in this section relies on.

The kara-katas repo is a different story — it's a content repo, not the compiler, and direct commits to its `main` are fine.

**Always update `main` via `git merge --ff-only` from the primary worktree.** Cross-worktree `git update-ref refs/heads/main <source-tip>` bypasses git's "checked-out branch can't be ff'd" safety net and has two known failure modes — both have hit this repo:

1. **Stale primary worktree.** The primary worktree's index and working tree don't refresh after the ref moves; subsequent `git status` there renders the just-landed commit as "uncommitted changes" (the inverse diff of what was shipped). Recovery: `git stash push` clears the false diff in one step. Detailed reproduction in the user's memory at `reference_update_ref_stale_primary_worktree`.

2. **Silent main rewind.** If the source branch's history doesn't include the current main tip (e.g. branched off main before another feature merged), `update-ref` overwrites main and the commits between the source's fork point and the previous tip become orphans — still in the reflog (default 90-day retention) but invisible from `git log main`. Recognize by `reset: moving to HEAD` reflog entries with no source SHA in the action column. Recovery: identify the previous tip from `git reflog main`, `git update-ref refs/heads/main <previous-tip>`, `git reset --hard` to sync the worktree, then cherry-pick anything that was on the rewound branch. Save uncommitted state to a patch first if `reset --hard` is involved.

`git merge --ff-only <branch>` from the primary worktree avoids both: it refreshes index+worktree atomically and rejects non-fast-forward updates loudly. If the ff is rejected, the source branch needs `git rebase main` before retrying — never reach for `--no-ff` or `update-ref` as a workaround.

**Prefer rebase + ff over cherry-pick when integrating a side branch.** `git rebase main` from inside the side branch's worktree, then `git merge --ff-only <branch>` from the primary, preserves the side branch's identity — its tip ends up on main's history with the same SHA, so a subsequent `git branch -d <branch>` (the *safe* form that refuses to delete unmerged work) succeeds cleanly. Cherry-pick produces a content-equivalent commit with a fresh SHA; main then has the patch but the side branch's tip is orphaned, forcing `git branch -D` (force-delete) and leaving the original SHA reachable only via the reflog. Reserve cherry-pick for cases where no live branch ref exists — recovering a single commit from a deleted branch or from an orphan SHA in the reflog. The 2026-05-20 recovery used cherry-pick for one such reconstruction; for any future rewind recovery, prefer `git rebase <restored-main> <orphan-branch>` followed by ff if the source branch is still around.

## Architecture

`karac` is a Rust implementation of the Kāra language compiler. The pipeline flows:

```
Source → Lexer → Parser → AST → Resolver → TypeChecker → EffectChecker → OwnershipChecker → Interpreter
```

Each phase is a separate module under `src/`:

| Module | Role |
|---|---|
| `token.rs` | Token/Span definitions used across all phases |
| `lexer.rs` | Tokenizes source into `Vec<SpannedToken>` |
| `ast.rs` | AST node definitions; every node carries a `Span` |
| `parser.rs` | Recursive-descent parser; produces `ParseResult` with error recovery |
| `resolver.rs` | Name resolution, scope analysis, visibility checking |
| `typechecker.rs` | Type inference, generic instantiation, trait bound checking, pattern exhaustiveness |
| `effectchecker.rs` | Effect inference for private fns; effect verification for public fns; conflict detection |
| `ownership.rs` | Parameter mode inference (own/ref/mut ref), move checking, RC fallback detection |
| `interpreter.rs` | Tree-walk interpreter (Phase 4, in progress) |
| `lib.rs` | Public API — thin wrappers that chain phases together |

The entry point for programmatic use is `src/lib.rs`, which exposes `tokenize`, `parse`, `resolve`, `typecheck`, `effectcheck`, and `ownershipcheck` as top-level functions.

**Codegen containment is a load-bearing architectural invariant.** `src/codegen.rs` (gated behind `--features llvm`) is the **only** module that imports `inkwell` or references LLVM types. All upstream phases — `token`, `lexer`, `ast`, `parser`, `resolver`, `typechecker`, `effectchecker`, `ownership`, `concurrency`, `interpreter` — treat the backend as a black box and use plain Rust types. **Never add `inkwell::` or LLVM-typed imports to those modules.** New phases that need to communicate codegen hints (layout decisions, vectorization annotations, etc.) must do so through plain-data hint records consumed by `codegen.rs`, not through embedded LLVM types in the analysis output. This containment is what makes a future codegen-substrate swap (e.g., MLIR) a contained surgery on one module rather than a compiler rewrite. Full architectural commitment in [`docs/design.md § Codegen architecture`](docs/design.md#codegen-architecture).

Integration tests live in `tests/` (one file per phase). End-to-end `.kara` programs live in `examples/`.

## Language Design

The language spec lives in `docs/design.md` (authoritative). Implementation plan in `docs/roadmap.md`.

Key Kāra language concepts the compiler must implement:

- **Generics syntax:** `[T]` not `<T>` — `Vec[i32]`, `fn sort[T: Ord](...)`. No turbofish.
- **Effects:** Eight built-in verbs — six *resource verbs* (`reads`, `writes`, `sends`, `receives`, `allocates`, `panics`) that drive conflict analysis and two *execution verbs* (`blocks`, `suspends`) that drive scheduler placement. Resource verbs apply to user-defined resources; execution verbs take no resource parameter. Private function effects are *inferred*; public function effects are *declared and verified*.
- **Ownership tiers:** owned (default) → `ref` → RC. Parameter modes are always declared at the signature — bare `T` is owned, `ref T` / `mut ref T` / `mut Slice[T]` are explicit borrow forms; bare `self` / `ref self` / `mut ref self` follow the same rule for receivers. Body-level ownership analysis is a checking aid (verifies usage matches the declared mode, drives `karac explain` "would-be mode" diagnostics, feeds use-site classification for the RC fallback pass) — it is not a signature-derivation mechanism.
- **Call-site mutation markers:** free-function calls write `mut` on arguments whose place-expression root is a fresh owned binding (or a temporary / literal / function return) when the callee's parameter is `mut ref T` / `mut Slice[T]`. Arguments rooted at a `mut ref` binding already in scope forward without marking. Method calls, field assignment, and index assignment never mark. `ref` is never legal at call sites. See design.md Feature 4 Part 1½.
- **`shared struct`/`shared enum`:** reference-semantics types using RC.
- **Layout blocks:** separate logical struct definition from physical memory layout (SoA, field grouping for cache locality).

## Coding Standards

- Idiomatic Rust; follow `rustfmt` conventions.
- Every compiler phase must emit structured diagnostics with source spans — never just panic.
- Tests for every language construct. Use `tests/` for integration tests, unit tests inside each module for focused coverage.

## Developing Kāra code (not the Rust compiler — the `.kara` you write)

New Kāra — katas, examples, tests, dogfooding functions, self-hosting units — is developed and verified **through the Mend loop**, not hand-fixed: run `karac check --output=json`, apply `karac fix` for machine-applicable diagnostics as the primary fix path, feed the rest back, then verify the result against an **oracle** (expected output / test cases / a reference `solution.kara` / the self-host fixpoint). "It compiles" is not the bar. Each new artifact becomes a Mend task+oracle pair — format and granularity rule in [`examples/mend/TASK_FORMAT.md`](examples/mend/TASK_FORMAT.md). This continuously dogfoods the AI-first wedge (the flagship feature) and turns every diagnostic/fix gap into a backlog item: fix the compiler or open a `docs/bug-ledger.jsonl` entry, never route around it.

**Honesty rule (applies to any AI or contributor).** The Mend machine-fix *rate* is a statistic **only** over fresh, blind LLM authorship (`examples/mend/harness/mend_batch.py`, live) — a model that never saw the diagnostics. Authoring by anyone who already knows the language is biased (they won't make the known mistakes) and counts as dogfooding + gap-finding, **never** as the rate. Do not quote a machine-fix rate from non-blind authoring. Live mode needs an authenticated `claude` CLI (401s headless), so the measurement is a periodic developer-environment run, not a CI gate.
