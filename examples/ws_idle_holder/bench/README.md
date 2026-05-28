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
connection is counted as a failure instead of hanging the run).

TLS verification is disabled (the demo's cert is the self-signed
loopback test fixture). This harness must only ever target loopback.

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

## Status — verified at N=1; multi-connection blocked by a server bug

The harness pipeline is verified end-to-end at **N=1** (connect ~0.8 ms,
RSS delta captured, well-formed JSON, exit 0). It is **correct and
complete as a measurement instrument.**

Running it at N≥2 immediately surfaced a **server-side concurrency bug**:
the demo accepts and handshakes one connection but wedges its accept
loop once a handler is parked — it holds **at most 1 concurrent**
connection (or 2 sequential with close-before-next) before further TLS
handshakes hang. Slices 1/2 were only ever smoke-tested at a *single*
connection (`nc` / one Python `ssl` client), so this harness is the
first multi-connection exercise of the server — and it did its job by
catching the wedge. This is a hard blocker for slice 4 (M1 100K) and is
tracked as its own P0 entry under phase-6-runtime.md (see "Demo 1
flagship-server accept loop wedges under concurrency"). The harness will
produce the M1 number unchanged once that bug is fixed.

## See also

- `docs/implementation_checklist/phase-6-runtime.md` § Flagship Demo 1
  (line 170) — parent epic; slice 4 (M1 100K verification) consumes this
  harness; slice 7 (CI gate) wires CI against its JSON.
- `examples/parallax/bench/` — sister bench harness (HTTP throughput via
  `wrk`); `docs/investigations/bench_robustness.md` — its measurement-
  methodology gap log (G4 = the p99.9 limitation this harness avoids).
