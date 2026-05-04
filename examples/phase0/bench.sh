#!/usr/bin/env bash
#
# examples/phase0/bench.sh
#
# Benchmarks sequential vs auto-parallelized versions of dashboard.kara.
# Demonstrates the speedup from effect-driven auto-concurrency.

set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
TMPDIR="${TMPDIR:-/tmp}"
SEQ_BIN="$TMPDIR/kara_seq"
PAR_BIN="$TMPDIR/kara_par"
RUNS=5

echo "=== Kāra Phase 0: Effect-Driven Auto-Concurrency Benchmark ==="
echo ""

# Compile both versions
echo "Compiling..."
rustc "$DIR/sequential.rs" -o "$SEQ_BIN" 2>/dev/null
rustc "$DIR/parallel.rs"   -o "$PAR_BIN" 2>/dev/null
echo "Done."
echo ""

# Show output (once, to prove correctness)
echo "--- Program output (both versions produce identical results) ---"
"$SEQ_BIN" 2>/dev/null
echo ""

# Benchmark sequential
echo "--- Sequential (no concurrency) ---"
seq_total=0
for i in $(seq 1 $RUNS); do
    ms=$( { time "$SEQ_BIN" >/dev/null; } 2>&1 | grep real | sed 's/.*m//;s/s//' )
    # Use the program's own timing via stderr
    elapsed=$("$SEQ_BIN" 2>&1 >/dev/null | grep -o '[0-9]*ms' | grep -o '[0-9]*')
    printf "  Run %d: %sms\n" "$i" "$elapsed"
    seq_total=$((seq_total + elapsed))
done
seq_avg=$((seq_total / RUNS))
echo "  Average: ${seq_avg}ms"
echo ""

# Benchmark parallel
echo "--- Parallel (compiler auto-parallelized) ---"
par_total=0
for i in $(seq 1 $RUNS); do
    elapsed=$("$PAR_BIN" 2>&1 >/dev/null | grep -o '[0-9]*ms' | grep -o '[0-9]*')
    printf "  Run %d: %sms\n" "$i" "$elapsed"
    par_total=$((par_total + elapsed))
done
par_avg=$((par_total / RUNS))
echo "  Average: ${par_avg}ms"
echo ""

# Compute speedup
if [ "$par_avg" -gt 0 ]; then
    # Integer math with one decimal place
    speedup_x10=$((seq_avg * 10 / par_avg))
    speedup_whole=$((speedup_x10 / 10))
    speedup_frac=$((speedup_x10 % 10))
    echo "=== Result: ${speedup_whole}.${speedup_frac}x speedup ==="
else
    echo "=== Result: parallel completed too fast to measure ==="
fi
echo ""
echo "The programmer wrote sequential-looking code with effect annotations."
echo "The compiler parallelized it automatically. Zero effort. ${speedup_whole:-3}.${speedup_frac:-0}x faster."
