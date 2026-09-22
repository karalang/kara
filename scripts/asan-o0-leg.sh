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
# How much of each new failure's captured output to echo, and how many of
# them to echo it for. See the CAPTURED OUTPUT block below for why this
# exists at all; the caps are here so a leg that goes broadly red reports a
# readable summary rather than a wall of text nobody scrolls through.
CAPTURE_LINES="${ASAN_LEG_CAPTURE_LINES:-80}"
CAPTURE_MAX="${ASAN_LEG_CAPTURE_MAX:-5}"
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

# ── Archive FRESHNESS (B-2026-09-16-4) ───────────────────────────────────────
#
# Every other archive guard in this tree tests PRESENCE. `link_or_skip`
# discriminates on `undefined symbol`, which is the LOUD half of staleness
# (B-2026-07-28-1): an archive missing a symbol codegen emits. REQUIRE_ARCHIVE
# above turns a soft-skip into a panic — presence again. The opt-in carve-out
# below greps the failure log for archive FILENAMES, and has to, so that a
# missing REQUIRED archive stays a hard failure.
#
# None of that can see the SILENT half: a commit that changes what an EXISTING
# `karac_*` symbol DOES. Same name, same signature, different semantics. The
# link succeeds and the fixture runs the old behaviour, green.
#
# Measured 2026-09-16, which is why this exists: after rebuilding the lean and
# full archives (the two commands most people run), this container's opt-in
# regex/arrow/unicode archives were five days old, `runtime/src` had moved three
# times since — one of them a pure behaviour change — and both legs reported
# 1641/1641 green with six fixtures executing a stale runtime. The opt-in
# archives are the worst case precisely because CLAUDE.md tells everyone to SKIP
# building them, so they are the ones nobody ever rebuilds; the hazard is
# created by having built them ONCE.
#
# The comparison is the archive's mtime against the COMMIT TIME of the last
# change to `runtime/src`, not against file mtimes — a fresh clone stamps every
# file at checkout, so file mtimes say nothing.
if [[ "${KARAC_ALLOW_STALE_ARCHIVE:-0}" != "1" ]]; then
  runtime_ct="$(git log -1 --format=%ct -- runtime/src 2>/dev/null || true)"
  if [[ -z "$runtime_ct" ]]; then
    # A shallow clone (cloud containers, actions/checkout depth-1) may not carry
    # the commit that last touched runtime/src. Say so rather than pass quietly:
    # a skipped check that looks like a passed one is this row's whole subject.
    echo ">> archive freshness: SKIPPED — no runtime/src history (shallow clone?)"
  else
    # Only the archives THIS leg can link. A `_wasm` / `_wasm_threads` archive
    # is for `karac build --target=wasm_*` and can never reach a
    # memory_sanitizer fixture, so failing on one would be noise — and a gate
    # that fires on irrelevant things gets routed around with the override
    # below, which would defeat the whole check. They are reported separately.
    stale_archives=()
    stale_other=()
    for a in target/release/libkarac_runtime*.a; do
      [[ -e "$a" ]] || continue
      a_mt="$(stat -c %Y "$a" 2>/dev/null || stat -f %m "$a" 2>/dev/null || echo 0)"
      [[ "$a_mt" -lt "$runtime_ct" ]] || continue
      case "$a" in
        *_wasm.a|*_wasm_threads.a) stale_other+=("$a") ;;
        *) stale_archives+=("$a") ;;
      esac
    done
    if [[ ${#stale_other[@]} -gt 0 ]]; then
      echo ">> note: stale wasm archive(s), not linkable by this suite so not fatal:"
      printf '     %s\n' "${stale_other[@]}"
      echo "   Rebuild them before trusting any --target=wasm_* result."
    fi
    if [[ ${#stale_archives[@]} -gt 0 ]]; then
      echo "!! STALE RUNTIME ARCHIVE(S) — older than the last runtime/src change."
      echo "   runtime/src last changed: $(git log -1 --format='%h %ci  %s' -- runtime/src)"
      for a in "${stale_archives[@]}"; do
        printf '     %s  built %s\n' "$a" \
          "$(date -d "@$(stat -c %Y "$a" 2>/dev/null || stat -f %m "$a")" '+%Y-%m-%d %H:%M' 2>/dev/null \
             || date -r "$(stat -f %m "$a" 2>/dev/null || echo 0)" '+%Y-%m-%d %H:%M' 2>/dev/null)"
      done
      echo "   These link cleanly and run the OLD behaviour — nothing else in the"
      echo "   tree detects that. Rebuild before trusting this leg (CLAUDE.md"
      echo "   § Commands has the recipe; the opt-in regex/arrow/gpu/unicode"
      echo "   archives need their own --features rebuild, lean-then-full order"
      echo "   applies to the canonical name)."
      echo "   Override with KARAC_ALLOW_STALE_ARCHIVE=1 if you know the change"
      echo "   cannot affect what these fixtures execute."
      exit 3
    fi
    echo ">> archive freshness: OK — all present archives postdate runtime/src@$(git log -1 --format=%h -- runtime/src)"
  fi
fi

# B-2026-09-09-7 — put the TRUE allowance in the log before the leg spends it.
# Advisory only: a hard refusal here would turn a tight-but-workable box red,
# which is worse than the failure it guards against.
bash "$REPO/scripts/disk-guard.sh" preflight 8 "$LEG leg" || true

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
  # B-2026-09-09-7 — this is exactly where a FULL DISK lands, and it arrives
  # disguised: an LLVM "PLEASE submit a bug report" banner, a linker SIGBUS, or
  # no output at all. Say which before printing the tail, so the tail is read
  # in the right frame.
  bash "$REPO/scripts/disk-guard.sh" classify "$LOG" || true
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
# 6 of the 1617 fixtures need one (4 regex, 1 arrow, 1 unicode), and with them
# subtracted the leg matches the quarantine list exactly. The COUNT drifts as
# fixtures land -- it read "5 ... (4 regex, 1 arrow)" until the normalize
# fixture's unicode archive joined them (re-measured 2026-09-14) -- so trust the
# SKIPPED list the leg prints, not this number. The detection below is keyed on
# the archive FILENAMES and needs no count, which is why the drift was harmless.
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

  # ECHO THE CAPTURED OUTPUT, because otherwise it does not survive this
  # function returning. `$LOG` is a `mktemp` under `trap 'rm -f "$LOG"' EXIT`,
  # so the ASAN report -- the stack, the bytes, whether it was a leak or a
  # double free -- is deleted moments after the name is printed, and on CI
  # there is no log file to go back to at all. A red on this leg was therefore
  # a fixture NAME and nothing else, which is only enough to reproduce with if
  # the failure reproduces for you: a leg red on the CI runner in four of seven
  # consecutive runs, intermittently, and green here (B-2026-09-22-3) could not
  # be diagnosed at all.
  #
  # This is the same shape as B-2026-09-19-5 and B-2026-09-22-1 one level up --
  # a lane that reports that something failed and discards what it said. Naming
  # a failure without its evidence is the weaker half of an instrument.
  #
  # Capped rather than unbounded, on both axes: a leg that goes broadly red
  # would otherwise bury its own summary. When the cap bites it says so, and
  # ASAN_LEG_CAPTURE_LINES / ASAN_LEG_CAPTURE_MAX raise it.
  n_new="$(echo "$new_failures" | sed '/^$/d' | wc -l | tr -d ' ')"
  echo
  echo "   CAPTURED OUTPUT (this log is deleted when the leg exits -- this is the"
  echo "   only place it survives, so read it here rather than re-running):"
  shown=0
  while IFS= read -r t; do
    [[ -z "$t" ]] && continue
    if [[ "$shown" -ge "$CAPTURE_MAX" ]]; then
      echo "   ... $((n_new - shown)) more not shown (ASAN_LEG_CAPTURE_MAX=$CAPTURE_MAX)"
      break
    fi
    shown=$((shown + 1))
    echo "   ---- $t ----"
    awk -v want="$t" -v max="$CAPTURE_LINES" '
      /^---- .* stdout ----$/ { on = ($2 == want); next }
      on && /^failures:$/     { on = 0 }
      on { if (++n > max) { print "[truncated -- raise ASAN_LEG_CAPTURE_LINES]"; exit } print }
    ' "$LOG" | sed 's/^/     /'
  done <<<"$new_failures"
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
else
  # B-2026-09-09-7 shape 5 — named fixtures with ordinary-looking failures can
  # still be a full disk, with no disk message anywhere in the log. Only the
  # free space can tell, so check it before this red is attributed to the
  # change under test.
  echo
  bash "$REPO/scripts/disk-guard.sh" classify "$LOG" || true
fi
exit "$status"
