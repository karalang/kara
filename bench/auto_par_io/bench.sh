#!/usr/bin/env bash
# auto_par_io — does auto-par overlap independent I/O? (before/after harness for path A)
#
# Companion to ./README.md. Measures whether the compiler's auto-parallelizer
# overlaps independent BLOCKING statements, the way design.md:5907 / :9044 say
# it should. Self-calibrating: every run re-measures three rails on the same
# machine back-to-back, so the verdict is a RATIO (load- and thermal-immune),
# not an absolute number against a stale baseline.
#
#   seq  rail : KARAC_AUTO_PAR=0 build of blocks_fanout.kara   (serial FLOOR)
#   par  rail : build of blocks_fanout_par.kara (explicit par {})  (CEILING)
#   auto rail : default build of blocks_fanout.kara            (UNDER TEST)
#
# Two deterministic guards (exact, not timed): the `query concurrency` plan and
# the presence of the `karac_par_run` symbol in the auto binary. Wall-clock just
# confirms the grouping turns into real overlap.
#
# BEFORE A1: auto ≈ seq (serial), plan [], no par_run.
# AFTER  A1: auto ≈ par (overlap), plan [[0,1,2,3]], par_run present.

set -euo pipefail
cd "$(dirname "$0")"

require() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "$1 not found — install with: $2" >&2
        exit 1
    fi
}
require karac "cargo install --path ../.. --features llvm  (then cp to ~/.local/bin)"

# Optional hard timeout so a hung run frees the harness (bench discipline).
TO=""
if command -v timeout  >/dev/null 2>&1; then TO="timeout 30";
elif command -v gtimeout >/dev/null 2>&1; then TO="gtimeout 30"; fi

RUNS=3   # median of N; deterministic signals need only 1, wall-clock wants a few

# build_bin SRC OUT [VAR=val ...]  — build SRC, rename the stem binary to OUT.
build_bin() {
    local src="$1" out="$2"; shift 2
    rm -f "$out" "${src%.kara}"
    env "$@" karac build "$src" >/dev/null 2>&1
    mv "${src%.kara}" "$out"
}

# median_real BIN — run BIN RUNS times, echo the median `real` seconds.
median_real() {
    local bin="$1" t; local -a samples=()
    $bin >/dev/null 2>&1 || true   # warm
    for _ in $(seq "$RUNS"); do
        t=$({ $TO /usr/bin/time -p "./$bin" >/dev/null; } 2>&1 | awk '/^real/{print $2}')
        samples+=("${t:-99}")
    done
    printf '%s\n' "${samples[@]}" | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'
}

# plan_groups FILE.fn  — echo the parallel_groups array (compact).
plan_groups() {
    karac query concurrency "$1" 2>/dev/null | sed 's/.*"parallel_groups"://; s/}$//'
}

# stmts_grouped FILE.fn I J — "yes" iff some single group contains BOTH stmt I
# and stmt J. This is the precise A1 signal for the blocks probe: are the
# independent blocking calls actually co-grouped (vs. raw par_run presence,
# which fires even for a useless trivial group like [last-usleep, println]).
stmts_grouped() {
    karac query concurrency "$1" 2>/dev/null | python3 -c '
import sys, json
i, j = int(sys.argv[1]), int(sys.argv[2])
try:
    g = json.load(sys.stdin).get("parallel_groups", [])
except Exception:
    print("err"); sys.exit()
print("yes" if any(i in s["statements"] and j in s["statements"] for s in g) else "no")
' "$2" "$3"
}

has_par_run() { nm "$1" 2>/dev/null | grep -q par_run && echo yes || echo no; }

echo "=================================================================="
echo " auto_par_io — auto-par I/O overlap  (K=4 blocking calls, D=400ms)"
echo "=================================================================="

# ── Probe 1: blocks fan-out (the A1 target) ──────────────────────────
echo
echo "## blocks fan-out  (blocks_fanout.kara — 4× independent usleep)"
build_bin blocks_fanout.kara     blocks_auto
build_bin blocks_fanout.kara     blocks_seq   KARAC_AUTO_PAR=0
build_bin blocks_fanout_par.kara blocks_par

echo "   plan (auto)        : $(plan_groups blocks_fanout.kara.main)"
echo "   blocking grouped?  : $(stmts_grouped blocks_fanout.kara.main 0 1)  (stmts 0 & 1 share a group — the A1 signal)"
SEQ=$(median_real blocks_seq)
PAR=$(median_real blocks_par)
AUTO=$(median_real blocks_auto)
echo "   seq  (floor)   : ${SEQ}s"
echo "   par  (ceiling) : ${PAR}s"
echo "   auto (test)    : ${AUTO}s"
# Verdict: is auto nearer the ceiling or the floor? midpoint split.
VERDICT=$(awk -v a="$AUTO" -v s="$SEQ" -v p="$PAR" \
    'BEGIN{ mid=(s+p)/2; print (a<=mid) ? "OVERLAPPING (auto reached ceiling — A1 satisfied)" \
                                        : "SERIAL (auto stuck at floor — A1 pending)" }')
echo "   => $VERDICT"

# ── Probe 2: positive control (rig can SEE auto-par fire) ────────────
echo
echo "## positive control  (positive_control.kara — distinct-resource shape)"
build_bin positive_control.kara pos_ctl
PG=$(plan_groups positive_control.kara.process_request)
PR=$(has_par_run pos_ctl)
echo "   plan (process_request) : $PG"
echo "   par_run sym            : $PR"
if [ "$PR" = yes ] && [ "$PG" != "[]" ]; then
    echo "   => OK (harness detects auto-par when it fires)"
else
    echo "   => BROKEN RIG (auto-par should fire here; blocks RED above is meaningless)"
fi

rm -f blocks_auto blocks_seq blocks_par pos_ctl
echo
echo "Done. Re-run after each A1 step; the blocks verdict flips floor→ceiling."
