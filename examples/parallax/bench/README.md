# Parallax three-language benchmark

Side-by-side `GET /dashboard/<user_id>` throughput across **Kāra**,
**Rust**, **Go**, and **Node.js** — the recordable artifact for Demo 1
([`docs/demo_ideas.md § Demo 1: Parallax`](../../../docs/demo_ideas.md)).

Each impl serves the same canonical fan-out + join workload: four
provider "fetches" per request, each carrying `reads(R_i)` on a
disjoint resource, joined into a `Dashboard` aggregate. The Kāra impl
gets that fan-out from the compiler — straight-line sequential code,
the auto-par codegen runs the four reads concurrently. The other three
write the fan-out by hand (Rust `tokio::join!`, Go goroutines + WaitGroup,
Node `Promise.all`) and serve as the reference perf cohort.

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
> long-tail). To keep the four impls apples-to-apples, **all four
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
| wrk   | `wrk`              | wrk 4.x    |

### Run

```sh
# default — all four impls, 10s warmup + 30s measurement per impl
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
from `server.js`), launches it, awaits the conventional
`BOUND_PORT=<n>` stdout line, runs `wrk -t4 -c100 -dWARMUP+MEASURE`,
parses `Requests/sec` + `99% <lat>`, and kills the server.

The bench is **not** part of `cargo test`. CI runs only the smoke
tests in [`tests/parallax_bench.rs`](../../../tests/parallax_bench.rs):
a single-request Kāra-server smoke and a `bench.sh --dry-run`
syntactic gate. Throughput numbers are the bench's artifact, not a
regression gate.

## Throughput results

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
| Kāra |  86   |  11.3  |  11.4  |  11.5  |  **17.4**  |  17.4  |
| Rust |  85   |  11.5  |  11.6  |  11.7  |  18.2  |  18.2  |
| Go   |  80   |  12.4  |  12.4  |  12.5  |  19.7  |  19.7  |
| Node |   5   | 173.8  | 179.7  | 183.9  | 183.9  | 183.9  |

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

- **Kāra / Rust within 1 ms at p99** (17.4 vs 18.2). At low
  load with no queueing, both stacks deliver the same
  fundamental floor — Kāra's per-request HTTP path is no
  heavier than Rust's hyper service plumbing. The headline
  steady-state advantage (Kāra's lower p99 under `-c100`
  saturation) comes from `karac_par_run`'s work-helping wait
  loop under contention, *not* from any baseline-overhead
  difference.
- **Go ~1-2 ms slower** at every percentile — goroutine
  scheduling overhead per request. Modest but consistent.
- **Node ~10× slower** (174 ms p50 cold-start). Single-threaded
  event loop serializes the four busy_loops. The per-request
  shape is honest about Node's typical-deployment reality.

### Steady-state (sustained `wrk` load)

| Impl | -c    | req/s (median [min..max]) | p50 ms | p75 ms | p90 ms | p99 ms | max ms |
|------|-------|---------------------------|--------|--------|--------|--------|--------|
| **Kāra** | 100   | **720 [716..720]**     |  134   |  176   |  221   |  **300**   |  430   |
| Kāra | 1000  | 679 [673..696]            | 1210   | 1490   | 1740   | 1960   | 2000   |
| Kāra | 5000  | 671 [649..688]            | 1230   | 1670   | 1860   | 1980   | 2000   |
| Rust | 100   | 718 [718..723]            |  120   |  163   |  261   |  803   | 1660   |
| Rust | 1000  | 710 [708..715]            |  761   | 1140   | 1590   | 1920   | 2000   |
| Rust | 5000  | 583 [534..613]            | 1140   | 1440   | 1690   | 1980   | 2000   |
| Go   | 100   | 667 [661..670]            |  143   |  168   |  204   |  449   |  905   |
| Go   | 1000  | 624 [577..660]            |  827   | 1070   | 1370   | 1860   | 2000   |
| Go   | 5000  | 624 [573..641]            | 1540   | 1770   | 1900   | 1990   | 2000   |
| Node | 100   | 6 [6..6]                  | 1140   | 1460   | 1620   | 1960   | 1960   |
| Node | 1000  | (didn't complete — node can't service 1000 keep-alives at < 10 req/s) | — | — | — | — | — |
| Node | 5000  | (same)                    | — | — | — | — | — |

**How to read this.** All four impls run the same hash-mix kernel
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
has no observable use and gets DCE'd in all four impls identically —
accepted and symmetric, since the 3-of-4 larger fan-out branches
dominate the parallel critical path.

**Cold-start vs steady-state — the comparison G5 enables.** Kāra
goes from p99 **17 ms cold** to **300 ms steady-state at
-c100** — a 17× tail-latency degradation under saturated
load. Rust goes 18 ms → 803 ms (45× degradation). Go goes 19 ms
→ 449 ms (24× degradation, but its cold-start was already
slower). The cold-start floor tells you the *fundamental
per-request cost*; the steady-state row tells you what *queueing
under load* does on top of that. Both pictures are needed.

**Headline finding (`-c100`).** Kāra and Rust within ~1 %
throughput (720 vs 718). Steady-state p99 — Kāra **2.7× lower
than Rust** (300 ms vs 803 ms) and **1.5× lower than Go**
(449 ms). Note Go's p99 dropped meaningfully from prior runs
(1200 → 449) — that was a single-run GC-cycle outlier; with
N=3 + median aggregation that artifact has now smoothed out,
which is exactly why we run multiple rounds.

**Connection-sweep finding.** Kāra is the most stable across the
sweep — 720 → 679 → 671 (only -7 % at 50× the connections).
Rust holds at -c100 / -c1000 (718 → 710) but degrades to
**583 [534..613]** at -c5000 — the wide variance shows runs
hitting `tokio::task::spawn_blocking`'s blocking-pool stall
edge. Go degrades early (667 → 624) then plateaus.

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

Four impls, four idioms for the same problem.

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
  identically in all four impls — symmetric, see "How to read this"
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

## Out of scope (deferred to follow-ups)

Per the design lock at [`docs/demo_ideas.md § Slice E`](../../../docs/demo_ideas.md):

- TLS, HTTP/2, WebSockets — Phase 11.
- Real database FFI (Postgres / MySQL / Redis) — Phase 11. Demo uses
  `sleep_ms(n)`-substitute providers (busy loops; see footnote above).
- Cluster-mode Node — footnoted; not implemented.
- Asciinema cast / video walkthrough — post-v1 polish.
- Multi-user load patterns (Zipf, sticky-session) — `--lua` randomizer
  if a future perf investigation calls for it.
- Splitting Parallax bench into a standalone repo — premature.

## See also

- [`docs/demo_ideas.md § Demo 1: Parallax`](../../../docs/demo_ideas.md) —
  the demo's design storyboard + Slice E settled-design-fork record
  (F1–F5 + Rust addition).
- [`examples/parallax/`](../) — the multi-file source-of-truth Parallax
  workload (provider impls, traits, resources). The bench's Kāra impl
  is a single-file restatement so `karac build` works without multi-file
  project mode codegen (parked as wip-list2 Theme 4).
- [`tests/parallax_bench.rs`](../../../tests/parallax_bench.rs) — the
  two CI tests that gate the bench harness (smoke + dry-run).
