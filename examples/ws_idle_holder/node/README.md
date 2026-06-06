# `ws_idle_holder/node` — Node.js (`ws` + `https`/OpenSSL) reference impl (comparator)

A raw **Node.js `ws`-library** WebSocket-over-TLS server that mirrors the
`ws_idle_holder` flagship demo (`../src/main.kara`): same end-to-end shape
— bind a TLS listener, print `BOUND_PORT=<n>`, accept WebSocket-over-TLS
connections, echo any frame and hold each idle until the peer closes — so
the `../bench/` harness measures every impl identically via `--server-bin`.
Commercial-tier comparator **#73 (Node.js on Linux)**.

## Why a Node.js comparator

Node.js is one of the most-deployed server runtimes, and the `ws` library
is the raw-library Node WebSocket prod default — the "what a competent Node
shop ships" baseline, **not** socket.io (which adds an engine.io
transport/RPC/room layer and is a distinct framework-tier stretch
comparator, #75). Both this server and the Kāra impl traverse the same
kernel critical path (`accept(2)` → TLS handshake → RFC 6455 upgrade), so
the comparison isolates language-runtime overhead from the IO substrate.

## Real-world-vs-purist caveat

This is the **raw `ws` library on `https` + OpenSSL** — the lean Node WS
prod default, deliberately *not* a framework (no socket.io, no router, no
RPC layer, no rooms/presence). That mirrors the Rust comparator's "raw
`tokio` + `tokio-tungstenite`", Go's "raw `gorilla/websocket`", and .NET's
"raw Kestrel `UseWebSockets()`" choices, and the project's apples-to-apples
discipline: compare against the lean prod default a real Node shop reaches
for first, and call out any framework overhead separately when a
framework-tier comparator (socket.io #75) is added. No framework overhead
is folded into this number.

## Design choices

- **Concurrency model:** Node's **single-threaded libuv event loop**.
  Unlike Go's goroutine-per-connection or the JVM/.NET thread-pool models,
  Node multiplexes every connection over one OS thread running a
  callback-driven reactor — architecturally the **closest of all the
  comparators to Kāra's own event-loop reactor** (`runtime/src/
  event_loop.rs`) and to the Rust comparator's single tokio runtime. A
  real Node shop at very high conn counts often runs the `cluster` module
  (one worker per core) behind a load balancer, but that is a
  core-scaling / throughput choice, not a density one: each worker holds
  its share at the same per-conn cost, and the harness measures one
  process's RSS. A single process is therefore both the honest
  per-process density measure and the apples-to-apples basis (every
  comparator is measured as one OS process).
- **WS upgrade:** the `ws` library's `WebSocketServer` in attached mode,
  hooking the `https` server's `'upgrade'` event to drive the RFC 6455
  handshake. Equivalent to the Kāra runtime's hand-rolled
  `ws_drive_upgrade_handshake`, gorilla's `Upgrader`, and the Rust
  comparator's `tokio-tungstenite::accept_async`.
- **TLS:** in-process `https.createServer` over Node's **bundled OpenSSL
  3.x**, `minVersion = TLS 1.2` (max defaults to TLS 1.3), no client auth,
  single self-signed cert. This is the **same OpenSSL substrate as the
  .NET-on-Linux comparator (#71)** — and unlike Go's pure-Go `crypto/tls`
  or Kāra's rustls — so the Node-vs-.NET-Linux pair reads cleanly as
  runtime overhead over a shared TLS stack. In-process TLS is the
  apples-to-apples basis (every comparator terminates TLS in-process).
- **`perMessageDeflate: false`** — set explicitly (also `ws`'s own
  default, since a per-conn zlib context is expensive). No compression
  context is allocated per connection, matching every other comparator
  (none compress); keeps the per-conn density honest.
- **No optional native addons.** `ws` can optionally load `bufferutil` /
  `utf-8-validate` (C++ addons) for faster masking / UTF-8 validation;
  they are **not** installed, so the build is pure-JS and the rig needs no
  native toolchain. They affect CPU, not per-conn density.
- **Listen backlog:** explicit `listen(0, '127.0.0.1', 65535)`. Node's
  default backlog is 511 and — unlike Go — it does not auto-read
  `/proc/sys/net/core/somaxconn`, so we pass 65535 to match the rig's
  `net.core.somaxconn=65535` (set by `../bench/scripts/ec2_setup.sh`) and
  the Rust comparator's explicit `socket2 listen(65535)`.
- **TCP_NODELAY:** on by default for all Node TCP sockets, matching the
  Rust comparator's explicit `set_nodelay(true)`.
- **Cert + key:** read from the committed fixture files `cert.pem` /
  `key.pem` next to `server.js` (resolved via `__dirname`) — the same
  self-signed test fixtures (CN=localhost, valid through 2036) at
  `tests/fixtures/tls/`. The .NET comparator reads sibling PEMs the same
  way; Go inlines the identical bytes (`//go:embed` can't reach a parent
  dir). None expose anything not already committed.
- **Runtime version:** **Node.js 24 LTS** is the target prod default
  (`engines: ">=22"`; locally validated on Node 25). `ws` is pinned via
  `package-lock.json` (committed) and installed with `npm ci`.

## Usage

```sh
# Install the pinned dependency tree (run inside this dir):
npm ci --omit=dev

# Bench it through the shared harness:
( cd ../bench && cargo build --release )
../bench/target/release/ws-idle-holder-bench \
    --server-bin "$(pwd)/run_server.sh" \
    -n 1000 --concurrency 64 --churn-rounds 0
```

The `--server-bin` flag is identical to every other comparator — the
harness reads `BOUND_PORT=<n>` from the spawned process's stdout and
measures its RSS via `ps -o rss=`. `run_server.sh` `exec`s `node
server.js`, so the harness-spawned PID *is* the measured process.

## At-scale results

> **NOT YET RUN — comparator prepped and locally validated, awaiting a
> rig box.** The headline 250K + 50K-linearity runs land on a fresh
> Linux EC2 box per the bench-day cadence; numbers and the head-to-head
> with Kāra go here and in **§Node.js in `bench/REPORT.md`** (the
> authoritative record) once measured.

**Expected range (public data):** ~30–50 KB/conn. The per-conn cost is V8
object overhead for the `ws` socket wrapper + Node stream buffers + the
per-conn OpenSSL `SSL` record buffers (the same SslStream-class native
buffers the .NET-Linux comparator showed are *not* pooled) + libuv
per-handle state. Single-threaded, so no per-conn thread/goroutine stack
— which should land Node below the goroutine-per-conn Go number and in the
neighborhood of (or above) .NET, both sharing OpenSSL.

### GC-heap dial — anticipated; to be resolved by measurement

V8 has a managed heap with lazy commit, so going in there is the same
concern the [Netty](../java/README.md) and [.NET](../dotnet/README.md)
comparators raised: that `per_conn_bytes = RSS-delta / N` is a
`-Xmx`-style dial (`--max-old-space-size` is V8's analog) rather than live
memory. Two facts argue it will land like .NET (a real number), not like
the JVM (a dial): (1) most per-conn state — socket/TLS record buffers,
`ws` frame buffers — is **native C++ memory outside the V8 heap**, exactly
the unmanaged-buffer class that made .NET's number real; (2) the default
V8 old-space is small and grows on demand, not pre-committed like a JVM
`-Xmx` reservation. The methodology to **confirm at run time** mirrors
.NET's Workstation-GC cross-check:

1. **Linearity:** 50K→250K `per_conn_bytes` drift < 5% ⇒ slope ≈ absolute
   ⇒ live memory (no 1M escalation needed).
2. **Heap-cap sidebar:** re-run 50K with
   `NODE_OPTIONS=--max-old-space-size=512`; if RSS-delta/N barely moves,
   the per-conn cost is native (live), not V8 heap slack.

Both are cheap and decide whether the headline is the raw RSS-delta/N or a
caveated marginal-slope range. Documented as *anticipated* — the data
decides, as it did for .NET.

### Reproduce — turnkey rig recipe

Per the bench-day comparator scale split, commercial comparators run
**250K (headline) + 50K (linearity sub-curve)** on a fresh Linux EC2 box,
with a 1M escalation only if the 50K→250K `per_conn_bytes` drift exceeds
5%.

```sh
# 0. Kernel + nofile + loopback-alias setup (idempotent; fresh login after
#    so the systemd nofile cap actually lifts).
sudo bash examples/ws_idle_holder/bench/scripts/ec2_setup.sh
exit   # then SSH back in; verify: ulimit -n  ->  3000000

# 1. Toolchains. ec2_setup.sh installs no compilers. Node is an
#    interpreter (no self-contained bundle like Go/.NET), so install a
#    Node 24 LTS runtime + the harness's cargo:
sudo snap install node --classic --channel=24/stable   # or the official tarball
( cd examples/ws_idle_holder/node && npm ci --omit=dev )   # vendors `ws` from the lockfile
( cd examples/ws_idle_holder/bench && cargo build --release )

# 2. Run both scales (bare-WS → no BENCH_EXTRA_ARGS needed):
cd examples/ws_idle_holder/bench/scripts
NODE_BIN="$(cd ../../node && pwd)/run_server.sh"
./run_250k.sh "$NODE_BIN" node_linux_250k.json
./run_50k.sh  "$NODE_BIN" node_linux_50k.json

# 3. (If linearity or a heap-cap sidebar is wanted) 50K with the V8 heap
#    cap, the live-vs-slack cross-check:
NODE_OPTIONS=--max-old-space-size=512 ./run_50k.sh "$NODE_BIN" node_linux_50k_cap.json
```

Then **immediately** scp the JSONs to `docs/investigations/` in the local
repo and `git add` them — scp is not `git add`, and an untracked JSON
sitting in `docs/investigations/` while REPORT.md claims it "wasn't
mirrored" is a documented failure mode. Only after the JSON is committed:
update REPORT.md's comparator row, the status matrix, the phase-6 entry,
and this README's results section.

## Local validation (2026-06-06, macOS, Node 25.9.0, `ws` 8.21.0)

A correctness smoke run on the dev box, **not a density measurement**:

```
established 200/200 (0 failed)
active traffic: 150 sent / 150 echoed / 0 echo failures
```

This confirms the `BOUND_PORT` handshake, TLS, RFC 6455 upgrade, and the
echo round-trip all work end-to-end through the shared harness. The
`per_conn_bytes` reported at N=200 (~67 KB) is **not** the density figure —
at tiny N it is dominated by fixed V8 + Node-runtime overhead and RSS
granularity, exactly the small-N artifact the Go and Rust comparator
READMEs flag for macOS. The real density number comes from the 250K Linux
rig run.

## What this impl deliberately omits

- **No socket.io** — raw `ws` library only (socket.io is the
  framework-tier stretch comparator #75).
- **No `cluster` / multi-process fan-out** — single process (see the
  concurrency-model note; clustering is a core-scaling choice, not a
  density one).
- **No GC tuning** — V8 defaults; the heap dial is *measured*, not tuned.
- No structured logging (`clientError` and per-socket `error` swallowed so
  a bad-handshake storm can't crash the process / spam the harness).
- No graceful shutdown / max-conn cap / rate limiting.
- **Linux only here** — the headline run is Linux (where Node deploys).
