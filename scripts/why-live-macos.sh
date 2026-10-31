#!/usr/bin/env bash
# Why does a Mach-O executable built by `karac build` still carry std's panic
# hook (B-2026-10-30-37)? Builds the panic-free floor's print/Vec program with
# symbols kept, lists the panic-hook symbols and the image's exports, then
# relinks with ld64's `-why_live` on each hook symbol, which prints the chain
# of references that keeps it alive. Diagnosis only: it never fails.
set -u
cd "$(dirname "$0")/.."
KARAC="${KARAC:-target/debug/karac}"
DIR="$(mktemp -d)"
cat > "$DIR/main.kara" <<'KARA'
fn main() {
    let mut v: Vec[i64] = Vec.new();
    for i in 0..1000 {
        v.push(i * i);
    }
    let mut t = 0;
    for x in v {
        t += x;
    }
    println(t);
    print("done\n");
}
KARA
export KARAC_AUTO_PAR=0
KARAC_DEBUG_INFO=1 "$KARAC" build "$DIR/main.kara" -o "$DIR/main.out" < /dev/null
ls -l "$DIR/main.out"
echo "== RUST_BACKTRACE occurrences (the floor's test)"
grep -c RUST_BACKTRACE "$DIR/main.out" || true
echo "== exported symbols"
nm -gU "$DIR/main.out" | head -20
echo "== panic-hook symbols"
SYMS="$(nm "$DIR/main.out" | awk '{print $NF}' | grep -E 'default_hook|rust_panic_with_hook|begin_panic_handler|panic_fmt' | sort -u)"
echo "$SYMS"
ARGS=""
for s in $(echo "$SYMS" | head -4); do
    ARGS="$ARGS -Wl,-why_live,$s"
done
echo "== why live"
KARAC_DEBUG_INFO=1 KARAC_LINK_ARGS="$ARGS" "$KARAC" build "$DIR/main.kara" -o "$DIR/why.out" < /dev/null 2>&1 | head -200
exit 0
