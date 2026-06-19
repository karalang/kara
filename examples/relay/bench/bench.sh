#!/bin/sh
# Relay bench driver (2026-06-19). Adapted from the settled Parallax bench
# harness (`examples/parallax/bench/bench.sh`) — same flags, same graceful
# toolchain-skip, same BOUND_PORT port-discovery, same multi-round median
# aggregation + wrk `--latency` percentile parser, same "numbers are the
# artifact, no CI assertions" stance (F1–F5, `docs/dogfooding.md` appendix).
#
# What it measures: a Layer-7 HTTP reverse-proxy throughput/latency
# comparison across THREE proxy impls — kara, go, node — each forwarding to
# ONE shared upstream backend. The shared backend (a trivial Go origin
# returning a constant "OK") is launched ONCE, up front; its ephemeral port
# is discovered and exported as `RELAY_UPSTREAM=127.0.0.1:<port>` to every
# proxy, so the *proxy* is the thing under test, not the backend. wrk hits
# the proxy's bound port; the proxy forwards to the shared upstream.
#
# The differentiator framing is vs Go: the goroutine-per-connection
# lifecycle Go's `httputil.ReverseProxy` runs is exactly what you never
# write in Kāra's effect-driven event loop.
#
# Usage:
#   bench.sh                          # full bench, all three proxies
#   bench.sh --dry-run                # print what would run; touch nothing
#   bench.sh --impls=k,g              # comma-separated subset
#                                     # (k=kara, g=go, n=node)
#   bench.sh --connections=100,1000   # connection-count sweep (default
#                                     # 100,1000,5000). One row per
#                                     # (impl, conn) pair in the output.
#   bench.sh --runs=5                 # measurement rounds per
#                                     # (impl, conn) pair (default 3).
#                                     # Output reports median req/s with
#                                     # [min..max] range across rounds.
#   bench.sh --warmup=5               # one-time per-server warmup before
#                                     # any measurement (default 0). See the
#                                     # WARMUP_SEC default rationale below.
#   bench.sh --measure=15             # measure-window length per round
#                                     # (default 10s).
#
# **Toolchain probing.** The shared upstream needs `go`. Each proxy checks
# its toolchain (cargo for kara; go for go; node for node; wrk always
# required for measurements). Missing toolchain → `skip: <lang> ...` to
# stderr; the impl is skipped, the bench continues for the others. If the
# shared upstream can't build/launch (no go), the whole bench can't run —
# every proxy needs something to forward to.
#
# **Output format.** wrk's `--latency` produces fixed percentiles
# (p50/p75/p90/p99) plus a `Latency ... Max` line. We parse all five (p50,
# p75, p90, p99, max) plus `Requests/sec:`. Per `(impl, conn)` pair, N runs
# produce N values per metric; we report median req/s with [min..max] range,
# and the median of each percentile across the N rounds.
#
# **No throughput-number assertions in CI.** Per F3 the numbers are the
# artifact, not a regression gate. `tests/relay_bench.rs::
# test_bench_script_dry_run` invokes us with `--dry-run` only.

set -eu

# ── Resolve script directory regardless of caller's CWD ────────────
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BENCH_DIR="$SCRIPT_DIR"
REPO_ROOT=$(CDPATH= cd -- "$BENCH_DIR/../../.." && pwd)

# ── Defaults ───────────────────────────────────────────────────────
DRY_RUN=0
IMPLS_FILTER="k,g,n"
# Default warmup = 0 (no warmup). With $RUNS=3 measure rounds and
# median-of-runs aggregation, the first-round cold-start outlier is
# naturally excluded. Also avoids a Node-specific failure mode: at -c100 a
# 3 s warmup pins 100 keep-alive connections in TIME_WAIT on the macOS
# loopback for ~30 s after closing, starving subsequent measure rounds of
# ephemeral ports. Users who want explicit warmup can pass --warmup=N.
WARMUP_SEC=0
MEASURE_SEC=10
WRK_THREADS=4
CONNECTIONS_LIST="100,1000,5000"
RUNS=3
PORT_TIMEOUT_SEC=30

# ── Parse args ─────────────────────────────────────────────────────
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --impls=*) IMPLS_FILTER="${arg#--impls=}" ;;
    --warmup=*) WARMUP_SEC="${arg#--warmup=}" ;;
    --measure=*) MEASURE_SEC="${arg#--measure=}" ;;
    --connections=*) CONNECTIONS_LIST="${arg#--connections=}" ;;
    --runs=*) RUNS="${arg#--runs=}" ;;
    -h|--help)
      sed -n '3,57p' "$0"
      exit 0
      ;;
    *)
      echo "unknown arg: $arg" >&2
      exit 2
      ;;
  esac
done

want_impl() {
  case "$IMPLS_FILTER" in
    *"$1"*) return 0 ;;
    *) return 1 ;;
  esac
}

# ── Toolchain check helpers ─────────────────────────────────────────
have() {
  command -v "$1" >/dev/null 2>&1
}

# ── Shared upstream backend ─────────────────────────────────────────
# One fixed, fast Go origin returning a constant "OK", the SAME backend for
# all three proxies. Built + launched ONCE; its port is exported as
# RELAY_UPSTREAM to every proxy. Intentionally trivial so it out-throughputs
# every proxy and is never the bottleneck.
UPSTREAM_PID=""
UPSTREAM_LOG=""
RELAY_UPSTREAM=""

prepare_upstream() {
  if ! have go; then
    echo "skip: upstream not built (go not installed) — no backend to proxy to" >&2
    return 1
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] upstream: would go build in bench/upstream/ and launch it" >&2
    return 0
  fi
  UPSTREAM_EXE="$BENCH_DIR/upstream/relay-bench-upstream"
  (cd "$BENCH_DIR/upstream" && go build -o "$UPSTREAM_EXE" .) >/dev/null 2>&1 || {
    echo "skip: upstream build failed (go build)" >&2
    return 1
  }
  [ -x "$UPSTREAM_EXE" ] || {
    echo "skip: upstream exe missing at $UPSTREAM_EXE" >&2
    return 1
  }
  return 0
}

launch_upstream() {
  UPSTREAM_EXE="$BENCH_DIR/upstream/relay-bench-upstream"
  UPSTREAM_LOG=$(mktemp)
  "$UPSTREAM_EXE" >"$UPSTREAM_LOG" 2>&1 &
  UPSTREAM_PID=$!
  i=0
  while [ "$i" -lt "$PORT_TIMEOUT_SEC" ]; do
    port=$(grep -m1 -E '^BOUND_PORT=' "$UPSTREAM_LOG" 2>/dev/null | head -n1 | sed -E 's/^BOUND_PORT=//')
    if [ -n "${port:-}" ]; then
      RELAY_UPSTREAM="127.0.0.1:$port"
      export RELAY_UPSTREAM
      return 0
    fi
    sleep 1
    i=$((i + 1))
  done
  return 1
}

kill_upstream() {
  if [ -n "$UPSTREAM_PID" ]; then
    kill "$UPSTREAM_PID" 2>/dev/null || true
    wait "$UPSTREAM_PID" 2>/dev/null || true
    UPSTREAM_PID=""
  fi
  [ -n "$UPSTREAM_LOG" ] && rm -f "$UPSTREAM_LOG"
}

# ── Per-proxy builders ──────────────────────────────────────────────
# Each builder builds its proxy and returns 0 on success, non-zero on skip.
# The actual server launch + wrk loop happens in run_impl after the builder
# returns.

prepare_kara() {
  if ! have cargo; then
    echo "skip: kara not built (cargo not installed)" >&2
    return 1
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] kara: would build karac + libkarac_runtime + server.kara binary" >&2
    return 0
  fi
  (cd "$REPO_ROOT" && cargo build --release --features llvm -p karac >/dev/null 2>&1) || {
    echo "skip: kara build failed (cargo build --release --features llvm)" >&2
    return 1
  }
  (cd "$REPO_ROOT" && cargo rustc -p karac-runtime --release --crate-type staticlib >/dev/null 2>&1) || {
    echo "skip: kara runtime build failed" >&2
    return 1
  }
  KARAC_BIN="$REPO_ROOT/target/release/karac"
  KARA_SRC="$BENCH_DIR/kara/server.kara"
  KARA_EXE="$BENCH_DIR/kara/.bin/server"
  mkdir -p "$BENCH_DIR/kara/.bin"
  KARAC_RUNTIME="$REPO_ROOT/target/release/libkarac_runtime.a" \
    "$KARAC_BIN" build "$KARA_SRC" >/dev/null 2>&1 || {
    echo "skip: kara compile failed (karac build server.kara)" >&2
    return 1
  }
  if [ -f "$REPO_ROOT/server" ]; then
    mv "$REPO_ROOT/server" "$KARA_EXE"
  elif [ -f "./server" ]; then
    mv "./server" "$KARA_EXE"
  elif [ -f "$BENCH_DIR/kara/server" ]; then
    mv "$BENCH_DIR/kara/server" "$KARA_EXE"
  fi
  [ -x "$KARA_EXE" ] || {
    echo "skip: kara exe missing at $KARA_EXE" >&2
    return 1
  }
  return 0
}

prepare_go() {
  if ! have go; then
    echo "skip: go not installed" >&2
    return 1
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] go: would go build in bench/go/" >&2
    return 0
  fi
  GO_EXE="$BENCH_DIR/go/relay-bench-go"
  (cd "$BENCH_DIR/go" && go build -o "$GO_EXE" .) >/dev/null 2>&1 || {
    echo "skip: go build failed (go build)" >&2
    return 1
  }
  [ -x "$GO_EXE" ] || {
    echo "skip: go exe missing at $GO_EXE" >&2
    return 1
  }
  return 0
}

prepare_node() {
  if ! have node; then
    echo "skip: node not installed" >&2
    return 1
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] node: would run node $BENCH_DIR/node/server.js" >&2
    return 0
  fi
  [ -f "$BENCH_DIR/node/server.js" ] || {
    echo "skip: node server.js missing" >&2
    return 1
  }
  return 0
}

# ── Server launcher + port discovery ────────────────────────────────
SERVER_PID=""
launch_and_get_port() {
  cmd="$1"
  log="$2"
  rm -f "$log"
  $cmd >"$log" 2>&1 &
  SERVER_PID=$!
  i=0
  while [ "$i" -lt "$PORT_TIMEOUT_SEC" ]; do
    port=$(grep -m1 -E '^BOUND_PORT=' "$log" 2>/dev/null | head -n1 | sed -E 's/^BOUND_PORT=//')
    if [ -n "${port:-}" ]; then
      echo "$port"
      return 0
    fi
    sleep 1
    i=$((i + 1))
  done
  echo "0"
  return 1
}

kill_server() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
  fi
}

# ── wrk runner + percentile parser ──────────────────────────────────
# Runs one wrk measurement round. Echoes a single space-separated row:
#   `<rps> <p50_ms> <p75_ms> <p90_ms> <p99_ms> <max_ms>`
# (latencies normalized to milliseconds.) On parse failure echoes
# `NA NA NA NA NA NA` so the aggregator's awk math doesn't choke.
run_wrk_one() {
  port="$1"
  conns="$2"
  url="http://127.0.0.1:$port/"
  out=$(wrk -t"$WRK_THREADS" -c"$conns" -d"${MEASURE_SEC}s" --latency "$url" 2>&1) || true
  echo "$out" | awk '
    function to_ms(v,    n, u) {
      n = v + 0
      u = v
      sub(/^[0-9.]+/, "", u)
      if (u == "us") return n / 1000.0
      if (u == "ms") return n
      if (u == "s")  return n * 1000.0
      if (u == "m")  return n * 60000.0
      return n
    }
    /^Requests\/sec:/ { rps = $2 + 0 }
    /^[[:space:]]+Latency[[:space:]]+[0-9]/ { lat_max = to_ms($4) }
    /^[[:space:]]+50%[[:space:]]/  { p50 = to_ms($2) }
    /^[[:space:]]+75%[[:space:]]/  { p75 = to_ms($2) }
    /^[[:space:]]+90%[[:space:]]/  { p90 = to_ms($2) }
    /^[[:space:]]+99%[[:space:]]/  { p99 = to_ms($2) }
    END {
      printf "%s %s %s %s %s %s\n",
        (rps    ? rps    : "NA"),
        (p50    ? p50    : "NA"),
        (p75    ? p75    : "NA"),
        (p90    ? p90    : "NA"),
        (p99    ? p99    : "NA"),
        (lat_max ? lat_max : "NA")
    }
  '
}

# ── Multi-run aggregator ────────────────────────────────────────────
# Runs run_wrk_one $RUNS times for a (port, conns) pair; aggregates across
# runs. Echoes one row:
#   `<conns> <rps_med> <rps_min> <rps_max> <p50_med> <p75_med> <p90_med> <p99_med> <max_med>`
# All latencies in milliseconds.
run_wrk_aggregated() {
  port="$1"
  conns="$2"
  raw=""
  i=0
  while [ "$i" -lt "$RUNS" ]; do
    line=$(run_wrk_one "$port" "$conns") || line="NA NA NA NA NA NA"
    raw="${raw}${line}
"
    i=$((i + 1))
  done
  printf '%s' "$raw" | awk -v conns="$conns" '
    function median(a, n,    i, j, t) {
      for (i = 1; i <= n; i++)
        for (j = i + 1; j <= n; j++)
          if (a[i] > a[j]) { t = a[i]; a[i] = a[j]; a[j] = t }
      if (n % 2 == 1) return a[(n + 1) / 2]
      else return (a[n / 2] + a[n / 2 + 1]) / 2
    }
    function minof(a, n,    i, m) { m = a[1]; for (i = 2; i <= n; i++) if (a[i] < m) m = a[i]; return m }
    function maxof(a, n,    i, m) { m = a[1]; for (i = 2; i <= n; i++) if (a[i] > m) m = a[i]; return m }
    # At high connection counts wrk under load can return a
    # `Requests/sec:` line without any Latency Distribution rows (server
    # saturating, socket errors, distribution table suppressed). We keep
    # those in the rps aggregate but exclude them from percentile medians
    # (their 0s would dominate the median).
    $1 != "NA" {
      rn++; rps[rn] = $1
    }
    $1 != "NA" && $2 != "NA" && $3 != "NA" && $4 != "NA" && $5 != "NA" && $6 != "NA" {
      pn++
      p50[pn] = $2; p75[pn] = $3; p90[pn] = $4; p99[pn] = $5; lmax[pn] = $6
    }
    END {
      if (rn == 0) {
        printf "%s NA NA NA NA NA NA NA NA\n", conns
        exit
      }
      printf "%s %.2f %.2f %.2f", conns, median(rps, rn), minof(rps, rn), maxof(rps, rn)
      if (pn == 0) {
        printf " NA NA NA NA NA\n"
      } else {
        printf " %.2f %.2f %.2f %.2f %.2f\n",
          median(p50, pn), median(p75, pn), median(p90, pn), median(p99, pn),
          median(lmax, pn)
      }
    }
  '
}

# ── Cold-start probe ────────────────────────────────────────────────
# Single wrk -t1 -c1 -d1s window immediately after server spawn — the first
# ~N proxied requests on the cold runtime, before any other wrk traffic.
# Emits the same percentile shape as the steady-state aggregator.
run_cold_start() {
  port="$1"
  url="http://127.0.0.1:$port/"
  out=$(wrk -t1 -c1 -d1s --latency "$url" 2>&1) || true
  echo "$out" | awk '
    function to_ms(v,    n, u) {
      n = v + 0
      u = v
      sub(/^[0-9.]+/, "", u)
      if (u == "us") return n / 1000.0
      if (u == "ms") return n
      if (u == "s")  return n * 1000.0
      if (u == "m")  return n * 60000.0
      return n
    }
    /^Requests\/sec:/ { rps = $2 + 0 }
    /^[[:space:]]+Latency[[:space:]]+[0-9]/ { lat_max = to_ms($4) }
    /^[[:space:]]+50%[[:space:]]/  { p50 = to_ms($2) }
    /^[[:space:]]+75%[[:space:]]/  { p75 = to_ms($2) }
    /^[[:space:]]+90%[[:space:]]/  { p90 = to_ms($2) }
    /^[[:space:]]+99%[[:space:]]/  { p99 = to_ms($2) }
    END {
      if (rps) {
        printf "%.2f|%.2f|%.2f|%s|%s|%s|%s|%s\n",
          rps, rps, rps,
          (p50 ? sprintf("%.2f", p50) : "NA"),
          (p75 ? sprintf("%.2f", p75) : "NA"),
          (p90 ? sprintf("%.2f", p90) : "NA"),
          (p99 ? sprintf("%.2f", p99) : "NA"),
          (lat_max ? sprintf("%.2f", lat_max) : "NA")
      } else {
        printf "NA|NA|NA|NA|NA|NA|NA|NA\n"
      }
    }
  '
}

# ── Run one proxy across all connection counts ──────────────────────
# $1 = impl tag (k|g|n), $2 = display name, $3 = prepare fn, $4 = run cmd.
# Builds the proxy, launches it (RELAY_UPSTREAM already exported pointing at
# the shared upstream), runs cold-start, then sweeps connection counts × runs
# and emits one result row per (impl, conn) to stdout:
#   `<name>|<conns>|<rps_med>|<rps_min>|<rps_max>|<p50>|<p75>|<p90>|<p99>|<max>`
# Cold-start row uses `cold` as the conn marker.
run_impl() {
  tag="$1"
  name="$2"
  prepare="$3"
  cmd="$4"
  if ! want_impl "$tag"; then
    return 0
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    "$prepare" || true
    echo "$name|cold|DRY|DRY|DRY|DRY|DRY|DRY|DRY|DRY"
    for c in $(echo "$CONNECTIONS_LIST" | tr ',' ' '); do
      echo "$name|$c|DRY|DRY|DRY|DRY|DRY|DRY|DRY|DRY"
    done
    return 0
  fi
  if ! "$prepare"; then
    echo "$name|cold|SKIP|SKIP|SKIP|SKIP|SKIP|SKIP|SKIP|SKIP"
    for c in $(echo "$CONNECTIONS_LIST" | tr ',' ' '); do
      echo "$name|$c|SKIP|SKIP|SKIP|SKIP|SKIP|SKIP|SKIP|SKIP"
    done
    return 0
  fi
  log=$(mktemp)
  trap 'kill_server; rm -f "$log"' EXIT INT TERM
  port=$(launch_and_get_port "$cmd" "$log") || {
    for c in $(echo "$CONNECTIONS_LIST" | tr ',' ' '); do
      echo "$name|$c|BIND_FAIL|BIND_FAIL|BIND_FAIL|BIND_FAIL|BIND_FAIL|BIND_FAIL|BIND_FAIL|BIND_FAIL" >&2
    done
    kill_server
    rm -f "$log"
    trap - EXIT INT TERM
    return 0
  }
  if ! have wrk; then
    for c in $(echo "$CONNECTIONS_LIST" | tr ',' ' '); do
      echo "$name|$c|WRK_MISSING|WRK_MISSING|WRK_MISSING|WRK_MISSING|WRK_MISSING|WRK_MISSING|WRK_MISSING|WRK_MISSING" >&2
    done
    kill_server
    rm -f "$log"
    trap - EXIT INT TERM
    return 0
  fi
  # Cold-start probe — runs FIRST, before any other wrk traffic.
  cold=$(run_cold_start "$port") || cold="NA|NA|NA|NA|NA|NA|NA|NA"
  echo "$name|cold|$cold"
  # Optional one-time per-server warmup at the smallest connection count.
  if [ "$WARMUP_SEC" -gt 0 ]; then
    first_conn=$(echo "$CONNECTIONS_LIST" | cut -d',' -f1)
    url="http://127.0.0.1:$port/"
    wrk -t"$WRK_THREADS" -c"$first_conn" -d"${WARMUP_SEC}s" "$url" >/dev/null 2>&1 || true
  fi
  for c in $(echo "$CONNECTIONS_LIST" | tr ',' ' '); do
    agg=$(run_wrk_aggregated "$port" "$c") || agg="$c NA NA NA NA NA NA NA NA"
    echo "$name|$(echo "$agg" | tr ' ' '|')"
  done
  kill_server
  rm -f "$log"
  trap - EXIT INT TERM
}

# ── Main ────────────────────────────────────────────────────────────
echo "Relay bench harness — kara, go, node (Layer-7 reverse proxy)"
echo "  bench dir: $BENCH_DIR"
if [ "$DRY_RUN" -eq 1 ]; then
  echo "  mode: DRY RUN (no servers spawned, no wrk)"
fi
if have wrk; then
  echo "  wrk: $(wrk --version 2>&1 | head -1)"
fi
echo "  impls filter: $IMPLS_FILTER"
echo "  connections sweep: $CONNECTIONS_LIST"
echo "  runs per (impl, conn): $RUNS"
echo "  warmup: ${WARMUP_SEC}s (one-time per-server), measure: ${MEASURE_SEC}s × $RUNS rounds"
echo

# Shared upstream — built + launched ONCE, exported to every proxy.
if ! prepare_upstream; then
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "  upstream: (dry-run) shared Go backend"
  else
    echo "FATAL: shared upstream unavailable — no backend for the proxies to forward to." >&2
    echo "       Install go and re-run, or pass --dry-run to inspect the plan." >&2
    exit 1
  fi
fi
if [ "$DRY_RUN" -eq 0 ]; then
  trap 'kill_upstream' EXIT INT TERM
  if ! launch_upstream; then
    echo "FATAL: shared upstream did not emit BOUND_PORT within ${PORT_TIMEOUT_SEC}s." >&2
    kill_upstream
    exit 1
  fi
  echo "  shared upstream: RELAY_UPSTREAM=$RELAY_UPSTREAM (Go origin, constant \"OK\")"
  echo
fi

results=""

KARA_EXE_HOLDER="$BENCH_DIR/kara/.bin/server"
out=$(run_impl "k" "kara" prepare_kara "$KARA_EXE_HOLDER")
results="$results
$out"

GO_EXE_HOLDER="$BENCH_DIR/go/relay-bench-go"
out=$(run_impl "g" "go" prepare_go "$GO_EXE_HOLDER")
results="$results
$out"

NODE_CMD_HOLDER="node $BENCH_DIR/node/server.js"
out=$(run_impl "n" "node" prepare_node "$NODE_CMD_HOLDER")
results="$results
$out"

if [ "$DRY_RUN" -eq 0 ]; then
  kill_upstream
  trap - EXIT INT TERM
fi

echo
echo "Cold-start (first ~1s after server spawn, -t1 -c1 sequential)"
echo
printf "  %-7s | %-26s | %-8s | %-8s | %-8s | %-8s | %-9s\n" \
  "impl" "req/s" "p50 ms" "p75 ms" "p90 ms" "p99 ms" "max ms"
printf "  %s\n" "--------+----------------------------+----------+----------+----------+----------+----------"
printf "%s\n" "$results" | while IFS='|' read -r name conns rps_med rps_min rps_max p50 p75 p90 p99 lmax; do
  [ -z "$name" ] && continue
  [ "$conns" = "cold" ] || continue
  if [ "$rps_med" = "DRY" ] || [ "$rps_med" = "SKIP" ] || [ "$rps_med" = "NA" ]; then
    printf "  %-7s | %-26s | %-8s | %-8s | %-8s | %-8s | %-9s\n" \
      "$name" "$rps_med" "$rps_med" "$rps_med" "$rps_med" "$rps_med" "$rps_med"
  else
    printf "  %-7s | %-26s | %-8s | %-8s | %-8s | %-8s | %-9s\n" \
      "$name" "$rps_med" "$p50" "$p75" "$p90" "$p99" "$lmax"
  fi
done

echo
echo "Steady-state — req/s reported as median across $RUNS rounds, [min..max] in brackets;"
echo "               latencies are median across rounds in milliseconds."
echo
printf "  %-7s | %-6s | %-26s | %-8s | %-8s | %-8s | %-8s | %-9s\n" \
  "impl" "-c" "req/s (med [min..max])" "p50 ms" "p75 ms" "p90 ms" "p99 ms" "max ms"
printf "  %s\n" "--------+--------+----------------------------+----------+----------+----------+----------+----------"
printf "%s\n" "$results" | while IFS='|' read -r name conns rps_med rps_min rps_max p50 p75 p90 p99 lmax; do
  [ -z "$name" ] && continue
  [ "$conns" = "cold" ] && continue
  if [ "$rps_med" = "DRY" ] || [ "$rps_med" = "SKIP" ] || [ "$rps_med" = "NA" ]; then
    printf "  %-7s | %-6s | %-26s | %-8s | %-8s | %-8s | %-8s | %-9s\n" \
      "$name" "$conns" "$rps_med" "$rps_med" "$rps_med" "$rps_med" "$rps_med" "$rps_med"
  else
    rps_summary=$(printf "%.0f [%.0f..%.0f]" "$rps_med" "$rps_min" "$rps_max")
    printf "  %-7s | %-6s | %-26s | %-8s | %-8s | %-8s | %-8s | %-9s\n" \
      "$name" "$conns" "$rps_summary" "$p50" "$p75" "$p90" "$p99" "$lmax"
  fi
done

echo
if [ "$DRY_RUN" -eq 1 ]; then
  echo "DRY RUN complete — no benchmark numbers produced."
  echo "Re-run without --dry-run to measure."
fi
