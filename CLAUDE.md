# CLAUDE.md

Guidance for Claude Code (claude.ai/code) in this repository. This is the v2
version, written at M3, when the legacy pipeline was deleted. The pre-M3 file,
with every rule's measured history, is kept verbatim at
[`docs/legacy/CLAUDE-legacy.md`](docs/legacy/CLAUDE-legacy.md); read the
matching section there before relaxing a rule here.

## The gate

Four legs, all clean before any commit lands:

```bash
cargo fmt --all -- --check                                       # first action of every session
cargo clippy --all --all-targets -- -D warnings                  # CI's leg, the one easiest to miss
cargo clippy --all --all-targets --features llvm -- -D warnings  # the backend surface
cargo test --all                                                 # every workspace crate: front end, MIR, kernel, lsp, runtime
cargo test --features llvm                                       # everything, including the compiled backend
```

- **fmt first.** If `--check` fails on a fresh checkout, land `cargo fmt --all`
  as a standalone `chore: cargo fmt cleanup` commit before feature work. Never
  fold drift into a feature commit.
- **Both clippy legs.** `--features llvm` is not a superset: each cfg has code
  the other cannot see. `--all-targets`, never `--tests`, for the same reason.
- **Wasm cfgs** are invisible to native clippy. When touching runtime code under
  a wasm cfg: `cargo clippy -p karac-runtime --target wasm32-wasip1 --no-default-features`
  and `cargo clippy -p karac-runtime --target wasm32-wasip1-threads --no-default-features --features wasm-threads`.
- **`--all` on the default test leg.** Bare `cargo test` tests only the root
  package, so the kernel, lsp and runtime crates' tests never run; CI runs
  `--all` (measured at M3: two kernel reds reached main this way).
- `cargo test lexer` runs one test file; `cargo test -- <name>` runs one test.

## Runtime archives

`karac build`, `karac run` (through `karac_jit_runner`) and the
`mir-llvm` / `mir-llvm-asan` corpus backends link a static runtime archive
(`src/backend/driver.rs`). One-time setup, in this order:

```bash
# Lean (rustls-free) FIRST, then the full archive overwrites the canonical name.
cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
cargo rustc -p karac-runtime --release --crate-type staticlib
# Wasm (`--target=wasm_wasi`) and threaded wasm; separate target dirs.
cargo rustc -p karac-runtime --release --target wasm32-wasip1 --no-default-features --crate-type staticlib
cp target/wasm32-wasip1/release/libkarac_runtime.a target/release/libkarac_runtime_wasm.a
cargo rustc -p karac-runtime --release --target wasm32-wasip1-threads --no-default-features --features wasm-threads --crate-type staticlib
cp target/wasm32-wasip1-threads/release/libkarac_runtime.a target/release/libkarac_runtime_wasm_threads.a
# OPTIONAL, one per feature, each built LAST and renamed, then the plain full build re-run:
#   --features gpu|regex|arrow|unicode  ->  libkarac_runtime_{gpu,regex,arrow,unicode}.a
cargo rustc -p karac-runtime --release --features unicode --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_unicode.a
cargo rustc -p karac-runtime --release --crate-type staticlib
# OPTIONAL, ThreadSanitizer (`karac __mir-llvm-tsan`); its own target dir, so nothing above is rebuilt.
# std is rebuilt instrumented (`rustup component add rust-src` once): see the bullet below.
RUSTC_BOOTSTRAP=1 RUSTFLAGS=-Zsanitizer=thread cargo rustc -Zbuild-std=std,panic_abort --target x86_64-unknown-linux-gnu -p karac-runtime --release --target-dir target/tsan --crate-type staticlib
cp target/tsan/x86_64-unknown-linux-gnu/release/libkarac_runtime.a target/release/libkarac_runtime_tsan.a
rm -rf target/tsan
```

- **The TSan archive's std must be instrumented too.** Against the
  precompiled std, TSan cannot see `std::sync::Mutex` take its lock, so every
  `program_state!` access (borrow flags, stdout, channels) can read as a
  race: 15 of 17 corpus reports re-run at f5689254b4 were that, and went away
  with `-Zbuild-std`. A report that names no `Mutex M…` on a lock-guarded global
  is the sign of a plain-std archive.

- **`cargo rustc … --crate-type staticlib`, never `cargo build -p karac-runtime`.**
  The crate is `["staticlib", "rlib"]`; emitting both under fat LTO keeps std's
  backtrace symbolizer alive in every binary (+41% on the smallest). See the
  comment at `runtime/Cargo.toml`'s `crate-type`.
- The driver picks the archive from the emitted object's symbols: the lean one
  unless a TLS symbol is referenced, and an opt-in archive when a
  `karac_runtime_gpu_*` / `karac_arrow_*` / `karac_regex_*` / `karac_unicode_*`
  symbol is, and the TSan one for a `-fsanitize=thread` link. A missing opt-in
  archive fails at link with the recipe. The TSan archive must be the
  instrumented one: TSan sees the order the pool's lock imposes only in
  instrumented code, and against the plain archive a clean task program
  reports dozens of races.
  `KARAC_RUNTIME=<path>` is honored verbatim; `KARAC_FORCE_FULL_RUNTIME=1`
  forces the full archive.

### Staleness: the archives and the runner are built artifacts

- **Rebuild the archives whenever `runtime/src` changes at all**, not only when
  its symbol set does. A behaviour-only change links fine against an old archive
  and silently runs the old behaviour. The runtime also compiles
  `src/http1.rs`, `src/format_spec.rs`, `src/tls_conn.rs` and `src/ws_frame.rs`
  through `#[path]` (`grep -rn '#\[path' runtime/src`), so they count. The check
  before trusting any compiled measurement:
  `git log --oneline <sha-the-archives-were-built-at>..HEAD -- runtime/src src/http1.rs src/format_spec.rs src/tls_conn.rs src/ws_frame.rs`.
- **"The archives" includes the opt-in ones.** Nobody rebuilds those, so a
  container that built one once keeps serving it.
- **`undefined reference to karac_*` from a test means rebuild the archives**,
  not debug the backend.
- **`karac run` executes through the sibling `karac_jit_runner` binary**
  (`src/backend/runner.rs`), which statically links the runtime too. Build both
  bins: `cargo build --features llvm`, with no `--bin` filter. A JIT
  `Symbols not found: [ karac_… ]` while `karac build` works means a stale
  runner, or a missing keep-list entry in `runtime/src/lib.rs`.
- **`KARAC_REQUIRE_RUNTIME_ARCHIVE=1`** turns a missing-archive skip into a
  failure. Set it only on a `--features llvm` run (the default leg has no
  backend, so its skips are legitimate) and pair it with `--no-fail-fast`, since
  it also fails the opt-in archives' tests on a checkout without them.

## Measuring

The rules here all guard against one failure: an instrument that silently
reports nothing, read as a negative.

- **A filtered `cargo test` that matches nothing exits 0** with `0 passed; 0 failed`.
  Assert the pass COUNT, and read every verdict as three-valued: pass, fail, or
  void. On a control arm a void run looks exactly like success.
- **A control arm names its tree.** Run it in a scratch worktree
  (`git worktree add --detach <dir> origin/main`), or check out a NAMED base and
  restore it. Never use `HEAD~1`, which drifts with every commit, and never
  `git stash` without refusing when the stash took nothing. While the fix is
  uncommitted, restoring to `HEAD` deletes it.
- **Rebuild after the restore.** The source is back; the binary is still the
  control's.
- **`git archive | tar -x` keeps old mtimes**, so cargo rebuilds nothing. `touch`
  the tree after an extraction, or bisect with `git checkout`.
- **`Map` / `Set` iteration and destruction order is random per process**
  (SipHash under a per-process key, one implementation in `hash/`). Never assert
  an order; compare contents. `KARAC_HASH_SEED=<n>` pins it for a reproduction;
  the corpus runner pins `1`. `SortedMap` / `SortedSet` are ordered.
- **Leaks are measured on Linux.** LeakSanitizer does not run on macOS, and some
  leaks are arm64-only, so a green local Mac run is no evidence about leaks.
- **A schedule-sensitive fault needs repetition** before it is called fixed or
  bisected.

## The corpus is the gate

`corpus/` holds every runnable Kāra program the project has, with its expected
output (`corpus/README.md`). `corpus/CLASSIFICATION.md` is the M1 gate (bar 95%).

```bash
python3 scripts/corpus/run.py --backend mir-interp --filter <tag-or-path>
python3 scripts/corpus/run.py --backend mir-llvm --filter <tag-or-path>
python3 scripts/corpus/run.py --backend mir-llvm-asan --filter <tag-or-path>   # ASan/LSan lane
python3 scripts/corpus/classify.py --filter <path>      # re-judge entries against their expectation
```

- `run.py` exits 2 when nothing executed, so a void filter cannot read as a pass.
- The ASan lane is `karac __mir-llvm-asan`: the same IR built with
  AddressSanitizer. `tests/mir_asan.rs` proves the instrument reports.
- A full reclassify runs over two hours: `--resume` across restarts, on the same
  release binary. `extract.py --katas ../kara-katas`, never without `--katas`.
- An expected output comes from a spec source (`expected_from` = pin, spec,
  mirror, decision or oracle); `classify.py` ignores any other expected.out.

## Long-running work: don't poll, sweep

A full gate runs 25 to 40 minutes. Start it in the background and let the
completion notification arrive; never write an `until … sleep` loop. Never match
a process by a string in your own command line (`pgrep -f "cargo test"` matches
its own shell). If a wait is unavoidable, poll a sentinel the command writes as
its last action. Before declaring work done, stop what you started
(`pgrep -af 'sleep|until'`).

## A red gate with no named test is probably a full disk

A cloud session's disk is a per-session allowance of about 38 GiB, and `df`'s
size column is the host volume: read `used + avail`, or run
`bash scripts/disk-guard.sh allowance`. One linked test leg costs about 20 GiB, so
the second leg is where it runs out. The signs are `No space left on device`,
`ld terminated with signal 7`, a bare exit code, or file-writing tests failing
with plain assertion diffs. Either fix works:

```bash
CARGO_PROFILE_TEST_DEBUG=line-tables-only cargo test --features llvm   # ~250 -> ~100 MiB per test binary
cargo clean -p karac                                                    # between legs
```

Read `df` AFTER the failing leg, and re-run the named failures alone before
blaming a commit you rebased onto.

## Branches

- **Cloud container: work directly on `main`.** No feature branch and no PR
  unless asked. Before starting a slice and before every push:
  `git fetch origin main`, then `git rebase origin/main` (or `reset --hard` if
  you have no local commits). Push with `git push origin main`, retrying network
  errors with 2/4/8/16 s backoff. Never force-push past commits you did not
  create; a non-fast-forward means fetch and rebase. Deleting a remote branch
  returns 403 from inside a session: flag it for the owner.
- **Local multi-worktree checkout:** every slice in its own worktree off a
  freshly synced `main`; integrate with `git rebase main` in the worktree, then
  `git merge --ff-only <branch>` from the primary. Never `update-ref`. The two
  ways that has gone wrong are in the legacy file.

## Fixtures

- A new behaviour is pinned by a **corpus program** with a spec expectation when
  it can be (`corpus/core/` for core-semantics pins), or by a test in the v2
  suites (`tests/mir_*.rs`, `tests/core_strict_check.rs`, `tests/build_v2.rs`,
  `tests/run_v2.rs`, `tests/repl_v2.rs`).
- Several threads push to `main` at once. Prefer a NEW file for a new family of
  fixtures over appending to the end of a shared one: two appends at EOF always
  conflict, two new files never do. At a conflict, keep both sides' fixtures.
- Test names are unique across `tests/`, so `grep -rn '<name>' tests/` finds any
  fixture. Run `cargo fmt --all` after every rebase.
- A fixture that pins a bug's wrong answer is flipped by the fix.

## The bug ledger

`docs/bugs-v2.jsonl` is the live ledger, one JSON object per line; the pre-M3
ledger is archived read-only at `docs/legacy/bug-ledger.jsonl`, and ids are
unique across both. `docs/bugs-v2.md` is the generated rollup and explains the
schema.

- **Query it, never read it whole:** `grep '"status": "open"' docs/bugs-v2.jsonl`,
  or `grep '<id>' docs/bugs-v2.jsonl docs/legacy/bug-ledger.jsonl`.
- **Vocabularies are closed and lint-enforced.** `class` is one of miscompile ·
  double-free · use-after-free · leak · crash · codegen-gap · missing-feature ·
  false-positive · soundness · run-vs-build · diagnostics · perf · other.
  `status` is open · fixed · wontfix · relocated · invalid · not-reproduced, and
  the closed values are not interchangeable: `invalid` = the premise was refuted;
  `not-reproduced` = could not make it happen again; `wontfix` = real, measured
  to a standstill, nothing left to do; `relocated` = real and still scheduled,
  tracked elsewhere (it requires a `tracker`). Surfaces include `mir` and
  `mir-llvm`; in this ledger `interp` is the MIR interpreter.
- **Filing:** allocate the id as late as possible, off a fresh fetch. Two
  sessions computing the same id is common; at the rebase conflict keep BOTH
  rows and renumber yours. Taking one side deletes the other session's row,
  which lint rule 7 catches. `detail` + `fix` stay under 6 KB (rule 11).
- **Closing:** `scripts/bug-close.py <id> --expect "<substring of its title>" --sha <sha> --fix <file>`,
  run AFTER the final rebase, reading the sha back from `git log`. A rebase
  rewrites the sha, and the script and lint rule 6b refuse an orphaned one.
  `--expect` exists because a close keyed on id alone once wrote one bug's fix
  onto another session's row. Never relax it to make it pass; file a fresh id.
  Re-read the row before closing: a claim does not freeze it.
- Run `KARA_KATAS_DIR=../kara-katas ./scripts/bug-lint.sh` after the final rebase
  on any ledger change. `hooks/pre-push` runs it only after
  `scripts/install-hooks.sh`, which a fresh container has not run.
- A ledger id cited in code must name a row (rule 9). A comment predicting what
  a fixture does once a row is fixed writes `PREDICTS <id>`.

## Claiming a bug

Open ledger rows are the shared work queue, and two sessions picking the same
row is the default unless one claims it first. The claim lives in Claude Code
Remote session metadata, not in a commit.

1. Sync the ledger (`git fetch origin main` + rebase) before reading it.
2. `list_sessions({mine: true, limit: 30})`: a `kara-bug:<id>` tag on a RUNNING
   session, or a recently updated IDLE one, is a live claim. Pick another row.
3. Tag yourself (`get_session()` for your id, then `set_session_tags`) and put the
   id in your title.
4. List again. If another session tagged the same id, the older `created_at`
   keeps it.
5. Re-read the board at the pre-push fetch. Another session's tag on the row you
   are about to close means stop and coordinate.
6. Drop the tag once the closing commit lands.

The cloud git gateway refuses any ref outside `refs/heads/main` (HTTP 403), which
is why the claim is metadata rather than a git ref.

## Architecture

`karac` is the Kāra compiler, in Rust.

```
Source → lexer → parser → resolver → typechecker → effectchecker → ownership
       → typed HIR → MIR (src/mir/: lower, borrowck, elaborate, effects)
       → MIR interpreter (src/mir/interp.rs)  |  LLVM (src/backend/)
```

- `src/lib.rs` is the programmatic API. `karac build` / `run` / `test` / `repl`
  run on MIR; `karac __mir-run`, `__mir-llvm-run` and `__mir-llvm-asan` are the
  corpus runner's backends.
- **Codegen containment is a load-bearing invariant.** `src/backend/` (behind
  `--features llvm`) is the only code that imports `inkwell` or names an LLVM
  type, and CI's containment guard enforces it. A stage that needs to pass a
  hint to code generation does so through plain-data records.
- `docs/core-semantics.md` is normative for the core language (moves, borrows,
  drops, patterns, providers), each rule pinned by a program in `corpus/core/`.
  `docs/design.md` holds the rest of the language and the rationale;
  `docs/library/` the standard library; `docs/deferred.md` what is scheduled
  after v1.
- Language reminders: generics are `[T]`; parameters BORROW by default (`own T`
  takes ownership, `mut ref T` writes); effects are inferred for private
  functions and declared for public ones; `shared struct` / `shared enum` are
  reference-counted.

## Coding standards

- Idiomatic Rust, `rustfmt` conventions.
- Every phase reports structured diagnostics with source spans; never panic on
  user input.
- A test for every language construct.

## Writing Kāra

New Kāra (katas, examples, apps) goes through the Mend loop: `karac check
--output=json`, `karac fix` for machine-applicable diagnostics, then an oracle
(expected output, a Python mirror, a reference solution). "It compiles" is not
the bar. A diagnostic or fix gap becomes a compiler fix or a ledger row, never a
workaround. The Mend machine-fix RATE is only a statistic over blind LLM
authorship (`examples/mend/harness/mend_batch.py`); never quote one from
authoring by someone who knows the language.
