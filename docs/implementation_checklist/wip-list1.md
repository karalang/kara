# WIP — List 1 (serial work, this session)

This file holds **delegate-ready** items only — slices whose plans are
drafted to autonomous-friendly bar in their phase tracker, with all
prerequisites cleared. Items in active triage live in
[`wip-staging.md`](wip-staging.md); long-term themed parking lives in
[`wip-list2.md`](wip-list2.md).

```
roadmap.md / phase trackers
        ↓
   wip-staging.md   (active triage — needs plan drafting / design discussion / prerequisite)
        ↓
   wip-list1.md     (delegate-ready, this file)
        ↓
   subagent execution → close-out
```

## Working patterns

**Mirror to the phase tracker.** When picking up work, also mirror the
bullet (with the box checked off as work progresses) into the relevant
`phase-N-*.md` tracker so the durable record lives alongside every
other completed phase entry. The phase tracker is the canonical record;
this list is the at-a-glance execution order.

**Delegate implementation, keep verification.** Slices in this queue
are drafted with self-contained per-item plans in their phase tracker
(goal, sub-steps, tests, files, out-of-scope, stop triggers — enough
that a fresh agent can implement from the plan alone). The main
session can delegate implementation to a subagent and act as
orchestrator — keeps main context manageable across long sessions.
Validated 2026-05-07 on slice 3 (~17 min subagent time, 97 subagent
tool calls, ~112K tokens kept out of main context; main spent 1
prompt + 4 verification commands).

Cycle, per slice:

1. **Main writes a self-contained subagent prompt.** Point at the plan
   section in the relevant `phase-N-*.md`, the working directory, the
   test commands (`cargo test`, `cargo test --features llvm`,
   `cargo clippy --all --tests -- -D warnings`,
   `cargo fmt --all -- --check`), the commit-message style (recent slice
   commits as templates), the hard-stop triggers from the plan
   (inline-fix is the default — the subagent doesn't need to flag
   minor friction in the report), and what to report back (commit
   hash, test counts, design-affecting deviations from plan,
   hard-stops with annotation refs). Include accumulated session
   context the subagent won't otherwise see — known workarounds
   (e.g., the struct-literal generic-arg gap pattern), pre-existing
   drift in unrelated files, code-slot conflicts (e.g., `E02xx` codes
   already taken).
2. **Subagent implements, verifies, closes out, commits, reports.**
   Single contained commit covering: the implementation, the doc
   close-out under the relevant item (same shape as slice 1 / 2 / 3
   close-outs in `phase-4-interpreter.md` — *What landed.* → *Tests.*
   → *Deviations from the proposed plan.* → *Out of scope, still
   open.*), the wip-list1 checkbox flip, **any parent-CR slice-roadmap
   bullet flip** (some CRs in phase trackers maintain a slice ledger
   separate from the per-item close-out — e.g., `phase-4-interpreter.md`
   § method-resolution slice roadmap has `- [x] Slice N — ... Landed
   YYYY-MM-DD (commit X). [brief summary]` bullets parallel to the
   per-item close-outs; both must update on slice landing), and any
   side-effect mappings the plan calls for (e.g., a new diagnostic-code
   entry in `src/cli.rs`).
3. **Main verifies.** Read the diff (`git show <hash> --stat` then
   spot-check critical files), run the new tests independently
   (`cargo test --test <suite> <test_pattern>` is fast), confirm both
   `cargo clippy --all --tests -- -D warnings` and
   `cargo fmt --all -- --check` are clean. Don't trust the subagent's
   summary alone — the report describes intent; the diff is ground
   truth.
4. **Main updates the queue and moves on.** Tick the bullet off here,
   kick off the next slice, or pause for discussion.

**Per-commit gates.** All commits — slice impl, doc updates, chores —
clear four checks before landing: `cargo test`,
`cargo test --features llvm`, `cargo clippy --all --tests -- -D warnings`,
and `cargo fmt --all -- --check`. **Clippy and fmt are both hard
gates.** Neither tolerates drift, including drift in files the commit
didn't touch.

**Pre-slice gate.** First action of any new slice or session: run
`cargo fmt --all -- --check`. If it fails, fix with `cargo fmt --all`
and land as a standalone `chore: cargo fmt cleanup` commit *before*
starting slice work. Without this, post-slice fmt cleanup either
contaminates the slice diff with unrelated drift or gets surgically
reverted (the established-but-flawed workaround that let drift
accumulate across slices 1, 2, 3 until CI flushed it on 2026-05-07).
Pre-slice gate breaks that cycle: slice work happens against a clean
tree, so the post-slice fmt check just verifies the slice's own
changes.

**What main does NOT delegate.** Design conversations; cross-slice
handoff decisions; deciding when to pause vs. continue; anything that
requires conversation history. The subagent operates from its prompt
only — it doesn't see this session's running context. If a slice
needs design judgment that wasn't captured in its plan, that's a
discussion-mode item, not an autonomous-queue item — it belongs in
[`wip-staging.md`](wip-staging.md) under the **needs design discussion**
state, not here.

**Friction handling — inline fix is the default.** Friction the agent
encounters during slice work gets fixed inline as part of the slice
work. No special flagging in the report; the slice's commit absorbs
the fix. The agent owns code hygiene for the files they touch.
Inline-fix territory includes:
- Clippy lint corner cases (apply the lint or add a scoped `#[allow]`
  with a one-line reason).
- Doc placement nits (pick the more sensible spot, move on).
- Test fixture workarounds (e.g., struct-literal generic-arg gap →
  use function-parameter form to pin receiver type).
- Code-slot conflicts (e.g., proposed `E0237` already taken → pick
  the next free slot, update all references).
- Pre-existing nit-level issues in adjacent code the agent
  encounters naturally while editing (warnings on lines they're
  changing, obvious typos, dead `#[allow]` attributes — fix them).

**Hard-stop = "I need main's input to proceed."** Reserved for the
narrow set of situations where the right next step isn't obvious and
the agent isn't authorized to guess. When a hard-stop fires:

1. Don't commit the slice. Leave the working tree clean (stash or
   discard partial work depending on whether it's salvageable for
   when main returns).
2. Annotate the slice's plan section in the relevant `phase-N-*.md`
   tracker with a `**Blocked (YYYY-MM-DD).**` paragraph explaining
   the trigger, what was investigated, and what input is needed
   from main.
3. Flip the wip-list1 bullet to prefix `**[BLOCKED]**` (keep the
   `[ ]` checkbox unchecked) with a one-line pointer to the blocker
   annotation in the phase tracker, OR move the slice back to
   [`wip-staging.md`](wip-staging.md) under the **awaits prerequisite**
   state if the blocker is durable enough that it shouldn't sit in
   the active execution queue.
4. **Move to the next non-dependent slice in the queue.** A blocked
   slice doesn't halt the whole queue. Skip slices that depend on
   the blocked one (the queue's slice-ordering prose names
   dependencies); pick up the next independent slice. Queue only
   ends when every remaining slice is either done or blocked.

Hard-stop triggers (the actual halt conditions; everything else is
inline-fix territory):
- Pre-existing test breakage that requires a design fork to resolve.
- Parser/AST shape changes needed.
- Effect-checker / ownership-checker invariants turning out
  load-bearing in unanticipated ways.
- The slice's premise turns out wrong (e.g., the assumed mechanism
  doesn't exist, or exists in a fundamentally different form than
  the plan assumed).

**When the plan isn't detailed enough yet.** Slice doesn't belong in
this file. It belongs in [`wip-staging.md`](wip-staging.md) under the
**needs plan drafting** state until the plan lands. Promotion to
wip-list1 is a single docs commit; do not bundle with implementation.

---

## Active queue

- [x] **Slice 1 — Plumbing: thread `ConcurrencyAnalysis` into `Codegen`.** Pure refactor; foundation for slice 2 (auto-par codegen MVP, the Parallax punchline). Plan source: [`phase-7-codegen.md`](phase-7-codegen.md) § "Par codegen: auto-parallelization of non-`par` regions" → "Slice plan (drafted 2026-05-08) — slice 1: plumbing". No IR shape change, no test-output change; existing suite must remain green. Promoted from staging 2026-05-08. Landed 2026-05-08 (commit c0e72fc).

- [x] **Slice 2 — Auto-par codegen MVP (the Parallax punchline).** Consume the slice-1 `parallel_groups_for_current_fn` getter at function-body scope: emit `karac_par_run` for compiler-inferred non-trivial parallel groups outside explicit `par {}` blocks. The "write sequential code, the compiler parallelizes it" promise becomes true in compiled output, not just the interpreter. Plan source: [`phase-7-codegen.md`](phase-7-codegen.md) § "Par codegen: auto-parallelization of non-`par` regions" → "Slice plan (drafted 2026-05-08) — slice 2: auto-par codegen MVP". Promoted from staging 2026-05-08. Landed 2026-05-08 (commit 8bc3bab).

- [x] **Slice 3 — Debugger Contract: SpawnSiteId metadata table emission.** First piece of the four-part Debugger Contract. Mint a stable per-binary `SpawnSiteId: u32` per `par {}` block (explicit + inferred both flow through `emit_par_run`'s new `record_spawn_site` site) and emit three module-scope external-linkage globals — `KARAC_SPAWN_SITES`, `KARAC_SPAWN_SITES_LEN`, `KARAC_SPAWN_SITES_ENABLED` — slice 5's `std.runtime::list_par_blocks()` / `has_debug_metadata()` will read directly. Default-on for dev; `KARAC_RUNTIME_DEBUG_METADATA=0` flips the gate off (globals still emit, but `LEN = 0`, `ENABLED = false`, array `[0 x …]`). Plan source: [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) § "Auto-Concurrency Codegen — Debugger Contract" → "Debugger Contract slice 3 — SpawnSiteId metadata table emission". Promoted from staging 2026-05-08. Landed 2026-05-08 (commit c6d8b44).

- [x] **Slice 4 — Debugger Contract: parent-frame ref + `KaracWaitTarget` surface.** Items (2) + (3) of the four-piece contract. Item 2 (parent-frame ref) ships real machinery: every worker frame produced by `karac_par_run` carries a stack-allocated `KaracFrame { parent, spawn_site_id, worker_index, wait_target }` registered in a process-wide `ACTIVE_FRAMES` registry; slice 5 walks the parent chain to reconstruct the structured-concurrency tree. Item 3 (await-chain pointer) ships contract surface only — `KaracWaitTarget` enum + `wait_target` field exist and are stable, but every v1 frame is populated as `KaracWaitTarget::None` (no real suspension to track until Phase 6.3). Two new public-extern getters land for slice 5 to wrap: `karac_runtime_get_current_frame` + `karac_runtime_for_each_active_frame`. Codegen-side: `karac_par_run` extern decl + call site grow a `spawn_site_id: u32` arg (slice 3's `par_id`). Plan source: [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) § "Auto-Concurrency Codegen — Debugger Contract" → "Debugger Contract slice 4 — parent-frame ref + await-chain pointer". Promoted from staging 2026-05-08. Landed 2026-05-08 (commit 22fa27a).

- [x] **Slice 6 — Parallax-lite microbenchmark workload.** First demo-shaped consumer of slices 1 + 2. Hand-coded `examples/parallax_lite/` Kāra project with three trait-bound effect resources (`MetricsA / MetricsB / MetricsC`), an `InMemoryMetrics` provider impl, and a `process_request()` aggregator whose three top-level call statements (each `with writes(R_i)` on disjoint resources) auto-parallelize through slice 2's `compile_function_body` path. Slice 6 also adds a `KARAC_AUTO_PAR=0` codegen gate (env-var-only, mirroring slice 3's `KARAC_RUNTIME_DEBUG_METADATA` shape) that flips off auto-par dispatch back to plain sequential `compile_block` — enabling side-by-side wall-clock benchmarking on the same source. Test surface: 4 mandatory IR-shape / concurrency / cost-summary / clean-compile tests + 2 `#[ignore]`-gated env-var + wall-clock tests. Locally observed ~2.81x speedup on three CPU-bound branches (well above the relaxed 1.3x assertion threshold). Plan source: [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) § "Auto-Concurrency Codegen — Parallax-lite Workload" → "Debugger Contract slice 6 — Parallax-lite microbenchmark workload". Promoted from staging 2026-05-08. Landed 2026-05-08 (commit 0c832c4).

- [x] **Slice 5 — Debugger Contract: `std.runtime::list_par_blocks()` / `list_tasks()` / `has_debug_metadata()`.** Item (4) of the four-piece contract; closes out the Debugger Contract slate. Three Kāra-callable APIs declared on an empty-marker `pub struct Runtime { }` in baked stdlib (`runtime/stdlib/runtime.kara`) that materialize slice 3's `KARAC_SPAWN_SITES` LLVM globals + slice 4's `ACTIVE_FRAMES` registry as user-facing Vec[ParBlockInfo] / Vec[TaskInfo] / bool surfaces. `has_debug_metadata()` reads `KARAC_SPAWN_SITES_ENABLED`; `list_par_blocks()` snapshots active worker frames and joins each frame's `spawn_site_id` against `KARAC_SPAWN_SITES` to fill `(file, line, col, worker_count)`; `list_tasks()` always returns the empty Vec in v1 (no real suspension exists yet — Phase 6.3's network event loop lights this up additively). Strong-linkage path taken for the runtime crate's slice-3 globals reads (toolchain is stable Rust per cargo 1.95.0, no `rust-toolchain.toml`; weak-linkage `#[linkage = "extern_weak"]` would require nightly per hard-stop trigger 1). Runtime-side full Vec materialization chosen for `list_par_blocks` (hard-stop trigger 3 fallback) — runtime allocates the `Vec[ParBlockInfo]` element buffer + per-entry file `String`s and writes the `{data, len, cap}` descriptor through an out-pointer; codegen-side complexity drops from ~80 lines of inline IR to ~25. `WaitTarget` variant renamed `None` → `Running` to avoid shadowing `Option.None` (deviation 1). Test surface: 5 codegen E2E + 3 typechecker + 3 interpreter + 6 runtime-crate layout / read-path tests. Plan source: [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) § "Auto-Concurrency Codegen — `std.runtime` Introspection APIs" → "Debugger Contract slice 5 — `std.runtime::list_par_blocks()` / `list_tasks()` / `has_debug_metadata()`". Promoted from staging 2026-05-08. Landed 2026-05-08 (commit 2d00e63).

- [x] **Slice A — Par codegen: return values (full Parallax demo, load-bearing codegen unblock).** Lift the slice-2 `group_defines_binding_used_outside` gate via per-branch return slots: each parallel group whose let-bindings are read outside the group gets a synthesized parent-frame return struct (sibling to `KaracFrame`, ABI stable for slice-5 introspection); each branch writes its produced bindings into assigned fields by offset; after `karac_par_run` returns, the parent emits load instructions binding the outside-of-group let-bindings from those fields. Locked design choices (2026-05-09): (i) single parent-allocated return struct per group (mirrors capture-into-Env model); (ii) sibling to `KaracFrame`, NOT inline (preserves slice-4 ABI stability); (iii) move-only slot semantics, no destructor on slot (barrier guarantees parent doesn't see slot until all branches complete; panic propagation predates parent let-binding effect); (iv) test surface: demo-shaped four-read fan-out+join E2E + parallax-lite-shaped three-read fast-regression fixture; ASAN test for move-only no-double-drop verification. **The load-bearing unblock for the Parallax demo punchline** — after this lands, `let p = fetch_profile(...); let o = fetch_orders(...); ... build_dashboard(p, o, ...)` auto-parallelizes through `karac_par_run` instead of falling back to sequential. Plan source: [`phase-7-codegen.md`](phase-7-codegen.md) § "Par codegen: return values" → "Slice plan (drafted 2026-05-09) — Full Parallax demo: return values" (commit `909eb05`). Promoted from staging 2026-05-09. Landed 2026-05-08 (commit ab611d3).

- [ ] **Slice D — `karac build --concurrency-report` human-readable renderer (full Parallax demo).** Build-time CLI flag that runs the existing `concurrencycheck` analysis and renders the result in human-readable text alongside the binary build. Output shape locked by the demo storyboard (`docs/demo_ideas.md:80-88`): per parallel group, per-call line with `[<line>] <call_expr>  // reads(R1), writes(R2)`, and a one-sentence "why" derived from the analyzer's existing `reason` field. The structured-JSON output via `karac query concurrency` is unchanged; this is a renderer additive, not a new analysis pass. Pairs with Slice A's auto-par execution to make the compiler's reasoning visible alongside the speedup — the recordable terminal-side artifact for the Parallax demo. No design forks; no prereqs. Plan source: [`phase-5-diagnostics.md`](phase-5-diagnostics.md) § 5.1 → "Slice plan (drafted 2026-05-09) — Full Parallax demo: concurrency-report renderer". Promoted from staging 2026-05-09.

- [ ] **Slice F — `std.json` (Json enum + parse + stringify + `ToJson` / `FromJson`).** v1 stdlib JSON surface: `Json` enum (Null/Bool/Number/String/Array/Object), `Json.parse(s) -> Result[Json, JsonError]`, `json.stringify() -> String`, manual `ToJson` / `FromJson` traits with builtin impls for primitives + Vec / Option. Backing implementation is `serde_json` via Rust FFI from karac-runtime (mirroring Slice B's hyper FFI pattern). Locked design choices (2026-05-09): (i) `Json.Number(f64)` per JSON spec (no split Int/Float); (ii) Object backed by `Vec[(String, Json)]` preserving insertion order; (iii) `serde_json` via FFI (no hand-rolled parser); (iv) `JsonError { line, col, msg }` with line/col from serde; (v) manual-impl-only traits in v1, derived in v1.5; (vi) 6-test minimal surface — round-trip on representative shapes + error-reporting + manual ToJson impl. **Soft prerequisite for Slice B's `Response.json(value)` builder ergonomics** — B can ship without F via the `body(bytes)` fallback, but the demo storyboard reads cleaner with F. No design forks open; no prereqs. Plan source: [`phase-8-stdlib-floor.md`](phase-8-stdlib-floor.md) § "`std.json`" → "Slice plan (drafted 2026-05-09) — Full Parallax demo: `std.json`". Promoted from staging 2026-05-09.

---

## Timing log

| # | Slice | Started | Landed | Duration | Commit |
|---|---|---|---|---|---|
| 1 | Plumbing: `ConcurrencyAnalysis` into `Codegen` | 2026-05-08 | 2026-05-08 | ~30 min | c0e72fc |
| 2 | Auto-par codegen MVP (Parallax punchline) | 2026-05-08 | 2026-05-08 | ~45 min | 8bc3bab |
| 3 | Debugger Contract: SpawnSiteId metadata table | 2026-05-08 | 2026-05-08 | ~60 min | c6d8b44 |
| 4 | Debugger Contract: parent-frame ref + `KaracWaitTarget` surface | 2026-05-08 | 2026-05-08 | ~75 min | 22fa27a |
| 6 | Parallax-lite microbenchmark workload + `KARAC_AUTO_PAR` gate | 2026-05-08 | 2026-05-08 | ~90 min | 0c832c4 |
| 5 | std.runtime introspection APIs (`has_debug_metadata` / `list_par_blocks` / `list_tasks`) | 2026-05-08 | 2026-05-08 | ~120 min | 2d00e63 |
| A | Par codegen: return values (full Parallax demo) | 2026-05-08 | 2026-05-08 | ~120 min | ab611d3 |
| D | `karac build --concurrency-report` renderer (full Parallax demo) | _—_ | _—_ | _—_ | _—_ |
| F | `std.json` (Json enum + parse + stringify + ToJson/FromJson) | _—_ | _—_ | _—_ | _—_ |
