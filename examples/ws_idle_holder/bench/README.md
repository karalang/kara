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

### EC2 1M rig — `scripts/`

The canonical N=1M c=64 idle-hold run lives in `scripts/`:

- **`scripts/ec2_setup.sh`** — sysctl bumps (`somaxconn`,
  `tcp_max_syn_backlog`, `ip_local_port_range`, `tcp_rmem` / `wmem`),
  loopback aliases `127.0.0.2..28`, `/etc/security/limits.d/`
  `nofile = 1250000`. Linux-only; needs `sudo`; idempotent. Captures
  the tunings discovered during the 2026-05-29 Kāra 1M verification.
- **`scripts/run_1m.sh <server-bin> [output.json]`** — wraps the
  harness with the canonical flags (`-n 1000000 --concurrency 64
  --churn-rounds 0 --connect-timeout-ms 30000 --source-ips 127.0.0.2..28`)
  so Kāra and Rust-comparator runs are guaranteed identical. Sets
  `ulimit -n 1250000` inline; tails `dmesg` post-run for SYN-flood
  signals.

End-to-end EC2 flow:

```sh
# On a fresh r8g.4xlarge (or equivalent), Ubuntu 24.04 arm64:
git clone <repo> && cd kara
sudo bash examples/ws_idle_holder/bench/scripts/ec2_setup.sh

# Build everything once:
cargo build --features llvm --release            # karac compiler
cargo build -p karac-runtime --release           # runtime lib
( cd examples/ws_idle_holder
  KARAC_RUNTIME="$PWD/../../target/release/libkarac_runtime.a" \
    ../../target/release/karac build )           # Kāra demo
( cd examples/ws_idle_holder/rust && cargo build --release )   # Rust comparator
( cd examples/ws_idle_holder/bench && cargo build --release )  # bench harness

# Run both at 1M, save JSONs:
bash examples/ws_idle_holder/bench/scripts/run_1m.sh \
    examples/ws_idle_holder/ws_idle_holder \
    kara-1m.json
bash examples/ws_idle_holder/bench/scripts/run_1m.sh \
    examples/ws_idle_holder/rust/target/release/ws-idle-holder-rust \
    rust-1m.json
```

**Before terminating the rig — pull the JSONs off-box.** This is a hard
gate, not optional: once the instance is gone, the raw JSON artifacts
are gone with it. The denormalized headline numbers survive in
`REPORT.md`, but the full per-percentile / per-step structure (needed
for audit, replay, or any later re-analysis) does not. Each `run_*m.sh`
prints the absolute path of its output JSON as its last log line — use
those paths in the `scp` step. Pattern (from your local shell, before
`aws ec2 terminate-instances`):

```sh
# Pull both runs into the local repo (paths shown by run_*m.sh log tail):
scp ec2-user@<rig>:/path/to/kara-1m.json     docs/investigations/demo1_m3_1m.kara.json
scp ec2-user@<rig>:/path/to/rust-1m.json     docs/investigations/demo1_m3_1m.rust.json
# (At 2M, rename target files to *_2m.* — same pattern.)
```

The script tail emits a `>>> BEFORE TERMINATING` reminder block as a
backstop; treat the JSON-off-box step as a peer to "ship the commit"
in the per-comparator close-out flow.

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

EC2 re-verification at `--concurrency 64` on the same r8g.4xlarge
collapsed the latency to `mean=81.7ms, p99=256ms` for the same 1 M
connections (`docs/investigations/demo1_m3_curve_c64.json`),
confirming the queue-depth diagnosis: 8× concurrency reduction →
5.7× mean drop and 7.2× p99 drop, matching the `(C/W)×T` prediction
to within 0.05% (predicted 81.7 ms, observed 81.74 ms).

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
(slice 7) parses this object and threshold-checks the numbers.

## CI regression gate (slice 7)

`scripts/bench_gate.py` + `scripts/bench_baseline.json` wire this harness
into CI as a per-PR regression gate — the `bench-gate` job in
`.github/workflows/ci.yml`. The job builds the demo with `karac build`,
runs the harness at a **CI load tier** (2000 loopback connections + 3
churn rounds — CI cannot hold the canonical 1M+, so it runs a small
steady-state exercise that still catches regressions), pipes the JSON to
the gate, and compares against the committed baseline.

Two independent checks, in order:

1. **Correctness (hard, never overridable)** — every requested connection
   established, zero connect failures, zero churn reconnect failures, and
   the harness's own `ok` flag true. A build that drops connections is not
   a "5%-regression" question; an override must not let it merge.
2. **Regression vs baseline (overridable with justification)** — the
   tracked steady-state metrics did not get more than the per-metric
   `tolerance_pct` worse: connect `p50 / p95 / p99 / p99.9` (establishment
   cost, tracked separately per the roadmap), `per_conn_bytes` (idle
   density), and `cliff_ratio` (P99 churn cliff, which also has an
   absolute ceiling). Higher is worse for every metric; improvements pass.

`per_conn_bytes` is the tight, deterministic sentinel — idle density is
scale- and machine-invariant (~1 % run-to-run), so it carries a 12 %
gate. Connect latency on a shared runner is noisy, so its tolerances are
wide and catch only gross regressions. Tolerances live in the baseline
JSON and are tuned per-metric with **no code change**.

```sh
# Run the gate locally against a captured report:
python3 scripts/bench_gate.py --report run.json --baseline scripts/bench_baseline.json

# Recalibrate the baseline from a representative run (preserves tolerances):
python3 scripts/bench_gate.py --report run.json --baseline scripts/bench_baseline.json --update-baseline

# Verify the gate's own logic (fast, no build — also runs first in CI):
python3 scripts/bench_gate.py --selftest
```

**Override.** A >tolerance regression fails the job unless overridden.
Set `BENCH_GATE_OVERRIDE=<justification>` locally, or in CI add a
`[bench-override: <reason>]` token to the head commit message — the job
lifts the reason into the env var, and the regression downgrades to a
warning. Correctness failures are never downgraded.

The job is **non-required** (not in branch protection) until it builds a
green streak: a noisy-latency false positive on a shared runner must not
block the required gates. The baseline was seeded on an
ubuntu-latest-class box (4 vCPU / 16 GB x86-64 Linux, LLVM 18.1.3); if the
actual runner sits systematically off, recalibrate with
`--update-baseline` from a green run.

## Status — landed; drove the full Phase-3 density campaign (to 2M)

The original accept-loop wedge (held ≤1 concurrent connection) was
**resolved 2026-05-28** (dispatcher-yield async integration +
`spawn`-move double-drop fix). The follow-on establishment-rate collapse
that capped early single-box runs near ~77K was **cleared by the
2026-05-29 1M verification** — the sysctl / `nofile` / loopback-alias
tunings now captured in `scripts/ec2_setup.sh`. With both resolved, this
harness drove the entire Phase-3 comparator campaign:

- **Kāra + Rust reference — 1M and 2M idle wss:// connections, 0
  failures**, head-to-head on one r8g.4xlarge: **Kāra ~12.1 KB/conn vs
  Rust ~27.9 KB/conn (2.30×)**, scale-invariant 1M↔2M (Kāra −0.03 %
  drift).
- **Five commercial comparators at 250K headline + 50K linearity**
  (2026-06-06), each on a fresh box, apples-to-apples in-process TLS:
  Phoenix Channels, Java/Netty, Go (gorilla/websocket), .NET/Kestrel
  (Linux), Node.js (`ws`).

Density ladder (per-conn server RSS, densest first): **Kāra 12.1 KB** ·
Netty 14.4 KB¹ · Rust 27.9 KB · Node 40.4 KB · Go 43.4 KB · .NET 52.9 KB
· Phoenix 102.8 KB.

Full methodology, per-comparator tables, GC-dial analysis, and the
production-cost reframes live in **[`REPORT.md`](REPORT.md)** (the
authoritative record). The early single-box figures this section used to
carry (a 50K / 7.9 KB-per-conn baseline, the ~77K acceptance wall) are
**superseded** by the at-scale campaign; that historical M1 ramp and its
root-cause analysis remain in `docs/investigations/demo1_m1_verification.md`.

¹ Netty's RSS is a JVM `-Xmx` dial — 14.4 KB is its balanced-heap
deployment point (marginal slope ~12.8 KB, live set ~8–10 KB); every
other stack reports a fixed live footprint. See REPORT.md § Java/Netty.

## See also

- `docs/implementation_checklist/phase-6-runtime.md` § Flagship Demo 1
  (line 170) — parent epic; slice 4 (M1 100K verification) consumes this
  harness; slice 7 (CI gate) wires CI against its JSON.
- `examples/parallax/bench/` — sister bench harness (HTTP throughput via
  `wrk`); `docs/investigations/bench_robustness.md` — its measurement-
  methodology gap log (G4 = the p99.9 limitation this harness avoids).
