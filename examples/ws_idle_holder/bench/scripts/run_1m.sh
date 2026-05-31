#!/usr/bin/env bash
# Canonical N=1M c=64 idle-hold run for the ws_idle_holder bench
# harness. Invokes the harness with the standard flags so Kāra and
# Rust comparator runs are guaranteed identical.
#
# Usage:
#   run_1m.sh <server-bin> [output.json]
#
# - server-bin: absolute path to either the Kāra demo binary
#   (examples/ws_idle_holder/ws_idle_holder, produced by `karac build`)
#   or the Rust comparator
#   (examples/ws_idle_holder/rust/target/release/ws-idle-holder-rust,
#   produced by `cargo build --release` inside that dir).
# - output.json: optional, defaults to "<basename>-1m.json" in cwd.
#
# Prereq: scripts/ec2_setup.sh (sysctls + loopback aliases + nofile
# limits.d). This script also calls `ulimit -n 1250000` inline as a
# safety net for the current shell.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <server-bin> [output.json]" >&2
    exit 1
fi

SERVER_BIN="$1"
OUTPUT="${2:-$(basename "$SERVER_BIN")-1m.json}"

if [[ ! -x "$SERVER_BIN" ]]; then
    echo "$0: $SERVER_BIN not found or not executable" >&2
    exit 1
fi

# Absolutise the server-bin path. The bench harness spawns it via Rust's
# `Command::new`, which PATH-looks-up a bare name with no slash (e.g.
# `ws_idle_holder`) rather than resolving it against cwd — so a bare path
# that passes the `-x` check above would still fail to spawn with "No such
# file or directory". Canonicalising here makes any accepted path spawn
# correctly regardless of the bench's working directory.
SERVER_BIN="$(cd "$(dirname "$SERVER_BIN")" && pwd)/$(basename "$SERVER_BIN")"

# Resolve the bench harness binary relative to this script.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BENCH_BIN="$BENCH_DIR/target/release/ws-idle-holder-bench"

if [[ ! -x "$BENCH_BIN" ]]; then
    echo "$0: bench harness not built at $BENCH_BIN" >&2
    echo "    run: (cd $BENCH_DIR && cargo build --release)" >&2
    exit 1
fi

# Source IPs: 127.0.0.2..28 (27 IPs). Each held conn picks one
# round-robin via --source-ips so the (src_ip, dst_ip, dst_port) tuple
# isn't pinned to a single source and exhausting its ~50K-port pool.
# Requires loopback aliases from ec2_setup.sh.
SOURCE_IPS=""
for i in $(seq 2 28); do
    SOURCE_IPS="${SOURCE_IPS:+${SOURCE_IPS},}127.0.0.${i}"
done

# Raise fd limit inline (needs hard limit from ec2_setup.sh's limits.d
# entry to actually take effect at 1.25M).
ulimit -n 1250000 2>/dev/null || true

echo "[run_1m] server-bin  : $SERVER_BIN"
echo "[run_1m] bench-bin   : $BENCH_BIN"
echo "[run_1m] output      : $OUTPUT"
echo "[run_1m] ulimit -n   : $(ulimit -n)"
echo "[run_1m] starting at : $(date -u +%FT%TZ)"
echo

"$BENCH_BIN" \
    --server-bin "$SERVER_BIN" \
    -n 1000000 \
    --concurrency 64 \
    --churn-rounds 0 \
    --connect-timeout-ms 30000 \
    --source-ips "$SOURCE_IPS" \
    | tee "$OUTPUT"

echo
echo "[run_1m] complete at : $(date -u +%FT%TZ)"
echo "[run_1m] JSON written: $OUTPUT"
echo "[run_1m] absolute    : $(cd "$(dirname "$OUTPUT")" && pwd)/$(basename "$OUTPUT")"

# Post-run diagnostic: any SYN flood / cookie messages? If yes the
# listen-backlog is being saturated and the connect-tail latencies are
# distorted by 1s SYN retransmits.
if command -v dmesg >/dev/null 2>&1; then
    echo
    echo "[run_1m] dmesg tail (last 20 lines):"
    if dmesg | tail -20 2>/dev/null; then
        :
    elif command -v sudo >/dev/null 2>&1; then
        sudo dmesg | tail -20 || true
    fi
fi

echo
echo "[run_1m] >>> BEFORE TERMINATING THIS INSTANCE: scp the JSON above"
echo "[run_1m] >>> off-box to docs/investigations/ in the local repo."
echo "[run_1m] >>> Once the rig is gone, the raw JSON is gone — only the"
echo "[run_1m] >>> denormalized numbers in REPORT.md survive."
