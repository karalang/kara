# `ws_idle_holder/rust` — Rust reference impl (comparator)

A `tokio` + `rustls` + `tokio-tungstenite` mirror of the
`ws_idle_holder` flagship demo (`../src/main.kara`). Same end-to-end
shape — bind a TLS listener, print `BOUND_PORT=<n>`, accept
WebSocket-over-TLS connections, hold each idle until the peer
closes — so the `../bench/` harness measures both impls identically.

## Why a Rust comparator

`docs/implementation_checklist/phase-6-runtime.md` line 170 names the
ws-idle-holder workload as the runtime's M1/M2/M3 scale target. A
Rust reference built on the same kernel surface (tokio + rustls) gives
an "honest perf ceiling" against which the Kāra impl is measured —
both traverse the same `accept(2)` → TLS handshake → RFC 6455 upgrade
critical path, so the comparison isolates language-runtime overhead
from the IO substrate.

## Design choices

- **Async runtime:** `tokio` multi-thread, `tokio::spawn` per
  connection — the natural Rust shape, not a hand-tuned mirror of
  Kāra's internal handshake-worker pool.
- **WS upgrade:** `tokio-tungstenite::accept_async`. Equivalent to
  the Kāra runtime's hand-rolled `ws_drive_upgrade_handshake` in
  `runtime/src/event_loop.rs`.
- **TLS:** `rustls 0.23` with the `ring` crypto provider — exactly
  what `runtime/src/tls.rs` uses. No aws-lc-rs, no native TLS, no
  openssl. v1 protocol-versions default (TLS 1.2 + 1.3), no client
  auth.
- **Listen backlog:** `socket2` `listen(65535)` on Linux,
  `listen(16384)` on macOS — matches the Kāra runtime's
  `KARAC_RUNTIME_TCP_LISTEN_BACKLOG`. macOS silently breaks loopback
  acceptance above 16384 even with `kern.ipc.somaxconn` raised.
- **Cert + key:** `include_str!` of `tests/fixtures/tls/{cert,key}.pem`,
  shared with the Kāra demo so both impls source from the same PEM.
- **Release profile:** `lto = "fat"`, `codegen-units = 1`,
  `strip = "symbols"`, `panic = "abort"` — same posture as the
  Kāra-built demo so the comparison isn't distorted by a laxer Rust
  release profile.

## Usage

```sh
# Build (standalone — run inside this dir):
cargo build --release

# Bench it through the shared harness:
( cd ../bench && cargo build --release )
../bench/target/release/ws-idle-holder-bench \
    --server-bin target/release/ws-idle-holder-rust \
    -n 1000 --concurrency 64 --churn-rounds 0
```

The `--server-bin` flag is identical to the Kāra-server invocation —
the harness reads `BOUND_PORT=<n>` from the spawned process's stdout
and measures its RSS via `ps -o rss=`, exactly as it does against
`../ws_idle_holder` (the Kāra demo binary).

## Initial macOS M5 Pro numbers (2026-05-30, N=5000 c=128, no churn)

|                       | Kāra      | Rust (tokio) |
|-----------------------|-----------|--------------|
| `connect.mean_ms`     | 4.30      | 4.30         |
| `connect.p50_ms`      | 4.07      | 4.02         |
| `connect.p95_ms`      | 5.89      | 5.03         |
| `connect.p99_ms`      | 7.14      | **13.78**    |
| `per_conn_bytes`      | **8703**  | 30435        |
| `rss_after_kb` peak   | 44752     | 151232       |

**Read:** at N=5000 c=128 on a single M5 Pro, the two impls are
indistinguishable at the median; the Rust impl's p50/p95 are
fractionally faster, but its p99 is ~2× the Kāra p99 and **its
per-connection memory is ~3.5× the Kāra impl's**. Kāra's
synchronous handshake worker pool yields tighter tails at this scale
than tokio's task-per-connection shape; tokio-tungstenite's
per-task buffer overhead dominates the memory delta. The mean wash is
expected — the median TLS handshake is dominated by ring's symmetric
crypto, identical on both sides. **These are macOS single-box
numbers** — the headline parity comparison ships against the
r8g.4xlarge EC2 rig used for the Kāra 1M Linux verification, where
both impls have the full 16 vCPU / 65535 listen-backlog ceiling.

**Caveat — Rust at higher N on macOS:** at `N ≥ 10000, c=128` with
the default single 127.0.0.1 source IP, the Rust comparator stalls
client-side with `Can't assign requested address (os error 49)` after
~2000 connections — ephemeral-port pressure on the loopback tuple,
not a server-side defect. Use `--source-ips 127.0.0.2,127.0.0.3,...`
(the same mitigation the Kāra demo's macOS-at-1M path will need) for
N beyond ~5000 on macOS. Linux's 28K-port single-tuple cap allows
higher N before this surfaces; the r8g.4xlarge EC2 path uses 27
source IPs to push past that.

## What this impl deliberately omits

- No echo / per-connection work — idle hold is the workload.
- No structured logging.
- No graceful shutdown / max-conn cap.
- No connection-attempt rate limiting.
