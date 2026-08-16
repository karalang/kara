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
- [ ] **4. E2E harness — close the archive-ABSENT vacuous hole.** Stale
  archive now panics (B-2026-07-28-1), but a *missing* archive still
  green-skips ~1,560 tolerant `if let Some(out)` sites in `tests/codegen.rs`
  locally (CI builds archives, so CI is immune). Options: opt-in
  `KARAC_REQUIRE_RUNTIME_ARCHIVE=1` turning any `link_or_skip` soft-skip into
  a panic, or a per-process executed-count canary (≥1 successful
  `run_program`).
- [ ] **5. Tests — convert fixed 50ms "settle" sleeps to bounded retries:**
  `tests/http_server.rs:1575, 1750`, `tests/tcp_stream.rs:225, 430` (+4
  siblings), `tests/coro_e2e.rs:822, 829`. Same files already use the
  retry-loop shape (`http_server.rs:330` — ×10 @50ms, 10s cap). Cheapest
  flakiness reduction available.
- [ ] **6. CI — nightly full-CI cron on main.** `cancel-in-progress: true`
  (`ci.yml:37–39`) + no scheduled full run means a push burst can leave
  intermediate main commits untested with no backfill (only fuzz/supply-chain
  crons exist).
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
- [ ] **9. Perf hygiene (longer-term).** (a) Derive `Copy` on `Span`
  (`src/token.rs:6`, four `usize`s) — deletes ~1,800 `span.clone()` calls.
  (b) Effect inference deep-clones every function body per pass and re-clones
  per SCC convergence iteration (`src/effectchecker/inference.rs:258–287`);
  the same clone-all-bodies block is copy-pasted at
  `src/effectchecker/subtyping.rs:27`, `with_e.rs:30`, `modbind_synth.rs:500,
  :593` — `Rc<Block>` or split-borrow kills both. (c) Name interning:
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
