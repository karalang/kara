#!/usr/bin/env bash
# reap_1m.sh — 1M-scale mass-disconnect confirmation of the coroutine
# connection-reap fix (main `a5fd2798`). Combines the fd-accounting of
# reap_check.sh with the source-IP / nofile plumbing of run_1m.sh.
#
# Shape: establish N (default 1M) idle TLS WebSocket connections, hold,
# then the bench client `drop(held)`s ALL of them and exits → every
# server-side peer gets a FIN at once. We launch the server OURSELVES
# (via --addr, so the bench does NOT kill it) and watch the server's
# /proc/<pid>/fd table:
#
#   * peak  ≈ baseline + established  (server holds one fd per live conn)
#   * after the client exits, fds must DRAIN back to ~baseline and
#     CLOSE-WAIT back to ~0 — the reap fix dropping each coroutine's
#     owned WebSocket param on completion. Pre-fix the fds stayed pinned
#     at peak with CLOSE-WAIT ≈ N forever (the coroutine completed but
#     never ran the WebSocket Drop).
#
# This is the at-scale version of the binary fd-accounting signal; it is
# NOT a latency/density run (use run_1m.sh for that).
#
# Prereq: scripts/ec2_setup.sh (sysctls + 127.0.0.2..51 loopback aliases
# + nofile limits). Run this under a shell whose `ulimit -n` reaches
# >=1.25M (a fresh login after ec2_setup, or `sudo bash -c 'ulimit -n
# 1250000; …'`). The script also raises it inline as a safety net.
#
# Usage:
#   reap_1m.sh <server-bin> [N] [hold-secs] [drain-timeout-secs]
#
#   server-bin         : absolute path to the Kāra demo binary (`main`
#                        from `karac build examples/ws_idle_holder/...`).
#   N                  : connections to establish (default 1000000).
#   hold-secs          : steady-state hold before mass-disconnect (def 10).
#   drain-timeout-secs : max seconds to wait for the server fd table to
#                        return to baseline after the client exits (def 180).

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <server-bin> [N] [hold-secs] [drain-timeout-secs]" >&2
    exit 1
fi

SERVER_BIN="$1"
N="${2:-1000000}"
HOLD="${3:-10}"
DRAIN_TIMEOUT="${4:-180}"

if [[ ! -x "$SERVER_BIN" ]]; then
    echo "$0: $SERVER_BIN not found or not executable" >&2
    exit 1
fi
SERVER_BIN="$(cd "$(dirname "$SERVER_BIN")" && pwd)/$(basename "$SERVER_BIN")"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BENCH_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BENCH_BIN="$BENCH_DIR/target/release/ws-idle-holder-bench"
if [[ ! -x "$BENCH_BIN" ]]; then
    echo "$0: bench harness not built at $BENCH_BIN" >&2
    echo "    run: (cd $BENCH_DIR && cargo build --release)" >&2
    exit 1
fi

# Source IPs 127.0.0.2..51 (50 aliases from ec2_setup.sh). Each conn
# round-robins one as its bind source so the (src,dst,dport) tuple isn't
# pinned to one ~50K-port pool: 50 IPs x ~50K = ~2.5M tuples >> 1M.
SOURCE_IPS=""
for i in $(seq 2 51); do
    SOURCE_IPS="${SOURCE_IPS:+${SOURCE_IPS},}127.0.0.${i}"
done

# Raise the fd limit inline (needs the hard cap from ec2_setup.sh).
ulimit -n 1250000 2>/dev/null || true
SOFT_NOFILE="$(ulimit -n)"
if [[ "$SOFT_NOFILE" != "unlimited" && "$SOFT_NOFILE" -lt $(( N + 1000 )) ]]; then
    echo "$0: WARNING ulimit -n is $SOFT_NOFILE, below N+1000=$(( N + 1000 ))." >&2
    echo "    Run ec2_setup.sh and re-login, or run under" >&2
    echo "    sudo bash -c 'ulimit -n 1250000; $0 $*'" >&2
fi

# fast unsorted fd count (sorting ~1M entries every sample is wasteful).
fd_count() { ls -U "/proc/$1/fd" 2>/dev/null | wc -l | tr -d ' '; }
# server-side sockets in CLOSE-WAIT (the leak's fingerprint). Kernel-side
# state filter, so it's cheap even when the total socket count is huge.
close_wait() { ss -H -tan state close-wait 2>/dev/null | wc -l | tr -d ' '; }

# ── Launch the server, capture pid + ephemeral BOUND_PORT ──────────────
SRV_OUT="$(mktemp)"
"$SERVER_BIN" >"$SRV_OUT" 2>&1 &
SRV_PID=$!

cleanup() {
    kill "$SRV_PID" 2>/dev/null || true
    wait "$SRV_PID" 2>/dev/null || true
    rm -f "$SRV_OUT"
    [[ -n "${SAMPLER_PID:-}" ]] && kill "$SAMPLER_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "[reap1m] server pid   : $SRV_PID"
echo "[reap1m] N            : $N   hold: ${HOLD}s   drain-timeout: ${DRAIN_TIMEOUT}s"
echo "[reap1m] ulimit -n    : $SOFT_NOFILE"
echo "[reap1m] source IPs   : 127.0.0.2..51 (50 aliases)"

PORT=""
for _ in $(seq 1 100); do
    PORT="$(grep -oE 'BOUND_PORT=[0-9]+' "$SRV_OUT" 2>/dev/null | head -1 | cut -d= -f2 || true)"
    [[ -n "$PORT" ]] && break
    if ! kill -0 "$SRV_PID" 2>/dev/null; then
        echo "[reap1m] FATAL: server exited before binding. Output:" >&2
        cat "$SRV_OUT" >&2
        exit 1
    fi
    sleep 0.1
done
if [[ -z "$PORT" ]]; then
    echo "[reap1m] FATAL: no BOUND_PORT within 10s. Output:" >&2
    cat "$SRV_OUT" >&2
    exit 1
fi
echo "[reap1m] bound port   : $PORT"

BASE_FDS="$(fd_count "$SRV_PID")"
echo "[reap1m] baseline fds : $BASE_FDS"

# ── Background sampler: fd count + CLOSE-WAIT, every 2s, timestamped ────
SAMPLE_LOG="$(mktemp)"
(
    while kill -0 "$SRV_PID" 2>/dev/null; do
        printf '%s fds=%s close_wait=%s\n' \
            "$(date -u +%H:%M:%S)" "$(fd_count "$SRV_PID")" "$(close_wait)"
        sleep 2
    done
) >"$SAMPLE_LOG" &
SAMPLER_PID=$!

# ── Drive: establish N, hold, then drop ALL (client closes+exits) ──────
echo "[reap1m] driving bench (establish $N, hold ${HOLD}s, then mass-disconnect)..."
set +e
"$BENCH_BIN" \
    --addr "127.0.0.1:$PORT" \
    --server-pid "$SRV_PID" \
    --server-name localhost \
    -n "$N" \
    --concurrency 256 \
    --hold-secs "$HOLD" \
    --churn-rounds 0 \
    --connect-timeout-ms 30000 \
    --source-ips "$SOURCE_IPS" \
    >"$BENCH_DIR/reap_1m_bench.json" 2>"$BENCH_DIR/reap_1m_bench.err"
BENCH_RC=$?
set -e
echo "[reap1m] bench exit rc: $BENCH_RC  (all client conns now closing)"

ESTABLISHED="$(grep -oE '"established"[: ]+[0-9]+' "$BENCH_DIR/reap_1m_bench.json" 2>/dev/null | head -1 | grep -oE '[0-9]+$' || echo '?')"
PEAK_FDS="$(grep -oE 'fds=[0-9]+' "$SAMPLE_LOG" 2>/dev/null | cut -d= -f2 | sort -n | tail -1)"
PEAK_FDS="${PEAK_FDS:-0}"
echo "[reap1m] established  : $ESTABLISHED   peak server fds: $PEAK_FDS"

# ── Poll the drain curve until fds return to baseline (or timeout) ─────
SLACK=$(( N / 100 + 200 ))   # 1% + 200: listener/poller/eventfd/in-flight reaps
TARGET=$(( BASE_FDS + SLACK ))
echo "[reap1m] waiting for fds to drain to <= $TARGET (baseline $BASE_FDS + slack $SLACK)..."
DRAINED=0
ELAPSED=0
while [[ $ELAPSED -lt $DRAIN_TIMEOUT ]]; do
    CUR_FDS="$(fd_count "$SRV_PID")"
    CUR_CW="$(close_wait)"
    echo "[reap1m]   t+${ELAPSED}s  fds=$CUR_FDS  close_wait=$CUR_CW"
    if [[ "$CUR_FDS" -le "$TARGET" && "$CUR_CW" -le "$SLACK" ]]; then
        DRAINED=1
        break
    fi
    sleep 5
    ELAPSED=$(( ELAPSED + 5 ))
done

FINAL_FDS="$(fd_count "$SRV_PID")"
FINAL_CW="$(close_wait)"
kill "$SAMPLER_PID" 2>/dev/null || true

echo
echo "──────────── 1M reap verdict ────────────"
echo "  baseline fds      : $BASE_FDS"
echo "  established        : $ESTABLISHED"
echo "  peak fds (sampled): $PEAK_FDS   (expect ~baseline + established)"
echo "  final fds         : $FINAL_FDS  (expect ~baseline after drain)"
echo "  final CLOSE-WAIT  : $FINAL_CW   (expect ~0)"
echo "  drain target      : <= $TARGET fds   (slack $SLACK)"
echo
echo "  fd/close-wait timeline (every ~Nth sample):"
awk 'NR%5==1' "$SAMPLE_LOG" | tail -20 | sed 's/^/    /'
rm -f "$SAMPLE_LOG"

if [[ "$DRAINED" -eq 1 ]]; then
    echo "  RESULT: PASS — server reaped all $ESTABLISHED connections (fds drained to baseline)."
    exit 0
else
    echo "  RESULT: FAIL — fds did not drain within ${DRAIN_TIMEOUT}s (leak: conns not reaped)." >&2
    exit 1
fi
