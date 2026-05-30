# `ws_idle_holder/bench` — Demo 1 slice 3 bench harness

Bench-harness **client** for the `ws_idle_holder` flagship demo
(`docs/implementation_checklist/phase-6-runtime.md` line 170 / 180).
Opens N idle `wss://` WebSocket connections against the slice-1/2
server, holds them, and measures the three axes the slice plan calls
for, emitting a single JSON object on stdout for the CI gate (slice 7).

## Why a Rust client (not Kāra)

The harness must **open** `wss://` connections, but the v1 Kāra
WebSocket stdlib is server-side only — `WebSocket.accept` / `accept_tls`
+ `recv_*` / `send_*`, with no client `connect`/dial. A Kāra client
would need a whole new client-side TLS-connect + WS-handshake stdlib
surface — out of scope for a measurement harness, and exactly the
"kara client impractical at v1 scale" case the slice-3 tracker entry
anticipated. A Rust client (tokio + rustls) also keeps the measurement
honest: client-side perf bugs don't contaminate server numbers.

Standalone crate, **not** part of the karac-rust workspace (root
`Cargo.toml` `exclude` + an empty `[workspace]` table here so it
self-roots under the worktree path). Mirrors `examples/parallax/bench/
rust/`.

## What it measures

- **(a) connect-establishment latency** — p50 / p95 / p99 / p99.9 (+
  min / max / mean), computed locally from the full sample set via
  nearest-rank. Because the harness owns the raw samples it reports the
  high percentiles the wrk-based Parallax harness couldn't reach (see
  `docs/investigations/bench_robustness.md` G4). One sample per
  connection = TCP connect + TLS handshake + WS HTTP-upgrade round-trip
  (time to `101 Switching Protocols`).
- **(b) steady-state RSS / per-connection memory** — reads the server's
  RSS before vs. after the N connections are held (`/proc/<pid>/status`
  `VmRSS` on Linux, `ps -o rss=` on macOS/BSD), divides the delta by N.
  Only meaningful at large N — at small N it is dominated by fixed
  first-connection overhead (task stack, TLS session buffers).
- **(c) P99 latency cliff under churn** — closes then reopens a fraction
  of the held connections over several rounds; reports reconnect p99 and
  `cliff_ratio = reconnect_p99 / initial_connect_p99` (≫ 1 ⇒ a cliff).

## Usage

```sh
# Build (standalone — run inside this dir):
cargo build --release

# Build the demo server it targets (from workspace root):
cargo build --bin karac --features llvm
cargo build -p karac-runtime --release
( cd examples/ws_idle_holder
  KARAC_RUNTIME="$PWD/../../target/release/libkarac_runtime.a" \
    ../../target/debug/karac build )   # -> examples/ws_idle_holder/ws_idle_holder

# Spawn-and-measure: harness owns the server lifecycle, reads BOUND_PORT
# from its stdout, measures its RSS, kills it on exit.
./target/release/ws-idle-holder-bench \
    --server-bin ../ws_idle_holder -n 100

# Or measure an already-running server (pass --server-pid for RSS):
./target/release/ws-idle-holder-bench --addr 127.0.0.1:8443 --server-pid 12345 -n 100
```

Key flags (`--help` for all): `-n/--connections`, `--concurrency`
(in-flight handshake cap), `--churn-rounds` (0 = off), `--churn-fraction`,
`--hold-secs`, `--connect-timeout-ms` (per-connection deadline so a stuck
connection is counted as a failure instead of hanging the run),
`--source-ips` (see below).

### `--source-ips` — beating the loopback ephemeral-port cap

A single client source IP dialing one `127.0.0.1:<port>` is capped by
`net.ipv4.ip_local_port_range` (≈28K ports on a stock kernel) — every
connection burns one ephemeral *source* port on the `(src_ip, dst_ip,
dst_port)` tuple — and establishment slows sharply as that pool nears
exhaustion. Widening the range needs root.

`--source-ips 127.0.0.2,127.0.0.3,127.0.0.4,127.0.0.5` makes the harness
bind each connection's source to one of the listed loopback addresses
(round-robin by connection index). Linux routes all of `127.0.0.0/8` to
`lo`, so binding these succeeds **without root**, and each source IP
carries its own ephemeral-port pool: N IPs ⇒ ~N×28K ceiling, and keeping
each IP well under its cap also keeps establishment fast. This is the
path to >28K (toward 100K) on a single box. Empty (the default) = single
implicit `127.0.0.1` source. Echoed back in the JSON `config.source_ips`.

TLS verification is disabled (the demo's cert is the self-signed
loopback test fixture). This harness must only ever target loopback.

### `--concurrency` — sweep, don't single-point

`--concurrency` caps in-flight handshakes during the initial ramp. The
choice is not neutral: at equilibrium, mean connect latency follows
`(concurrency / KARAC_WS_ACCEPT_THREADS) × T_handshake`, so the headline
number changes a lot depending on what you pick.

Pre-M3 our published 1 M run used `--concurrency 512` against the
default 32 handshake workers on r8g.4xlarge, which produced
`mean=466ms, p99=1856ms` — geometrically correct
(`(512/32)×29ms = 464ms`), but a misleading single-point read on what
the server is actually capable of. The honest representation is the
curve, not a number.

**Convention for headline reporting:** sweep `--concurrency` across at
least `{64, 128, 256, 512, 1024}` and report both the elbow (where
throughput plateaus) and the high end (where the queue tail dominates).
A shell loop is sufficient — the harness emits one JSON object per run,
keyed by `config.concurrency`:

```sh
for C in 64 128 256 512 1024; do
  ./target/release/ws-idle-holder-bench --server-bin ./ws_idle_holder \
    -n 100000 --concurrency "$C" --hold-secs 5 \
    --connect-timeout-ms 30000 --churn-rounds 0 > "run_c${C}.json"
done
```

A single-point "1M @ concurrency 512" is fine as a stress-test
capacity check (does the server hold the load?) but not as the
latency headline.

### `KARAC_WS_STATS` — server-side queue depth instrumentation

Setting `KARAC_WS_STATS=1` in the server's env (the bench's
`--server-bin` inherits the env) enables a once-per-second reporter
that dumps the handshake-pool state to `stderr`:

```
[karac_ws_stats] submit_total=N done_total=N failed_total=N
                 in_flight=N work_q=N done_q=N workers=N
                 submit_per_s=N done_per_s=N mean_handshake_ms=X
```

`mean_handshake_ms` is the per-call server-CPU time inside
`ws_handshake_conn_tls` (TLS + WS upgrade); combine with `workers`
and the ramp-phase concurrency to predict `(C/W)×T` mean.

## Output (stdout = JSON only; logs → stderr)

```json
{
  "ok": true,
  "config":  { "target": "...", "connections": 100, "concurrency": 256,
               "churn_rounds": 3, "churn_fraction": 0.1 },
  "connect": { "established": 100, "failed": 0, "samples": 100,
               "min_ms": .., "mean_ms": .., "p50_ms": .., "p95_ms": ..,
               "p99_ms": .., "p999_ms": .., "max_ms": .. },
  "memory":  { "available": true, "server_pid": 123,
               "rss_before_kb": .., "rss_after_kb": .., "per_conn_bytes": .. },
  "churn":   { "rounds": 3, "batch": 10, "reconnect_established": 30,
               "reconnect_failed": 0, "p99_ms": .., "cliff_ratio": .. }
}
```

`ok` is `true` iff every requested connection established. The CI gate
(slice 7, line 168) parses this object and threshold-checks the numbers.

## Status — clean to 50K; 100K blocked by establishment-rate collapse

The original accept-loop wedge (held ≤1 concurrent connection) was
**resolved 2026-05-28** (dispatcher-yield async integration +
`spawn`-move double-drop fix). With that fixed, the harness now drives
real scale on a single unprivileged Linux box (2 cores, 8 GB):

- **Clean baseline: 50,000 idle wss:// connections** — 50000/50000
  established, 0 failed, in 72.8 s; connect p99 208 ms; **7.9 KB/conn**;
  churn cliff_ratio 6.6.
- Per-connection memory is linear at ~7.9 KB/conn from 1K upward, so
  100K would cost only ~790 MB server RSS — memory is not the limiter.

**100K is not yet reachable**: establishment throughput degrades
superlinearly with held-connection count (925/s @10K → 686/s @50K →
~6/s @77K), stalling acceptance near ~77K. The *hold* is stable (linear
memory, ~5 server threads for tens of thousands of parked connections);
the wall is server-side accept/establish throughput. Full topology,
tuning, the results ladder, and root-cause analysis live in
`docs/investigations/demo1_m1_verification.md`. The collapse is filed as
its own P0 blocker under phase-6-runtime.md (Demo 1 sub-entries). The
harness itself is correct and complete — it will produce the 100K number
unchanged once that blocker is fixed.

## See also

- `docs/implementation_checklist/phase-6-runtime.md` § Flagship Demo 1
  (line 170) — parent epic; slice 4 (M1 100K verification) consumes this
  harness; slice 7 (CI gate) wires CI against its JSON.
- `examples/parallax/bench/` — sister bench harness (HTTP throughput via
  `wrk`); `docs/investigations/bench_robustness.md` — its measurement-
  methodology gap log (G4 = the p99.9 limitation this harness avoids).
