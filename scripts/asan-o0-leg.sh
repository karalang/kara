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
# PARAMETERIZED SINCE B-2026-09-07-40. The instrumented leg
# (scripts/asan-instrumented-leg.sh) is the same run with one more env var and
# its own quarantine list, so it sets the three knobs below and execs this
# script rather than duplicating ~100 lines of ratchet logic. The defaults
# reproduce the -O0 leg exactly; any other leg's environment is inherited by the
# cargo run, so a new leg is a wrapper, not a fork.
#
# Usage:
#   scripts/asan-o0-leg.sh                  # full leg
#   scripts/asan-o0-leg.sh --update         # rewrite the list from this run
#   ASAN_O0_TEST_THREADS=8 scripts/asan-o0-leg.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LEG="${ASAN_LEG_NAME:--O0}"
OPT_LEVEL="${ASAN_LEG_OPT_LEVEL:-0}"
EXPECTED="${ASAN_LEG_EXPECTED:-$REPO/tests/asan-o0-known-failures.txt}"
THREADS="${ASAN_O0_TEST_THREADS:-4}"
LOG="$(mktemp -t asan-leg-XXXXXX.log)"
trap 'rm -f "$LOG"' EXIT

UPDATE=0
[[ "${1:-}" == "--update" ]] && UPDATE=1

# B-2026-09-13-8 — REQUIRE THE ARCHIVES BY DEFAULT. Without this the leg is a
# gate that cannot tell "skipped" from "passed": on a tree with no runtime
# archives every fixture soft-skips through `link_or_skip` and the suite prints
# `test result: ok` in a third of a second, having linked nothing. Measured
# 2026-09-14 — two fixtures "passed" in 0.30 s with the archives moved aside,
# and failed in 0.52 s with the flag set. That vacuity is how `6ea22e3b4` landed
# four red fixtures on `main` behind a green run.
#
# The whole-suite case is caught today by the ratchet's other arm (all 309
# quarantined fixtures "start passing"), but it is caught with the WRONG
# message — "remove them from the list, it only shrinks" — which prescribes
# deleting the quarantine list. Naming the real cause is the point.
#
# Honour an explicit setting so a caller who genuinely wants the soft-skip can
# say so; `0` restores the old behaviour.
REQUIRE_ARCHIVE="${KARAC_REQUIRE_RUNTIME_ARCHIVE:-1}"

echo ">> [$LEG] KARAC_OPT_LEVEL=$OPT_LEVEL cargo test --features llvm --test memory_sanitizer (--test-threads=$THREADS)"
KARAC_OPT_LEVEL="$OPT_LEVEL" KARAC_REQUIRE_RUNTIME_ARCHIVE="$REQUIRE_ARCHIVE" \
  cargo test --features llvm --test memory_sanitizer \
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
  echo ">> [$LEG] ASAN unavailable on this host — leg skipped (not a pass)"
  exit 0
fi

RESULT_LINE="$(grep -E '^test result:' "$LOG" | tail -1)"
echo ">> $RESULT_LINE"

all_failed="$(grep -oE '^test [A-Za-z0-9_:]+ \.\.\. FAILED' "$LOG" |
  sed -E 's/^test (.*) \.\.\. FAILED$/\1/' | sort -u)"

# B-2026-09-13-8 — SUBTRACT the fixtures that failed only because an OPT-IN
# archive is absent. Requiring the archives (above) turns those from a skip into
# a failure, and CLAUDE.md § Commands tells everyone to SKIP building the
# regex / arrow / gpu / unicode archives unless doing that kind of work — so an
# unconditional flag would fail this leg on every ordinary checkout. Measured:
# 5 of the 1613 fixtures need one (4 regex, 1 arrow), and with them subtracted
# the leg matches the quarantine list exactly.
#
# Keyed on the OPT-IN archive filenames rather than on "a link failure", because
# a MISSING REQUIRED archive must stay a hard failure — that is the vacuity this
# whole change is about. Its panic names no opt-in archive, so it does not match.
optin_skipped="$(awk '
  /^---- .* stdout ----$/ { t = $2; next }
  t != "" && /libkarac_runtime_(regex|arrow|gpu|unicode)\.a/ { print t; t = "" }
' "$LOG" | sort -u)"

got="$(comm -23 <(echo "$all_failed" | sed '/^$/d') <(echo "$optin_skipped" | sed '/^$/d'))"

if [[ -n "$optin_skipped" ]]; then
  echo
  echo ">> SKIPPED — opt-in runtime archive absent (NOT counted as pass or fail):"
  echo "$optin_skipped" | sed 's/^/     /'
  echo "   Build the archive named in the failure to cover these; CLAUDE.md"
  echo "   § Commands deliberately leaves them unbuilt on an ordinary checkout."
fi

if [[ "$UPDATE" == "1" ]]; then
  echo "$got" | sed '/^$/d' >"$EXPECTED.new"
  echo ">> wrote $EXPECTED.new — annotate each line with its owning bug row before replacing the list"
  exit 0
fi

if [[ ! -f "$EXPECTED" ]]; then
  echo "!! no quarantine list at $EXPECTED"
  echo "   Seed one with: <this leg's script> --update"
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
  echo "!! NEW $LEG FAILURES (not on the quarantine list):"
  echo "$new_failures" | sed 's/^/     /'
  # A LINK failure lands in this same bucket and is NOT a leak. Under
  # KARAC_REQUIRE_RUNTIME_ARCHIVE=1 the soft-skip becomes a hard failure, so a
  # missing OPT-IN archive (regex / arrow / gpu — none of which the CLAUDE.md
  # setup recipe builds by default) fails exactly like a sanitizer report. Said
  # "these are real, fix the codegen defect" unconditionally, this message sends
  # you hunting a leak that is not there; the actual fix is one `cargo rustc`.
  # So name the cause before prescribing a remedy.
  if grep -q 'needs the .* runtime archive' "$LOG"; then
    echo "   NOT LEAKS — at least one is a LINK failure for a missing OPT-IN archive:"
    grep -oE 'needs the [a-z]+ runtime archive `[^`]+`' "$LOG" | sort -u | sed 's/^/     /'
    echo "   Build the named archive(s) per CLAUDE.md § Commands, then re-run. Under"
    echo "   KARAC_REQUIRE_RUNTIME_ARCHIVE=1 a missing archive is a hard failure"
    echo "   rather than a skip, which is why these surface here rather than passing"
    echo "   vacuously. Re-run the plain full build afterward so the canonical"
    echo "   archive name is the non-feature one again."
  else
    echo "   These are real: at -O$OPT_LEVEL the fixture's allocations are not optimized away,"
    echo "   so ASAN is reporting on memory the program actually touched. Fix the"
    echo "   codegen defect, or add the fixture to $(basename "$EXPECTED") WITH the"
    echo "   bug row that owns it."
  fi
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
  echo ">> $LEG leg matches the quarantine list exactly ($n known failure(s))"
fi
exit "$status"
