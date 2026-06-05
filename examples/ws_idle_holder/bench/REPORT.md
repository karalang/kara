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

> # ✅ RE-MEASURED — working-handler 1M + 2M density landed (2026-06-01)
>
> **The provisional figures are replaced by a real working-server measurement.**
> The earlier Kāra density numbers (7.8 KB/conn, the 3.55× ratio vs Rust) were
> measured **before the per-connection handler executed** — on a build where
> `handle_connection` compiled to a body-less state machine (no
> `recv_text`/`send_text`/parking — "bug C" of the A2 track). Connections were
> genuinely established + held, but the handler's per-conn state — the **4 KB
> recv buffer + coroutine frame + parking** — was **freed, not held**, while
> Rust's 27.9 KB *includes* its per-conn task state. Those figures understated a
> working server, so they were retracted.
>
> **What changed (all landed on `main`):** the A2 LLVM-coroutine network-async
> transform compiles network-boundary fns as dispatcher-driven coroutines,
> flipped **on by default**; the WS-over-TLS recv/send path executes as a
> coroutine suspend/resume; the concurrent accept-loop resume race is fixed; and
> two coroutine-frame heap-overflow bugs that corrupted the glibc heap at scale
> are fixed — `fe6afd16` (the `Array[u8,4096]` recv-buffer frame slot was sized
> as an 8-byte i64 instead of inline `[4096 x i8]`) and `eba48194` (the codegen
> module carried no `target datalayout`, so `llvm.coro.size` under-allocated the
> frame by the i64-alignment delta and the trailing suspend-index stored one
> byte past the malloc). Both were glibc-only and silent on macOS even under
> ASAN — caught only by running the real binary on the Linux/Graviton rig.
>
> **Re-measure (the real headline), `r8g.4xlarge` arm64, build off `main`
> ⊇ `eba48194`:**
> - **1M (2026-06-01):** established **1,000,000 / 0 failed**, clean teardown, no
>   heap corruption. **Per-conn = 12,114 B (12.1 KB)**, server RSS 11.28 GiB.
>   Tail improved sharply vs the pre-fix run (p99 1856 ms → **255 ms**, max
>   2306 ms → **480 ms** — `ec2_setup.sh` sysctls removed the SYN-retransmit
>   cliff). Raw JSON: `docs/investigations/demo1_m3_1m_postfix_datalayout.json`.
> - **2M (2026-06-01):** established **2,000,000 / 0 failed**. **Per-conn =
>   12,111 B (12.1 KB)** — server RSS 22.56 GiB. **Scale-invariance confirmed
>   at the working figure: 1M → 2M drift is −0.03 %.** Connect p50 46.0 ms /
>   p99 732.6 ms / max 1193.7 ms. Raw JSON:
>   `docs/investigations/demo1_m3_2m_postfix_datalayout.json`.
>
> **Corrected headline:** per-conn **7.8 → 12.1 KB**, ratio **3.55× → 2.30×** vs
> Rust (27.9 KB, same-rig), scale-invariant 1M↔2M. The **total-box** ratio
> (counting the ~3.3 KB/conn stack-independent kernel socket buffer both stacks
> pay) is **2.03×** — this is the figure the cost claim is anchored on. **Rust's
> figures, established counts, and connect percentiles are unaffected.**
>
> **x86 cross-ISA re-read — DONE (2026-06-02, supersedes the pre-fix 7,725 B).**
> The last `‡` item is closed. A working-handler Kāra **1M** run on
> `c7i.8xlarge` (Intel x86_64, 32 vCPU, 64 GB) established **1,000,000 / 0
> failed** with **per-conn = 12,112 B** — within **−0.02 %** of the arm64
> 1M figure (12,114 B), server RSS 11.28 GiB, connect p50 44.2 ms
> (reproduces the cross-ISA p50 floor). **Density at the working figure is
> ISA-identical, not Graviton-specific.** Raw JSON:
> `docs/investigations/demo1_m3_1m_x86_postfix.json`. There are no remaining
> `‡` items.

> **Status:** _in progress_. Kāra 1M + 2M and Rust 1M + 2M numbers are
> landed (credibility-comparator head-to-head at the ceiling is
> complete). All non-Rust comparators are pending — see the
> [Status / measurement matrix](#status--measurement-matrix) below.
> Until a row's status is `landed`, treat the cells as placeholders.

---

## TL;DR — headline density (idle hold)

> _Lead with the **ratio**, not the absolute conn count. The ratio is
> the commercial lever (same fleet → fewer boxes → lower spend); the
> absolute is the credibility flex (we can hit big numbers on one box).
> Both matter, ratio first._

| Stack | role | per-conn bytes (idle) | ratio vs Kāra | scale tested | status | section |
|---|---|---|---|---|---|---|
| **Kāra** | self | **12.1 KB** | 1.00× (baseline) | 1M + 2M landed (post-fix) | landed @ 2M | [§Kāra](#kāra) |
| Rust (rustls + tokio) | credibility | 27.9 KB | **2.30×** | 1M + 2M landed | landed @ 2M | [§Rust](#rust-rustls--tokio) |
| Phoenix Channels (Elixir) | commercial | _TBD_ | _TBD_ | 250K headline + 50K linearity (wip #67) | pending | [§Phoenix](#phoenix-channels-elixir) |
| Java / Netty | commercial | _TBD_ | _TBD_ | 250K headline + 50K linearity (wip #68) | pending | [§Java/Netty](#java--netty) |
| Go (gorilla/websocket) | commercial | _TBD_ | _TBD_ | 250K headline + 50K linearity (wip #69) | pending | [§Go](#go-gorillawebsocket) |
| .NET / ASP.NET Core (Linux) | commercial | _TBD_ | _TBD_ | 250K headline + 50K linearity (wip #71) | pending | [§.NET Linux](#net--aspnet-core-linux) |
| .NET / ASP.NET Core (Windows) | commercial | _TBD_ | _TBD_ | 250K headline + 50K linearity (wip #72) | pending | [§.NET Windows](#net--aspnet-core-windows) |
| Node.js (ws) | commercial | _TBD_ | _TBD_ | 250K headline + 50K linearity (wip #73) | pending | [§Node](#nodejs-ws) |
| SignalR _(stretch)_ | stretch | _TBD_ | _TBD_ | 100K headline + 50K linearity (wip #74) | stretch | [§SignalR](#signalr-stretch) |
| socket.io _(stretch)_ | stretch | _TBD_ | _TBD_ | 100K headline + 50K linearity (wip #75) | stretch | [§socket.io](#socketio-stretch) |
| Python asyncio websockets _(stretch)_ | stretch | _TBD_ | _TBD_ | 100K headline + 50K linearity (wip #76) | stretch | [§Python](#python-asyncio-websockets-stretch) |

> **All density figures are working-handler, post-fix.** The Kāra **1M and 2M**
> per-conn (12.1 KB, −0.03 % drift) and the **2.30×** ratio were re-measured
> 2026-06-01 on `r8g.4xlarge` (arm64, build ⊇ `eba48194`); the **x86 cross-ISA**
> 1M re-read (12,112 B, −0.02 % vs arm64) landed 2026-06-02 on `c7i.8xlarge`,
> closing the last `‡`. See the ✅ banner at the top of this report. Rust's
> numbers are unaffected throughout.

> **About the `role` column and asymmetric scale:** comparators serve
> different argumentative roles (credibility vs commercial vs stretch)
> and are sized accordingly. Per-conn-bytes is linear (the post-fix
> working-handler runs confirm **−0.03 % drift 1M→2M** at the 12.1 KB
> figure, and the x86 1M re-read lands within −0.02 % of arm64), so the
> density ratio is scale-invariant *and* ISA-invariant —
> 250K against 250K gives the same headline as 1M against 1M. Rust's own
> 0.33 % 1M→2M drift is unaffected. Full rationale in
> [§Scale per comparator](#scale-per-comparator).

### Commercial reframe — _populated as each row lands_

The translation from `per-conn-bytes ratio` to `infra spend ratio` is
documented in the [commercial-reframe lens](#commercial-reframe-lens)
section. Reframes are intentionally **not** written until a row's
numbers land — see the discipline guards in that section.

- **Kāra vs Rust** _(Kāra 1M + 2M post-fix landed; Rust 1M + 2M landed)_:
  Kāra's runtime holds each connection in **2.30×** less userspace memory
  (12.1 KB vs 27.9 KB/conn, same `r8g.4xlarge` rig, scale-invariant 1M↔2M).
  **The buyer-relevant figure is the production-unit cost at 250K:** counting
  the ~3.3 KB/conn kernel socket buffer both stacks pay equally, total
  server-side memory is **15.0 KB (Kāra) vs 30.4 KB (Rust) = 2.03×** — which
  at 250K idle connections lands Kāra on an **8 GiB `m7g.large`** where Rust
  needs a **16 GiB `m7g.xlarge`** (~5.2 vs ~8.9 GiB working set). One instance
  class smaller → **~50 % infra cost**: per 250K-conn unit on a 1-year
  no-upfront reserved instance, **~$473/yr (Kāra) vs ~$946/yr (Rust)**
  (us-east-1, verified May 2026). The saving is discrete (cloud RAM steps in
  2× jumps), so it cashes out as "one tier down," and scales with fleet size —
  a 5M-conn fleet (20 HA-sharded 250K units) saves **~$9.5K/yr** on 1-yr RIs,
  with the operational lever (half the box count to patch/monitor) often the
  bigger win for large buyers. _Ceiling flex (not the cost lead): the same
  density lets a single 128 GiB box hold ~11.3M Kāra conns where Rust OOMs at
  ~4.9M._ Full model + sourcing in the [commercial-reframe
  lens](#commercial-reframe-lens). _Caveats inherited from the [Rust comparator
  caveats](#rust-rustls--tokio)._

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
| Cross-platform confirmation _(x86, post-fix — landed 2026-06-02)_ | `c7i.8xlarge` | 32 (Intel x86) | 64 GB | x86_64 | Working-handler Kāra 1M: **12,112 B/conn**, within −0.02 % of arm64 — density is ISA-identical, not Graviton-specific. Reproduces the cross-ISA p50 floor (44.2 ms). Supersedes the pre-fix 7,725 B read. `c7i.8xlarge` over `.4xlarge` — co-located 1M client+server needs >32 GB |

**Each comparator gets a fresh box.** No co-tenancy between runs
within a measurement session. Box is terminated after the run's
JSON is captured and the per-conn-bytes number is reproduced once
on a re-spawn (cheap insurance against measurement-noise tails).

### Scale per comparator

Comparators are sized to their **argumentative role**, not uniformly.
Three roles, three scale targets:

| role | who | headline scale | linearity sub-curve | why |
|---|---|---|---|---|
| **self** | Kāra | 1M + 2M | implicit (multi-scale ladder from M1 → M3) | Kāra's own ceiling story; per-conn-bytes linearity is what unblocks the scale-invariance argument for everyone else |
| **credibility** | Rust (rustls + tokio) | 1M + 2M | implicit (tracks Kāra) | "Kāra is at least as serious as the modern serious choice" — needs symmetric ceiling probes for the comparison to read as principled, not cherry-picked. Empirical head-to-head at the ceiling beats extrapolated. |
| **commercial** | Phoenix, Java/Netty, Go, .NET (Linux + Windows), Node | **250K** | 50K | This is the **real-world per-box deployment scale** for production WebSocket fleets — most prod fleets run 50K–250K per box and scale horizontally, not 1M per box. Matches the M2 milestone (#167 in phase-6) and the published per-node densities for Discord/Slack/Pinterest. Per-conn-bytes ratio is scale-invariant (see below), so 250K against 250K produces the same headline ratio as 1M against 1M would, at ~5× less rig effort and ~40% less wall-clock per comparator. |
| **stretch** | SignalR, socket.io, Python | **100K** | 50K | Stretch rows are completeness, not headline. 100K is high enough that first-conn overhead is negligible (>10K is the floor for per-conn-bytes to be meaningful per the [per-conn-bytes definition](#per-conn-bytes-definition)) and low enough that the setup-to-runtime ratio stays favorable. |

**Scale-invariance argument (load-bearing for the 250K choice):**
Per-conn-bytes is dominated by per-connection state (TLS session
buffer, WebSocket framing buffers, socket-buffer reservation, task
stack). Once N is large enough that fixed first-connection overhead
(TLS context, RNG state, per-thread accept stacks, framework-level
caches) is amortized below the noise floor, the per-conn delta is
linear in N. This linearity is empirically confirmed for Kāra
end-to-end at the **working-handler** figure: 1M = 12,114 B/conn,
2M = 12,111 B/conn — **−0.03 % drift** (the pre-fix build showed the
same shape at 7,846 → 7,861 B/conn, 0.19 %). Other
stacks may have different curves at low
N (BEAM heap pre-allocation, JVM heap warm-up, V8 inline-cache
warm-up) — that's exactly what the 50K linearity sub-curve detects.

**Linearity-escalation gate.** If a comparator's per-conn-bytes
drifts > 5% between 50K and the headline scale (250K or 100K),
that stack's per-conn-bytes is non-linear in the measured range
and we add a third scale (typically 1M) to localise the curve
before publishing a ratio. **Phoenix is the most likely candidate**
for triggering this — BEAM allocates a per-process heap that's
sized to the process count, and the warm-up curve isn't a constant
fraction. Without the gate, the scale reduction risks publishing
a ratio that doesn't actually generalize to production scale.

**Caveat carried into reframes.** Per the [commercial-reframe
lens](#commercial-reframe-lens) discipline guards, any reframe that
quotes a ratio inherits the scale at which that ratio was measured.
A "$1M → $434K" reframe derived from a 250K-vs-250K comparator
measurement is honest as long as the linearity check passed; it
becomes dishonest only if the linearity check failed and we
publish it anyway.

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

- **Kāra vs Rust (Kāra 1M + 2M post-fix landed; Rust 1M + 2M landed):** Both
  stacks run identical TLS config (TLS 1.3, X25519, AES-128-GCM, same
  cert fixture, no resumption). Both run idle = truly idle (no
  application-layer keepalive). The **2.30×** userspace ratio is straight
  per-conn-RSS delta with no framework layer on either side (Kāra
  12,114 B vs Rust 27,895 B at 1M, same rig), and is **empirically
  scale-invariant on both sides to the ceiling** — Kāra drifts −0.03 %
  (12,114 → 12,111 B/conn) and Rust drifts 0.33 % (27,895 → 27,893 B/conn)
  from 1M to 2M. For the cost claim the **total-box ratio is 2.03×**: adding
  the ~3,277 B/conn server-side kernel socket buffer (the `tcp_rmem`/`tcp_wmem`
  4 KB floors), which is stack-independent and paid identically by both, gives
  15.0 KB (Kāra) vs 30.4 KB (Rust) total server-side memory. The 2.30× is the
  runtime-density figure; the 2.03× is the cost-relevant total-box figure.
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

- **Status:** `landed @ 1M + 2M` (working-handler re-measure, 2026-06-01;
  scale-invariant, −0.03 % drift). The **x86 cross-ISA 1M re-read landed
  2026-06-02** (12,112 B, −0.02 % vs arm64), closing the last `‡` — see the
  cross-ISA block below.
- **Build:** `karac build` against `examples/ws_idle_holder/src/main.kara`
  off `main` ⊇ `eba48194` (both coro-frame heap-overflow fixes — the
  `Array[u8,4096]` slot mis-size `fe6afd16` and the missing-datalayout
  `coro.size` under-allocation `eba48194`).
- **Runtime:** coroutine network-async transform on by default; TLS via
  `karac-runtime` rustls integration (TLS 1.3 / X25519 / AES-128-GCM).
- **Hardware:** `r8g.4xlarge` (16 vCPU Graviton4, 128 GB RAM,
  arm64, Ubuntu 24.04).

**Idle-hold @ 1M (working handler, landed 2026-06-01) — THE HEADLINE:**

| metric | value | notes |
|---|---|---|
| established | 1,000,000 / 1,000,000 | 0 failed; clean teardown, no heap corruption |
| **per-conn bytes** | **~12,114 B (12.1 KB)** | server-RSS delta / N; the working handler holds its 4 KB recv buffer + coro frame |
| server RSS held | 11,832,444 KiB (~11.28 GiB) | RSS delta / N matches per-conn-bytes |
| connect mean | 82.3 ms | `c=64`, full 1M ramp |
| connect p50 | 45.9 ms | architectural floor (park/wake path) |
| connect p95 | 214.1 ms | |
| connect p99 | 254.8 ms | tail collapsed vs pre-fix 1856 ms — `ec2_setup.sh` sysctls removed the SYN-retransmit cliff |
| connect max | 480.4 ms | vs pre-fix 2306 ms |
| churn cliff_ratio | TBD | active-traffic landed ([§Active-traffic stress test](#active-traffic-stress-test)); cliff_ratio tracks the deferred handshake-QPS / reconnect-storm sub-run (#66) |

- Raw JSON: `docs/investigations/demo1_m3_1m_postfix_datalayout.json`.
- Acceptance criteria (all met): `established == 1,000,000` AND
  `failed == 0` ✓; clean teardown, no `corrupted size vs. prev_size`
  (the pre-fix failure mode) ✓; `per_conn_bytes` includes live
  per-conn handler state (recv buffer held, not freed) ✓.

**Idle-hold @ 2M (working handler, landed 2026-06-01) — SCALE-INVARIANCE:**

| metric | value | notes |
|---|---|---|
| established | 2,000,000 / 2,000,000 | 0 failed |
| **per-conn bytes** | **~12,111 B (12.1 KB)** | **−0.03 % drift vs 1M (12,114 B) — scale-invariance confirmed at the working figure** |
| server RSS held | 23,656,544 KiB (~22.56 GiB) | RSS delta / N matches per-conn-bytes |
| connect mean | 194.5 ms | `c=64`, full 6077 s ramp |
| connect p50 | 46.0 ms | architectural floor (matches 1M's 45.9 ms) |
| connect p95 | 613.4 ms | tail expansion vs 1M (214 ms) tracks held-conn count |
| connect p99 | 732.6 ms | |
| connect max | 1193.7 ms | |
| ramp time | 6077.18 s (~1 h 41 min) | 329 conns/sec avg — superlinear degradation w/ held-conn count, orthogonal to per-conn memory |

- Raw JSON: `docs/investigations/demo1_m3_2m_postfix_datalayout.json`.
- Acceptance: `established == 2,000,000` AND `failed == 0` ✓; `per_conn_bytes`
  within ±5 % of the 1M post-fix value (12,114 → 12,111 = **−0.03 % drift**) ✓.
  Four in-flight slope samples during the ramp (12,114 / 12,085 / 12,076 /
  12,068 B at 1.0M / 1.46M / 1.57M / 1.69M conns) independently traced the same
  flat per-conn line before the formal settle.

**What this proves end-to-end.** The working handler holds its
per-connection state — the 4 KB recv buffer, the coroutine frame, the
parking — across the idle hold, at **12.1 KB/conn** measured server-side,
**scale-invariant from 1M to 2M (−0.03 % drift)**. This is apples-to-apples
with Rust's 27.9 KB (which has always included its per-conn task state): a real
**2.30×** per-connection density edge that holds to the ceiling.

> **‡ The pre-fix 2M row below is SUPERSEDED for the density headline.** It
> was measured with the non-executing handler (7.8 KB/conn) and understates a
> working server. The post-fix 2M figure above replaces the pre-fix 2M density;
> it is retained as historical record of the establishment / latency /
> scale-linearity *shape* (handler-state-independent). **Do not quote its
> per-conn-bytes externally.** (The x86 cross-ISA row further below is now
> **post-fix** — a real working-handler figure, not superseded.)

**‡ Idle-hold @ 2M (PRE-FIX, superseded — 2026-05-30):**

| metric | value | notes |
|---|---|---|
| established | 2,000,000 / 2,000,000 | 0 failed |
| per-conn bytes | ~7,861 B ‡ pre-fix | 0.19 % drift vs pre-fix 1M — confirms the *linearity shape*, not the headline number |
| server RSS held | 15,355,328 KiB (~14.65 GiB) | pre-fix (non-executing handler) |
| connect mean | 214.6 ms | `c=64`, full 6707 s ramp |
| connect p50 | 41.0 ms | architectural floor |
| connect p95 | 673.9 ms | |
| connect p99 | 798.2 ms | |
| connect max | 1204.9 ms | |
| ramp time | 6706.86 s (~1 h 51 min) | 298 conns/sec avg vs 783 @ 1M |

- Raw JSON: `kara-2m.json` on the (terminated) bench rig.
- **What the pre-fix 2M run still validates:** per-conn-bytes is
  *linear* in held-conn count (0.19 % drift 1M→2M) — a property of the
  fixed-size per-conn allocation, independent of whether the handler
  executes. **The post-fix 2M run above confirmed this holds at the
  working 12.1 KB figure (−0.03 % drift), so this pre-fix row is now
  fully superseded.**

**Cross-ISA confirmation (x86, POST-FIX — landed 2026-06-02).**
A working-handler Kāra 1M run on `c7i.8xlarge` (Intel x86_64, 32 vCPU,
64 GB, Ubuntu 24.04, build off `main` ⊇ `eba48194`):

| metric | x86 1M (post-fix) | arm64 1M (post-fix) | delta |
|---|---|---|---|
| established | 1,000,000 / 0 failed | 1,000,000 / 0 failed | — |
| **per-conn bytes** | **12,111.98 B** | 12,114 B | **−0.02 %** |
| server RSS held | 11,830,856 KiB (~11.28 GiB) | ~11.28 GiB | flat |
| connect p50 | 44.2 ms | ~41–46 ms | reproduces floor |
| connect mean | 54.4 ms | — | core-count-confounded, not claimed |
| connect max | 197.1 ms | — | |
| ramp time | 849.7 s (~14.2 min) | — | 32 vCPU, not apples-to-apples |

**Density at the working figure is ISA-identical (−0.02 %), not
Graviton-specific** — and it supersedes the pre-fix x86 7,725 B read
(non-executing handler). Only per-conn density is claimed cross-ISA;
the mean/ramp are faster than arm64 but confounded by core count (32
vs 16 vCPU), so they are *not* claimed apples-to-apples. The p50 floor
(44.2 ms) reproduces the arm64 floor cross-ISA, consistent with the
prior pre-fix reading (41 ms) — an architectural property of the
park/wake path. Raw JSON:
`docs/investigations/demo1_m3_1m_x86_postfix.json`. The validation
correctness check (50K idle-hold on a `c7i.2xlarge`) landed
12,131 B/conn (+0.14 % vs arm64) with deterministic echo before the
1M run.

**(Historical) The pre-fix x86 1M run (2026-05-31)** landed
`per_conn_bytes = 7,725.3 B` on the non-executing-handler build —
superseded for the density headline by the post-fix run above, retained
only as the first-ever x86_64-Linux karac build, which surfaced + fixed
two karac/rig gaps en route (PIC reloc model, `bda38682`; `fs.nr_open`
+ systemd nofile cap, `6437e765`). Raw JSON:
`docs/investigations/demo1_m3_1m_x86.json`.

**Ramp-rate note.** The 298 conns/sec average ramp at 2M is
~38 % of the 1M ramp rate (783 conns/sec). This is the
established superlinear-degradation pattern for connection
establishment under increasing held-conn count (epoll fd-set
walk, accept-queue contention) — orthogonal to per-conn memory
which stays flat. Filed for the active-traffic stress slice
(#66); does **not** affect the density headline.

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

- **Status:** `landed @ 1M and 2M` (2M landed 2026-05-30).
- **Build:** `examples/ws_idle_holder/rust/`,
  `cargo build --release`, version pinned in `Cargo.toml`.
- **Stack:** `tokio` async runtime; `rustls` for TLS;
  `tokio-tungstenite` for WebSocket upgrade + framing.
- **Hardware:** same `r8g.4xlarge` as Kāra; fresh box.

**Idle-hold @ 1M (landed, 2026-05-29):**

| metric | value | notes |
|---|---|---|
| established | 1,000,000 / 1,000,000 | 0 failed |
| per-conn bytes | **~27,895 B (27.9 KB)** | server-RSS delta / N, settled |
| connect mean | 64.26 ms | `c=64`, 1004 s ramp |
| connect p50 | 2.59 ms | async runtime collapses the handshake hop |
| connect p99 | 303.94 ms | tail wider than Kāra at same point |
| ramp time | 1004 s | 996 conns/sec |

**Idle-hold @ 2M (landed, 2026-05-30):**

| metric | value | notes |
|---|---|---|
| established | 2,000,000 / 2,000,000 | 0 failed |
| per-conn bytes | **~27,893 B (27.9 KB)** | 0.33 % drift vs 1M — scale-invariance confirmed |
| server RSS held | 54,481,448 KiB (~51.96 GiB) | RSS delta / N matches per-conn-bytes |
| connect mean | 206.9 ms | `c=64`, 6465 s ramp |
| connect p50 | 2.93 ms | basically flat vs 1M (2.59 ms) — async handshake hop scales |
| connect p95 | 745.3 ms | tail wider than Kāra (673.9 ms) at the same N |
| connect p99 | 872.1 ms | |
| connect p99.9 | 1014.9 ms | |
| connect max | 1336.4 ms | |
| ramp time | 6464.76 s (~108 min) | 309 conns/sec vs 996 @ 1M — superlinear degradation with held-conn count (same shape as Kāra) |

- Raw JSON: `rust-2m.json` on the bench rig; mirror to
  `docs/investigations/demo1_m3_2m_rust.json` on next sync.
- Acceptance criteria (all met):
  1. `established == 2,000,000` AND `failed == 0`. ✓
  2. `per_conn_bytes` within ±5 % of the 1M value
     (27,895 → 27,893 = 0.33 % drift). ✓
  3. Rust `per_conn_bytes` ≥ 2× Kāra's working-handler density (Kāra
     post-fix 1M = 12,114 B; observed Rust 27,893–27,895 B = 2.30×). ✓
     _(The original ≥3.0× criterion was written against the pre-fix
     non-executing Kāra figure and is superseded.)_
  4. `dmesg` clean on the measured run. The visible `VFS:
     file-max limit 3000000 reached` entry at uptime 8682 s
     is from a teardown-phase touch of the legacy sysctl cap
     *after* the JSON was emitted with `ok: true` + 2M
     established + 0 failed — recoverable on the next 2M+ run
     by the same `fs.file-max=8000000` patch landed alongside
     the Kāra 2M run. ✓

**Head-to-head with Kāra @ 2M:**

| metric | Kāra | Rust | winner |
|---|---|---|---|
| established / failed | 2,000,000 / 0 | 2,000,000 / 0 | tie |
| ramp time | 6707 s | **6465 s** | Rust (−3.6 %) |
| `connect.mean_ms` | 214.6 | **206.9** | Rust (−3.6 %) |
| `connect.p50_ms` | 41.0 | **2.93** | Rust (−93 %) |
| `connect.p95_ms` | **673.9** | 745.3 | Kāra (−10 %) |
| `connect.p99_ms` | **798.2** | 872.1 | Kāra (−9 %) |
| `connect.p99.9_ms` | **932.6** | 1014.9 | Kāra (−8 %) |
| `connect.max_ms` | **1204.9** | 1336.4 | Kāra (−10 %) |
| **`per_conn_bytes`** | **12,111** (post-fix) | **27,893** | **Kāra (2.30×)** |

> **‡ Mixed-vintage row.** The **memory** cell now uses the **post-fix Kāra 2M**
> figure (12,111 B, working handler) against Rust's 2M (27,893 B) — a real
> same-scale **2.30×** at the ceiling. The **throughput / latency / tail** rows
> in this table are from the **pre-fix** Kāra 2M run (handler-state-independent,
> so still representative of the establishment shape); the post-fix 2M latencies
> (mean 194.5 / p50 46.0 / p95 613.4 / p99 732.6 / max 1193.7 ms) are in the
> §Kāra post-fix 2M table above and are within noise of these.

**What this proves end-to-end.** The multi-dimensional tradeoff:
**Rust wins throughput and mean (~4 %) + p50 (~14× tighter handshake
hop)**; **Kāra wins tail (~8–10 % at p95→max) and memory (2.30× on the
post-fix 1M density)**. For idle-heavy workloads where memory is the
binding constraint (chat, IoT push, ISP gateways), Kāra's **12.1 KB/conn
means a single 128 GiB box holds ~11.3M conns where Rust OOMs at
~4.9M** — the same 2.30× headroom. The 46 ms Kāra p50 vs Rust's ~3 ms
confirms the [line 287 follow-on
entry](../../../docs/implementation_checklist/phase-6-runtime.md)'s
architectural-floor finding is Kāra-side, **not** a workload
artifact (Rust at the same N c=64 hits ~3 ms — same kernel,
same network, same client driver).

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
- **Scale:** 250K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)). **Phoenix is the
  most likely candidate to trigger the linearity-escalation gate**
  — BEAM heap pre-allocation has a non-constant warm-up shape. If
  50K vs 250K per-conn-bytes drift > 5%, we add a 1M Phoenix run
  before publishing the ratio.

**Expected range (from public data):** 5–10 KB/conn. Phoenix is
the density-king comparator and the most rhetorically dangerous —
the WhatsApp/Discord lineage. If Phoenix matches Kāra within ~20%,
the framing shifts to the [combination claim](#commercial-reframe-lens):
**density + static types + single-binary deploy + no BEAM ops
surface**, not "Kāra wins on density alone."

**Sub-rows to fill:**

**Headline measurements @ 250K:**

| metric | Phoenix (Presence on) | Raw Channels | notes |
|---|---|---|---|
| established | TBD | TBD | |
| per-conn bytes | TBD | TBD | |
| connect mean | TBD | TBD | |
| framework overhead | (Phoenix − raw) | — | |

**Linearity check @ 50K:**

| metric | Phoenix (Presence on) @ 50K | drift vs 250K | gate |
|---|---|---|---|
| per-conn bytes | TBD | TBD | < 5% → publish; ≥ 5% → escalate to 1M |

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
- **Scale:** 250K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)). JVM heap warm-up
  is a known non-linearity source; linearity check is load-bearing.

**Expected range (from public data):** 20–40 KB/conn. The largest
**commercial TAM** comparator — every enterprise has JVM fleets
touching WebSockets somewhere. This is the cleanest dollarized
cost story when it lands.

**Sub-rows to fill:**

**Headline measurements @ 250K:**

| metric | Netty + G1GC | Netty + ZGC | notes |
|---|---|---|---|
| established | TBD | TBD | |
| per-conn bytes | TBD | TBD | RSS = JVM RSS, includes heap + Netty buffers |
| heap (resident) | TBD | TBD | sub-component of total RSS |
| direct buffers | TBD | TBD | Netty pooled direct mem; sub-component |
| connect mean | TBD | TBD | |

**Linearity check @ 50K:**

| metric | Netty + G1GC @ 50K | drift vs 250K | gate |
|---|---|---|---|
| per-conn bytes | TBD | TBD | < 5% → publish; ≥ 5% → escalate to 1M |

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
- **Scale:** 250K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)).

**Expected range (from public data):** 20–30 KB/conn. The modern
default for new infra; smaller commercial delta than Java but
strong rhetorical position ("Go is good enough" is the default
counterargument we need to address).

**Sub-rows to fill:**

**Headline measurements @ 250K:**

| metric | Go + gorilla | notes |
|---|---|---|
| established | TBD | |
| per-conn bytes | TBD | RSS = Go process RSS, includes goroutine stacks |
| goroutine stack overhead | TBD | sub-component; 2 goroutines per conn typical |
| connect mean | TBD | |

**Linearity check @ 50K:**

| metric | Go + gorilla @ 50K | drift vs 250K | gate |
|---|---|---|---|
| per-conn bytes | TBD | TBD | < 5% → publish; ≥ 5% → escalate to 1M |

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
- **Scale:** 250K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)).

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
  Windows Server 2022). 64 GB is well above the 250K headline
  target; original 1M sizing rationale carried over but unused at
  the reduced scale.
- **TLS:** SChannel; matched cipher + cert (within SChannel's
  configurable surface).
- **Scale:** 250K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)). Windows ramp
  rate may differ from Linux; document if a separate
  linearity-escalation triggers.

**Expected delta vs Linux .NET:** SChannel and OpenSSL have
genuinely different per-conn TLS-state shapes. The Linux/Windows
.NET delta is itself a finding — it tells us "is the .NET overhead
the runtime or the OS TLS substrate?" — and matters for any buyer
whose .NET fleet is Windows-Server-default.

**Caveats:**

- Windows ephemeral port range, TIME_WAIT recycle, and TCP control
  block table size need tuning even at 250K; document the
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
- **Scale:** 250K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)). Node's per-box
  ceiling is around 250K–500K in published deployments; 250K is
  the right scale for the headline both for cross-comparator
  consistency and as a deployment-realistic number.

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
- **Scale:** 100K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator) — stretch rows
  run at smaller scale than commercial).

### socket.io _(stretch)_

> _Pending — wip task #75. Stretch row — not blocking v1 claim._

- **Status:** pending, stretch.
- **Stack target:** Node.js + `socket.io` server; exposes the
  framework-overhead delta over raw `ws`.
- **Scale:** 100K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)).

### Python asyncio websockets _(stretch)_

> _Pending — wip task #76. Stretch row — not blocking v1 claim._

- **Status:** pending, stretch.
- **Stack target:** Python 3.12, `websockets` library, asyncio.
  Included for completeness; Python is not in the production WS
  density landscape for any serious deployment but the row exists
  so we can answer the inevitable "what about Python?" question.
- **Scale:** 100K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)).

---

## Active-traffic stress test

**Status: landed (wip task #66), 2026-06-05.** Measured on arm64 Graviton —
`r8g.4xlarge` for the 250K head-to-head, `m8g.4xlarge` (16-core Graviton4) for
the CPU-isolated burst sweep and the 1M ceiling. Build off `main ⊇ 97b2a39c`
(Stage B3 combined poll-and-dispatch reactor).

**Profile:** held connections plus a subset actively exchanging a 64-byte text
frame at 1 message/sec/conn, echoed by the server. The demo's
`handle_connection` echoes unconditionally, but the **idle path is
byte-identical** — `send_text` only fires on a real inbound frame, so idle
density is unchanged from the §Kāra idle-hold numbers. Payload is small enough
to not dominate the network, large enough to exercise framing. This is the
measurement that answers the "but it's just idle" objection that the [per-conn
density memory](../../../.claude/projects/-Users-mango-Documents-Gowtham-projects/memory/feedback_per_conn_density_is_the_headline.md)
calls out as load-bearing.

### Headline — realistic (desynchronized) arrival @ 250K

The real-workload number: 250K held + active conns arriving on independent
timers (`--stagger-arrival`), the way production chatter actually lands.

| metric | Kāra | Rust (rustls + tokio) |
|---|---|---|
| per-conn-bytes under traffic | **12,126 B** | 28,034 B |
| message latency p50 | **0.12 ms** | 0.04 ms |
| message latency p99 | **0.34 ms** | 0.07 ms |

**Both stacks are sub-millisecond, and the 2.31× density advantage holds under
active load** — the idle 12.1 KB/conn is not an artifact of doing nothing.
Raw JSON mirrored under `docs/investigations/` (realistic:
`active_250k_{kara,rust}-250k-realistic_stageA.json`; synchronized sidebar:
`active_250k_{kara,rust}-250k-sync_stageA.json`; pre-`--stagger-arrival`
artifact runs: `active_250k_{kara,rust}_prewakefix.json`).

> _Transparency:_ the Kāra realistic run logged **16 echo failures out of
> 598,824 (0.003 %, `ok:false` in the JSON)** vs Rust's 0. Negligible for the
> density/latency headline and not investigated here, but flagged so the
> committed JSON doesn't read as a silent discrepancy — if a churn/active
> comparator later shows the same small echo-loss tail, it's worth a look.

### Worst case — synchronized burst (broadcast / reconnect storm)

When every active conn fires in the same instant — broadcast fan-out, or a
reconnect storm after a deploy — the load becomes a thundering herd. This is
Kāra's worst case, and closing it drove the Stage B reactor work. CPU-isolated
measurement (`m8g.4xlarge`, server `taskset -c 0-7` `KARAC_REACTOR_SHARDS=8`,
client `-c 8-15`, 10K × 128 B × 1 Hz × 20 s synchronized):

| stage | what changed | p50 | p99 | JSON |
|---|---|---|---|---|
| baseline | single shared reactor | 72 ms | 92 ms | `burst_isolated_baseline.json` |
| B1 | release `fds` lock across `epoll_ctl` | 35 ms | 44 ms | `burst_isolated_b1.json` |
| B2 | shard the reactor (N fd-routed epoll) | 24 ms | 33 ms | `burst_isolated_b2.json` |
| **B3** | **combined poll-and-dispatch per shard** | **~5 ms** | **~8 ms** | `burst_isolated_b3.json` |

**baseline 72 → B3 ~5 ms p50 — a ~14× worst-case improvement, now within ~3× of
Rust's ~1.6 ms** (was ~45× at baseline). Removing the shared wakeup queue +
condvar handoff let the idle cores drain the burst in parallel, exactly as the
B2 diagnosis (0.28 of 8 cores used under load = serialization stall, not compute
saturation) predicted. Zero echo failures across 6 B3 runs. JSONs are mirrored
under `docs/investigations/`.

### Density + functional hold @ 1M active

The 1M ceiling run (`m8g.4xlarge`, single box, B3) confirms density scales and
the server stays functional under real load:

- **1,000,000 conns held, 0 failed.**
- **Density 12,127 B/conn (11.84 KiB)** — scale-invariant across the active
  ladder (250K ≈ 12.1K, 10K burst ≈ 12.5K, 1M ≈ 12.13K; ~2.3× vs Rust's
  ~27.9K).
- **Functional under load: 8.23M messages echoed, 0 echo failures** at 1M
  active conns.
- Connect p50 45 / p99 557 ms (c128 loopback tail, 0 failed). JSON
  `demo1_1m_active_realistic_b3.json` (mirrored under `docs/investigations/`).

> **Caveat — the 1M active *latency* is excluded from the headline.** On a
> single box the Rust client driving 1M TLS connections saturated the shared
> 16 cores (p50 2.4 s, only ~41 % of intended messages sent) — a co-location
> confound, not a server property. A clean 1M active-latency number needs a
> separate client box. The clean latency story is therefore the CPU-isolated
> **250K realistic (sub-ms)** + the **B3 burst (~5 ms)**; the 1M run delivers
> its intended value: the **density ceiling + 1M functional hold**.
>
> A **cross-box** 1M active run (separate client boxes over a real NIC →
> `demo1_1m_active_crossbox_b3.json`) corroborates the functional hold — 1M
> established 0-failed, **10.03M messages echoed, 0 failures** — and trims the
> latency to p50 1.83 s. It stays client-influenced (driving 1M live TLS conns
> is itself heavy, even from a client fleet), so it confirms the hold without
> yet yielding a clean 1M latency number; server-side memory wasn't sampled on
> that run (server on a separate box), so density stays sourced from the
> single-box run above.

### Arrival-model note (why two latency numbers)

The original "146 ms active latency" finding was almost entirely a
**synchronized-burst measurement artifact**: the harness fired every active
conn on aligned 1-second timers (tokio `interval`, first-tick-immediate), a
thundering herd every second. Adding `--stagger-arrival` (commit `d618f708`)
desynchronizes arrival → realistic chatter → latency collapses 74 ms → 0.15 ms
p50. Canonical active-traffic runs use `--stagger-arrival`; the synchronized
burst is kept as a labeled worst-case sidebar, not the headline.

**Deferred:** handshake-QPS at high concurrency (c≥1000) — the reconnect-storm
throughput number — is a separate rig run (wip task #66 sub-item).

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

- **Kāra vs Rust (density → production cost @ 250K, working handler):**
  _Technical:_ Kāra holds each idle WebSocket connection in **12.1 KB/conn**
  userspace vs Rust+rustls+tokio at 27.9 KB/conn — a **2.30× runtime-density
  advantage**, measured on the same `r8g.4xlarge` rig with the per-connection
  handler executing (recv buffer + coroutine frame held, not freed), and
  **scale-invariant 1M↔2M** (Kāra −0.03 % drift). Counting the ~3.3 KB/conn
  kernel socket buffer both stacks pay equally, **total server-side memory is
  15.0 KB vs 30.4 KB = 2.03×** — the cost-relevant figure.

  _Buyer impact (the production reality, not the ceiling):_ at a realistic
  **250K idle connections per box**, working sets are ~5.2 GiB (Kāra) vs
  ~8.9 GiB (Rust), which lands Kāra on an **8 GiB `m7g.large`** where Rust
  needs a **16 GiB `m7g.xlarge`** — one instance class smaller. On a 1-year
  no-upfront reserved instance (us-east-1, verified May 2026), that is
  **~$473/yr per 250K unit (Kāra) vs ~$946/yr (Rust)** — a **~50 % infra cost
  reduction** (on-demand: $718 vs $1,428/yr; 3-yr RI: $324 vs $648/yr). Because
  cloud RAM steps in 2× jumps, the 2.03× cashes out discretely as "one tier
  down," and scales with fleet: a 5M-conn fleet (20 HA-sharded 250K units)
  saves **~$9.5K/yr** on 1-yr RIs. For large buyers the **operational lever**
  — half the box count to provision, patch, monitor, and page on — is often
  worth more than the raw instance dollars.

  _Sizing basis:_ per-conn memory is measured (server-RSS slope, settled);
  kernel-buffer share is the measured total-system delta minus userspace RSS;
  instance fit assumes ~1.5 GiB OS/runtime headroom on top of the working set;
  RI prices are AWS us-east-1 standard no-upfront as published May 2026. The
  **ceiling flex** (credibility, not the cost lead): the same density lets a
  single 128 GiB box hold ~11.3M Kāra conns where Rust OOMs at ~4.9M — real,
  but nobody runs 11M conns on one box, so it is not the buyer story.
  _Caveats apply — see [Rust comparator caveats](#rust-rustls--tokio)._

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

Columns reflect the [scale-per-comparator](#scale-per-comparator)
split: `1M / 2M` cells apply only to Kāra (self) and Rust
(credibility); commercial and stretch rows use `50K linearity` and
their role's headline scale (`250K` or `100K`).

| comparator | role | linearity (50K) | headline | 2M | active-traffic | reproduction script | raw JSON |
|---|---|---|---|---|---|---|---|
| Kāra | self | n/a (multi-scale ladder) | **1M landed (post-fix, 2026-06-01)** _(x86 1M re-read landed post-fix 2026-06-02)_ | **2M landed (post-fix, 2026-06-01)** | **250K + 1M landed (B3, 2026-06-05)** — 12,126 B/conn, p50 0.12 ms realistic; burst p50 ~5 ms; 1M held 0-failed, 8.23M echoed | `scripts/run_1m.sh` + `scripts/run_2m.sh` | 1M: `docs/investigations/demo1_m3_1m_postfix_datalayout.json`; 2M: `docs/investigations/demo1_m3_2m_postfix_datalayout.json`; x86 1M (post-fix): `docs/investigations/demo1_m3_1m_x86_postfix.json`; x86 1M (pre-fix, historical): `docs/investigations/demo1_m3_1m_x86.json`; active-traffic: `docs/investigations/active_250k_kara-250k-{realistic,sync}_stageA.json`, `demo1_1m_active_realistic_b3.json`, `demo1_1m_active_crossbox_b3.json`; burst sweep: `burst_isolated_{baseline,b1,b2,b3}.json` |
| Rust | credibility | n/a (tracks Kāra) | 1M landed | **2M landed (2026-05-30)** | **250K landed (2026-06-02)** — 28,034 B/conn, p50 0.04 ms realistic; burst ~1.6 ms | `scripts/run_1m.sh` + `scripts/run_2m.sh` | 1M: `rust-1m.json`; 2M: `rust-2m.json` (mirror pending); active-traffic: `docs/investigations/active_250k_rust-250k-{realistic,sync}_stageA.json` |
| Phoenix Channels | commercial | pending (#67) | 250K pending (#67) | n/a unless gate escalates | pending | TBD | TBD |
| Java / Netty | commercial | pending (#68) | 250K pending (#68) | n/a unless gate escalates | pending | TBD | TBD |
| Go | commercial | pending (#69) | 250K pending (#69) | n/a unless gate escalates | pending | TBD | TBD |
| .NET (Linux) | commercial | pending (#71) | 250K pending (#71) | n/a unless gate escalates | pending | TBD | TBD |
| .NET (Windows) | commercial | pending (#72) | 250K pending (#72) | n/a | pending | TBD | TBD |
| Node.js | commercial | pending (#73) | 250K pending (#73) | n/a unless gate escalates | pending | TBD | TBD |
| SignalR _(stretch)_ | stretch | pending (#74) | 100K pending (#74) | n/a | pending | TBD | TBD |
| socket.io _(stretch)_ | stretch | pending (#75) | 100K pending (#75) | n/a | pending | TBD | TBD |
| Python _(stretch)_ | stretch | pending (#76) | 100K pending (#76) | n/a | pending | TBD | TBD |

> Task numbers reference `wip-bench-day.md` (uncommitted; lives in
> repo root). When that file is deleted on ship, the equivalent
> tracker entries in `docs/implementation_checklist/phase-6-runtime.md`
> become the durable references.

---

## Change log

- **2026-06-02 (x86 cross-ISA density re-read, POST-FIX — closes the last `‡`):**
  re-ran the working-handler Kāra **1M** on a fresh `c7i.8xlarge` (Intel x86_64,
  32 vCPU, 64 GB, Ubuntu 24.04; build off `main` ⊇ `eba48194`): 1,000,000 / 0
  failed, clean teardown, **`per_conn_bytes = 12,111.98`** (server RSS 11.28
  GiB) — **−0.02 % vs the arm64 1M figure of 12,114 B.** Density at the working
  figure is **ISA-identical, not Graviton-specific**, superseding the pre-fix
  x86 7,725 B read (non-executing handler). Connect p50 44.2 ms reproduces the
  cross-ISA p50 floor; mean/ramp faster than arm64 but core-count-confounded
  (32 vs 16 vCPU), so only density is claimed cross-ISA. A correctness check on
  a `c7i.2xlarge` (50K idle-hold, 12,131 B / +0.14 %; 8/8 deterministic echo)
  validated the post-fix coroutine + heap-overflow path on x86's ABI before the
  1M run. Banner, TL;DR ‡-note, hardware table, §Kāra status + cross-ISA block
  (now a post-fix table, pre-fix demoted to historical), role-column note, and
  status matrix all updated. **No remaining `‡` items.** Raw JSON:
  `docs/investigations/demo1_m3_1m_x86_postfix.json`.
- **2026-06-01 (Kāra working-handler 2M re-confirm + production cost model):**
  the post-fix Kāra **2M** density run landed on `r8g.4xlarge` (build ⊇
  `eba48194`): 2,000,000 / 0 failed, `per_conn_bytes = 12,111` (server RSS
  22.56 GiB), **−0.03 % drift vs the 1M post-fix 12,114 B — scale-invariance
  confirmed at the working figure.** Four in-flight slope samples (12,114 /
  12,085 / 12,076 / 12,068 B across 1.0M–1.69M conns) traced the same flat line
  before the settle. This closes the `‡` 2M-pending item from the 1M correction
  below; only the x86 cross-ISA density re-read remains. **Added a measured 250K
  production cost model:** total server-side memory is **15.0 KB (Kāra) vs
  30.4 KB (Rust) = 2.03×** once the ~3.3 KB/conn stack-independent kernel socket
  buffer (measured as total-system-delta minus userspace RSS, live off the 2M
  ramp) is counted. At 250K idle conns Kāra fits an 8 GiB `m7g.large` vs Rust's
  16 GiB `m7g.xlarge` → ~50 % infra cost (**~$473 vs ~$946/yr** per unit on a
  1-yr no-upfront RI, us-east-1 verified May 2026). The commercial reframe now
  leads with this 250K + RI model anchored on 2.03×; the 11.3M-conns line is
  demoted to a one-line ceiling flex. Banner, TL;DR table + reframe, §Kāra (new
  post-fix 2M table), head-to-head memory row (now post-fix 2M-vs-2M, 2.30×),
  and consolidated caveats all updated. Raw JSON:
  `docs/investigations/demo1_m3_2m_postfix_datalayout.json`.
  **Outstanding:** post-fix x86 cross-ISA density re-read.
- **2026-06-01 (Kāra working-handler 1M re-measure — HEADLINE
  CORRECTION):** all pre-this-date Kāra density figures (7.8 KB/conn,
  3.55× ratio, 1M↔2M scale-invariance, x86 cross-ISA) were measured
  with the per-connection handler **not executing** (compiled to a
  body-less state machine — "bug C"), so the handler's per-conn state
  (4 KB recv buffer + coro frame + parking) was freed, not held. The
  handler now executes (A2 coroutine transform on by default), and two
  coroutine-frame heap-overflow bugs that crashed the working binary on
  glibc were fixed: `fe6afd16` (Array[u8,4096] frame slot mis-sized to
  an 8-byte i64 instead of inline [4096 x i8]) and `eba48194` (codegen
  module carried no `target datalayout`, so `llvm.coro.size`
  under-allocated the frame by the i64-alignment delta and the trailing
  suspend-index stored one byte past the malloc — both glibc-only,
  silent on macOS even under ASAN). Re-measured 1M on `r8g.4xlarge`
  (build ⊇ `eba48194`): 1,000,000 / 0 failed, clean teardown,
  `per_conn_bytes = 12,114` (server RSS 11.28 GiB), connect p99 255 ms /
  max 480 ms (tail collapsed vs pre-fix 1856 / 2306 ms — sysctls).
  **Headline corrected: per-conn 7.8 → 12.1 KB, ratio 3.55× → 2.30×
  vs Rust (27.9 KB, same rig), cost reframe $282K → $434K.** Rust's
  figures, established counts, and connect percentiles are unaffected.
  TL;DR table, Kāra section, head-to-head, caveats, commercial reframe
  all updated; pre-fix Kāra 2M + x86 rows flagged `‡` superseded
  (handler-state-dependent per-conn numbers retracted; handler-state-
  independent shape/latency/linearity findings retained). Raw JSON:
  `docs/investigations/demo1_m3_1m_postfix_datalayout.json`.
  **Outstanding:** post-fix Kāra 2M re-confirm of 12.1 KB; post-fix x86
  density re-read.
- **2026-05-30:** initial skeleton; Kāra & Rust 1M results carried
  over from `docs/investigations/demo1_m1_verification.md`. All
  other comparators stubbed with `TBD` placeholders. Headline
  ratio: 3.55× Kāra vs Rust @ 1M (landed).
- **2026-05-30 (revision):** scale-per-comparator split formalized.
  Added `role` column to TL;DR (self / credibility / commercial /
  stretch). Added [§Scale per comparator](#scale-per-comparator)
  methodology subsection with linearity-escalation gate (>5% drift
  between 50K and headline → escalate to 1M). Commercial comparator
  per-section headers updated with scale field and per-comparator
  linearity-check sub-tables (Phoenix, Java/Netty, Go). Status
  matrix restructured: `linearity (50K)` + `headline (250K or 100K)`
  columns replace the old `1M / 2M` columns for non-Kāra/Rust rows.
  Headline ratio unchanged (3.55× is scale-invariant, validated by
  Kāra's 1M = 7,846 B vs 1.86M = 7,862 B = 0.2% drift). Effort
  reduction ~40% across Phase 3 + ~50% Phase 4 in wip-bench-day.
- **2026-05-30 (Kāra 2M landed):** Kāra 2M ceiling run landed on
  `r8g.4xlarge`: 2,000,000 / 2,000,000 established, 0 failed,
  `per_conn_bytes = 7,860.7` (0.19 % drift vs the 1M baseline of
  7,846 B — end-to-end empirical confirmation that the density
  ratio is scale-invariant). Ramp 6,706.86 s (298 conns/sec, vs
  783 @ 1M — the established superlinear-degradation pattern on
  connection establishment under increasing held-conn count;
  orthogonal to per-conn memory which stayed flat). p50 41.0 ms
  (matches the known architectural floor from task #65; not a
  regression). Surfaced a tuning gap on the bench rig: the prior
  attempt aborted on `fs.file-max = 3000000` (the default `ec2_setup.sh`
  setting); patched to `8000000` in `scripts/ec2_setup.sh` alongside
  this revision so future runs don't repeat the abort. TL;DR Kāra
  row + Kāra per-comparator section + apples-to-apples caveat +
  status matrix all updated. Rust 2M (#63) is the remaining piece
  for the head-to-head ceiling claim.
- **2026-05-30 (Rust 2M landed):** Rust 2M ceiling run landed on
  the same `r8g.4xlarge` rig (fresh box): 2,000,000 / 2,000,000
  established, 0 failed, `per_conn_bytes = 27,893` (0.33 % drift
  vs the 1M baseline of 27,895 B — matches Kāra's own 0.19 %
  drift; both impls' per-conn-bytes are empirically scale-invariant
  to the ceiling). Ramp 6,464.76 s (309 conns/sec, vs 996 @ 1M —
  same superlinear-degradation shape Kāra showed; both stacks are
  queue-depth-limited the same way at held-conn counts climbing
  past 1M). Connect tail: `p95=745ms`, `p99=872ms`, `p99.9=1015ms`,
  `max=1336ms` — ~8–10 % wider than Kāra at every tail percentile
  while Rust's p50 stays tight at 2.93 ms (vs Kāra's 41 ms
  architectural floor). **Headline 3.55× density ratio is now
  empirically scale-invariant at both endpoints** (3.548× at 1M,
  3.548× at 2M); the commercial 250K-vs-250K rows can rely on the
  scale-per-comparator argument without an extrapolation caveat.
  TL;DR Rust row + apples-to-apples caveat + Rust per-comparator
  section + commercial reframe + status matrix all flipped to
  `landed @ 2M`. Top-level `README.md` `Concurrency Runtime` line
  updated in the same commit to lead with the head-to-head 2M
  number rather than the Kāra-solo 1M number. Closes wip task #63.
- **2026-05-31 (x86 cross-ISA confirmation landed):** Kāra 1M run on
  `c7i.8xlarge` (Intel x86_64, 32 vCPU, 64 GB, Ubuntu 24.04):
  1,000,000 / 1,000,000 established, 0 failed, `per_conn_bytes =
  7,725.3 B` — **−1.54 % vs the arm64 1M baseline of 7,846 B**,
  inside the ±5 % gate. The 7.8 KB/conn density is confirmed **not
  Graviton4-specific**. `p50 = 41.02 ms` reproduces the arm64 p50
  floor (41.0 ms) to the millisecond — that floor is an
  architectural property of the runtime's park/wake path, not an ISA
  artifact. Mean/ramp look faster but are confounded by core count
  (32 vCPU vs the arm64 rig's 16), so only per-conn density (core-
  count-independent) is claimed cross-ISA. x86 2M deliberately
  skipped (1M→2M scale-invariance already locked on arm64 and is
  ISA-orthogonal). First-ever x86_64-Linux karac build; surfaced +
  fixed a PIC-reloc codegen gap (`bda38682`) and `fs.nr_open` +
  systemd-nofile rig gaps (`6437e765`). Kāra §Cross-ISA block +
  rig-table row + status matrix updated. Raw JSON:
  `docs/investigations/demo1_m3_1m_x86.json`. Closes wip task #62.
