#!/usr/bin/env bash
# sso — is small-string optimization a net win, and where does it lose?
#
# Self-calibrating: every rail is rebuilt and re-timed on the machine in front
# of you, back to back. Absolute milliseconds are obviously not comparable
# across machines or days.
#
# THE RATIOS ARE NOT COMPARABLE EITHER, and this header said the opposite until
# 2026-09-15. It claimed the back-to-back build made "the verdict a RATIO",
# disclaiming only the milliseconds — and the whole track relied on that. It is
# false. Measured on two containers with the SAME COMPILER (karac built at
# 27466ba, run on both):
#
#   rail        host A           host B (nproc=4)   
#   lexlike     -12.7 / -13.7%   +22.9%             36 pts, SIGN FLIPS
#   substr      +33.7 / +34.3%   +73.6%             doubles
#   builder20   +15.1 / +14.1%   +3.1%              12 pts better
#   promote     +7.8 / +7.9%     +0.0%              
#   pfx_idx     +14.1 / +15.9%   +15.9%             portable, near-exactly
#
# A rail's SSO ratio is a property of the rail AND the host: different rails are
# bound by different subsystems, so a different CPU allocation reprices them
# non-uniformly — the malloc-dominated rails got relatively cheaper against
# SSO's overhead while the memory-bound ones got worse. That is also why
# opposite-direction movement is NOT evidence against a host change, which is
# the wrong inference that delayed finding this (B-2026-09-15-6).
#
# PRACTICAL RULE: never compare a rail delta against a number from a previous
# session's table. Re-run both legs on one machine. What DOES survive across
# hosts is the GAP between two rails measured together — substr minus lexlike
# is 46-48 pts on host A and 49-51 on host B — so differentials are the durable
# unit here, not levels.
#
# Three workloads, chosen because they disagree:
#
#   lexer   selfhost lexer over a large real input. Allocation-dominated, and
#           strings travel BY VALUE into callees (keyword_or_ident(text: String),
#           make_spanned(token: Token)) -- two consuming calls per token. SSO WINS.
#   lexlike slice a 3-byte token, compare it to a keyword, discard. No consuming
#           call. SSO LOSES.
#   substr  the same shape via `String.substring` rather than slice syntax.
#           SSO is a wash.
#   builder20/60  build a String by pushing one char at a time. The receiver is
#           heap from its first push and never inline, so this is SSO's TAX on a
#           mutating loop with no possible win. B-2026-09-14-20's regression.
#   promote an inline receiver (from `substring`) mutated once — the promotion
#           path itself. Read as a PAIR with builder: moving cost from one to
#           the other is not a win.
#
# Last measured on x86-64 (2026-09-15, KARAC_SSO=1 vs =0, RUNS=15, auto-par
# pinned off -- see the tunables note below; numbers from before 2026-09-15 were
# NOT pinned and are not comparable to these):
#   lexer  -17.6%   lexlike  -13%   substr  +31..34%
#   builder20 +15%   builder60 +12%   promote +8%   pfx_idx +15%   pfx_chars -10%
#
# DO NOT TRUST A PRE-2026-09-15 NUMBER FROM THIS HEADER, and do not re-measure
# to "confirm" one. This block used to read "lexlike 34% SLOWER (2026-09-12)",
# and that figure was never reproducible: bench/sso/lexlike.kara was committed
# by 5bcafe9 at 09-13 00:04, 1h42m AFTER the commit the number was attributed
# to, and 5bcafe9 wrote both the rail and the header line. The figure described
# an uncommitted pre-harness workload, not the rail beneath it. Building the
# committed rail at five commits spanning 09-12..09-15 gives -12 to -14% at
# every one of them, flat. Recorded as B-2026-09-15-1 (invalid).
#
# THE OPEN QUESTION is SUBSTR, and it is sharper than a magnitude. At ONE
# commit, `substr` is a 14.7% SSO WIN under default auto-par and a 30.9% SSO
# LOSS pinned -- same rail, same compiler, a sign flip decided by the scheduler
# (B-2026-09-15-6). Pinned it is the worst rail in the track, while `lexlike` --
# the SAME slice/compare/discard shape, reached through slice syntax instead of
# `String.substring` -- wins 13%. Nothing attributes that 44-point gap and no
# profiler has ever been pointed at this track.
#
# See the README for what has already been ruled out.
#
# Tunables (env): ITERS (microbench iterations, default 10000000), PASSES (lexer
# passes over the input, default 200), RUNS (samples per rail, default 7),
# KARAC (compiler path, default target/release/karac then target/debug/karac).
#
# AUTO-PAR IS PINNED OFF, and that is a CONTROL rather than a preference. The
# spike doc's kata sweep states it as one -- "KARAC_AUTO_PAR=0 is held fixed,
# since auto-par is a third surface and letting it vary would make any
# difference unattributable" -- but this script did not apply it until
# 2026-09-15, so every rail number published before that date was measured with
# the driver loops FANNED OUT. Each rail's `main` accumulates
# `total = total + s.len()`, which the analyzer reads as a `+` reduction and
# parallelizes at these iteration counts; both legs of a comparison fanned out
# equally, so the SIGNS held, but the magnitudes were parallel-throughput
# deltas and understated per-iteration cost. Check with
# `karac build --concurrency-report <rail>.kara`. Set KARAC_AUTO_PAR=1 here
# only to measure the thread pool on purpose.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BUILD="$HERE/build"
ITERS="${ITERS:-10000000}"
# The builder/promote rails allocate a String per iteration, so they need three
# orders of magnitude fewer iterations than the slice-compare rails to land in
# the same wall-clock range. Scaled off ITERS so one env var still moves
# everything together.
BITERS="${BITERS:-$(( ITERS / 4 ))}"
PASSES="${PASSES:-200}"
RUNS="${RUNS:-7}"
# See the header: this is a control, not a preference.
AUTO_PAR="${KARAC_AUTO_PAR:-0}"

if [ -z "${KARAC:-}" ]; then
  for c in "$ROOT/target/release/karac" "$ROOT/target/debug/karac"; do
    [ -x "$c" ] && KARAC="$c" && break
  done
fi
if [ -z "${KARAC:-}" ] || [ ! -x "$KARAC" ]; then
  echo "no karac binary — build one first: cargo build --features llvm" >&2
  exit 1
fi

rm -rf "$BUILD"; mkdir -p "$BUILD/lexer/src"

# --- lexer rail: build a driver project against the REAL selfhost lexer -------
# Copied at run time rather than vendored, so the benchmark cannot drift out of
# sync with the lexer it claims to measure.
for f in lexer token span; do cp "$ROOT/selfhost/src/$f.kara" "$BUILD/lexer/src/"; done
sed "s/while i < 200/while i < $PASSES/" "$HERE/driver.kara" > "$BUILD/lexer/src/main.kara"
cat > "$BUILD/lexer/kara.toml" <<'TOML'
[package]
name = "lexprof"
version = "0.1.0"
authors = []
edition = "2026"

[dependencies]
TOML
# The input is GENERATED, not committed: a concatenation of the selfhost sources.
# Its exact size therefore tracks the tree and will not match any absolute figure
# recorded on another day. That is fine and deliberate — the ratio is the claim.
cat "$ROOT"/selfhost/src/*.kara > "$BUILD/lexer/input.kara"
echo "lexer input: $(wc -c < "$BUILD/lexer/input.kara") bytes x $PASSES passes"

# --- microbench rails ---------------------------------------------------------
# name:source:iters:len — `len` is "" for the rails that take no LEN.
MICRO=(
  "lexlike:lexlike:$ITERS:"
  "substr:substr:$ITERS:"
  "builder20:builder:$BITERS:20"
  "builder60:builder:$BITERS:60"
  "promote:promote:$BITERS:20"
  "pfx_idx:pfx_idx:$BITERS:20"
  "pfx_chars:pfx_chars:$BITERS:20"
)
for spec in "${MICRO[@]}"; do
  IFS=: read -r name srcf it ln <<< "$spec"
  sed -e "s/ITERS/$it/" -e "s/LEN/$ln/" "$HERE/$srcf.kara" > "$BUILD/$name.kara"
done

# best-of-RUNS wall time in ms. Deletes the binary and asserts it reappeared:
# a build that silently fails must not leave a stale binary to be timed as if it
# were fresh (that hole reported one encoder's numbers for another's).
timeit() { # $1=binary
  local bin="$1" best=99999999 t0 t1 ms
  for _ in $(seq 1 "$RUNS"); do
    t0=$(date +%s%N); "$bin" >/dev/null 2>&1; t1=$(date +%s%N)
    ms=$(( (t1 - t0) / 1000000 )); [ "$ms" -lt "$best" ] && best=$ms
  done
  echo "$best"
}

printf '\n%-10s %10s %10s %10s   %s\n' rail SSO=0 SSO=1 delta note
printf -- '---------- ---------- ---------- ----------   ----\n'

declare -A T
for sso in 0 1; do
  # lexer
  ( cd "$BUILD/lexer" && rm -f lexprof && KARAC_SSO=$sso KARAC_AUTO_PAR=$AUTO_PAR "$KARAC" build >/dev/null 2>&1 )
  [ -x "$BUILD/lexer/lexprof" ] || { echo "lexer rail SSO=$sso: BUILD PRODUCED NO BINARY" >&2; exit 1; }
  mv "$BUILD/lexer/lexprof" "$BUILD/lexer/rail$sso"
  T[lexer$sso]=$( cd "$BUILD/lexer" && timeit "./rail$sso" )
  # microbenches
  for spec in "${MICRO[@]}"; do
    m="${spec%%:*}"
    ( cd "$BUILD" && rm -f "$m" && KARAC_SSO=$sso KARAC_AUTO_PAR=$AUTO_PAR "$KARAC" build "$m.kara" >/dev/null 2>&1 )
    [ -x "$BUILD/$m" ] || { echo "$m rail SSO=$sso: BUILD PRODUCED NO BINARY" >&2; exit 1; }
    mv "$BUILD/$m" "$BUILD/$m.rail$sso"
    T[$m$sso]=$( timeit "$BUILD/$m.rail$sso" )
  done
done

for rail in lexer lexlike substr builder20 builder60 promote pfx_idx pfx_chars; do
  a=${T[${rail}0]}; b=${T[${rail}1]}
  pct=$(awk -v a="$a" -v b="$b" 'BEGIN{ printf "%+.1f%%", (b-a)*100.0/a }')
  note=$(awk -v a="$a" -v b="$b" 'BEGIN{ print (b<a) ? "SSO wins" : "SSO loses" }')
  printf '%-10s %9sms %9sms %10s   %s\n' "$rail" "$a" "$b" "$pct" "$note"
done
echo
echo "delta is SSO=1 relative to SSO=0; negative means SSO is faster."
