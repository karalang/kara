#!/usr/bin/env bash
# Self-test for asan-o0-leg.sh's FAILURE CLASSIFICATION — the branch that
# decides which remedy to print for a new red.
#
# WHY. The leg classifies by grepping the suite log for messages that
# `tests/memory_sanitizer/mod.rs` produces. That is a coupling across two
# files with nothing joining them, and its failure mode is SILENCE: reword an
# assert message in mod.rs and the branch stops matching, the leg falls
# through to the generic "ASAN is reporting on memory the program actually
# touched", and a red is misrouted with no symptom anywhere. That has already
# happened once for real — B-2026-09-22-3's four CI reds were routed as a
# memory fault on the SSO string surface when ASAN had in fact passed and the
# program printed a wrong value.
#
# So this asserts the coupling in BOTH directions: every output-mismatch
# message mod.rs can emit is matched by the leg's pattern, and the
# memory-error message it emits is NOT.
#
# THE HARVEST IS ANCHORED STRUCTURALLY, on the message being the third
# argument of an `assert_eq!(got, expected_stdout, ...)`, and NOT on any word
# in the message. Anchoring on the wording was this file's own first bug: a
# harvest keyed on /mismatch/ cannot see a message reworded to drop that word,
# which is precisely the rewording it is supposed to catch. Under that
# spelling a fault injection renaming the message left the suite GREEN with
# its cell count silently down by one. The count is printed, and asserted
# against a floor, for the same reason.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$here/.."
leg="$here/asan-o0-leg.sh"
mod="$repo/tests/memory_sanitizer/mod.rs"

fail() { echo "ASAN-LEG SELFTEST FAIL: $*" >&2; exit 1; }
[ -r "$leg" ] || fail "cannot read $leg"
[ -r "$mod" ] || fail "cannot read $mod"

# The leg's discriminator, read OFF the script rather than restated here -- a
# copy would drift from the thing under test. Anchored on the branch's own
# line so it cannot pick up a sibling `grep -qE` and end up multi-line, which
# silently changes what every cell below tests.
pat="$(grep -E "grep -qE '.*stdout mismatch.*' \"\\\$LOG\"" "$leg" \
       | sed -E "s/^[^']*'//; s/' \"\\\$LOG\".*$//")"
[ -n "$pat" ] || fail "could not read the output-mismatch pattern out of asan-o0-leg.sh —
the branch was renamed or removed, and the classification is now unguarded"
[ "$(wc -l <<<"$pat")" = 1 ] || fail "read more than one pattern out of asan-o0-leg.sh:
$pat"

# ---- the harvest ----------------------------------------------------------
msgs="$(python3 - "$mod" <<'PY'
import re, sys
src = open(sys.argv[1]).read()
# Every assert_eq! that compares against expected_stdout, and its message.
# Structural: the message's WORDING is what is under test, so it cannot also
# be what selects it.
for m in re.finditer(
        r'assert_eq!\(\s*got,\s*expected_stdout,\s*("(?:[^"\\]|\\.)*")\s*,?\s*\)',
        src, re.S):
    print(m.group(1)[1:-1].replace("{label}", "some_label"))
PY
)"
msgs="$(sort -u <<<"$msgs" | sed '/^$/d')"
n="$(wc -l <<<"$msgs" | tr -d ' ')"
[ "$n" -ge 2 ] || fail "harvested only $n stdout-comparison message(s) from mod.rs, expected
at least 2 — the harvest has gone stale and every cell below is vacuous:
$msgs"

# ---- 1. every one of them must MATCH the leg's pattern --------------------
while IFS= read -r m; do
    [ -z "$m" ] && continue
    grep -qE "$pat" <<<"assertion \`left == right\` failed: $m" \
        || fail "the leg's output-mismatch branch does NOT match a message mod.rs emits:
  message: $m
  pattern: $pat
A red carrying this message will be misrouted as a memory fault."
done <<<"$msgs"

# ---- 2. a genuine ASAN memory error must NOT match ------------------------
# The other direction: a pattern widened to catch everything would tell you to
# ignore real leaks and double frees.
memerr="$(grep -oE '\[\{label\}\] ASAN reported a memory error[^"]*' "$mod" \
          | head -1 | sed 's/{label}/some_label/')"
[ -n "$memerr" ] || fail "could not find the memory-error assert message in mod.rs"
grep -qE "$pat" <<<"$memerr" \
    && fail "the leg's output-mismatch branch matches a REAL ASAN memory error:
  message: $memerr
  pattern: $pat
Real leaks and double frees would be reported as wrong values."

# ---- 3. the link-failure branch stays disjoint from it --------------------
# Branch ORDER decides the remedy, so the two discriminators must not both
# fire on one log.
grep -qE "$pat" <<<'this program needs the regex runtime archive `libkarac_runtime_regex.a`' \
    && fail "the output-mismatch pattern also matches a missing-archive link failure"

echo "asan-leg selftest: $n output-mismatch message(s) classified, memory errors and link failures excluded"
