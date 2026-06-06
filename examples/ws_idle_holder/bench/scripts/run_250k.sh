#!/usr/bin/env bash
# N=250K c=64 idle-hold run for the ws_idle_holder bench harness — the
# Phase 3 COMMERCIAL-comparator headline scale (decision 2026-05-30:
# credibility/Rust tracks Kāra at 1M+2M; commercial comparators —
# Phoenix/Java/Go/.NET/Node — run 250K headline + 50K linearity). 250K
# is M2 + a realistic per-box deployment scale; pair with run_50k.sh for
# the linearity sub-curve. If 50K→250K per_conn_bytes drift exceeds 5%,
# escalate that comparator to a 1M run (run_1m.sh) — most likely Phoenix
# (BEAM heap pre-alloc).
#
# Mirrors run_1m.sh's flag set so every comparator is apples-to-apples;
# only N, the source-IP fan-out, the inline ulimit, and labels differ.
#
# Usage:
#   run_250k.sh <server-bin> [output.json]
#
# - server-bin: absolute path to the server binary under test — the Kāra
#   demo (examples/ws_idle_holder/ws_idle_holder, via `karac build`), the
#   Rust comparator, or a commercial comparator binary, e.g. the Go impl
#   (examples/ws_idle_holder/go/ws-idle-holder-go, via `go build`).
# - output.json: optional, defaults to "<basename>-250k.json" in cwd.
#
# Prereq: scripts/ec2_setup.sh (sysctls + loopback aliases + nofile
# limits.d). This script also calls `ulimit -n 400000` inline as a safety
# net for the current shell — well under ec2_setup.sh's 3M hard cap.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <server-bin> [output.json]" >&2
    exit 1
fi

SERVER_BIN="$1"
OUTPUT="${2:-$(basename "$SERVER_BIN")-250k.json}"

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

# Source IPs: 127.0.0.2..28 (27 IPs) — the same fan-out run_1m.sh uses.
# 27 × ~50K ports ≈ 1.36M source tuples = >5× headroom over 250K, which
# trivially absorbs ephemeral-port-range exclusions and TIME_WAIT churn.
# (Reusing run_1m.sh's exact block keeps this a true analogue; 250K needs
# only ~5 IPs, so 27 is harmless overkill.) Requires loopback aliases
# from ec2_setup.sh.
SOURCE_IPS=""
for i in $(seq 2 28); do
    SOURCE_IPS="${SOURCE_IPS:+${SOURCE_IPS},}127.0.0.${i}"
done

# Raise fd limit inline (needs hard limit from ec2_setup.sh's limits.d
# entry; 400K covers 250K client conns + the harness's own fds + margin).
ulimit -n 400000 2>/dev/null || true

echo "[run_250k] server-bin  : $SERVER_BIN"
echo "[run_250k] bench-bin   : $BENCH_BIN"
echo "[run_250k] output      : $OUTPUT"
echo "[run_250k] ulimit -n   : $(ulimit -n)"
echo "[run_250k] starting at : $(date -u +%FT%TZ)"
echo

"$BENCH_BIN" \
    --server-bin "$SERVER_BIN" \
    -n 250000 \
    --concurrency 64 \
    --churn-rounds 0 \
    --connect-timeout-ms 30000 \
    --source-ips "$SOURCE_IPS" \
    | tee "$OUTPUT"

echo
echo "[run_250k] complete at : $(date -u +%FT%TZ)"
echo "[run_250k] JSON written: $OUTPUT"
echo "[run_250k] absolute    : $(cd "$(dirname "$OUTPUT")" && pwd)/$(basename "$OUTPUT")"

# Post-run diagnostic: any SYN flood / cookie messages? If yes the
# listen-backlog is being saturated and the connect-tail latencies are
# distorted by 1s SYN retransmits.
if command -v dmesg >/dev/null 2>&1; then
    echo
    echo "[run_250k] dmesg tail (last 20 lines):"
    if dmesg | tail -20 2>/dev/null; then
        :
    elif command -v sudo >/dev/null 2>&1; then
        sudo dmesg | tail -20 || true
    fi
fi

echo
echo "[run_250k] >>> BEFORE TERMINATING THIS INSTANCE: scp the JSON above"
echo "[run_250k] >>> off-box to docs/investigations/ in the local repo,"
echo "[run_250k] >>> then 'git add' it (scp != tracked — watch for the gap)."
echo "[run_250k] >>> Once the rig is gone, the raw JSON is gone — only the"
echo "[run_250k] >>> denormalized numbers in REPORT.md survive."
