# `ws_idle_holder/java` — Netty reference impl (comparator)

A raw **Netty** WebSocket-over-TLS server that mirrors the `ws_idle_holder`
flagship demo: it holds N idle connections so the shared bench harness can
measure per-connection memory (density), connect latency, and churn against
Kāra. Commercial-tier comparator **#68**.

## Why a Java comparator

Java has the **largest commercial TAM** of any comparator — every
enterprise runs JVM fleets, and a lot of them touch WebSockets somewhere.
Raw Netty is the high-density Java WebSocket prod default (LinkedIn-tier
shops deploy it directly), so it is the fairest "what a real Java shop
ships" baseline for the cost claim.

## Real-world-vs-purist caveat

Per the bench-day discipline (`apples-to-apples = mimic real-world prod
config`), this is **raw Netty** — `WebSocketServerProtocolHandler` on a
plain `NioEventLoopGroup` pipeline — **not** Spring / Vert.x / Akka, which
fold in router/DI/actor overhead and are distinct framework-tier
comparators (out of scope for v1). No framework overhead is folded into the
per-conn number; this is the lean Java prod default.

## Design choices

- **Raw Netty pipeline:** `SslHandler → HttpServerCodec →
  HttpObjectAggregator → WebSocketServerProtocolHandler("/") → echo`. The
  idiomatic minimal high-density Netty WS stack.
- **WS upgrade at `/`**, matching the bare-WS comparators (Kāra/Rust/Go) —
  the harness sends `GET / HTTP/1.1` with the RFC 6455 Upgrade headers, so
  **no harness changes** are needed (unlike Phoenix's Channels join).
- **TLS:** JDK JSSE `SSLEngine` (`SslProvider.JDK`), TLS 1.2 + 1.3, no
  client auth, single self-signed cert — the same `tests/fixtures/tls`
  fixture (CN=localhost), bundled into the jar as a classpath resource so
  this dir is self-contained when scp'd to a rig. See "TLS provider".
- **GC = G1 (JDK 21 default):** the broad-deployment default per the
  apples-to-apples discipline (no aggressive ZGC + specialist tuning). ZGC
  is run as an optional sidebar via `JAVA_OPTS` (pure runtime flag, no
  rebuild) to quantify the GC-config delta.
- **Ephemeral port → `BOUND_PORT`:** binds `127.0.0.1:0`, reads the actual
  port from the bound channel, prints `BOUND_PORT=<n>` on stdout for the
  harness's `--server-bin` contract.
- **`run_server.sh` execs the JVM** in place, so the harness-spawned PID is
  the `java` PID — `ps -o rss=` measures the JVM (heap + Netty direct
  buffers + metaspace + thread stacks) directly.
- **Listen backlog `SO_BACKLOG=65535`** matches the Rust comparator's
  explicit `listen(65535)`; the kernel clamps to `net.core.somaxconn`
  (65535 on the rig). `TCP_NODELAY` on (matches the others).
- **Heap/direct sizing (`-Xms256m -Xmx24g -XX:MaxDirectMemorySize=48g`):**
  box-sizing, *not* GC tuning. `-Xms` stays small so committed heap (and
  thus RSS) tracks the actual live set; the ceilings only prevent an
  artificial OOM at 250K. The per-conn-bytes metric (RSS-delta / N)
  subtracts the JVM baseline measured *before* any connection, so the
  ceilings do not inflate the density figure.

### TLS provider (JDK JSSE vs OpenSSL/tcnative)

This comparator uses the **JDK JSSE `SSLEngine`** — the zero-native-
dependency default, what a vanilla Netty deployment gets out of the box.
High-throughput Netty fleets sometimes add `netty-tcnative` (OpenSSL) for
faster handshakes and lower per-connection TLS memory; that is a non-
default opt-in and is noted as a caveat, not measured here. JDK JSSE is the
honest "default Java shop" basis and keeps the build dependency-free.

## Usage

```bash
# Build the fat jar (run inside this dir):
mvn -q package           # -> target/ws-idle-holder-netty.jar

# Bench it through the shared harness (G1, the default):
cd ../bench
cargo run --release -- \
  --server-bin ../java/run_server.sh \
  -n 500 --concurrency 64 --churn-rounds 0 --hold-secs 3

# ZGC sidebar: prefix with JAVA_OPTS (no rebuild):
JAVA_OPTS="-XX:+UseZGC -XX:+ZGenerational" cargo run --release -- \
  --server-bin ../java/run_server.sh -n 500 ...
```

> `java` must be on PATH for `run_server.sh` (the harness inherits the
> caller's environment and spawns the launcher). JDK 21 LTS.

## At-scale results

**NOT YET RUN.** Prepped + locally validated; the 50K / 250K (G1, and ZGC
sidebar) rig runs await a user-provisioned box (per the bench-day rig-spend
sign-off discipline). Expected ~20–40 KB/conn. Results table lands here
after the rig run, mirroring the Go/Phoenix comparators' format.

### Reproduce — turnkey rig recipe

```bash
# 0. Kernel + nofile + loopback-alias setup (idempotent; fresh login after
#    so the systemd nofile cap lifts). The JVM inherits the 3M nofile cap
#    for its server-side fds.
examples/ws_idle_holder/bench/scripts/ec2_setup.sh
# re-login here

# 1. Toolchains. ec2_setup.sh installs no compilers. Java needs a JDK 21 +
#    Maven; the harness needs cargo.
sudo apt-get update && sudo apt-get install -y openjdk-21-jdk maven   # or sdkman
# rustup for the harness if absent.

# 2. Build the comparator jar + the bench harness on-box:
( cd examples/ws_idle_holder/java && mvn -q package )
( cd examples/ws_idle_holder/bench && cargo build --release )

# 3. Run both scales (JSON tee'd to ./<basename>-{250k,50k}.json). The
#    bare-WS comparators need no BENCH_EXTRA_ARGS (default path "/").
cd examples/ws_idle_holder/bench/scripts
JAVA="$(cd ../../java && pwd)/run_server.sh"

# Headline (G1, the default GC):
./run_250k.sh "$JAVA" netty_g1_250k.json
./run_50k.sh  "$JAVA" netty_g1_50k.json
# GC sidebar (generational ZGC):
JAVA_OPTS="-XX:+UseZGC -XX:+ZGenerational" ./run_250k.sh "$JAVA" netty_zgc_250k.json

# 4. scp the JSONs off-box to docs/investigations/ and `git add` them
#    BEFORE terminating the instance (scp != tracked).
```

> **Linearity gate:** if 50K→250K `per_conn_bytes` drift exceeds 5%,
> escalate to a 1M run (`run_1m.sh`). JVM heap warm-up is a known
> non-linearity source, so the linearity check is load-bearing here (small-N
> per-conn is dominated by JVM baseline + JIT — the local N=500 smoke read
> ~1.1 MiB/conn, pure warm-up noise; do **not** read small-N as density).

## Local validation (macOS, OpenJDK 21.0.11 / Netty 4.1.115)

- `mvn package` clean → `target/ws-idle-holder-netty.jar` (fat jar, cert
  bundled).
- `run_server.sh` boots, prints `BOUND_PORT`.
- Harness via `--server-bin`, N=500: **500/500 established, 0 failed**;
  active echo **150 sent / 150 echoed / 0 failures** (TLS WS upgrade at `/`,
  idle hold, and echo all confirmed end-to-end).

## What this impl deliberately omits

- **No framework** (Spring/Vert.x/Akka) — raw Netty only; those are
  separate framework-tier comparators.
- **No OpenSSL/tcnative** — JDK JSSE only (see TLS provider).
- **No specialist GC tuning** — G1 default + an optional ZGC sidebar, both
  prod-realistic; no hand-tuned heap regions / pause targets.
- **No clustering / no app logic** — just the WS-over-TLS surface the
  density measurement needs.
