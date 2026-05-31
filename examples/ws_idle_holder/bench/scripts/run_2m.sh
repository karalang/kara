#!/usr/bin/env bash
# N=2M c=64 idle-hold run for the ws_idle_holder bench harness — the
# headline ceiling sweep for a single r8g.4xlarge (or equivalent
# Linux box). Mirrors run_1m.sh's flag set so the comparison stays
# apples-to-apples; only N, the source-IP fan-out, and the inline
# ulimit differ.
#
# Usage:
#   run_2m.sh <server-bin> [output.json]
#
# - server-bin: absolute path to the server binary under test. Either
#   the Kāra demo binary (examples/ws_idle_holder/ws_idle_holder,
#   produced by `karac build`) or the Rust comparator
#   (examples/ws_idle_holder/rust/target/release/ws-idle-holder-rust).
#   The Rust comparator IS run at 2M as the credibility comparator
#   (decision 2026-05-30): an empirical head-to-head at the 2M ceiling
#   is rhetorically stronger than extrapolating the per-conn-bytes
#   ratio from 1M, and it validates Rust per-conn-bytes linearity at
#   scale (rustls session cache, tokio task accounting). Commercial
#   comparators (Phoenix/Java/Go/.NET/Node) stay at 250K — only the
#   credibility comparator tracks Kāra's full ceiling.
# - output.json: optional, defaults to "<basename>-2m.json" in cwd.
#
# Prereq: scripts/ec2_setup.sh (sysctls + 50 loopback aliases + nofile
# limits.d at 3M). This script also calls `ulimit -n 3000000` inline as
# a safety net for the current shell.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <server-bin> [output.json]" >&2
    exit 1
fi

SERVER_BIN="$1"
OUTPUT="${2:-$(basename "$SERVER_BIN")-2m.json}"

if [[ ! -x "$SERVER_BIN" ]]; then
    echo "$0: $SERVER_BIN not found or not executable" >&2
    exit 1
fi

# Resolve the bench harness binary relative to this script.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BENCH_BIN="$BENCH_DIR/target/release/ws-idle-holder-bench"

if [[ ! -x "$BENCH_BIN" ]]; then
    echo "$0: bench harness not built at $BENCH_BIN" >&2
    echo "    run: (cd $BENCH_DIR && cargo build --release)" >&2
    exit 1
fi

# Source IPs: 127.0.0.2..51 (50 IPs). 50 × ~50K ports ≈ 2.52M source
# tuples — 24% headroom over the 2M target, which absorbs ephemeral
# port range exclusions and any TIME_WAIT churn during the run.
# Requires loopback aliases from ec2_setup.sh.
SOURCE_IPS=""
for i in $(seq 2 51); do
    SOURCE_IPS="${SOURCE_IPS:+${SOURCE_IPS},}127.0.0.${i}"
done

# Raise fd limit inline (needs hard limit from ec2_setup.sh's limits.d
# entry to actually take effect at 3M).
ulimit -n 3000000 2>/dev/null || true

echo "[run_2m] server-bin  : $SERVER_BIN"
echo "[run_2m] bench-bin   : $BENCH_BIN"
echo "[run_2m] output      : $OUTPUT"
echo "[run_2m] ulimit -n   : $(ulimit -n)"
echo "[run_2m] starting at : $(date -u +%FT%TZ)"
echo

"$BENCH_BIN" \
    --server-bin "$SERVER_BIN" \
    -n 2000000 \
    --concurrency 64 \
    --churn-rounds 0 \
    --connect-timeout-ms 30000 \
    --source-ips "$SOURCE_IPS" \
    | tee "$OUTPUT"

echo
echo "[run_2m] complete at : $(date -u +%FT%TZ)"
echo "[run_2m] JSON written: $OUTPUT"

# Post-run diagnostic: any SYN flood / cookie messages? If yes the
# listen-backlog is being saturated and the connect-tail latencies are
# distorted by 1s SYN retransmits.
if command -v dmesg >/dev/null 2>&1; then
    echo
    echo "[run_2m] dmesg tail (last 20 lines):"
    if dmesg | tail -20 2>/dev/null; then
        :
    elif command -v sudo >/dev/null 2>&1; then
        sudo dmesg | tail -20 || true
    fi
fi
