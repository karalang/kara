#!/usr/bin/env bash
# Run the memory_sanitizer suite at KARAC_OPT_LEVEL=0 and gate on the result
# against a checked-in expected-failures list. B-2026-08-04-17.
#
# WHY A SEPARATE LEG AT ALL. ~70 fixtures in tests/memory_sanitizer.rs allocate
# NOTHING at the default -O2: their payload folds to a constant, or its bytes
# are never read, so LLVM deletes the allocation and the fixture asserts a clean
# ASAN run over memory that was never touched. Those same fixtures allocate for
# real at -O0, where no pass pipeline runs. So one extra whole-suite run at -O0
# gives ~70 fixtures genuine coverage with no fixture rewritten and no
# production code touched — measured (2026-08-05): 20 fixtures are zero at BOTH
# levels (heapless by design, nothing owed) and 70 are zero at -O2 but non-zero
# at -O0 (real heap work the optimizer deleted).
#
# WHY AN EXPECTED-FAILURES FILE RATHER THAN `#[ignore]`. The quarantined
# fixtures fail ONLY at -O0 and pass at -O2. `#[ignore]` is per-test, not
# per-level, so ignoring them would delete their -O2 coverage to buy the -O0
# leg — a straight downgrade. Keeping the list out-of-band leaves every fixture
# live on the default leg and quarantines it only here.
#
# The list is a ratchet in BOTH directions: a fixture that starts failing and is
# not listed fails this leg (a regression), and a listed fixture that starts
# PASSING also fails it (the list must shrink as the owning bugs are fixed, or
# it rots into a permanent allowlist). Every entry names the bug row that owns
# it; nothing goes on the list without one.
#
# Usage:
#   scripts/asan-o0-leg.sh                  # full leg
#   scripts/asan-o0-leg.sh --update         # rewrite the list from this run
#   ASAN_O0_TEST_THREADS=8 scripts/asan-o0-leg.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED="$REPO/tests/asan-o0-known-failures.txt"
THREADS="${ASAN_O0_TEST_THREADS:-4}"
LOG="$(mktemp -t asan-o0-XXXXXX.log)"
trap 'rm -f "$LOG"' EXIT

UPDATE=0
[[ "${1:-}" == "--update" ]] && UPDATE=1

echo ">> KARAC_OPT_LEVEL=0 cargo test --features llvm --test memory_sanitizer (--test-threads=$THREADS)"
KARAC_OPT_LEVEL=0 cargo test --features llvm --test memory_sanitizer \
  -- --test-threads="$THREADS" >"$LOG" 2>&1
echo ">> suite exited $?"

# A suite that never linked a binary reports "clean" for every fixture, which
# would read as "the whole quarantine list got fixed". Distinguish that from a
# real run before comparing anything (the same vacuity trap this row is about,
# one level up).
if ! grep -qE '^test result:' "$LOG"; then
  echo "!! no test-result line — the suite did not run to completion:"
  tail -30 "$LOG"
  exit 2
fi
if grep -q 'ASAN unavailable on this host' "$LOG"; then
  echo ">> ASAN unavailable on this host — leg skipped (not a pass)"
  exit 0
fi

RESULT_LINE="$(grep -E '^test result:' "$LOG" | tail -1)"
echo ">> $RESULT_LINE"

got="$(grep -oE '^test [A-Za-z0-9_:]+ \.\.\. FAILED' "$LOG" |
  sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u)"

if [[ "$UPDATE" == "1" ]]; then
  echo "$got" | sed '/^$/d' >"$EXPECTED.new"
  echo ">> wrote $EXPECTED.new — annotate each line with its owning bug row before replacing the list"
  exit 0
fi

if [[ ! -f "$EXPECTED" ]]; then
  echo "!! no quarantine list at $EXPECTED"
  echo "   Seed one with: scripts/asan-o0-leg.sh --update"
  echo "   then annotate every line with the bug row that owns it."
  exit 2
fi

# Strip comments/blanks; an entry is `<test path>` optionally followed by
# whitespace and a `# B-…` annotation.
expected="$(sed -E 's/[[:space:]]*#.*$//' "$EXPECTED" | sed '/^[[:space:]]*$/d' | sort -u)"

new_failures="$(comm -23 <(echo "$got" | sed '/^$/d') <(echo "$expected"))"
now_passing="$(comm -13 <(echo "$got" | sed '/^$/d') <(echo "$expected"))"

status=0
if [[ -n "$new_failures" ]]; then
  status=1
  echo
  echo "!! NEW -O0 FAILURES (not on the quarantine list):"
  echo "$new_failures" | sed 's/^/     /'
  echo "   These are real: at -O0 the fixture's allocations are not optimized away,"
  echo "   so ASAN is reporting on memory the program actually touched. Fix the"
  echo "   codegen defect, or add the fixture to $(basename "$EXPECTED") WITH the"
  echo "   bug row that owns it."
fi
if [[ -n "$now_passing" ]]; then
  status=1
  echo
  echo "!! QUARANTINED FIXTURES THAT NOW PASS:"
  echo "$now_passing" | sed 's/^/     /'
  echo "   Remove them from $(basename "$EXPECTED") (and close the owning bug row if"
  echo "   this was its last fixture). The list is a ratchet — it only shrinks."
fi

if [[ "$status" == "0" ]]; then
  n=$(echo "$expected" | sed '/^$/d' | wc -l | tr -d ' ')
  echo ">> -O0 leg matches the quarantine list exactly ($n known failure(s))"
fi
exit "$status"
