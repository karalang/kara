# v69 — Go-parity ops gaps: compile speed, runtime profiling, cross-compile UX, effect-driven debug polish

**Status:** Open brainstorming, 2026-05-20. Four gaps surfaced during a fresh-eye review of the Go-vs-Kāra ops/tooling competitive surface. Each gap is partially or wholly unaddressed in `roadmap.md` / `design.md` and would be a credible dismissal vector — or a missed differentiator — for backend engineers comparing the two at v1. Decide whether each becomes a tracked v1 ship gate, a Phase 8.5 deliverable, or a v1.x follow-up before moving on.

---

## Gap 1 — Compile speed as a tracked v1 metric

### Current state

- **Informal benchmarking only.** Kata bench scripts measure cold *memory* footprint of `karac build` vs `rustc -O`, not elapsed time. Runtime perf is rigorous (hyperfine, warmup, tuned runs); compile-speed perf is unmeasured.
- **No CI gate.** A 10× compile-time regression would not fail anything. The 2026-05-12 Maranget O(N²) blowup was caught by the *memory* spike on kata #1, not by an elapsed-time regression alarm.
- **No published number.** Nothing the README or future blog post can quote as "Kāra compiles at X% of Rust / Y% of Go on a 10K-LOC project."
- **Kata sample skew.** Kata corpus is algorithmic-shape (~100 LOC, optimizer-heavy). Backend-shape code (lots of small fns, generics, traits, IO) exercises the front-end (typecheck, effectcheck, ownership) more heavily — a different cost surface. Kata extrapolation underestimates.

### Why this lives in `karac-rust`, not `kara-katas`

The kata repo is the right home for runtime-perf benchmarks — algorithmic shapes, per-kata hyperfine discipline, Rust/Python/C cross-comparison, kata-level regression scrutiny. Compile-speed regression detection is a *compiler-quality gate*, not a kata exercise, and the repo boundary matters for three reasons:

1. **Gates have to fail PRs to the compiler repo.** A cross-repo CI gate (kara-katas runs on karac-rust PRs) is operationally fragile — version skew between the two repos, contributor-confusion about where to land changes, slower CI feedback. Compile-speed regressions are bugs in karac; the gate that catches them belongs next to the code that introduces them.
2. **Corpus shape is different.** Katas are ~100-LOC algorithmic programs that stress the optimizer. Compile-speed-as-v1-gate needs backend-shape code that stresses the front-end (typecheck, effectcheck, ownership, monomorphization) — that's where the cost surface that scales with project size lives. The kata corpus systematically under-samples the failure modes that matter at 10K LOC.
3. **Quality-gate infra wants to be co-located with its data.** The regression threshold, baseline numbers, CI plumbing, and the program corpus all reference each other. Splitting them across repos adds friction every time one moves.

Conclusion: katas stay where they are (runtime perf is their strength); a new `bench/compile_speed/` track lands in `karac-rust/` as compile-quality infrastructure parallel to the existing `bench/hash_quality`, `bench/hot_swap_cost`, `bench/indirection_cost` tracks. They're complementary, not redundant.

### Why this matters

- **Easiest dismissal vector for Go-shop engineers.** Go's dev loop is fast *because the language was designed for it*. If `karac build` is Rust-shaped (slow), the entire Go-comparison pitch dies in the second paragraph regardless of language quality elsewhere.
- **Compounds over the project.** Compile speed gets worse monotonically without active defense — every feature adds analysis cost. Without a regression gate, the v1 number is whatever happens to be true the day it ships, not a number that was protected.
- **Public credibility lever.** Rust-vs-Kāra and Go-vs-Kāra compile-time numbers on a real-shape codebase are the single most viral measurement a new systems language can publish. Underinvesting here costs adoption surface that costs nothing else to gain.

### Open questions

1. **What's the representative-scale benchmark program?** Options:
   - (a) The flagship demo (Parallax-lite / Parallax / `kara-postgres`-using server) at whatever LOC it reaches.
   - (b) A synthetic 10K-LOC corpus designed to exercise the front-end at scale (many small functions, many traits, many effect declarations, generics).
   - (c) Both: (a) is the "real shape" number, (b) is the "stress shape" number. The latter is the regression alarm; the former is the public quote.
2. **What's the regression-threshold CI gate?** Suggested: >5% compile-time regression on either benchmark blocks merge, same shape as the 6.3 runtime-perf gate (`>5% regression on steady-state P50/P95/P99/P99.9 blocks merge`).
3. **What's the comparison baseline?**
   - Always: `rustc -O` on the same program (translated). Already the practice for kata runtime.
   - Probably: `go build` on the same program (translated). New; needs Go reference impl for each benchmark.
   - Optionally: `clang -O2` on the C kata for orders-of-magnitude calibration.
4. **Cold vs incremental?**
   - Cold is the easiest to measure and the most visible to a first-time user (`karac build` on a fresh checkout).
   - Incremental is what determines dev-loop quality. Kāra doesn't have an incremental compilation model yet (Future / post-self-hosting). At v1, only cold can be honestly measured.
   - Recommendation: lock cold as the v1 number. Defer incremental until the reactive query model lands.
5. **Where in the repo?** Suggested: `bench/compile_speed/` alongside existing `bench/hash_quality`, `bench/hot_swap_cost`, `bench/indirection_cost`. Top-level `bench.sh` aggregator. CI workflow that runs on PR, posts the number, fails on regression.

### Proposed v1 outcome

- A `bench/compile_speed/` corpus with two programs: (a) a real backend-shape program (probably Parallax-lite when it stabilizes), (b) a synthetic front-end-stress program.
- Hyperfine-measured cold compile time for `karac build`, `rustc -O`, `go build` on each, published as a ratio.
- CI gate: >5% regression on either fails the build.
- README line: "On a 10K-LOC backend program, `karac build` runs at ≤Nx of `rustc -O` and ≤Mx of `go build`" — published numbers from the gate.

### What this is *not*

- Not an incremental-compile commitment. Defer.
- Not a JIT-vs-AOT compile-speed comparison. The REPL number is already published per Phase 8.5 Track 1 ("cold-start latency is the v1 perf headline"). This is the AOT story.
- Not a guarantee. The number is whatever measurement gives us; the *gate* is the commitment, not the value.

### Resolutions in progress (2026-05-20)

Decisions reached during discussion; graduate as a batch into `roadmap.md` + `bench/compile_speed/` setup once Gap 1 is fully closed.

- **Corpus home.** Curated set lives in `karac-rust/bench/compile_speed/` as plain file copies. No sync infrastructure with `kara-katas`; the two corpora evolve independently. Reasoning: cross-repo CI pinning has three real costs (version skew, PR-feedback-loop friction, failure attribution ambiguity) that parallel runners don't fix.
- **Corpus shape: two parts.** (1) Curated kata subset — seeded with **one kata to start**, set evolves as the template stabilizes; specific kata selection deferred to corpus-setup time. (2) Synthetic front-end stress program (~10K LOC, many small fns, generics, effect declarations, traits). Reasoning: katas are algorithmic-shape and systematically under-stress the front-end where cost scales with project size; synthetic is the regression alarm that katas can't replicate.
- **Synthetic ships at v1, not deferred.** Without it the gate has a known blind spot the doc itself calls out.
- **Threshold: start generous, tune with data.** Initial value: **30% regression blocks merge**. Generous on purpose — during active development, legitimate changes shift compile time by a few % easily, and a tight gate would flake or create friction without catching the regressions that actually matter. The 30% gate catches order-of-magnitude blowups today (e.g. the 2026-05-12 Maranget O(N²) shape) without false-positiving on routine work. Tighten as data accumulates: standing up the corpus + 10–20 baseline runs reveals (a) noise floor and (b) karac-vs-rustc steady-state ratio, which together set the next threshold step. Long-term target: ≤5% once the compiler stabilizes. Doc commits to the trajectory (generous → tight); CI carries the current value.
- **Baseline vs reference measurement — the distinction matters.**
  - **Baseline (in CI, gates merges): `rustc -O` only.** Rust is the semantic and architectural peer — same family (LLVM backend, monomorphization, ownership analysis), same optimization-vs-compile-speed tradeoff curve. A karac-vs-rustc ratio measures *engineering quality on the same design surface*, which is the honest comparison. This is the number the regression gate watches. Nothing else gates merges.
  - **Reference measurements (published, not gated): `go build` and `clang -O2`.** Different role — context for readers, not enforcement. Measured where the source exists; numbers published in bench output and READMEs; no CI gate, no per-benchmark obligation to write fresh translations.
    - **Go**: minimal optimization, no monomorphization, no LLVM, simple linker — Go was *designed* for fast compilation as an explicit tradeoff against optimization. A karac-vs-go ratio measures *design choices*, not engineering quality. Published with the philosophy caveat: "Go optimizes for compile speed by design; Kāra (like Rust) optimizes for runtime performance and chooses where to spend compile cycles."
    - **Clang**: shares the LLVM backend with rustc, so the karac-vs-clang delta isolates *karac-specific frontend + analysis cost* from LLVM-backend cost. Useful calibration data when C source exists for the benchmark; not a structural commitment.
  - Reasoning for the split: a CI gate that watches three baselines is three places to flake and three things to keep green per PR. The peer baseline (rustc) is the one whose number we should defend; the others are signal for readers and ourselves, not enforcement.
- **Cold only at v1.** Incremental deferred until reactive query model lands.
- **Bench instruction is missing today.** Neither `kara-katas` nor `karac-rust/bench/` has a documented bench-setup protocol — the implicit pattern is visible only by reading three of the existing kata `bench.sh` files. Compile-elapsed time is literally one missing block in the existing kata `bench.sh` (parallel to the existing rusage compile-memory block, swapping `/usr/bin/time -l` for `hyperfine --warmup 1 --runs 10`). Action: write `karac-rust/bench/README.md` as the canonical bench-setup instruction covering hyperfine discipline (warmup/runs for short vs long workloads), rusage discipline (single-sample memory), artifact-deletion-for-cold protocol, output format, and **add compile-elapsed as a tracked measurement alongside the existing compile-memory measurement**. Mirror as `kara-katas/BENCH.md` so kata authors have a template; kata compile-elapsed data is reference, not gate.
- **Published numbers live in both packages.** The compile-speed measurement is a public-facing number, not just a CI artifact. It belongs in:
  - Per-kata READMEs in `kara-katas` (compile-elapsed table alongside the existing runtime + compile-memory tables — readers comparing katas see all three dimensions in one place).
  - `karac-rust/bench/compile_speed/README.md` for the curated set + synthetic numbers (the gate-protected number; the headline that goes in the v1 launch blog post).
  Both publish, neither defers to the other. kara-katas numbers are the per-shape reference data; karac-rust numbers are the gate's source of truth and the public-quote-able aggregate. The bench instruction (above) specifies the rendering format so the two stay consistent.
- **Kata sample skew gets its own plan, tracked in `kara-katas`.** The algorithmic-only corpus systematically under-samples backend / front-end-stress shapes — the cost surfaces that matter at 10K LOC. Closing this is a multi-quarter effort that doesn't gate the compile-speed work in karac-rust, but it must not be lost. Filed as `kara-katas/PLAN.md` (created 2026-05-20) with five coverage axes (shape, scale, language-feature stress, stdlib breadth, comparison targets), priority ordering, and a "done when" criterion. Each axis has `[ ]` checkboxes so progress is visible at a glance and individual katas can be implemented incrementally. The synthetic 10K-LOC front-end-stress program (resolution above) is referenced from the plan but lives in karac-rust — kara-katas covers the *real-shape* coverage gap; the synthetic covers the *stress-shape* gap. Complementary, not redundant.
- **Real-shape backend number comes from the kara-katas backend-service kata, not Parallax-lite.** OQ1 originally proposed Parallax-lite as the real-shape public quote (option a). Resolved instead via option 1: when the backend-service kata lands (priority #1 in `kara-katas/PLAN.md`), it becomes the curated subset's backend representative *and* the real-shape public-quote number. One mechanism, one corpus. Parallax-lite stays a demo, not benchmark infrastructure. **Consequence: the backend-service kata is elevated to v1-required status in `kara-katas/PLAN.md`** — without it, v1 ships with synthetic-stress + algorithmic-kata numbers only and can't quote a backend-shape compile-time number, which is precisely the public-quote OQ1 was framed around.
- **Top-level `bench/bench.sh` aggregator: yes, thin wrapper.** Each existing track (`hash_quality`, `hot_swap_cost`, `indirection_cost`) already has its own reproduction; `compile_speed` adds a fourth. A top-level `bench/bench.sh` is a thin shell wrapper that invokes each track's own script in sequence, not a unified harness. Purpose: one command runs everything, easier CI entry point, discoverability for new contributors. Each track stays independently runnable; the aggregator is convenience, not coupling.
- **CI workflow: simple PR-gated shape with baseline file committed to repo.**
  - **Trigger**: PR-only at v1. No scheduled runs. (Scheduled nightlies are a v1.x add when we have signal to debug.)
  - **Baseline storage**: `bench/compile_speed/baseline.json` committed to the repo. Updated by a separate workflow that runs on main-merge — that job measures the just-merged commit and overwrites the baseline file as a follow-up commit (or as a PR if branch protection requires).
  - **Per-PR job**: runs the bench corpus, parses output to JSON, compares against `baseline.json`, posts a PR comment with the result table + verdict (pass/fail + per-benchmark ratio against rustc + per-benchmark delta against baseline). Fails the job if any benchmark exceeds the threshold (30% initially per the threshold resolution above).
  - **Caching**: stock GH Actions build cache for karac itself; no benchmark-result caching at v1 (each PR pays the full cost — ~10–20 runs × N benchmarks). Acceptable because the corpus is small (one kata + one synthetic at v1); revisit if wall-time becomes painful.
  - **Known risks to accept**: (a) CI runner variance — baseline numbers shift across runner generations; mitigation is comparing ratios (karac/rustc) rather than absolute karac times, since rustc shifts too. (b) baseline rot if main-merges aren't measured — mitigation is the dedicated main-merge workflow above. Both risks are tolerable; revisit if they bite.

---

## Gap 2 — Operational maturity: pprof equivalent / production profiling

### Current state

**Already designed** (Phase 8):
- `std.runtime::list_tasks() -> Vec[TaskInfo]` — every suspended task with WaitTarget, source loc, effect summary
- `std.runtime::list_par_blocks() -> Vec[ParBlockInfo]` — every active par-block with SpawnSiteId, worker count, per-worker source loc
- `std.runtime::has_debug_metadata() -> bool` — profile-gated detection
- `std.panic` — structured JSON crash report with logical/provider stack, RC-fallback annotations, parallel context
- `std.tracing` — span/trace context propagation, OTel-export-ready (Phase 8 Backend Platform, P1)
- Profile-gated debug-metadata emission (`runtime_debug_metadata = true|false`)

**Not designed:**
- **No sampling profiler.** `pprof` exposes time spent per function as a sampled flamegraph. No analog in current `std.runtime`.
- **No live-process metrics endpoint.** Go's `net/http/pprof` exposes `/debug/pprof/heap`, `/debug/pprof/goroutine`, `/debug/pprof/profile` at runtime, on a live process, accessible via standard tools (`go tool pprof http://...`). Kāra has nothing equivalent.
- **No execution tracer.** Go's runtime/trace shows per-goroutine timeline, GC events, system calls. Kāra has no execution-trace surface.
- **No standardized observability protocol output for `par`/`suspend` lifecycle.** `std.tracing` is span-based (OTel), not low-overhead always-on instrumentation. The auto-concurrency runtime emits no metrics by default.

### Why this matters

- **The 3am-page argument requires this.** "Compile-time elimination of bugs Go teams page on at 3am" is the headline pitch. But Go teams *still* page at 3am for things compile-time can't catch (memory pressure, runtime deadlocks across providers, GC-equivalent — in Kāra, RC chain stalls, contention on the network event loop, p99 cliffs from auto-concurrency miscalibration). When that page fires, the team reaches for `go tool pprof`. Without an equivalent, every backend team that adopts Kāra and hits a production-only issue has no path to diagnose it.
- **It's the single biggest "Go has it, Kāra doesn't" delta after compile speed.** Operational maturity overall takes 5+ years to catch; the *profiler surface*, by contrast, is a few weeks of engineering — design + LLVM line-table integration + a sampling thread + a serialization format. The maturity is in the tools that consume the data; the data emission is implementable now.
- **Auto-concurrency makes it more important, not less.** A Go team can read goroutine names and reason about why a deadlock happened. A Kāra team facing a stuck `par`-block needs the compiler-generated SpawnSiteId resolved to source location, the effect set at the stall point, the parent-task chain, and ideally the sampled hot path. The design has the *static* part; the *dynamic* part (sampling) is missing.

### Open questions

1. **Sampling profiler surface — built-in or stdlib?**
   - Built-in (compile-time-instrumented sampling like Rust's coverage instr): more accurate, higher overhead, requires codegen support.
   - Stdlib + signal-based sampling (Go pprof shape): lower overhead, slightly less precise, fits the "library on top of the runtime" model.
   - Recommendation: signal-based, matches Go's model and the project's lean-runtime preference.
2. **Live-process endpoint — opt-in or always available?**
   - Go's `net/http/pprof` is opt-in (`import _ "net/http/pprof"` registers handlers on the default mux). Production deployments enable it on a debug port.
   - Recommendation: opt-in via stdlib import (`import std.runtime.profiler`), matches Go's discipline.
3. **Output format — pprof-compatible or new?**
   - pprof-compatible (protobuf, Google's format): instant tool reuse (`go tool pprof`, Pyroscope, Grafana, etc.). Zero-cost UX win.
   - New format: aligned with Kāra's structured-diagnostic philosophy, but everyone has to write tooling from scratch.
   - Recommendation: pprof-compatible at v1 (cheap interop), with a Kāra-native extension field for SpawnSiteId / effect-set annotations that tools can ignore. Get the ecosystem benefit for free; layer the differentiation on top.
4. **Execution tracer — v1 or v1.x?**
   - Sampling profiler is the higher-value, lower-cost item. Execution tracer (full timeline) is more engineering, narrower audience.
   - Recommendation: sampling profiler at v1; execution tracer at v1.x. Don't bundle.
5. **Integration with `std.runtime` introspection.** The existing `list_tasks()` / `list_par_blocks()` API is static (one-call enumeration). A sampling profiler is continuous. Probably distinct namespaces (`std.runtime` static, `std.runtime.profiler` continuous), or fold both under `std.runtime` with clear sub-modules.

### Proposed v1 outcome

- `std.runtime.profiler` stdlib module with: signal-based sampling, pprof-compatible protobuf output, opt-in live endpoint via stdlib import.
- Sampled data includes: function source location, effect set at sample point, current SpawnSiteId / parent-task chain (the existing Debugger Contract metadata).
- README line: "`go tool pprof http://localhost:6060/debug/pprof/profile` works against a Kāra HTTP server."
- This is a P1 v1 deliverable, sequenced after the Phase 8 Backend Platform spine (`std.http`, TLS, WebSocket) because the live endpoint needs an HTTP server to live on.

### What this is *not*

- Not a full APM stack. APM (DataDog, NewRelic) is application instrumentation on top of a profiler; this is the profiler itself.
- Not a GC story (Kāra has no GC). The "memory profiler" surface for pprof maps to RC allocation tracking, not heap walks.
- Not real-time observability. `std.tracing` (already designed) covers spans/traces. This covers wall-clock attribution.

### Resolutions in progress (2026-05-20)

Decisions reached during discussion; graduate as a batch into `roadmap.md` + `design.md` (`std.runtime.profiler` module spec) once Gap 2 is fully closed.

- **Sampling surface: signal-based, stdlib, with runtime cooperation hook.** Profiler lives in `std.runtime.profiler` as a stdlib module — *not* compile-time-instrumented. Reasoning: (1) the 3am-page use case is wall-clock attribution ("where's the CPU going"), which signal-based sampling answers directly; compile-time instrumentation answers call-counts, a different question. (2) Compile-time instrumentation would expand `codegen.rs`'s responsibilities (probe placement, counter ABI, runtime-collection hooks), conflicting with the codegen-containment principle in `CLAUDE.md § Architecture`. Signal-based lives entirely in stdlib + runtime; codegen contributes only the existing debug metadata (source-loc, SpawnSiteId, parent-task chain) that the sampler annotates frames with.
- **Auto-concurrency requires a runtime cooperation hook — load-bearing sub-decision.** Go's pprof works because the Go runtime exposes "what goroutine is executing on this OS thread" to the sampler in signal handler context. Kāra's auto-concurrency runtime needs the analogous surface: **a per-worker atomic "current task" slot**, updated by the scheduler on task entry/exit, readable from the signal handler in async-signal-safe context (no locks, no allocation). Without it, sampled frames can't be attributed to the task / SpawnSiteId / parent-task chain — losing the entire compile-time-metadata differentiation. This is small but real engineering; specify it in `std.runtime.profiler`'s design alongside the sampler itself.
- **Sampling rate: 100Hz default, env-var configurable.** Matches Go's discipline; enough resolution for 3am-page work without visible CPU burn (≈1% overhead at 100Hz). Configurable via env var (e.g., `KARA_PROFILER_HZ=1000` for finer-grained sampling at debug time). Higher rates available; not the default.
- **CPU time at v1; wall time at v1.1.** Go exposes both (`pprof.CPUProfile` vs `pprof.GoroutineProfile`); we ship CPU-time at v1 as the headline number (the surface developers reach for first). Wall-time profile lands as a v1.1 follow-up — same machinery, different timer source (CLOCK_MONOTONIC vs SIGPROF). v1.1 because it's additive (new value type, same wire format, same endpoint), confined to runtime + stdlib, and shares the foundation locked for v1 — same shape of minor-version feature as the execution tracer below.
- **Signal mechanism: `setitimer(ITIMER_PROF)` + `SIGPROF` on both Linux and macOS, with macOS quirk acknowledged.** Cross-platform mechanism; standard across the pprof-compatible ecosystem. macOS quirk: signals aren't always delivered to the thread that consumed CPU (Linux is per-thread, macOS is process-wide-then-routed); mitigation is the sampler thread reads per-worker state from the cooperation hook above, not from the signal context's `ucontext_t`. Document the quirk in `std.runtime.profiler`'s design notes so future contributors don't reach for a `ucontext_t`-based shortcut.
- **Live-process endpoint: env-var opt-in, separate listener, localhost-only by default.** Three coupled decisions:
  - **Opt-in via env var, not stdlib import.** `KARA_PROFILER_PORT=6060` enables the endpoint at runtime; default is off. Reasoning: (1) incident response — operator flips a flag at next deploy, no source edit, no PR, no review; Go's `import _ "net/http/pprof"` shape requires code change at exactly the worst possible time. (2) Kāra's module system has no `_`-import-for-side-effect idiom; forcing one onto stdlib to match Go would be a Go-shaped wart. (3) Consistency with the sampling-rate env var (`KARA_PROFILER_HZ`) — same config surface, single mechanism.
  - **Separate listener, not piggy-backing on the app's HTTP server.** Profiler endpoint runs on its own listener bound to `KARA_PROFILER_PORT`. Reasoning: shared listener means shared auth/middleware (or bypass — both bad); mixing app and debug traffic creates security and ops issues that production Go shops have learned to avoid by running a separate debug port.
  - **Localhost-only by default.** `KARA_PROFILER_PORT=6060` binds to `127.0.0.1:6060` only. Production access via SSH tunnel or sidecar. To expose externally, `KARA_PROFILER_BIND=0.0.0.0:6060` — explicit opt-in, with the security implications obvious from the env var name. Matches the convention production Go shops actually follow.
  - **Not at v1: effect-typed exposure** (e.g., `exposes(Profiler)` effect on the registering fn). Architecturally consistent with Kāra's pitch but adds design surface for marginal gain at v1; env vars are universally understood. Revisit in v1.x when the operational story matures and the cost of a new effect verb is amortized across more uses.
- **Output format: pprof-compatible protobuf with Kāra-native extension labels.** Adopt Google's pprof spec (`profile.proto` v3) as the wire format, with Kāra-specific data layered via `Sample.label`. Reasoning: (1) the entire pprof ecosystem (`go tool pprof`, Pyroscope, Grafana Phlare, Polar Signals, Datadog, New Relic) reads it natively — the README line *"`go tool pprof http://localhost:6060/debug/pprof/profile` works against a Kāra HTTP server"* is true on day one with zero tooling effort. (2) `Sample.label` is key/value with repeats, expressive enough to carry Kāra-specific data without custom protobuf fields. (3) Inventing a new profile format means everyone — including us — writes tooling from scratch; the structured-diagnostic philosophy applies to human-facing diagnostics (panics, hover), profiles are tool-fed data where ecosystem interop dwarfs format aesthetics. Sub-decisions:
  - **Spec version**: `profile.proto` v3 (current Google-published spec); track upstream rather than pinning a revision.
  - **Compression**: gzipped protobuf with `Content-Encoding: gzip` on the endpoint — what every pprof tool expects.
  - **Symbolization**: symbolize-on-emit using LLVM debug-info at sample-collection time; output is immediately usable without an external symbol-server. Matches Go's behavior; avoids the offline-symbolization step that surprises first-time users.
  - **Value type at v1**: `cpu` nanoseconds (samples weighted by sampling interval). Single value type, single profile. Wall-time profile (deferred to v1.1 per the CPU-vs-wall-time resolution above) adds a second value type using the same machinery.
  - **Extension encoding**: `spawn_site_id` as a single string label; `effect` as repeated labels (one per effect in the set); `parent_task_chain` as a single comma-separated string or repeated `parent_task_N` labels (final encoding chosen when `std.runtime.profiler` is specced — depth-ordering needs of consumers determine the call). The *principle* — extensions live in `Sample.label`, no custom protobuf message fields — is locked.
- **Execution tracer: v1.1, not v1.** Defer the Go-`runtime/trace`-equivalent timeline tracer to v1.1 (the next minor release after v1), not bundled with the v1 sampling profiler. Reasoning: (1) **Higher cost, narrower audience** — sampling profiler is 4–8 weeks and answers the universal "where's the CPU going" question; execution tracer is multi-month (event types, runtime hooks at every scheduling decision, ring buffers, binary format, rendering tooling) and answers the narrower "why is this worker waiting" question. Bundling delays v1 for narrower payoff. (2) **Sampling already covers the v1 pitch.** "`go tool pprof` works against a Kāra HTTP server with effect-set annotations" is the differentiation the 3am-page argument needs at launch; the par-block-timeline-with-effect-transitions differentiation is a v1.1 story that builds on the v1 foundation, not a precondition for v1's pitch landing. (3) **Foundation extends cleanly to v1.1.** The runtime cooperation hook locked above (per-worker atomic "current task") is the same surface a tracer extends — add per-worker ring buffers, emit events on task start/end/suspend/resume, drain from a background thread. v1.1 work is "extend the cooperation surface," not "redesign the runtime." Why v1.1 specifically and not v2: the tracer is additive (no breaking changes), confined to runtime + stdlib (no typechecker/effect-checker/codegen impact), and builds on locked v1 surface — the shape of a minor-version feature. v2 would imply foundational rework, which this isn't. **Format choice deferred to v1.1 design time** (Go `runtime/trace` vs Perfetto-compatible vs Chrome trace events); the principle is locked, the wire format is not. The point of pre-locking the v1 cooperation-hook design is to make this v1.1 work non-blocking on any v1 decision.
- **Namespace: core in `std.runtime.profiler`, HTTP endpoint in `std.http.profiler`, static introspection stays in `std.runtime`.** Three-way split:
  - **`std.runtime` keeps the static one-call introspection API** — `list_tasks()`, `list_par_blocks()`, `has_debug_metadata()`. These are snapshot inspection: ask once, get the current state. Already designed; unchanged by this resolution.
  - **`std.runtime.profiler` holds the continuous sampler core** — lifecycle (start/stop), sampling thread, signal handler, pprof emission, the per-worker cooperation hook from the resolution above. No HTTP dependency, no `std.http` coupling. Usable from CLI tools and embedded targets that want file-dump profiles without pulling HTTP machinery. v1.1 adds wall-time value type here.
  - **`std.http.profiler` holds the HTTP endpoint** — wraps the sampler core, registers handlers on a separate listener, reads `KARA_PROFILER_PORT` / `KARA_PROFILER_BIND` env vars at startup. Pulls in `std.http` (the server-side dependency) and `std.runtime.profiler` (the data source). Mirrors Go's `net/http/pprof` pattern.
  - Reasoning: (1) Static introspection and continuous profiling have different usage patterns (snapshot vs lifecycle) — different modules. (2) Core/HTTP split keeps the sampler dependency-free; users on platforms without HTTP (embedded, CLI-only) can still profile. (3) `std.http.profiler` as the HTTP wrapper composes naturally with other HTTP utilities and matches the Go pattern users will already know.

---

## Gap 3 — Cross-compile UX

### Current state

**Already designed:**
- `[target.X.dependencies]` / `[target.X.profile]` manifest blocks (Phase 8.5 Track 2)
- `--target <triple>` flag mentioned for reproducibility lock (`design.md` § Package System line 1213) and for GPU codegen (`--target cuda`, Phase 10)
- Single `kara.lock` across targets (design.md line 1152) — prevents version skew during cross-compile
- LLVM backend supports the codegen mechanically

**Not designed:**
- **No Go-style trivial-UX commitment.** Go's `GOOS=linux GOARCH=arm64 go build` is two env vars, no toolchain install. Kāra's `--target` flag exists but there's no design for shipping bundled sysroots / linkers the way Go ships pre-built `std` for all targets.
- **No `karac targets` discoverability surface.** A user can't ask "what targets does this karac support?" without reading docs.
- **No failure-mode design.** When the linker for the target isn't found, what does the user see? Today: an LLVM linker error. Tomorrow at v1: ? No commitment.
- **No design for which targets are first-class at v1.** Linux/macOS/Windows x86_64 + arm64 is the assumed set, but it's not written down with a quality bar.

### Why this matters

- **Go's two-env-var flow is *why* Go ships to ARM at midnight.** Cross-compile triviality enables a category of operational workflows that don't exist when toolchain setup is required: building from a laptop for an embedded device, building from CI for multi-arch container images, building for a target the developer doesn't own.
- **This is the smallest of the three gaps in design effort, but the most visible in first-five-minutes UX.** A developer who runs `karac build --target linux-arm64 my-server.kara` and gets "linker not found, install LLVM with these flags" goes back to Go. The same developer who runs it and gets a working binary has just experienced the systems-language pitch with zero friction.
- **Locks in cross-compile as a v1 commitment vs v1.x deferral.** Without explicit design, this slides to "we'll deal with it later" — and "later" is after public launch, which is too late. Decide now.

### Open questions

1. **Which targets are first-class at v1?**
   - Minimum: Linux x86_64, Linux arm64, macOS x86_64, macOS arm64, Windows x86_64.
   - Likely: above + Windows arm64, WASM (already in Phase 10).
   - Each first-class target must: have a bundled sysroot/linker, have CI coverage, be in `karac targets` output.
2. **How do bundled toolchains ship?**
   - Option A: `karac` binary statically links LLD and bundles per-target sysroots. Big binary, zero setup.
   - Option B: `karaup target add linux-arm64` downloads the toolchain on demand, similar to `rustup target add`. Small `karac`, one-command setup.
   - Recommendation: B. Matches the `karaup` toolchain manager pattern already implied by `karac-toolchain.toml`.
3. **What's the canonical UX?**
   - `karac build --target linux-arm64` (one flag, target triple).
   - Or env-var form: `KARA_TARGET=linux-arm64 karac build` (Go-style).
   - Or both — they're not mutually exclusive.
   - Recommendation: support both, document `--target` as canonical (matches `cargo` convention; env-var form is fallback for CI scripts).
4. **What's the failure mode when toolchain isn't installed?**
   - Worst: cryptic LLVM linker error.
   - Best: `error: target 'linux-arm64' not installed. Run: karaup target add linux-arm64`.
   - The latter is a few hours of polish work. Lock the diagnostic shape now.
5. **What about cross-compile-for-test?**
   - `cargo test --target X` is fiddly because tests need to *run*, which requires emulation (QEMU) or remote execution.
   - Recommendation: at v1, `karac build --target X` works; `karac test --target X` errors with "cross-target tests not supported at v1, use --target host." Don't try to ship QEMU integration.
6. **GPU targets (Phase 10) — same surface?**
   - `--target cuda` is already in the GPU design. Confirms `--target` is the universal mechanism, not split between CPU and GPU. Good.

### Proposed v1 outcome

- `karac build --target <triple>` works for: Linux x86_64/arm64, macOS x86_64/arm64, Windows x86_64.
- `karac targets` lists supported triples and shows installed/not-installed per the toolchain manager.
- `karaup target add <triple>` installs the needed sysroot + linker.
- Diagnostic for missing target points at the install command.
- README line: "Cross-compile to any supported target with one flag — no extra setup."
- This is Phase 8.5 Track 2 work (build & dependency tooling). It's smaller than Tracks 1 and 3 but should be filed there with explicit deliverables, not left implicit.

### What this is *not*

- Not cross-compile-and-test. Defer.
- Not Linux-from-Linux distro detection. The target triple is enough; we don't try to guess the user's libc preference.
- Not platform-specific stdlib forking. Same source compiles to all targets; per-target deps via the manifest mechanism that's already designed.

### Resolutions in progress (2026-05-20)

Decisions reached during discussion; graduate as a batch into `roadmap.md` Phase 8.5 Track 2 once Gap 3 is fully closed.

- **First-class targets at v1: five-target set.** `karac build --target <triple>` works first-class for: **Linux x86_64**, **Linux arm64**, **macOS x86_64**, **macOS arm64**, **Windows x86_64**. "First-class" means three things: (1) bundled sysroot + linker installable via `karaup target add <triple>`, (2) PR-time CI smoke build for each target (not the full test suite — see OQ5), (3) listed in `karac targets` output with installed/not-installed status. Reasoning:
  - **Five-target minimum is the credible Go-comparison set.** `GOOS=linux GOARCH=arm64 go build` works for these five out of the box on Go; anything less and Kāra loses the cross-compile UX comparison on the most common target combinations developers actually hit.
  - **Linux x86_64 + arm64**: backend deployment standard; arm64 specifically for AWS Graviton, Azure Ampere, embedded.
  - **macOS x86_64 + arm64**: x86_64 stays in because organizations still ship Intel Macs (cost of supporting it is low — clang + LLD already handle it); arm64 is the default development machine.
  - **Windows x86_64**: minority audience but real; backend Windows server work exists.
  - **WASM via Phase 10 track, not this list.** Phase 10 already commits WASM via `--target wasm32-...`; cross-compile UX integrates with it via the same flag mechanism but the target's first-class commitment is owned by Phase 10, not Phase 8.5 Track 2.
  - **Windows arm64 deferred.** Surface-device market and ARM Windows laptop install base too small at v1 to justify the CI matrix expansion. Revisit post-v1 when user demand emerges; not a v1 readiness blocker.
  - **No multi-tier system at v1.** Rust's Tier 1/2/3 model exists because Rust supports 100+ targets; with five first-class targets, a single "supported" category is enough. Tiering can be added later if the target set grows past 10.
- **Bundled toolchain shipping: on-demand via `karaup target add`, not statically bundled.** Reject Option A (statically link LLD + bundle all per-target sysroots into the `karac` binary). Reject reason: 500MB+ base binary (LLD + 5 sysroots + precompiled stdlibs), every install pays for targets the user doesn't need, toolchain updates require re-downloading the entire `karac`. Adopt Option B (rustup-style on-demand) because it matches the `karaup` toolchain manager pattern already implied by `karac-toolchain.toml`, scales pay-for-what-you-use, and lets toolchain updates ship independently of the `karac` binary (LLD security fix → push a new toolchain release without bumping `karac`). Sub-decisions:
  - **Toolchain contents**: sysroot (target libc + system headers where needed) + LLD + precompiled stdlib for the target. Precompiling stdlib per-target adds download size but matches the zero-setup promise — users don't wait for stdlib to build on first cross-compile.
  - **Hosting**: project-hosted CDN as primary; GitHub Releases as free fallback (slower, rate-limited but works without infra); enterprise mirror config (`karaup config mirror <url>`) for air-gapped or compliance-constrained environments. Three-tier hosting means we never block adoption on infra readiness.
  - **Install trigger**: explicit `karaup target add <triple>`. **Not** auto-install on first `karac build --target <triple>`. Auto-install adds complexity (interactive prompts break in CI, non-TTY environments, scripted workflows); explicit install with the missing-toolchain diagnostic pointing at the command (per OQ4) is friendlier in the long run.
  - **Version pinning**: each `karac` release ships with a known-good toolchain version; `karaup target add <triple>` defaults to the toolchain version matching the current `karac`. Mismatched versions emit a warning (not an error — sometimes wanted for compatibility testing). Override via `karaup target add <triple> --toolchain-version <version>` for advanced cases.
- **Canonical UX: `--target` flag is canonical; `KARA_TARGET` env var is fallback.** Both supported; flag wins on precedence; host is the default when neither is set. Reasoning: matches cargo (most users' baseline expectation), explicit flag is visible in build scripts and CI logs, env var composes with shell environment management (direnv, CI variables). Sub-decisions:
  - **Env var naming: `KARA_TARGET`, not `KARAC_TARGET`.** Single `KARA_*` namespace consistent with `KARA_PROFILER_PORT` / `KARA_PROFILER_HZ` from Gap 2. No `KARAC_*` vs `KARA_*` split.
  - **Triple format**: short forms for the five first-class targets (`linux-arm64`, `linux-x86_64`, `macos-arm64`, `macos-x86_64`, `windows-x86_64`), full LLVM triples (`aarch64-unknown-linux-gnu`, etc.) accepted as aliases and required for community/advanced targets that need disambiguation (musl vs gnu, MSVC vs MinGW). Reasoning: short forms win the Go-comparison UX; full triples stay available for users who need precision.
  - **Precedence**: `--target` flag overrides `KARA_TARGET` env var when both are set. Standard explicit-over-implicit rule.
  - **Default**: host target when neither flag nor env var set. Matches cargo, matches Go (Go defaults to native; GOOS/GOARCH override).
  - **One target per invocation at v1.** No `--target a --target b` multi-target sweep; users script a loop. Matches cargo + Go behavior. Multi-target-as-one-invocation adds per-target failure handling and output-dir layout complexity without proportional value at v1.
  - **No GOOS/GOARCH split.** Go's two-var form (`GOOS=linux GOARCH=arm64`) predates LLVM-triple convention. Kāra uses LLVM, so single-triple is natural; `KARA_OS` + `KARA_ARCH` would be a Go-shaped wart on a triple-native compiler.
- **Missing-toolchain failure mode: action-pointing structured diagnostic, detected on `--target` parse.** Forbid cryptic LLVM linker errors by construction — `karac` checks toolchain presence as soon as the target is resolved, before any compilation work begins. Canonical shape:

  ```
  error[E0789]: target 'linux-arm64' not installed

  To install, run:
    karaup target add linux-arm64

  Currently installed:
    - linux-x86_64 (host)
    - macos-arm64

  Run `karac targets` for the full supported list.
  ```

  Seven properties to lock:
  - **Detection point**: on `--target` parse / resolution, not on LLVM invocation. The compiler never gets to the link step with a missing toolchain.
  - **Action-pointing**: exact `karaup target add <triple>` command is in the error text, copy-pasteable. No "see the docs" misdirection.
  - **Installed-list in the error**: tells the user what they *can* use right now without running a second command. Discoverability + confidence in one shot.
  - **Pointer to `karac targets`** for the full supported set (vs just installed). Discoverability for advanced/community targets.
  - **Structured diagnostic with E-code**: `E0789` (slot to be assigned in the karac diagnostic system); JSON-parseable via `--output=json` consistent with the rest of karac's structured-diagnostic philosophy. IDE/tool consumers get the same shape they get for type errors, effect errors, ownership errors.
  - **Fallback diagnostic when `karaup` isn't found in PATH** (e.g., user installed `karac` standalone from a release binary): `E0790` — points at the karaup install URL first, then the target add command. Two-step instructions for the standalone-install case.
  - **Corrupted/incomplete install detection**: if the toolchain directory exists but checksums don't match, emit `E0791` with `karaup target reinstall <triple>` as the action. Don't silently fall through to LLVM and produce a cryptic error mid-link.
- **Cross-compile-for-test: `--no-run` at v1, full cross-execute errors with workaround, QEMU/runner integration deferred to v1.x.** Three behaviors:
  - **`karac test`** (no `--target`): runs all tests on host. Standard, unchanged from existing behavior.
  - **`karac test --target X --no-run`**: builds the test binary for the target without executing it. Validates compile + link surface (catches API mismatches, missing symbols, ABI issues for the target) without needing emulation. Matches cargo's `--no-run` pattern; familiar UX for users coming from Rust. Ships at v1 as the cross-compile test-build option.
  - **`karac test --target X`** (without `--no-run`): structured error `E0792` pointing at the workarounds. Canonical text:

    ```
    error[E0792]: cross-target test execution not supported at v1

    To verify tests compile for 'linux-arm64', use:
      karac test --target linux-arm64 --no-run

    To run tests, use --target host (the default) or run them on the
    target device directly.
    ```

  - Reasoning: (1) The 90% case is "does my server build for arm64" — backend developers want compile-validation for the target, not behavioral testing under emulation. `--no-run` handles this at low engineering cost. (2) The 10% case is library portability testing under QEMU; real audience but small, and QEMU doesn't fully match real hardware (timing, syscall behavior, SIMD). Library authors who care use real hardware in CI, not QEMU. (3) QEMU integration is multi-month work for narrow audience — fails the v1-scope test.
  - **v1.x consideration**: when user demand emerges, add `karac test --target X --runner <cmd>` matching cargo's `--runner` flag. `<cmd>` wraps QEMU, SSH-to-device, or a custom test launcher. Cross-target test execution lands as a composable extension via the `--runner` mechanism, not a forced QEMU dependency.
- **GPU + WASM targets share the `--target` surface; Phase 8.5 defines the mechanism, Phase 10 plugs in backends.** No split between `--target` (CPU) and a separate flag (GPU/WASM). Sub-decisions:
  - **Universal `--target` flag.** CPU triples (`linux-arm64`), GPU short-forms (`cuda` → `nvptx64-nvidia-cuda`), and WASM (`wasm` → `wasm32-wasip1` or similar) all flow through the same mechanism. Developers don't learn a different invocation per target type; the triple is the universal address. Confirms what's already in design.md (`--target cuda`).
  - **GPU-specific options layered as sub-flags, not as targets**: `--target cuda --gpu-arch sm_80`, `--target cuda --ptx-version 8.0`. Multiple GPU archs at once via comma-separated list (`--gpu-arch sm_70,sm_80,sm_86` for fat binary) — target-internal detail, doesn't violate the one-target-per-invocation rule from OQ3.
  - **Toolchain install mechanism reused with vendor-toolkit caveat**: `karaup target add cuda` installs Kāra's NVPTX backend (the parts we own and ship). Vendor SDKs (CUDA toolkit, ROCm, etc.) are user-installed separately — we can't redistribute proprietary vendor blobs. Missing-vendor-toolkit diagnostic follows the OQ4 pattern: `E0793: CUDA toolkit not found at $CUDA_HOME; install from https://developer.nvidia.com/cuda-downloads`.
  - **Targets and profiles are orthogonal.** `--target` selects architecture; `--profile deterministic` / `--profile embedded` selects language-level constraints. They multiply — `--target linux-arm64 --profile embedded` is a valid combination, not a contradiction. Don't conflate the two surfaces.
  - **Phase ownership**: Phase 8.5 Track 2 (this gap's home) ships the `--target` flag mechanism + first-class CPU targets + toolchain manager UX; Phase 10 ships the GPU/WASM backends that plug into this surface. Each phase owns its part; the surface is designed once and reused, not duplicated per backend.

---

## Gap 4 — Effect-driven debugging: designed-to-polished

### Current state

**Already designed** (in design.md / roadmap Phase 8):
- `std.runtime::list_tasks() -> Vec[TaskInfo]` — suspended tasks with `WaitTarget`, source location, effect summary
- `std.runtime::list_par_blocks() -> Vec[ParBlockInfo]` — active par-blocks with SpawnSiteId, worker count, per-worker source location
- `std.panic` — structured JSON crash report with panic site, panic kind, effect set, logical/provider stack, RC-fallback annotations, parallel context
- Stable SpawnSiteId per `par {}` block embedded in executable metadata
- Parent-frame reference on every par/spawn/TaskGroup worker frame
- Await-chain pointer on every suspended task pointing to its WaitTarget
- `std.tracing` spans with OTel-export-ready propagation (Phase 8 Backend Platform, P1)

**Not designed (the polish gap):**
- **No CLI renderer.** There's no `karac debug <crash.json>` or `karac inspect <pid>` that takes the metadata and produces human-readable output. The JSON is the surface today.
- **No effect-set rendering convention.** What does "the effect set at the panic site" *look like* when a user reads it? Comma-separated names? Tree? Inline source highlight? Undefined.
- **No IDE hover spec.** The LSP design (Phase 8.5 Track 3) commits to hover showing "type + effect signature" but doesn't specify how `panics(IoError) + reads(UserDB) + sends(Network)` renders in a tooltip, nor how blocked-task state shows up in a debugger sidebar.
- **No 3am-operator runbook.** Given a crash JSON file from production, what does the on-call engineer *do*? No documented workflow.
- **No live-process inspection CLI.** `std.runtime::list_tasks()` is callable from Kāra code, but there's no equivalent of `go tool stack` that attaches to a running process and dumps task state without code changes.
- **No integration design with the sampling profiler.** When Gap 2's sampling profiler ships, how do effect-set annotations attach to sample frames? Undecided.

### Why this matters

- **This is the only ops-side differentiator vs Go that doesn't take 5 years of operational maturity to build.** The metadata exists by design; the experience of using it is a tooling polish problem. Few weeks of engineering, not few years.
- **A great pitch becomes a flat pitch without rendering.** "Panic + effect set at site + parent task chain is better than goroutine name + stack" only lands if the user can *see* the effect set without parsing JSON by eye. The rendering quality determines whether this differentiator shows up in benchmarks-of-developer-experience or stays as a spec footnote.
- **First public demo will be screenshotted.** Whatever a Kāra crash looks like the day of launch becomes the canonical "this is how Kāra debugs" image on Hacker News. That image is currently undefined.
- **Compounds with Gap 2.** The sampling profiler is the *what's running* surface; effect-driven debugging is the *what went wrong* surface. They share metadata sources and need to render consistently. Designing them together is cheaper than designing them apart and reconciling later.

### Open questions

1. **CLI surface shape.**
   - `karac debug <crash.json>` (one-shot crash-report renderer)
   - `karac inspect <pid>` (attach to live process, dump task state)
   - `karac explain-panic <crash.json>` (AI-style natural language explanation of what happened)
   - Suggested: at least the first two at v1; the third is a v1.x experiment.
2. **Effect-set rendering convention.**
   - Compact: `reads(UserDB) + sends(Network) + panics(IoError)`
   - Grouped by category: resource verbs, execution verbs, panic types
   - Annotated source view: source-file panel with effect markers at call sites
   - Suggested: compact form for crash reports; annotated source view for IDE hover.
3. **Where does the rendering logic live?**
   - In `karac` itself (single binary, no extra install)
   - In a separate `kara-debug` binary (cleaner separation)
   - In the LSP (debug surface inside IDE only)
   - Suggested: in `karac` for the CLI surface (no extra install matters for ops); LSP wraps the same library code for IDE.
4. **Audience priority — operator or developer?**
   - Operator at 3am reading a crash file: needs concise, immediately actionable rendering.
   - Developer in IDE: needs rich, exploratory rendering.
   - The same metadata serves both, but different rendering modes.
   - Suggested: operator first (crash file → CLI rendering), developer second (IDE integration via LSP).
5. **Integration with `std.tracing`.**
   - `std.tracing` spans carry effect context naturally; can the crash renderer cross-reference the trace ID that was active at the panic site?
   - If so, the user can move from "panic happened here" → "this is what the request was doing across N services."
   - Open: whether v1 ships this end-to-end or stops at the panic-site rendering.

### Proposed v1 outcome

- `karac debug <crash.json>` CLI that renders: panic site source (with line + column highlight), effect set in compact form, parent task chain visualization, RC-fallback annotations, build-metadata footer.
- `karac inspect <pid>` (Linux + macOS at v1) that attaches to a running process and dumps `list_tasks()` / `list_par_blocks()` output without code changes — equivalent to `go tool stack`.
- LSP hover renders effect signature with grouped categories (resource verbs / execution verbs / panic types).
- Documented "operator at 3am" runbook in the book, with a screenshot of `karac debug` output on a real panic from one of the demos.
- This is Phase 8.5 Track 4 work (discovery / v1 ship-readiness) — could also fit under a new sub-track of Track 1 (Interactive Development) since CLI + LSP integration overlap with REPL surface.

### What this is *not*

- Not a full debugger (no breakpoints, no step-through). That's gdb/lldb territory; LLVM debuginfo emission covers it for tools that already exist.
- Not an APM (no per-request tracking, no aggregation across processes). `std.tracing` covers that.
- Not the sampling profiler (Gap 2). Different surface, shared metadata.
- Not real-time IDE integration with sub-100ms latency. The reactive LSP layer is post-self-hosting (Future section in roadmap).

### Resolutions in progress (2026-05-20)

Decisions reached during discussion; graduate as a batch into `roadmap.md` Phase 8.5 Track 4 (or new sub-track of Track 1) once Gap 4 is fully closed.

- **CLI surface: `karac debug` + `karac inspect` at v1, `karac explain-panic` at v1.x, single binary.** Three sub-commands locked:
  - **`karac debug <crash.json>` at v1** — the operator-at-3am surface. Reads a crash JSON file produced by `std.panic` (file path or `-` for stdin, e.g., `cat crash.json | karac debug -`). Renders human-readable output: panic site source line, effect set at panic, parent task chain, RC-fallback annotations, build metadata footer. `--output=json` re-emits the structured form for tooling consumers (LSP, AI agents, etc.) — same data, pretty-printed and schema-validated.
  - **`karac inspect <pid>` at v1 (Linux + macOS only)** — the live-process surface. Attaches via `ptrace` (Linux) or `task_for_pid` (macOS); reads runtime metadata leveraging the per-worker cooperation hook from Gap 2 (same surface, reused). Dumps `list_tasks()` / `list_par_blocks()` output without requiring code changes to the running program. Equivalent to Go's `go tool stack`. `--output=json` for tool consumers; `--once` (default) for one-shot snapshot; `--watch` for periodic re-dump (high incident-response value at small additional cost).
  - **Windows `karac inspect` deferred to v1.x.** Windows debug APIs are different enough (no ptrace; uses `DebugActiveProcess` / `ReadProcessMemory`) to be a multi-week port. Add when Windows backend usage warrants it.
  - **`karac explain-panic <crash.json>` deferred to v1.x.** AI-style natural-language explanation of crashes. Deferred because: (1) requires committing to an AI provider integration in the compiler binary, a heavier architectural decision than v1 needs; (2) explanation quality depends on data quality — build the structured rendering foundation first, layer AI on top.
  - **Single `karac` binary, not separate `kara-debug`.** Reason: ops surface needs to work with whatever's already installed; making `karac inspect` require a separate `kara-debug` install defeats the "works with what you have" promise. Library logic lives in a crate the LSP also depends on (per OQ3), but the *binary* is `karac`.
- **Effect-set rendering: three modes, JSON as structured root.** Single rendering library produces three forms from one structured representation:
  - **Compact (CLI / crash reports)**: one-line, scannable, matches source-declaration syntax. Example: `effects: reads(UserDB) + sends(Network) + panics(IoError)`. The `+` is the canonical effect-combination operator from `design.md` — users see the same shape they wrote.
  - **Grouped (IDE hover)**: multi-line, categorized into **Resource** (reads/writes/sends/receives/allocates), **Execution** (blocks/suspends), **Panic** (panic types). Leverages vertical space the CLI doesn't have. Omit empty groups (don't render "Execution: (none)" — visual noise).
  - **Annotated source view**: NOT at v1. v1.1 IDE feature (effect markers in gutter, click-to-explore). The compact + grouped forms cover v1 needs; annotated source is an enhancement, not a foundation piece.
  - Sub-decisions:
    - **Stable ordering**: group order is Resource → Execution → Panic; within each group, alphabetical by verb, then alphabetical by resource name. Deterministic so the same effect set always renders identically — load-bearing for diffability (`karac query effects --diff` between revisions doesn't show spurious reordering).
    - **Parameter syntax matches source**: `reads(UserDB)` single, `reads(UserDB, OrderDB)` multi-resource, `panics(IoError | NetworkError)` union — mirror exactly what the user wrote.
    - **Empty-effects rendering**: `effects: (none)` in compact form; `Effects: (pure)` in grouped form. "pure" carries meaningful information (no resource access, no panic, no blocking) — better than "(empty)" or "(no effects)".
    - **Colors when TTY, stripped when piped**: resource verbs cyan, execution verbs yellow, panic types red on TTY; auto-detect via `isatty`. Honor `--no-color` flag and `NO_COLOR` env var. JSON output never has color codes (tool consumption).
    - **JSON form is the structured root** that drives both renderings:
      ```json
      "effects": {
        "resource": [{"verb": "reads", "params": ["UserDB"]}, ...],
        "execution": [],
        "panic": [{"verb": "panics", "params": ["IoError"]}]
      }
      ```
      Both compact and grouped renderers consume this; tools wanting raw data read it directly. Single source of truth — there's exactly one representation; rendering layers transform it for display.
- **Rendering logic lives in a workspace crate shared by `karac` binary and LSP server.** Architectural shape:
  - **Crate organization**: extend the existing structured-diagnostic infrastructure (the same machinery that powers compile-time error rendering — source-span highlighting, color/no-color logic, terminal width handling, ANSI escape codes) to cover runtime-diagnostic rendering. Don't fork into a separate `karac-debug-rendering` sibling crate; load-bearing primitives are shared between compile-time and runtime diagnostics. If a named crate doesn't exist today, create one (e.g., `karac-render`) covering both scopes.
  - **API exposes structured + rendered forms together**:
    - `render_crash_report(report: &CrashReport, opts: RenderOpts) -> String`
    - `crash_report_to_json(report: &CrashReport) -> serde_json::Value`
    - `render_effects(effects: &EffectSet, mode: EffectRenderMode) -> String` (`mode` selects compact/grouped/annotated)
    - LSP server calls the same functions and routes output into LSP-shaped responses (hover content, diagnostic items) rather than stdout. Same library, different surface.
  - **JSON-as-root principle from OQ2 applies here too**: structured form is the authoritative representation; rendered forms are projections. Every diagnostic has a JSON serializer; renderers are convenience layers on top.
  - **AI-agent integration is out of scope** for this crate. The crate provides the structured + rendered data; `karac explain-panic` (v1.x) would consume the JSON and call into an AI provider — that's a separate concern.
  - **Non-Rust LSP future is via subprocess + JSON, not FFI.** If the LSP is ever reimplemented in a non-Rust language, the integration path is `karac --output=json` as a subprocess + language-side pretty-printing. The JSON contract is stable; the Rust crate is an implementation detail. Don't design for cross-language FFI at v1.
- **Audience priority: operator-first, developer-second.** When engineering budget is tight, polish goes to the CLI / crash-report surface first. Both surfaces ship at v1; the *differentiation* lives in the operator-facing one. Reasoning:
  - **The 3am-pager argument is the v1 pitch.** Falling short on operator UX means the "compile-time elimination + better runtime debugging when things break" pitch stays as a spec footnote. The screenshot that goes on the launch blog post is from `karac debug`, not from the IDE hover.
  - **Blast radius is asymmetric.** A confusing IDE hover wastes a minute; a confusing crash report at 3am extends an incident by hours. Operator UX has higher consequences from bad rendering.
  - **CLI is the foundational shape**; IDE/LSP layers on top of the same structured-data root (per OQ3). Build CLI first; LSP integration wraps a working renderer rather than co-designing chicken-and-egg.
  - **Concrete polish allocation**:
    - **CLI gets differentiation polish at v1**: source-span highlighting with line context, parent-task chain as a visual graph (e.g., `└──▶ fetch_user_dashboard ──▶ par_block@spawn_site_42`), effect-set rendering with TTY colors per OQ2, RC-fallback annotations with explanations.
    - **IDE/LSP at v1 ships functional, not flashy**: compact form on hover, grouped form on extended hover, structured-diagnostic items in problems pane. Annotated source view, click-to-explore, and rich exploratory UI are v1.1 work (already deferred in OQ2 annotated-source).
  - **Engineering sequencing** (cheapest-to-most-valuable order):
    1. Structured-data root (CrashReport, TaskInfo, ParBlockInfo JSON schemas).
    2. CLI renderer (`karac debug`, `karac inspect`) — operator surface.
    3. LSP integration that wraps the renderer.
    4. 3am-operator runbook in the book.
  - **3am-operator runbook is a v1 deliverable, not a v1.x add.** Short and starter-shaped, not exhaustive. Contents: (a) entry point ("when you get paged with a crash JSON, run `karac debug <file>` first"); (b) 3–5 worked examples (panic on resource, panic on RC fallback, par-block stall) showing the actual rendered output; (c) common patterns and what to check next (`panics(IoError) + reads(UserDB)` → check DB connectivity); (d) escalation ("how to use `--output=json` to feed an AI agent or paste into a bug report"). The runbook grows from incident data once Kāra is in production; starter version is enough at launch.
  - **Decision-by-default: when a feature could ship in either surface but not both, ship in CLI first.** Example: when `karac explain-panic` (v1.x) lands, it lands as a CLI subcommand before becoming an IDE feature. Same rule applies to any future debug-surface enhancement.
- **`std.tracing` integration at v1: crash report carries trace_id + span_id when active.** The crash report bridges from "panic happened here" to "this is the request flow across N services." Cheap to ship (one runtime hook + two schema fields + one render line), high value, std.tracing is already a Phase 8 P1 v1 commitment so the dependency is met. Concrete shape:
  - **Panic handler reads active span from std.tracing context.** When `std.panic` constructs the crash report, it asks std.tracing "is a span active?"; if yes, capture `trace_id` and `span_id`. If no (or std.tracing not compiled in), leave fields absent — graceful, no warning.
  - **Crash report schema gains optional `tracing` block**: `{"tracing": {"trace_id": "abc123...", "span_id": "1234..."}}`. Field names match OTel convention exactly; consuming tools map directly. Field is omitted entirely when no active span.
  - **CLI renders as a separate line** when present: `trace: abc123def4567890 (span: 1234567890abcdef)`. Copy-pasteable into Jaeger / Tempo / Datadog / any OTel backend. Skip the line entirely when absent — don't render `trace: (none)` (visual noise).
  - **NOT at v1**:
    - No URL construction (`view in Jaeger: <url>`). Requires per-org configuration that's premature; raw trace ID is enough.
    - No trace context across crash boundaries. Single-process panic context only — don't walk to parent task's trace context or stitch across processes.
    - No retroactive span enrichment from crash data (annotating spans with "this span experienced a panic"). Adds bidirectional coupling between std.tracing and std.panic; keep one-directional at v1.
    - No hover/IDE rendering. LSP hover is static analysis — it doesn't see live trace IDs. If trace ID in IDE is wanted later, it's a v1.x debug-session feature, not hover.

---

## Cross-cutting question — which of these are v1 ship gates?

All four resolved as v1 ship gates (2026-05-20). The original "Likely yes" framing was hedging language while decisions were open; the resolutions sections under each gap have now locked the deliverables, scope, and v1 vs v1.1 split.

| Gap | Ship-gate at v1? | Phase placement | Engineering size |
|---|---|---|---|
| Compile speed CI gate | **Yes (resolved)** — easiest dismissal vector, smallest engineering | `bench/compile_speed/` in main package + CI; Phase 8.5 Track 4 (discovery) or new track | 1–2 weeks for corpus + harness + CI; ongoing tuning |
| Sampling profiler | **Yes (resolved)** — needed for the 3am-page pitch to land | Phase 8 § Backend Platform, after `std.http` / `std.tracing` spine | 4–8 weeks for signal sampler + pprof emission + opt-in HTTP endpoint |
| Cross-compile UX | **Yes (resolved)** — first-five-minutes UX, smallest design effort | Phase 8.5 Track 2 (build tooling) — explicit deliverables, not implicit | 2–4 weeks for `karaup target add` + bundled-sysroot infra + diagnostics |
| Effect-driven debugging polish | **Yes (resolved)** — the only ops-side differentiator buildable in weeks, not years | Phase 8.5 Track 4 or new sub-track of Track 1 | 3–6 weeks for `karac debug` + `karac inspect` + LSP hover rendering + runbook |

All four together: ~10–18 weeks of engineering, parallelizable, on top of the existing Phase 8 spine. Not v1-blocking *as scope* — but each is a credibility-blocking gap if missing. Two items deferred to v1.1: wall-time profile + execution tracer (both extend the v1 sampling-profiler foundation without breaking it).

### Related items that came up but are *not* design gaps

Two items from the original Go-comparison discussion that look like gaps but aren't design work:

- **Compile-time elimination of nil panics / race conditions / unhandled errors as a pitch.** The language already does this. Work is README + book + launch-blog framing ("lead with the pager argument, not the dev-loop argument"). Belongs in the launch-narrative track, not a design brainstorm. Worth tracking somewhere — possibly a `docs/pitch.md` or as an item in Phase 8.5 Track 4 — but not here.
- **LLVM-optimized binary perf-per-server vs Go.** This is benchmarking/validation work, not design. The 1M-connection demo already commits to perf measurement; a per-server perf ratio against Go on the same workload is a free add-on. The work is "publish the number," not "design how to measure." Belongs in the bench infrastructure as a v1 published metric, not as a brainstorming entry.

## Resolution criteria

This doc closes (moves to `archive/`) when each of the four gaps has:

1. A concrete deliverable specified in `roadmap.md` (a new line item or sub-section, not just a mention).
2. A phase / track placement.
3. A "Done when" criterion measurable at v1 launch.
4. For the compile-speed gate specifically: a target-corpus shape, a comparison baseline (`rustc -O` as CI gate; `go build` and `clang -O2` as published reference measurements, not CI gates), and a regression-threshold % committed (locked at 30% initial, ≤5% long-term target).
5. For effect-driven debug polish specifically: a canonical rendering example committed (the screenshot that goes in the launch blog post).

The two related-but-not-design items (compile-time-bug-elimination pitch, LLVM perf-per-server measurement) close when each is filed in its appropriate non-design track (pitch doc, bench infrastructure) — they don't need design resolution to leave this doc.

### Closure status (2026-05-20)

- **Criteria 1–4: met by the Resolutions in progress sections under each gap.** Design decisions, scope, phase placement, target-corpus shape, baselines, and threshold are all locked. Graduation step is mechanical: copy resolutions into `roadmap.md` as line items, `design.md` for module specs (e.g., `std.runtime.profiler` + `std.http.profiler` namespacing), and a new bench-instruction doc at `karac-rust/bench/README.md`.
- **Criterion 5 remains open as a v1 implementation deliverable.** The canonical rendering example (the launch-blog screenshot) requires the `karac debug` CLI to exist and a real demo panic to render — neither exists yet. This criterion closes during Phase 8.5 Track 4 implementation, not in this brainstorming doc.
- **Related items**: pitch doc + LLVM perf-per-server measurement remain non-design follow-ups; not blocked on this doc.
- **Cross-references created during resolution**: `kara-katas/PLAN.md` (corpus coverage roadmap), `kara-katas` backend-service kata elevated to v1-required, `karac-rust/bench/compile_speed/` directory + corpus + CI gate to be scaffolded during graduation.

Until graduation lands the resolutions into `roadmap.md` / `design.md`, the four design gaps remain open *as implementation work*; design surface is closed.
