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

**Run the clippy gate on BOTH feature legs — `--features llvm` is not a superset.** The two cfgs contain different code, so each has lints the other cannot see, exactly as `--all-targets` covers cfgs `--tests` cannot. Measured: two `src/cli.rs` helpers whose only callers sit inside `#[cfg(feature = "llvm")]` were dead in the DEFAULT build, so `cargo clippy --all --all-targets -- -D warnings` failed on a clean checkout while the `--features llvm` leg stayed green — and CI runs the default leg (B-2026-08-18-23). Run both before declaring work done:

```bash
cargo clippy --all --all-targets -- -D warnings                  # CI's leg — the one easiest to miss locally
cargo clippy --all --all-targets --features llvm -- -D warnings  # the codegen surface
```

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
# Regex archive (OPTIONAL — only for building programs that use `Regex.compile` /
# `is_match`; carries the `regex` crate). Emits the canonical name, so build it
# LAST and rename immediately (like the GPU archive). Skip unless doing regex work.
cargo rustc -p karac-runtime --release --features regex --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_regex.a
# Re-run the plain full build afterward so the canonical name is the non-regex archive again:
cargo rustc -p karac-runtime --release --crate-type staticlib
# Arrow archive (OPTIONAL — only for building programs that call `to_arrow_ipc`;
# carries the arrow-rs IPC crates). Emits the canonical name, so build it LAST
# and rename immediately (like the GPU/regex archives). Skip unless doing Arrow work.
cargo rustc -p karac-runtime --release --features arrow --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_arrow.a
# Re-run the plain full build afterward so the canonical name is the non-arrow archive again:
cargo rustc -p karac-runtime --release --crate-type staticlib
# Unicode archive (OPTIONAL — only for building programs that call
# `String.normalize(form)`; carries the ICU normalization tables). Emits the
# canonical name, so build it LAST and rename immediately (like the
# GPU/regex/arrow archives). Skip unless doing normalization work.
cargo rustc -p karac-runtime --release --features unicode --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_unicode.a
# Re-run the plain full build afterward so the canonical name is the non-Unicode archive again:
cargo rustc -p karac-runtime --release --crate-type staticlib
```

**Four archives: lean-then-full (+ wasm + wasm-threads).** `karac` links the **lean** `libkarac_runtime_min.a` for any program that references no TLS-only runtime symbol (`karac_runtime_tls_*` / `_serve_https` / `_http_client_*` / `_http_builder_*` / `_ws_accept_tls`), and the **full** `libkarac_runtime.a` otherwise; it falls back to the full archive when the lean one is absent, so building only the full archive is always correct (just no size win). The lean archive omits the `rustls`/`ring` tree (gated behind the runtime's `tls` feature; the default set is `["tls", "net"]`, and lean keeps `net`), recovering ~65 KiB on every compute/auto-par binary — see phase-7-codegen.md § "Phase 4". The **wasm** archive (`--no-default-features`, i.e. no `net` either) compiles out the whole tokio/hyper/mio/socket2 + native-scheduler/event-loop surface — none of those deps build on wasm32 — and instead carries the **sequential cooperative scheduler** (`runtime/src/seq_scheduler.rs` + `seq_par_run`, phase-10 "WASM concurrency lowering — sequential default"): `spawn()`/`TaskGroup`/`par {}` work on wasm, single-threaded, FIFO-deterministic. It is what `karac build --target=wasm_wasi` links; without it, wasm builds fail at link with a pointer to this recipe. The **wasm-threads** archive (`--target wasm32-wasip1-threads --no-default-features --features wasm-threads`) is the threaded sibling: the native pool substrate compiled for wasm (std threads are real there — pthreads over the wasi-threads ABI, futex atomics over shared memory) plus `runtime/src/wasm_threads_scheduler.rs` for the spawn/TaskGroup externs (exactly one scheduler exports `karac_runtime_*` per archive — `scheduler.rs` under `net`, `seq_scheduler.rs` on sequential wasm, `wasm_threads_scheduler.rs` under `wasm-threads`). It is the second leg of `karac build --target=wasm_browser --features wasm-threads`'s dual artifact (`<stem>.threads.wasm`); without it those builds fail at link with the recipe. Wasm-cfg clippy must be run per-target — CI's native clippy never sees either wasm arm: `cargo clippy -p karac-runtime --target wasm32-wasip1 --no-default-features` and `cargo clippy -p karac-runtime --target wasm32-wasip1-threads --no-default-features --features wasm-threads`. All commands must use `cargo rustc … --crate-type staticlib`, NOT `cargo build`. Build order matters: `--no-default-features` and the default build both emit `target/release/libkarac_runtime.a`, so build lean first and copy it to `libkarac_runtime_min.a` *before* the full build overwrites the canonical name. (`KARAC_FORCE_FULL_RUNTIME=1` forces the full archive for any program — an escape hatch if symbol detection ever misfires. `KARAC_RUNTIME=<path>` overrides resolution entirely and is honored **verbatim** — the named file is the linked file, with no lean-sibling substitution — so tests that build a feature-gated archive, e.g. `tests/park_and_wake.rs`'s `test-helpers` build, link exactly what they built.) A **fifth, optional `gpu` archive** (`libkarac_runtime_gpu.a`, `--features gpu`, spike slice-0) sits outside the lean/full axis: it is a superset of the full archive plus the wgpu/Metal backend, **auto-selected** only when the emitted object references `karac_runtime_gpu_*` (a `gpu.dispatch` program). Non-GPU builds never see it; a GPU build without it fails at link with an actionable "build `libkarac_runtime_gpu.a`" message. It stays opt-in because wgpu + naga + objc2 add ~4.5 MB to the archive and a heavy dep tree — see `docs/spikes/gpu-wgsl-slice0.md`. A **sixth, optional `regex` archive** (`libkarac_runtime_regex.a`, `--features regex`, B-2026-07-14-19) sits on the same axis as `gpu`: a superset of the full archive plus the `regex` crate (~2.5 MB), **auto-selected** only when the emitted object references `karac_regex_*` (a program using `Regex.compile` / `is_match` / `find` / `find_all` / `replace_all`). Non-regex builds never see it; a regex build without it fails at link with an actionable "build `libkarac_runtime_regex.a`" message. `karac run` (JIT) routes a regex program to the tree-walk interpreter, because the sibling `karac_jit_runner` links the runtime *without* the opt-in `regex` symbols (same pattern as `gpu.dispatch` under the JIT). A **seventh, optional `arrow` archive** (`libkarac_runtime_arrow.a`, `--features arrow`) sits on that same opt-in axis: the full archive plus the arrow-rs IPC crates (~1.6 MB), **auto-selected** only when the emitted object references `karac_arrow_*` (a program calling `to_arrow_ipc`). It backs the phase-11 Arrow IPC codegen twin — `Column.to_arrow_ipc` lowers to `karac_arrow_column_to_ipc`, which walks the Column control block and emits a stream **byte-identical to the interpreter's** (both link the same arrow-rs version; the E2E test asserts it). Non-arrow builds never see it; an arrow build without it fails at link with an actionable "build `libkarac_runtime_arrow.a`" message. `karac run` (JIT) routes an Arrow IPC program to the interpreter for the same reason regex does. An **eighth, optional `unicode` archive** (`libkarac_runtime_unicode.a`, `--features unicode`, B-2026-08-20-41) completes that axis: the full archive plus `icu_normalizer`'s compiled tables (**+161 KiB** on the archive; ~99 KiB in a stripped fat-LTO staticlib, measured against `unicode-normalization`'s 136 KiB), **auto-selected** only when the emitted object references `karac_unicode_*` (a program calling `String.normalize(form)`). It backs design.md § Strings (Equality)'s normalization-aware comparison — `s.normalize(Nfc)` lowers to `karac_unicode_normalize`, and the **interpreter links the same `icu_normalizer`** (already in `karac`'s graph via `ureq → url → idna → idna_adapter`, so the interpreter half cost zero new crates), which is what makes the two backends byte-identical by construction rather than by convention — the Arrow twin's rule. Non-normalizing builds never see it; a normalizing build without it fails at link with an actionable "build `libkarac_runtime_unicode.a`" message. `karac run` (JIT) routes a normalizing program to the interpreter for the same reason regex and Arrow do. Note that **Rust's std ships case-mapping tables but no normalization tables**, which is why `to_lowercase` needs no archive and this does. The full `Regex` surface — `compile` / `is_match` (slice 1) plus `find` → `Option[Match]`, `find_all` → `Vec[Match]`, `replace_all` → `String` (slice 2) — is now wired through codegen: the runtime entrypoints return primitive byte offsets / a malloc'd buffer and codegen owns all `Match` / `Vec` / `String` layout (slicing the subject for each `Match.text`). The only deferral left is exact regex-crate error-string parity for an INVALID pattern (codegen's `Regex.compile` Err uses a static message).

**Use `cargo rustc … --crate-type staticlib`, NOT `cargo build -p karac-runtime --release`.** The runtime's `[lib] crate-type` is `["staticlib", "rlib"]` (the `rlib` exists only for the `llvm`-feature in-process JIT test path). Under `lto = "fat"`, emitting both artifacts in one `cargo build` defeats the staticlib's cross-module DCE — std's panic/alloc-error default hooks stay reachable and the ~57 KiB DWARF backtrace symbolizer survives `-dead_strip` into *every* AOT binary (measured: auto-par floor 295.7 KiB → 417.7 KiB, +41%). `cargo rustc --crate-type staticlib` builds only the staticlib, so LTO strips the symbolizer. See the comment at `runtime/Cargo.toml`'s `crate-type` line for the full rationale.

Without this, the E2E tests (including all `tests/memory_sanitizer.rs` cases) skip with a stderr notice rather than exercise real binaries — they pass vacuously. `tests/memory_sanitizer.rs` additionally requires a `cc` that supports `-fsanitize=address`; if missing (or if `KARAC_SKIP_ASAN_TESTS=1` is set), it skips gracefully.

**A STALE archive is not a missing archive — the E2E harness now fails loudly on it (B-2026-07-28-1).** An archive built before a runtime symbol current codegen emits still *exists*, so it used to hit the same soft-skip as an absent one: linking failed, `run_program` returned `None`, and the ~960 tolerant `if let Some(out) = out { … }` call sites asserted nothing while reporting green (only the ~300 strict `assert_eq!(run(…), Some(…))` sites failed, with an uninformative `left: None` that reads like a codegen bug). `common::link_or_skip` now discriminates on the linker error: an **undefined-symbol** failure means the archive was found and lacked a symbol — always staleness (or a missing keep-list entry) — so it panics with this rebuild recipe; any other link error keeps the soft-skip. Practical rule: **`undefined reference to karac_*` from an E2E test means rebuild the archives (lean then full), not debug codegen.** A freshly-cloned cloud container inherits whatever archive the image was built with, so this bites after `main` gains any runtime symbol. The ABSENT-archive half of the hole has its own opt-in gate: **`KARAC_REQUIRE_RUNTIME_ARCHIVE=1`** turns every remaining `link_or_skip` soft-skip (archive missing, no linker) into a panic. CI's archive-building codegen/memory-sanitizer jobs set it, so a broken archive step fails the job instead of green-skipping the suite; set it locally when a run must prove it actually exercised real binaries. **Set it ONLY on a `--features llvm` run.** On the default leg `karac` has no codegen at all, so a self-host oracle's `karac build` type-checks and emits nothing — a legitimate skip that the flag converts into a panic reading `self-host oracle did not link … Build the runtime archives`, which sends you off rebuilding archives that were never the problem. The tell is buried at the end of the quoted linker output: `note: karac build requires the llvm feature; project type-checked but no executable was produced`. Measured 2026-08-29: `KARAC_REQUIRE_RUNTIME_ARCHIVE=1 cargo test` fails `selfhost_parser_matches_rust_parser` on a tree whose default suite is otherwise green, and the same test passes 12/12 without the flag. **And pair it with `--no-fail-fast`, because it also arms the OPTIONAL archives' skips.** `tests/gpu_e2e.rs`'s `gpu_or_skip` honors the same flag, so on a tree without the opt-in `libkarac_runtime_gpu.a` — which CLAUDE.md tells you to skip unless doing GPU work, and which no ordinary checkout has — the flag turns 60 legitimate skips into 60 hard failures. `cargo test` stops at the first failing test BINARY, and `gpu_e2e` sorts before `memory_sanitizer` and `par_codegen`, so the run ends without ever executing them: the two suites most likely to matter for a codegen change are exactly the ones silently not run. Measured 2026-08-30, on a change whose whole point was auto-par drop ordering. Either build the optional archives first or run `--no-fail-fast` and check that `gpu_e2e` is the only FAILED binary (CI has a separate GPU job that builds the archive, which is why its jobs do not hit this).

**A BEHAVIOUR-ONLY runtime change is the SILENT half of staleness, and the rule above does not catch it.** Everything in the previous paragraph is about the LOUD case: the archive lacks a symbol codegen emits, so the link fails and now panics. The other half never reaches the linker. A commit that rewrites what an EXISTING `karac_*` symbol does — same name, same signature, different semantics — leaves a stale archive linking perfectly and running the OLD behaviour, against a compiler that is post-fix. Nothing fails; the program is simply wrong in the way the commit was supposed to fix.

Measured (B-2026-08-27-7's session, 2026-08-27): `7c10fdf` rewrote `runtime/src/lib.rs` (102 lines added, 11 removed) for B-2026-08-26-39 — the error-return-trace buffer, global to thread-local ring — and added **zero** `#[no_mangle]` symbols. A session that had just rebuilt archives for an earlier commit checked "did any new `karac_*` symbol land?", correctly got none, and kept the archive. That archive still held the pre-fix runtime, so `7c10fdf`'s OWN regression test failed 6/6, reproducing the pre-fix measurement quoted in its row almost exactly (6 distinct traces over 40 runs vs the row's 7 over 30). The conclusion drawn was "main is red and the fix does not work" — both false, and a row asserting it was one command from being filed. Rebuilding the archives made the test pass 3/3 and all 40 runs identical. The tell was available and missed: the runs never printed the "cross-task propagation is not yet tracked" note that `7c10fdf` itself introduced, so the linked runtime visibly predated the commit under test.

**A `git archive`-BASED BISECT OF `src/` LIES BY DEFAULT, and it lies in the same shape as the archive staleness above — one level up.** `git archive <sha> src | tar -x` restores files with their ORIGINAL mtimes, which are older than the `target/` artifacts already on disk, so `cargo build` decides there is nothing to do and the next test measures the PREVIOUS binary. Every step of such a bisect then reports the state of the step before it, which reads as "the bug is present at every revision" and points at whatever commit you happened to start from.

Measured (2026-08-29, while checking B-2026-08-28-73's attribution): an eight-commit bisect returned FAILED at every revision including one whose `src` was byte-identical to a revision that had just passed. Adding `touch src/*.rs src/**/*.rs` after each extraction flipped the same checkout to passing, and the real answer was three commits away from the accused one. `git checkout -- src` does NOT have this problem (it stamps mtimes at checkout time); only archive/tar extraction does.

Practical rule: **`touch` the tree after any archive extraction, or bisect with `git checkout` instead.** The tell that you are in it is a bisect where a revision's result does not change when its content demonstrably does — check one such pair deliberately before trusting the run.

**A NON-VACUITY CHECK ON AN ALREADY-COMMITTED FIX CANNOT USE `git stash push src/` — it takes nothing, silently.** The check that a new fixture actually fails without its fix is only worth running if the tree it measures is really the unfixed one. `git stash push src/` stashes *uncommitted* changes, so once the fix is committed it stashes an empty set, exits 0, and the run measures the FIXED tree — every new fixture "passes without the fix", which reads as a vacuous test and invites deleting or weakening it.

Measured twice: B-2026-09-14-1 (caught by a determinism check — a cell that read identically before and after) and again on B-2026-09-15-5's session, where the tell was a marker count printed as a guard (`grep -c '<BUG-ID>' src/... ` → 3 and 2, expected 0). The second occurrence is why this is a rule and not a note: the failure mode is invisible in the test output itself, because passing tests are what it produces.

Practical rule: **check out the UNFIXED tree by NAME, run, then restore** (keeping `tests/` at HEAD, so new fixtures meet old code). `git checkout` stamps fresh mtimes, so it does not have the archive-extraction problem above. And **print a guard the run itself can fail on**: a `grep -c` of the fix's bug-id in each file it touched, expected zero, before the fixtures run.

    BASE=origin/main            # or the exact sha the fix sits on top of
    git checkout $BASE -- src/  # the control arm
    …run…
    git checkout HEAD  -- src/  # restore; then `git reset` — checkout STAGES

**NAME THE TREE; DO NOT SPECIFY IT BY DEPTH.** `git checkout HEAD~1 -- src/` is the spelling this rule carried until 2026-09-20, and it is correct for exactly as long as HEAD *is* the fix commit — which is the first few minutes of its life. Close the row and HEAD becomes the ledger commit, so `HEAD~1` is the fix and the "control" arm carries it. Rebase and the same thing happens with other people's commits underneath. A control specified as "one back from wherever I am" drifts on every commit and every rebase, silently, and the script that contains it keeps running.

The failure it produces is the worst shape available: BOTH arms carry the fix, both measure clean, and the run reports a fixture that passes with the fix and without it — which reads as a VACUOUS FIXTURE and invites deleting the test that had just proved itself. Measured 2026-09-20: a session's marker column caught it and exited 3, but only by luck of ordering — that column exists to prove the apparatus is SET, and here it happened to also catch it being MIS-SPECIFIED. Nothing else in the run could have.

**AND THE RULE IS THE REFUSAL, NOT THE SPELLING.** A named tree is one way to satisfy it; a control that FAILS CLOSED is the other, and either alone is enough. One working form stashes a single path and then requires a MARKER COUNT OF ZERO in the stashed tree, exiting 3 with `REFUSING: stash took nothing` — under exactly the drift above, where the fix is already committed and the stash therefore takes nothing, it refuses instead of reporting a vacuous pass. What separates both from the depth form is that neither can quietly become the test arm. A named-tree control with no refusal in it is not covered: it just fails later and for a different reason.

WHY THE WRONG SPELLING SURVIVED IN THIS FILE FOR AS LONG AS IT DID, which is the part to recognise in your own scripts: the depth form is correct in the narrow window everyone TESTS it in — a fix committed alone and measured immediately, where HEAD really is the fix — and wrong in the window everyone actually WORKS in, because this workflow commits a ledger row on top of the fix every single time. Advice that is true exactly when you check it and false whenever you rely on it will not be caught by checking it.

A named tree is also the only spelling that can express the question you usually want once a family has more than one fix in flight: *does this corner still fail with `<some other fix>` present but WITHOUT the fix under test?* That is a question about a particular tree, and no depth can phrase it.

**AND WHATEVER FORM THAT CHECK TAKES, IT LEAVES THE APPARATUS SET TO THE CONTROL — the tree is fixed and the BINARY is not.** Every staleness rule above is about an artifact OLDER than the tree. This one is not: `target/debug/karac` is current, freshly linked and correct, and simply built from the other arm of the experiment. `git status` is clean, the source carries the fix, and every reflex says you are on the fixed tree — which is exactly why nothing catches it. It is not a property of `git stash`: **`git checkout HEAD~1 -- src/`, the form this file recommends two paragraphs up, has it identically (and see the naming rule above before reaching for that spelling at all).** Any experiment that swaps the tree and swaps it back leaves the artifact built from whichever arm was checked out LAST.

**`cargo test --test <name>` looks like the rebuild and is not.** A filtered run builds that test target and its deps, and the dep is the karac LIBRARY — `tests/codegen.rs` and `tests/memory_sanitizer.rs` both compile in process (`karac::codegen::compile_to_object_with_options` + `link_executable`), never shelling out. So the exposure is narrower and sharper than "anything run afterwards": **in-process fixture results are SOUND under a filtered run; `.kara` cells executed through `target/debug/karac` are NOT**, because nothing in that dep graph reaches the standalone bin. Unfiltered `cargo test` does rebuild the bins, which is why this stayed hidden — fixtures are what sessions run most, and they were never wrong. When you discover you were in the window, that split tells you which measurements to re-run instead of all of them.

Measured 2026-09-20 (B-2026-09-19-43): a fix's own bug row reproduced its ORIGINAL failure against a compiler believed to be fixed, and a follow-up width grid then failed at all three widths including a struct structurally identical to a cell watched passing an hour earlier. Both measurements were real, reproducible, and about the control binary; the conclusion drawn was "the fix is partial", and a row saying so was minutes from being written. Two other sessions audited their own scripts within the hour and found the same defect in three more.

Practical rule: **rebuild after the restore, and gate the measurement on TWO refusing columns rather than one printed warning.** They catch different failures — a behavioural fix-presence cell cannot see cargo deciding there is nothing to do when the bin happens to be correct for an unrelated reason, and an mtime assertion cannot see a bin built correctly from the wrong tree:

```bash
git checkout HEAD -- src/ && cargo build --features llvm      # THE REBUILD, not optional
[ src/<the file you changed> -nt target/debug/karac ] && exit 3   # mtime column
./target/debug/karac build canary.kara -o /tmp/c && /tmp/c | grep -qx 'dK2' || exit 7   # fix-presence column
```

Make the canary its OWN one-statement program rather than one of the cells under test, so it cannot come out right for a reason unrelated to the fix, and have it EXIT rather than print. A printed column is something a reader has to notice: in the measured case a loud contradiction was available — two cells of the same shape disagreeing — and the rule was still only reached for because it was loud. A cheap grid with no internal contradiction goes unchallenged.

**THE SAME SHAPE ONE STEP OVER: A TEST FILTER THAT MATCHES NOTHING.** `cargo test <filter>` with a filter that names no test prints `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1709 filtered out` and **exits 0**. Beside a real fixture's `1 passed`, a reader scanning for green sees two `ok` lines; the tells are `0 passed` and the filtered-out count, neither of which is where the eye goes. The mechanism here is that a fixture's LABEL — the string inside its `assert` call — and its test FUNCTION NAME are different strings, so filtering by the one you were just reading matches nothing. Measured 2026-09-20, on the check that exists to prove a keep-both-sides fixture merge did not damage another session's test: exactly where a silent zero-match is worst, because the green light certifies having checked nothing on the resolve most likely to be wrong.

Practical rule: **assert the pass COUNT, not the exit status** — `cargo test <filter> 2>&1 | grep -q '1 passed'`, or run unfiltered. And note the family: a stale binary is the apparatus set to the CONTROL, an empty filter is the apparatus pointed at NOTHING, and a vacuous fixture is the apparatus measuring a value nothing observes. All three are one failure — an instrument silently reporting nothing, read as reporting a negative — and all three exit 0.

**Practical rule: rebuild the archives whenever `runtime/src` changes AT ALL, not only when the symbol set does.** Symbol presence is necessary, not sufficient. The check before trusting any AOT measurement is a diff, not an `nm`:

```bash
git log --oneline <sha-the-archives-were-built-at>..HEAD -- runtime/src   # non-empty ⇒ rebuild, lean then full
```

**"The archives" means ALL of them, the OPT-IN ones included, and that needs saying because the sentence above sits next to the lean/full recipe and reads as if it were only about those two.** The `regex` / `arrow` / `unicode` / `gpu` archives are the ones this rule actually fails on: CLAUDE.md tells every session to skip building them, so nobody rebuilds them, and a container where somebody built them ONCE keeps serving that build for as long as it lives. Measured 2026-09-16 (B-2026-09-16-4): both ASAN legs reported 1641/1641 green with six fixtures linking regex/arrow/unicode archives five days old, across three `runtime/src` commits — one of them a pure behaviour change. A container that never built them is SAFER, because those fixtures are then honestly skipped. **`scripts/asan-o0-leg.sh` now checks this** — it compares each linkable archive's mtime against the commit time of the last `runtime/src` change and exits 3 with the list, so the two legs no longer need the rule to be remembered (`KARAC_ALLOW_STALE_ARCHIVE=1` overrides; stale `_wasm*` archives are reported but not fatal, since no native fixture can link one). Nothing else in the tree checks freshness: `link_or_skip` discriminates on `undefined symbol`, which is the LOUD half only.

The same applies to `karac_jit_runner`, for the same reason and with the same silence — it statically links the runtime too, so a behaviour-only change leaves `karac run` executing old semantics with no missing-symbol error to give it away.

**Leak detection: the Linux-CI `memory-sanitizer` job is the authoritative gate, not local macOS.** `-fsanitize=address` runs **LeakSanitizer on Linux but NOT on macOS** — so a local `cargo test --features llvm --test memory_sanitizer` on a Mac catches use-after-free / double-free only, and **silently misses leaks**. The CI `memory-sanitizer` job (ubuntu, [`docs/spikes/ci-test-coverage.md`](docs/spikes/ci-test-coverage.md) Tier 2) runs the same suite *with* LSan, so it is the comprehensive, automatic leak gate for the whole codegen-ownership class — strictly better than the older manual one-at-a-time `leaks --atExit` spot-check. Practical rule: **do not conclude "no leak" from a green local Mac asan run**; consult the Linux CI job (or run the suite under a Linux/LSan toolchain) before trusting a leak-class fix. CI now also runs the `--features llvm` codegen E2E + self-host oracle (`codegen-e2e`) and the wasm clippy/archive gates (`wasm`) — see the spike for the full tier map; the once-true "CI runs no `--features llvm`" assumption is obsolete. **Leaks are NOT always architecture-independent.** B-2026-07-12-29 *was* an arm64-ONLY RC leak (a compound index-assign of a shared/`Option[shared]` Vec element — `work[i] = work[j]` — balanced on x86, leaked on arm64; fixed in `63d618e`) — the proof-of-existence that an x86-green `memory-sanitizer` run does NOT clear the arm64 leak surface. A second **`memory-sanitizer-arm64`** job (ubuntu-24.04-arm, non-required) now runs the same LSan suite on arm64 as a PERMANENT gate for that class; a green x86 CI run does NOT by itself clear an arm64 leak — cross-check the arm64 leg (or `scripts/lsan-local.sh`, which is native arm64) before trusting a shared-Vec / index-assign leak-class fix.

**The two ASAN ratchet legs are a PRE-PUSH step for any change that adds or touches a memory-class fixture, and nothing else runs them.** `scripts/asan-o0-leg.sh` re-runs the whole `memory_sanitizer` suite at `KARAC_OPT_LEVEL=0` and gates it against `tests/asan-o0-known-failures.txt`; `scripts/asan-instrumented-leg.sh` is its sibling. Neither is part of the default leg, the `--features llvm` leg, or the clippy legs — so a fixture can be written, pass every gate in the Commands block above, and still redden a required leg on `main`. That has now happened twice: B-2026-09-01-10 found the leg red with several hundred commits landed against a gate nobody turned, and B-2026-09-10-12 found it red within the hour of a fixture landing. Run both before pushing such a change:

```bash
bash scripts/asan-o0-leg.sh          # whole suite at -O0, ratcheted against the known-failures list
bash scripts/asan-instrumented-leg.sh
```

**There is a THIRD leg, `scripts/asan-sso-leg.sh`, and it is deliberately NOT on that pre-push list** (B-2026-09-16-35). It runs the same suite with Small-String Optimization on (`KARAC_SSO=1`) against its own `tests/asan-sso-known-failures.txt`. It needs to exist because `KARAC_SSO` is read once per process through a `OnceLock` defaulting to OFF and `tests/memory_sanitizer.rs` compiles every fixture IN-PROCESS, so no fixture in that file can turn the feature on for itself — until this leg, every one of the ~1660 ASAN cells exercised the non-SSO String paths only, including the heap allocation codegen has owned since B-2026-09-16-1 while the free stayed in the runtime. It is excluded from the pre-push set because SSO is still STAGED behind a default-off flag: a red run there is a defect in the unfinished SSO surface, not a regression on the shipping default. Run it when touching `src/codegen/sso.rs`, the String read surface, or anything that allocates a String buffer; the default flip is what promotes it. CI runs it as the non-required `memory-sanitizer-sso` job.

**And note what seeding that leg did NOT establish, because the same trap is one level up from every rule in this section.** The lane was green on a clean tree on its first run (1673 passed / 0 failed), and that is no evidence it can see anything. Reproducing the fault its filing row measured — a range-slice result aggregate pointed into the SOURCE buffer instead of its own allocation, a guaranteed double free — the whole `asan_string_*` set stayed green at `KARAC_SSO=1` as well as at `=0`, because the heap route is only entered by a slice longer than the 23-byte inline capacity and no fixture made one. A new lane can be vacuous in exactly the way a new fixture can. **So pair every new lane with a fault injection that proves it reports**, the same non-vacuity discipline the named-tree control rule above asks of a fixture.

**Both legs now REQUIRE the runtime archives, and that is load-bearing rather than tidy** (B-2026-09-13-8). `asan-o0-leg.sh` sets `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` itself (pass `KARAC_REQUIRE_RUNTIME_ARCHIVE=0` to opt out), and the instrumented leg inherits it by exec. Without it the authoritative pre-push gate could not tell "skipped" from "passed": on a tree with no archives every fixture soft-skips through `link_or_skip`, and the leg printed `test result: ok. 1613 passed` in 60 s and exited 0 — measured 2026-09-14 with the archives moved aside. The old safety net was the ratchet's other arm ("a quarantined fixture started passing"), and **it has decayed to nothing**: both quarantine lists are fully drained (0 live entries), so an empty `got` against an empty `expected` matches exactly. That decay is silent and gets worse as the lists shrink, which is the opposite of what a ratchet should do. The fixtures that need an OPT-IN archive are subtracted and reported as SKIPPED rather than failed, so the flag does not fail the leg on an ordinary checkout — 6 of 1617 on 2026-09-14 (4 regex, 1 arrow, 1 unicode) and 5 on 2026-09-20 (no unicode skip) — the count is a MEASUREMENT OF ONE TREE and drifts as fixtures and archives land, so read the SKIPPED list the leg prints rather than any number written here; the leg detects them by archive FILENAME and needs no count — that carve-out is why the flag could not simply be exported by the caller.

**CHECK THE LEG'S ARITHMETIC, BECAUSE "matches the quarantine list exactly" ALSO PRINTS WHEN NOTHING RAN.** Both quarantine lists are drained, so an empty `got` matches an empty `expected` and the ratchet's other arm reports success over a run of zero fixtures — the same decay described above, seen from the leg's output rather than from the list. The identity that distinguishes a real pass accounts for every fixture in the file:

    passed + failed + skipped-for-a-missing-opt-in-archive + ignored
      == `grep -c '^    #\[test\]' tests/memory_sanitizer.rs`, ON THE TREE THE LEG RAN AGAINST

**THE TWO SIDES ROT DIFFERENTLY, so state it as a grep and never as a number.** The left is read off the leg's own output every run and cannot go stale. The right is a property of ONE FILE ON ONE TREE and moves whenever any thread lands a fixture, including threads whose work has nothing to do with yours. So the rule is not `1696 + 5 + 9 == 1710`; it is *the leg's three numbers sum to a grep taken on the same tree the leg ran against*. Written the first way it is a recalled number wearing an equation — see the recalled-versus-measured rule elsewhere in this file — and the difference is invisible for exactly as long as both happen to be true.

**ITS LIMIT, which has to be stated or a closed identity reads as proof the suite means something:** it proves no fixture was silently ABSENT from the run. It says NOTHING about whether any fixture was VACUOUS. A fixture that compiles a program observing nothing counts as passed and closes the arithmetic perfectly. It is the executed-assertion check at FILE scope rather than at filter scope, with the same boundary.

**AND THE LEG'S OWN VERDICT LINE LIES TO A GREP**, which is why the identity and not the word is what to key on. The raw line reads `test result: FAILED. 1696 passed; 5 failed` — those 5 are the opt-in-archive fixtures the leg subtracts and then names, so the leg is GREEN and the word says FAILED. A reader grepping for `FAILED` gets a red that is not one; a script keying on it gets the expensive error. Measured 2026-09-20 on one such run: `1696 + 5 + 9 == 1710`, against 1710 `#[test]` in the file on that tree — `main` carried 1709 at the time and the leg's tree had one unlanded fixture, which is exactly why the count has to be taken from the tree the leg ran on rather than from a number written down here. A file-level identity proves the FILE ran; it says nothing about whether YOUR cell did. Only the named `<test path> ... ok` line proves that, so a run that added a fixture asserts both. Its virtue over comparing against the quarantine list is that it degrades in the SAFE direction as the lists drain rather than becoming more vacuous as they do, and it fails loudly on the one thing the comparison cannot see — fixtures that were silently absent from the run.

The `-O0` leg is where a leak-class cell is actually measured — at `-O2` LLVM deletes allocations nothing observes, so an `-O2`-only zero is evidence of nothing. **But note the converse failure mode, which is not about coverage at all** (B-2026-09-10-12): that leak WAS measured at `-O0` by its author's own valgrind sweep, printed as `definitely lost: 48 bytes in 1 blocks`, and missed because a 15-cell instrumented log was summarized with `grep`/`tail` rather than a per-cell verdict. When a probe sweep covers many cells, have it print one unambiguous PASS/FAIL line per cell — a filtered view of a long log can crop the one block that mattered.

**Embedded-component wasm tests additionally need `wasm-tools`** (`cargo install wasm-tools` or `brew install wasm-tools`): `--bindings component` — the `wasm_wasi` default — shells out to it for componentization (phase-10 embedded-WIT migration; design.md § Component Model emission). Without it, the embedded-component E2E tests in `tests/cli.rs` skip with a stderr notice (same vacuous-pass caveat as the archives). The wasi preview1 adapter is vendored in karac itself (`wasi-preview1-component-adapter-provider` crate) — no extra setup.

**`karac run` (the default JIT executor) needs the SIBLING `karac_jit_runner` binary rebuilt too — `cargo build --bin karac` alone leaves it STALE.** `karac run` defaults to LLJIT (LLJIT-productionization Slice 6c; `--interp` forces the tree-walk backend), and it executes by spawning a *separate* one-shot `karac_jit_runner` binary (its own `[[bin]]` target, `src/bin/karac_jit_runner.rs`) that statically links the runtime and hands its `#[no_mangle] karac_*` symbols to the JIT via `dlsym`. Because it is a distinct bin, `cargo build --features llvm --bin karac` does **not** rebuild it — so after you add or touch a runtime symbol (anything in `runtime/src/`), a `--bin karac`-only rebuild pairs your fresh `karac` with a **stale runner** whose symbol table predates your change, and `karac run` then fails cryptically with `JIT session error: Symbols not found: [ karac_<sym> ]` (even on `hello world`, if the missing symbol is one every module declares — e.g. `karac_critical_section_release`) while `karac build` (AOT, links the archive) and `karac run --interp` both work. This is a build-staleness footgun, NOT a compiler bug: **before trusting `karac run`, rebuild BOTH bins** — `cargo build --features llvm` (no `--bin` filter), or explicitly `cargo build --features llvm --bin karac --bin karac_jit_runner`. (`cargo test --features llvm` also rebuilds all bins, which is why the failure evaporates after a test run.) A JIT symbol that AOT resolves but the runner can't usually means either a stale runner (rebuild) or a genuine missing `__preserve_no_mangle_symbols` keep-list entry (`runtime/src/lib.rs`, the `karac_realloc_or_panic` / B-2026-07-12-22 class) — check the runner's freshness FIRST.

## Long-running verification: don't poll it, and sweep what you started

The full gate set here runs 25–40 minutes (`cargo test`, then the `--features
llvm` suite, then four clippy legs), which is long enough that the natural move
is to write a wait loop. Don't. **When a harness-tracked background command
finishes, the session is re-invoked automatically**, so an `until … sleep` loop
adds nothing — and it is the thing that gets stranded. Start the run in the
background and let the notification arrive. (A foreground run is not the
alternative: these cycles exceed the 600 s tool timeout and get backgrounded
anyway.)

**Never scan for a process by a string that also appears in your own command
line.** `pgrep -f "cargo test"` matches the shell running the very loop that
contains it, so the condition never goes false and the waiter polls until the
container is reclaimed. The `pkill` form is worse: `pkill -f "cargo test"`, used
to cancel a gate cycle, killed the issuing shell along with it (exit 144).
Measured 2026-09-04 — one session finished its work with three such loops still
running, two of them self-matching.

If a wait is genuinely unavoidable, poll a **sentinel the command itself writes
as its last action** (`echo "ALL GATES DONE" >> gate.log`) — a string that exists
only on completion and does not appear in the waiter. That has one failure mode
worth knowing: if the run is CANCELLED the sentinel never lands, so its waiter
strands forever. Killing a gate cycle means stopping its waiter too.

**Sweep before declaring the work done.** `pgrep -af 'sleep|until'`, or `TaskStop`
on the ids the task list reports. Stragglers do not touch the tree, but they keep
the session looking busy indefinitely and they burn CPU on the same box the gate
timings are measured on.

## A red gate with no named test is probably a FULL DISK, not a failure

The writable disk in a cloud session is a **per-session allowance of ~38 GiB**,
and `df -h /` actively misleads about it: the `252G` size column is the host
volume, so the number to read is `used + avail`. When that allowance runs out
mid-gate, the failure NEVER says "disk" in a form that survives a
`grep -E 'FAILED|test result'`. The four shapes, all measured in one session
(B-2026-09-09-7):

    rustc-LLVM ERROR: IO failure on output stream: No space left on device
    collect2: fatal error: ld terminated with signal 7 [Bus error]
    error: failed to write file .../dep-graph.part.bin: No space left on device (os error 28)
    (nothing at all -- the harness tmpdir fills, the child's output is lost, exit code only)

The `signal 7` form is the linker's mmap'd write failing on a full filesystem
and it prints LLVM's `PLEASE submit a bug report` banner, which sends you to the
wrong project. The fourth reads as a hang or a flake.

**Why it lands on the SECOND gate leg.** One leg of `cargo test --no-run` --
pure linking, no tests run -- costs **19.6 GiB**: 134 test executables at ~250 MiB
each under cargo's default `debug = 2`, plus a 420 MiB `libkarac` rlib. That
leaves ~700 MiB, so the second feature leg cannot start. **Clippy is not the
culprit** even though it looks like one: both clippy legs together cost 1.19 GiB,
because clippy emits metadata rather than linked binaries. A session that
blames clippy here will "fix" it by serialising the wrong commands.

Either of these makes the gate set fit:

```bash
CARGO_PROFILE_TEST_DEBUG=line-tables-only cargo test --features llvm   # 252 -> 97 MiB per binary
cargo clean -p karac    # ~20-24 GiB back in seconds; run between feature legs
```

`line-tables-only` keeps file/line in panic backtraces and drops only the type
and variable DWARF. **Check `df` before concluding anything from a red gate** --
a gate that failed with no named test may not have run one.

**`scripts/disk-guard.sh` now does this checking for you, so the rule above no
longer has to be remembered** (B-2026-09-09-7, the same argument that put the
archive-freshness check into the ASAN legs). Both ASAN legs call it at three
points -- an advisory `preflight` that logs the TRUE allowance before the leg
spends it, and `classify` on the two failure paths. Run it around your own gate
legs too:

```bash
bash scripts/disk-guard.sh allowance            # used+avail, ignoring df's host 'size'
bash scripts/disk-guard.sh preflight 8 "llvm leg"
bash scripts/disk-guard.sh classify gate.log    # exit 4 = this red was disk
```

`preflight` is deliberately ADVISORY and always exits 0: a hard refusal would
turn a tight-but-workable box red, which is a worse failure than the one it
guards against. Its job is to get the real number into the log, because `df`'s
size column is the host volume and reading it is what invites the wrong
conclusion.

**There is a FIFTH shape the four above do not cover, and no signature can find
it.** A full disk can fail NAMED tests with ordinary-looking assertion diffs and
NO disk message anywhere -- the tests that write files fail first. Measured
twice: the `BufReader` cluster in `tests/interpreter.rs` (B-2026-09-09-5), and
on 2026-09-16 a `git_fetch` / `registry_proxy` cluster whose count assertions
read `left: 0 right: 1`, where the gate script had freed 23 GiB immediately
before the leg and the leg's own linking spent it. Both look exactly like
load-sensitive flakes, and both pass in isolation, which is what makes them
convincing. So CLAUDE.md's "no named test" heuristic is necessary but not
sufficient: the other tells are WHICH tests failed (file-writing ones, with no
path to the change under test) and reading `df` **after** the failing leg rather
than before it, since the leg is what consumes the space. `classify` checks free
space for exactly this reason and SUSPECTS rather than asserts when it fires.

**On a freshly REBASED tree that fifth shape becomes an accusation.** The
tests that fail are unrelated to your change, the tree just gained someone
else's commits, and the obvious reading is that one of them regressed you --
so the thread pays for a bisect against a commit that did nothing wrong. This
is likeliest exactly when it is most expensive: several sessions pushing
means frequent rebases, and a rebase is usually followed by a re-verification
leg, which is what spends the allowance. Before attributing a post-rebase red
to the commits you rebased onto, read `df` (or `scripts/disk-guard.sh
allowance`) AFTER the failing leg, and re-run the named failures in isolation
-- they pass, which is the tell rather than the exoneration.

## Branch management

**Two environments, two workflows — pick by where the session runs.** The worktree rules in the rest of this section govern the **local multi-worktree checkout** (the primary machine, where sibling worktrees run parallel slices and the primary's clean `git status` is load-bearing). They do **not** apply to an **ephemeral cloud container** (Claude Code on the web / a fresh clone discarded when the session ends): there are no sibling worktrees, no parallel slices, and nothing to isolate from, so `EnterWorktree` + a feature branch buys nothing but ceremony. In a cloud container, **work directly on `main`** — commit straight to `main`, no feature branch, no PR unless explicitly asked (owner-authorized 2026-07-07, overriding the mandatory-worktree default below for this environment only).

The one discipline that carries over regardless of environment: **`main` advances from other sources mid-session** — other cloud sessions, teammates, bots. This repo has seen a sibling commit (`docs(mend)`) land on remote `main` *during a single task*. So working "directly on `main`" is not "ignore the remote":

- **Before starting a slice AND before every push**, sync local `main` to the remote: `git fetch origin main`, then `git reset --hard origin/main` if you have no local-only commits yet, or `git rebase origin/main` to replay your commits on top if you do.
- **Push with `git push origin main`**; retry transient network errors with 2s/4s/8s/16s backoff. A push rejected as **non-fast-forward** means the remote advanced after your last fetch — `git fetch` + `git rebase origin/main`, then retry. **Never** `push --force` / `update-ref` past commits you did not create (the silent-rewind footgun in failure mode 2 below); rebase onto them instead.
- **Deleting the working branch**, when one exists, is a UI/one-click step for the owner — the cloud git gateway returns `403` on ref deletion and the GitHub MCP server exposes no delete-branch tool, so it cannot be done from inside the session. Delete the *local* branch after confirming it is fully contained in `main` (`git merge-base --is-ancestor <branch> main`), and flag the remote ref for the owner to remove.

**On the local checkout, all dev work in karac-rust happens in an isolated worktree, not on the primary `main` checkout.** Every implementation slice — feature, fix, refactor, even single-line bug fixes — starts with `EnterWorktree` (which honors `.claude/settings.local.json`'s `worktree.baseRef: "head"`, so the new worktree picks up local-but-unpushed `main` commits). **Before `EnterWorktree`, always sync the primary's `main` with the remote first** — `git fetch origin main && git merge --ff-only origin/main` from the primary. (`git pull --ff-only` is the same two commands; this file spells out the pair everywhere so there is ONE sync verb to look for, and so the cloud-container rule above — where `pull` is wrong, because the follow-up is `reset`/`rebase` rather than a merge — cannot be read as contradicting this one.) `baseRef: "head"` only inherits *local* `main` commits; it does **not** pull commits another session, a teammate, or a bot pushed to remote `main`. Branch off a stale local `main` and the worktree forks from behind, forcing an avoidable `git rebase` (and the fork-point confusion it invites) at integration time — this repo has had remote `main` advance mid-task, so the pull is not optional. Commit inside the worktree, then `git rebase main` from the worktree and `git merge --ff-only <branch>` from the primary to integrate. Direct commits to `main` from the primary checkout are reserved for pure recovery operations (the `update-ref` failure-mode dance below) — never for normal feature/fix work, even if "it's just two lines."

Why mandatory rather than judgment-call: the primary worktree's role is review, cross-referencing, and integration. Mixing in-progress work there contaminates `git status`, blocks parallel slices, and skips the rebase-loud-fail signal that catches stale fork-points (the same signal that prevents the silent-rewind footgun in failure mode 2 below). Worktree isolation makes "what's on main" and "what I'm currently doing" structurally separate, which is what every other rule in this section relies on.

The kara-katas repo is a different story — it's a content repo, not the compiler, and direct commits to its `main` are fine.

**Always update `main` via `git merge --ff-only` from the primary worktree.** Cross-worktree `git update-ref refs/heads/main <source-tip>` bypasses git's "checked-out branch can't be ff'd" safety net and has two known failure modes — both have hit this repo:

1. **Stale primary worktree.** The primary worktree's index and working tree don't refresh after the ref moves; subsequent `git status` there renders the just-landed commit as "uncommitted changes" (the inverse diff of what was shipped). Recovery: `git stash push` clears the false diff in one step. Detailed reproduction in the user's memory at `reference_update_ref_stale_primary_worktree`.

2. **Silent main rewind.** If the source branch's history doesn't include the current main tip (e.g. branched off main before another feature merged), `update-ref` overwrites main and the commits between the source's fork point and the previous tip become orphans — still in the reflog (default 90-day retention) but invisible from `git log main`. Recognize by `reset: moving to HEAD` reflog entries with no source SHA in the action column. Recovery: identify the previous tip from `git reflog main`, `git update-ref refs/heads/main <previous-tip>`, `git reset --hard` to sync the worktree, then cherry-pick anything that was on the rewound branch. Save uncommitted state to a patch first if `reset --hard` is involved.

`git merge --ff-only <branch>` from the primary worktree avoids both: it refreshes index+worktree atomically and rejects non-fast-forward updates loudly. If the ff is rejected, the source branch needs `git rebase main` before retrying — never reach for `--no-ff` or `update-ref` as a workaround.

**Prefer rebase + ff over cherry-pick when integrating a side branch.** `git rebase main` from inside the side branch's worktree, then `git merge --ff-only <branch>` from the primary, preserves the side branch's identity — its tip ends up on main's history with the same SHA, so a subsequent `git branch -d <branch>` (the *safe* form that refuses to delete unmerged work) succeeds cleanly. Cherry-pick produces a content-equivalent commit with a fresh SHA; main then has the patch but the side branch's tip is orphaned, forcing `git branch -D` (force-delete) and leaving the original SHA reachable only via the reflog. Reserve cherry-pick for cases where no live branch ref exists — recovering a single commit from a deleted branch or from an orphan SHA in the reflog. The 2026-05-20 recovery used cherry-pick for one such reconstruction; for any future rewind recovery, prefer `git rebase <restored-main> <orphan-branch>` followed by ff if the source branch is still around.

## Claiming a bug (multi-session coordination)

Several agent sessions work this repo in parallel, and the open rows of `docs/bug-ledger.jsonl` are the shared work queue. **Two sessions picking the same row is the default outcome unless one of them claims it first.** The claim must not be a commit — a commit-to-claim scheme costs a push per pick, races on `main`, and leaves the ledger churning with rows that flip to in-progress and back. It lives in **Claude Code Remote session metadata** instead: outside the repo, visible to every session on the account, and self-expiring.

**Claim before reading the bug in depth, not after.** The trigger is "I am about to pick an open ledger row to work on" — *including* when the user names a specific bug id, since another session may already hold it.

1. **Sync the ledger, before you read it.** `git fetch origin main`, then
   `git reset --hard origin/main` (or `git rebase origin/main` if you have
   local-only commits). Branch management already requires this "before
   starting a slice", and it is a NUMBERED STEP here because that is the only
   form sessions actually follow — several have picked a row off a stale
   `open` set, which means a row someone closed minutes ago or a set missing
   everything filed since the last fetch. Unconditionally, not "if the ledger
   moved": you cannot know that without fetching, and a fetch costs seconds
   against work that runs for tens of minutes.
2. **Read the board.** `list_sessions({mine: true, limit: 30})` on the `claude-code-remote` MCP server (load the schema via ToolSearch if it isn't in context). Each row carries `tags`, `title`, `session_status`, `updated_at`, and `post_turn_summary`; collect the `kara-bug:<ID>` tags. A tag on a `RUNNING` session — or on an `IDLE` one whose `updated_at` is recent — is a **live claim: pick a different row**. A tag on an `ARCHIVED` session, or one stale by a day or more, is a dead claim and the row is free.
3. **Stake it.** `get_session()` with no arguments returns your own session id; then `set_session_tags({session_ids: ["<own id>"], add: ["kara-bug:B-2026-08-17-29"]})`. Mirror the id into the title as well (`set_session_title`, e.g. `"bugs group D · B-2026-08-17-29"`) so the claim is legible in the web session list without anyone reading tags.
4. **Re-read once.** List again after tagging. If another session tagged the same id inside that window, **the session with the older `created_at` keeps it**; the younger removes its tag and picks another row. Deterministic, no negotiation, no message round-trip.
4½. **Re-read the board at the pre-push fetch, not only at pick time.** Step 2
   is read ONCE, when you pick, so two sessions whose pick times straddle a
   claim cannot see each other — the session that picked FIRST holds no tag and
   is invisible to the one that claims SECOND, and the `created_at` tiebreak in
   step 4 resolves simultaneous *tags*, not an untagged session. That is not
   hypothetical: B-2026-09-13-8 records two sessions independently fixing
   B-2026-09-12-25, the second landing during the first's gate cycle. So list
   the board again at the `git fetch` every push already performs, and treat
   another session's tag on the row you are about to close as
   stop-and-coordinate rather than a race to push. One tool call, at the one
   moment both sessions are guaranteed to look.
5. **Release on close.** Drop the tag (`set_session_tags({remove: [...]})`) once the closing commit lands. That commit's `status: "fixed"` is the real release — the tag is only the in-flight signal, so a session that dies mid-fix leaks nothing.

**Re-read the row itself before closing it — a claim does not freeze it.** The
tag stops another SESSION from picking the row up; it does not stop the owner,
or a human, from editing that row while you hold it. Measured 2026-09-01:
`8c152fc` added two new measurements to B-2026-09-01-38 about an hour after it
was claimed, one of which asked a scope question the in-flight fix had already
answered. Closing from the copy you read at claim time would have written a
`fix` that ignored them and left the row's own open question unanswered in its
closing prose. Step 1's fetch cannot help here — the edit lands mid-flight — so
`grep '<BUG-ID>' docs/bug-ledger.jsonl` again after the final rebase, when you
are reading the fix SHA back out of `git log` anyway.

**Why metadata and not a git ref.** The better mechanism would be a custom ref: `git push origin <existing-sha>:refs/claims/<BUG-ID>` pushes zero new objects, and git's ref update is a server-side compare-and-swap — a genuine atomic lock. **The cloud git gateway rejects it with `HTTP 403` on any ref outside `refs/heads/main`** (measured 2026-08-17), and the same goes for notes and orphan branches. Session metadata is the fallback *because* the atomic option is unavailable; don't spend a session re-deriving that.

**Known limits — this is a convention, not a lock.** `set_session_tags` has no compare-and-swap, so step 4 (the re-read) closes a one-round-trip race window by agreement rather than by construction; that is acceptable against work sessions running tens of minutes, and the `created_at` tiebreak is what makes a collision recoverable rather than silent. The protocol cannot be wrapped in a `scripts/` helper — bash has no MCP access, so these are tool calls the agent makes directly. `mine: true` scopes to a single account, so outside contributors are not covered. If the `claude-code-remote` tools are absent in a given environment, skip the protocol and say so in the first reply rather than silently working an unclaimed row.

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
- **Ownership tiers:** owned (default) → `ref` → RC. Parameter modes are always declared at the signature — bare `T` is owned, `ref T` / `mut ref T` / `mut Slice[T]` are explicit borrow forms; bare `self` / `ref self` / `mut ref self` follow the same rule for receivers. Body-level ownership analysis is a checking aid (verifies usage matches the declared mode, drives `karac explain` "would-be mode" diagnostics, feeds use-site classification for the RC fallback pass) — it is not a signature-derivation mechanism. **One contained exception feeds codegen:** the RC-elision hint (`OwnershipCheckResult::elidable_ref_params`, `src/rc_elide.rs`, default ON since B-2026-07-15-21) — the set of read-only, non-escaping `ref`-classified `shared`/`Option[shared]` params whose balanced retain/release codegen may skip (a 17–32% win on read-only tree walks). It is a plain-data hint computed in `ownership.rs` and consumed via the existing `borrowed_arg_skip` / `borrowed_param_dec_skip` channel — no LLVM type crosses the boundary, so codegen-containment holds. Opt out with `KARAC_RC_ELIDE_REF_PARAMS=0`; see [`docs/spikes/rc-elide-ref-params.md`](docs/spikes/rc-elide-ref-params.md).
- **Call-site mutation markers:** free-function calls write `mut` on arguments whose place-expression root is a fresh owned binding (or a temporary / literal / function return) when the callee's parameter is `mut ref T` / `mut Slice[T]`. Arguments rooted at a `mut ref` binding already in scope forward without marking. Method calls, field assignment, and index assignment never mark. `ref` is never legal at call sites. See design.md Feature 4 Part 1½.
- **`shared struct`/`shared enum`:** reference-semantics types using RC.
- **Layout blocks:** separate logical struct definition from physical memory layout (SoA, field grouping for cache locality).

## `Map` / `Set` iteration order is random per process — never assert it

`Map` and `Set` hash through **SipHash-1-3 under a per-process random key**
(design.md § `Hash` and `Hasher`, "Default hasher for v1"; B-2026-08-21-6). The
one shared implementation lives in the **`karac-hash` crate** (`hash/`) — the
interpreter calls it directly, the compiled backends reach it through
`karac_hash_bytes` in `karac-runtime` — so the two backends agree by
construction, the same rule the Arrow IPC twin and `String.normalize` follow.

The consequence is a test-writing rule with teeth: **iteration order differs
between two runs of the same binary**, so an assertion that pins it fails on
most runs. It is not enough to check that a test passes once — a two-key map has
a 50% chance of looking stable. Compare CONTENTS: sort the walked lines, or
compare as a set, and keep exact ordering only for the statements around the
walk. (`tests/interpreter.rs`'s `sorted_prefix` helper does the first half.)
Four such tests existed when the seeding landed and every one of them passed on
the run that introduced it.

**`KARAC_HASH_SEED=<n>` pins the key** (decimal or `0x…`; `0` is a legal pin), so
a run can be reproduced exactly — that is how a suspected order-dependence is
confirmed, and how the kata A/B harness compares output at all. Sweeping the
suite under several pinned seeds is the way to *find* order dependencies:

```bash
for seed in 1 2 3 5; do KARAC_HASH_SEED=$seed cargo test 2>&1 | grep FAILED; done
```

Do not set it outside testing: a published key is the same as no key, which is
the DoS-resistance the default exists to provide. `Map[K, V, FxBuildHasher]`
opts out deliberately (unkeyed, stable across runs of one binary) — see
`runtime/stdlib/hash.kara` for what that gives up.

**This covers element DESTRUCTION order too, and that is the half that bites**
(B-2026-08-27-7). When a `Map`/`Set` dies, each element's user `Drop` body runs
exactly once, but the SEQUENCE is the container's iteration order — unspecified,
per-process, and a different permutation on each backend. The reason it hides is
that a program cannot sort its way out: with `for (k, v) in m` the author chose
to observe the order and can sort at the use site, whereas drop bodies are
sequenced by the runtime with no use site to intervene at. So a kata that prints
from a container element's `Drop` breaks the A/B rule with nothing in the source
that looks order-dependent. `SortedMap`/`SortedSet` destroy in KEY order,
seed-independent and identical on every backend (measured) — they are the escape
hatch here exactly as they are for iteration. Within a map, keys are destroyed
before values.

The corollary for tests is the one above, unchanged: assert that each body fired
**once**, never the sequence. Every walker test in the tree does this
deliberately — which is also why an ORDER regression in one of them is invisible,
so an A/B fixture is the only thing that catches that class.

## Coding Standards

- Idiomatic Rust; follow `rustfmt` conventions.
- Every compiler phase must emit structured diagnostics with source spans — never just panic.
- Tests for every language construct. Use `tests/` for integration tests, unit tests inside each module for focused coverage.

## Developing Kāra code (not the Rust compiler — the `.kara` you write)

New Kāra — katas, examples, tests, dogfooding functions, self-hosting units — is developed and verified **through the Mend loop**, not hand-fixed: run `karac check --output=json`, apply `karac fix` for machine-applicable diagnostics as the primary fix path, feed the rest back, then verify the result against an **oracle** (expected output / test cases / a reference `solution.kara` / the self-host fixpoint). "It compiles" is not the bar. Each new artifact becomes a Mend task+oracle pair — format and granularity rule in [`examples/mend/TASK_FORMAT.md`](examples/mend/TASK_FORMAT.md). This continuously dogfoods the AI-first wedge (the flagship feature) and turns every diagnostic/fix gap into a backlog item: fix the compiler or open a `docs/bug-ledger.jsonl` entry, never route around it.

**Honesty rule (applies to any AI or contributor).** The Mend machine-fix *rate* is a statistic **only** over fresh, blind LLM authorship (`examples/mend/harness/mend_batch.py`, live) — a model that never saw the diagnostics. Authoring by anyone who already knows the language is biased (they won't make the known mistakes) and counts as dogfooding + gap-finding, **never** as the rate. Do not quote a machine-fix rate from non-blind authoring. Live mode needs an authenticated `claude` CLI (401s headless), so the measurement is a periodic developer-environment run, not a CI gate.

**Querying the ledger — filter, never full-read.** `docs/bug-ledger.jsonl` is an append-only log (one JSON object per line) and it only grows, so **never read the whole file into context**. As of 2026-08-25 it is **8.8 MB over 1561 rows** — roughly **2.2M tokens**, which does not fit in any context window, and it gains ~700 rows a month. (This figure read "~0.5 MB" until 2026-08-25, 17x stale; a prohibition resting on a number that small invites someone to decide a full read is survivable, so re-measure it rather than trusting this sentence.) It is line-oriented on purpose — query it: open bugs are `grep '"status": "open"' docs/bug-ledger.jsonl` (a handful of lines); a specific bug or its cross-refs are `grep 'B-2026-07-04-8' docs/bug-ledger.jsonl`. The human/LLM-readable rollup is `docs/bug-ledger.md`, regenerated from the jsonl by `python3 scripts/bug-curve.py --inject docs/bug-ledger.md`: it renders **open** bugs in full and collapses **fixed** ones to a one-line index (id · surface · sev · one-line title · fix SHA) — the fixed prose stays in the jsonl, grep-able by id. Read the `.md` for a survey (310 KB / ~1.7k lines — kept survey-able on purpose; it was 1.9 MB until the Fixed index stopped rendering each entry's full multi-paragraph `fix` prose into a one-line table cell) and grep the `.jsonl` for detail. When you fix or add a bug, edit the `.jsonl` and regenerate the `.md` (do not hand-edit the generated block). **Ledger field discipline (canonicalized 2026-07-17, lint-enforced by `scripts/bug-lint.sh`):** `class` is a CONTROLLED failure-mode vocabulary — one of `miscompile · double-free · use-after-free · leak · crash · codegen-gap · missing-feature · false-positive · soundness · run-vs-build · diagnostics · perf · other` — one primary class per bug, nuance in `detail`, never a new free-text class (that's how the field rotted the first time). `severity` is `high · medium · low`; `surface` is a base phase value or a `+`-joined compound (`typecheck+codegen`), with parenthetical detail in `detail`, not in the surface string. `source` is `family[:slug]` with the family from a closed set (kata · kata-gap · kata-gap-audit · selfhost · dogfood · probe · spike · internal · followup · test-infra · example); free-text provenance goes in `detail` as a SOURCE NOTE. `status` is `open · fixed · wontfix · relocated · invalid · not-reproduced`, and the four closed-without-a-fix values are **not** interchangeable: `invalid` means the PREMISE WAS REFUTED (the reported behaviour does not happen, or was misattributed — B-2026-07-17-14, B-2026-07-29-8), `not-reproduced` means it could not be made to happen again, `wontfix` means it is REAL AND REPRODUCIBLE but has been measured to a standstill with no action left (B-2026-08-10-20), and `relocated` means it is REAL AND STILL SCHEDULED but has no action item today and a concrete external trigger, so the work now lives on a canonical tracker — a `[->]` checklist entry, a roadmap item, a `deferred.md` tier (B-2026-08-23-11 → `implementation_checklist/phase-5-diagnostics.md § 5.5`). Reach for `wontfix` or `relocated` rather than parking a permanent no-op in `open` — that status is the work queue, which is why the rollup renders open rows in full — and never label a reproducible finding `invalid` just to close it, which writes a false claim into the ledger. **`wontfix` and `relocated` are the pair most easily confused**: `wontfix` says *measured to a standstill, nothing left to do*; `relocated` says *tracked elsewhere, here is where*. Calling scheduled work `wontfix` reads as a judgement that nobody wants it. A `relocated` row MUST carry a `tracker` naming its new home — `bug-lint.sh` enforces that, because a relocation whose pointer is missing is just a disappearance — and if the trigger ever fires, file a FRESH id citing the relocated row rather than reopening it. `wontfix` rows render as their own collapsed section with titles **in full**, because the measurements that closed the question are the whole point: read one before reopening its subject. `relocated` rows render likewise, with the tracker pointer as a column. If a closed-out investigation leaves a live remainder, split it into its own open row rather than burying it in the closed one (B-2026-08-10-20 → B-2026-08-11-10). Run `KARA_KATAS_DIR=../kara-katas ./scripts/bug-lint.sh` on a ledger change — it enforces all five enums, ID uniqueness, and that no row published on `origin/main` has gone missing. Run it AFTER your final rebase, not before: the last two checks are about the state you are actually pushing.

**Close rows with `scripts/bug-close.py`, not a hand-rolled script.** It takes a mandatory `--expect <substring>` that must appear in the row's title or source, checks it against the row ON DISK before writing, and refuses if it does not match — then writes canonical JSON and regenerates the rollup for you:

```bash
scripts/bug-close.py B-2026-08-11-9 --expect "comparison-op" \
    --sha 22ba601 --fix fix.txt --append-detail correction.txt   # --dry-run to check first
```

**Why the assertion is mandatory rather than optional.** B-IDs are allocated by reading the highest id in the file, so two sessions filing within the same window compute the *same* next id — this happened four times on 2026-08-11 alone. Three times it was a harmless renumber. The fourth time a session's close script found the row by id alone and wrote its title, fix and status onto a *different* session's row, producing one hybrid row (one session's `source`/`class`/`detail` under the other's title and fix) and leaving its own bug — a fixed high-severity double free — with no row anywhere in the ledger. `bug-lint.sh` cannot catch this: it checks that ids are UNIQUE, and they were, because the write was in-place. Corruption of a row's *content* is invisible to any check on the file, so the guard has to run at the write, in the one place that knows which bug the caller thinks it is closing. If `--expect` fails, the fix is to file under a fresh id — **never** to relax `--expect` until it passes, which is precisely the clobber being prevented. The script also refuses to re-close an already-closed row (`--allow-reclose` to override), since that is the other shape the same race takes.

The remaining hazard is *allocation*, which this does not solve: two sessions can still pick the same id when filing. Compute the next id as late as possible, immediately before the push, and re-check it after any rebase — a `git fetch` between allocating and pushing is worth more than it looks.

**An id collision is cheap to fix, because nothing has left the machine yet.** Two detectors fire before the push, and both were verified 2026-09-02:

* `git rebase origin/main` **conflicts**. Both sessions append at EOF, so the two rows land in one hunk and you see the collision directly.
* `scripts/bug-lint.sh` errors: `duplicate B-ID 'B-…' (also line N)`.

So the fix is not a force-push or a fresh branch — it is: keep BOTH rows, renumber yours, regenerate the `.md`, `git commit --amend`, re-run the lint, push. The id lives in the tip commit at filing time (the row, any parent row's prose that cites it, the commit message), so one amend covers it. Source comments citing an id are only written when the row is FIXED, long after the id is stable, so they are not part of this.

**Run the lint AFTER the final rebase, not before it** — the same reason a fix SHA has to be read back after one. Linting first checks a tree that the rebase is about to change, which is exactly the window a colliding row lands in.

**The dangerous resolution is the tidy one.** Resolving that conflict by taking `--ours` or `--theirs` instead of keeping both silently DELETES the other session's row: no duplicate, no format error, valid JSON, clean encoding, clean push. It is strictly worse than the duplicate it replaces, and rules 1–6 are all blind to it. Rule 7 (`no PUBLISHED row has disappeared`) exists for exactly that outcome — it compares against `origin/main` and errors with the one-line `git show` that restores the row. It runs only when HEAD already contains `origin/main`, and says so when it skips, because before a rebase the remote legitimately holds rows you have not merged yet.

**The fix SHA needs that same after-the-rebase re-check, and for the same reason.** A row is closed with `--sha <the fix commit>`, and the workflow above requires `git rebase origin/main` before every push — which *rewrites that commit's SHA*. Record the SHA before rebasing and the row names a commit that exists only in your container: it resolves fine locally (the reflog keeps it) and resolves nowhere for anyone who clones `main`, so the row stops being traceable to its fix and nothing local can tell you. Measured: EIGHT rows on `main` carry a `SHA NOTE` recording an orphan (B-2026-08-20-15, B-2026-08-29-10, B-2026-08-29-48, B-2026-09-04-30, B-2026-09-05-16, B-2026-09-12-13, B-2026-09-13-1, B-2026-09-16-4), so the class recurs — and every one of them was caught by a human reading rebase output, because until 2026-09-17 nothing in the tree could see it. **So close the row AFTER the final rebase, reading the SHA back out of `git log` rather than from memory** — and if a rebase happens after the close, re-read it and correct the row before pushing. **Two gates now enforce that, and both work on a shallow clone** (B-2026-09-16-8): `bug-close.py` REFUSES a `--sha` that is reachable from neither `HEAD` nor `origin/main` — i.e. one a rebase has already orphaned — and names the post-rebase twin it finds by matching the commit subject (`--allow-unreachable-sha` overrides); and `bug-lint.sh` rule 6b re-checks every fix SHA this working copy just wrote, reading the `<bid> <sha>` pairs `bug-close.py` leaves in `.git/kara-closed-fix-shas` so the check still fires when the rebase was triggered BY the push (`git push` → non-fast-forward → fetch, rebase, retry, in one command), the shape in which "close after the final rebase" and "before the push" are the same instant. Rule 6, the older check, asks whether the sha RESOLVES, and `3b2a932` narrowed that question to the rows the tree changes so it can run on a shallow clone at all. Useful, but not the same question: **presence is the wrong test for an orphan**, because a commit a rebase replaced keeps its object — the reflog holds it — and so resolves perfectly in the very container that orphaned it (measured 2026-09-17: a real orphan in a changed row, `1 row(s) … checked`, `0 errors`). Rule 6's full-tree audit additionally still needs a full clone, which the cloud sessions do not have (`actions/checkout` is depth-1 by default, which is why the CI job sets `fetch-depth: 0`) — and that is the gap the eight above sat in for days each, surfacing only when a session unshallowed its clone for an unrelated reason. Note also that `hooks/pre-push`, which would run the lint for you, is opt-in per clone (`scripts/install-hooks.sh`) and therefore INERT in a fresh container: run the lint explicitly. If a row's opener turns out to cite an orphan, correct the opener and leave a `SHA NOTE:` recording what happened — the lint warns about a dangling SHA in the prose rather than erroring, precisely so a row is never punished for keeping that record.
