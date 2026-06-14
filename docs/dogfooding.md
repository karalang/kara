# Kāra Dogfooding Projects

The V1 program roster. Each entry is a real product built *in* Kāra — not a toy
— that serves two jobs at once: it **dogfoods the compiler** to surface bugs and
harden the language under real load, and it **proves a differentiating
capability** to an outside audience. These are not a post-launch backlog and
they are not "ideas": **building them is part of getting to a V1 launch.** The
list grows as new sharp edges are found — it does not shrink.

The dogfooding purpose is load-bearing. A demo's job is not only to look good in
a recording; it is to exercise a real slice of the language hard enough that the
gaps, friction, and miscompiles fall out (cf. `feedback_katas_are_bug_finders`,
`feedback_no_workarounds_fix_compiler` — when a project hits a `karac` wall, the
fix is in the compiler, and the project's shape is the test case). Each program
is selected to showcase a capability other languages cannot replicate without
significantly more programmer effort. The audience is: other language designers,
systems programmers, AI researchers, and early adopters.

Sequencing below (the build order) is about *when within the pre-launch runway*
each is built, not whether it ships before or after launch — all are V1-scope.

**Projects are keyed by name, not number.** The roster reorders and grows
without renumbering, and a name never collides with the *separate* launch-track
numbering used in `docs/roadmap.md` / `phase-6-runtime.md` ("Flagship Demo 1/2/3"
= idle-holder / Parallax / data-pipeline), which is a different namespace.

---

## Contents

- [Roster](#roster) — the at-a-glance status table (single bookkeeping surface)
- [Demo planning](#demo-planning) — five pillars + practical filters
- [Tier 1 — Must-Build](#tier-1--must-build-core-story-highest-impact) — Parallax, Mend, Slipstream
- [Tier 2 — High-Value](#tier-2--high-value-compelling-story-focused-audience) — Cartographer, Husk, Weave, Tangle, Chronicle
- [Tier 3 — Domain-Specific](#tier-3--domain-specific-strong-for-specific-audiences) — Relay, Forge, Iris, Plume, Fathom
- [Build Sequence](#build-sequence)
- [Reusable Scaffolding](#reusable-scaffolding)
- [Adding a project](#adding-a-project) — the entry template
- Appendix — Parallax bench design lock (Slice E)

---

## Roster

At-a-glance status. **This table is the single bookkeeping surface** — to add a
project, add one row here and one row to [Build Sequence](#build-sequence); the
per-project sections below hold the design. Status legend: ✅ shipped ·
🔨 in progress · ⬜ planned.

| Project | Proves | Status | Built when (gate) | Tier |
|---|---|---|---|---|
| **Parallax** | Auto-concurrency without `async`/coloring (fan-out + join) | ✅ shipped | auto-par codegen + HTTP FFI | 1 |
| **Mend** | AI-first: structured compiler output as a machine fix-loop | ✅ shipped | `karac … --output=json` + `karac fix` | 1 |
| **Slipstream** | Auto-concurrency + SoA layout + one source on CPU/GPU | ⬜ planned | Phase 11 (CPU) · Phase 10 (GPU) | 1 |
| **Cartographer** | Effect graph as a live architecture artifact | ✅ shipped | whole-program query + live WASM studio (D3 + Monaco) + per-callee blocking attribution — all design points covered | 2 |
| **Husk** | `kernel` profile — no heap/panic/std, MMIO, ISRs | ⬜ planned | v8 hardware gaps (`#[repr]`, `#[interrupt]`, asm) | 2 |
| **Weave** | Refinement types + contracts + effects together | ✅ shipped (CSV cut) | refinement+contracts (CSV) · `Pool[T]`+TLS+tracing (service) | 2 |
| **Tangle** | No `'a` at the cases that force `Rc<RefCell>`/arenas elsewhere — graphs, back-pointers, undo/redo; every RC escalation surfaced | ✅ shipped | ownership + `karac query ownership` (done) | 2 |
| **Chronicle** | Self-hosting; Kāra's own tooling explains Kāra — *and* the ownership model holds across the whole compiler, zero lifetime annotations | ⬜ planned | Phase 10/12 self-hosting | 2 |
| **Relay** | Effect-driven event-loop networking (no `async fn`) | ⬜ planned | Phase 6 v1.1 network event loop | 3 |
| **Forge** | `embedded` profile firmware on a real MCU | ⬜ planned | v8 hardware gaps | 3 |
| **Iris** | One source → native + WASM, no port | ⬜ planned | Phase 10 WASM target | 3 |
| **Plume** | Parallel browser compute driven by event streams — no `async`/coloring | ⬜ planned | Phase 10 event-stream surface (`animation_frames` + event-data channels) + framebuffer-blit host fn | 3 |
| **Fathom** | Browser × multi-core pixel compute, one source | ✅ shipped (non-interactive cut) | `animation_frames` + `Vec.as_ptr` blit host fn (built) · SIMD kernel + interactive pan/zoom (event-data channels) = follow-ups | 3 |

---

## Demo planning

A strong Kāra demo makes one or more of these five pillars tangible. Each pairs the user-facing win with the compiler/tooling moment that delivers it:

1. **Effects-as-signature** — A public function's signature tells reviewers what real-world resources it touches. *Show:* `karac query effects` on the codebase; public effect diffs between revisions.

2. **Provider injection for tests** — Tests replace `Clock`, `Env`, `RandomSource`, databases, etc. without dependency-injection boilerplate. *Show:* deterministic tests run against injected providers; identical results every run.

3. **Auto-concurrency from effects** — The compiler says "these operations are independent; I ran them together; here is why." *Show:* independent resource reads parallelized with no annotations; the concurrency report explains the decision.

4. **Profiles as target constraints** — A `deterministic` or `embedded` profile rejects the wrong code mechanically. *Show:* the profile failing a build with a clear effect-based error.

5. **AI-readable diagnostics** — AI agents consume structured compiler output and produce safe, scoped edits. *Show:* `karac build --output=json` feeding an LLM that fixes its own Kāra code.

A grand-tour demo built on a small service or pipeline (`Clock`, `Env`, `FileSystem`, `Network`, plus a user-defined database resource) can cover all five. Most demos below focus on one or two pillars; the suite together should cover them.

On top of the pillar lens, every demo should also pass these practical filters:

- Hard to replicate in another language without significantly more programmer effort.
- Achievable by a small team (1–3 people, 1–3 months).
- Visible, concrete output — not just a benchmark number.
- Tells a complete story in under 5 minutes.

---

## Tier 1 — Must-Build (core story, highest impact)

### Parallax — Auto-Concurrency API Gateway

**Primary capability:** Auto-concurrency without `async fn` or function coloring.

**What it is:** A real HTTP API server that aggregates data from multiple
upstream services — the canonical "fan-out and join" pattern that every
backend language handles differently.

```kara
// The entire concurrency story in one function.
// No goroutines. No async fn. No .then() chains. No colored functions.
pub fn get_dashboard(user_id: UserId) -> Dashboard
    with reads(UserDB, OrderDB, NotifDB, RecommendDB)
{
    let profile       = fetch_profile(user_id);       // reads(UserDB)
    let orders        = fetch_recent_orders(user_id);  // reads(OrderDB)
    let notifications = fetch_notifications(user_id);  // reads(NotifDB)
    let recommended   = fetch_recommendations(user_id);// reads(RecommendDB)

    // Four reads on four different resources — zero data dependencies.
    // Compiler runs all four concurrently. No annotation needed.

    build_dashboard(profile, orders, notifications, recommended)
}
```

**What the demo shows:**
1. The Kāra code above, as-written — clean, sequential-looking.
2. `karac build --concurrency-report` output: shows the four calls in a
   `parallel_group`, explains *why* they are parallel ("no data dependencies;
   no effect conflicts: different resources, all reads").
3. Side-by-side benchmark: the same server in Go with explicit goroutines and
   channels, the same server in Node.js with `Promise.all`. Kāra matches their
   throughput with simpler code.
4. One deliberate change: make two functions write to the same resource. Show
   the compiler's concurrency report flip from `parallel_group` to `sequential`
   with a clear explanation.

**Why it's compelling:** Every developer has written `Promise.all` or
`goroutine + channel` boilerplate for this. Kāra gets the same result from
plain sequential code — the compiler does the work. The concurrency report makes
the decision *visible and auditable*, which no other language offers.

**Effort:** Medium. HTTP server layer needed (FFI or a thin Kāra wrapper).
Benchmark harness is straightforward.

**Bench & build history:** the three-language benchmark harness, its settled
design forks, and the Kāra-vs-Rust measurement story are recorded in the
**Parallax bench design lock (Slice E)** appendix at the end of this doc, and in
[`examples/parallax/bench/README.md`](../examples/parallax/bench/README.md).

---

### Mend — AI Writes Kāra

**Primary capability:** AI-first tooling — structured compiler output as a
machine-readable feedback loop.

**What it is:** A live demonstration where an LLM (Claude or GPT-4) writes,
diagnoses, and fixes Kāra code autonomously using only `karac build --output=json`.
No human in the loop after the initial prompt.

**The loop:**

```
User prompt → LLM writes Kāra code
→ karac build --output=json
→ LLM reads structured errors + fix diffs
→ LLM applies diffs mechanically
→ repeat until clean build
```

**What the demo shows (live, in a terminal):**
1. User types: *"write a function that reads users from a database and sends
   them a welcome email concurrently."*
2. LLM writes reasonable but imperfect Kāra: wrong effect annotations, one
   missing ownership annotation.
3. `karac build --output=json` returns structured diagnostics including
   `"inferred_effects": ["reads(UserDB)", "sends(EmailService)"]` and a
   machine-applicable diff.
4. LLM applies the diff verbatim — no re-reasoning, no guessing.
5. Clean build in 2-3 iterations.

**The before/after is the story:** Show the same task in Python. The LLM
writes code that *looks* correct but has a subtle concurrency bug (shared
mutable state, race condition). Python's tools cannot catch it statically.
Kāra's compiler catches it before the code ever runs.

**Why it's compelling:** This is the thesis of the language — "a language
designed to be written by AI." The demo makes it literal. The structured
output isn't a nice-to-have; it's the mechanism. No other language has an
effect system + ownership checker + machine-applicable fix diffs in a single
JSON envelope.

**Effort:** Small to Medium. The demo framework is mostly a shell script that
calls `karac` and pipes output to an LLM API. The hard part is picking
examples that show the interesting cases without being contrived.

---

### Slipstream — Interactive Wind Tunnel

**Primary capability:** Auto-concurrency of sequential code; layout blocks for
cache-efficient SoA access; same code runs on CPU and GPU.

**What it is:** A real-time 2D wind tunnel simulator. Air flows left to right
around a NACA wing cross-section; the user rotates the wing and watches
aerodynamic stall develop live. Runs the Lattice Boltzmann Method (LBM D2Q9)
— the standard algorithm for interactive computational fluid dynamics, with
reference implementations in every major language. The physics is solved, the
algorithm is 40 years old. The demo is about what Kāra does to it.

```kara
// LBM node — 9 velocity distribution functions (D2Q9 model)
struct LbmNode {
    f0: f32, f1: f32, f2: f32, f3: f32, f4: f32,
    f5: f32, f6: f32, f7: f32, f8: f32,
    flags: u8,   // solid=airfoil boundary, fluid=open
}

// SoA layout: all f0 values contiguous, then all f1, ..., then all f8.
// The streaming step walks one velocity component across all nodes —
// SoA turns that into a sequential memory scan; LLVM auto-vectorizes it.
layout grid: Vec[LbmNode] {
    group distributions { f0, f1, f2, f3, f4, f5, f6, f7, f8 }
    group geometry      { flags }
}

// Collision + streaming — pure per-node, no cross-node writes in collision.
// Written as a plain sequential function. The compiler sees that
// lbm_step and advect_tracers read the same resource (FluidGrid) with no
// writes between them — reads/reads never conflict — and runs them in a
// parallel_group automatically. No par {} written anywhere.
fn simulate_tick(world: mut ref World) with writes(FluidGrid) {
    let new_grid    = lbm_step(world.grid, world.viscosity);   // reads(FluidGrid)
    let new_tracers = advect_tracers(world.tracers, world.grid); // reads(FluidGrid)
    world.apply(new_grid, new_tracers);
}

// GPU path: identical kernel, dispatched to the GPU.
// Same source — no rewrite, no port.
#[gpu]
fn lbm_step_gpu(grid: ref Vec[LbmNode], viscosity: f32) -> Vec[LbmNode]
    with reads(FluidGrid), allocates(GpuBuffer)
{ ... }
```

**What the demo shows:**
1. A window: tracer particles stream left to right around a wing. A turbulent
   wake forms behind the trailing edge. Flow is smooth and continuous.
2. Drag a slider to rotate the wing angle of attack — at around 15° the flow
   separates and the wake explodes into chaotic swirling. Stall. Drag the
   slider back — flow reattaches.
3. Press `C`/`G` to switch between CPU and GPU paths. FPS changes; the
   fluid behavior is identical. The on-screen overlay shows:
   `Path: CPU | FPS: 60 | Layout: SoA | Cache miss: 0.4%`
4. Press `L` to toggle layout off (AoS). The cache miss counter jumps — and
   on a large enough grid the framerate visibly drops. Press `L` again to
   recover.
5. `karac build --perf-report` shown in a terminal beside the window: the
   `simulate_tick` body appears as a `parallel_group` containing `lbm_step`
   and `advect_tracers`, with the compiler's reasoning: *"reads(FluidGrid) ×
   reads(FluidGrid) — no conflict; no data dependency; parallelized."*

**Why it's compelling:** One sentence explains it to anyone: *"This is what a
$10M wind tunnel does — in real time on your laptop."* The angle-of-attack
slider makes it a tool, not a screensaver: you ask a question (will this wing
stall?) and the simulator answers it. Engineers and non-engineers both
understand it immediately. The C/G/L toggles deliver the same experience as
the original particle demo — abstract concepts made visible through numbers
that move on screen — but grounded in something real rather than decorative.

The Kāra story lands without needing explanation: point at `simulate_tick` and
ask "where is the threading code?" There isn't any. The compiler found it.

**Effort:** Medium. The LBM algorithm is well-understood with clean reference
implementations (Dan Schroeder's JavaScript version is ~300 lines). The CPU
demo is unblocked after Phase 11 completes (auto-concurrency codegen lands in
Phase 8 floor, but the full stdlib + FFI for SDL2 rendering need the long-tail
in Phase 11). The GPU path requires Phase 10 but can be added later without
changing the Kāra source — only the dispatch call changes.

**Browser edition (cross-target capstone).** The same `simulate_tick` source
also targets the browser: `--target=wasm_browser --features wasm-threads` runs
the fluid kernel across a Web Worker pool, with `animation_frames()` driving the
loop, the angle-of-attack slider arriving as an event-data channel, and a
`canvas.put_pixels` blit replacing SDL2 — no Kāra-source change, only the
render/input host layer swaps (the cross-target story **Iris** proves, applied
to the flagship). This is the full continuous-loop browser demo; **Plume**
(below) is its tractable precursor on the identical spine. Gated on the same
Phase-10 event-stream surface as Plume/Fathom.

---

## Tier 2 — High-Value (compelling story, focused audience)

### Cartographer — Effect Graph Visualizer

**Primary capability:** `karac query` as a developer tool; effect types as
architecture documentation.

**What it is:** A web-based visualization of a program's effect graph. Feed
it any Kāra project; it renders an interactive graph showing every function,
its effects, and which functions can run concurrently vs must serialize.

> **Built (compiler half) — 2026-06-14.** Shipped at
> [`examples/cartographer/`](../examples/cartographer/). The dogfood drove the
> real compiler gap: `karac query effects` / `karac query concurrency` only took
> a per-function target, so there was no way to ask for the whole graph. Both
> now accept a bare `<file>.kara` target and emit a whole-program envelope —
> `effects` gives `{functions:[{function, line, is_test, inferred_effects,
> declared_effects}], calls:[{caller, callee}]}` (effect-colored nodes **plus**
> the call-graph edges, which previously only `affected-by` exposed and only as
> reach-from-one-node), `concurrency` gives every analyzed function's parallel
> bands. Keys join 1:1 across both envelopes and `affected-by`. The example
> ships a runnable subject service (`src/service.kara`), a `cartograph.sh` that
> regenerates the graph from the compiler, and a self-contained static SVG
> `viewer.html` (no D3/Monaco/WASM) as the proof renderer. Regression tests:
> `tests/cli.rs::test_query_effects_whole_program_emits_nodes_and_call_edges`,
> `tests/concurrency.rs::test_cli_query_concurrency_whole_program`.
>
> **Built (frontend half) — 2026-06-14.** The live studio `studio.html`: edit
> Kāra and the effect graph redraws on every keystroke, with the compiler running
> *in the browser tab* as WASM (no server round-trip, no local `karac`). The
> whole-program JSON builders moved to a wasm-safe `src/effect_graph.rs` with a
> `cartograph_json(source)` library entry point; the CLI emitters delegate to it,
> so CLI and studio are byte-identical (pinned by
> `tests/cli.rs::test_cartograph_json_matches_cli_query_output`). The
> `karac-playground` WASM crate gained a `cartograph` export; the studio is a
> Monaco editor (Kāra highlighting + live compiler-diagnostic squiggles) + a D3
> force-directed graph (effect-colored nodes, dashed-gold parallel-band rings,
> draggable, click-for-detail) driven by it. `studio.sh` builds the wasm + serves
> it. This is the **Iris cross-target story applied to a real tool** — the same
> analysis source runs native (CLI) and wasm (browser); building it surfaced no
> wasm bugs (the frontend analysis pipeline is wasm-clean).
>
> **Built (blocking attribution) — 2026-06-14.** Closes the last design point.
> The concurrency analysis now emits **serialization points** (the inverse of
> parallel bands): for every statement pair that can't parallelize, the cause,
> the resource, and — for an effect conflict — the **specific callee** whose
> effect forced it (`StmtEffect` gained a `source_callee` back-link;
> `conflict_detail` attributes each conflict). `karac query concurrency <file>`
> emits `serialization_points:[{statements, reason, resource, blocking_callees}]`;
> inverting `blocking_callees` across functions answers "which callers does `f`
> block" — surfaced in both viewers' detail panels (`record_access` blocks
> `double_audit` on `AuditLog`; a `reads`/pure fn blocks nothing). Regression:
> `tests/concurrency.rs::test_cli_query_concurrency_serialization_points_attribute_blocking_callee`.
> **Cartographer's design points are now all covered.**

**What the demo shows:**
1. Load a medium-sized Kāra project (say, a 500-line HTTP service).
2. The visualizer shows: green nodes (pure functions), blue nodes (reads),
   orange nodes (writes), edges (call graph), parallel bands (auto-concurrency
   groups).
3. Click a function: see its declared effects, inferred effects, and which
   callers are blocked from parallelizing because of it.
4. Edit the code live (Monaco editor embedded): change an effect annotation,
   watch the graph update.

**Why it's compelling:** This makes the effect system *visible*. Most
programmers can't see the concurrency structure of their program. Kāra + this
tool makes it a first-class architectural artifact, not a runtime observation.
The graph is also the best possible explanation of what the effect system does
for new users.

**Effort:** Medium. `karac query effects` and `karac query concurrency` provide
all the data. The visualizer is mostly a frontend (D3.js or similar). Compile
Kāra to WASM so the tool runs in-browser without a server.

---

### Husk — Minimal Kernel

**Primary capability:** `kernel` profile — no heap, no panics, no std, inline
assembly, MMIO, interrupt handlers.

**What it is:** A tiny OS kernel that boots on QEMU (RISC-V or ARM). Keyboard
input, VGA text output, a trivial shell with three commands. Roughly 1,000
lines of Kāra.

```kara
// This is real systems code — no std, no heap, hardware I/O via effects.
#[interrupt(TIMER)]
fn timer_isr() with writes(Hardware) {
    TICKS.fetch_add(1, Release);
    clear_timer_interrupt();
}

pub fn kernel_main() with writes(Hardware, Stdout) {
    init_uart();
    install_interrupt_vectors();
    enable_interrupts();

    loop {
        let line = read_line();    // reads(Hardware) — UART
        shell_dispatch(line);
    }
}
```

**What the demo shows:**
1. QEMU boots. Text appears on screen. You type commands.
2. `karac build --output=json` for the kernel — show the effect graph:
   every function's hardware access is explicit. The ISR has `writes(Hardware)`,
   the memory allocator has `allocates(Heap)` suppressed (forbidden by profile).
3. Deliberately try to use `Vec` (heap-allocated) in kernel code — show the
   compile error: "allocates(Heap) is forbidden by the `kernel` profile."
4. Show `karac query stack kernel_main` — static stack depth analysis, no
   surprises at runtime.

**Why it's compelling:** "Can it boot?" is the most credible systems language
question. A working kernel demo answers it definitively. It also showcases
the `kernel` profile as a practical, useful feature — not a theoretical one.

**Effort:** Large. Requires completing the `#[repr]`, `#[interrupt]`, inline
asm, and volatile memory access gaps from the hardware analysis (v8). This
demo is the validation test for those features.

---

### Weave — Data Pipeline with Verifiable Invariants

**Primary capability:** Refinement types + contracts + effect types together.

**What it is:** An ETL pipeline that transforms data from raw CSV through
validation, enrichment, and aggregation. Every stage's preconditions and
postconditions are declared and checked; every stage's data access is tracked
by the effect system.

```kara
// Refinement types: the type system enforces that invalid data can't
// reach downstream stages. NOTE the constraint language admits only PURE
// ZERO-ARG methods on self — `self.contains("@")` (an argument-bearing call)
// is NOT expressible (GAP-W1), so the bounded-length invariant lives in the
// refinement and the "@"-structure check lives in parse_row's body.
type BoundedText   = String where self.len() >= 1 and self.len() <= 254
type PositivePrice = f64   where self > 0.0
type NonEmpty[T]   = Vec[T] where self.len() > 0

// Effect inference makes parse_row carry `panics` (from `fields[i]` indexing),
// so the honest public signature declares it (GAP-W5) — not "pure".
fn parse_row(raw: String) -> Result[ValidatedRow, ParseError]
    with panics
    ensures(result) match result {
        Ok(row) => row.price > 0.0 and row.qty > 0,
        Err(_)  => true,
    }

fn enrich_row(row: ValidatedRow) -> EnrichedRow
    with reads(CurrencyDB)                 // needs live exchange rates
    ensures(result) result.qty == old(row.qty)  // re-prices, never drops qty

fn aggregate(rows: NonEmpty[EnrichedRow]) -> Summary
    requires rows.len() > 0
    ensures(result) result.row_count == rows.len()
```

> **Built (CSV cut) — 2026-06-13.** Shipped at
> [`examples/weave/`](../examples/weave/) as a single self-contained
> `src/main.kara` running via `karac run` (tree-walk interpreter); see its
> README for the run command + expected output. The build surfaced six
> findings (GAP-W1…W6) — the dogfood's load-bearing job. **Fixed:**
> `String.split` (interpreter + typechecker + codegen); the
> missing-effect-declaration diagnostic, which had been suggesting an
> un-parseable fix-it (`Add: allocates(Heap), panics()` — comma-separated,
> empty-parens, undeclarable `Heap`); and (2026-06-13 follow-up) the
> `allocates(Heap)` declarability knot — resolved spec-conformantly by
> exempting the substrate effect from the must-declare set, so an allocating
> pub fn needs no `with` clause (design.md § Effect Substrate); and (2026-06-13
> follow-up) **user `impl Display`** — the operator-trait gate is lifted so user
> types render via a real `impl Display { fn to_string }` (`ParseError` in this
> example now uses one; `f"{err}"` dispatches to it across interpreter +
> codegen). That fix surfaced and fixed a deeper pre-existing interpreter bug
> (a constructor-pattern binding shadowed by an in-scope unit-variant local);
> and (2026-06-13 follow-up) **multi-module `karac run`** — `cmd_run` now merges
> a project's sibling modules into a super-program before interpreting, so
> cross-module calls work via `karac run`; and (2026-06-13 follow-up) **codegen
> `String.split`** — a runtime out-param helper (`karac_runtime_string_split`)
> builds the `Vec[String]` so `karac build` (native) handles split too. All six
> findings' actionable fixes have now landed; and (2026-06-13 follow-up)
> **Weave now `karac build`s to a native binary** whose output is byte-identical
> to `karac run` — the first dogfood to drive refinement types + contracts +
> effects + provider injection + struct-variant `impl Display` through the full
> codegen path. Getting there surfaced and fixed **seven** distinct codegen bugs
> (see the codegen-build entry below). **All Weave follow-ups are now closed** —
> the last open one, wasm `String.split`, landed 2026-06-13 (B-2026-06-13-16).
>
> Resolved follow-ons (design record):
> - [x] **Weave `karac build` (full codegen path) — ✓ RESOLVED 2026-06-13.**
>   Building Weave (not just `karac run`) drove the refinement + contract +
>   effect + provider + struct-variant-Display surface through codegen and
>   surfaced **seven** distinct codegen bugs, all now fixed with regression tests
>   (`tests/codegen.rs::e2e_refinement_*` / `e2e_contract_ensures_result_field_access`
>   / `e2e_struct_variant_string_payload_*` / `e2e_with_provider_constructor_call_provider`;
>   `tests/memory_sanitizer.rs::asan_plain_enum_struct_variant_string_payload_no_double_free`
>   / `asan_refinement_try_from_vec_no_double_free`):
>   (1) a refinement alias of a collection (`NonEmpty[T] = Vec[T]`) used as a
>   param type didn't dispatch `.len()` / `for` — `register_var_from_type_expr`
>   now resolves the alias to its *instantiated* base with generic-arg
>   substitution; (2) struct fields naming a refinement/distinct alias mis-sized
>   to `i64` — the alias-base maps are now populated *before* `build_struct_types`;
>   (3) `with_provider[R](Type.new(..), ..)` couldn't infer the provider type from
>   a constructor call — `infer_provider_type_name` now reads the call's return
>   type; (4) `ensures(result) result.field == …` read the wrong slot — the
>   `result` binding now records its struct type name; (5/6/7) three
>   heap-payload double-frees: a String moved into a struct-variant payload (no
>   construction-side move-suppression), a struct-variant *match* (the
>   cap-suppression skipped struct patterns, and a `ref self` Display match
>   tracked its borrowed bindings as owned), and refinement `try_from` over a
>   `Vec` (the consumed source wasn't suppressed on the Ok branch). The built
>   binary's output is byte-identical to the interpreter. → recorded in
>   [`phase-7-codegen.md`](implementation_checklist/phase-7-codegen.md). Open:
>   `String.split` on a non-identifier receiver and a call-result `.method()`
>   chain both still fall through codegen dispatch (sidestepped in Weave by
>   binding to a local) — minor, tracked in phase-7.
> - [x] **Codegen `String.split` — ✓ RESOLVED 2026-06-13 (native + wasm).** A
>   runtime out-param helper `karac_runtime_string_split` (all targets) does the
>   split in Rust and writes the `Vec[String]` `{data,len,cap}` (malloc'd buffers
>   the binding frees) to out-pointers; the `vec_method.rs` `"split"` arm derives
>   `(sep_ptr, sep_len)` from a char/String and calls it. Tests:
>   `tests/codegen.rs::e2e_string_split_codegen` +
>   `tests/memory_sanitizer.rs::asan_string_split_no_leak_no_double_free` +
>   `tests/cli.rs::wasm_string_split_build_and_run_e2e` (node:wasi, output
>   byte-identical to native). → closed in
>   [`phase-7-codegen.md`](implementation_checklist/phase-7-codegen.md). **Wasm
>   split — ✓ CLOSED 2026-06-13 (B-2026-06-13-16):** the `cfg(not(wasm))` gate
>   assumed "no libc malloc on wasm," but `wasm_alloc.rs` makes wasi-libc
>   `malloc`/`free` the global allocator (one unified heap); the real blocker was
>   a latent ABI bug — the FFI's size params were `usize` (i32 on wasm32) vs
>   codegen's i64, trapping `signature_mismatch:karac_runtime_string_split` —
>   fixed by retyping them `u64`. See
>   [`phase-10-targets.md`](implementation_checklist/phase-10-targets.md)
>   ("`String.split` on wasm").
> - [x] **Multi-module `karac run` — ✓ RESOLVED 2026-06-13.** `cmd_run` now
>   detects when the entry file belongs to a multi-module project, builds the
>   module tree, and merges every module's items into a super-program (the
>   `run`-side analog of the codegen super-module) before interpreting — so
>   cross-module free functions AND associated functions work via `karac run`.
>   No-op for single-file scripts / one-module projects. → closed in
>   [`phase-4-interpreter.md`](implementation_checklist/phase-4-interpreter.md)
>   ("CR-24 follow-up: multi-module `karac run`"); regression
>   `tests/cli.rs::test_run_project_multi_module_loads_siblings`.
> - [x] **User `impl Display` — ✓ RESOLVED 2026-06-13.** Design decision: v1
>   admits user `impl Display`. The operator-trait resolver gate now carves out
>   `Display` (alongside `Eq`/`Ord`); dispatch wired in the interpreter (`to_string`
>   / f-string / `println`) and codegen (the `to_string` arm, `fstr_render_part`,
>   `compile_print` all defer to a compiled `<Type>.to_string`). Kāra's `Display`
>   stays `fn to_string(ref self) -> String` (no Rust `Formatter`). Fixed two
>   pre-existing bugs en route: a pattern-binding/unit-variant shadowing bug in
>   the interpreter (case-class rule), and a literal-backed-String double-free in
>   `println` codegen (`cap > 0` free guard). Codegen note: a user `impl Display`
>   on a **struct-variant** enum (`V { field }`) is additionally gated on the
>   separate pre-existing struct-variant-field-binding codegen bug (bugs.md);
>   tuple-variant + unit enums + structs all work in codegen today.
> - [x] **`allocates(Heap)` declarability knot — ✓ RESOLVED 2026-06-13.**
>   design.md § Effect Substrate was decisive (heap is substrate; declaring it
>   would be noise; absence ≠ denial), so the spec-conformant fix was to exempt
>   the substrate effect from the must-declare set (`is_default_permitted_effect`
>   filter in `src/effectchecker/verify.rs`) rather than make `Heap` a writable
>   resource. An allocating pub fn now needs no `with` clause; inference is
>   unchanged; `embedded`/`isr` rejection (heap-as-scoped) untouched. Four
>   spec-violating tests rewritten + two added. → closed in
>   [`phase-3-effect-checker.md`](implementation_checklist/phase-3-effect-checker.md)
>   ("Resolve the `allocates(Heap)` declarability inconsistency"); was
>   `bugs.md` B-2026-06-13-4.

**What the demo shows:**
1. Run the pipeline on a dataset with intentionally bad rows. Show parse errors
   reported cleanly with row numbers.
2. `karac build --output=json`: effect annotations tell you exactly what
   external resources each stage touches. The pipeline's data flow is auditable
   from the signatures alone.
3. Show what happens if you skip the parse stage and pass raw strings to
   `enrich_prices` — compile error: `String` is not `ValidEmail`.
4. Show what happens if you pass an empty `Vec` to `aggregate` — compile error
   at the call site: `Vec[EnrichedRow]` is not `NonEmpty[EnrichedRow]`.

**Why it's compelling:** This is the "correctness by construction" story.
Compare it to a Python pipeline where bad data causes a runtime exception 10
steps later. In Kāra, the shape of bad data is a compile error. For data
engineers who have debugged midnight pipeline failures, this is immediately
valuable.

**Effort:** Small to Medium. Purely a Kāra application — no FFI, no GPU, no
special tooling. The complexity is in selecting good examples. Bonus: use this
pipeline to process real public data (census data, stock prices) so the demo
has realistic inputs.

**This is the data-engineering flagship.** It is V1-scope, sequenced into the
pre-launch runway after the server demos (build order, not a post-launch
deferral). Two shapes are in play and either qualifies:

- **Refinement-typed CSV ETL** (the form sketched above) — the cheaper
  "correctness by construction" cut. Pure Kāra, no FFI; the dogfooding value is
  hammering refinement types + contracts + effect inference together on a real
  multi-stage transform.
- **Live Kafka → S3 → DuckDB service** — a runtime-exercising variant over the
  same V1 runtime (`Pool[T]`, TLS, `std.tracing`), a service-shaped alternative
  to the batch ETL. This shape doubles as the verification artifact for the
  backend-first-compounds claim: the `Pool[T]` + TLS + tracing surface built for
  the server demos is reused wholesale, so the data-eng story is cheap
  incremental engineering on top of the V1 runtime rather than a separate
  build-out — and it dogfoods that runtime surface under sustained streaming
  load, which the request/response demos don't.

(This is the demo formerly tracked as "Flagship Demo 3" in
`docs/implementation_checklist/phase-6-runtime.md`; that checklist tracks the
runtime-gating server demos, so the data-pipeline design lives here in the
roster while remaining V1-scope.)

---

### Chronicle — Compiler Written in Itself

**Primary capability:** Self-hosting — Phase 10 is complete; the compiler is
the demo.

**What it is:** Show `karac query effects src/typechecker.kara.check_function`.
The output is the type checker's effect signature: what resources it reads (the
AST, the symbol table), what it writes (the diagnostic list, the type table).
Then show the concurrency report for the full compilation pipeline — which
phases run in parallel, which must serialize.

**What the demo shows:**
1. `karac query effects src/` — generate an effect report for the entire
   compiler. Every module's effect footprint, the full resource dependency graph.
2. `karac build --concurrency-report src/main.kara` — show which compiler
   phases the *Kāra compiler itself* parallelizes when compiling a large program.
3. `karac query concurrency src/compiler.kara.compile` — show the
   parallelization decisions for the compilation pipeline (lexer → parser → type
   checker can be pipelined; type checker → effect checker → ownership checker
   are dependent).
4. Run `diff` between the Rust-based compiler and the Kāra-based compiler on
   their output for a test program. Identical.

**Why it's compelling:** Self-hosting is the canonical "we're serious" signal
in language design. But more than that — using Kāra's own tooling to explain
Kāra's own architecture is the meta-level demonstration of the AI-first thesis.
The compiler's architecture is expressed in its effect signatures.

**Also the largest *organic* probe of the ownership model.** A compiler is tens
of thousands of lines of exactly the borrow-heavy code — ASTs, symbol tables,
shared intermediate structures — that stresses a borrow checker hardest. Writing
it in Kāra with **zero lifetime annotations**, every RC fallback surfaced by
`karac query ownership`, is the standing receipt behind the README's
"no lifetime annotations" claim — not an asserted equivalence but a working
artifact. This is the *organic* leg (it didn't bite on a real codebase);
**Tangle** below is the *targeted-hard-shape* leg (the cases that classically
force annotations), and the adversarial soundness corpus in
[`phase-9-verification.md`](../implementation_checklist/phase-9-verification.md)
is the *adversarial* leg (programs that deliberately try to smuggle a dangling
reference past the checker). The three together are what back the safety claim.

**Effort:** Small (Phase 10 already delivers this). The work is curating the
right query examples and building a good presentation layer.

---

### Tangle — The Borrow Checker's Hard Cases

**Primary capability:** Ownership inference at the shapes that classically force
lifetime annotations (or `Rc<RefCell>` / arenas / `unsafe`) in Rust — handled in
Kāra with no `'a` syntax, and every RC escalation made visible, not silent.

**What it is:** A small but real program built entirely from the data structures
that torture borrow checkers — a mutable graph with cross-edges, a tree with
parent back-pointers, an intrusive/doubly-linked list, an undo/redo history over
shared state, and a tiny tree-walking interpreter with a shared environment. Not
a contrived test: a usable artifact (e.g. a dependency-graph analyzer or a small
in-memory document model with undo) whose *internals* happen to be exactly the
aliasing-heavy shapes. It pairs the two soundness legs: **Chronicle** is the
organic at-scale probe; Tangle is the *targeted* one — it goes straight at the
cases the model is most likely to get wrong.

**What the demo shows:**
1. The graph/back-pointer/undo code as-written — no lifetime parameters anywhere,
   the source-pinning rule and ownership inference carrying what Rust needs `<'a>`
   (often several) to express.
2. `karac query ownership` on each hot structure: where the model stayed in the
   owned/`ref` tiers, and — at the genuinely cyclic/shared cases — exactly where
   it **escalated to RC**, with the trigger line. The escalation is *surfaced*,
   never silent — the honest-conservatism story made concrete.
3. A side-by-side with the Rust shape of one structure (the doubly-linked list or
   the parent-pointer tree): the `Rc<RefCell<…>>` / explicit-lifetime version next
   to the Kāra version. Same capability, no annotation tax.
4. Built under ASAN (codegen path): the whole thing runs leak- and
   use-after-free–clean, so "accepted by the checker" is shown to mean "safe at
   runtime," not just "compiled."

**Why it's compelling:** Every Rust programmer has fought the borrow checker over
exactly these shapes — a graph, a back-pointer, an undo stack. Showing them
compile with no lifetime syntax, with the one real cost (RC at true cycles) made
explicit rather than hidden, is the most direct possible answer to "Rust-level
safety without lifetime annotations — prove it." It proves the *expressiveness*
half (the hard shapes work); the adversarial corpus
([`phase-9-verification.md`](../implementation_checklist/phase-9-verification.md))
proves the *soundness* half (the checker can't be tricked).

**Effort:** Small–Medium. Pure Kāra — no FFI, no GPU, no special tooling; gated
only on ownership inference + `karac query ownership`, both shipped. The work is
choosing structures that are genuinely borrow-hostile without being contrived,
and curating the Rust side-by-side honestly.

---

## Tier 3 — Domain-Specific (strong for specific audiences)

### Relay — High-Performance Network Proxy

**Audience:** Infrastructure engineers, performance-focused backend developers.

**What it is:** A Layer 7 HTTP proxy / load balancer. Accepts connections,
routes requests, collects metrics. Targets 500K+ requests/second on a single
machine.

**Why Kāra:** The auto-concurrency runtime (Phase 6 v1.1 — network event loop)
routes `sends(Network)` and `receives(Network)` effects to epoll/kqueue
automatically. No `async fn`. No explicit task management. The proxy is
written as if it's sequential; the compiler manages the event loop.

**Differentiator vs Go:** Go's goroutines handle the concurrency, but the
programmer still has to think about goroutine lifecycle. In Kāra, the effect
system drives routing — you never write goroutine-management code.

**Effort:** Medium. Builds on the HTTP server from Parallax. The interesting
part is the benchmark setup and comparison (wrk, hey, or equivalent).

---

### Forge — Embedded Sensor Logger

**Audience:** Embedded systems engineers, hardware developers.

**What it is:** A firmware image for a microcontroller (STM32 or RP2040).
Reads temperature/humidity from an I2C sensor, logs to SD card via SPI, and
streams over UART. Runs under the `embedded` profile — no heap, no panics,
no std.

**Why Kāra:** The `embedded` profile enforces the constraints at compile time.
No stray `Vec.new()` slipping in. The hardware effects (`reads(Hardware)`,
`writes(Hardware)`) make the I/O map explicit. Interrupt handlers use the
`#[interrupt]` attribute (from the v8 hardware gaps — this demo validates
that feature).

**Differentiator vs Rust embedded:** Rust embedded is excellent but requires
understanding lifetimes in depth. Kāra's ownership inference handles most of
it, surfacing only the edge cases that require annotation. The effect system
is cleaner than Rust's embedded-hal trait hierarchy for expressing hardware
access.

**Effort:** Large. Requires the hardware-level gaps from v8 to be resolved
(volatile access, `#[repr]`, `#[interrupt]`, `no_std` boundary). This demo
is the integration test for the embedded story.

---

### Iris — Browser Image Editor

**Audience:** Web developers, WASM users.

**What it is:** An in-browser image editor with non-trivial filters (Gaussian
blur, sharpening, histogram equalization, edge detection). Written entirely in
Kāra, compiled to WASM, runs in browser with no server.

**Why Kāra:** The WASM target (Phase 10) compiles the same Kāra code that
runs natively on the command line. Show the filter code once — then show it
running as a native binary (fast) and as a WASM module in Firefox (also fast).
No rewrite, no port, no language boundary.

**Differentiator vs Rust + wasm-pack:** Kāra is simpler to learn and has
better error messages. The layout blocks + SIMD story (when available) means
the image kernels are naturally cache-efficient. Bonus: the effect annotations
on image processing functions (`reads(ImageBuffer)`, `writes(ImageBuffer)`)
make the data flow visible.

**Effort:** Medium. WASM backend is Phase 10. Image processing algorithms are
straightforward. The interesting engineering is the browser integration (JS
glue, canvas rendering) and keeping the native / WASM comparison honest.

> **Front-end browser track (Plume / Fathom below, + Slipstream's wasm edition).**
> Iris is the *discrete* browser demo — load → apply filter → show — so it leans
> on framebuffer-blit + parallel filters and barely touches the deferred
> event-stream surface. The three entries below are *continuous-loop* browser
> demos (a live render loop + live input), which is what drives the Phase-10
> host-async event-stream APIs (`animation_frames`, event-data channels, DOM
> event streams) tracked in `phase-10-targets.md` (the `std.web.time` /
> event-stream wrappers entry). They are **not pre-launch-gating** — pickable
> pre-v1 or immediately post-v1 as the front-end story firms up. All share one
> spine: Kāra computes a pixel buffer into shared linear memory; one
> `canvas.put_pixels(ptr, w, h)` host fn blits it (`ctx.putImageData`) — no
> per-primitive Canvas drawing API; worker-pool parallelism + SIMD-128 already
> ship on `--features wasm-threads`.

---

### Plume — Particle Flow Field (Browser)

**Primary capability:** The wasm-threads front-end spine — a continuous render
loop driven by *host event streams* (`animation_frames`, pointer/slider events)
feeding channels, with the per-frame compute fanned out across a real Web Worker
pool — all reading as plain blocking Kāra (no `async`/`await`, no callback
coloring).

**What it is:** An in-browser particle flow field — tens of thousands of tracer
particles advected through a vector field, rendered to a canvas. The pointer (or
a slider) perturbs the field and the flow responds in real time. Written
entirely in Kāra, compiled to `--target=wasm_browser --features wasm-threads`.

**What the demo shows:**
1. Particles stream and swirl at 60fps; drag the pointer and the flow bends
   around it — input arrives as `for ev in pointer_moves(canvas) { … }`.
2. Overlay: `Workers: 18 | FPS: 60 | particles: 200k`. Toggle to one worker and
   the framerate visibly collapses — real multi-core, in a browser tab.
3. The render loop shown beside the window:
   `for _tick in animation_frames() { par { advect(field, particles, …) } canvas.put_pixels(fb, W, H) }`
   — a plain blocking loop. "Where's the `await`? There isn't one — the worker
   parks in `recv` and the host wakes it."

**Why it's compelling:** JavaScript can't do this ergonomically — Web Workers +
SharedArrayBuffer are a manual, error-prone chore and the language has no clean
blocking-recv. Kāra makes a multi-core browser app read like a single-threaded
loop. It is the *front-end* half of the "auto-concurrency without coloring"
thesis, in the one environment anyone can click and run — and the tractable
precursor to Slipstream's browser edition (same spine, simpler kernel).

**Effort:** Medium. Gated on the Phase-10 event-stream surface (`animation_frames`
producer + event-data channels) + the `canvas.put_pixels` blit host fn;
worker-pool parallelism + SIMD-128 already ship on wasm-threads.

---

### Fathom — Fractal Explorer (Browser)

> **Built (non-interactive cut) — 2026-06-14.** Shipped at
> [`examples/fathom/`](../examples/fathom/): a parallel Mandelbrot explorer that
> compiles to `--target=wasm_browser --features wasm-threads` and renders across
> the Web Worker pool at ~60fps (measured ~58fps under node's 4-worker pool;
> more cores in a real browser). The render loop is a plain blocking
> `loop { frames.recv(); render_frame(); }`; each frame's rows fan out via
> `TaskGroup.spawn` and the framebuffer is blitted through one `put_pixels` host
> fn. This is the first front-end-track demo and the first consumer of the
> Phase-10 event-stream surface.
>
> The dogfood drove four real `karac` gaps (all closed): (1)
> **`std.web.time.animation_frames()`** — a multi-shot host-async `requestAnimation
> Frame` channel producer (sibling of `after`, coalesced to one un-drained tick);
> (2) **`Vec[u8].as_ptr()` / `.as_mut_ptr()`** — the heap-buffer FFI handoff a
> `host fn` blit consumes (an `Array[u8, N]` framebuffer would overflow the wasm
> stack); (3) **`TaskHandle[T].join()` for a non-scalar `T`** — `join` had
> returned `i64` unconditionally, so a `spawn` returning `Vec[u8]` came back as
> garbage and trapped (B-2026-06-14-14, fixed native + wasm); and (4) a `for x in
> xs` loop binding sharing a name with an earlier same-function `let x` (here
> `for handle in handles` after `let handle = spawn(...)`) was conflated by the
> ownership RC analysis into a spurious RC fallback → codegen RC-boxed it and
> mis-lowered the plain loop element as an Rc pointer → segfault / `join`
> deadlock (B-2026-06-14-13, fixed by scoping the for-loop binding to a per-loop
> `@forN` CFG rename frame like match arms). Regression tests:
> `tests/cli.rs::wasm_threads_animation_frames_recv_e2e`,
> `tests/codegen.rs::{e2e_taskhandle_join_returns_nonscalar_vec,
> e2e_for_loop_binding_name_collision_no_false_rc, test_vec_as_ptr_loads_data_field}`.
>
> **Follow-ups (own gates):** the inner kernel is currently **scalar f64** — the
> `Vector[f64, 2]` SIMD-128 lowering (needs the comparison→mask→select path
> verified end-to-end) is not yet wired; and the **interactive pan/zoom** cut
> waits on event-data channels (`Channel[T]` for `T != ()`, the harder
> event-stream slice). The shipped cut auto-zooms, no input.

**Primary capability:** The same browser spine reduced to its essence —
multi-core pixel compute via framebuffer-blit, with zero domain code. The
fastest path to a clickable "all your cores, in a browser tab, from one source"
proof. (SIMD-128 is the design target for the inner kernel; the shipped cut is
scalar — see the Built note.)

**What it is:** An in-browser Mandelbrot/Julia explorer — pan and wheel-zoom into
the fractal, each frame computed in parallel across the worker pool and blitted
to a canvas. Pure compute: no physics, no simulation state.

**What the demo shows:**
1. Smooth 60fps zoom into unbounded fractal detail — wheel to dive, drag to pan.
2. Overlay shows worker count and FPS; halving the pool halves the framerate —
   the parallelism is real and measurable, not a loading-spinner illusion.
3. The inner iteration vectorizes to `Vector[f64, 2]` → WASM SIMD-128, same
   source, no flag.

**Why it's compelling:** The smallest demo that still lands the headline — real
multi-core + SIMD in the browser from one Kāra source — so it is the cheapest,
fastest *shippable* proof of the front-end story. Weaker narrative than fluid
(the "yet another Mandelbrot" risk), which is exactly why it is the fallback /
warm-up, not the flagship.

**Effort:** Small–Medium. A non-interactive cut needs only `animation_frames` +
the blit host fn; the pan/zoom cut adds wheel/pointer event-data channels.
Shares the entire spine with Plume.

---

## Build Sequence

All of these are V1-scope — the ordering is *when within the pre-launch runway*
each is built, not whether it ships before or after launch. Build in this order;
the "Ready when" column notes the compiler capability each is gated on.

| Order | Project | Ready when | Why this slot |
|---|---|---|---|
| 1 | **Mend** | Now (structured JSON output exists) | Cheapest to build. Makes the AI-first thesis real. Sets the tone. |
| 2 | **Parallax** | Auto-par codegen + HTTP FFI (done) | Broadest appeal. Every backend engineer relates to fan-out + join. |
| 3 | **Cartographer** | ✅ built 2026-06-14 (`examples/cartographer/`) — whole-program `karac query effects`/`concurrency` + static SVG viewer + live WASM studio (D3 + Monaco, compiler-in-browser) + per-callee blocking attribution; all design points covered | Teaches the effect system visually. Reduces onboarding friction for new users. |
| 4 | **Tangle** | Now (ownership inference + `karac query ownership` exist) | Proves the no-`'a` safety claim at the hard shapes. Cheap, pure Kāra, backs the README ownership section directly. |
| 5 | **Weave** | ✅ CSV cut built 2026-06-13 (`examples/weave/`) — runs under `karac run` (interpreter) **and** `karac build`s to a native binary, output byte-identical. Service cut still gated on `Pool[T]` + TLS + tracing | Correctness story for data engineers. Complements the concurrency story. |
| 6 | **Chronicle** | Self-hosting (Phase 10/12) | Self-hosting milestone. Marks Kāra as "a real language." |
| 7 | **Slipstream** | CPU path after Phase 11 (long-tail stdlib + FFI); GPU path added later with no Kāra-source change | Visually striking, instantly explainable. |
| 8 | **Husk** | Hardware gaps from v8 (`#[repr]`, `#[interrupt]`, inline asm, `no_std`) | Systems credibility. Validates the `kernel` profile. |

**Parallax** and **Mend** together are the minimum viable showcase — they
cover the two core theses (auto-concurrency, AI-first) with achievable effort,
and they're the earliest to land. Everything else layers on top within the same
pre-launch runway; the roster is not exhaustive and grows as dogfooding surfaces
new capabilities worth proving.

**Front-end browser track (separate, pre- *or* immediately post-v1).** The
continuous-loop browser demos — **Plume**, **Fathom**, and **Slipstream's wasm
edition** — are deliberately *not* in the strict V1-ordered list above. They
gate on the Phase-10 host-async event-stream surface (`animation_frames` +
event-data channels + a framebuffer-blit host fn — tracked in
`phase-10-targets.md`), and the WebAssembly front-end is a long-term (v1/v2)
story, so they are pickable as that surface firms up. Build order *within* the
track: **Fathom** (spine warm-up, zero domain code) → **Plume** (interactive,
exercises event-data channels) → **Slipstream wasm edition** (the flagship, same
spine + the full LBM kernel). Each forces the next slice of the event-stream
surface, so the track doubles as the consumer that justifies building it.
**Fathom's non-interactive cut is built (2026-06-14)** — it drove
`animation_frames` + the `Vec.as_ptr` blit handoff (the first event-stream
producer + the framebuffer host fn), so Plume now only needs the event-data
(`Channel[T]`, `T != ()`) slice on top of the proven spine.

---

## Reusable Scaffolding

Build these once; they serve multiple demos:

| Scaffold | Used by |
|---|---|
| HTTP server / client layer (thin FFI over hyper or similar) | Parallax, Slipstream, Relay |
| `karac --output=json` visualization harness | Mend, Cartographer, Chronicle |
| WASM → browser integration boilerplate | Cartographer, Iris |
| Benchmark harness (wrk, criterion, perf) | Parallax, Slipstream, Relay |
| QEMU boot scaffolding | Husk, Forge |

---

## Adding a project

The roster grows. To add a project:

1. Add one row to [Roster](#roster) — name, what it proves, status, gate, tier.
2. Add one row to [Build Sequence](#build-sequence) in its build-order slot.
3. Add a design section under the matching tier, keyed by **name** (no number),
   using this shape:

```markdown
### <Name> — <one-line title>

**Primary capability:** <the one differentiator this project proves>

**What it is:** <2–4 sentences — the concrete artifact>

**What the demo shows:** <numbered list of the observable moments>

**Why it's compelling:** <the peer/buyer takeaway in one paragraph>

**Effort:** <Small | Medium | Large — plus the gating capability>
```

Keep entries **design-shaped**. Execution status, bench numbers, and build
history belong in the phase checklists, the per-example `README.md`, or an
appendix (see the Parallax bench design lock below) — not inline in the entry,
so entries stay uniform and easy to add.

---

## Appendix — Parallax bench design lock (Slice E)

Relocated out of the Parallax entry to keep roster entries uniform. This is the
"§ Slice E" design lock referenced from `examples/parallax/bench/README.md` and
the `docs/investigations/` perf docs.

#### Slice E — Three-language benchmark harness ✓ Landed 2026-05-09 (scaffolding `ea1d26d`; verification run `4f7b72d`)

Parallax's recordable artifact: side-by-side `GET /dashboard/:user_id` benchmark across Kāra, Rust, Go, and Node. Predecessor: HTTP handler ABI trampoline (`5f4cbcc`). Slices A (`ab611d3`), B (`3c3d87b`), C (`f5c7b31`), D (`502250a`), F (`91768f2`) all shipped 2026-05-09 — Slice E is the final piece that makes Parallax recordable. Sub-steps (a)–(f) shipped as scaffolding (`ea1d26d`) on 2026-05-09; sub-step (g) verification run executed on Apple M5 Pro 2026-05-09 — table at `examples/parallax/bench/README.md` populated with measured req/s + p99 across all four impls.

**Settled design forks (2026-05-09).**

- **F1 — Recording artifact.** Markdown throughput table at `examples/parallax/bench/README.md` + checked-in `bench.sh` reproducer. Engineers believe code, not videos; a rerunnable script is the highest-credibility artifact. Asciinema cast and video walkthrough are post-v1 polish — additive, not gating.
- **F2 — Load distribution.** `wrk` Lua script generates random uniform user IDs in `1..1000`. Provides realistic-looking traffic in the recording without affecting throughput numbers (providers use `sleep_ms(n)`, not real lookups, so there's nothing to cache).
- **F3 — Win condition.** Measure first; frame post-hoc based on actual numbers. No defensive bar pre-baked. If results disappoint, that's a diagnostic signal for follow-up perf investigation, not a story to pre-rationalize.
- **F4 — Cross-language fairness controls.** Tokio worker count for Kāra and Rust matches `GOMAXPROCS` (= CPU count); Go uses default `GOMAXPROCS`; Node runs single-process (faithful to the language's default deployment reality); same hardware, sequential `wrk` runs (10s warmup + 30s measurement per impl). README footnotes Node's single-process choice and notes that cluster mode would scale ~Nx at the cost of process orchestration.
- **F5 — Provider delays.** Asymmetric: 2/5/8/12 ms per provider. Total sequential ~27ms; total parallel ~12ms (waiting on the slowest). Asymmetry surfaces the "join waits on slowest provider" property in the trace narration.
- **Rust added to comparison cohort 2026-05-09.** Extends the original Go+Node staging entry to a 4-way comparison. Rust (tokio + hyper + `tokio::join!()`) is the natural perf ceiling and uses the same runtime stack as Kāra, so the Kāra-vs-Rust gap measures Kāra's value-type ABI + trampoline overhead vs raw Rust. Single 4-way table per F3 — let the data tell the story rather than pre-splitting cross-paradigm vs implementation-overhead framings.
- **Demo location.** `examples/parallax/bench/{kara,rust,go,node}/` with top-level `bench.sh` and `README.md`. Same compiler repo (matches the existing `examples/parallax_lite/` precedent). Reproduction is opt-in (not part of `cargo test`); `bench.sh` graceful-degrades when language toolchains are missing (skip-with-stderr-notice pattern from `tests/memory_sanitizer.rs::KARAC_SKIP_ASAN_TESTS`).

**Out of scope (deferred to follow-ups).**

- TLS, HTTP/2, WebSockets — Phase 11.
- Real database FFI (Postgres / MySQL / Redis) — Phase 11 long-tail. Demo uses `sleep_ms(n)` providers + in-memory state.
- Cluster-mode Node — footnoted in README; not implemented in v1 bench.
- Asciinema cast / video walkthrough — post-v1 polish if shareability benefits warrant it.
- Multi-user load patterns (Zipf, sticky-session) — random uniform is enough to exercise per-request fan-out; richer load shapes are a follow-up if perf investigation calls for it.
- Splitting Parallax bench into a standalone repo (`parallax-bench/`) for shareable cloning — premature; same-repo until a real reason to split surfaces.
- **Closing the Kāra-vs-Rust gap (resolved 2026-05-10 — measure-first paid off).** F3 said *measure first*, then frame post-hoc. Did that. Three rounds of measurement + investigation (`docs/investigations/parallax_perf.md`, `docs/investigations/http_layer_perf.md`, `docs/investigations/bench_robustness.md`) established that **the apparent Kāra-vs-Rust delta on the original Parallax bench was largely a measurement artifact** — both runtimes' release codegen was DCE-eliding the busy_loops, so v1-v3 numbers compared HTTP-dispatch overhead, not the fan-out work the bench was designed around. Once the bench was made apples-to-apples (G1 — hash-mix kernel + observable fold, `5ef2ea6`), Kāra and Rust converged to within ~3 % on throughput at -c100 (715 vs 720 rps), with Kāra **2.6× lower p99** (300 ms vs 803 ms) at saturated load thanks to `karac_par_run`'s work-helping wait loop. The original closure path — borrowed accessors → inline trampoline → `#[repr(C)]` Request — was specced as the way to close a *trampoline-overhead* gap, and the trampoline isn't the bottleneck on this bench's CPU-saturated shape (probed in `http_layer_perf.md`: H1 `block_in_place` ~7 % rps gain at saturation, H2 step 1 null result at our scale). Those four follow-ups are still meaningful for a *plaintext-throughput* bench shape (no busy_loops, just return `"OK"`) where HTTP-layer overhead dominates — tracked in [`phase-7-codegen.md`](../docs/implementation_checklist/phase-7-codegen.md) "HTTP FFI closure path" entry, picked up when either a plaintext bench is built or an unrelated need touches the HTTP FFI surface.
