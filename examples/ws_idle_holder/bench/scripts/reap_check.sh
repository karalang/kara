#!/usr/bin/env bash
# reap_check.sh — at-scale confirmation of the coroutine connection-reap
# fix (main `a5fd2798`): a coroutine handler's owned `WebSocket` param is
# dropped on task completion, so its fd + TLS session are released when the
# peer disconnects. Pre-fix the server held every closed connection's fd in
# CLOSE-WAIT forever (the coroutine completed but never ran the `WebSocket`
# Drop); post-fix the server's fd table returns to baseline.
#
# This is an fd-ACCOUNTING test, not a scale/latency test — it runs
# co-located (client + server on one box), no taskset, no cross-box
# secondary-IP setup. The signal is binary:
#
#   * Open N connections, hold, then churn (close+reopen ALL, R rounds),
#     then the client exits → ALL connections close.
#   * Sample the server's live fd count + CLOSE-WAIT count throughout.
#   * VERDICT:
#       - During hold/churn, fds track ~baseline+N (not monotonic growth
#         across rounds — pre-fix each round's closed fds are never reaped,
#         so fds climb by the round batch every round).
#       - After the client exits + a settle delay, fds return to ~baseline
#         and CLOSE-WAIT drains to ~0. Pre-fix: fds stay at baseline +
#         (total connections opened) and CLOSE-WAIT stays pinned.
#
# Usage:
#   reap_check.sh <server-bin> [N] [rounds] [hold-secs]
#
#   server-bin : absolute path to the Kāra demo binary
#                (examples/ws_idle_holder/ws_idle_holder, from `karac build`).
#   N          : concurrent held connections (default 2000 — well under the
#                127.0.0.1 ephemeral-port pool, so no source-IP aliasing
#                needed; the leak signal is binary regardless of N).
#   rounds     : churn rounds, each closing+reopening 100% of N (default 5).
#   hold-secs  : seconds to hold the steady-state set before churn (default 3).

set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <server-bin> [N] [rounds] [hold-secs]" >&2
    exit 1
fi

SERVER_BIN="$1"
N="${2:-2000}"
ROUNDS="${3:-5}"
HOLD="${4:-3}"
SETTLE=5   # seconds to wait after the client exits before the final fd read

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

ulimit -n 100000 2>/dev/null || true

# fd count for the server pid (Linux /proc).
fd_count() { ls "/proc/$1/fd" 2>/dev/null | wc -l | tr -d ' '; }
# server-side sockets parked in CLOSE-WAIT (the leak's fingerprint).
# -H suppresses the header so the count is sockets-only.
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

echo "[reap] server pid    : $SRV_PID"
echo "[reap] N             : $N   rounds: $ROUNDS   hold: ${HOLD}s   settle: ${SETTLE}s"

# Wait for BOUND_PORT=<n>.
PORT=""
for _ in $(seq 1 100); do
    PORT="$(grep -oE 'BOUND_PORT=[0-9]+' "$SRV_OUT" 2>/dev/null | head -1 | cut -d= -f2 || true)"
    [[ -n "$PORT" ]] && break
    if ! kill -0 "$SRV_PID" 2>/dev/null; then
        echo "[reap] FATAL: server exited before binding. Output:" >&2
        cat "$SRV_OUT" >&2
        exit 1
    fi
    sleep 0.1
done
if [[ -z "$PORT" ]]; then
    echo "[reap] FATAL: no BOUND_PORT within 10s. Output:" >&2
    cat "$SRV_OUT" >&2
    exit 1
fi
echo "[reap] bound port    : $PORT"

BASE_FDS="$(fd_count "$SRV_PID")"
echo "[reap] baseline fds  : $BASE_FDS"

# ── Background sampler: fd count + CLOSE-WAIT, 2x/sec, timestamped ──────
SAMPLE_LOG="$(mktemp)"
(
    while kill -0 "$SRV_PID" 2>/dev/null; do
        printf '%s fds=%s close_wait=%s\n' \
            "$(date -u +%H:%M:%S)" "$(fd_count "$SRV_PID")" "$(close_wait)"
        sleep 0.5
    done
) >"$SAMPLE_LOG" &
SAMPLER_PID=$!

# ── Drive: establish N, hold, churn 100% R rounds (client closes all on exit) ──
echo "[reap] driving bench (establish $N, hold ${HOLD}s, churn ${ROUNDS}x100%)..."
set +e
"$BENCH_BIN" \
    --addr "127.0.0.1:$PORT" \
    --server-pid "$SRV_PID" \
    --server-name localhost \
    -n "$N" \
    --concurrency 64 \
    --hold-secs "$HOLD" \
    --churn-rounds "$ROUNDS" \
    --churn-fraction 1.0 \
    --churn-batch-cap 0 \
    --connect-timeout-ms 30000 \
    >/dev/null 2>"$BENCH_DIR/reap_bench.err"
BENCH_RC=$?
set -e
echo "[reap] bench exit rc : $BENCH_RC  (all client conns now closed)"

PEAK_FDS="$(grep -oE 'fds=[0-9]+' "$SAMPLE_LOG" 2>/dev/null | cut -d= -f2 | sort -n | tail -1)"
PEAK_FDS="${PEAK_FDS:-0}"

# ── Settle, then the load-bearing post-drain read ──────────────────────
echo "[reap] settling ${SETTLE}s for server-side reap..."
sleep "$SETTLE"
FINAL_FDS="$(fd_count "$SRV_PID")"
FINAL_CW="$(close_wait)"

kill "$SAMPLER_PID" 2>/dev/null || true

echo
echo "──────────── reap verdict ────────────"
echo "  baseline fds      : $BASE_FDS"
echo "  peak fds (sampled): $PEAK_FDS   (expect ~baseline + up to $N)"
echo "  final fds         : $FINAL_FDS  (expect ~baseline after drain)"
echo "  final CLOSE-WAIT  : $FINAL_CW   (expect ~0)"
echo
echo "  fd/close-wait timeline (tail):"
tail -12 "$SAMPLE_LOG" | sed 's/^/    /'
rm -f "$SAMPLE_LOG"

# Pass: final fds within a small slack of baseline AND CLOSE-WAIT drained.
# Slack absorbs the listener fd, runtime poller/event-loop fds, and a few
# in-flight reaps. Pre-fix, FINAL_FDS ≈ BASE_FDS + N*(ROUNDS+1) and
# FINAL_CW ≈ N, both far outside the slack.
SLACK=$(( N / 10 + 50 ))
echo "  pass slack        : $SLACK fds over baseline"
if [[ "$FINAL_FDS" -le $(( BASE_FDS + SLACK )) && "$FINAL_CW" -le "$SLACK" ]]; then
    echo "  RESULT: PASS — server reaped closed connections (fds returned to baseline)."
    exit 0
else
    echo "  RESULT: FAIL — fds did not drain (leak: closed conns not reaped)." >&2
    exit 1
fi
