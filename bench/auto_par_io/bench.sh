#!/usr/bin/env bash
# auto_par_io — does auto-par overlap independent I/O, and does it SCALE?
# Before/after harness for path A. Companion to ./README.md.
#
# Self-calibrating: every run re-measures three rails on the same machine
# back-to-back, so the verdict is a RATIO (load- and thermal-immune), not an
# absolute against a stale baseline.
#
#   seq  rail : KARAC_AUTO_PAR=0 build of the straight-line source  (serial FLOOR)
#   par  rail : build of the explicit-`par {}` source               (overlap CEILING)
#   auto rail : default build of the straight-line source           (UNDER TEST)
#
# K-SWEEP: the rails run for K in {4, 16, 64} blocking calls. K=4 is under the
# blocking-pool worker count (~core count); K=64 is several × over it. A correct
# fix overlaps in pool-bounded WAVES — `auto` should track the `par` ceiling at
# every K (par hits the same pool ceiling, so it stays a fair reference). A fix
# that works at K=4 but wave-serializes at K=64 is caught here, not in prod.
#
# Two deterministic guards (exact, not timed): "do the blocking calls co-group?"
# (query concurrency) and a positive control proving the rig detects par_run when
# auto-par fires. Wall-clock confirms grouping becomes overlap.
#
# BEFORE A1: every K row => SERIAL  (grouped? no, auto ≈ seq).
# AFTER  A1: every K row => OVERLAP (grouped? yes, auto ≈ par, pool-bounded).
#
# Tunables (env): D_US (per-call µs, default 200000), RUNS (samples, default 1 —
# usleep is deterministic), KSWEEP (default "4 16 64"). For fast iteration during
# A1 dev: `KSWEEP=4 ./bench.sh`.

set -euo pipefail
cd "$(dirname "$0")"

require() {
    command -v "$1" >/dev/null 2>&1 || { echo "$1 not found — $2" >&2; exit 1; }
}
require karac   "cargo install --path ../.. --features llvm (then cp to ~/.local/bin)"
require python3 "needed for the JSON co-group check"

TO=""
if   command -v timeout  >/dev/null 2>&1; then TO="timeout 120"
elif command -v gtimeout >/dev/null 2>&1; then TO="gtimeout 120"; fi

D_US=${D_US:-200000}
RUNS=${RUNS:-1}
KSWEEP=${KSWEEP:-"4 16 64"}
DMS=$((D_US / 1000))
CORES=$( (command -v nproc >/dev/null && nproc) || sysctl -n hw.ncpu 2>/dev/null || echo "?")

GEN=()   # generated files to clean up on exit
cleanup() { rm -f "${GEN[@]}" 2>/dev/null || true; }
trap cleanup EXIT

# gen_straight K FILE — K independent blocking usleep statements, straight-line.
gen_straight() {
    local k="$1" f="$2" i
    { echo 'unsafe extern "C" { fn usleep(usecs: u32) -> i32; }'
      echo 'fn main() {'
      for ((i=0;i<k;i++)); do echo "    usleep($D_US);"; done
      echo '    println(0);'
      echo '}'
    } > "$f"; GEN+=("$f")
}
# gen_par K FILE — same K calls inside an explicit `par {}` (the ceiling).
gen_par() {
    local k="$1" f="$2" i
    { echo 'unsafe extern "C" { fn usleep(usecs: u32) -> i32; }'
      echo 'fn main() {'
      echo '    par {'
      for ((i=0;i<k;i++)); do echo "        usleep($D_US);"; done
      echo '    }'
      echo '    println(0);'
      echo '}'
    } > "$f"; GEN+=("$f")
}
# gen_straight_suspends K FILE — K independent `suspends` sleep_ms calls,
# straight-line. `sleep_ms` (std.time, A2a-2.2) parks on the reactor timer
# wheel instead of pinning an OS thread, so the `par` rail overlaps WITHOUT
# a thread per nap. The auto rail stays SERIAL until A2b lifts the
# (Suspends,Suspends) conflict — this rail makes that gap measurable.
gen_straight_suspends() {
    local k="$1" f="$2" i
    { echo 'fn main() {'
      for ((i=0;i<k;i++)); do echo "    sleep_ms($DMS);"; done
      echo '    println(0);'
      echo '}'
    } > "$f"; GEN+=("$f")
}
# gen_par_suspends K FILE — same K `sleep_ms` calls inside `par {}` (the
# timer-wheel overlap ceiling; proves the primitive overlaps).
gen_par_suspends() {
    local k="$1" f="$2" i
    { echo 'fn main() {'
      echo '    par {'
      for ((i=0;i<k;i++)); do echo "        sleep_ms($DMS);"; done
      echo '    }'
      echo '    println(0);'
      echo '}'
    } > "$f"; GEN+=("$f")
}

build_bin() { # SRC OUT [VAR=val ...]
    local src="$1" out="$2"; shift 2
    rm -f "$out" "${src%.kara}"
    env "$@" karac build "$src" >/dev/null 2>&1
    mv "${src%.kara}" "$out"; GEN+=("$out")
}
median_real() { # BIN — median of RUNS `real` seconds
    local bin="$1" t; local -a s=()
    # One warm run, discarded: a freshly-built binary's first exec pays
    # dyld/page-in cold start (~0.2s here) — that, not the sleep, is what
    # would otherwise corrupt a RUNS=1 single sample on the first K measured.
    $TO "./$bin" >/dev/null 2>&1 || true
    for _ in $(seq "$RUNS"); do
        t=$({ $TO /usr/bin/time -p "./$bin" >/dev/null; } 2>&1 | awk '/^real/{print $2}')
        s+=("${t:-99}")
    done
    printf '%s\n' "${s[@]}" | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'
}
plan_groups() { karac query concurrency "$1" 2>/dev/null | sed 's/.*"parallel_groups"://; s/}$//'; }
stmts_grouped() { # FILE.fn I J -> yes iff some single group holds BOTH I and J
    karac query concurrency "$1" 2>/dev/null | python3 -c '
import sys, json
i, j = int(sys.argv[1]), int(sys.argv[2])
try: g = json.load(sys.stdin).get("parallel_groups", [])
except Exception: print("err"); sys.exit()
print("yes" if any(i in s["statements"] and j in s["statements"] for s in g) else "no")
' "$2" "$3"
}
has_par_run() { nm "$1" 2>/dev/null | grep -q par_run && echo yes || echo no; }

echo "======================================================================"
echo " auto_par_io — auto-par I/O overlap + scaling   (D=${DMS}ms, cores≈${CORES})"
echo "======================================================================"
echo
echo "## blocks fan-out, K-sweep  (K independent usleep, want auto≈par at every K)"
printf '   %4s | %-8s | %7s | %7s | %7s | %s\n' K "grouped?" seq par auto verdict
printf '   %4s-+-%-8s-+-%7s-+-%7s-+-%7s-+-%s\n' "----" "--------" "-------" "-------" "-------" "-------"
for K in $KSWEEP; do
    gen_straight "$K" "blocks_K${K}.kara"
    gen_par      "$K" "blocks_par_K${K}.kara"
    build_bin "blocks_K${K}.kara"     "blocks_auto_${K}"
    build_bin "blocks_K${K}.kara"     "blocks_seq_${K}"  KARAC_AUTO_PAR=0
    build_bin "blocks_par_K${K}.kara" "blocks_par_${K}"
    G=$(stmts_grouped "blocks_K${K}.kara.main" 0 1)
    S=$(median_real "blocks_seq_${K}"); P=$(median_real "blocks_par_${K}"); A=$(median_real "blocks_auto_${K}")
    V=$(awk -v a="$A" -v s="$S" -v p="$P" 'BEGIN{print (a<=(s+p)/2)?"OVERLAP":"SERIAL"}')
    printf '   %4s | %-8s | %6ss | %6ss | %6ss | %s\n' "$K" "$G" "$S" "$P" "$A" "$V"
done
echo "   (par scales in pool-bounded waves: ~ceil(K/cores)×D — that is the honest ceiling)"

echo
echo "## suspends fan-out, K-sweep  (K independent sleep_ms — want auto≈par at every K)"
echo "##   A2b state: auto-par OVERLAPS standalone sleep_ms timer waits (par thread-block,"
echo "##   like blocks). Only a direct sleep_ms is exempt; channel recv / network / user"
echo "##   suspends wrappers stay serial (a channel recv lifted into a branch deadlocks)."
printf '   %4s | %-8s | %7s | %7s | %7s | %s\n' K "grouped?" seq par auto verdict
printf '   %4s-+-%-8s-+-%7s-+-%7s-+-%7s-+-%s\n' "----" "--------" "-------" "-------" "-------" "-------"
for K in $KSWEEP; do
    gen_straight_suspends "$K" "suspends_K${K}.kara"
    gen_par_suspends      "$K" "suspends_par_K${K}.kara"
    build_bin "suspends_K${K}.kara"     "suspends_auto_${K}"
    build_bin "suspends_K${K}.kara"     "suspends_seq_${K}"  KARAC_AUTO_PAR=0
    build_bin "suspends_par_K${K}.kara" "suspends_par_${K}"
    G=$(stmts_grouped "suspends_K${K}.kara.main" 0 1)
    S=$(median_real "suspends_seq_${K}"); P=$(median_real "suspends_par_${K}"); A=$(median_real "suspends_auto_${K}")
    # A2b: auto should track the par ceiling (same OVERLAP verdict as blocks).
    V=$(awk -v a="$A" -v s="$S" -v p="$P" 'BEGIN{print (a<=(s+p)/2)?"OVERLAP":"SERIAL"}')
    printf '   %4s | %-8s | %6ss | %6ss | %6ss | %s\n' "$K" "$G" "$S" "$P" "$A" "$V"
done
echo "   (A2b lifted (Suspends,Suspends) and exempts a standalone sleep_ms from the suspends"
echo "    boundary gate: timer waits overlap via par_run, like blocks. Channel recv / network"
echo "    parks / sleep_ms-wrapper fns stay serial. Network http_get fan-out is A2b-2.)"

echo
echo "## positive control  (positive_control.kara — distinct-resource shape)"
build_bin positive_control.kara pos_ctl
PG=$(plan_groups positive_control.kara.process_request); PR=$(has_par_run pos_ctl)
echo "   plan (process_request) : $PG"
echo "   par_run sym            : $PR"
if [ "$PR" = yes ] && [ "$PG" != "[]" ]; then
    echo "   => OK (harness detects auto-par when it fires)"
else
    echo "   => BROKEN RIG (auto-par should fire here; the K-sweep RED is meaningless)"
fi
echo
echo "Done. Re-run after each A1 step; every K verdict flips SERIAL→OVERLAP."
