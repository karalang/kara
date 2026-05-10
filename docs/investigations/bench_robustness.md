# Parallax bench — robustness + realism gaps

**Status:** open. **Started:** 2026-05-10. **Owner:** unassigned.

The Parallax bench (`examples/parallax/bench/`) shipped as a
side-by-side throughput artifact for Demo 1 (Slice E, `4f7b72d`).
After two rounds of perf work (`3953a14`, `280ce2d`) the bench has
revealed structural gaps in *how it measures*, not just *what it
measures*. This doc enumerates those gaps and their fixes —
distinct from [`http_layer_perf.md`](http_layer_perf.md) which
tracks the path to higher throughput numbers.

Cross-refs:
- Bench harness scaffolding: `ea1d26d`. Verification run:
  `4f7b72d`. Pool fix: `3953a14`. Codegen O2: `280ce2d`.
- Slice E design lock: [`docs/demo_ideas.md § Slice E`](../demo_ideas.md).
- Parallax perf investigation: [`parallax_perf.md`](parallax_perf.md).
  The codegen O2 work surfaced gap **G1** (optimization-eaten
  busy_loops) which made the headline numbers no longer apples-to-
  apples; that's the reason this doc exists.

---

## Gap inventory (ranked by impact on number trustworthiness)

### G1 — Optimization-eaten busy_loops [load-bearing]

**What's wrong.** The `busy_loop` kernel runs `Σ_{i=0}^{n-1} i`,
which LLVM's `LoopIdiomRecognize` pass identifies as the
triangular-number sum and replaces with the closed-form
`n(n-1)/2` (6 instructions, constant-time, regardless of n). The
four `fetch_*` helpers then drop the result via `let _ =
busy_loop(...)`, so DCE eliminates the calls entirely. Both Kāra
(`default<O2>`, post-`280ce2d`) and Rust (release profile) do
this; Go's release codegen does it too.

The bench was designed to exercise fan-out efficiency on four
disjoint CPU-bound work units (2/5/8/12 ms each). Today, **none
of the impls actually run those work units** — the bench numbers
report HTTP-handler dispatch overhead, not fan-out efficiency.

**Fix options, ranked.**
1. **Weave `Dashboard` field values into the response body**
   (recommended — closes a v1 limitation simultaneously). Today
   the Kāra impl returns a fixed JSON literal because of two
   pre-existing codegen gaps (`refs_in_expr` lacks
   `InterpolatedStringLit` arm; f-string accumulators are
   unconditionally scope-exit-freed even when returned —
   documented in `examples/parallax/bench/kara/server.kara:99-114`).
   Closing those gaps lets `handle()` build the response body
   from `dashboard.profile_name`, `dashboard.order_id`, etc. —
   the values become user-observable and DCE can no longer elide
   the work that produces them. **Bonus**: matches the design
   intent (Parallax demo's "fan-out + join" should produce a
   dashboard whose data is in the response, not a fixed string).
2. **Optimization barrier (`black_box`-equivalent)**. Add
   `karac_runtime_black_box(value)` extern that takes a value by
   reference and prevents the optimizer from elision. Rust impl
   uses `std::hint::black_box`. Quick fix; preserves the bench's
   shape; doesn't close the response-body limitation.
3. **Use a non-recognizable kernel**. Replace `busy_loop` with
   work the optimizer can't reduce — e.g., a hash-mixing loop,
   a checksum, or pseudo-random walk. Sound but loses the
   simplicity of the current "obviously CPU work" framing.

**Recommended:** (1) for production-grade realism + closes a
related codegen gap; (2) for a quick fix while (1) is being
designed.

### G2 — Single connection-count value (`-c100`) [structural]

**What's wrong.** `bench.sh` hardcodes `wrk -t4 -c100 -d{warmup,
measure}s`. As shown in [`http_layer_perf.md`](http_layer_perf.md)'s
connection sweep, the Kāra server's throughput rises from 94 K
(`-c100`) to 135 K (`-c5000`); a single point on this curve
under-reports the server's capacity. Rust's row also looks
artificially low at `-c100`.

**Fix.** Sweep `-c` and report the curve. `bench.sh` runs three
points: `-c100`, `-c1000`, `-c5000`. README shows three rows per
impl (or a chart). Total bench time triples (~12 minutes total)
which is acceptable for a non-CI bench.

### G3 — Single measurement, no run-to-run statistics [structural]

**What's wrong.** Each impl gets one 30 s measurement. Run-to-run
variance on the same machine is typically 5-10 % even with no
config changes. A single number can't distinguish a 3 % perf
regression from a 5 % run-to-run blip.

**Fix.** Run each impl `N=5` times. Report median + min + max
(or mean ± stddev). Increase total bench time ~5× — acceptable
for a non-CI bench. Add a `--runs=N` flag to `bench.sh` that
defaults to 5 and can be lowered for quick iteration.

### G4 — No higher-percentile latency (p99.9, p99.99) [structural]

**What's wrong.** Only p99 is reported. Real services care about
p99.9, p99.99, and max. `wrk --latency` already collects the full
distribution; we only parse `99%` line. A pause-driven outlier
(GC, scheduler hiccup, page fault) can be invisible at p99 but
deadly at p99.9.

**Fix.** Parse `99.000%`, `99.900%`, `99.990%`, and `100.000%`
lines from `wrk --latency` output. README table grows to 5
percentile columns. Especially relevant for Go (GC-driven tail)
and Node (event-loop-driven tail) — the current p99 column likely
under-states their worst-case behavior.

### G5 — Cold-start vs steady-state conflated [structural]

**What's wrong.** Tokio runtime warm-up, hyper accept-loop
warm-up, allocator state, OS page cache, CPU branch predictor —
all need ~hundreds of ms to stabilize. The current 10 s warmup is
borderline; first-request latency (cold dispatch path JIT, TLS
init, etc.) can leak into measurement window.

**Fix.** Measure cold-start separately. Add a `--cold-start` mode
that runs `wrk -d1s` against a fresh-spawned server and reports
the first 100 latencies. Steady-state numbers stay in the main
table; cold-start numbers go in a separate "Cold start" section
(for Demo readers / production sizing).

### G6 — wrk version not pinned [precision]

**What's wrong.** `bench.sh` checks for `wrk` but doesn't pin a
version. wrk 4.0.x and 4.2.x have slightly different output
formats (specifically the latency-distribution row format).
Future readers running the bench on a different wrk version may
hit parser breakage.

**Fix.** Print `wrk --version` at the top of every bench run;
write to a `bench.run.log` file alongside the README so
reproducibility is documented. If a parse failure occurs, surface
the wrk version in the error.

### G7 — Same-machine load generator [precision]

**What's wrong.** `wrk` and the server compete for the same CPU
cores. With `-t4 -c100`, wrk uses ~4 cores; server uses up to 18.
Cross-contamination means each impl's number is reduced by
whatever wrk happens to consume that run. Network-localhost is
also faster than realistic LAN — adds ~10× the round-trip
latency a production deployment would see.

**Fix (later).** Run `wrk` on a separate machine over a 1 Gbps
LAN. Adds setup complexity, partial mitigation by binding wrk to
a P-core via `taskset -c 0-3` and binding server to E-cores via
`taskset -c 10-17` on Linux (macOS lacks easy CPU pinning).
Acceptable to defer until G1-G3 land — those are higher-impact.

### G8 — Single endpoint [realism]

**What's wrong.** `GET /dashboard/<id>` is the only endpoint
tested. Real services have many endpoints with different cost
profiles. A single-endpoint bench can mask issues with router
performance, request-method dispatch, etc.

**Fix (later).** Add 2-3 endpoints with different shapes:
`GET /healthz` (no work), `GET /dashboard/:id` (current),
`POST /event` (with body — exercises body-parsing path). Bench
each separately. Ship as v2 of the bench.

### G9 — Body size variance not characterized [realism]

**What's wrong.** Response body is a fixed 144-byte JSON literal.
Real APIs have bodies from <1 KB to >1 MB. Throughput at small
body sizes is dominated by per-request overhead; at large bodies,
by serialization + memcpy.

**Fix (later).** Add a `--body-size=<bytes>` knob that sets the
response body to a synthetic JSON of that size. Run the bench at
{144 B, 1 KB, 10 KB, 100 KB} and report the curve. Identifies
where the impl's performance crosses from per-request-overhead-
bound into bandwidth-bound.

### G10 — No regression-detection / CI tracking [structural]

**What's wrong.** Numbers go in `examples/parallax/bench/README.md`
manually after each verification run. Easy to forget; easy to let
a regression slip in over multiple commits. CI runs the smoke
tests but not the bench.

**Fix (later).** Save bench results to a checked-in JSON file
(`examples/parallax/bench/runs/{date}.json`) per run. CI doesn't
*run* the bench (still too expensive) but does *check* that any
PR touching the bench surface includes a corresponding
`runs/{date}.json` update. Manual but enforced. Long-term:
nightly CI run on a dedicated bench machine, regression-flag
PRs that exceed a tolerance band.

### G11 — Long-duration soak missing [realism]

**What's wrong.** 30 s measurement window catches throughput +
tail latency under brief load but misses slow-degradation issues
— memory growth, fd leak, gradual GC pause-time growth, allocator
fragmentation. A real production server runs for days.

**Fix (later).** Add a `--soak=<duration>` mode that runs the
bench for hours and reports the throughput curve over time. Watch
for monotonic degradation. Out of scope for daily iteration but
worth running once per major perf-impacting change.

---

## Suggested execution order

1. **G1** (optimization-eaten busy_loops) — load-bearing for any
   future bench-driven decision. Block all other improvements
   until this lands; otherwise the numbers we capture are
   misleading. Recommended path: weave Dashboard values into
   response body (closes the codegen-gap follow-up
   simultaneously).
2. **G2 + G3 + G4** (connection sweep, multi-run, percentile
   distribution) — together these elevate the bench from "single
   snapshot" to "robust measurement". Can land in one slice.
3. **G5** (cold-start separation) — useful for Demo narrative
   but lower priority than G1-G4.
4. **G6** (wrk version pin / log) — small, do whenever.
5. **G7-G11** (separate machine, multi-endpoint, body-size
   sweep, CI regression tracking, soak) — all are "make it
   really production-grade" upgrades; defer until the v1 bench
   has shipped a couple iterations and proven its diagnostic
   value.

---

## Findings

_(empty — fill in as fixes land; date each entry, link to
commits or supporting artifacts.)_
