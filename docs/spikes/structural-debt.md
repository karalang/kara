# Structural debt backlog — split from project-review-2026-08-16

**Status:** OPEN — the long-lived remainders of the 2026-08-16 project review
(`docs/spikes/project-review-2026-08-16.md`, now deleted per its own rule:
items 1–6, 8, 9a, 9b landed; see git history of that file for the full
burn-down and per-item commit SHAs). Everything here is improvement work with
no urgency signal — pick items up as touched-anyway work or dedicated slices.

## Extraction (review item 7)

The extraction pattern that tamed typechecker/ (43 files), interpreter/ (40)
and ownership/ (13) has not reached:

- ~~`src/cli.rs` — 14.5k lines~~ DONE — eight command submodules extracted
  (fix_cmds, test_cmd, maintenance_cmds, pkg_cmds, diag_json, query_cmd,
  run_check_cmds, build_cmds); driver at 2,016 lines (Pipeline, dispatch,
  text diagnostics, args surface).
- ~~`src/concurrency.rs` — 7.5k lines, no submodule dir at all~~ DONE —
  eight submodules extracted over two slices (predicates, reduction_shapes,
  var_extract, reads, effects_collect, conflicts, reductions, hazards);
  driver at 2,203 lines (types, entry, analyze_stmt, grouping). Comparable
  to the other extracted phases now.
- ~~`src/codegen/method_call.rs` — 20.7k lines~~ DONE — second-level split
  into method_call_iter (6.5k, the iterator-fusion surface), method_call_ffi,
  method_call_sync, method_call_vector; driver at 9.4k (compile_method_call
  dispatch + adjacent helpers). Peers still oversized when touched:
  `stmts.rs` 13.8k, `vec_method.rs` 12.7k, `runtime.rs` 12.2k,
  `control_flow_match.rs` 10.5k — same recipe applies.

Related, break up as touched: the 800–1,750-line dispatch bodies —
`infer_expr_inner` (`src/typechecker/exprs.rs`, ~1,750 lines),
the `expr_method_call.rs` region (~1,490), `contextual_scalar_collection_type`
(~1,230), `parse_prefix` (~846). Their debug-build frame sizes are also what
forced the parser recursion ceiling (B-2026-08-16-4) down to 128 — shrinking
them buys both readability and nesting headroom.

## Name interning (review item 9c) — spike RUNNING, stages 0–3f landed

Measured and progressively burned down in
[`name-interning.md`](name-interning.md) (2026-08-17): a front-end phase
benchmark (`examples/bench_frontend.rs`) + callgrind attribution found ~60%
of front-end instructions in string-identity overhead. Stages 1–2¾ (the
`collect_calls_in_expr` quadratic B-2026-08-17-1, FxHash on internal
tables, the seam through the result structs) took wall −36% / instructions
1.62B → 0.863B. Stage 3a then landed the first slice of the `Symbol(u32)`
interner proper (`src/intern.rs` + the effectchecker's full key space):
effectcheck **−35% wall / −69% allocations**. Stages 3b–3f followed
DHAT attribution: borrow-based callee effect sets, one shared
rc_predicate use-sites build per CFG, FxHash on the remaining internal
working sets, the parser's borrowed token peeks (kind-probes had cloned
payload tokens — 17% of ALL front-end allocations), and `FnHandle`
(the effectchecker had deep-cloned every function body in the program).
Spike net on the 10.9k-line synthetic corpus: **instructions
1.62B → 0.520B (−68%), allocations 3.14M → 0.605M (−81%)**; effectcheck
allocations −97%. Remaining: the ownership / typechecker /
AST-identifier key spaces, one session-sized slice each — scope in the
spike doc.

## Smaller residuals

- ~~`stacker::maybe_grow` at the parser's descent entries~~ DONE — stacker
  was already a direct dep (interpreter eval site); the three entries now
  grow the stack (512 KiB red zone / 16 MiB chunks), parsing survives any
  thread size, and the 128 ceiling is purely semantic.
- ~~Whole-program `program.items.clone()` walks~~ DONE — all 19 sites across
  resolver/typechecker/effectchecker/ownership/interpreter converted to
  `&[Item]` reborrows (the `&'a Program` field is `Copy`, so the clones were
  never needed). 9b is now fully closed: `check_call_site_subtyping`'s
  walk was converted to by-reference too — zero body clones remain in the
  effectchecker.
- ~~Focused tests for the item-8 runners-up~~ DONE — tests/effect_graph.rs
  pins the Cartographer envelope contracts (5 tests); and the review's claim
  about `ownership_oracle.rs` was WRONG — it already has 19 unit tests in
  `src/ownership_oracle/tests.rs` (a submodule file the review scan missed).
