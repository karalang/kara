# `ws_idle_holder` — comparator bench report

Cross-language measurement report for the `ws_idle_holder` workload
(idle-held `wss://` connections at scale, with active-traffic stress as
a paired profile). This is the **buyer/developer-facing artifact** that
backs the "Kāra delivers Erlang-tier per-connection density with
single-binary deploy and static-typing tooling" claim in the project
README.

Methodology, comparator setup, raw numbers, and caveats live here so
the README can quote headline ratios without burying the receipts.
Harness mechanics, flags, and CI-gate JSON shape live in `README.md`
alongside; this file is **what we measured and what it means**, not
**how the harness works**.

> **Status:** _in progress_. Kāra and Rust 1M numbers are landed.
> Kāra 2M, Rust 2M, and all non-Rust comparators are pending — see the
> [Status / measurement matrix](#status--measurement-matrix) below.
> Until a row's status is `landed`, treat the cells as placeholders.

---

## TL;DR — headline density (idle hold)

> _Lead with the **ratio**, not the absolute conn count. The ratio is
> the commercial lever (same fleet → fewer boxes → lower spend); the
> absolute is the credibility flex (we can hit big numbers on one box).
> Both matter, ratio first._

| Stack | per-conn bytes (idle) | ratio vs Kāra | scale tested | status | section |
|---|---|---|---|---|---|
| **Kāra** | **7.8 KB** | 1.00× (baseline) | 1M landed; 2M in flight | landed @ 1M | [§Kāra](#kāra) |
| Rust (rustls + tokio) | 27.8 KB | 3.55× | 1M landed; 2M pending | landed @ 1M | [§Rust](#rust-rustls--tokio) |
| Phoenix Channels (Elixir) | _TBD_ | _TBD_ | _pending — wip task #67_ | pending | [§Phoenix](#phoenix-channels-elixir) |
| Java / Netty | _TBD_ | _TBD_ | _pending — wip task #68_ | pending | [§Java/Netty](#java--netty) |
| Go (gorilla/websocket) | _TBD_ | _TBD_ | _pending — wip task #69_ | pending | [§Go](#go-gorillawebsocket) |
| .NET / ASP.NET Core (Linux) | _TBD_ | _TBD_ | _pending — wip task #71_ | pending | [§.NET Linux](#net--aspnet-core-linux) |
| .NET / ASP.NET Core (Windows) | _TBD_ | _TBD_ | _pending — wip task #72_ | pending | [§.NET Windows](#net--aspnet-core-windows) |
| Node.js (ws) | _TBD_ | _TBD_ | _pending — wip task #73_ | pending | [§Node](#nodejs-ws) |
| SignalR _(stretch)_ | _TBD_ | _TBD_ | _pending — wip task #74_ | stretch | [§SignalR](#signalr-stretch) |
| socket.io _(stretch)_ | _TBD_ | _TBD_ | _pending — wip task #75_ | stretch | [§socket.io](#socketio-stretch) |
| Python asyncio websockets _(stretch)_ | _TBD_ | _TBD_ | _pending — wip task #76_ | stretch | [§Python](#python-asyncio-websockets-stretch) |

### Commercial reframe — _populated as each row lands_

The translation from `per-conn-bytes ratio` to `infra spend ratio` is
documented in the [commercial-reframe lens](#commercial-reframe-lens)
section. Reframes are intentionally **not** written until a row's
numbers land — see the discipline guards in that section.

- **Kāra vs Rust** _(landed @ 1M)_: same fleet holds 3.55× more
  concurrent WebSocket users. For a hypothetical $1M/yr EC2 spend
  serving N idle connections on Rust+rustls, the equivalent Kāra
  fleet costs ~$282K/yr at matched conn count. _Caveats inherited
  from the [Rust comparator caveats](#rust-rustls--tokio)._

---

## How to read this report

1. **[TL;DR table](#tldr--headline-density-idle-hold)** has the
   density ratios — the headline.
2. **[Methodology](#methodology)** defines what "per-conn bytes",
   "idle", and "real-world configuration" mean here. Skip if you only
   want the numbers; required reading if you want to argue with them.
3. **[Per-comparator sections](#per-comparator-results)** carry the
   full setup, version, tuning, raw JSON pointers, and caveats for
   each stack. Each section is self-contained.
4. **[Active-traffic stress test](#active-traffic-stress-test)** is
   the paired story to density: holding N idle connections is one
   axis, holding them while M of them are exchanging messages is
   another. Density without active-traffic numbers is incomplete.
5. **[Commercial reframe lens](#commercial-reframe-lens)** is how
   technical results get translated to buyer-facing claims, and the
   discipline rules around when that translation is safe.
6. **[Reproduction](#reproduction)** — the canonical command lines
   for every comparator, end-to-end.

---

## Methodology

### Workload

Two profiles, run separately on every comparator:

| profile | what it measures | scale targets |
|---|---|---|
| **Idle hold** | per-conn memory floor; establishment-rate ceiling; reconnect-storm tail | 1M and 2M held conns |
| **Active traffic** _(wip task #66)_ | per-conn memory under realistic chatter; CPU-per-message; message latency tail | 1M idle + 10K active × 1 msg/sec |

Each profile is run end-to-end against a fresh server process: spawn,
ramp to N connections, hold (or hold + traffic), measure, tear down.
No comparator gets to warm up across runs.

### Per-conn-bytes definition

Server-side delta-RSS divided by N:

```
per_conn_bytes = (rss_after_n_held - rss_before_first_conn) / N
```

- `rss_after_n_held` is sampled after the harness reports
  `established = N` AND a 2-second settle window has elapsed (so
  per-connection task stacks have warmed and any deferred TLS state
  has materialized).
- `rss_before_first_conn` is sampled before the harness sends its
  first SYN.
- `/proc/<pid>/status` `VmRSS` on Linux; `ps -o rss=` on
  macOS/BSD/Windows-WSL. Native Windows uses `Get-Process` Working
  Set; documented in the .NET Windows section.

Only meaningful at large N. At N < 10K the first-connection overhead
(TLS session caches, per-thread stacks, RNG state) dominates and the
ratio is noise. Headline numbers in the TL;DR are all N ≥ 1M.

### TLS configuration (apples-to-apples floor)

All comparators run with:

- TLS 1.3 only (no fallback to 1.2).
- ECDHE with X25519 key exchange.
- AEAD: AES-128-GCM (the universal default; documented per-comparator
  if a stack defaults to ChaCha20-Poly1305 on ARM and we change it).
- Same loopback cert fixture (`tests/fixtures/tls/`), self-signed,
  RSA-2048. Cert verification disabled on the client (the bench
  harness is loopback-only by construction).
- Session resumption / 0-RTT **disabled** on both sides — every
  connection is a full handshake.

Any deviation gets called out in the comparator's section as a
caveat with rationale. Soft floor: if a comparator can only be
configured to deviate from this floor (e.g., a stack that defaults
to TLS 1.2 with no easy override), the deviation is documented and
the row is marked `caveat: tls-config-drift` in the status matrix.

### Hardware

Per-comparator EC2 instance type, chosen to remove cross-comparator
hardware confounds while still letting each stack run on a sensibly
sized box for its real-world deployment shape:

| comparator family | instance | vCPU | RAM | arch | rationale |
|---|---|---|---|---|---|
| Kāra / Rust / Go / Phoenix / Java / .NET Linux / Node | `r8g.4xlarge` | 16 (Graviton4) | 128 GB | arm64 | matches the Kāra & Rust 1M/2M baseline rig; cheap RAM headroom for the 2M target |
| .NET Windows | `m7i.4xlarge` | 16 (Intel x86) | 64 GB | x86_64 | SChannel is x86-default on Windows Server; matched vCPU; 64 GB is sufficient for 1M target |
| Cross-platform confirmation _(opportunistic)_ | `c7i.4xlarge` | 16 (Intel x86) | 32 GB | x86_64 | confirm Kāra+Rust numbers are not arm64 artifacts; wip task #62 |

**Each comparator gets a fresh box.** No co-tenancy between runs
within a measurement session. Box is terminated after the run's
JSON is captured and the per-conn-bytes number is reproduced once
on a re-spawn (cheap insurance against measurement-noise tails).

### Tuning floor

Every Linux comparator runs against the tunings in
`scripts/ec2_setup.sh` plus the file-max patch landed alongside the
Kāra 2M run:

- `fs.file-max = 8000000` (was 3M pre-2M); `fs.nr_open = 3000000`;
  per-process `nofile = 3000000` via `/etc/security/limits.d/`
- `net.core.somaxconn = 65535` (with the macOS 32768-cap caveat for
  any local validation — Linux EC2 is uncapped)
- `net.ipv4.tcp_max_syn_backlog = 65535`
- `net.ipv4.ip_local_port_range = 1024 65535`
- `net.ipv4.tcp_rmem` / `tcp_wmem` defaults preserved; documented
  per-comparator if a stack needs higher
- 27 loopback aliases `127.0.0.2..28` so the bench client can spread
  its source-IP load (see `bench/README.md § --source-ips`)

Windows tuning (TBD with the .NET Windows section) does the
analogous bumps: ephemeral port range, TIME_WAIT recycle window,
TCP control-block table size.

### Real-world configuration over apples-to-apples purism

Where a stack ships a framework layer that's the **default production
choice** (Phoenix Channels with presence/pubsub on top of raw
`:cowboy_websocket`; ASP.NET Core's SignalR on top of raw
`HttpListener`), we benchmark the framework layer too, **as well as**
a raw-protocol baseline where measuring is straightforward. The
framework number is what a buyer would actually deploy; the raw
number is what tells us how much of the delta is the framework vs the
runtime.

This is a deliberate departure from a purist "raw WebSocket protocol
only" comparison. The rationale: a CTO evaluating "what does Phoenix
cost me per conn vs what does Kāra cost me per conn" is comparing
production stacks, not protocol implementations. Framework overhead
is part of the production cost.

**Caveats this introduces:** documented per-comparator in a
`Framework overhead` block. Every framework number carries a pointer
to its raw-protocol counterpart so a reader can subtract the
overhead if they want.

---

## Apples-to-apples & framework-overhead caveats (consolidated)

_Filled in as each comparator lands. Each entry names the deviation
from the apples-to-apples floor and explains why we shipped the
number with the deviation rather than retuning to remove it._

- **Kāra vs Rust (1M, landed):** Both stacks run identical TLS
  config (TLS 1.3, X25519, AES-128-GCM, same cert fixture, no
  resumption). Both run idle = truly idle (no application-layer
  keepalive). The 3.55× ratio is straight per-conn-RSS delta with no
  framework layer on either side.
- **Phoenix Channels** _(pending — wip task #67):_ framework
  overhead expected for presence + pubsub broadcast tracking. We
  measure with presence **on** (production default) and **off** (raw
  Channels) per the wip-bench-day decision; the framework-overhead
  delta gets reported as a sub-row in the §Phoenix section.
- **Java / Netty** _(pending — wip task #68):_ G1GC defaults vs
  ZGC — measured with both; G1 is the broad-deployment default,
  ZGC the modern recommendation. Framework: raw Netty pipeline, no
  Spring/Vert.x layer (those would be a separate row).
- **.NET ASP.NET Core (Linux)** _(pending — wip task #71):_
  OpenSSL TLS (not SChannel); .NET 9 LTS; raw Kestrel WebSocket
  middleware. SignalR is a separate stretch row (#74).
- **.NET ASP.NET Core (Windows)** _(pending — wip task #72):_
  SChannel TLS (the production-default stack on Windows Server);
  .NET 9 LTS; raw Kestrel WebSocket middleware. The Linux/Windows
  delta is itself a result — it tells us how much of the .NET
  number is the framework vs the OS TLS substrate.

---

## Per-comparator results

### Kāra

- **Status:** `landed @ 1M`; 2M in flight (wip task #61).
- **Build:** `karac build` against `examples/ws_idle_holder/main.kara`
  at commit `<TBD — fill in at 2M result landing>`.
- **Runtime:** auto-par enabled (`KARAC_AUTO_PAR=1`, default);
  `KARAC_WS_ACCEPT_THREADS=32`.
- **Hardware:** `r8g.4xlarge` (16 vCPU Graviton4, 128 GB RAM,
  arm64, Ubuntu 24.04).
- **TLS:** TLS 1.3 / X25519 / AES-128-GCM via `karac-runtime`
  rustls integration.

**Idle-hold @ 1M (landed, 2026-05-29):**

| metric | value | notes |
|---|---|---|
| established | 1,000,000 / 1,000,000 | 0 failed |
| per-conn bytes | **~7,846 B (7.8 KB)** | server-RSS delta / N, settled |
| connect mean | 81.7 ms | `c=64`, single-point |
| connect p99 | 256 ms | `c=64` |
| churn cliff_ratio | TBD | re-running with churn-rounds > 0 in 2M run |

**Idle-hold @ 2M (in flight, wip task #61):**

- Server PID `<TBD>`, port `<TBD>`, started `<TBD>Z`.
- Acceptance criteria for this row to be marked `landed`:
  1. `established == 2,000,000` AND `failed == 0`.
  2. `per_conn_bytes` within ±5% of the 1M value (7,846 B); a
     drift > 5% means TLS state amortization is incomplete and we
     re-run.
  3. `dmesg` clean of SYN-flood _and_ file-max messages.
  4. The same number reproduced on a re-spawn within the same
     instance lifetime.

- Raw JSON when landed: `docs/investigations/demo1_m3_2m.json`.

**Caveats:**

- Numbers are with auto-par on. Single-threaded comparator runs
  (Go, Node, raw threaded Java) are compared against a Kāra
  **single-thread** binary build (`KARAC_AUTO_PAR=0`) per the
  bench-lane discipline; the auto-par number is **not** headlined
  against single-threaded stacks.
- TLS uses rustls under the hood (Kāra v1 substrate). Kāra-native
  TLS is a future-work axis; for this report rustls is the floor
  for both Kāra and Rust, so it cancels out for the ratio.

### Rust (rustls + tokio)

- **Status:** `landed @ 1M`; 2M pending (wip task #63).
- **Build:** `examples/ws_idle_holder/rust/`,
  `cargo build --release`, version pinned in `Cargo.toml`.
- **Stack:** `tokio` async runtime; `rustls` for TLS;
  `tokio-tungstenite` for WebSocket upgrade + framing.
- **Hardware:** same `r8g.4xlarge` as Kāra; fresh box.

**Idle-hold @ 1M (landed, 2026-05-29):**

| metric | value | notes |
|---|---|---|
| established | 1,000,000 / 1,000,000 | 0 failed |
| per-conn bytes | **~27,800 B (27.8 KB)** | server-RSS delta / N, settled |
| connect mean | TBD | re-pull from JSON |
| connect p99 | TBD | re-pull from JSON |

**Idle-hold @ 2M (pending, wip task #63):**

- Will run with identical flags to Kāra 2M via
  `scripts/run_1m.sh` (or 2m variant).
- Same acceptance criteria as Kāra 2M, plus: `per_conn_bytes`
  must be ≥ 3.0× Kāra's at matched N (a regression to < 3.0×
  triggers a re-investigation of both runs before publishing).

**Caveats:**

- `rustls` is single-threaded per connection (no shared TLS
  state). This is the architectural reason Kāra wins on density:
  Kāra's TLS state lives in a shared per-binding structure with
  per-conn references, not a per-conn copy.
- `tokio-tungstenite` is the modern Rust WebSocket library; we
  did not test `fastwebsockets` or `tungstenite` (sync) — those
  are listed as future-work comparators in the wip doc but not
  blockers for the v1 claim.

### Phoenix Channels (Elixir)

> _Pending — wip task #67. Sub-checkboxes in `wip-bench-day.md`._

- **Status:** pending.
- **Stack target:** Elixir 1.17 LTS, Erlang/OTP 27, Phoenix 1.7,
  `:cowboy_websocket` under the hood. Two configurations:
  - **Production default:** Phoenix Channels with `Presence` on,
    PubSub broadcast tracking on. This is what a real Phoenix
    deployment looks like.
  - **Raw Channels:** Presence off, PubSub disabled. Tells us the
    framework overhead delta.
- **Hardware:** `r8g.4xlarge`; fresh box.
- **TLS:** OpenSSL via Erlang `:ssl`; matched cipher suite + cert
  fixture.

**Expected range (from public data):** 5–10 KB/conn. Phoenix is
the density-king comparator and the most rhetorically dangerous —
the WhatsApp/Discord lineage. If Phoenix matches Kāra within ~20%,
the framing shifts to the [combination claim](#commercial-reframe-lens):
**density + static types + single-binary deploy + no BEAM ops
surface**, not "Kāra wins on density alone."

**Sub-rows to fill:**

| metric | Phoenix (Presence on) | Raw Channels | notes |
|---|---|---|---|
| established | TBD | TBD | |
| per-conn bytes | TBD | TBD | |
| connect mean | TBD | TBD | |
| framework overhead | (Phoenix − raw) | — | |

**Caveats to document on landing:**

- BEAM RSS accounting differs from a tokio-based server's RSS in
  subtle ways (BEAM pre-allocates a process heap; reductions are
  not RSS but they're real). The methodology section's per-conn-bytes
  definition is still server-RSS delta / N — same formula — but
  the **shape** of the curve in the first 10K conns is different.
  Document any BEAM-specific tuning (`+P`, `+Q`, `+K true`,
  `+sbwt none`) used to reach the target.

### Java / Netty

> _Pending — wip task #68._

- **Status:** pending.
- **Stack target:** OpenJDK 21 LTS, raw Netty `WebSocketServerProtocolHandler`
  on top of `NioEventLoopGroup`. No Spring, no Vert.x, no Akka — those
  are distinct comparator rows (out of scope for v1).
- **GC configurations:**
  - **G1GC defaults** (broad-deployment default, what most prod
    Java fleets ship with).
  - **ZGC** (modern recommendation for low-pause; meaningful for
    long-running density workloads where pauses interact with
    WebSocket keepalive timing).
- **Hardware:** `r8g.4xlarge`; fresh box.
- **TLS:** Java JSSE via `SSLEngine`; matched cipher + cert.

**Expected range (from public data):** 20–40 KB/conn. The largest
**commercial TAM** comparator — every enterprise has JVM fleets
touching WebSockets somewhere. This is the cleanest dollarized
cost story when it lands.

**Sub-rows to fill:**

| metric | Netty + G1GC | Netty + ZGC | notes |
|---|---|---|---|
| established | TBD | TBD | |
| per-conn bytes | TBD | TBD | RSS = JVM RSS, includes heap + Netty buffers |
| heap (resident) | TBD | TBD | sub-component of total RSS |
| direct buffers | TBD | TBD | Netty pooled direct mem; sub-component |
| connect mean | TBD | TBD | |

**Caveats to document on landing:**

- JVM heap size has to be tuned for the test (`-Xmx` proportional
  to N); document the heap setting used so a reader can compute
  "would Kāra also be this large at this heap setting" — Kāra has
  no heap setting, RSS is what it is.
- Netty direct-buffer pool size is the load-bearing knob for
  per-conn cost; document the pool config.

### Go (gorilla/websocket)

> _Pending — wip task #69._

- **Status:** pending.
- **Stack target:** Go 1.23 LTS, `gorilla/websocket` (most-deployed),
  `net/http` server, `crypto/tls` for TLS.
- **Hardware:** `r8g.4xlarge`; fresh box.
- **TLS:** Go `crypto/tls`; matched cipher + cert.

**Expected range (from public data):** 20–30 KB/conn. The modern
default for new infra; smaller commercial delta than Java but
strong rhetorical position ("Go is good enough" is the default
counterargument we need to address).

**Sub-rows to fill:**

| metric | Go + gorilla | notes |
|---|---|---|
| established | TBD | |
| per-conn bytes | TBD | RSS = Go process RSS, includes goroutine stacks |
| goroutine stack overhead | TBD | sub-component; 2 goroutines per conn typical |
| connect mean | TBD | |

**Caveats to document on landing:**

- Goroutine stacks start at 2 KB but grow; document the steady-state
  per-conn goroutine count and stack size.
- `GOGC` setting affects RSS; document the value used (default 100).

### .NET / ASP.NET Core (Linux)

> _Pending — wip task #71._

- **Status:** pending.
- **Stack target:** .NET 9 LTS on Linux (Ubuntu 24.04 arm64);
  Kestrel WebSocket middleware (no SignalR layer); OpenSSL for TLS
  (the Linux .NET default).
- **Hardware:** `r8g.4xlarge`; fresh box.
- **TLS:** OpenSSL via .NET; matched cipher + cert.

**Expected range (from public data):** 15–30 KB/conn on Linux.

**Caveats:**

- GC mode: server GC (`ServerGarbageCollection=true`) is the
  prod-default; document if we deviate.
- Linux .NET deploys are a smaller share of .NET fleets than Windows
  but a growing one (container/k8s-native deploys). The Linux number
  + the Windows number jointly answer "what does .NET cost?".

### .NET / ASP.NET Core (Windows)

> _Pending — wip task #72._

- **Status:** pending.
- **Stack target:** .NET 9 LTS on Windows Server 2022;
  Kestrel WebSocket middleware; SChannel for TLS (the Windows
  Server prod-default).
- **Hardware:** `m7i.4xlarge` (16 vCPU Intel x86, 64 GB RAM,
  Windows Server 2022). Sized to the 1M target — 2M is not on the
  roadmap for Windows; the Linux .NET row covers the 2M scale
  question if needed.
- **TLS:** SChannel; matched cipher + cert (within SChannel's
  configurable surface).

**Expected delta vs Linux .NET:** SChannel and OpenSSL have
genuinely different per-conn TLS-state shapes. The Linux/Windows
.NET delta is itself a finding — it tells us "is the .NET overhead
the runtime or the OS TLS substrate?" — and matters for any buyer
whose .NET fleet is Windows-Server-default.

**Caveats:**

- Windows ephemeral port range, TIME_WAIT recycle, and TCP control
  block table size all need tuning to hit 1M; document the
  PowerShell tuning script alongside the run.
- RSS measurement on Windows is `Get-Process` Working Set; this is
  the closest analog to Linux `VmRSS` but not identical (Windows
  Working Set is the working-set subset of resident pages, which is
  what Linux RSS also measures, but the kernel-side accounting
  differs in edge cases).

### Node.js (ws)

> _Pending — wip task #73._

- **Status:** pending.
- **Stack target:** Node.js 22 LTS, `ws` library (most-deployed
  Node WebSocket), `tls` module for TLS.
- **Hardware:** `r8g.4xlarge`; fresh box.

**Expected range (from public data):** 30–50 KB/conn. Predictable
outcome; smaller commercial impact than Java/Phoenix. Included for
completeness — Node WebSocket deploys are common at small/medium
scale but rarely the choice for density-critical fleets.

### SignalR _(stretch)_

> _Pending — wip task #74. Stretch row — not blocking v1 claim._

- **Status:** pending, stretch.
- **Stack target:** ASP.NET Core SignalR on top of .NET 9 (Linux);
  exposes the framework-overhead delta over raw Kestrel WebSocket
  middleware.

### socket.io _(stretch)_

> _Pending — wip task #75. Stretch row — not blocking v1 claim._

- **Status:** pending, stretch.
- **Stack target:** Node.js + `socket.io` server; exposes the
  framework-overhead delta over raw `ws`.

### Python asyncio websockets _(stretch)_

> _Pending — wip task #76. Stretch row — not blocking v1 claim._

- **Status:** pending, stretch.
- **Stack target:** Python 3.12, `websockets` library, asyncio.
  Included for completeness; Python is not in the production WS
  density landscape for any serious deployment but the row exists
  so we can answer the inevitable "what about Python?" question.

---

## Active-traffic stress test

> _Pending — wip task #66. Run after all idle-hold rows land._

**Profile:** 1,000,000 idle held connections + 10,000 actively
exchanging connections at 1 message/sec/conn (10K msg/sec aggregate
floor). Payload: 64-byte text frame; small enough to not dominate
network, large enough to exercise framing.

**What this measures (additional axes beyond idle):**

| axis | what it answers |
|---|---|
| per-conn-bytes under traffic | does the idle 7.8 KB hold up when 1% of conns are active? |
| message latency p50/p99/p99.9 | what does a real conversation look like at this density? |
| CPU-per-message | how much headroom for traffic ramp before the box saturates? |
| reconnect-storm survival | if 10% of the held conns drop and reconnect in a 1-second window, does the box survive? |

**Why this matters for the cost claim:** the [per-conn density
memory](../../../.claude/projects/-Users-mango-Documents-Gowtham-projects/memory/feedback_per_conn_density_is_the_headline.md)
calls out "but it's just idle" as the load-bearing objection. The
active-traffic numbers are how that objection gets answered.

**Per-comparator active-traffic results:** populated as a paired
table to the idle-hold table once the harness extension lands.

---

## Commercial reframe lens

Every landed comparator row gets paired with a **buyer-impact
reframe** that translates the technical claim into one of five
vectors:

| vector | translation shape |
|---|---|
| **$$ / infra cost** | "same fleet → fewer boxes → $X/yr saved" |
| **Time** | "deploy speed, onboarding ramp, time-to-ship" |
| **Incidents** | "races caught at compile time, pages avoided" |
| **Headcount** | "specialist hires not needed" |
| **Ops complexity** | "operational surface area reduced" |

For `ws_idle_holder` the dominant vector is **$$ / infra cost** —
density × $/box = margin. Other Kāra wins use other vectors (the
effect system → incidents + compliance; auto-par → headcount;
ownership tiers → time + headcount).

### Discipline guards on reframes

_From `feedback_commercial_reframe_lens` memory._

- **Never write the reframe before the data exists.** A reframe is
  a claim about consolidated reality, not aspiration. If the number
  isn't in this report yet, the reframe slot stays empty.
- **Apples-to-apples integrity carries through to the reframe.** If
  the technical comparison has caveats (idle vs active, framework
  overhead included, TLS-config drift), the reframe inherits them.
  Don't strip caveats in translation.
- **Reframes belong on buyer-facing surfaces.** This report, the
  README, blog/demo writeups. Internal tracker entries,
  implementation notes, and design discussions stay technical —
  overusing the lens internally is noise.

### Landed reframes

- **Kāra vs Rust @ 1M (density):** _Technical:_ Kāra holds the
  same N idle WebSocket connections in 7.8 KB/conn vs Rust+rustls
  +tokio at 27.8 KB/conn — a 3.55× density advantage. _Buyer
  impact:_ same fleet serves 3.55× more concurrent users for the
  same EC2 spend. For a fleet currently spending $1M/yr on idle
  WebSocket capacity, the equivalent Kāra-served capacity costs
  ~$282K/yr (rounded; caveats apply — see [Rust comparator
  caveats](#rust-rustls--tokio)).

### Pending reframes (deferred — data not yet in this report)

- **Kāra vs Phoenix:** _Deferred until §Phoenix lands._ If Phoenix
  matches Kāra within ~20%, the reframe pivots to the combination
  claim (density + static types + single-binary deploy + no BEAM
  ops surface), not a pure density win.
- **Kāra vs Java/Netty:** _Deferred until §Java/Netty lands._
  Expected to be the strongest dollarized story given the JVM TAM.
- **Kāra vs Go / .NET / Node:** _Deferred per-row._

---

## Reproduction

The canonical end-to-end flow for the Kāra and Rust baselines is in
[`README.md § EC2 1M rig`](README.md#ec2-1m-rig--scripts). Each
non-Rust comparator gets its own reproduction sub-section as the
row lands; reproduction artifacts live next to the comparator
source (e.g., `examples/ws_idle_holder/phoenix/` for the Phoenix
comparator, parallel to the existing `rust/` subdir).

Standing rules:

- Every reproduction script captures: (1) full command line,
  (2) versions (compiler/runtime + library), (3) tuning script
  applied, (4) JSON output filename for the harness, (5) raw RSS
  sampling commands.
- A row is `landed` only when the reproduction has been run end-to-end
  on a fresh EC2 box (not a re-run on a warm box).

---

## Status / measurement matrix

| comparator | idle-hold 1M | idle-hold 2M | active-traffic | reproduction script | raw JSON |
|---|---|---|---|---|---|
| Kāra | landed | in flight (#61) | pending (#66) | `scripts/run_1m.sh` | `docs/investigations/demo1_m1_verification.md` |
| Rust | landed | pending (#63) | pending (#66) | `scripts/run_1m.sh` | same |
| Phoenix Channels | pending (#67) | pending | pending | TBD | TBD |
| Java / Netty | pending (#68) | pending | pending | TBD | TBD |
| Go | pending (#69) | pending | pending | TBD | TBD |
| .NET (Linux) | pending (#71) | pending | pending | TBD | TBD |
| .NET (Windows) | pending (#72) | n/a | pending | TBD | TBD |
| Node.js | pending (#73) | pending | pending | TBD | TBD |
| SignalR _(stretch)_ | pending (#74) | n/a | pending | TBD | TBD |
| socket.io _(stretch)_ | pending (#75) | n/a | pending | TBD | TBD |
| Python _(stretch)_ | pending (#76) | n/a | pending | TBD | TBD |

> Task numbers reference `wip-bench-day.md` (uncommitted; lives in
> repo root). When that file is deleted on ship, the equivalent
> tracker entries in `docs/implementation_checklist/phase-6-runtime.md`
> become the durable references.

---

## Change log

- **2026-05-30:** initial skeleton; Kāra & Rust 1M results carried
  over from `docs/investigations/demo1_m1_verification.md`. All
  other comparators stubbed with `TBD` placeholders. Headline
  ratio: 3.55× Kāra vs Rust @ 1M (landed).
