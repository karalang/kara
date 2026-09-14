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
# Last measured on x86-64 (2026-09-12, KARAC_SSO=1 vs =0):
#   lexer   15.8% FASTER      lexlike  34% SLOWER      substr  5% SLOWER
#
# THE OPEN QUESTION this track exists to answer: where does lexlike's remaining
# 34% go? Half of what used to be a 66% regression turned out to be a
# store-to-load forwarding stall in the inline encoder, removed as a side effect
# of an unrelated correctness fix (B-2026-09-12-20). That is reason to suspect
# the rest is also an implementation artifact rather than inherent to the
# representation -- but it needs a PROFILER to attribute, and it has never been
# profiled. See the README for what has already been ruled out, so you do not
# re-run those.
#
# Tunables (env): ITERS (microbench iterations, default 10000000), PASSES (lexer
# passes over the input, default 200), RUNS (samples per rail, default 7),
# KARAC (compiler path, default target/release/karac then target/debug/karac).

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
  ( cd "$BUILD/lexer" && rm -f lexprof && KARAC_SSO=$sso "$KARAC" build >/dev/null 2>&1 )
  [ -x "$BUILD/lexer/lexprof" ] || { echo "lexer rail SSO=$sso: BUILD PRODUCED NO BINARY" >&2; exit 1; }
  mv "$BUILD/lexer/lexprof" "$BUILD/lexer/rail$sso"
  T[lexer$sso]=$( cd "$BUILD/lexer" && timeit "./rail$sso" )
  # microbenches
  for spec in "${MICRO[@]}"; do
    m="${spec%%:*}"
    ( cd "$BUILD" && rm -f "$m" && KARAC_SSO=$sso "$KARAC" build "$m.kara" >/dev/null 2>&1 )
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
