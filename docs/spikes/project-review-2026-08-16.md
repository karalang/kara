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

- [ ] **1. HIGH — parser recursion guard.** `parse_expr_bp_with_ctx`
  (`src/parser/exprs.rs:84`) → `parse_prefix` (`src/parser/exprs.rs:479`)
  recurse with no depth limit; `karac check` on ~300 nested parens aborts with
  `fatal runtime error: stack overflow` (reproduced, debug build). The
  fat-stack mitigation (`src/lib.rs:563`, `run_on_interp_thread`) covers only
  the interpreter. Fix: depth counter (or `stacker::maybe_grow`) at the
  `parse_expr` entry emitting a structured diagnostic; **file the ledger row
  when starting this item** (class `crash`, surface `parse`). Machine-generated
  Kāra (Mend loop) is exactly the input that can hit this.
- [ ] **2. MED — runtime null guards in `env_set`/`env_var`.**
  `runtime/src/lib.rs:921` and `:1093` pass the canonical empty-string
  `{null, 0, 0}` to `slice::from_raw_parts` (requires non-null even for
  len 0 — library UB; Miri/hardening trap). Rest of crate guards with
  `is_null()` (`lib.rs:4259` et al.). While there: write the crate-level
  null-handling rule — the defensive (channel.rs, most string fns) vs
  trust-codegen (map.rs module contract) split is documented per-module but
  has no single crate rule, which is the crack this slipped through.
- [ ] **3. CI — add `--all-targets` to the base clippy job** (`ci.yml:122`).
  Non-LLVM cfg(test) lint surface is currently enforced only by the
  `continue-on-error` drift canary; diverges from the CLAUDE.md local gate.
  One-token fix.
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

- `runtime/src/channel.rs:234–433`: six `.lock().unwrap()` sites off the
  poison-tolerant convention (89 sites elsewhere use
  `unwrap_or_else(|p| p.into_inner())`). Unreachable in release
  (panic=abort); live only under `cargo test`. Fold into item 2's pass.
- `runtime/src/channel.rs:160–166` (`deliver`): no
  `debug_assert!(blob.len() >= elem_size)` cross-check against a codegen
  elem-size mismatch (heap over-read if codegen ever bugs). Free assert; fold
  into item 2's pass.
- Frontend panic discipline is otherwise excellent: ~20 real unwrap/expect
  sites in ~90k lines, all locally guarded; resolver fully clean; 10
  TODO/FIXME markers repo-wide.
- 24 `#[ignore]` tests, all with reasons (MCJIT/arm64 hang ×6, pinned codegen
  gaps ×9, bench/env-global ×5, probe aids) — healthy, no action.
- Dep duplicates are trivial (`getrandom` 0.2/0.3, `hashbrown` 0.15/0.17,
  `webpki-roots` 0.26/1.0) — transitive, not worth forcing.
