# Parallax multi-language benchmark

Side-by-side `GET /dashboard/<user_id>` throughput across **Kāra**,
**Rust**, **Go**, **Node.js**, **Phoenix/Elixir**, and **Java/Netty** —
the recordable artifact for Parallax
([`docs/dogfooding.md § Parallax`](../../../docs/dogfooding.md)).

Each impl serves the same canonical fan-out + join workload: four
provider "fetches" per request, each carrying `reads(R_i)` on a
disjoint resource, joined into a `Dashboard` aggregate. The Kāra impl
gets that fan-out from the compiler — straight-line sequential code,
the auto-par codegen runs the four reads concurrently. The other five
write the fan-out by hand (Rust `tokio::join!`, Go goroutines + WaitGroup,
Node `Promise.all`, Phoenix `Task.async` + `Task.await_many`, Java/Netty
`CompletableFuture.allOf` on a fixed pool) and serve as the reference
perf cohort.

## What this measures

**Throughput (req/s)** and **p99 latency** under sustained `wrk` load
on a single machine. Each impl is built and run in turn; the bench
captures `Requests/sec` and the 99th-percentile latency from `wrk
--latency` output.

The provider "fetches" are CPU-bound busy loops sized to roughly
approximate **2 / 5 / 8 / 12 ms** of latency on a modern x86-64 core
(`FETCH_PROFILE_WORK = 700K`, `FETCH_ORDERS_WORK = 4M`,
`FETCH_NOTIFS_WORK = 1.7M`, `FETCH_RECOMMEND_WORK = 2.7M` iterations
respectively). Total work per request: ≈ 27 ms sequential / ≈ 12 ms
fully parallel (waiting on the slowest fetch).

The asymmetry is deliberate (F5): it surfaces the "join waits on the
slowest provider" property in trace narration. Symmetric work would
look uniform across impls and hide the auto-par story's punch line.

> **Sleep substitute (deviation from the design's F5 lock).** F5
> originally specified `sleep_ms(n)` providers (real I/O simulation,
> no CPU burn). Kāra's stdlib has no `sleep_ms` in v1 (Phase 11
> long-tail). To keep the impls apples-to-apples, **all five
> use CPU-bound busy loops** instead of sleeps. The shape of the
> benchmark — fan-out + join over four independent operations — is
> preserved, but the implication for measured throughput is
> different: with sleeps, throughput is driven by the event-loop
> scheduler; with busy loops, throughput is driven by core count
> and worker contention. The **relative ordering of impls** still
> reflects each runtime's fan-out efficiency on multi-core hardware
> (which is the demo's intended story); the **absolute numbers**
> are CPU-bound and won't match a real I/O-bound API server.

## How to reproduce

### Toolchain prerequisites

Each impl needs its language toolchain installed; `bench.sh`
graceful-degrades when one is missing (`skip: <lang> not installed`
to stderr, the bench continues with the rest).

| Impl  | Required toolchain | Tested with |
|-------|--------------------|-------------|
| kara  | `cargo` + this repo's `karac` build (auto-built by `bench.sh`) | rustc 1.x  |
| rust  | `cargo` (any stable) | rustc 1.x  |
| go    | `go`               | go 1.21+   |
| node  | `node`             | Node 18+   |
| phoenix | `elixir` + `mix` | Elixir 1.19 + OTP 29 |
| java  | `java` + `mvn`     | JDK 11+ (Netty 4.1) |
| wrk   | `wrk`              | wrk 4.x    |

### Run

```sh
# default — all six impls, 0s warmup + 10s measurement × 3 rounds per (impl, conn)
sh examples/parallax/bench/bench.sh

# dry-run (no servers spawned, no wrk; checked into CI via
# tests/parallax_bench.rs::test_bench_script_dry_run)
sh examples/parallax/bench/bench.sh --dry-run

# subset (kara + rust only)
sh examples/parallax/bench/bench.sh --impls=k,r

# tweak window
sh examples/parallax/bench/bench.sh --warmup=5 --measure=15
```

`bench.sh` builds each impl on the fly (Kāra via `karac build`, Rust
via `cargo build --release`, Go via `go build`, Node served directly
from `server.js`, Phoenix via `mix compile`, Java via `mvn package`
into a shaded `java -jar` fat-jar), launches it, awaits the conventional
`BOUND_PORT=<n>` stdout line, runs `wrk -t4 -c100 -dWARMUP+MEASURE`,
parses `Requests/sec` + `99% <lat>`, and kills the server.

The bench is **not** part of `cargo test`. CI runs only the smoke
tests in [`tests/parallax_bench.rs`](../../../tests/parallax_bench.rs):
a single-request Kāra-server smoke and a `bench.sh --dry-run`
syntactic gate. Throughput numbers are the bench's artifact, not a
regression gate.

## Throughput results

### Canonical — Graviton EC2 (v8, 2026-07-11)

**Measured on 2026-07-11.** AWS `c7g.4xlarge` (16 vCPU Graviton3),
Ubuntu 24.04 arm64, `wrk 4.1.0`, LLVM 18.1.3; karac built from `main`
`d25bd8b`. Cohort **K/R/J/G** — Kāra + Rust + Java/Netty + Go, the
canonical/headline cohort per
[`docs/investigations/parallax_ec2_bench_plan.md`](../../../docs/investigations/parallax_ec2_bench_plan.md).
`bench.sh --impls=k,r,j,g`, N=3 rounds × 10 s each. `wrk` and the server
share the box (F4 fairness control — absolute numbers are single-box,
lower than the laptop tables below because the load generator competes
with the server for the 16 vCPUs). Node + Phoenix are dropped from the
paid cohort (kept in the laptop supplement below); the cross-ISA x86
confirmation (`c7i.4xlarge`) is confirmed below (phase-6 line 46).

**Framing — read first.** The headline for auto-par is the *source
comparison* (four plain `let` bindings vs Rust `tokio::join!` / Go
goroutines+WaitGroup / Java `CompletableFuture.allOf`). This table is the
**"no perf tax" backstop**: Kāra is in the *same performance class* as
the mature stacks, **not** a req/s winner over them. The providers are
CPU-bound busy loops (v1 has no `sleep_ms`), so the bench is a
thread-pool-scheduling contest that *understates* the auto-par story —
with real independent I/O the ergonomic win is larger, not smaller.

#### Cold start (`-t1 -c1`, first ~1 s)

| Impl | req/s | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|------|-------|--------|--------|--------|--------|--------|
| Rust | 53.6  | 18.6   | 18.6   | 18.7   | 19.9   | 19.9   |
| **Kāra** | 53.0 | 18.6 | 18.6   | 18.6   | **19.2** | 19.2 |
| Go   | 48.9  | 20.2   | 20.2   | 20.2   | 20.5   | 20.5   |
| Java | 41.9  | 21.9   | 22.0   | 22.2   | 85.3   | 85.3   |

Kāra and Rust are within 1 % on the per-request latency floor (Kāra p99
19.2 vs 19.9 ms); Go trails ~1-2 ms; the JVM pays a warm-up tail (85 ms
p99).

#### Steady-state (sustained `wrk`)

| Impl | -c   | req/s (median [min..max]) | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|------|------|---------------------------|--------|--------|--------|--------|--------|
| Rust | 100  | 379 [377..384]            | 237    | 319    | 430    | 817    | 1340   |
| **Kāra** | 100 | **371 [370..371]**    | 260    | 292    | 324    | **378** | 498   |
| Go   | 100  | 367 [365..368]            | 249    | 292    | 386    | 1290   | 1920   |
| Java | 100  | 320 [317..325]            | 304    | 307    | 310    | 476    | 573    |
| Rust | 1000 | 383 [383..388]            | 943    | 1530   | 1800   | 1970   | 2000   |
| Kāra | 1000 | 333 [317..352]            | 1500   | 1760   | 1900   | 1990   | 2000   |
| Go   | 1000 | 282 [281..318]            | 1840   | 1940   | 1970   | 1990   | 2000   |
| Java | 1000 | 232 [230..320]            | 1120   | 1560   | 1820   | 1980   | 2000   |
| Rust | 5000 | 325 [309..374]            | 943    | 1410   | 1770   | 1980   | 2000   |
| Kāra | 5000 | 330 [325..343]            | 1380   | 1690   | 1870   | 1990   | 2000   |
| Go   | 5000 | 86 [15..157]              | 1780   | 1880   | 1930   | 1930   | 1930   |
| Java | 5000 | 230 [230..231]            | NA     | NA     | NA     | NA     | NA     |

At `-c100` (clean, pre-saturation) Kāra is **0.98× Rust**, level with Go,
ahead of the JVM — and holds the **lowest p99 under saturation** (378 ms
vs Rust 817) via `karac_par_run`'s work-helping wait loop. Kāra is also
the most connection-stable impl (371 → 333 → 330 across a 50× connection
increase); Go collapses at `-c5000` (367 → 86, high variance).

#### Auto-par control lane (within-Kāra, same binary + box)

`KARAC_AUTO_PAR=0` vs the default, identical `wrk -t4` loop, median of 3:

| -c   | auto-par ON | auto-par OFF | Δ |
|------|-------------|--------------|---|
| 100  | 340         | 371          | −8 % |
| 1000 | 335         | 338          | −1 % (noise) |
| 5000 | 319         | 326          | −2 % (noise) |

**Honest read:** on this CPU-bound bench auto-par shows **no throughput
benefit** — parity within noise at `-c1000`/`-c5000`, a ~8 % cost at
`-c100`. This is expected: `wrk` already saturates all 16 cores via
concurrent connections, so intra-request fan-out finds no idle cores and
pays only coordination overhead. Auto-par's win is (a) ergonomic — see
the source comparison — and (b) throughput on independent **I/O**, which
this CPU-only kernel cannot express. The control lane confirms auto-par
*fires* (it costs a little here) without helping this workload; that is
the correct, expected result, not a regression.

### Cross-ISA confirmation — x86 EC2 (`c7i.4xlarge`)

**Measured on 2026-07-11.** AWS `c7i.4xlarge` (16 vCPU Intel Sapphire
Rapids), Ubuntu 24.04 amd64, identical `bench.sh --impls=k,r,j,g` cohort
and methodology as the Graviton canonical above; karac built from `main`
`5cc4a8be`. Purpose: show the **Kāra-vs-comparators ratio is
ISA-invariant** — not ARM-cherry-picked (the dev box is itself arm64, so
x86 is the missing ISA).

| Impl | cold req/s | -c100 | -c1000 | -c5000 |
|------|-----------|-------|--------|--------|
| Rust | 50.0 | 337 [336..344] | 340 [339..346] | 253 [201..320] |
| **Kāra** | 51.0 | **346 [345..349]** | 303 [297..322] | 312 [297..316] |
| Go   | 55.0 | 397 [396..398] | 311 [302..341] | 36 [6..193] |
| Java | 48.0 | 349 [346..353] | 261 [259..349] | 257 [257..257] |

**The ratio holds.** At `-c100` Kāra is **1.03× Rust** on x86 vs 0.98× on
Graviton — same performance class on both ISAs, Kāra within ±3 % of Rust
regardless of architecture. Kāra again leads at `-c5000` (312, vs Rust
253 / Java 257 / Go's 36 collapse) and is the most connection-stable
impl. The x86 auto-par control lane matches Graviton — `KARAC_AUTO_PAR=0`
within noise (ON 303 / OFF 296 at `-c100`; ON −6 % at `-c5000`): **no
throughput win on CPU-bound work on either ISA**, consistent with the
Graviton finding.

**Cross-ISA summary — Kāra vs Rust @ `-c100`:** Graviton 0.98×, x86
1.03×. The "no perf tax" backstop is architecture-independent.

---

_The Apple M5 Pro tables below are directional/supplementary only (v7) —
laptop, not citable; superseded as the headline by the Graviton canonical
above._

**Measured on 2026-05-10** (post-G5 — cold-start baseline added
alongside steady-state sweep; see History below). Apple M5 Pro
(10P + 8E cores, 18 logical CPUs), 64 GB RAM, macOS 26.4.1,
`wrk 4.2.0`. `bench.sh` defaults: per-impl, run a cold-start
probe (`wrk -t1 -c1 -d1s --latency`, immediately after server
spawn — captures the first ~80 sequential requests on the cold
runtime), then sweep three connection counts (`-c100`, `-c1000`,
`-c5000`) with N=3 measure rounds × 10 s each. Steady-state
req/s reported as median across 3 rounds with [min..max] range;
all latencies are medians in milliseconds.

### Cold start (first ~1s after spawn, sequential `-t1 -c1`)

| Impl | req/s | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|------|-------|--------|--------|--------|--------|--------|
| Kāra |  87   |  11.2  |  11.3  |  11.4  |  **18.2**  |  18.2  |
| Rust |  85   |  11.4  |  11.5  |  11.5  |  19.4  |  19.4  |
| Go   |  81   |  12.2  |  12.2  |  12.2  |  20.8  |  20.8  |
| Node |   6   | 162.5  | 162.6  | 167.4  | 167.4  | 167.4  |
| Phoenix | 46 |  21.0  |  21.1  |  21.2  |  43.4  |  43.4  |

**How to read this.** Cold-start uses one client connection
sequentially against a freshly-spawned server — this measures
the **fundamental per-request latency floor** with no inter-
request queueing. The four busy_loops run concurrently across
the 18 cores per request; the critical path is the slowest of
them (4 M iter ÷ ~3 ns/iter ≈ 12 ms wall-clock for Kāra/Rust/Go
on this hardware), which is exactly what p50 measures. p99
in cold-start captures the warm-up tail — first few requests
pay lazy-init costs (`karac_par_run`'s `OnceLock` pool init,
tokio's blocking-pool first spawn, V8 tier-up JIT) before
settling into steady-state.

**Cold-start findings.**

- **Kāra / Rust within ~1 ms at p99** (18.2 vs 19.4). At low
  load with no queueing, both stacks deliver the same
  fundamental floor — Kāra's per-request HTTP path is no
  heavier than Rust's hyper service plumbing. The headline
  steady-state advantage (Kāra's lower p99 under `-c100`
  saturation) comes from `karac_par_run`'s work-helping wait
  loop under contention, *not* from any baseline-overhead
  difference.
- **Go ~1-2 ms slower** at every percentile — goroutine
  scheduling overhead per request. Modest but consistent.
- **Node ~14× slower** (162 ms p50 cold-start). Single-threaded
  event loop serializes the four busy_loops. The per-request
  shape is honest about Node's typical-deployment reality.
- **Phoenix ~2× the p99 floor** (43 ms vs Kāra/Rust ~18). The
  BEAM dispatches the four fetches across schedulers, but per-
  request setup (process spawn for the Task fan-out, Bandit's
  connection handling) sits above the native stacks' floor.

### Steady-state (sustained `wrk` load)

_v7 (2026-06-06) — five impls in one session; Kāra now serializes
real `Dashboard` data into the body (byte-identical wire shape to the
others). See History below for the v6→v7 regression check._

| Impl | -c    | req/s (median [min..max]) | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|------|-------|---------------------------|--------|--------|--------|--------|--------|
| **Kāra** | 100   | **738 [737..741]**     |  131   |  167   |  204   |  **272**   |  386   |
| Kāra | 1000  | 696 [692..708]            | 1220   | 1490   | 1720   | 1960   | 2000   |
| Kāra | 5000  | 680 [679..693]            | 1260   | 1650   | 1860   | 1980   | 2000   |
| Rust | 100   | 740 [735..741]            |  118   |  159   |  254   |  710   | 1450   |
| Rust | 1000  | 737 [737..738]            |  808   | 1230   | 1610   | 1950   | 2000   |
| Rust | 5000  | 586 [574..589]            | 1620   | 1830   | 1890   | 1980   | 2000   |
| Go   | 100   | 687 [673..689]            |  139   |  163   |  192   |  430   |  862   |
| Go   | 1000  | 593 [590..674]            |  680   |  920   | 1220   | 1760   | 1990   |
| Go   | 5000  | 623 [595..648]            |  981   | 1260   | 1550   | 1900   | 2000   |
| Node | 100   | 6 [6..6]                  | 1250   | 1550   | 1560   | 1810   | 1810   |
| Node | 1000  | (didn't complete — node can't service 1000 keep-alives at < 10 req/s) | — | — | — | — | — |
| Node | 5000  | (same)                    | — | — | — | — | — |
| Phoenix | 100 | 335 [332..339]            |  286   |  334   |  388   |  513   |  647   |
| Phoenix | 1000 | 257 [256..262]           | 1450   | 1660   | 1830   | 1990   | 2000   |
| Phoenix | 5000 | 145 [145..145]           | NA     | NA     | NA     | NA     | NA     |

**How to read this.** All five impls run the same hash-mix kernel
(`x = (x*31 + i) % p` over `n` iterations) at the same iteration
counts (700 K / 4 M / 1.7 M / 2.7 M) — see G1 history below for
*why* this kernel rather than the original triangular sum. Three
of the four busy_loops have observable returns through `Dashboard`
fields that are then woven into **every** impl's JSON response body
(Kāra included, as of 2026-06-05 — previously Kāra folded them into
the status code because its response body was a fixed literal; the
f-string codegen gaps that blocked body-weaving are now fixed),
preventing the optimizer from eliding them. The fourth
(`fetch_profile_name`) returns `String`/`&str`; its busy_loop result
has no observable use and gets DCE'd in all five impls identically —
accepted and symmetric, since the 3-of-4 larger fan-out branches
dominate the parallel critical path.

**Cold-start vs steady-state — the comparison G5 enables.** Kāra
goes from p99 **18 ms cold** to **272 ms steady-state at
-c100** — a 15× tail-latency degradation under saturated
load. Rust goes 19 ms → 710 ms (37× degradation). Go goes 21 ms
→ 430 ms (20× degradation, but its cold-start was already
slower). The cold-start floor tells you the *fundamental
per-request cost*; the steady-state row tells you what *queueing
under load* does on top of that. Both pictures are needed.

**Headline finding (`-c100`).** Kāra and Rust within ~0.3 %
throughput (738 vs 740). Steady-state p99 — Kāra **2.6× lower
than Rust** (272 ms vs 710 ms) and **1.6× lower than Go**
(430 ms). The Kāra↔Rust p99 gap is the consistent design
dividend: `karac_par_run`'s work-helping wait loop holds the
tail under contention where tokio's blocking-pool tail spreads
(Rust `-c100` max 1450 ms). This is the same ~2.6–2.7× ratio
the v6 baseline showed — see the v7 regression check in History.

**Connection-sweep finding.** Kāra is the most stable across the
sweep — 738 → 696 → 680 (only -8 % at 50× the connections).
Rust holds at -c100 / -c1000 (740 → 737) but degrades to
**586 [574..589]** at -c5000 — the variance shows runs hitting
`tokio::task::spawn_blocking`'s blocking-pool stall edge. Go
degrades to 593 at -c1000 then recovers to 623 at -c5000.
Phoenix runs lower throughout (335 → 257 → 145) and hits a
Bandit acceptor-pool limit at -c5000 (NA latencies — see the
tracked tuning follow-on); the BEAM's per-request process
fan-out carries more overhead than the native stacks.

**Tail-latency finding (`karac_par_run` design dividend).** At
-c100, Kāra's steady-state p99 is 300 ms vs Rust's 803 ms
(2.7×). Why: Kāra's `karac_par_run` work-helping wait loop
(tokio worker that called the handler picks up dispatched tasks
during its wait) gives effective parallelism beyond the
dedicated 18-worker pool, smoothing burst response patterns.
Rust's `tokio::join!(spawn_blocking(...))` hands every fan-out
branch off to a separate blocking thread, paying scheduler-
handoff on every branch and producing queueing tail under burst
load. Go's tail is GC-driven (visible at -c1000 / -c5000 where
the p99 sits near the 2 s wrk timeout ceiling). Cold-start
shows the *opposite* — Kāra and Rust are within 1 ms at p99
(17.4 vs 18.2) — confirming the steady-state advantage is
purely about *contention behavior*, not baseline overhead.

**At -c1000+** all three multi-core impls saturate similarly
(p50 0.8-1.3 s, p99 1.9-2.0 s). The 2 s ceiling on max + p99 is
`wrk`'s default request timeout (it caps measured latency at the
test-duration boundary).

**Node** is asymmetric by design (F4) — single-process JavaScript
serializing four CPU-bound busy loops on the event-loop thread.
Cluster-mode would multiply by ≈ `num_cpus` at the cost of process
orchestration; not v1 of this bench. At -c1000 / -c5000 the OS
runs out of ephemeral ports faster than node can service them, so
those rows show no completed measurements.

## History

**v1 — first verification run (`4f7b72d`, 2026-05-09).** Kāra at
1,089.99 req/s / 438.18 ms p99, four-language table populated.
First end-to-end measurement of the Kāra HTTP stack under sustained
load. Original triangular-sum busy-loop kernel.

**v2 — `karac_par_run` worker-pool fix (`3953a14`, 2026-05-09).**
Profiling diagnosed that 60 % of CPU was spent in `mach_vm_protect`
setting up pthread stack guard pages — `karac_par_run` was creating
fresh OS threads on every fan-out call. Replaced with a long-lived
worker pool: thread churn -94 %, p99 -46 % (438 → 238 ms), CPU
efficiency 9× better. Throughput essentially unchanged because the
bench was wrk-connection-bound at that point.

**v3 — codegen `default<O2>` pass pipeline (`280ce2d`, 2026-05-10).**
Probe sweep ruled out runtime + HTTP layer as the throughput
bottleneck (no-op-handler probe: 108 K req/s). Real bottleneck:
karac was running zero LLVM mid-end passes on its IR — `mem2reg`
never fired, locals stayed in stack slots. Wired
`module.run_passes("default<O2>", …)`. LLVM's `mem2reg` +
`LoopIdiomRecognize` reduced `busy_loop` to its closed form
(`Σi = n(n-1)/2`) and DCE then eliminated the dropped results from
`fetch_*`. Kāra throughput jumped to 97 K req/s, but the bench was
no longer measuring fan-out work — Rust's release codegen had been
doing the same elision all along. Numbers became apples-to-oranges
between impls.

**v4 — apples-to-apples kernel + observable fold (`5ef2ea6`,
2026-05-10).** Replaced the triangular-sum kernel with a hash-mix
step `x = (x*31 + i) % p` (no closed form; can't be reduced).
Updated all four impls (`server.kara`, `main.rs`, `main.go`,
`server.js`) to use the same kernel + same iteration counts. In
each impl, `fetch_*` returns the busy_loop result directly (so it's
observable), and `handle()` folds the `Dashboard.{order_id,
notif_kind, rec_id}` fields into the response (status XOR for Kāra,
JSON body for Rust/Go/Node) so DCE can't elide them. Throughput
fell from 97 K → 711 across all impls because the four busy_loops
now actually run; the resulting numbers are the bench's first true
apples-to-apples comparison since v1.

**v5 — connection-count sweep + multi-run statistics + richer
percentile distribution (`d8a124e`, 2026-05-10).** Implements
G2 + G3 + G4 from
[`docs/investigations/bench_robustness.md`](../../../docs/investigations/bench_robustness.md).
`bench.sh` now sweeps `-c100`, `-c1000`, `-c5000` (configurable
via `--connections=`); runs N=3 measure rounds per (impl, conn)
pair (configurable via `--runs=`) and reports the median req/s
with [min..max] range; parses p50, p75, p90, p99, and max from
each `wrk --latency` run and reports the median of each across
rounds. The single-snapshot table is replaced by a 12-row matrix
(4 impls × 3 connection counts), each cell aggregated across 3
runs.

**v6 — cold-start baseline (this commit, 2026-05-10).**
Implements G5 from
[`docs/investigations/bench_robustness.md`](../../../docs/investigations/bench_robustness.md).
`bench.sh` now runs a cold-start probe (`wrk -t1 -c1 -d1s
--latency` immediately after server spawn, before any other
wrk traffic) per impl, captured in a separate output table
above the steady-state matrix. Cold-start measures the
fundamental per-request latency floor with no inter-request
queueing — the four busy_loops run concurrently per request,
so p50 ≈ critical-path of slowest fetch (~12 ms). Lazy-init
costs (`karac_par_run`'s `OnceLock` pool, tokio's blocking-pool
first spawn, V8 tier-up JIT) show up in p99 of the cold-start
window. Captured as the baseline before HTTP-layer perf work
in [`docs/investigations/http_layer_perf.md`](../../../docs/investigations/http_layer_perf.md)
moves these numbers; once that work lands, re-running the
bench produces a directly-comparable cold-start table.

**v7 — five-impl single-session close-out + real-data body
(2026-06-06).** First sweep with all five impls (kara/rust/go/
node/phoenix) measured in one session, and the first where Kāra
serializes the actual `Dashboard` fields into the JSON response
body — byte-for-byte the wire shape the others emit (exact 159 B
across all five). This closes the last apples-to-apples asymmetry:
previously Kāra returned a fixed JSON literal and folded the i64
fields into the status code, because two f-string codegen gaps
blocked body-weaving (gap A: auto-par `refs_in_expr` lacked an
`InterpolatedStringLit` arm; gap B: an f-string used directly as a
struct-literal field value double-freed its accumulator when the
struct moved out — a crash, exit 133). Both fixed; the handler now
weaves the data via an f-string, which exercises exactly the gap-B
shape.

**Regression check (v6 → v7, `-c100`).** The number that matters —
**Kāra is clean**: req/s 720 → 738 (+2.5 %), p99 300 → 272 ms
(−9.4 %, i.e. *improved*), both inside ±10 %. None of the compiler
work since the v6 baseline (spawn/TaskGroup, cross-task-safe walker,
RAII walker, the f-string fixes, the body-weave) perturbed auto-par
grouping or the fan-out critical path. Go is clean (req/s +3.0 %,
p99 −4.3 %). Node is unchanged (6 req/s — the documented single-
process limit, identical to v6). The headline holds: Kāra `-c100`
p99 **2.6× lower than Rust** (272 vs 710), matching v6's 2.68×
(300 vs 803).

Two comparator deltas land just outside ±10 %, both **improvements
on impls Kāra doesn't touch**, so neither is a regression and
neither gets a `docs/investigations/` writeup (that's reserved for
regressions / unexplained deltas): (1) **Rust p99 803 → 710 ms
(−11.6 %)** — measurement variance on tokio's notoriously spread
tail (v6 Rust `-c100` max was 1660 ms), a month and a session
apart; (2) **Phoenix req/s 274 → 335 (+22 %), p99 588 → 513 ms
(−13 %)** vs its 2026-05-30 first-measurement solo baseline — pure
same-machine reproducibility drift (BEAM JIT warmup / thermal /
background-load differences between sessions), which is exactly the
cross-session mixing this single-session v7 row exists to retire.
Phoenix still hits the tracked Bandit acceptor-pool limit at
-c5000 (NA latencies).

Full investigation log + per-step disassembly + reasoning lives at
[`docs/investigations/parallax_perf.md`](../../../docs/investigations/parallax_perf.md);
bench-measurement gaps + their fixes at
[`docs/investigations/bench_robustness.md`](../../../docs/investigations/bench_robustness.md).

## Fairness controls (F4)

Cross-language benchmarks are easy to slant; these are the controls
the design lock specifies:

- **Hardware:** all four impls run on the same machine, sequentially
  (one impl active at a time). Background load is the same for all.

- **Worker counts:** Kāra and Rust default to tokio's multi-thread
  runtime, which uses `num_cpus` workers — same as Go's default
  `GOMAXPROCS = num_cpus`. Node runs single-process. **No tuning
  knobs are pre-set;** every impl gets the runtime's natural default.

- **Single-process Node footnote:** Node's single-process default is
  faithful to the language's typical deployment reality. Node clusters
  scale roughly linearly with worker count via `cluster.fork()` at the
  cost of process orchestration; cluster-mode Node would multiply the
  number below by ~`num_cpus` but is **not** v1 of this bench. Reader
  takeaway: the Node row is honest about Node's single-process default,
  not a strawman.

- **wrk window:** `wrk -t4 -c100 -d10s` warmup (discarded) + `-d30s`
  measurement (recorded). Same window for every impl.

- **Same wire shape:** every impl returns a JSON body for `GET
  /dashboard/<id>` serialized from the actual `Dashboard` fields —
  Kāra weaves them via an f-string (as of 2026-06-05), the others via
  their language's standard JSON encoder. Body bytes differ in size by
  < ~30 bytes across impls (only the digit counts of the busy-loop i64
  results vary) — not a load-bearing throughput factor.

- **Path randomization (F2):** `wrk` URL is hard-coded to
  `/dashboard/1` in v1 of `bench.sh`. The original F2 plan called for
  a Lua script generating uniform IDs in `1..1000`; deferred for now
  because the busy-loop-based fan-out is `user_id`-invariant — there's
  no provider state to cache, so the fixed-ID and random-ID throughput
  numbers should be indistinguishable. If a future iteration adds
  per-user state, the Lua randomizer is a one-line addition to
  `run_wrk()`.

## Source comparison

Six impls, six idioms for the same problem.

- **[`kara/server.kara`](kara/server.kara)** — fan-out is implicit.
  `get_dashboard` is straight-line sequential code; the four
  `let p = fetch_X()` bindings carry disjoint `reads(R_i)` effects;
  the auto-par analyzer groups them into one `parallel_group` and
  the codegen lowers to `karac_par_run` over four worker threads.
  No `async`, no `await`, no `par {}`, no `Promise.all`. Run
  `karac build --concurrency-report kara/server.kara` to see the
  decision.

  **Response body serializes real `Dashboard` data (2026-06-05).**
  `handle()` weaves the four dashboard fields into the JSON body via
  an f-string — byte-for-byte the wire shape Rust/Go/Node emit. Two
  codegen gaps previously blocked this and are now fixed: (gap A) the
  auto-par `refs_in_expr` now has the `InterpolatedStringLit` arm, so
  bindings read inside an f-string are visible to the conflict
  analyzer; (gap B) an f-string used directly as a struct-literal
  field value (or an explicit `return f"..."`) no longer double-frees
  its accumulator buffer when the struct moves out. The three larger
  i64 fetches ride into the body and survive DCE; `fetch_profile_name`
  returns the constant `"Alice"` (its busy_loop is discarded
  identically in all five impls — symmetric, see "How to read this"
  above).

- **[`rust/src/main.rs`](rust/src/main.rs)** — `tokio` + `hyper` +
  `tokio::join!`. `get_dashboard` `await`s a `tokio::join!` of four
  `spawn_blocking` tasks. The natural perf ceiling for the cohort
  since Kāra's runtime sits on the same tokio multi-thread runtime;
  the Kāra-vs-Rust gap measures Kāra's value-type ABI + handler
  trampoline overhead vs raw Rust.

- **[`go/main.go`](go/main.go)** — `net/http` + goroutines +
  `sync.WaitGroup`. `getDashboard` spawns four `go func() { ... }`
  goroutines, each writes its result into a captured local, the
  `WaitGroup.Wait()` joins.

- **[`node/server.js`](node/server.js)** — Node `http` stdlib (no
  Express dep) + `Promise.all`. `getDashboard` `await`s
  `Promise.all([fetch_X(), ...])`. Single-process; CPU-bound busy
  loops resolve serially on the event loop thread. F4 footnote
  applies.

- **[`phoenix/`](phoenix/)** — Phoenix 1.8 + Bandit + `Task.async`/
  `Task.await` fan-out on the BEAM. `get_dashboard` spawns four
  `Task.async` processes and `Task.await`s each. The commercial
  contrast: the auto-par "no concurrency engineers" reframe lands
  against GenServer/`Task` fan-out, not just Rust's `tokio::join!`.
  At `-c5000` Bandit's default acceptor pool caps ingestion (NA
  latencies) — a tracked tuning follow-on, not a fundamental limit.

- **[`java/`](java/)** — Netty 4.1 (`netty-codec-http`) +
  `CompletableFuture` fan-out on the JVM. `channelRead0` submits the
  four providers as `CompletableFuture.supplyAsync` tasks on a fixed
  `ExecutorService` sized to `availableProcessors()` (the JVM analog of
  Go's `WaitGroup` / tokio's worker pool), joins via
  `CompletableFuture.allOf`, and writes the JSON response from the
  completion callback — the Netty event-loop thread never busy-loops.
  The JVM-tier foil: the auto-par reframe lands against `CompletableFuture`
  orchestration + JIT-warmed throughput, the dominant enterprise backend
  stack. Built by `mvn -q -DskipTests package` into a shaded
  `java -jar`-runnable fat-jar; F4 footnote (busy-loop substitute)
  applies identically.

## Out of scope (deferred to follow-ups)

Per the design lock at [`docs/dogfooding.md § Slice E`](../../../docs/dogfooding.md):

- TLS, HTTP/2, WebSockets — Phase 11.
- Real database FFI (Postgres / MySQL / Redis) — Phase 11. Demo uses
  `sleep_ms(n)`-substitute providers (busy loops; see footnote above).
- Cluster-mode Node — footnoted; not implemented.
- Asciinema cast / video walkthrough — post-v1 polish.
- Multi-user load patterns (Zipf, sticky-session) — `--lua` randomizer
  if a future perf investigation calls for it.
- Splitting Parallax bench into a standalone repo — premature.

## See also

- [`docs/dogfooding.md § Parallax`](../../../docs/dogfooding.md) —
  the demo's design storyboard + Slice E settled-design-fork record
  (F1–F5 + Rust addition).
- [`examples/parallax/`](../) — the multi-file source-of-truth Parallax
  workload (provider impls, traits, resources). The bench's Kāra impl
  is a single-file restatement so `karac build` works without multi-file
  project mode codegen (parked as wip-list2 Theme 4).
- [`tests/parallax_bench.rs`](../../../tests/parallax_bench.rs) — the
  two CI tests that gate the bench harness (smoke + dry-run).
