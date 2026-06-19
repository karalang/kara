#!/usr/bin/env bash
# Relay cross-host bench orchestrator — runs on the control machine (e.g. your
# Mac), drives a separate-client / separate-server rig over SSH.
#
# Topology (3 hosts recommended; 2 works by co-locating upstream with client):
#
#     control (this machine, SSH only)
#          │ ssh
#   client (wrk) ──network──► proxy (under test) ──network──► upstream (origin)
#
# The proxy host is the clean variable: one impl runs there at a time. The
# client runs wrk; the upstream serves a constant body of a configurable size.
# Unlike the loopback bench, the *payload* matters here — a 2-byte body goes
# network-bound, so we sweep larger payloads to keep the proxy the bottleneck.
#
# Prereqs: `bench-remote.sh --setup ...` first (rsyncs the repo to each host and
# runs remote/provision.sh per role). Then run without --setup to measure.
#
# Usage:
#   bench-remote.sh --setup \
#       --proxy HOST --client HOST [--upstream HOST] --user U
#   bench-remote.sh \
#       --proxy HOST --client HOST [--upstream HOST] --user U \
#       [--payloads 0,1024,16384] [--connections 100,1000] \
#       [--impls k,g,n] [--duration 10] [--runs 3] [--threads 8] \
#       [--proxy-port 18080] [--upstream-port 19000] [--remote-dir kara]
#   bench-remote.sh --dry-run ...     # print the plan; no ssh, no servers
set -euo pipefail

# ── Defaults ─────────────────────────────────────────────────────────
PROXY_HOST=""; CLIENT_HOST=""; UPSTREAM_HOST=""; USER_NAME="${USER}"
# Data-plane addresses (the IPs used for client→proxy and proxy→upstream
# traffic). On cloud VMs these are the PRIVATE IPs, distinct from the public
# SSH addresses above. Default to the SSH host (correct for a flat LAN).
PROXY_DATA=""; UPSTREAM_DATA=""
SSH_OPTS="-o ConnectTimeout=10 -o BatchMode=yes -o StrictHostKeyChecking=accept-new"
PAYLOADS="0,1024,16384"; CONNECTIONS="100,1000"; IMPLS="k,g,n"
DURATION=10; RUNS=3; THREADS=8; PROXY_PORT=18080; UPSTREAM_PORT=19000
REMOTE_DIR="kara"; DO_SETUP=0; DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --proxy) PROXY_HOST="$2"; shift 2;;
    --client) CLIENT_HOST="$2"; shift 2;;
    --upstream) UPSTREAM_HOST="$2"; shift 2;;
    --proxy-data) PROXY_DATA="$2"; shift 2;;
    --upstream-data) UPSTREAM_DATA="$2"; shift 2;;
    --user) USER_NAME="$2"; shift 2;;
    --ssh-opts) SSH_OPTS="$2"; shift 2;;
    --payloads) PAYLOADS="$2"; shift 2;;
    --connections) CONNECTIONS="$2"; shift 2;;
    --impls) IMPLS="$2"; shift 2;;
    --duration) DURATION="$2"; shift 2;;
    --runs) RUNS="$2"; shift 2;;
    --threads) THREADS="$2"; shift 2;;
    --proxy-port) PROXY_PORT="$2"; shift 2;;
    --upstream-port) UPSTREAM_PORT="$2"; shift 2;;
    --remote-dir) REMOTE_DIR="$2"; shift 2;;
    --setup) DO_SETUP=1; shift;;
    --dry-run) DRY_RUN=1; shift;;
    -h|--help) sed -n '2,40p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

[ -n "$PROXY_HOST" ]  || { echo "--proxy HOST required" >&2; exit 2; }
[ -n "$CLIENT_HOST" ] || { echo "--client HOST required" >&2; exit 2; }
# Upstream co-locates with the client host if a third host isn't given.
[ -n "$UPSTREAM_HOST" ] || UPSTREAM_HOST="$CLIENT_HOST"
# Data-plane addresses default to the SSH hosts (flat LAN); on cloud, pass the
# private IPs via --proxy-data / --upstream-data.
[ -n "$PROXY_DATA" ]    || PROXY_DATA="$PROXY_HOST"
[ -n "$UPSTREAM_DATA" ] || UPSTREAM_DATA="$UPSTREAM_HOST"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../../../.." && pwd)"
RDIR="$REMOTE_DIR/examples/relay/bench"   # bench dir relative to remote ~/

log() { printf '\033[1m[bench-remote]\033[0m %s\n' "$*" >&2; }
ssh_to() { ssh $SSH_OPTS "${USER_NAME}@$1" "$2"; }

# Launch CMD on remote HOST in the background; echo its PID. LOG is the remote
# logfile. Used for the servers (proxy / upstream); torn down by kill_remote.
launch_remote() { # host cmd logfile
  ssh_to "$1" "nohup sh -c '$2' >'$3' 2>&1 & echo \$!"
}
kill_remote() { ssh_to "$1" "kill $2 2>/dev/null || true; sleep 0.3; kill -9 $2 2>/dev/null || true" >/dev/null 2>&1 || true; }

impl_cmd() { # impl host-relative launch command (proxy)
  case "$1" in
    k) echo "RELAY_BIND=0.0.0.0:$PROXY_PORT RELAY_UPSTREAM=$UPSTREAM_DATA:$UPSTREAM_PORT ~/$RDIR/kara/.bin/server";;
    g) echo "RELAY_BIND=0.0.0.0:$PROXY_PORT RELAY_UPSTREAM=$UPSTREAM_DATA:$UPSTREAM_PORT ~/$RDIR/go/relay-bench-go";;
    n) echo "RELAY_BIND=0.0.0.0:$PROXY_PORT RELAY_UPSTREAM=$UPSTREAM_DATA:$UPSTREAM_PORT node ~/$RDIR/node/server.js";;
  esac
}
impl_name() { case "$1" in k) echo kara;; g) echo go;; n) echo node;; esac; }

# Parse one wrk --latency run (stdin) → "rps p50 p99 max transferMBps" (ms / MB/s).
parse_wrk() {
  awk '
    function to_ms(v,  n,u){n=v+0;u=v;sub(/^[0-9.]+/,"",u);
      if(u=="us")return n/1000;if(u=="ms")return n;if(u=="s")return n*1000;if(u=="m")return n*60000;return n}
    function to_mb(v,  n,u){n=v+0;u=v;sub(/^[0-9.]+/,"",u);
      if(u=="GB")return n*1024;if(u=="MB")return n;if(u=="KB")return n/1024;if(u=="B")return n/1048576;return n}
    /^Requests\/sec:/ {rps=$2+0}
    /^Transfer\/sec:/ {xfer=to_mb($2)}
    /^[[:space:]]+50%[[:space:]]/ {p50=to_ms($2)}
    /^[[:space:]]+99%[[:space:]]/ {p99=to_ms($2)}
    /^[[:space:]]+Latency[[:space:]]+[0-9]/ {lmax=to_ms($4)}
    END{printf "%s %s %s %s %s\n",(rps?rps:"NA"),(p50?p50:"NA"),(p99?p99:"NA"),(lmax?lmax:"NA"),(xfer?xfer:"NA")}'
}

# ── Setup mode: rsync repo to each host + provision per role ──────────
if [ "$DO_SETUP" -eq 1 ]; then
  for hr in "$PROXY_HOST:proxy" "$CLIENT_HOST:client" "$UPSTREAM_HOST:upstream"; do
    host="${hr%%:*}"; role="${hr##*:}"
    log "setup $role on $host: rsync repo → ~/$REMOTE_DIR, then provision.sh $role"
    if [ "$DRY_RUN" -eq 1 ]; then continue; fi
    rsync -az --delete \
      --exclude target --exclude .git --exclude '*/.bin' \
      --exclude 'examples/relay/bench/*/relay-bench-*' \
      -e "ssh $SSH_OPTS" "$REPO_ROOT/" "${USER_NAME}@$host:~/$REMOTE_DIR/"
    ssh_to "$host" "bash ~/$RDIR/remote/provision.sh $role"
  done
  log "setup complete"
  [ "$DRY_RUN" -eq 1 ] && { log "(dry-run: nothing executed)"; exit 0; }
fi

# ── Preflight + RTT ──────────────────────────────────────────────────
log "rig: client=$CLIENT_HOST  proxy=$PROXY_HOST  upstream=$UPSTREAM_HOST  user=$USER_NAME"
log "sweep: payloads=[$PAYLOADS]  connections=[$CONNECTIONS]  impls=$IMPLS  ${DURATION}s × $RUNS runs"
if [ "$DRY_RUN" -eq 1 ]; then
  log "DRY RUN — would: preflight ssh, measure RTT, then per payload {launch upstream; per impl {launch proxy; upstream-direct sanity; per conn × run: wrk}}"
  for p in $(echo "$PAYLOADS" | tr ',' ' '); do for i in $(echo "$IMPLS" | tr ',' ' '); do for c in $(echo "$CONNECTIONS" | tr ',' ' '); do
    echo "  [plan] payload=${p}B impl=$(impl_name "$i") -c$c : wrk -t$THREADS -c$c -d${DURATION}s http://$PROXY_DATA:$PROXY_PORT/"
  done; done; done
  exit 0
fi

for h in "$CLIENT_HOST" "$PROXY_HOST" "$UPSTREAM_HOST"; do
  ssh_to "$h" "echo ok" >/dev/null || { echo "ssh preflight failed for $h" >&2; exit 1; }
done
log "ssh preflight ok for all hosts"
# ICMP is often blocked between cloud instances; fall back to a TCP-connect
# time against the (already-open) proxy data port range via SSH port if needed.
RTT="$(ssh_to "$CLIENT_HOST" "ping -c5 -q $PROXY_DATA 2>/dev/null | tail -1" || true)"
log "client→proxy RTT: ${RTT:-unknown (ICMP likely blocked; see per-run connect times)}"

RESULTS=""   # rows: "name payload conn rps p50 p99 max xfer"
SANITY=""    # rows: "payload rps xfer" (upstream-direct from client)

cleanup() { [ -n "${UP_PID:-}" ] && kill_remote "$UPSTREAM_HOST" "$UP_PID"; [ -n "${PX_PID:-}" ] && kill_remote "$PROXY_HOST" "$PX_PID"; }
trap cleanup EXIT INT TERM

for payload in $(echo "$PAYLOADS" | tr ',' ' '); do
  log "=== payload ${payload}B: launching upstream on $UPSTREAM_HOST ==="
  UP_CMD="RELAY_UPSTREAM_BIND=0.0.0.0:$UPSTREAM_PORT RELAY_BODY_BYTES=$payload ~/$RDIR/upstream/relay-bench-upstream"
  UP_PID="$(launch_remote "$UPSTREAM_HOST" "$UP_CMD" "/tmp/relay-upstream.log")"
  sleep 1
  # Upstream-direct sanity: the upstream must out-throughput the proxies, else
  # it (not the proxy) is the bottleneck. Measured from the client host.
  s="$(ssh_to "$CLIENT_HOST" "wrk -t$THREADS -c100 -d3s --latency http://$UPSTREAM_DATA:$UPSTREAM_PORT/ 2>&1" | parse_wrk)"
  SANITY="${SANITY}${payload} $(echo "$s" | awk '{print $1, $5}')
"
  log "upstream-direct @${payload}B: rps/xfer = $(echo "$s" | awk '{print $1, $5"MB/s"}')"

  for i in $(echo "$IMPLS" | tr ',' ' '); do
    name="$(impl_name "$i")"
    log "--- impl=$name @${payload}B: launching proxy on $PROXY_HOST ---"
    PX_PID="$(launch_remote "$PROXY_HOST" "$(impl_cmd "$i")" "/tmp/relay-proxy-$name.log")"
    sleep 1
    for c in $(echo "$CONNECTIONS" | tr ',' ' '); do
      best_rps=""; agg=""
      r=0
      while [ "$r" -lt "$RUNS" ]; do
        line="$(ssh_to "$CLIENT_HOST" "wrk -t$THREADS -c$c -d${DURATION}s --latency http://$PROXY_DATA:$PROXY_PORT/ 2>&1" | parse_wrk)"
        agg="${agg}${line}
"
        r=$((r+1))
      done
      # median rps + median of percentiles across runs
      med="$(printf '%s' "$agg" | awk '
        function med(a,n,  i,j,t){for(i=1;i<=n;i++)for(j=i+1;j<=n;j++)if(a[i]>a[j]){t=a[i];a[i]=a[j];a[j]=t}
          return (n%2)?a[(n+1)/2]:(a[n/2]+a[n/2+1])/2}
        $1!="NA"{n++;rps[n]=$1;p50[n]=$2;p99[n]=$3;mx[n]=$4;xf[n]=$5}
        END{if(!n){print "NA NA NA NA NA";exit} printf "%.0f %.2f %.2f %.2f %.1f\n",med(rps,n),med(p50,n),med(p99,n),med(mx,n),med(xf,n)}')"
      RESULTS="${RESULTS}${name} ${payload} ${c} ${med}
"
      log "    $name @${payload}B -c$c → $(echo "$med" | awk '{print $1" rps, p99 "$3"ms, "$5"MB/s"}')"
    done
    kill_remote "$PROXY_HOST" "$PX_PID"; PX_PID=""
  done
  kill_remote "$UPSTREAM_HOST" "$UP_PID"; UP_PID=""
done
trap - EXIT INT TERM

# ── Report ───────────────────────────────────────────────────────────
echo
echo "Relay CROSS-HOST benchmark — client=$CLIENT_HOST proxy=$PROXY_HOST upstream=$UPSTREAM_HOST"
echo "client→proxy RTT: ${RTT:-unknown}"
echo
echo "Upstream-direct sanity (must exceed every proxy row for that payload):"
printf "  %-10s | %-12s | %-10s\n" "payload" "req/s" "MB/s"
printf '%s' "$SANITY" | while read -r p rps xf; do [ -z "$p" ] && continue; printf "  %-10s | %-12s | %-10s\n" "${p}B" "$rps" "$xf"; done
echo
echo "Proxy throughput (req/s = median across $RUNS runs; latencies ms; ${DURATION}s windows):"
printf "  %-6s | %-8s | %-6s | %-10s | %-8s | %-8s | %-9s\n" "impl" "payload" "-c" "req/s" "p50 ms" "p99 ms" "MB/s"
printf "  %s\n" "-------+----------+--------+------------+----------+----------+----------"
printf '%s' "$RESULTS" | while read -r name payload conn rps p50 p99 mx xf; do
  [ -z "$name" ] && continue
  printf "  %-6s | %-8s | %-6s | %-10s | %-8s | %-8s | %-9s\n" "$name" "${payload}B" "$conn" "$rps" "$p50" "$p99" "$xf"
done
echo
echo "Reminder: this is the proxy under test on a dedicated host; if any proxy row"
echo "approaches its payload's upstream-direct sanity number, the upstream/network"
echo "— not the proxy — is the ceiling, so bump upstream size or payload."
