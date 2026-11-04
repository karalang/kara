#!/usr/bin/env bash
# The `cargo test --features llvm` gate leg, one test target at a time.
#
# Since LLVM 22 (6dcdc1a604) a cloud container's ~38 GiB allowance holds
# ~24 GiB before the leg starts, and linking every test binary at once runs
# out of disk ("No space left on device", `ld terminated with signal 7`).
# This runs `--lib`, `--bins`, each `tests/*.rs` and `--doc` in turn and
# deletes each test binary after it runs, so only one is on disk at a time.
#
# usage: bash scripts/llvm-leg-split.sh [log]   (default log: target/llvm-leg.log)
# Honors the environment the plain leg does (CARGO_PROFILE_TEST_DEBUG,
# KARAC_REQUIRE_RUNTIME_ARCHIVE, KARAC_HASH_SEED, ...); defaults
# CARGO_PROFILE_TEST_DEBUG to line-tables-only. Exits 1 if any target fails,
# and prints the pass/fail totals summed over every `test result:` line.
set -u
cd "$(dirname "$0")/.."
LOG="${1:-target/llvm-leg.log}"
mkdir -p "$(dirname "$LOG")"
: > "$LOG"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-line-tables-only}"
failed_targets=()
run() {
    if ! cargo test --features llvm --no-fail-fast "$@" >> "$LOG" 2>&1; then
        failed_targets+=("$*")
    fi
}
run --lib
run --bins
for f in tests/*.rs; do
    t=$(basename "$f" .rs)
    run --test "$t"
    rm -f target/debug/deps/"$t"-* 2>/dev/null
done
run --doc
read -r passed failed < <(awk '/^test result:/ {
    for (i = 1; i < NF; i++) {
        if ($(i + 1) ~ /^passed/) p += $i
        if ($(i + 1) ~ /^failed/) f += $i
    }
} END { print p + 0, f + 0 }' "$LOG")
results=$(grep -c '^test result:' "$LOG")
echo "llvm leg: $results result lines, $passed passed, $failed failed (log: $LOG)"
grep -E '^test .* FAILED' "$LOG" | head -20
if [ ${#failed_targets[@]} -ne 0 ]; then
    echo "failing targets: ${failed_targets[*]}"
    exit 1
fi
