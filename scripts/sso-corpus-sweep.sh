#!/usr/bin/env bash
# Re-time the SSO corpus regressions at KARAC_SSO=0 vs =1. B-2026-09-14-28.
#
# WHY THIS EXISTS AS A SCRIPT. The "+5.7% aggregate over 17 residual
# regressions" that the SSO spike doc quotes was produced ad hoc: the doc
# records the RESULTS and not the harness, so when two construction fixes
# (9d3ceb9, 066695ff) landed and made every construction rail flip sign, the
# corpus figure could not be re-taken without rebuilding the method from
# scratch. A number nobody can re-run is a number that rots silently. This is
# that method, committed.
#
# WHY SEVEN KATAS AND NOT 321. The spike doc's own re-measurement (§ "THE
# RESIDUAL, MEASURED PROPERLY: 7 not 17") found that a best-of-3 sweep's
# PER-KATA rows do not survive contact with a careful measurement: 10 of the 17
# claimed regressions evaporated at best-of-15 (row_buffers +29% -> +3%) while
# the worst three got substantially WORSE (vertical +36% -> +86%). Best-of-3 is
# not a conservative estimate of a per-kata delta, it is an unbiased-but-wide
# one. So this sweeps the SEVEN established-real regressions at high sample
# count rather than the whole corpus at low sample count.
#
# DISCIPLINE, all of it learned expensively on this track:
#   * KARAC_AUTO_PAR=0 pinned. Auto-par does not uniformly compress -- it masks
#     some regressions to zero and AMPLIFIES others fourfold (B-2026-09-15-6).
#   * Both arms built from ONE karac, timed in one pass, interleaved.
#   * Output verified against the SSO=0 arm per kata. A faster wrong answer is
#     not a result.
#   * RUNS defaults to 15. Three is not enough; the doc above is the evidence.
#   * Levels are host-dependent (B-2026-09-15-6). Never compare a delta here to
#     a delta from another machine or another commit -- re-run both arms.
#
# AND THE HARNESS REPORTS A SPREAD, NOT A NUMBER, because on a cloud container
# a single pass of this sweep is not reproducible. Measured 2026-09-16, three
# identical RUNS=15 passes minutes apart on one commit:
#
#     vertical  +20.6 / +51.8 / +52.9    32.3pts spread
#     atoi      +16.0 / +18.1 /  -3.3    21.4pts, CROSSES ZERO
#     word_ladder -4.9 / -16.2 /  +0.7   16.9pts, CROSSES ZERO
#
# Interleaving the arms sample-by-sample was tried as the fix -- the standard
# remedy for drift bias, and this container drifts enough to change CPU model
# mid-session (2.80GHz -> 2.10GHz Xeon). It did NOT help: worst spread went
# 32.3 -> 36.2 points. So the problem is variance, not bias, and no timing
# discipline available here removes it. What the harness can honestly do is
# measure the variance and refuse to report through it, which is what PASSES
# and SPREAD_LIMIT below are for. A kata that comes back UNRESOLVED needs a
# quiet host, not a bigger RUNS.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KATAS="${KARA_KATAS_DIR:-$ROOT/../kara-katas}"
RUNS="${RUNS:-15}"
# Repeat the WHOLE measurement this many times and report the SPREAD. A single
# pass of this sweep is not a result -- see the header note on convergence.
PASSES="${PASSES:-3}"
# A kata whose passes disagree by more than this many points is reported
# UNRESOLVED rather than given a number.
SPREAD_LIMIT="${SPREAD_LIMIT:-10}"
WORK="${WORK:-$(mktemp -d)}"
KARAC="${KARAC:-$ROOT/target/debug/karac}"
[ -x "$KARAC" ] || { echo "no karac at $KARAC — cargo build --features llvm" >&2; exit 1; }

# The seven established-real regressions, by kata path under $KATAS.
KATAS_LIST=(
  "vertical:leetcode/1-100/14-longest-common-prefix/bench/vertical.kara"
  "shortest_distance:leetcode/201-300/243-shortest-word-distance/bench/shortest_distance.kara"
  "shortest_distance_iii:leetcode/201-300/245-shortest-word-distance-iii/bench/shortest_distance_iii.kara"
  "word_ladder:leetcode/101-200/127-word-ladder/bench/word_ladder.kara"
  "alien:leetcode/201-300/269-alien-dictionary/bench/alien.kara"
  "alien_seq:leetcode/201-300/278-alien-dictionary/bench/alien_seq.kara"
  "atoi:leetcode/1-100/8-string-to-integer-atoi/bench/atoi.kara"
)

echo "commit:  $(cd "$ROOT" && git rev-parse --short HEAD)"
echo "host:    nproc=$(nproc) $(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2 | xargs)"
echo "RUNS=$RUNS  KARAC_AUTO_PAR=0  karac=$KARAC"
echo

# INTERLEAVED, and that is not a nicety. Timing all N runs of arm A and then
# all N of arm B biases the comparison by whatever the machine does in between,
# and a cloud container drifts: this one changed CPU model mid-session (2.80GHz
# -> 2.10GHz Xeon). Measured on the all-A-then-all-B version, three identical
# RUNS=15 passes minutes apart disagreed by up to 32 POINTS on one kata and
# crossed zero on two others. Alternating the arms sample-by-sample cancels
# first-order drift, because both arms see the same slow and fast windows.
#
# Returns "best0 best1" — the minimum for each arm over the interleaved series.
timeit_pair() { local b0=$1 b1=$2 best0=999999 best1=999999 s e ms i
  for i in $(seq "$RUNS"); do
    s=$(date +%s%N); "$b0" >/dev/null 2>&1; e=$(date +%s%N)
    ms=$(( (e-s)/1000000 )); [ $ms -lt $best0 ] && best0=$ms
    s=$(date +%s%N); "$b1" >/dev/null 2>&1; e=$(date +%s%N)
    ms=$(( (e-s)/1000000 )); [ $ms -lt $best1 ] && best1=$ms
  done; echo "$best0 $best1"; }

printf "%-22s %10s %9s %10s   %s\n" kata "deltas" spread verdict oracle
printf "%-22s %10s %9s %10s   %s\n" ---------------------- ---------- --------- ---------- ------
fail=0; resolved=()
for spec in "${KATAS_LIST[@]}"; do
  name="${spec%%:*}"; rel="${spec#*:}"; src="$KATAS/$rel"
  if [ ! -f "$src" ]; then printf "%-22s  SOURCE NOT FOUND: %s\n" "$name" "$rel"; fail=1; continue; fi
  d="$WORK/$name"; rm -rf "$d"; mkdir -p "$d"; cp "$src" "$d/k.kara"
  ok=1
  for sso in 0 1; do
    ( cd "$d" && KARAC_SSO=$sso KARAC_AUTO_PAR=0 "$KARAC" build k.kara >build.$sso.log 2>&1 && mv k "bin$sso" ) || ok=0
    [ -x "$d/bin$sso" ] || ok=0
  done
  if [ "$ok" != 1 ]; then printf "%-22s  BUILD FAILED (see %s)\n" "$name" "$d"; fail=1; continue; fi
  o0="$("$d/bin0")"; o1="$("$d/bin1")"
  if [ "$o0" != "$o1" ]; then
    printf "%-22s  OUTPUT DIVERGED — SSO=0 %q vs SSO=1 %q\n" "$name" "$o0" "$o1"; fail=1; continue
  fi
  ds=()
  for _p in $(seq "$PASSES"); do
    read -r t0 t1 <<< "$(timeit_pair "$d/bin0" "$d/bin1")"
    ds+=("$(python3 -c "print(f'{($t1-$t0)*100/$t0:.1f}')")")
  done
  read -r med spread verdict <<< "$(python3 - "$SPREAD_LIMIT" "${ds[@]}" <<'PYV'
import sys, statistics
lim=float(sys.argv[1]); d=[float(x) for x in sys.argv[2:]]
sp=max(d)-min(d)
# SPREAD alone decides. A tight band that happens to contain zero (say
# -2.9/-1.4/+1.5) is a RESULT -- it says "no effect" -- and an earlier version
# of this script wrongly flagged it UNRESOLVED for crossing. What makes a kata
# unreadable is passes that disagree, not a sign that happens to straddle.
v = "UNRESOLVED" if sp > lim else "ok"
print(f"{statistics.median(d):+.1f} {sp:.1f} {v}")
PYV
)"
  shown="$(IFS=/; echo "${ds[*]}")"
  if [ "$verdict" = ok ]; then resolved+=("$med"); fi
  printf "%-22s %10s %8spts %10s   match\n" "$name" "$shown" "$spread" "$verdict"
done

echo
echo "  PASSES=$PASSES  RUNS=$RUNS  SPREAD_LIMIT=${SPREAD_LIMIT}pts"
if [ ${#resolved[@]} -gt 0 ]; then
  python3 - "${resolved[@]}" <<'PY2'
import sys, statistics
d=[float(x) for x in sys.argv[1:]]
print(f"  RESOLVED katas : {len(d)}")
print(f"  median         : {statistics.median(d):+.1f}%")
print(f"  regressed >=5% : {sum(1 for x in d if x >= 5)}")
print(f"  improved  >=5% : {sum(1 for x in d if x <= -5)}")
PY2
else
  echo "  RESOLVED katas : 0 — nothing on this host converged; do NOT quote a corpus number from this run"
fi
echo

echo "workdir: $WORK"
[ "$fail" = 0 ] && echo "SWEEP OK" || echo "SWEEP INCOMPLETE — see failures above"
exit $fail
