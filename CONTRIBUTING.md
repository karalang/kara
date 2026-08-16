# Contributing to Kāra

Thanks for looking. Kāra is pre-v1 and moving fast — expect breakage, and
expect the answer to "why is it like this?" to be written down somewhere.

If you only want to ask something, [start a
Discussion](https://github.com/karalang/kara/discussions/new/choose). You do
not need to open a PR to be useful; a good bug report with a minimal `.kara`
repro is worth a lot here.

## Before you start

For anything beyond a typo, **open an issue or a Discussion first**. The
compiler has a lot of load-bearing invariants that are not obvious from the
code, and a short conversation up front is cheaper than a rewritten PR. This
is especially true for codegen and the ownership/drop machinery, where most
of the subtle bugs live.

## Getting a working build

You need Rust (the version is pinned — see below) and, for anything touching
codegen, **LLVM 18**.

```bash
git clone https://github.com/karalang/kara
cd kara
cargo build                  # compiler, no LLVM backend
cargo test                   # front-end tests: lexer, parser, resolver,
                             # typechecker, effects, ownership, interpreter

scripts/install-hooks.sh     # enable the pre-push bug-ledger lint (once)
```

That is enough for front-end work. For codegen you additionally need LLVM 18
and the runtime archives:

```bash
# apt: llvm-18 llvm-18-dev libpolly-18-dev   |   brew: llvm@18
export LLVM_SYS_181_PREFIX=$(llvm-config-18 --prefix)
cargo build --release --features llvm

# Runtime archives `karac build` links into native binaries.
# Lean FIRST, then full — both emit the same filename, so order matters.
cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
cargo rustc -p karac-runtime --release --crate-type staticlib
```

Use `cargo rustc … --crate-type staticlib`, **not** `cargo build -p
karac-runtime --release`. Emitting both the staticlib and the rlib in one
invocation under fat LTO defeats the staticlib's dead-code elimination and
inflates every compiled binary by ~41% (measured). The full reasoning is at
`runtime/Cargo.toml`'s `crate-type` line.

[CLAUDE.md](CLAUDE.md) carries the complete recipe, including the wasm
archives and the optional GPU / regex / Arrow ones. It is written for AI
agents working in this repo, but it is the most detailed build-and-invariants
document here and worth reading if you plan to stay a while.

### The vacuous-pass trap

**Without the archives, the codegen E2E tests do not fail — they skip**, with
a note on stderr, and the suite reports green having asserted nothing. If you
are changing codegen and your tests all pass suspiciously fast, check that
the archives exist.

A *stale* archive is different and now fails loudly: `undefined reference to
karac_*` from an E2E test means rebuild the archives (lean, then full), not
debug your codegen.

## The gates

Every one of these must be clean before a PR lands. They are the same
commands CI runs, so running them locally costs you nothing but time.

```bash
cargo fmt --all -- --check
cargo clippy --all --all-targets -- -D warnings
cargo test                              # front-end
cargo test --features llvm              # + codegen E2E and memory sanitizer
```

Two notes that catch people out:

- **`--all-targets`, not `--tests`.** `--tests` only builds the test cfg, so
  a lint that fires only in production code slips through.
- **Run `cargo fmt --all -- --check` first, before you start.** If it is
  already dirty, land the formatting as its own commit rather than mixing it
  into your change.

### Leaks are a Linux thing

`-fsanitize=address` runs LeakSanitizer on **Linux but not macOS**. A green
local `memory_sanitizer` run on a Mac catches use-after-free and double-free
and *silently misses leaks*. The CI `memory-sanitizer` job is the real gate.
There is also an arm64 leak job, and it exists because a real bug leaked on
arm64 while being balanced on x86 — a green x86 run does not clear arm64.

## CI

18 jobs. The ones most likely to surprise you:

| job | what it catches |
|---|---|
| `codegen-e2e` (×3: Linux, macOS, Linux arm64) | real compiled binaries |
| `memory-sanitizer` (+ arm64) | leaks / UAF / double-free |
| `msrv` | the declared `rust-version` floor still builds |
| `supply-chain` | `cargo deny` — advisories, licenses, sources |
| `wasm` | wasm-only clippy and archive builds the native jobs cannot see |
| `windows-lint` | the `#[cfg(windows)]` surface Linux clippy never type-checks |
| `stable-drift` | **non-blocking** — floating `stable` vs the pinned toolchain |

There is also a nightly `Fuzz` workflow (drop-soundness + libFuzzer
front-end targets) and a nightly supply-chain audit. Neither gates PRs.

### Toolchain

`rust-toolchain.toml` pins the exact compiler so rustfmt output, the clippy
lint set, and the size/speed baselines are reproducible. `rust-version` in
`Cargo.toml` is a separate knob — the oldest rustc the workspace supports,
verified on its own by the `msrv` job. Bumping the pin does not oblige you to
bump the floor.

## Architectural invariants

A few rules are load-bearing. Breaking them is not a style disagreement.

**Codegen containment.** `src/codegen.rs` and `src/codegen/` are the *only*
places that may import `inkwell` or name an LLVM type. Every upstream phase
treats the backend as a black box. If a new analysis needs to tell codegen
something, it does so through plain-data hint records, not embedded LLVM
types. CI enforces this with a grep. The point is that swapping the codegen
substrate stays contained surgery rather than a compiler rewrite.

**Every phase emits structured diagnostics with spans.** Never `panic!` on
bad user input — a panic in a compiler phase is a bug even when the input is
nonsense.

**Tests for every language construct.** Integration tests live in `tests/`
(one file per phase); unit tests live beside the code.

## Working on the compiler (Rust)

Normal stuff: match the surrounding code's idiom, comment density, and
naming. This codebase comments *why* far more than most — a comment
explaining which bug a guard exists for is more valuable than one restating
the code, and several of them are the only record of why an obvious-looking
simplification is wrong.

## Working on Kāra code (`.kara`)

Different rules apply, and they are not optional.

New Kāra — katas, examples, tests, dogfooding, self-hosting — is developed
through the **Mend loop**, not hand-fixed:

```
karac check --output=json
  → karac fix for machine-applicable diagnostics
  → feed the rest back → repeat
  → run an ORACLE: is it correct, not just compiling?
```

"It compiles" is not the bar. Every artifact needs an oracle — expected
output, test cases, a reference solution, or the self-host fixpoint. See
[`examples/mend/TASK_FORMAT.md`](examples/mend/TASK_FORMAT.md).

This continuously dogfoods the AI-first wedge, which is the point: every
diagnostic or fix gap you hit becomes either a compiler fix or a ledger
entry. **Do not route around a compiler bug** — that is how gaps become
permanent.

### The honesty rule

The Mend machine-fix *rate* is a statistic over **fresh, blind LLM
authorship** only. Writing Kāra when you already know the language is biased
— you will not make the mistakes the number is measuring — and counts as
dogfooding, never as the rate. Please do not quote a machine-fix rate from
non-blind authoring.

## The bug ledger

Bugs live in `docs/bug-ledger.jsonl`, one JSON object per line, append-only.

**Never read the whole file** — it is ~0.5 MB and only grows. Query it:

```bash
grep '"status": "open"' docs/bug-ledger.jsonl     # open bugs
grep 'B-2026-07-04-8' docs/bug-ledger.jsonl       # one bug and its cross-refs
```

`docs/bug-ledger.md` is the generated rollup: open bugs in full, fixed ones
collapsed to a one-line index. Read the `.md` to survey, grep the `.jsonl`
for detail.

If you fix a bug, edit the `.jsonl`, then:

```bash
python3 scripts/bug-curve.py --inject docs/bug-ledger.md   # regenerate the rollup
./scripts/bug-lint.sh                                      # enforce the field enums
```

Do not hand-edit the generated block in the `.md`. `class`, `severity`,
`surface`, and `source` are controlled vocabularies — the lint will tell you
what is allowed. One primary `class` per bug; nuance goes in `detail`.

A practical note from experience: the ledger is one line per entry, so a
scripted edit that reformats or re-serializes rows produces an enormous
diff that hides the real change. After any ledger edit, check
`git diff --numstat docs/bug-ledger.jsonl` shows the number of lines you
actually meant to touch.

### The pre-push hook

`scripts/bug-lint.sh` also runs in CI as the required `Lint` job, so a
malformed ledger reddens `main`. The most common way that happens: a row's
`fix` cites a commit SHA that a later `git rebase` changed, so the ledger
points at a **pre-rebase SHA that no longer resolves**. The lint's fix-SHA
check catches it — but only on a full clone, so it slips past a shallow local
checkout and only fails in CI.

To catch it before the push, `hooks/pre-push` runs the lint and blocks a push
that fails it. `git clone` does not enable repo hooks, so opt in once per
clone:

```bash
scripts/install-hooks.sh          # sets core.hooksPath to hooks/
git push --no-verify              # bypass the gate for one push, when needed
```

Two caveats worth knowing:

- **It needs a full clone.** The fix-SHA resolvability check skips on a
  shallow clone (git cannot resolve historical SHAs) — which is exactly the
  blind spot that lets a dangling SHA through. Run `git fetch --unshallow` so
  the gate actually fires.
- **The real cure is upstream of the hook:** allocate the `fix` SHA *after*
  the rebase that finalizes the commit, not before the push. The hook is the
  backstop for when that slips.

## Pull requests

- Branch off `main`. Keep the change focused; unrelated cleanup belongs in
  its own commit.
- **Explain why, not just what.** A commit message that says which bug a
  change fixes, what you ruled out, and what you deliberately did not fix is
  worth more than a tidy diff. If you tried an approach and abandoned it,
  say so — that saves the next person the same dead end.
- Include a regression test that **fails without your fix**. A test that
  passes either way is not a regression test; check by stashing the fix.
- If your change leaves something unfixed, say so explicitly rather than
  letting it be discovered later. Partial work that is honestly scoped is
  welcome.
- All gates green.

Contributions are dual-licensed MIT / Apache-2.0, matching the project —
see the Contribution note at the end of [README.md](README.md).

## Where things are

| path | what |
|---|---|
| `src/` | the compiler, one module per phase |
| `src/codegen/` | the LLVM backend (the only place LLVM types appear) |
| `runtime/` | the C-ABI runtime linked into compiled binaries |
| `tests/` | integration tests, one file per phase |
| `examples/` | end-to-end `.kara` programs |
| `hooks/` | git hooks (enable with `scripts/install-hooks.sh`) |
| `lsp/` | the language server |
| `docs/design.md` | the language spec (authoritative) |
| `docs/roadmap.md` | the implementation plan |
| `docs/bug-ledger.md` | the bug rollup |
| `docs/spikes/` | design investigations and their conclusions |
| `CLAUDE.md` | build recipes and invariants in full detail |
