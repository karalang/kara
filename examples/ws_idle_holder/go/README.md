# `ws_idle_holder/go` — Go reference impl (comparator)

A `gorilla/websocket` + `crypto/tls` mirror of the `ws_idle_holder`
flagship demo (`../src/main.kara`). Same end-to-end shape — bind a TLS
listener, print `BOUND_PORT=<n>`, accept WebSocket-over-TLS connections,
echo any frame and hold each idle until the peer closes — so the
`../bench/` harness measures all impls identically via `--server-bin`.

## Why a Go comparator

Go is the **first commercial-stack comparator** in the bench-day Phase 3
sweep. `gorilla/websocket` is the raw-library Go prod default — the
"what a competent Go shop ships" baseline, not a framework. Both this
server and the Kāra impl traverse the same kernel critical path
(`accept(2)` → TLS handshake → RFC 6455 upgrade), so the comparison
isolates language-runtime overhead from the IO substrate.

## Real-world-vs-purist caveat

This is the **raw `gorilla/websocket` library on `net/http` + `crypto/tls`**
— the high-density Go WS prod baseline, deliberately *not* a framework
(no router, no RPC layer, no socket.io-style presence). That mirrors the
Rust comparator's "raw `tokio` + `tokio-tungstenite`" choice and the
project's apples-to-apples discipline: compare against the lean prod
default a real Go shop would reach for first, and call out any framework
overhead separately when a framework-tier comparator is added. There is
no framework overhead folded into this number.

## Design choices

- **Concurrency model:** `net/http`'s goroutine-per-connection via
  `http.Server.ServeTLS`, with an `http.Handler` that calls
  `upgrader.Upgrade`. This is the idiomatic gorilla server shape — what
  a competent Go developer writes, not a hand-tuned mirror of Kāra's
  internal handshake-worker pool. Go's scheduler multiplexes the
  goroutines across `GOMAXPROCS` OS threads (the analogue of the Rust
  comparator's tokio multi-thread runtime).
- **WS upgrade:** `gorilla/websocket` `Upgrader`. Equivalent to the
  Kāra runtime's hand-rolled `ws_drive_upgrade_handshake`
  (`runtime/src/event_loop.rs`) and the Rust comparator's
  `tokio-tungstenite::accept_async`.
- **TLS:** `crypto/tls` (Go stdlib), `MinVersion = TLS 1.2` (max
  defaults to TLS 1.3), no client auth, single cert — mirrors the
  rustls posture in `runtime/src/tls.rs`. Pure-Go, no OpenSSL/cgo link
  (the real-world Go prod default).
- **Listen backlog:** idiomatic `net.Listen`. Go derives the `listen(2)`
  backlog from `/proc/sys/net/core/somaxconn` on Linux, so a Go dev
  raises the sysctl rather than hand-coding a backlog.
  `../bench/scripts/ec2_setup.sh` sets `net.core.somaxconn=65535`, so
  Go's auto-backlog matches the Rust comparator's explicit
  `socket2 listen(65535)` on the rig.
- **TCP_NODELAY:** on by default for all Go TCP conns — matches the Rust
  comparator's explicit `set_nodelay(true)`.
- **Buffers:** `ReadBufferSize`/`WriteBufferSize = 4096` (gorilla's
  defaults, stated explicitly) — the same 4 KB recv buffer the Kāra
  demo's `handle_connection` allocates, so the per-connection buffer
  cost is part of the documented, comparable footprint.
- **Cert + key:** inlined as PEM string constants in `main.go`, exactly
  as `../src/main.kara` inlines them (the same committed fixtures at
  `tests/fixtures/tls/`). Go's `//go:embed` cannot reference a parent
  directory, so unlike the Rust comparator's `include_str!` of the
  shared fixture, the bytes are inlined here — the truest mirror of the
  Kāra demo, which inlines too.
- **Build posture:** `go build -ldflags="-s -w" -trimpath` — strip debug
  + symbol tables and drop absolute build paths, the Go analogue of the
  Rust comparator's `strip = "symbols"`. Go has no LTO knob (whole-program
  dead-code elimination is the default) and no `panic = "abort"`
  equivalent (Go panics unwind), but a steady-state idle/echo server
  never panics, so the runtime-size comparison stays honest.

## Usage

```sh
# Build (standalone module — run inside this dir):
go build -ldflags="-s -w" -trimpath -o ws-idle-holder-go .

# Bench it through the shared harness:
( cd ../bench && cargo build --release )
../bench/target/release/ws-idle-holder-bench \
    --server-bin "$(pwd)/ws-idle-holder-go" \
    -n 1000 --concurrency 64 --churn-rounds 0
```

The `--server-bin` flag is identical to the Kāra- and Rust-server
invocations — the harness reads `BOUND_PORT=<n>` from the spawned
process's stdout and measures its RSS via `ps -o rss=`.

## At-scale results (landed 2026-06-06)

Run on a fresh **`m8g.4xlarge`** (16-vCPU Graviton4, 61 GB), co-located
client+server over loopback, prod-default runtime config (`GOGC=100`,
`GOMAXPROCS=16`):

| scale | established | per-conn | connect p50 / p99 |
|---|---|---|---|
| **250K** (headline) | 250,000 / 0 failed | **44,386 B (43.35 KiB)** | 3.37 / 9.73 ms |
| **50K** (linearity) | 50,000 / 0 failed | 43,311 B (42.30 KiB) | 3.43 / 15.56 ms |

**Linearity drift 50K→250K = +2.5%** (< 5% gate → published at 250K, no
1M escalation). **Headline: Kāra holds ~3.66× the density** (12.1 KB vs
44.4 KB/conn), and Go lands ~1.59× heavier than even the Rust comparator
(27.9 KB) — the structural cost of a goroutine per blocked `ReadMessage`
plus `crypto/tls`'s per-conn record buffers plus gorilla's 4 KB×2
buffers, none shared across connections. Go's connect latency is faster
(~3.4 ms p50 vs Kāra's ~41 ms architectural floor), but density is the
headline metric. Full head-to-head + caveats in
[`../bench/REPORT.md` §Go](../bench/REPORT.md). Raw JSON:
`docs/investigations/go_idle_{250k,50k}.json`.

### Reproduce — turnkey rig recipe

Per the bench-day comparator scale split, the commercial comparators run
**250K (headline) + 50K (linearity sub-curve)** on a fresh Linux EC2 box,
with a 1M escalation only if the 50K→250K `per_conn_bytes` drift exceeds
5% (Go passed at +2.5%).

The runners (`../bench/scripts/run_250k.sh`, `run_50k.sh`) are analogues
of `run_1m.sh` — same flag set, only N / ulimit / source-IP fan-out
differ. On-box, one-command-per-scale recipe (Ubuntu 24.04 arm64, the AMI
used for the Kāra 1M verification):

```sh
# 0. Kernel + nofile + loopback-alias setup (idempotent; needs a fresh
#    login afterward so the systemd nofile cap actually lifts).
sudo bash examples/ws_idle_holder/bench/scripts/ec2_setup.sh
exit   # then SSH back in; verify: ulimit -n  ->  3000000

# 1. Toolchains. ec2_setup.sh does NOT install compilers — only the Go
#    toolchain is needed for THIS comparator (no karac/runtime build,
#    since the comparator binary is self-contained):
sudo snap install go --classic    # or the official tarball; any Go >= 1.21

# 2. Build the comparator + the bench harness on-box:
( cd examples/ws_idle_holder/go && go build -ldflags="-s -w" -trimpath -o ws-idle-holder-go . )
( cd examples/ws_idle_holder/bench && cargo build --release )

# 3. Run both scales (JSON is tee'd to ./<basename>-{250k,50k}.json):
GO_BIN="$(pwd)/examples/ws_idle_holder/go/ws-idle-holder-go"
examples/ws_idle_holder/bench/scripts/run_250k.sh "$GO_BIN"
examples/ws_idle_holder/bench/scripts/run_50k.sh  "$GO_BIN"
```

Then **immediately** scp both JSONs to `docs/investigations/` in the
local repo and `git add` them — scp is not `git add`, and an untracked
JSON sitting in `docs/investigations/` while REPORT.md claims it "wasn't
mirrored" is a documented failure mode. Only after the JSON is committed:
update REPORT.md's comparator row, the status matrix, the phase-6 entry,
and this README's numbers section.

## Local validation (2026-06-05, macOS, N=200 c=64, active echo)

A correctness smoke run on the dev box, **not a density measurement**:

```
established 200/200 (0 failed)
active traffic: 750 sent / 750 echoed / 0 echo failures
active echo p50 0.080 ms / p99 0.130 ms
```

This confirms the `BOUND_PORT` handshake, TLS, RFC 6455 upgrade, and the
echo round-trip all work end-to-end through the shared harness. The
`per_conn_bytes` reported at N=200 (~88 KB) is **not** the density figure
— at tiny N it is dominated by fixed Go-runtime + goroutine-stack
overhead and RSS granularity, exactly the small-N artifact the Rust
comparator README flags for macOS. The real density number comes from
the 250K Linux rig run.

## What this impl deliberately omits

- No structured logging (`http.Server.ErrorLog` is silenced so a
  bad-handshake storm doesn't spam stderr / the harness channel).
- No graceful shutdown / max-conn cap.
- No connection-attempt rate limiting.
