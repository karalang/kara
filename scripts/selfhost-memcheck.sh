#!/usr/bin/env bash
# Memory-check ONE .kara program through the SELF-HOSTED emitter
# (selfhost/src/codegen.kara), natively on macOS.
#
# Usage:
#   scripts/selfhost-memcheck.sh <program.kara>
#   scripts/selfhost-memcheck.sh <program.kara> --keep   # keep the emitted IR
#
# WHY THIS EXISTS. The port's own oracle (tests/selfhost_codegen.rs) diffs the
# emitted program's stdout against the seed. That catches wrong answers and
# crashes, but it is blind to two whole classes:
#
#   · a use-after-free that happens not to trip. B-2026-07-27-15 was filed as
#     INTERMITTENT — the same IR exited 0 three times and aborted twice — with
#     the warning that "one green run proves nothing". Programs in that class
#     look green.
#   · a leak. Output parity is unaffected, so the oracle cannot see it at all.
#     B-2026-07-27-12 sat open across three slices for exactly this reason.
#
# Neither has to be a matter of luck. The emitted IR needs only libc plus one
# f64 helper, so clang can build it directly and both classes become
# deterministic:
#
#   ASAN leg    `clang -fsanitize=address -x ir` — reports the use-after-free
#               on every run and NAMES the freeing frame, so you get
#               "sh_release_6 freed what u_render returned" instead of SIGTRAP.
#   leaks leg   a non-ASAN build of the same IR under `leaks --atExit`.
#               macOS has no LeakSanitizer, so this is the native leak signal
#               on this box. (`leaks` and ASAN's allocator do not compose,
#               hence two builds.)
#
# SCOPE. This checks the SELF-HOSTED emitter's output. For the Rust codegen's
# leak gate use scripts/lsan-local.sh (Linux ASAN+LSan in a container), whose
# CI equivalent is authoritative. `leaks` and valgrind account differently and
# scan the stack differently, so treat a 0 here as "no leak this tool can see",
# not as proof — cross-check anything load-bearing against the Linux leg.
#
# The macOS header rewrite below is the only edit made to the emitted IR: the
# port hardcodes a Linux triple and the glibc `@stdout`, which will not resolve
# against libSystem. Function bodies are untouched, so what runs is exactly what
# the emitter produced.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROG="${1:?usage: selfhost-memcheck.sh <program.kara> [--keep]}"
KEEP="${2:-}"

KARAC="$REPO/target/debug/karac"
[ -x "$KARAC" ] || { echo "no $KARAC — run: cargo build --features llvm"; exit 2; }

TMP="$(mktemp -d)"
[ "$KEEP" = "--keep" ] || trap 'rm -rf "$TMP"' EXIT

# ---- 1. build a driver that runs the emitter over PROG and prints its IR ----
mkdir -p "$TMP/src"
printf '[package]\nname = "cg"\nversion = "0.1.0"\n' > "$TMP/kara.toml"
for f in span.kara token.kara lexer.kara ast.kara parser.kara codegen.kara; do
  cp "$REPO/selfhost/src/$f" "$TMP/src/$f"
done
python3 - "$PROG" > "$TMP/src/main.kara" <<'PY'
import sys
src = open(sys.argv[1]).read()
esc = src.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
print("import parser.parse_program;")
print("import codegen.emit_program;")
print("")
print("fn main() with panics {")
print(f'    print(emit_program(parse_program("{esc}")));')
print("}")
PY

( cd "$TMP" && "$KARAC" build ) > "$TMP/build.log" 2>&1 || true
if [ ! -x "$TMP/cg" ]; then
  echo "!! emitter driver failed to build"; tail -25 "$TMP/build.log"; exit 1
fi
"$TMP/cg" > "$TMP/raw.ll" 2> "$TMP/emit.err" || {
  echo "!! emitter panicked"; cat "$TMP/emit.err"; exit 1; }

# ---- 2. header-only rewrite, macOS ONLY ----
# The port emits a Linux triple and glibc's `@stdout`, neither of which resolves
# against libSystem. On Linux that header is already correct and rewriting it
# would BREAK the link (there is no `@__stdoutp` in glibc), so this is gated on
# the host. Either way only the header changes — function bodies are untouched,
# so what runs is exactly what the emitter produced.
if [ "$(uname -s)" = "Darwin" ]; then
  python3 - "$TMP/raw.ll" > "$TMP/prog.ll" <<'PY'
import sys, re
ir = open(sys.argv[1]).read()
ir = re.sub(r'^target (datalayout|triple) = .*\n', '', ir, flags=re.M)
ir = ir.replace('@stdout = external global ptr', '@__stdoutp = external global ptr')
ir = re.sub(r'(?<![\w.$])@stdout(?![\w.$])', '@__stdoutp', ir)
sys.stdout.write(ir)
PY
else
  cp "$TMP/raw.ll" "$TMP/prog.ll"
fi

# The one runtime symbol the emitted IR needs beyond libc.
cat > "$TMP/stub.c" <<'C'
#include <stdio.h>
long karac_runtime_f64_to_str(double v, char *buf, long cap) {
    int n = snprintf(buf, (size_t)cap, "%g", v);
    return n < 0 ? 0 : (long)n;
}
C

# ---- 3. seed output, for parity alongside the memory verdicts ----
echo "== seed (karac run) =="
"$KARAC" run "$PROG" 2>&1 | sed 's/^/   /'

# ---- 4. ASAN leg ----
echo "== emitted, under AddressSanitizer =="
if clang -fsanitize=address -O0 -g -o "$TMP/asan" -x ir "$TMP/prog.ll" -x c "$TMP/stub.c" 2>"$TMP/asan.cc.err"; then
  set +e
  # An ASAN abort is the EXPECTED outcome here. A shell prints "Abort trap: 6"
  # for a foreground child it spawned DIRECTLY and that dies by signal, which
  # reads like a failure of this script — and a plain subshell does not help,
  # since it is the parent that prints. Interposing `bash -c` does: the inner
  # shell prints the notice to ITS stderr (dropped here) and then exits 134
  # normally, so the outer shell sees an ordinary nonzero exit and stays quiet.
  bash -c 'ASAN_OPTIONS=detect_leaks=0 "$1" > "$2" 2>&1' _ \
    "$TMP/asan" "$TMP/asan.out" 2>/dev/null
  rc=$?
  set -e
  sed 's/^/   /' "$TMP/asan.out"
  if grep -q 'ERROR: AddressSanitizer' "$TMP/asan.out"; then
    echo "   VERDICT: ASAN ERROR (exit $rc)"
  else
    echo "   VERDICT: clean (exit $rc)"
  fi
else
  echo "   !! clang could not build the emitted IR"; sed 's/^/   /' "$TMP/asan.cc.err"
fi

# ---- 5. leak leg (separate, non-ASAN build — the two allocators don't compose)
# macOS has `leaks`, Linux has valgrind. Both answer the same question; valgrind
# is the stronger of the two and is what the port's oracle prescribes.
if [ "$(uname -s)" != "Darwin" ]; then
  echo "== emitted, under valgrind --leak-check=full =="
  if ! command -v valgrind >/dev/null 2>&1; then
    echo "   skipped: valgrind not installed (apt-get install valgrind)"
  elif clang -O0 -g -o "$TMP/plain" -x ir "$TMP/prog.ll" -x c "$TMP/stub.c" 2>/dev/null; then
    set +e
    valgrind --leak-check=full --error-exitcode=0 "$TMP/plain" > /dev/null 2> "$TMP/vg.out"
    set -e
    grep -E 'definitely lost|indirectly lost|ERROR SUMMARY' "$TMP/vg.out" | sed 's/^/   /' \
      || echo "   (no valgrind summary)"
  else
    echo "   !! clang could not build the emitted IR"
  fi
  exit 0
fi

echo "== emitted, under leaks --atExit =="
if clang -O0 -g -o "$TMP/plain" -x ir "$TMP/prog.ll" -x c "$TMP/stub.c" 2>/dev/null; then
  # `leaks` EXITS NONZERO when it finds something, and `pipefail` is on, so
  # capture first and inspect after — piping it straight into grep makes the
  # leak-found case look like a script failure. The summary line reads "N leak
  # for"/"N leaks for" depending on count, so key off the invariant tail.
  set +e
  MallocStackLogging=1 leaks --atExit -- "$TMP/plain" > "$TMP/leaks.out" 2>/dev/null
  set -e
  if grep -qE 'total leaked bytes' "$TMP/leaks.out"; then
    grep -E 'total leaked bytes' "$TMP/leaks.out" | sed 's/^/   /'
  else
    echo "   (no leaks summary — is leaks(1) available?)"
  fi
else
  echo "   !! clang could not build the emitted IR"
fi

[ "$KEEP" = "--keep" ] && echo "== kept: $TMP/prog.ll =="
exit 0
