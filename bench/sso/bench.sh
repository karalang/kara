#!/usr/bin/env bash
# sso — is small-string optimization a net win, and where does it lose?
#
# Self-calibrating: every rail is rebuilt and re-timed on the machine in front
# of you, back to back, so the verdict is a RATIO. Absolute milliseconds from
# this track are NOT comparable across machines or across days — see
# docs/spikes/small-string-optimization.md § "ABSOLUTE TIMINGS IN THIS DOC ARE
# NOT COMPARABLE ACROSS SESSIONS". One day's run of the lexer rail moved 3.4x
# for a reason unrelated to SSO while the SSO ratio held at 13-16%.
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
# pinned off -- see the tunables note below; earlier numbers were NOT pinned):
#   lexer  17.6% FASTER   lexlike  13% FASTER   substr  34% SLOWER
#   builder20 +15%   builder60 +12%   promote +8%   pfx_idx +15%   pfx_chars -10%
#
# THE OPEN QUESTION HAS MOVED. It used to be "where does lexlike's remaining 34%
# go?". lexlike no longer regresses at all -- it is a 12-14% WIN as of
# 2026-09-15, and the pin does not explain that, because lexlike is the one rail
# the analyzer never fans out (declined_memory_bound), so its old and new
# numbers are both sequential and directly comparable. Something between
# 2026-09-12 and 2026-09-15 fixed it and NOTHING HAS ATTRIBUTED IT; the
# candidates are B-2026-09-12-20's overlay fix, the growth-test fold (c1adb9c),
# and keeping Vec off the SSO path.
#
# The question now is SUBSTR, at +34%. It is the same slice/compare/discard
# shape as lexlike but through `String.substring`, and the two disagree by 47
# points. It read as +5% for as long as it was fanned out. It has never been
# profiled either. See the README for what has already been ruled out.
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
