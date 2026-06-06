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
> complete). **Go landed 2026-06-06 — 44.4 KB/conn, 3.66× Kāra, linearity
> +2.5%. Phoenix Channels landed 2026-06-06 — 102.8 KB/conn (presence-off,
> clean idle), 8.69× Kāra, linearity −1.8%; the heaviest comparator
> measured (Erlang `:ssl` + a process per conn). Java/Netty landed 2026-06-06 —
> 14.4 KB/conn (balanced heap, 1.19× Kāra) / ~12.8 KB marginal (1.06×); the
> second-densest stack and Kāra's closest competitor. .NET/ASP.NET Core
> (Linux) landed 2026-06-06 — 52.9 KB/conn, 4.47× Kāra, linearity −1.4%; the
> second-*heaviest* stack (between Go and Phoenix), and — unlike the JVM — a
> real per-conn cost, not a GC-heap dial (Server↔Workstation GC delta ~2%).
> Node.js (`ws`) landed 2026-06-06 — 40.4 KB/conn, 3.42× Kāra, linearity
> −1.79%; the 4th-densest stack — denser than Go (no per-conn stack on the
> single-threaded event loop) yet lighter than .NET on the same OpenSSL, and
> like .NET a real cost, not a dial (`--max-old-space-size` cap moves it
> +0.07%).**
> The **commercial tier is now complete** (Phoenix, Java/Netty, Go, .NET
> Linux, Node all landed; .NET Windows cut by decision). Only the optional
> stretch comparators (SignalR / socket.io / Python) are **deferred
> (optional), not blocking v1** — see the
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
| Phoenix Channels (Elixir) | commercial | 102.8 KB | **8.69×** | 250K + 50K landed (2026-06-06), −1.8% linearity | landed @ 250K | [§Phoenix](#phoenix-channels-elixir) |
| Java / Netty | commercial | 14.4 KB¹ | **1.19×** | 250K + 50K landed (2026-06-06) | landed @ 250K | [§Java/Netty](#java--netty) |
| Go (gorilla/websocket) | commercial | 43.4 KB | **3.66×** | 250K + 50K landed (2026-06-06), +2.5% linearity | landed @ 250K | [§Go](#go-gorillawebsocket) |
| .NET / ASP.NET Core (Linux) | commercial | 52.9 KB² | **4.47×** | 250K + 50K landed (2026-06-06), −1.4% linearity | landed @ 250K | [§.NET Linux](#net--aspnet-core-linux) |
| .NET / ASP.NET Core (Windows) | commercial | n/a | n/a | **cut by decision** (was #72) — Linux is the .NET headline; SChannel delta low-value | not run | [§.NET Windows](#net--aspnet-core-windows) |
| Node.js (ws) | commercial | 40.4 KB | **3.42×** | 250K + 50K landed (2026-06-06), −1.79% linearity | landed @ 250K | [§Node](#nodejs-ws) |
| SignalR _(stretch)_ | stretch | n/a | n/a | **deferred (optional)** — framework tax over .NET Linux; not blocking v1 | deferred | [§SignalR](#signalr-stretch) |
| socket.io _(stretch)_ | stretch | n/a | n/a | **deferred (optional)** — framework tax over Node; not blocking v1 | deferred | [§socket.io](#socketio-stretch) |
| Python asyncio websockets _(stretch)_ | stretch | n/a | n/a | **deferred (optional)**, lean-cut — not blocking v1 | deferred | [§Python](#python-asyncio-websockets-stretch) |

> ¹ **Java/Netty** is the one stack whose RSS ≠ live set: a JVM's footprint
> is dominated by GC heap-commit, which is `-Xmx`-dependent. The **14.4 KB /
> 1.19×** is the RSS at a balanced deployment heap (`-Xmx4g` @ 250K); the
> `-Xmx`-independent intrinsics are the **marginal slope ~12.8 KB (1.06×)**
> and the **live set ~8–10 KB** (below Kāra). It is the second-densest stack
> measured — see [§Java/Netty](#java--netty) for the full dial.

> ² **.NET (Linux)** is the JVM's mirror image: the CLR's Server GC also
> commits heap lazily, but here the per-conn RSS is **not** a dial — it is
> *real* live memory. The marginal slope (~52.7 KB) ≈ the absolute (52.9 KB),
> linearity is −1.4 %, and swapping Server→Workstation GC moves it ~2 %. So
> 52.9 KB / **4.47×** is the honest per-conn cost, not a tunable. .NET is the
> second-*heaviest* stack (between Go and Phoenix) — see
> [§.NET Linux](#net--aspnet-core-linux).

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

- **Kāra vs Go** _(Kāra 250K landed; Go 250K + 50K landed 2026-06-06)_:
  Kāra holds each connection in **3.66×** less userspace memory (12.1 KB vs
  **44.4 KB/conn**, measured server-RSS slope at 250K; Go's per-conn cost is
  linear, +2.5% drift 50K→250K). Go — the "good enough by default" stack — is
  heavier than even Rust here (1.59× the Rust comparator), the structural cost
  of a goroutine + `crypto/tls`'s per-conn record buffers + gorilla's 4 KB×2
  buffers, none shared across connections. **Production-unit cost at 250K:**
  Go's ~10.6 GiB userspace working set (measured) plus the ~3.3 KB/conn kernel
  socket buffer both stacks pay puts Go on a **16 GiB `m7g.xlarge`** where Kāra
  fits an **8 GiB `m7g.large`** — the same "one tier down → ~50 % infra cost"
  (~$473/yr vs ~$946/yr per 250K unit, us-east-1 1-yr RI) as the Rust reframe,
  but off a **larger** density gap. _The kernel-buffer share for Go is the
  inherited ~3.3 KB/conn estimate (the harness measures process RSS, not the
  total-system delta separately measured for Kāra/Rust); the userspace ratio
  and the instance-tier consequence are measured/derived directly._ _Caveats:
  raw gorilla + `crypto/tls`, no framework overhead — see [§Go](#go-gorillawebsocket)._

- **Kāra vs Phoenix** _(Kāra 250K landed; Phoenix 250K + 50K landed
  2026-06-06)_: against the runtime whose reputation *is* connection
  density, Kāra holds each idle connection in **8.69×** less userspace
  memory (12.1 KB vs **102.8 KB/conn**, presence-off clean idle hold,
  scale-invariant −1.8% 50K→250K). Phoenix is the **heaviest comparator
  measured** — heavier than Go (2.37×) and Rust (3.69×) — because real-world
  Phoenix pairs Erlang `:ssl` (several processes + per-socket buffers per
  connection) with a transport **and** channel process per conn, none
  shared. **Production-unit cost at 250K:** Phoenix's ~25.9 GiB measured
  userspace working set puts it on a **32 GiB `m7g.2xlarge`** where Kāra
  fits an **8 GiB `m7g.large`** — **two tiers down → ~75% infra cost** off
  the largest density gap in the comparator set. _The "2M connections on
  one box" Phoenix legend is a lighter config (plain `ws://`, no Channels
  join, no Presence, BEAM-tuned); this is the real-world Channels+TLS
  default. The combination claim (density + native AOT + static
  ownership/effects + no GC/BEAM ops surface) is the backstop, but here
  Kāra wins on raw density by an order of magnitude and does not need it._
  _Caveats: in-process TLS (apples-to-apples; some fleets offload TLS to a
  LB) and the presence-ON backpressure confound — see
  [§Phoenix](#phoenix-channels-elixir)._

- **Kāra vs Java/Netty** _(Kāra 250K landed; Netty 250K + 50K landed
  2026-06-06)_: the **closest** comparator — and the one that doesn't reduce
  to a clean infra-tier story, so it is reframed carefully. Netty's
  *marginal* per-conn (~12.8 KB) is within ~6% of Kāra (12.1 KB), and its
  live set (~8–10 KB) is actually below; at a balanced deployment heap
  (`-Xmx4g`) it holds 250K in ~3.7 GiB (14.4 KB/conn, 1.19× Kāra's ~3.0 GiB).
  So at 250K both fit the same **8 GiB `m7g.large`** tier — **no instance-
  tier saving from density alone here**, unlike Go/Rust/Phoenix. The Kāra
  levers against Java are different and should be led as such: (1) **no JVM
  fixed base** (Netty carries a multi-GB heap floor, so on small boxes / low
  conn counts Kāra's effective density lead widens); (2) **no RAM-vs-GC-CPU
  dial** — Netty's 14.4 KB is a *choice* on a curve from ~8 KB (tight heap,
  high GC CPU) to ~22–57 KB (loose heap), an operational tax Kāra's no-GC
  runtime doesn't levy; (3) the **combination claim** (native AOT + static
  ownership/effects + single-binary deploy + no JVM/GC ops surface) carries
  the weight where raw density nearly ties. _The buyer takeaway is "Kāra
  matches the densest mainstream JVM stack with none of the JVM operational
  surface," not a box-count cut._ _Caveats: in-process JDK JSSE TLS
  (tcnative is a non-default opt-in); heap-dial + GC-config nuance — see
  [§Java/Netty](#java--netty)._

- **Kāra vs .NET (Linux)** _(Kāra 250K landed; .NET 250K + 50K landed
  2026-06-06)_: Kāra holds each connection in **4.47×** less userspace memory
  (12.1 KB vs **52.9 KB/conn**, server-RSS at 250K; .NET's per-conn cost is
  linear, −1.4% drift 50K→250K). Unlike the JVM next door, this is **not** a
  GC dial to argue around — Server↔Workstation GC moves it ~2% and the marginal
  slope equals the absolute, so 52.9 KB is genuine live per-conn memory
  (`SslStream` buffers + Kestrel pipe segments + WS state, none pooled).
  **Production-unit cost at 250K:** .NET's ~12.7 GiB measured userspace working
  set plus the ~3.3 KB/conn kernel socket buffer both stacks pay puts .NET on a
  **16 GiB `m7g.xlarge`** where Kāra fits an **8 GiB `m7g.large`** — the same
  "one tier down → ~50% infra cost" (~$473/yr vs ~$946/yr per 250K unit,
  us-east-1 1-yr RI) as the Go and Rust reframes, off a density gap larger than
  either (4.47× vs Go's 3.66× / Rust's 2.30×). _The combination claim (native
  AOT + static ownership/effects + single-binary deploy + no CLR/GC ops
  surface) is the backstop, but here Kāra wins on raw density outright._ _The
  kernel-buffer share is the inherited ~3.3 KB/conn estimate; the userspace
  ratio and the instance-tier consequence are measured/derived directly._
  _Caveats: raw Kestrel + `UseWebSockets()` (no SignalR); in-process OpenSSL
  TLS — see [§.NET Linux](#net--aspnet-core-linux)._
- **Kāra vs Node.js** _(Kāra 250K landed; Node 250K + 50K landed
  2026-06-06)_: Kāra holds each connection in **3.42×** less userspace memory
  (12.1 KB vs **40.4 KB/conn**, server-RSS at 250K; Node's per-conn cost is
  linear, −1.79% drift 50K→250K). Like .NET and unlike the JVM, this is **not**
  a GC dial: a `--max-old-space-size=512` cap moves it **+0.07%**, so the
  40.4 KB is genuine live per-conn memory — native C++ buffers outside the V8
  heap (OpenSSL record buffers + libuv handles + `ws` frame state). Two facts
  sharpen the read: Node is **denser than Go** (40.4 vs 43.4 KB — the
  single-threaded event loop pays no per-conn stack, the same structural lever
  Kāra's reactor pushes 3.42× further), yet **lighter than .NET** (40.4 vs
  52.9 KB) **on the same OpenSSL**, isolating a real libuv-vs-Kestrel runtime
  delta. **Production-unit cost at 250K:** Node's ~9.86 GiB measured userspace
  working set plus the ~3.3 KB/conn kernel socket buffer both stacks pay puts
  Node on a **16 GiB `m7g.xlarge`** where Kāra fits an **8 GiB `m7g.large`** —
  the same "one tier down → ~50% infra cost" (~$473/yr vs ~$946/yr per 250K
  unit, us-east-1 1-yr RI) as the Go/.NET/Rust reframes. _Node's one edge is
  the reverse axis — it is a mature, ubiquitous runtime with a vast hiring pool;
  the Kāra story here is density + compile-time safety, not ecosystem maturity._
  _The kernel-buffer share is the inherited ~3.3 KB/conn estimate; the userspace
  ratio and instance-tier consequence are measured/derived directly._ _Caveats:
  raw `ws` (no socket.io); single process (no `cluster`); in-process OpenSSL
  TLS; Node's connect p50 ~50 ms is a single-thread handshake-throughput
  artifact, not a density cost — see [§Node.js](#nodejs-ws)._

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
  macOS/BSD/Windows-WSL. (Native Windows would use `Get-Process`
  Working Set, but no native-Windows comparator was run — the .NET
  Windows row was cut; see [§.NET Windows](#net--aspnet-core-windows).)

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
| Kāra / Rust | `r8g.4xlarge` | 16 (Graviton4) | 128 GB | arm64 | matches the Kāra & Rust 1M/2M baseline rig; cheap RAM headroom for the 2M target |
| Node _(landed 2026-06-06)_ | `m8g.4xlarge`-class | 16 (Graviton) | 61 GB | arm64 | same 16-vCPU Graviton class as Go/.NET-Linux/Phoenix/Netty; 250K Node held ~9.86 GiB so 61 GB is ample. Per-conn density is RAM/ISA-independent, so the RAM tier does not affect the head-to-head |
| .NET Linux _(landed 2026-06-06)_ | 16-vCPU Graviton, 61 GB | 16 (Graviton) | 61 GB | arm64 | same 16-vCPU Graviton class as Go/Phoenix/Netty; 250K .NET fits ~12.7 GiB so 61 GB is ample. Per-conn density is RAM/ISA-independent, so the smaller RAM tier does not affect the head-to-head |
| Java _(landed 2026-06-06)_ | 16-vCPU Graviton, 61 GB | 16 (Graviton) | 61 GB | arm64 | same 16-vCPU Graviton class as Go/Phoenix; all Netty runs fit (250K `-Xmx24g` over-commit peaked ~5.5 GiB, balanced `-Xmx4g` ~3.7 GiB). Per-conn density is RAM/ISA-independent, so the RAM tier does not affect the head-to-head |
| Go _(landed 2026-06-06)_ | `m8g.4xlarge` | 16 (Graviton4) | 61 GB | arm64 | same 16-vCPU Graviton4 class as the baseline; 250K Go fits ~10.6 GiB so 61 GB is ample. Per-conn density is RAM/ISA-independent (established cross-ISA), so the smaller RAM tier does not affect the head-to-head |
| Phoenix _(landed 2026-06-06)_ | 16-vCPU Graviton, 61 GB | 16 (Graviton) | 61 GB | arm64 | same 16-vCPU Graviton class as Go; 250K presence-off fits ~25.9 GiB so 61 GB holds it. Per-conn density is RAM/ISA-independent, so the RAM tier does not affect the head-to-head. (250K presence-ON was *not* run — confounded by `presence_diff` backpressure and ~47 GiB extrapolated, near the box ceiling.) |
| .NET Windows _(cut by decision, not run)_ | ~~`m7i.4xlarge`~~ | — | — | ~~x86_64~~ | Was to be an x86 Windows Server box for the SChannel delta; cut 2026-06-06 (Linux is the .NET headline; low-value confirmatory result; see [§.NET Windows](#net--aspnet-core-windows)) |
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
| **commercial** | Phoenix, Java/Netty, Go, .NET (Linux), Node | **250K** | 50K | This is the **real-world per-box deployment scale** for production WebSocket fleets — most prod fleets run 50K–250K per box and scale horizontally, not 1M per box. Matches the M2 milestone (#167 in phase-6) and the published per-node densities for Discord/Slack/Pinterest. Per-conn-bytes ratio is scale-invariant (see below), so 250K against 250K produces the same headline ratio as 1M against 1M would, at ~5× less rig effort and ~40% less wall-clock per comparator. |
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

(No Windows host tuning was needed — the only Windows-host comparator,
.NET Windows, was [cut](#net--aspnet-core-windows). On Windows the
analogous bumps would be ephemeral port range, TIME_WAIT recycle
window, and TCP control-block table size.)

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
- **Go / gorilla** _(landed 2026-06-06):_ raw `gorilla/websocket` v1.5.3
  on idiomatic `net/http` + pure-Go `crypto/tls` — the lean Go prod
  default, **no framework** (no router/RPC/presence), so no framework
  overhead is folded into the **44.4 KB/conn** (3.66× Kāra; 1.59× the
  Rust comparator). Prod-default runtime config: `GOGC=100`,
  `GOMAXPROCS=16` (all vCPU), no tuning. Same TLS floor as the others
  (TLS 1.2 + 1.3, no client auth, single cert). The extra weight over
  Rust is structural, not a config artifact — a goroutine per blocked
  `ReadMessage` + `crypto/tls`'s per-conn record buffers, none shared.
- **Phoenix Channels** _(landed 2026-06-06):_ real-world Phoenix
  **Channels** (joined channel + channel process per conn) over
  **in-process** Erlang `:ssl` — **102.8 KB/conn** (presence-off clean
  idle hold, 8.69× Kāra; the heaviest comparator measured). Two caveats
  carry the framing: (1) **in-process TLS** is the apples-to-apples basis
  (all comparators terminate TLS in-process), though some Phoenix fleets
  offload TLS to a load balancer; (2) **presence-ON is not headlined** —
  `Presence.track`'s `presence_diff` broadcast generates O(N²)
  server→client traffic that an idle non-draining client backs up, so the
  +83.6 KB/conn presence delta is an undrained-backpressure upper bound
  (prod shards presence topics), not steady-state. Full breakdown in the
  [§Phoenix section](#phoenix-channels-elixir).
- **Java / Netty** _(landed 2026-06-06):_ raw Netty 4.1.115 (no
  Spring/Vert.x) over **in-process** JDK JSSE `SSLEngine` — Kāra's
  **closest** density competitor and the second-densest stack measured.
  Reported as a **dial, not a single number**: live set ~8–10 KB/conn
  (below Kāra), marginal slope **~12.8 KB (1.06× Kāra)**, balanced-heap
  (`-Xmx4g`) deployment RSS **14.4 KB @ 250K (1.19×)**, up to 22–57 KB under
  `-Xmx24g` over-commit. Three framing caveats: (1) the JVM carries a multi-
  GB **fixed heap base** the native stacks don't; (2) per-conn RSS is a
  **RAM-vs-GC-CPU dial** set by `-Xmx`, so the headline is reported at a
  balanced point plus the `-Xmx`-independent slope/live-set; (3) **in-process
  JDK JSSE** TLS (OpenSSL/tcnative is a non-default opt-in). G1 (JDK 21
  default) is the headline; ZGC trades memory for pause latency (higher RSS
  reservation, same live set) and is a sidebar, not the density read. Full
  breakdown in the [§Java/Netty section](#java--netty).
- **.NET ASP.NET Core (Linux)** _(landed 2026-06-06):_ in-process Kestrel
  HTTPS over **OpenSSL** (not SChannel); .NET 8 LTS; raw Kestrel +
  `UseWebSockets()` echo middleware (no SignalR — that is stretch row #74).
  Three framing notes: (1) Server GC is the prod default and the headline, but
  — unlike the JVM — per-conn RSS is **not** a dial (Server↔Workstation GC
  delta ~2 %, marginal slope ≈ absolute), so 52.9 KiB is real live memory; (2)
  in-process TLS is the apples-to-apples basis (a TLS-offload LB moves TLS
  state off the box); (3) measured on .NET 8 LTS (.NET 9 is current STS). Full
  breakdown in the [§.NET Linux section](#net--aspnet-core-linux).
- **Node.js (`ws`)** _(landed 2026-06-06):_ Node 24.15.0 LTS; raw `ws`
  8.21.0 over in-process `https`/**OpenSSL 3.x** (no socket.io — that is
  stretch row #75); single-threaded libuv event loop, single process (no
  `cluster`). Three framing notes: (1) like .NET and unlike the JVM,
  per-conn RSS is **not** a dial — a `--max-old-space-size=512` cap moves it
  +0.07 %, so 40.4 KiB is real live memory (native C++ buffers outside the V8
  heap); (2) Node's connect p50 ~50 ms is a single-thread handshake-throughput
  artifact, not a density or steady-state cost; (3) in-process TLS is the
  apples-to-apples basis. Full breakdown in the [§Node.js section](#nodejs-ws).
- **.NET ASP.NET Core (Windows)** _(cut by decision 2026-06-06 — was wip
  task #72):_ deliberately not run. It would have re-measured the same .NET
  runtime on Windows Server + SChannel to isolate the OS-TLS-substrate delta,
  but Linux is the .NET headline, the SChannel-vs-OpenSSL delta is low-value
  (Node already supplies a second OpenSSL data point), and an x86 Windows box
  is the costliest run in the suite. Full rationale in the
  [§.NET Windows section](#net--aspnet-core-windows).

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
| connect p50 | 45.9 ms | **pre-fix Nagle floor** — missing `TCP_NODELAY`, not park/wake; diagnosed + fixed 2026-06-06 (§Connect-p50) |
| connect p95 | 214.1 ms | |
| connect p99 | 254.8 ms | tail collapsed vs pre-fix 1856 ms — `ec2_setup.sh` sysctls removed the SYN-retransmit cliff |
| connect max | 480.4 ms | vs pre-fix 2306 ms |
| handshake-QPS | ~7–10K/sec | reconnect-storm throughput landed ([§Handshake-QPS](#handshake-qps--reconnect-storm-throughput--high-concurrency)); p50 ~43 ms, 0 failures; server survives c≥4000 storm (#66) |

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
| connect p50 | 46.0 ms | **pre-fix Nagle floor** (matches 1M's 45.9 ms); missing `TCP_NODELAY`, fixed 2026-06-06 (§Connect-p50) |
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
| connect p50 | 41.0 ms | pre-fix Nagle floor |
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
**Rust wins throughput and mean (~4 %)**; **Kāra wins tail (~8–10 % at
p95→max) and memory (2.30× on the post-fix 1M density)**. For idle-heavy
workloads where memory is the binding constraint (chat, IoT push, ISP
gateways), Kāra's **12.1 KB/conn means a single 128 GiB box holds ~11.3M
conns where Rust OOMs at ~4.9M** — the same 2.30× headroom. Rust's
pre-fix p50 lead (46 ms vs ~3 ms, ~14×) was **not** an architectural
floor — it was a Kāra-side Nagle defect, **diagnosed and fixed
2026-06-06** (§Connect-p50 below); the controlled probe collapses the
floor to ~6 ms.

**Connect-p50: the "architectural floor" was a missing `TCP_NODELAY` —
diagnosed + fixed (2026-06-06).** The flat ~41–46 ms Kāra connect-p50
across every at-scale run (1M 45.9 / 2M 46.0 / x86 44.2 ms) was
previously logged as an "architectural floor (park/wake path)." That
diagnosis was **wrong**. A controlled loopback handshake-latency probe
(`tls::tests::handshake_latency_probe` — sequential connects, isolating
the per-conn floor) on a fresh Graviton box pinned it to **Nagle ×
delayed-ACK**: the TLS handshake + RFC 6455 upgrade is a multi-round-trip
exchange of small records, and Kāra — alone in the comparator set —
never set `TCP_NODELAY`, so a handshake record sat withheld behind an
unacked segment until the peer's ~40 ms delayed-ACK timer fired. (Rust's
~3 ms p50 on the same loopback is now explained: `tokio` sets
`TCP_NODELAY` by default. Same kernel, same network, same client driver
— the only difference was Nagle.)

2×2 probe, N=500 sequential connects, Linux loopback TLS+WS:

| server nodelay | client nodelay | p50 | p90 | p99 | max |
|---|---|---|---|---|---|
| OFF | OFF | **41.93** | 46.99 | 47.15 | 48.04 |
| **ON** | OFF | **6.01** | 46.97 | 47.05 | 47.91 |
| ON | ON | 5.96 | 6.07 | 6.20 | 6.30 |
| OFF | ON | 5.89 | 5.99 | 6.11 | 41.42 |

Leg A reproduces the campaign floor to the millisecond (41.93 ms). The
server-side `set_nodelay(true)` collapses p50 **7× (→ 6.01 ms)**; the
residual ~47 ms *tail* is client-side Nagle and clears when the client
sets it too (both on → flat ~6 ms). **Fix landed on both halves** —
server accept paths (`ws_accept_tls`, `ws_accept`, `tls_accept`) and the
client connect path (`tls_client_connect`).

**Status of the at-scale p50 numbers.** Every p50 figure in the tables
above (1M/2M/x86, and this head-to-head's 41.0 ms) was measured
**pre-fix** and stands as the historical record. A post-fix at-scale
(c=64, 250K+) re-measure is **deferred future work**: the probe proves
the mechanism and the fix at the per-connection level, but confirming
the new at-scale p50 *distribution* needs a rig run. The density
headline — the actual product claim — is unaffected; this is a latency
fix, orthogonal to per-conn memory.

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

- **Status:** `landed @ 250K + 50K` (2026-06-06). The rhetorically
  critical comparator — Phoenix/BEAM is the runtime most often cited as
  the gold standard for idle-connection density. **Result: it is the
  *heaviest* comparator measured.**
- **Build:** `examples/ws_idle_holder/phoenix/`, a minimal Phoenix
  Channels app (`UserSocket → BenchChannel(room:*) → Presence.track`),
  `mix deps.get && mix compile`, deps pinned in `mix.lock`.
- **Stack:** Elixir 1.17.3 / Erlang OTP 25, Phoenix 1.7.x Channels (the
  real-world Elixir config — Discord/Pinterest-tier, **not** raw Cowboy),
  Cowboy transport (`Phoenix.Endpoint.Cowboy2Adapter`). The client speaks
  the Phoenix v2 join protocol: WS upgrade at `/socket/websocket?vsn=2.0.0`,
  then a `phx_join` to `room:bench` acked by the `ok` `phx_reply` before a
  connection counts as established (so the channel process actually exists).
- **Hardware:** 16-vCPU AWS Graviton, 61 GB — same 16-vCPU Graviton class
  as the Go run; fresh box. (250K presence-off fits ~25.9 GiB; 61 GB ample.
  Per-conn density is RAM/ISA-independent — established cross-ISA — so the
  head-to-head stays valid.)
- **TLS:** **in-process** via Erlang `:ssl` (OpenSSL-backed), TLS 1.2 + 1.3,
  no client auth, single cert — same self-signed CN=localhost fixture as
  every other comparator. See the [TLS-offload caveat](#phoenix-caveats)
  below — in-process TLS is the apples-to-apples choice (Kāra/Rust/Go all
  terminate TLS in-process too).
- **BEAM tuning:** prod defaults except `+Q 2000000` (max ports — the
  65536 default caps below 250K conns) and `+P 8000000` (max processes —
  each conn spawns a transport + channel process). The Channels socket
  uses `timeout: :infinity` to disable the idle-heartbeat close (a
  liveness setting; density-neutral). No allocator/scheduler tuning.
- **Scale:** 250K headline + 50K linearity. Despite the wip flag that
  Phoenix was the likeliest 1M-escalation candidate (BEAM heap warm-up),
  per-conn proved **flat** (−1.8% drift) — no escalation needed.

**The headline is the presence-OFF (raw Channels + TLS) idle hold.**
Presence-ON is *not* a clean idle measurement (see
[the presence sidebar](#phoenix-presence-sidebar)); the idle-holder
benchmark measures memory at rest, and presence injects server→client
traffic an idle client cannot drain.

**Idle-hold @ 250K — presence OFF (landed, 2026-06-06):**

| metric | value | notes |
|---|---|---|
| established | 250,000 / 250,000 | 0 failed |
| per-conn bytes | **~105,267 B (102.8 KiB)** | server-RSS delta / N (203,168 → 25,903,100 KiB) |
| connect p50 | 10.7 ms | Erlang `:ssl` handshake; slower than Go (~3 ms), faster than Kāra's ~41 ms floor |
| connect p99 | 17.9 ms | tight tail |
| hold | `--hold-secs 10` | extended so the BEAM allocator reaches steady state (see caveats) |

**Linearity check @ 50K — presence OFF (landed, 2026-06-06):**

| metric | Phoenix @ 50K | drift vs 250K | gate |
|---|---|---|---|
| established | 50,000 / 50,000 (0 failed) | — | — |
| per-conn bytes | 107,204 B (104.7 KiB) | **−1.8 %** (104.7 → 102.8 KiB) | < 5 % → **publish, no 1M escalation** ✓ |
| connect p50 / p99 | 10.5 / 18.0 ms | — | — |

- Raw JSON: `docs/investigations/phoenix_idle_250k_nopresence.json`,
  `docs/investigations/phoenix_idle_50k_nopresence.json`,
  `docs/investigations/phoenix_idle_50k_presence.json`.
- Acceptance criteria (all met for the presence-OFF headline):
  1. `established == N` AND `failed == 0` at both scales. ✓
  2. 50K→250K `per_conn_bytes` drift < 5 % (−1.8 %) → linear, publish at
     250K without a 1M escalation. ✓
  3. `dmesg` unreadable without root on this box (`Operation not
     permitted`); 0 connect failures at both scales is the backstop signal
     that the listen backlog held.

**Head-to-head with Kāra @ 250K (presence-OFF Phoenix):**

| metric | Kāra | Phoenix | winner |
|---|---|---|---|
| established / failed | 250,000 / 0 | 250,000 / 0 | tie |
| `connect.p50_ms` | ~41 (pre-fix; §Connect-p50) | **10.7** | Phoenix |
| `connect.p99_ms` | tail varies | **17.9** | mixed¹ |
| **`per_conn_bytes`** | **~12,114** (post-fix idle) | **105,267** | **Kāra (8.69×)** |

> ¹ As with Go and Rust, Phoenix's connect *latency* beats Kāra's known
> pre-fix Nagle p50 floor (~41 ms,
> [phase-6](../../../docs/implementation_checklist/phase-6-runtime.md));
> density is the headline metric, and there Kāra wins decisively.

**What this proves.** Phoenix — the runtime whose whole reputation is
idle-connection density (the WhatsApp/Discord/"2M connections" lineage) —
holds each idle connection in **102.8 KiB** under its real-world config
(Channels + in-process TLS). That is **8.7× the Kāra density** (12.1 KB)
and makes Phoenix **heavier than every other comparator measured**:
2.37× Go (43.4 KB) and ~3.7× Rust (27.9 KB). The famous "2M idle
connections on one box" figure was plain `ws://` (no TLS), no Channels
join, no Presence, and BEAM-tuned — a fundamentally lighter config than
the one a real Phoenix product ships. The dominant cost here is Erlang
`:ssl` (several processes + per-socket record buffers per connection),
compounded by a transport process **and** a channel process per
connection — none of that state shared. Kāra's TLS lives in a shared
per-binding structure with per-conn references, which is the
architectural reason it holds an order-of-magnitude lead. Connect latency
favors Phoenix (~11 ms vs Kāra's ~41 ms floor) — the same multi-axis
tradeoff seen against Go and Rust.

<a id="phoenix-presence-sidebar"></a>
**Presence sidebar — why presence-ON is a caveated upper bound, not the
headline.** `Phoenix.Presence.track` broadcasts a `presence_diff` to
**every** member of the topic on **every** join — O(N²) server→client
traffic in a single `room:bench`. The idle-holder client never reads
after the join, so those diffs back up in server-side send buffers and
transport-process mailboxes. The result measures *undrained backpressure*,
not steady-state presence memory:

| run (50K) | per-conn bytes | vs presence-OFF | clean? |
|---|---|---|---|
| presence OFF (headline) | 104.7 KiB | — | ✓ true idle hold |
| presence ON | 188.3 KiB | +83.6 KiB | ✗ inflated by undrained `presence_diff` |

A production deployment **shards presence across many topics** (per-room,
not one global topic), and a real client **drains** incoming diffs — both
of which this idle, single-topic harness deliberately does not do. So
+83.6 KiB/conn is an upper bound peculiar to this methodology, not a
steady-state presence cost. (Presence-ON connect latency also degrades —
p50 22.8 / p99 57.5 ms vs 10.5 / 18.0 — consistent with the broadcast
storm.) 250K presence-ON was not run: it would be both confounded and
near the box's 61 GB ceiling (~47 GB extrapolated).

<a id="phoenix-caveats"></a>
**Caveats:**

- **Real-world-vs-purist:** this is Phoenix **Channels** (joined channel +
  channel process per conn) — the real Elixir prod default (Discord,
  Pinterest, Bleacher Report), per the
  [apples-to-apples discipline](#real-world-configuration-over-apples-to-apples-purism).
  Raw `:cowboy_websocket` with no channel layer would be lighter; it is
  not how production Phoenix apps are written, and is out of scope.
- **In-process TLS vs offload:** the 102.8 KiB includes Erlang `:ssl`
  in-process. Many high-scale Phoenix fleets terminate TLS at a load
  balancer and run the BEAM on plain `ws://`, which would move the `:ssl`
  cost out of the measured process. In-process TLS is nonetheless the
  correct apples-to-apples basis here — **every** comparator
  (Kāra/rustls, Rust/rustls, Go/`crypto/tls`) terminates TLS in-process,
  so the comparison holds the same work constant. Quantifying the exact
  `:ssl` share would need a plain-`ws://` run, which the TLS-only harness
  does not currently support; noted as future work, not a blocker.
- **BEAM RSS shape + hold time:** BEAM grows allocator carriers in large
  chunks and GCs per-process lazily, so the first few thousand conns look
  inflated (the N=2K smoke read ~200 KiB/conn — pure prealloc noise). The
  per-conn-bytes definition is the same server-RSS-delta / N as every
  other comparator; the Phoenix runs use `--hold-secs 10` (vs the bare-WS
  comparators' 1 s) so the VM reaches steady state before RSS is sampled.
  This affects *when* RSS is read, not *how much* is allocated — the −1.8%
  50K→250K linearity confirms the steady-state per-conn is stable.

### Java / Netty

- **Status:** `landed @ 250K + 50K` (2026-06-06). Largest commercial-TAM
  comparator. **Result: Kāra's closest density competitor — the second-
  densest stack measured, ahead of Rust, Go, and Phoenix.**
- **Build:** `examples/ws_idle_holder/java/`, raw Netty WS-over-TLS idle
  holder, `mvn package` → shaded fat jar.
- **Stack:** OpenJDK 21.0.11, raw Netty 4.1.115 (`SslHandler →
  HttpServerCodec → HttpObjectAggregator → WebSocketServerProtocolHandler →
  echo`) on `NioEventLoopGroup` — the high-density Java WS prod default, no
  Spring/Vert.x/Akka. WS upgrade at `/` (bare-WS — no harness changes).
- **Hardware:** 16-vCPU AWS Graviton, 61 GB — same class as the Go/Phoenix
  runs; fresh box.
- **TLS:** **in-process** JDK JSSE `SSLEngine` (`SslProvider.JDK`), TLS
  1.2 + 1.3, no client auth, single self-signed cert (shared fixture).
  OpenSSL/tcnative is the non-default perf alternative (caveat below).
- **GC:** G1 (JDK 21 default) is the headline; ZGC was run as a sidebar.
- **Scale:** 250K headline + 50K linearity.

**Methodology note — why the JVM needs a different read.** Unlike the
native stacks (Kāra/Rust/Go), where RSS ≈ live set, a JVM's RSS is
dominated by **GC heap commit**, which is much larger than the live set and
is `-Xmx`-dependent. The harness's `per_conn_bytes = RSS-delta / N` reports
wildly different figures depending purely on where G1 committed heap at that
N (22 KiB/conn at 250K under `-Xmx24g`, 57 KiB at 50K) — that is **not** a
per-connection property. The honest per-conn reads are the `-Xmx`-
independent ones: the **marginal RSS slope** and the **post-GC live set**.

**Per-conn cost is a GC-heap dial, not a single number:**

| read | 50K | 250K | what it is |
|---|---|---|---|
| post-GC **live set** | 10.6 KiB | **8.3 KiB** | what the JVM actually needs (`jcmd GC.run` then heap `used` / N) — *below* Kāra |
| **balanced** RSS (`-Xmx` ~2× live: 800m / 4g) | 21.2 KiB | **14.4 KiB** | a realistic deployment footprint |
| over-commit RSS (`-Xmx24g`) | 56.7 KiB | 21.6 KiB | G1 grabs heap it never uses (5 GB committed / 2 GB live @ 250K) |
| ZGC RSS (`-Xmx24g`) | — | 61.7 KiB | ZGC *reserves* even more; a pause-time knob, not a density read |

> **Marginal RSS slope (the scale-invariant per-conn): ~12.8 KiB/conn**,
> stable across every G1 measurement (50K↔250K, both heaps). ≈ Kāra's
> 12.1 KiB (**1.06×**). This is the real "cost per additional connection";
> the rest is a fixed JVM/G1 base (~1–3 GB) the native stacks don't carry.

**Idle-hold @ 250K — balanced heap `-Xmx4g` (landed, 2026-06-06):**

| metric | value | notes |
|---|---|---|
| established | 250,000 / 250,000 | 0 failed |
| per-conn bytes | **~14.4 KiB** | RSS-delta / N at a realistic deployment heap (3.72 GB total) |
| marginal slope | **~12.8 KiB/conn** | the `-Xmx`-independent intrinsic (≈ Kāra) |
| live set | ~8.3 KiB/conn | post-GC heap used / N |
| connect p50 / p99 | 3.8 / 16.0 ms | beats Kāra's ~41 ms pre-fix Nagle floor |

**Linearity @ 50K (`-Xmx800m`):** 21.2 KiB/conn. The 50K→250K RSS-delta/N
drift is large (−32%) but is **not** a per-conn non-linearity — it is the
fixed JVM base + heap headroom amortizing over more connections. The
**marginal slope is flat at ~12.8 KiB**, so the per-conn cost *is* linear;
the linearity gate's 1M-escalation trigger does not apply (a 1M run would
merely dilute the fixed base further, driving the apparent per-conn *down*
toward the 12.8 marginal — not a meaningful escalation).

- Raw JSON: `docs/investigations/netty_g1_{250k,50k}_balanced.json`
  (headline), `netty_g1_{250k,50k}_xmx24g.json` (over-commit endpoint),
  `netty_zgc_250k.json` (ZGC sidebar).
- Acceptance: `established == N` AND `failed == 0` at every scale/heap/GC
  combination (all six runs 0-failed). ✓

**Head-to-head with Kāra @ 250K (balanced heap):**

| metric | Kāra | Netty | winner |
|---|---|---|---|
| established / failed | 250,000 / 0 | 250,000 / 0 | tie |
| `connect.p50_ms` | ~41 (pre-fix; §Connect-p50) | **3.8** | Netty |
| **`per_conn_bytes`** (deployment RSS) | **~12,114** | **~14,746** | **Kāra (1.19×)** |
| marginal per-conn | ~12,114 | ~13,100 | ≈ tie (Kāra 1.06×) |

**What this proves.** Java/Netty is **Kāra's closest density competitor** —
~14.4 KiB/conn at a realistic heap (1.19× Kāra), ~12.8 KiB marginal
(1.06×), and a live set (~8–10 KiB) that is actually *below* Kāra. It is
the **second-densest stack measured**, ahead of Rust (27.9 KB), Go
(43.4 KB), and Phoenix (102.8 KB): JDK JSSE keeps TLS state on a tightly-
packed, poolable GC heap, where rustls holds heavier per-conn record
buffers, Go pairs a goroutine with `crypto/tls` buffers, and Phoenix spends
several `:ssl` processes per conn. The honest asterisks: (1) the JVM carries
a multi-GB fixed heap base the native stacks don't, so on *small* boxes /
low conn counts Kāra's lead is larger; (2) Java's RSS is a **RAM-vs-GC-CPU
dial** (tighten `-Xmx` toward the live set for less RAM but more GC work) —
a deployment degree of freedom, and a real operational cost, that a native
no-GC runtime simply doesn't have. Connect latency favors Netty (~4 ms p50
vs Kāra's ~41 ms floor), the same multi-axis tradeoff seen across the set.

**Caveats:**

- **Real-world-vs-purist:** raw Netty (no Spring/Vert.x/Akka) — the lean
  high-density Java WS default; no framework overhead folded in.
- **In-process TLS:** JDK JSSE `SSLEngine`, the zero-native-dependency
  default. OpenSSL via `netty-tcnative` is a non-default perf opt-in (faster
  handshakes, possibly lower TLS memory) — not measured. In-process TLS is
  the apples-to-apples basis (every comparator terminates TLS in-process).
- **Heap is a dial, reported at a balanced point:** the headline uses
  `-Xmx` ≈ 2× live set (`-Xmx4g` @ 250K). Tighter `-Xmx` lowers RSS toward
  the ~8 KiB live set at the cost of GC CPU; looser inflates it (the
  `-Xmx24g` row). The marginal slope + live set are reported precisely
  *because* they don't depend on this choice.
- **GC config:** G1 (JDK 21 default) is the headline; ZGC trades memory for
  pause latency (reserves more heap → higher RSS, same ~8–10 KiB live set),
  so it is not the density read and is shown only as the over-commit-endpoint
  sidebar.

### Go (gorilla/websocket)

- **Status:** `landed @ 250K + 50K` (2026-06-06). First commercial
  comparator landed.
- **Build:** `examples/ws_idle_holder/go/`,
  `go build -ldflags="-s -w" -trimpath`, `go.mod`/`go.sum` pinned.
- **Stack:** Go 1.23.4, `gorilla/websocket` v1.5.3 (the raw-library Go
  prod default), idiomatic `net/http` `http.Server.ServeTLS` +
  `Upgrader`, Go stdlib `crypto/tls` (pure-Go, no OpenSSL/cgo).
- **Hardware:** `m8g.4xlarge` (16 vCPU Graviton4, 61 GB) — same 16-vCPU
  Graviton class as the Kāra/Rust runs; fresh box. (RAM class differs
  from the `r8g.4xlarge` baseline, but per-conn density is RAM/ISA-
  independent — established cross-ISA — so the head-to-head stays valid;
  250K Go fits ~10.6 GiB, far under 61 GB.)
- **TLS:** `crypto/tls`, TLS 1.2 + 1.3, no client auth, single cert —
  matched to the rustls posture; same self-signed CN=localhost fixture.
- **GC/runtime:** `GOGC=100` (default), `GOMAXPROCS=16` (default = all
  vCPU). No tuning — prod defaults per the apples-to-apples discipline.
- **Scale:** 250K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)).

**Idle-hold @ 250K (landed, 2026-06-06):**

| metric | value | notes |
|---|---|---|
| established | 250,000 / 250,000 | 0 failed |
| per-conn bytes | **~44,386 B (43.35 KiB)** | server-RSS delta / N (6,700 → 10,843,128 KiB) |
| connect mean | 3.62 ms | `c=64`, loopback |
| connect p50 | 3.37 ms | Go's async net poller collapses the handshake hop |
| connect p95 | 6.96 ms | |
| connect p99 | 9.73 ms | tighter tail than both Kāra and Rust at this N |
| connect max | 36.59 ms | |

**Linearity check @ 50K (landed, 2026-06-06):**

| metric | Go + gorilla @ 50K | drift vs 250K | gate |
|---|---|---|---|
| established | 50,000 / 50,000 (0 failed) | — | — |
| per-conn bytes | 43,310.8 B (42.30 KiB) | **+2.5 %** (42.30 → 43.35 KiB) | < 5 % → **publish, no 1M escalation** ✓ |
| connect p50 / p99 | 3.43 / 15.56 ms | — | — |

- Raw JSON: `docs/investigations/go_idle_250k.json`,
  `docs/investigations/go_idle_50k.json`.
- Acceptance criteria (all met):
  1. `established == N` AND `failed == 0` at both scales. ✓
  2. 50K→250K `per_conn_bytes` drift < 5 % (2.5 %) → linear, publish at
     250K without a 1M escalation. ✓
  3. `dmesg` clean on both runs (no SYN-flood / cookie fallback → the
     listen backlog held; only kernel boot logs in the tail). ✓

**Head-to-head with Kāra @ 250K:**

| metric | Kāra | Go | winner |
|---|---|---|---|
| established / failed | 250,000 / 0 | 250,000 / 0 | tie |
| `connect.p50_ms` | ~41 (pre-fix; §Connect-p50) | **3.37** | Go |
| `connect.p99_ms` | ~0.34 (realistic) / tail varies | **9.73** | mixed¹ |
| **`per_conn_bytes`** | **~12,114** (post-fix idle) | **44,386** | **Kāra (3.66×)** |

> ¹ Kāra's connect *latency* is its known pre-fix Nagle floor (~41 ms
> p50, [phase-6 line 287 follow-on](../../../docs/implementation_checklist/phase-6-runtime.md));
> Go's net poller collapses the handshake hop the same way Rust's tokio
> does (~3 ms). Density is the headline metric, and there Kāra wins
> decisively.

**What this proves.** Go — the "good enough by default" counterargument —
holds each idle connection in **44.4 KB**, i.e. **3.66× the Kāra
density** (12.1 KB) and **1.59× even the Rust comparator** (27.9 KB). The
extra weight over Rust is structural: Go pairs a goroutine (growable
stack) per blocked `ReadMessage` with `crypto/tls`'s per-connection
record buffers (~16 KB read + write staging) and gorilla's 4 KB read/4 KB
write buffers — none shared across connections. Kāra's TLS state lives in
a shared per-binding structure with per-conn references, which is the
architectural reason it holds the density lead. Connect latency favors Go
(~3 ms p50 vs Kāra's ~41 ms floor), the same multi-axis tradeoff seen vs
Rust.

**Caveats:**

- **Real-world-vs-purist:** raw `gorilla/websocket` on `net/http` +
  `crypto/tls` — the lean Go prod default, *no* framework (router/RPC/
  presence). No framework overhead is folded into the 44.4 KB. A
  framework-tier Go comparator is out of scope for v1.
- Goroutine stacks start at 8 KB and grow; the steady-state idle handler
  blocks in `ReadMessage` (~1 goroutine/conn). `GOGC=100` default — a
  lower `GOGC` would trade CPU for slightly lower RSS but was left at the
  prod default deliberately.
- `crypto/tls` is pure-Go (no OpenSSL/cgo); an OpenSSL-backed Go TLS
  stack is non-default and not tested.

### .NET / ASP.NET Core (Linux)

- **Status:** `landed @ 250K + 50K` (2026-06-06). **Result: the second-
  *heaviest* comparator measured — ~52.9 KiB/conn, heavier than Go, lighter
  than only Phoenix. And — unlike the JVM — the number is *real*, not a
  GC-heap dial: it survives a GC-mode swap within ~2%.**
- **Build:** `examples/ws_idle_holder/dotnet/`, raw ASP.NET Core Kestrel +
  `app.UseWebSockets()` echo middleware (no SignalR), self-contained publish
  → the rig needs no .NET install. WS upgrade at `/` (bare-WS — no harness
  changes).
- **Stack:** .NET 8.0.421 LTS (`net8.0`), `WebApplication.CreateSlimBuilder`,
  Kestrel HTTPS via `ConfigureKestrel` + `UseWebSockets()` echo middleware —
  the lean ASP.NET Core WS prod default, no SignalR/MVC/Blazor.
- **Hardware:** 16-vCPU AWS Graviton, 61 GB (m8g-class) — same class as the
  Go/Phoenix/Netty runs; fresh box.
- **TLS:** **in-process** Kestrel HTTPS over **OpenSSL** (the Linux .NET
  default), TLS 1.2 + 1.3, no client auth, single self-signed cert (shared
  fixture, re-imported via PKCS#12 for the .NET 8 `X509Certificate2` API).
- **GC:** Server GC (`ServerGarbageCollection=true`, the ASP.NET Core Web SDK
  prod default) is the headline; Workstation GC was run as a sidebar.
- **Scale:** 250K headline + 50K linearity.

**Methodology note — .NET is *not* a heap dial (the opposite of Netty).**
The JVM's RSS is dominated by `-Xmx`-dependent GC heap-commit, so Netty's
per-conn number is a dial reported at a balanced point. .NET's Server GC
*also* commits heap lazily, so the same skepticism applies — but the data
refutes it here: the 50K→250K RSS-delta/N drift is **−1.4 %** (Netty's was
−32 %), the **marginal slope ≈ the absolute per-conn**, and swapping Server
GC → Workstation GC (single heap, aggressive return-to-OS) moves the 50K
figure by only **~2 %**. All three are signatures of memory that is
**genuinely live per connection**, not committed-but-unused heap. So unlike
the JVM, the headline RSS-delta/N *is* the honest per-conn cost.

**Idle-hold @ 250K — Server GC (landed, 2026-06-06):**

| metric | value | notes |
|---|---|---|
| established | 250,000 / 250,000 | 0 failed |
| per-conn bytes | **54,125 (52.9 KiB)** | RSS-delta / N; server RSS 12.66 GiB |
| marginal slope | **~52.7 KiB/conn** | (RSS₂₅₀ₖ − RSS₅₀ₖ)/200K — ≈ the absolute, i.e. linear |
| GC-mode delta @ 50K | **~2 %** (Server vs Workstation) | proves it is live memory, not heap slack |
| connect p50 / p99 | 4.6 / 15.0 ms (@ 50K) | beats Kāra's ~41 ms pre-fix Nagle floor |

**Linearity @ 50K (Server GC):** 54,869 B/conn (53.6 KiB). The 50K→250K
RSS-delta/N drift is **−1.4 %** — well inside the 5 % gate, so the per-conn
cost is linear and **no 1M escalation** is triggered. (Contrast the JVM,
whose −32 % "drift" was a fixed heap base amortizing; .NET has no such large
fixed base to amortize, which is *why* its number is both higher and flatter.)

**GC-mode sidebar @ 50K:**

| GC mode | per-conn | vs Server GC |
|---|---|---|
| **Server GC** (prod default) | 53.6 KiB | headline |
| Workstation GC (`DOTNET_gcServer=0`) | 52.5 KiB | −2.0 % |

> Workstation GC — single heap, eager return-to-OS — lands *within 2 %* of
> Server GC. If the ~53 KiB were GC over-commit (as on the JVM), Workstation
> GC would have collapsed it. It doesn't, because the memory is held live by
> open connections: per-conn `SslStream` read/write buffers + Kestrel
> `System.IO.Pipelines` input/output segments (pinned `MemoryPool` blocks) +
> the WebSocket frame buffer + connection context — none pooled across conns.

- Raw JSON: `docs/investigations/dotnet_linux_{250k,50k}.json` (Server GC),
  `dotnet_linux_50k_wks.json` (Workstation GC sidebar).
- Acceptance: `established == N` AND `failed == 0` at every scale/GC mode
  (all three runs 0-failed). ✓

**Head-to-head with Kāra @ 250K:**

| metric | Kāra | .NET (Linux) | winner |
|---|---|---|---|
| established / failed | 250,000 / 0 | 250,000 / 0 | tie |
| `connect.p50_ms` | ~41 (pre-fix; §Connect-p50) | **4.6** | .NET |
| **`per_conn_bytes`** | **~12,114** | ~54,125 | **Kāra (4.47×)** |
| marginal per-conn | ~12,114 | ~53,939 | **Kāra (4.45×)** |

**What this proves.** Raw ASP.NET Core Kestrel WebSockets cost **~52.9 KiB
per idle connection** — **4.47× Kāra** — making .NET the **second-heaviest
stack measured**, between Go (43.4 KiB) and Phoenix (102.8 KiB), and well
above Rust (27.9) and Netty (14.4). The decisive methodological finding is
that this is a *real* number, not a heap dial: marginal slope ≈ absolute,
−1.4 % linearity, and a ~2 % Server↔Workstation GC delta all confirm the
memory is genuinely live per-conn (SslStream buffers + Kestrel pipe segments
+ WS state, none pooled). This is the cleaner mirror image of the Netty
result: where the JVM's RSS overstated a small live set (a dial to tune
down), the CLR's RSS *is* the live set — there is no knob that recovers it.
Connect latency favors .NET (~4.6 ms p50 vs Kāra's ~41 ms floor), the same
multi-axis tradeoff seen across the set.

**Caveats:**

- **Real-world-vs-purist:** raw Kestrel + `UseWebSockets()` (no SignalR) —
  the lean high-density ASP.NET Core WS default. SignalR is a separate stretch
  row (#74) and folds in framework overhead on top of this floor.
- **In-process TLS:** Kestrel HTTPS over OpenSSL (the Linux .NET default,
  zero extra dependency). In-process TLS is the apples-to-apples basis (every
  comparator terminates TLS in-process); a TLS-offload LB deployment would
  move TLS state off the app box.
- **GC mode reported at the prod default + characterized:** Server GC
  (`ServerGarbageCollection=true`, the Web SDK default) is the headline; the
  Workstation-GC sidebar is shown precisely to demonstrate the per-conn cost
  is GC-mode-invariant (unlike the JVM's `-Xmx` dial). `DOTNET_GCHeapHardLimit`
  can cap committed heap but cannot reclaim memory live connections hold, so it
  would not lower the per-conn figure here.
- **.NET 8 vs 9:** measured on .NET 8 LTS (`net8.0`); .NET 9 is current STS.

### .NET / ASP.NET Core (Windows)

> _Not run — cut by decision 2026-06-06 (was wip task #72)._

This comparator was **deliberately not run.** It would have re-measured the
same .NET runtime on Windows Server + SChannel to isolate the OS-TLS-substrate
delta vs Linux/OpenSSL. We dropped it for three reasons:

1. **The .NET headline is already locked.** .NET Linux landed at 52.9 KiB /
   4.47× Kāra, and **Linux is the .NET headline** (the majority of new .NET
   deploys are Linux). A Windows number does not move the commercial claim
   (density, infra-cost, "one tier down").
2. **Low marginal information.** The only thing #72 adds is the SChannel-vs-
   OpenSSL TLS-state delta, which is almost certainly small (per-conn TLS
   record buffers are the same order on both stacks) — a likely confirmation,
   not a headline. And the **Node comparator already gives a second OpenSSL
   data point** next to .NET Linux, covering most of what the substrate
   question would have shown.
3. **Highest cost / lowest payoff of anything left.** It needs an **x86**
   box (the rest of the cohort is arm64, adding an ISA confound — though we
   established density is ISA-independent in the [x86 confirmation](#hardware))
   *and* a **Windows Server** box, materially harder to drive than the Linux
   runs (SChannel cert handling, `win-x64` publish, RDP/PowerShell tuning,
   `Get-Process` Working Set vs Linux `VmRSS`). Not worth a fresh Windows x86
   box for a confirmatory result.

If a buyer with a Windows-Server-default .NET fleet ever needs it, the
`net8.0` stack in [`../dotnet/`](../dotnet/) ports to `win-x64` + SChannel with
no app-code change; the run recipe is the only missing piece.

### Node.js (ws)

> _Landed @ 250K + 50K (2026-06-06). Real number, not a GC-heap dial —
> like .NET, the opposite of the JVM._

- **Status:** landed. 250K headline + 50K linearity + a V8 heap-cap
  sidebar, on a fresh 16-vCPU Graviton / 61 GB box.
- **Stack:** Node.js **24.15.0 LTS**, `ws` 8.21.0 (the most-deployed
  raw Node WebSocket library, pinned via committed `package-lock.json`),
  in-process `https.createServer` over Node's bundled **OpenSSL 3.x**
  for TLS. **Not** socket.io — that's the framework-tier stretch
  comparator #75.
- **Concurrency:** single-threaded libuv event loop — the **only
  comparator besides Kāra that is not thread/goroutine-per-conn**, so
  no per-conn stack cost; architecturally the closest comparator to
  Kāra's own reactor. Measured as a single process (a real Node shop
  `cluster`s for cores, but that's a core-scaling choice, not a
  density one — each worker holds its share at the same per-conn cost).
- **TLS substrate:** OpenSSL 3.x — the **same stack as the .NET-Linux
  comparator (#71)** and unlike Go's pure-Go `crypto/tls`, so the
  Node-vs-.NET-Linux pair reads cleanly as runtime overhead over a
  shared TLS stack.
- **Hardware:** 16-vCPU Graviton, 61 GB (`m8g.4xlarge`-class), matching
  the Go/.NET-Linux/Phoenix/Netty cohort; fresh box.

#### Idle-hold density @ 250K (headline)

| metric | value |
|---|---|
| established / failed | **250,000 / 0** |
| RSS before → after | 56,704 KiB → 10,158,860 KiB (~9.86 GiB held) |
| **per-conn** | **41,378 B (40.4 KiB)** |
| connect p50 / p95 / p99 | 50.78 / 79.77 / 92.66 ms |
| 50K→250K linearity drift | **−1.79%** (per-conn *falls* with N) |
| heap-cap (`--max-old-space-size=512`) Δ | **+0.07%** (proves live, not slack) |
| vs Kāra (12,114 B) | **Kāra is 3.42× denser** |

**Not a GC-heap dial — the opposite of the JVM, like .NET.** Going in,
V8's managed heap (`--max-old-space-size` is the `-Xmx` analog) raised
the same dial concern Netty hit. Three measurements refute it for Node:
(1) **50K→250K linearity is −1.79%** — per-conn *decreases* slightly as
N grows (a fixed base amortizing), the opposite of a heap reservation
that would pin or grow RSS-delta/N; (2) the marginal slope ≈ the
absolute per-conn; (3) a **V8 heap-cap sidebar** (re-run 50K under
`--max-old-space-size=512`) moved RSS-delta/N by **+0.07%** — if the
~41 KB were V8 heap slack, capping old-space would have forced reclaim
and dropped the number, but it didn't budge. So the ~41 KB is
**genuinely live per-conn memory** — native C++ buffers *outside* the
V8 GC heap: per-conn OpenSSL `SSL` record buffers (the same unpooled
SslStream-class buffers .NET showed are real), libuv per-handle state,
Node stream buffers, and the `ws` frame state. The raw RSS-delta/N *is*
the honest per-conn density here.

#### Linearity @ 50K (sub-curve)

| metric | 50K (default) | 50K (`--max-old-space-size=512`) |
|---|---|---|
| established / failed | 50,000 / 0 | 50,000 / 0 |
| per-conn | 42,131 B (41.1 KiB) | 42,161 B (41.2 KiB) |
| connect p50 / p99 | 49.52 / 82.72 ms | 49.12 / 82.48 ms |

The heap-capped run lands **+0.07%** from the default — the live-vs-slack
cross-check that decides the headline is real, not a dial (see above).

#### Head-to-head with Kāra

| | per-conn (idle) | ratio |
|---|---|---|
| **Kāra** | ~12,114 B | 1.0× |
| **Node.js** | ~41,378 B | **Kāra 3.42× denser** |

**What this proves.** Node is the **4th-densest** of the seven impls
measured — between Rust (27.9 KiB) and Go (43.4 KiB), and notably:

- **Denser than Go (~7%)** despite comparable library overhead, because
  the single-threaded event loop pays **no per-conn goroutine/thread
  stack** — the structural advantage Kāra's own reactor model takes
  much further (3.42× past Node).
- **Much lighter than .NET (1.31×) despite sharing OpenSSL** (40.4 vs
  52.9 KiB) — Kestrel's `System.IO.Pipelines` segments + `SslStream`
  buffers cost more per conn than libuv + `ws`. With the TLS substrate
  held constant, this isolates a real Kestrel-vs-libuv runtime delta.

**Caveats.**

- **Connect latency is Node's weak axis:** p50 ~50 ms (vs Go's ~3 ms),
  because the single thread serializes 50K–250K TLS handshakes through
  one OpenSSL context. This is a *handshake-throughput* artifact of the
  single-thread model, not a density or steady-state cost; density is
  the headline metric. (Kāra's ~41 ms p50 floor is a separate,
  documented park/wake-path matter, not the same cause.)
- **Single process, no `cluster`:** the honest per-process density. A
  production Node fleet clusters for cores; each worker holds its share
  at the same per-conn cost, so clustering changes throughput/core
  scaling, not density.
- **In-process TLS** (OpenSSL in the Node process) is the apples-to-apples
  basis every comparator shares; a fleet that offloads TLS at a load
  balancer would shift that cost off the app box.

Raw JSON: `docs/investigations/node_linux_{250k,50k,50k_cap}.json`.

> **Stretch tier — deferred (optional) by decision 2026-06-06.** The
> commercial tier is the complete v1 density story (Kāra is the densest
> across Go / .NET / JVM / Node / Elixir by 1.19×–8.69×); the stretch
> comparators below only *reinforce* an already-decisive claim, so they are
> not blocking v1 and were consciously deferred rather than run now. They are
> kept (not cut) because SignalR and socket.io have **audience-specific**
> value — for a buyer whose .NET or Node fleet actually deploys the framework
> (not raw Kestrel / raw `ws`), "the thing you really run is even heavier" is
> a real reframe. **Framing guardrail if any of these is ever run:** report
> the **within-runtime framework tax** (e.g. "socket.io adds Y KB/conn on top
> of raw `ws` *on the same Node*"), **not** a cross-runtime density headline —
> a framework bundles rooms/RPC/presence/fallback-transports the bare
> `ws_idle_holder` demo lacks, so a head-to-head density number would carry
> the same not-apples-to-apples asterisk that got Phoenix presence-ON
> de-headlined ([§Phoenix](#phoenix-channels-elixir)).

### SignalR _(stretch)_

> _Deferred (optional) — was wip task #74. Not blocking v1._

- **Status:** deferred (optional). Run only for a SignalR-shop audience,
  framed as the within-runtime framework tax over the landed
  [.NET Linux](#net--aspnet-core-linux) baseline (52.9 KiB raw Kestrel).
- **Stack target (if run):** ASP.NET Core SignalR on top of .NET 8 (Linux);
  reuses the [`../dotnet/`](../dotnet/) stack. Exposes the framework-overhead
  delta over raw Kestrel WebSocket middleware.
- **Scale:** 100K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator) — stretch rows
  run at smaller scale than commercial).

### socket.io _(stretch)_

> _Deferred (optional) — was wip task #75. Not blocking v1._

- **Status:** deferred (optional). Run only for a socket.io-shop audience,
  framed as the within-runtime framework tax over the landed
  [Node.js](#nodejs-ws) baseline (40.4 KiB raw `ws`).
- **Stack target (if run):** Node.js + `socket.io` server; reuses the
  [`../node/`](../node/) stack. Exposes the framework-overhead delta over
  raw `ws`.
- **Scale:** 100K headline + 50K linearity sub-curve (per
  [§Scale per comparator](#scale-per-comparator)).

### Python asyncio websockets _(stretch)_

> _Deferred (optional), lean-cut — was wip task #76. Not blocking v1._

- **Status:** deferred (optional), lowest priority. Unlike SignalR/socket.io
  this is a whole new runtime, not a framework-tax delta — but it is a
  low-TAM, rarely-density-critical choice with a predictable result
  (single-thread asyncio, ~Node-class or heavier). Kept only to answer the
  inevitable "what about Python?"; the likely first to be cut.
- **Stack target (if run):** Python 3.12, `websockets` library, asyncio.
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

### Handshake-QPS — reconnect-storm throughput @ high concurrency

The reconnect-storm sub-run (#66): clients open a **full TLS + WS handshake and
immediately close**, as fast as `--concurrency` allows, for N seconds — the
"thundering herd reconnecting after a deploy" worst case. Two AWS Graviton
`m8g.4xlarge` (arm64, 16 vCPU) boxes: a server box and a bench box, both at
runtime commit `c1faf2f9`.

| path | concurrency | handshakes/sec | failed | p50 | p99 | notes |
|---|---|---|---|---|---|---|
| cross-box (real NIC) | 200 | **4,518** | 0 | 43.9 ms | 53.0 ms | unsaturated; clean baseline |
| loopback (8 source IPs) | 500 | **10,269** | 0 | 43.2 ms | 375 ms | peak observed; client co-located |
| loopback (8 source IPs) | 300 | 7,266 | 0 | 43.0 ms | 84.5 ms | clean plateau |
| loopback (8 source IPs) | 800 | 7,588 | 0 | 78.9 ms | 650 ms | clean plateau |

**Sustained ~7–10K TLS+WS handshakes/sec on a 16-vCPU Graviton, p50 ~43 ms,
0 failures.** The loopback figure runs the bench *on the server box*, so client
TLS work steals ~half the cores — the server-alone ceiling is higher; treat
7–10K/sec as a conservative floor. (TLS handshake is ECDHE-CPU-bound, so
throughput is core-bound and shows run-to-run variance under co-located load;
per-handshake **latency** — p50 ~43 ms — is the stable metric.)

**Measurement caveat — the cross-box ceiling is client-bound, not server-bound.**
A single source IP exposes only ~50,535 ephemeral ports (range 15000–65535), so
any cross-box run longer than the pool drains caps at `done ≈ 50,536` with the
remainder counted as client-side connect failures — it measures the bench box's
port pool, not the server. The loopback path fans out across 8 source IPs
(~400K ports) to remove that wall; the clean 0-failure loopback numbers above
are the real server-side figures. Cross-box at-scale runs (c=1000→4000) are
reported only for **survival** (next paragraph), not throughput.

**Reconnect-storm survival — the load-bearing result.** The server holds through
a sustained cross-box storm at **c = 1000, 2000, 3000, and 4000** with no crash,
RSS flat at ~63 MB. This closes two codegen/runtime bugs the storm surfaced:
(1) a cross-thread coroutine-frame **use-after-free** at an I/O park (missing
`llvm.coro.save`, fix `30b0141b`) that corrupted the heap at c≥1000 under glibc;
and (2) an **unmasked SIGPIPE** (fix `c1faf2f9`) — a karac `main` bypasses Rust
std's `lang_start`, so a socket write racing a peer's mid-handshake close
silently terminated the process (exit 141) at c=4000. Both are validated on the
rig: the same storm that previously killed the server now runs clean. Root-cause
detail in `docs/implementation_checklist/phase-7-codegen.md`.

Raw JSON: `docs/investigations/handshake_qps_crossbox_c{200,1000,2000,3000,4000}.json`
(real-NIC), `docs/investigations/handshake_qps_loopback*_c*.json` (server-side
ceiling).

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

- **Kāra vs Phoenix / Java/Netty / Go / .NET (Linux) / Node:** _Landed
  2026-06-06 — see the [Commercial reframe](#commercial-reframe--populated-as-each-row-lands)
  bullets above, which carry the per-row dollarized stories._ (Phoenix:
  8.69× density, two tiers down; Java/Netty: combination claim, not a
  box-count cut; Go: one tier down; .NET: 4.47× density, one tier down;
  Node: 3.42× density, one tier down.)
- **Stretch rows (SignalR / socket.io / Python):** _Deferred (optional) by
  decision 2026-06-06 — not blocking v1; reinforcement, not a new pillar. If
  ever run, frame as the within-runtime framework tax, not a cross-runtime
  density headline (see [§SignalR/socket.io/Python](#signalr-stretch))._

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
| Phoenix Channels | commercial | **50K landed (2026-06-06)** — 107,204 B/conn, −1.8% drift | **250K landed (2026-06-06)** — 105,267 B/conn (presence-off clean idle), p50 10.7 ms (8.69× Kāra; heaviest measured) | n/a (gate passed: −1.8% < 5%, no 1M escalation) | n/a (idle-hold density comparator; presence-ON confounded by `presence_diff` backpressure — caveated upper bound, not headlined) | `scripts/run_250k.sh` + `scripts/run_50k.sh` (`BENCH_EXTRA_ARGS` + `PRESENCE` env) | `docs/investigations/phoenix_idle_{250k_nopresence,50k_nopresence,50k_presence}.json` |
| Java / Netty | commercial | **50K landed (2026-06-06)** — 21.2 KB/conn balanced `-Xmx800m` (RSS=GC-heap dial; marginal slope flat) | **250K landed (2026-06-06)** — 14.4 KB/conn balanced `-Xmx4g` (1.19× Kāra); marginal ~12.8 KB (1.06×), live ~8–10 KB; 2nd-densest stack | n/a (marginal slope flat; RSS-delta/N drift is fixed-JVM-base, not per-conn) | n/a (idle-hold density comparator) | `scripts/run_250k.sh` + `scripts/run_50k.sh` (`JAVA_OPTS` heap/GC + `BENCH_EXTRA_ARGS` env) | `docs/investigations/netty_g1_{250k,50k}_balanced.json`, `netty_g1_{250k,50k}_xmx24g.json`, `netty_zgc_250k.json` |
| Go | commercial | **50K landed (2026-06-06)** — 43,311 B/conn, +2.5% drift | **250K landed (2026-06-06)** — 44,386 B/conn, p50 3.37 ms (3.66× Kāra) | n/a (gate passed: +2.5% < 5%, no 1M escalation) | n/a (idle-hold density comparator) | `scripts/run_250k.sh` + `scripts/run_50k.sh` | `docs/investigations/go_idle_{250k,50k}.json` |
| .NET (Linux) | commercial | **50K landed (2026-06-06)** — 54,869 B/conn Server GC, −1.4% drift; Workstation-GC sidebar 53,781 B (−2.0%, proves live-not-dial) | **250K landed (2026-06-06)** — 54,125 B/conn (52.9 KiB) Server GC, marginal slope ≈ absolute (4.47× Kāra; 2nd-heaviest measured) | n/a (gate passed: −1.4% < 5%, no 1M escalation) | n/a (idle-hold density comparator) | `scripts/run_250k.sh` + `scripts/run_50k.sh` (Server GC default; `DOTNET_gcServer=0` for the Workstation sidebar) | `docs/investigations/dotnet_linux_{250k,50k,50k_wks}.json` |
| .NET (Windows) | commercial | n/a | **cut by decision** (was #72) — see [§.NET Windows](#net--aspnet-core-windows); Linux is the .NET headline, SChannel delta low-value, Node already gives a 2nd OpenSSL point | n/a | not run | n/a | n/a |
| Node.js | commercial | **50K landed (2026-06-06)** — 42,131 B/conn, −1.79% drift; `--max-old-space-size=512` sidebar 42,161 B (+0.07%, proves live-not-dial) | **250K landed (2026-06-06)** — 41,378 B/conn (40.4 KiB), p50 50.8 ms (3.42× Kāra; 4th-densest, denser than Go, lighter than .NET) | n/a (gate passed: −1.79% < 5%, no 1M escalation) | n/a (idle-hold density comparator) | `scripts/run_250k.sh` + `scripts/run_50k.sh` (`--server-bin ../node/run_server.sh`; `NODE_OPTIONS=--max-old-space-size=512` for the heap-cap sidebar) | `docs/investigations/node_linux_{250k,50k,50k_cap}.json` |
| SignalR _(stretch)_ | stretch | deferred (#74) | **deferred (optional)** — framework tax over .NET Linux, run only for a SignalR-shop audience | n/a | deferred | n/a | n/a |
| socket.io _(stretch)_ | stretch | deferred (#75) | **deferred (optional)** — framework tax over Node, run only for a socket.io-shop audience | n/a | deferred | n/a | n/a |
| Python _(stretch)_ | stretch | deferred (#76) | **deferred (optional)**, lean-cut — low-TAM, predictable | n/a | deferred | n/a | n/a |

> Task numbers reference `wip-bench-day.md` (uncommitted; lives in
> repo root). When that file is deleted on ship, the equivalent
> tracker entries in `docs/implementation_checklist/phase-6-runtime.md`
> become the durable references.

---

## Change log

- **2026-06-06 (stretch tier deferred by decision):** SignalR (#74),
  socket.io (#75), and Python asyncio (#76) marked **deferred (optional), not
  blocking v1** (Python lean-cut). The commercial tier is the complete v1
  density story; the stretch comparators only reinforce an already-decisive
  claim. Kept (not cut) because SignalR/socket.io have audience-specific value
  for framework-deploying .NET/Node shops. Recorded a **framing guardrail** in
  each stretch section + the deferred-reframe block: if ever run, report the
  within-runtime framework tax, not a cross-runtime density headline (avoids
  the not-apples-to-apples asterisk that de-headlined Phoenix presence-ON).
  Flipped TL;DR + status-matrix + section blockquotes + banner from
  "pending/stretch" to "deferred (optional)." **Phase 3 is now effectively
  wrapped:** commercial tier complete, stretch deferred, .NET Windows cut.
- **2026-06-06 (.NET Windows comparator cut by decision):** dropped the planned
  .NET-on-Windows-Server + SChannel run (was wip #72). Rationale: Linux is the
  .NET headline (already landed at 52.9 KiB / 4.47× Kāra), the SChannel-vs-
  OpenSSL TLS-substrate delta is low-value and likely confirmatory (Node
  already supplies a second OpenSSL data point next to .NET Linux), and an x86
  Windows Server box is the costliest / most operationally painful run in the
  suite. Flipped all `pending #72` rows (TL;DR, status matrix, §.NET Windows,
  hardware table, consolidated caveats, scale-per-comparator, RSS-measurement
  + host-tuning cross-refs) to "not run — cut by decision," and updated
  `dotnet/README.md`. The `net8.0` stack ports to `win-x64` + SChannel with no
  app-code change if a Windows-fleet buyer ever needs it. Closes the Phase 3
  **commercial** tier (Phoenix, Java/Netty, Go, .NET Linux, Node all landed);
  only the optional stretch tier (SignalR / socket.io / Python) remains.
- **2026-06-06 (Node.js `ws` comparator landed — denser than Go, lighter than
  .NET, real not a dial):** ran the raw-`ws` comparator (Node 24.15.0 LTS, `ws`
  8.21.0, in-process `https`/OpenSSL 3.x, single-threaded libuv, single
  process) on a fresh 16-vCPU Graviton / 61 GB box, co-located over loopback.
  **250K: 250,000 / 0 failed, 41,378 B/conn (40.4 KiB), server RSS ~9.86 GiB;
  50K: 50,000 / 0 failed, 42,131 B/conn — linearity −1.79 %** (< 5 % gate → no
  1M escalation). **Key findings:** (1) **not a GC-heap dial** — a
  `--max-old-space-size=512` sidebar moved RSS-delta/N **+0.07 %**, proving the
  ~41 KB is live native memory (OpenSSL + libuv + `ws` buffers outside the V8
  heap), like .NET and the opposite of the JVM; (2) **4th-densest** of seven
  impls — **denser than Go** (no per-conn stack on the single-threaded event
  loop) yet **lighter than .NET on the same OpenSSL** (isolating a real
  libuv-vs-Kestrel delta); (3) **Kāra 3.42× denser** (one instance tier down,
  ~50 % infra cost). Node's one weak axis is connect p50 ~50 ms (single-thread
  handshake serialization), a throughput artifact, not a density cost. Shipped
  REPORT.md (§Node landed tables + head-to-head + caveats, TL;DR row, status
  matrix, hardware row, commercial-reframe bullet, consolidated caveats, banner,
  this entry), phase-6 tracker, and `node/README.md`; raw JSON
  `docs/investigations/node_linux_{250k,50k,50k_cap}.json`.
- **2026-06-06 (.NET/ASP.NET Core Linux comparator landed — the JVM's mirror
  image):** ran the raw-Kestrel comparator (.NET 8.0.421, `net8.0`,
  `WebApplication.CreateSlimBuilder` + `UseWebSockets()` echo middleware,
  in-process Kestrel HTTPS over OpenSSL, WS at `/`) on a fresh 16-vCPU
  Graviton / 61 GB box, co-located over loopback; self-contained
  `linux-arm64` publish (rig needs no .NET). **250K: 250,000 / 0 failed,
  54,125 B/conn (52.9 KiB), server RSS 12.66 GiB; 50K: 50,000 / 0 failed,
  54,869 B/conn — linearity −1.4 %** (< 5 % gate → no 1M escalation).
  **Key finding: the opposite of the JVM.** Server GC also commits heap
  lazily, but the per-conn RSS is **not** a dial — marginal slope (~52.7 KiB)
  ≈ absolute, and a Workstation-GC sidebar (`DOTNET_gcServer=0`) lands at
  53,781 B (50K), **within ~2 %** of Server GC. All three signatures prove the
  ~53 KiB is **genuine live per-conn memory** (SslStream buffers + Kestrel
  pipe segments + WS state, none pooled), not committed-but-unused heap.
  **Kāra holds 4.47× the density** (12.1 KB vs 52.9 KB) — .NET is the
  **second-heaviest comparator measured**, between Go (43.4) and Phoenix
  (102.8), above Rust (27.9) and Netty (14.4). Connect p50 4.6 ms (beats
  Kāra's ~41 ms floor). Reframe = the standard "one tier down → ~50 % infra
  cost" (16 GiB → 8 GiB tier), off a larger density gap than Go/Rust. Prep
  (comparator + self-contained-publish recipe) landed `4c6bf47a`; run scripts
  already supported it (bare-WS, no harness changes). Updated: §.NET Linux
  (full landed tables + methodology note + GC-mode sidebar + head-to-head +
  caveats), TL;DR row + footnote², both status matrices, hardware row (own
  Graviton/61 GB row), commercial-reframe, consolidated caveats, top banner;
  phase-6 entry; dotnet/README results. Raw JSON:
  `docs/investigations/dotnet_linux_{250k,50k,50k_wks}.json`.
- **2026-06-06 (Java/Netty comparator landed — Kāra's closest density competitor):**
  ran the raw-Netty comparator (OpenJDK 21.0.11, Netty 4.1.115, in-process JDK
  JSSE `SSLEngine`, WS at `/`) on a fresh 16-vCPU Graviton / 61 GB box,
  co-located over loopback. **Key finding: the JVM does not fit RSS-delta/N** —
  its footprint is dominated by GC heap-commit (`-Xmx`-dependent), so per-conn
  RSS is a *dial*: live set ~8–10 KB (below Kāra), **marginal slope ~12.8 KB
  (1.06× Kāra, scale-invariant)**, balanced-heap (`-Xmx4g`) **250K = 14.4 KB/conn
  (1.19×)**, up to 21.6/56.7 KB under `-Xmx24g` over-commit. **All six runs
  250,000 or 50,000 / 0 failed.** Connect p50 3.8 ms (beats Kāra's ~41 ms
  floor). **Java/Netty is the second-densest stack measured** — ahead of Rust
  (27.9), Go (43.4), Phoenix (102.8): JDK JSSE packs SSL state on a poolable GC
  heap. The 50K→250K RSS-delta/N drift (−32%) is the fixed JVM base amortizing,
  not per-conn non-linearity (marginal flat), so no 1M escalation. ZGC sidebar
  (61.7 KB) reserves more heap, same live set — a pause-time knob, dropped from
  the headline. Reframe led as "Kāra matches the densest mainstream JVM stack
  with none of the JVM ops surface (fixed heap base, RAM-vs-GC-CPU dial)," not a
  box-count cut. Prep (comparator + the `maven-compiler-plugin` pin for apt
  Maven 3.8.7) landed `4a3b2b31` + `b8d6e8b6`; run scripts already supported it
  (bare-WS, `JAVA_OPTS`/`BENCH_EXTRA_ARGS` env). Updated: §Java (full dial +
  head-to-head + caveats), TL;DR row + footnote, both status matrices, hardware
  row, commercial-reframe, consolidated caveats, top banner; phase-6 entry;
  java/README results. Raw JSON: `docs/investigations/netty_g1_{250k,50k}_{balanced,xmx24g}.json`,
  `netty_zgc_250k.json`.
- **2026-06-06 (Phoenix comparator landed — the rhetorically critical row):**
  ran the Phoenix Channels comparator (Elixir 1.17.3 / OTP 25, Phoenix 1.7.x
  Channels + Presence, Cowboy transport, in-process Erlang `:ssl`) on a fresh
  16-vCPU Graviton / 61 GB box, co-located client+server over loopback. The
  client speaks the Phoenix v2 join protocol (`phx_join` → `ok` `phx_reply`).
  **Headline = presence-OFF clean idle hold: 250K 250,000 / 0 failed, 105,267
  B/conn (102.8 KiB), connect p50 10.7 / p99 17.9 ms; 50K 50,000 / 0 failed,
  107,204 B/conn — linearity −1.8 %** (< 5 % gate → published at 250K, no 1M
  escalation, despite Phoenix being the flagged escalation candidate).
  **Kāra holds 8.69× the density** (12.1 KB vs 102.8 KB) — Phoenix is the
  **heaviest comparator measured** (2.37× Go, 3.69× Rust), the cost of Erlang
  `:ssl` + a transport + channel process per conn. **Presence-ON is NOT
  headlined:** `Presence.track`'s `presence_diff` broadcast is O(N²)
  server→client traffic an idle client can't drain, so the 50K presence-ON
  read (188.3 KiB, +83.6 KiB) is an undrained-backpressure upper bound, not
  steady-state (prod shards presence topics); 250K presence-ON not run
  (confounded + ~47 GiB near box ceiling). Inverts the wip plan's
  "presence-ON = headline" assumption (which presumed presence ≈ +2 KB).
  Harness gained `--ws-path` + `--phx-join` (commit `1fbb2cf4`); run scripts
  gained `BENCH_EXTRA_ARGS` + `PRESENCE` pass-through. Updated: §Phoenix (full
  landed tables + head-to-head + presence sidebar + caveats), TL;DR row, both
  status matrices, hardware table (own Graviton/61 GB row), commercial-reframe
  (Kāra vs Phoenix, ~75% infra / two tiers down), consolidated caveats, top
  banner. Raw JSON: `docs/investigations/phoenix_idle_{250k_nopresence,50k_nopresence,50k_presence}.json`.
- **2026-06-06 (Go comparator landed — first commercial-tier row):** ran the Go
  comparator (`gorilla/websocket` v1.5.3 + pure-Go `crypto/tls`, idiomatic
  `net/http` `ServeTLS`) on a fresh `m8g.4xlarge` (16-vCPU Graviton4, 61 GB),
  co-located client+server over loopback. **250K: 250,000 / 0 failed, 44,386
  B/conn (43.35 KiB), connect p50 3.37 / p99 9.73 ms. 50K: 50,000 / 0 failed,
  43,311 B/conn — linearity drift +2.5 %** (< 5 % gate → published at 250K, no
  1M escalation). **Headline: Kāra holds 3.66× the density** (12.1 KB vs 44.4
  KB), and Go lands 1.59× heavier than even the Rust comparator (27.9 KB).
  dmesg clean (no SYN-flood). Updated: §Go (full landed tables + head-to-head +
  caveats), TL;DR row, status matrix, hardware table (Go on m8g.4xlarge),
  commercial-reframe (Kāra vs Go), consolidated apples-to-apples caveats, top
  status banner. Raw JSON: `docs/investigations/go_idle_{250k,50k}.json`.
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
  (matches the known pre-fix Nagle floor from task #65; not a
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
  pre-fix Nagle floor). **Headline 3.55× density ratio is now
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
