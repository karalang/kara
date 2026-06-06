# `ws_idle_holder/dotnet` — ASP.NET Core / Kestrel reference impl (comparator)

A raw **ASP.NET Core Kestrel** WebSocket-over-TLS server that mirrors the
`ws_idle_holder` flagship demo: it holds N idle connections so the shared
bench harness can measure per-connection memory (density), connect latency,
and churn against Kāra. Commercial-tier comparator **#71 (.NET on Linux)**.

## Why a .NET comparator

.NET is a top-tier enterprise runtime, and ASP.NET Core's Kestrel +
`UseWebSockets()` middleware is the lean .NET WebSocket prod default. The
Linux run is the headline (60–70% of new .NET is Linux); a separate
**#72 (.NET Windows)** run on x86 + SChannel quantifies the OS-TLS-substrate
delta.

## Real-world-vs-purist caveat

This is **raw Kestrel WebSocket middleware** (`app.UseWebSockets()` + an echo
middleware) — **not SignalR**, which adds hub/RPC/backplane overhead and is a
distinct framework-tier stretch comparator (#74). No framework overhead is
folded into the per-conn number; this is the lean .NET prod default.

## Design choices

- **Raw Kestrel + `UseWebSockets()`** with a minimal echo middleware at `/`.
  The harness sends `GET / HTTP/1.1` with the RFC 6455 Upgrade headers, so
  **no harness changes** are needed (bare-WS, like Go/Netty — unlike Phoenix).
- **TLS:** in-process Kestrel HTTPS (OpenSSL on Linux), TLS 1.2 + 1.3, no
  client auth, single self-signed cert — the same `tests/fixtures/tls`
  fixture (CN=localhost), copied next to the binary and resolved via
  `AppContext.BaseDirectory`. The PEM is loaded then re-exported/imported as
  PFX so SslStream accepts the key handle on every platform. In-process TLS
  is the apples-to-apples basis (every comparator terminates TLS in-process).
- **Server GC** (`ServerGarbageCollection=true`) — the ASP.NET Core prod
  default (per-core heaps, throughput-oriented), concurrent GC on. No tuning.
- **Ephemeral port → `BOUND_PORT`:** binds `127.0.0.1:0`, reads the actual
  port from `app.Urls` after `Start()`, prints `BOUND_PORT=<n>` on stdout.
- **Self-contained publish:** `dotnet publish --self-contained` bundles the
  runtime, so the **rig needs no .NET install** — just scp the publish folder
  and run. `run_server.sh` execs the bundled binary in place, so the
  harness-spawned PID is the measured process (correct RSS).

### Expect a GC-heap dial (like the JVM)

.NET's Server GC commits heap per core and reclaims lazily, so — exactly as
with the [Netty comparator](../java/README.md) — the JVM/CLR's **RSS is
dominated by GC heap-commit**, not purely by per-connection live memory. The
harness's `per_conn_bytes = RSS-delta / N` will therefore be heap-policy-
influenced; the honest reads are the **marginal slope** and the post-GC
**live set** (`DOTNET_GCHeapHardLimit` / `GCHeapAffinitizeMask` are the .NET
analogs of the JVM `-Xmx` dial). The at-scale writeup reports those rather
than a single RSS number — see the §.NET section of `bench/REPORT.md` when it
lands.

## Usage

```bash
# Build a self-contained publish (run inside this dir). Pick the RID for the
# target: osx-arm64 (local), linux-arm64 / linux-x64 (rig).
dotnet publish -c Release -r osx-arm64 --self-contained -o publish

# Bench it through the shared harness:
cd ../bench
cargo run --release -- \
  --server-bin ../dotnet/run_server.sh \
  -n 500 --concurrency 64 --churn-rounds 0 --hold-secs 3
```

## At-scale results

**NOT YET RUN.** Prepped + locally validated; the 50K / 250K rig runs await a
user-provisioned box (per the bench-day rig-spend sign-off discipline).
Because the rig payload is a **self-contained** bundle, no .NET install is
needed there — publish `linux-arm64` locally and scp the folder. Expected
~15–30 KB/conn (with the GC-heap-dial caveat above). Results land here after
the run, mirroring the Go/Netty/Phoenix format.

### Reproduce — turnkey rig recipe

```bash
# 0. Kernel + nofile + loopback-alias setup (idempotent; fresh login after).
examples/ws_idle_holder/bench/scripts/ec2_setup.sh   # re-login here

# 1. The harness needs cargo; .NET is NOT needed on the rig (self-contained).
#    Build the publish on any .NET 8 box (e.g. your laptop) and scp it:
dotnet publish -c Release -r linux-arm64 --self-contained \
  -o examples/ws_idle_holder/dotnet/publish        # then scp the dir to the rig
( cd examples/ws_idle_holder/bench && cargo build --release )

# 2. Run both scales (bare-WS → no BENCH_EXTRA_ARGS needed):
cd examples/ws_idle_holder/bench/scripts
DOTNET="$(cd ../../dotnet && pwd)/run_server.sh"
./run_250k.sh "$DOTNET" dotnet_linux_250k.json
./run_50k.sh  "$DOTNET" dotnet_linux_50k.json

# 3. scp the JSONs off-box to docs/investigations/ and `git add` them BEFORE
#    terminating the instance.
```

> **GC-heap dial:** if 50K→250K `per_conn_bytes` drift exceeds 5% (likely, as
> with the JVM), report the marginal slope + a post-GC live-set read
> (`dotnet-gcdump` / `DOTNET_GCHeapHardLimit`) rather than the raw RSS, and a
> balanced-heap deployment point — do not treat raw RSS-delta/N as the
> per-conn density. The small-N smoke (~87 KiB/conn at N=500) is CLR warm-up
> noise, not density.

## Local validation (macOS, .NET 8.0.421)

- `dotnet publish` (osx-arm64 + linux-arm64 self-contained) clean; cert/key
  bundled; linux-arm64 produces an `ELF aarch64` binary (verified the rig
  cross-build from macOS).
- Harness via `--server-bin`, N=500: **500/500 established, 0 failed**; active
  echo **150 sent / 150 echoed / 0 failures** (TLS WS upgrade at `/`, idle
  hold, and echo confirmed end-to-end).

## What this impl deliberately omits

- **No SignalR** — raw Kestrel WS middleware only (SignalR is the framework-
  tier stretch comparator #74).
- **No GC tuning** — Server GC defaults; the heap dial is reported, not tuned.
- **Linux only here** — the Windows/SChannel run is comparator #72.
