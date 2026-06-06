# `ws_idle_holder/dotnet` — ASP.NET Core / Kestrel reference impl (comparator)

A raw **ASP.NET Core Kestrel** WebSocket-over-TLS server that mirrors the
`ws_idle_holder` flagship demo: it holds N idle connections so the shared
bench harness can measure per-connection memory (density), connect latency,
and churn against Kāra. Commercial-tier comparator **#71 (.NET on Linux)**.

## Why a .NET comparator

.NET is a top-tier enterprise runtime, and ASP.NET Core's Kestrel +
`UseWebSockets()` middleware is the lean .NET WebSocket prod default. The
Linux run is the headline (60–70% of new .NET is Linux). A separate Windows +
SChannel run (was #72) would have quantified the OS-TLS-substrate delta, but
it was **cut by decision (2026-06-06)** — Linux is the .NET headline, the
SChannel-vs-OpenSSL delta is low-value (the Node comparator already supplies a
second OpenSSL data point next to this one), and an x86 Windows box is the
costliest run in the suite. See **§.NET Windows in `bench/REPORT.md`**.

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

### GC-heap dial — anticipated, but it did NOT materialize

Going in, the concern was that .NET's Server GC (per-core heaps, lazy
reclaim) would make `per_conn_bytes = RSS-delta / N` a `-Xmx`-style dial like
the [Netty comparator](../java/README.md), where the JVM's RSS is dominated by
GC heap-commit rather than live memory. **The measurement refutes this for
.NET** (see [At-scale results](#at-scale-results)): 50K→250K drift is −1.4 %,
the marginal slope ≈ the absolute per-conn, and a Workstation-GC cross-check
(`DOTNET_gcServer=0`) lands within ~2 % of Server GC. All three show the
~53 KiB is **genuinely live per-connection memory** (SslStream buffers +
Kestrel pipe segments + WS state, none pooled), not committed-but-unused heap
— so the raw RSS-delta/N *is* the honest per-conn density here, the clean
mirror image of the JVM dial.

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

**Landed 2026-06-06** on a fresh 16-vCPU Graviton / 61 GB box, co-located
client+server over loopback, .NET 8.0.421 self-contained `linux-arm64` bundle,
Server GC (`ServerGarbageCollection=true`, the ASP.NET Core Web SDK default).

| scale | GC | established / failed | per-conn | connect p50 / p99 |
|---|---|---|---|---|
| **250K (headline)** | Server | 250,000 / 0 | **54,125 B (52.9 KiB)** | — |
| 50K (linearity) | Server | 50,000 / 0 | 54,869 B (53.6 KiB) | 4.6 / 15.0 ms |
| 50K (sidebar) | Workstation | 50,000 / 0 | 53,781 B (52.5 KiB) | 4.7 / 40.1 ms |

- **Linearity −1.4 %** (50K→250K, Server GC) — inside the 5 % gate, no 1M
  escalation. **Marginal slope ~52.7 KiB ≈ the absolute per-conn.**
- **The number is real, not a GC-heap dial — the opposite of the JVM.** The
  GC-heap-dial caveat below *anticipated* a JVM-style dial; the data refutes
  it. Slope ≈ absolute, −1.4 % drift, and a **~2 % Server↔Workstation GC
  delta** all show the ~53 KiB is genuinely live per-conn memory (`SslStream`
  buffers + Kestrel pipe segments + WS state, none pooled), not committed-but-
  unused heap. So the headline RSS-delta/N *is* the honest per-conn cost.
- **Headline: Kāra holds 4.47× the density** (12,114 B vs 54,125 B). .NET is
  the **second-heaviest comparator measured**, between Go (43.4 KiB) and
  Phoenix (102.8 KiB), above Rust (27.9) and Netty (14.4).
- Raw JSON: `docs/investigations/dotnet_linux_{250k,50k,50k_wks}.json`.

Full tables, head-to-head, and caveats: **§.NET Linux in `bench/REPORT.md`**
(the authoritative record).

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

> **GC-heap dial — anticipated, but it did NOT materialize (see results
> above).** The concern was that Server GC's lazy heap-commit would make
> RSS-delta/N a `-Xmx`-style dial as on the JVM. It isn't: 50K→250K drift is
> −1.4 % (well under 5 %), the marginal slope ≈ the absolute, and a
> Workstation-GC sidebar lands within ~2 % — so the per-conn RSS is genuinely
> live memory, and the raw RSS-delta/N *is* the per-conn density here. The
> Workstation-GC cross-check (`DOTNET_gcServer=0`) is the cheap way to confirm
> live-vs-slack; run it if a future re-measure shows drift creeping past 5 %.
> The small-N smoke (~87 KiB/conn at N=500) is CLR warm-up noise, not density.

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
- **Linux only** — the Windows/SChannel run (was #72) was cut by decision
  (2026-06-06); see §.NET Windows in `bench/REPORT.md`.
