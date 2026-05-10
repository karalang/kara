#!/bin/sh
# Slice E (2026-05-09) — Three-language Parallax bench driver.
#
# Builds + runs the four reference impls (kara, rust, go, node) and
# probes each with `wrk` for throughput + p99 latency. Sequential runs
# on the same machine — F4 fairness control.
#
# Usage:
#   bench.sh                 # build all impls, run wrk against each
#   bench.sh --dry-run       # print what would run; touch nothing
#   bench.sh --impls=k,r     # comma-separated subset (k=kara, r=rust,
#                            # g=go, n=node). Default: k,r,g,n.
#
# **Toolchain probing.** Each impl checks for its required toolchain
# (cargo for kara + rust; go for go; node for node; wrk always
# required for measurements). Missing toolchain → `skip: <lang> not
# installed` to stderr; the impl is skipped, the bench continues for
# the others.
#
# **Throughput parser.** `wrk -d30s` output has the form
# `Requests/sec: 1234.56` and `99% <latency-with-units>`. The parser
# extracts those two lines verbatim. Tested against `wrk` 4.x; if a
# future major rev changes the format, update PARSE_RPS / PARSE_P99.
#
# **No throughput-number assertions in CI.** Per the slice plan, the
# numbers are the artifact, not a regression gate. `tests/parallax_
# bench.rs::test_bench_script_dry_run` invokes us with `--dry-run`
# only.

set -eu

# ── Resolve script directory regardless of caller's CWD ────────────
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BENCH_DIR="$SCRIPT_DIR"
REPO_ROOT=$(CDPATH= cd -- "$BENCH_DIR/../../.." && pwd)

# ── Defaults ───────────────────────────────────────────────────────
DRY_RUN=0
IMPLS_FILTER="k,r,g,n"
WARMUP_SEC=10
MEASURE_SEC=30
WRK_THREADS=4
WRK_CONNS=100
PORT_TIMEOUT_SEC=30

# ── Parse args ─────────────────────────────────────────────────────
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --impls=*) IMPLS_FILTER="${arg#--impls=}" ;;
    --warmup=*) WARMUP_SEC="${arg#--warmup=}" ;;
    --measure=*) MEASURE_SEC="${arg#--measure=}" ;;
    -h|--help)
      sed -n '3,28p' "$0"
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

# ── Per-impl runners ────────────────────────────────────────────────
# Each runner echoes a single line on success: `<name>|<rps>|<p99>`.
# On skip, echoes `<name>|skip|<reason>` to stderr and returns 0 so the
# loop continues. Builds happen at the start of each runner, then the
# server is launched in the background, the wrk warmup + measurement
# fire, and the server is killed.

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
  (cd "$REPO_ROOT" && cargo build --release -p karac-runtime >/dev/null 2>&1) || {
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
  # `karac build` writes `./server` next to the cwd; move it to .bin/.
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

prepare_rust() {
  if ! have cargo; then
    echo "skip: rust not built (cargo not installed)" >&2
    return 1
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] rust: would cargo build --release in bench/rust/" >&2
    return 0
  fi
  (cd "$BENCH_DIR/rust" && cargo build --release >/dev/null 2>&1) || {
    echo "skip: rust build failed (cargo build --release)" >&2
    return 1
  }
  RUST_EXE="$BENCH_DIR/rust/target/release/parallax-bench-rust"
  [ -x "$RUST_EXE" ] || {
    echo "skip: rust exe missing at $RUST_EXE" >&2
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
  GO_EXE="$BENCH_DIR/go/parallax-bench-go"
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
# Spawn $1 in the background; capture stdout into $2; wait up to
# $PORT_TIMEOUT_SEC for `BOUND_PORT=<n>` line; echo port to stdout.
# The PID is left in $SERVER_PID for the caller to kill after wrk.
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

# ── wrk runner + parser ─────────────────────────────────────────────
# Run wrk twice (warmup, then measure). Echoes `<rps>|<p99>` on
# stdout. wrk's `Requests/sec:` line is the throughput; the latency
# distribution row whose first column is `99%` is the p99.
run_wrk() {
  port="$1"
  if ! have wrk; then
    echo "skip: wrk not installed" >&2
    return 1
  fi
  url="http://127.0.0.1:$port/dashboard/1"
  # warmup
  wrk -t"$WRK_THREADS" -c"$WRK_CONNS" -d"${WARMUP_SEC}s" "$url" >/dev/null 2>&1 || true
  # measure
  out=$(wrk -t"$WRK_THREADS" -c"$WRK_CONNS" -d"${MEASURE_SEC}s" --latency "$url" 2>&1) || true
  rps=$(echo "$out" | awk '/^Requests\/sec:/ { print $2 }')
  p99=$(echo "$out" | awk '/^[[:space:]]+99%/ { print $2 }')
  echo "${rps:-NA}|${p99:-NA}"
}

# ── Run an impl end-to-end ──────────────────────────────────────────
# $1 = impl tag (k|r|g|n), $2 = display name, $3 = prepare fn,
# $4 = run command (path to exe / interp). On dry-run, just announce
# the impl name and return.
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
    echo "$name|DRY|DRY"
    return 0
  fi
  if ! "$prepare"; then
    echo "$name|SKIP|SKIP"
    return 0
  fi
  log=$(mktemp)
  trap 'kill_server; rm -f "$log"' EXIT INT TERM
  port=$(launch_and_get_port "$cmd" "$log") || {
    echo "$name|BIND_FAIL|BIND_FAIL" >&2
    kill_server
    rm -f "$log"
    trap - EXIT INT TERM
    return 0
  }
  result=$(run_wrk "$port") || result="WRK_FAIL|WRK_FAIL"
  kill_server
  rm -f "$log"
  trap - EXIT INT TERM
  echo "$name|$result"
}

# ── Main ────────────────────────────────────────────────────────────
echo "Parallax bench harness — kara, rust, go, node"
echo "  bench dir: $BENCH_DIR"
if [ "$DRY_RUN" -eq 1 ]; then
  echo "  mode: DRY RUN (no servers spawned, no wrk)"
fi
echo "  impls filter: $IMPLS_FILTER"
echo "  warmup: ${WARMUP_SEC}s, measure: ${MEASURE_SEC}s, threads: $WRK_THREADS, conns: $WRK_CONNS"
echo

results=""

# kara
KARA_EXE_HOLDER="$BENCH_DIR/kara/.bin/server"
line=$(run_impl "k" "kara" prepare_kara "$KARA_EXE_HOLDER")
results="$results
$line"

# rust
RUST_EXE_HOLDER="$BENCH_DIR/rust/target/release/parallax-bench-rust"
line=$(run_impl "r" "rust" prepare_rust "$RUST_EXE_HOLDER")
results="$results
$line"

# go
GO_EXE_HOLDER="$BENCH_DIR/go/parallax-bench-go"
line=$(run_impl "g" "go" prepare_go "$GO_EXE_HOLDER")
results="$results
$line"

# node
NODE_CMD_HOLDER="node $BENCH_DIR/node/server.js"
line=$(run_impl "n" "node" prepare_node "$NODE_CMD_HOLDER")
results="$results
$line"

echo
echo "Results"
echo "  impl     | req/s        | p99 latency"
echo "  ---------+--------------+-------------"
printf "%s\n" "$results" | while IFS='|' read -r name rps p99; do
  [ -z "$name" ] && continue
  printf "  %-8s | %-12s | %-12s\n" "$name" "${rps:-NA}" "${p99:-NA}"
done

echo
if [ "$DRY_RUN" -eq 1 ]; then
  echo "DRY RUN complete — no benchmark numbers produced."
  echo "Re-run without --dry-run to measure."
fi
