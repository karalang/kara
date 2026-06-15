#!/usr/bin/env bash
#
# oracle-sync-guard.sh — provenance guard for the self-hosted-lexer oracle.
#
# WHY: `tests/selfhost_lexer.rs` is a *differential oracle* — it lexes a shared
# corpus with BOTH the Rust seed lexer (`src/lexer.rs`) and the Kāra port
# (`selfhost/src/main.kara`) and asserts the two token streams render
# identically. Its blind spot is *behaviour the corpus doesn't exercise*: if a
# seed commit changes `src/lexer.rs` behaviour without adding a corpus input
# that hits the new behaviour, the oracle stays green while the port silently
# drifts (exactly how commits 3311df6d / 99131a7b drifted the f-string
# interpolation + tuple-index paths — fixed in follow-on #31). The seed and the
# oracle corpus must move together.
#
# WHAT: fail when a commit/PR touches `src/lexer.rs` but NOT
# `tests/selfhost_lexer.rs`, over the range being tested. This forces the author
# to either (a) extend the corpus so the oracle exercises the new behaviour, or
# (b) consciously assert the change is oracle-irrelevant via an escape hatch.
#
# ESCAPE HATCHES (a legitimately oracle-irrelevant lexer edit isn't blocked):
#   1. Whitespace / format-only `src/lexer.rs` diff — if `git diff -w` reports
#      no change to that file (i.e. every hunk is pure whitespace, e.g. a
#      `cargo fmt` pass), the guard passes.
#   2. `[skip-oracle-sync]` anywhere in any commit message in the range — an
#      explicit, audit-trail-leaving author override (e.g. a comment-only or
#      doc-only lexer edit, or a corpus update that legitimately lands in a
#      follow-up commit).
#
# RANGE: on a PR, `$BASE_SHA..$HEAD_SHA` (merge-base of the PR base vs head, set
# by the workflow from the GitHub event). On a push, `$BASE_SHA..$HEAD_SHA` is
# the push's before..after. Falls back to `HEAD~1..HEAD` when neither is set
# (e.g. a local run) so the script is runnable by hand.
#
# Usage: BASE_SHA=<base> HEAD_SHA=<head> scripts/oracle-sync-guard.sh
set -euo pipefail

SEED="src/lexer.rs"
ORACLE="tests/selfhost_lexer.rs"

base="${BASE_SHA:-}"
head="${HEAD_SHA:-HEAD}"

# Resolve the comparison range. Prefer an explicit base; else merge-base of the
# two; else the single parent of HEAD. A missing/zero base (GitHub sends an
# all-zero SHA for a brand-new branch's first push) falls back to the parent.
if [[ -z "$base" || "$base" =~ ^0+$ ]]; then
  base="$(git rev-parse "${head}~1" 2>/dev/null || git rev-parse "$head")"
fi
# Use the merge-base so a stale fork point doesn't pull in unrelated files.
range_base="$(git merge-base "$base" "$head" 2>/dev/null || echo "$base")"

changed="$(git diff --name-only "$range_base" "$head")"

seed_changed=false
oracle_changed=false
grep -qx "$SEED"   <<<"$changed" && seed_changed=true
grep -qx "$ORACLE" <<<"$changed" && oracle_changed=true

if ! $seed_changed; then
  echo "oracle-sync-guard: $SEED not changed in ${range_base}..${head} — nothing to enforce."
  exit 0
fi
if $oracle_changed; then
  echo "oracle-sync-guard: both $SEED and $ORACLE changed — corpus moved with the seed. OK."
  exit 0
fi

# --- Escape hatch 1: whitespace/format-only seed diff. ---
# `git diff -w` ignores all whitespace; if it reports no change to the seed,
# every hunk was whitespace (a `cargo fmt` pass), so the oracle is unaffected.
if [[ -z "$(git diff -w --name-only "$range_base" "$head" -- "$SEED")" ]]; then
  echo "oracle-sync-guard: $SEED diff is whitespace/format-only — oracle unaffected. OK."
  exit 0
fi

# --- Escape hatch 2: explicit [skip-oracle-sync] marker in any commit message. ---
if git log --format='%B' "${range_base}..${head}" | grep -qF '[skip-oracle-sync]'; then
  echo "oracle-sync-guard: [skip-oracle-sync] marker found — author override honored. OK."
  exit 0
fi

cat >&2 <<EOF
oracle-sync-guard: FAIL

  $SEED changed in ${range_base}..${head}, but $ORACLE did NOT.

The self-hosted-lexer differential oracle only catches port drift on behaviour
its corpus exercises. A seed lexer change with no matching corpus input can let
the Kāra port (selfhost/src/main.kara) silently diverge while the oracle stays
green (see follow-on #31).

Fix ONE of:
  • Add a corpus input to $ORACLE that exercises the changed behaviour, and
    mirror the change in selfhost/src/main.kara (the port). This is the path you
    want for any behaviour-affecting lexer change.
  • If the change is genuinely oracle-irrelevant (comment/doc only, or the
    corpus update lands in a separate commit), add [skip-oracle-sync] to a
    commit message in this range.
  • Whitespace/format-only diffs are auto-exempt (no action needed).
EOF
exit 1
