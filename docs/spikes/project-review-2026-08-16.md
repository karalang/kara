# Project review 2026-08-16 — findings + burn-down checklist

**Status:** OPEN — temp tracking doc. Tick items as they land; **delete this
file when every box is checked** (or split any long-lived remainder into its
own spike/ledger row first). Reviewed at `ef893997`.

Method: repo gates run on the checkout (fmt / clippy `--all --all-targets
-D warnings` / full non-LLVM `cargo test` — all clean, 0 failures), plus four
parallel review passes: architecture invariants, frontend code quality,
runtime unsafe sampling (~15 sites), test/CI static review. The parser
stack-overflow was reproduced empirically; other findings are static reads.

## Verdict

Exceptionally healthy for the size: all gates green, codegen containment /
phase layering / feature gating / diagnostics discipline all HOLD (containment
verified by grep — zero non-comment `inkwell`/`llvm_sys` uses outside
`src/codegen*`), bug backlog at one open low-severity row, runtime unsafe
discipline mechanically enforced (`unsafe_op_in_unsafe_fn` +
`missing_safety_doc` deny, panic=abort, one documented `transmute` in 42k
lines). One verified crash bug, one latent-UB FFI defect, and a short list of
CI/structural gaps — none systemic.

## Burn-down (ranked)

- [x] **1. HIGH — parser recursion guard.** DONE — `ea042102` (ledger row
  B-2026-08-16-4): shared depth counter at the three descent entries
  (`parse_expr_bp_with_ctx` / `parse_type` / `parse_pattern`), ceiling 128,
  spanned E0001 at the limit; regression tests pin all three surfaces plus
  below-limit acceptance. Residual option (not scheduled): `stacker::maybe_grow`
  at the entries would decouple acceptance from debug frame-size drift.
- [x] **2. MED — runtime null guards in `env_set`/`env_var`.** DONE —
  `a26de4bc` (ledger row B-2026-08-16-5): both entries normalize null → ""
  with the `json_make_string` guard shape; `# Safety` contracts name the
  null-empty form. Same commit also cleared BOTH low-priority channel.rs
  notes below (poison-tolerant locks + `deliver` debug_assert). The
  crate-level null rule write-up (defensive vs trust-codegen — the
  per-module split the bug slipped between) remains open — see the ledger
  row's WHY IT EXISTED for the analysis.
- [x] **3. CI — add `--all-targets` to the base clippy job.** DONE —
  `534faf21`; verified clean locally with the full
  `cargo clippy --all --all-targets -- -D warnings` gate.
- [x] **4. E2E harness — close the archive-ABSENT vacuous hole.** DONE —
  `220070be`: `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` turns every remaining
  `link_or_skip` soft-skip into a panic; CI's seven archive-building jobs set
  it; documented in CLAUDE.md. Bonus find while wiring it (ledger row
  B-2026-08-16-8): `tests/memory_sanitizer.rs`'s four hand-rolled link-skip
  sites bypassed `link_or_skip` entirely — the B-2026-07-28-1 stale-archive
  panic never protected the ASan/LSan suite. All four now route through it.
  Verified empirically with archives hidden (default skips, gated fails).
  Second bonus find when the gate met a full E2E run (ledger row
  B-2026-08-16-10, fixed `c92476aa`): CI never built the regex/arrow opt-in
  archives, so the 8 Regex / Arrow-IPC E2E tests green-skipped in CI and had
  never run there — the six gated jobs now build lean → regex → arrow → full.
- [x] **5. Tests — convert fixed 50ms "settle" sleeps to bounded retries.**
  DONE — `e78a0ba7`: the two genuine settle sleeps (http_server keep-alive +
  chunked-POST) now retry the idempotent round trip (50ms cadence, 5s cap).
  Review citations corrected on inspection: `tcp_stream.rs`'s sleeps all sit
  inside bounded connect-retry loops already, and `coro_e2e.rs:829` is
  deliberate inter-connection pacing — both left alone.
- [x] **6. CI — nightly full-CI cron on main.** DONE — `d51cd332`: 03:17 UTC
  cron + `workflow_dispatch`; concurrency group keyed by event so the
  backfill neither cancels nor is cancelled by push runs; `oracle-sync`
  (a diff-range guard with no range on a schedule) skips on backfill runs.
- [ ] **7. Structure — resume extraction.** `src/cli.rs` (14.5k lines,
  `src/cli/` holds only 3 files) and `src/concurrency.rs` (7.5k, no submodule
  dir) are the two unmanaged modules; `src/codegen/method_call.rs` regrew to
  20.7k post-extraction and needs a second-level split. Related: break up the
  800–1,750-line dispatch bodies as they're touched —
  `infer_expr_inner` (`src/typechecker/exprs.rs:3625`, ~1,750 lines),
  `expr_method_call.rs:362` region (~1,490), `contextual_scalar_collection_type`
  (`exprs.rs:203`, ~1,230), `parse_prefix` (~846). Their frame sizes also
  compound item 1.
- [ ] **8. Tests — focused coverage for `desugar.rs`** (1,741 lines; every
  pipeline run crosses it via `desugar_program`) **and `par_cost.rs`** (1,635;
  drives auto-par decisions — a documented divergence surface). Runners-up
  with zero unit tests and no dedicated integration file: `ownership_oracle.rs`
  (1,344), `effect_graph.rs` (1,033).
- [ ] **9. Perf hygiene (longer-term).** (a) ~~Derive `Copy` on `Span`~~
  DONE — `7ef4e843`: derived + clippy `clone_on_copy` machine-fix sweep,
  ~2,500 clones deleted across 150 files; full E2E green (2,999/0). (b) ~~Effect inference body clones~~ DONE — body tables are now
  `Rc<Function>`: the SCC convergence loop and three of the four
  whole-program walks take handles. Remainders: `subtyping.rs`'s owned walk
  still clones from the Rc (unchanged cost), and `collect_function_info`'s
  one-time `program.items.clone()` stands. (c) Name interning:
  tables are `HashMap<String, …>` with lookup-then-clone-then-relookup
  (`src/typechecker/expr_ops.rs:416`, `expr_call.rs:654`).

## Low / no-action notes (recorded so they aren't re-found)

- ~~`runtime/src/channel.rs`: six poison-intolerant `.lock().unwrap()` sites;
  missing `deliver` blob-length debug_assert~~ — both landed with item 2
  (`a26de4bc`).
- Frontend panic discipline is otherwise excellent: ~20 real unwrap/expect
  sites in ~90k lines, all locally guarded; resolver fully clean; 10
  TODO/FIXME markers repo-wide.
- 24 `#[ignore]` tests, all with reasons (MCJIT/arm64 hang ×6, pinned codegen
  gaps ×9, bench/env-global ×5, probe aids) — healthy, no action.
- Dep duplicates are trivial (`getrandom` 0.2/0.3, `hashbrown` 0.15/0.17,
  `webpki-roots` 0.26/1.0) — transitive, not worth forcing.
