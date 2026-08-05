<!--
Delete any section that does not apply. A short PR with a clear "why" beats a
long one with a thorough "what" — the diff already says what changed.
-->

## What and why

<!--
What this changes, and the reason it is right. If it fixes a bug, say which
one (`B-YYYY-MM-DD-N`) and what the root cause turned out to be — the root
cause is the part that is expensive to rediscover later.
-->

## How it was verified

<!--
Which gates you ran, and anything you checked by hand. For a bug fix, the
useful sentence is that the regression test FAILS without the change — stash
the fix and run it. A test that passes either way is not a regression test.
-->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] `cargo test --features llvm` <!-- required for anything touching codegen -->
- [ ] Regression test added, and confirmed to fail without this change

<!--
Codegen changes: the E2E tests SKIP rather than fail when the runtime archives
are missing, so a green run means nothing without them. See CONTRIBUTING.md.

Leak-class changes: ASan runs LeakSanitizer on Linux but NOT on macOS, so a
green local Mac run does not clear a leak. CI is the gate.
-->

## What this does not do

<!--
Anything you deliberately left unfixed, tried and abandoned, or scoped out —
and why. This is genuinely useful: it stops the next person walking into the
same dead end, and honestly-scoped partial work is welcome here.
-->

## Ledger

<!--
Only if this fixes or discovers a bug.
- Fixed a ledger bug? Set `status`/`fix` in docs/bug-ledger.jsonl, regenerate
  the rollup (`python3 scripts/bug-curve.py --inject docs/bug-ledger.md`), and
  run `./scripts/bug-lint.sh`.
- Found one you are not fixing? File it rather than working around it.
- After any ledger edit, check `git diff --numstat docs/bug-ledger.jsonl`
  shows only the lines you meant to touch — a scripted rewrite that
  re-serializes rows buries the real change.
-->
