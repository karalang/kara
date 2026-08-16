# Structural debt backlog — split from project-review-2026-08-16

**Status:** OPEN — the long-lived remainders of the 2026-08-16 project review
(`docs/spikes/project-review-2026-08-16.md`, now deleted per its own rule:
items 1–6, 8, 9a, 9b landed; see git history of that file for the full
burn-down and per-item commit SHAs). Everything here is improvement work with
no urgency signal — pick items up as touched-anyway work or dedicated slices.

## Extraction (review item 7)

The extraction pattern that tamed typechecker/ (43 files), interpreter/ (40)
and ownership/ (13) has not reached:

- `src/cli.rs` — 14.5k lines; `src/cli/` holds only 3 files (args, explain,
  help). The largest file in the repo and the least-structured.
- `src/concurrency.rs` — 7.5k lines, no submodule dir at all.
- `src/codegen/method_call.rs` — regrew to 20.7k lines *after* extraction;
  needs a second-level split (peers: `stmts.rs` 13.8k, `vec_method.rs` 12.7k,
  `runtime.rs` 12.2k, `control_flow_match.rs` 10.5k).

Related, break up as touched: the 800–1,750-line dispatch bodies —
`infer_expr_inner` (`src/typechecker/exprs.rs`, ~1,750 lines),
the `expr_method_call.rs` region (~1,490), `contextual_scalar_collection_type`
(~1,230), `parse_prefix` (~846). Their debug-build frame sizes are also what
forced the parser recursion ceiling (B-2026-08-16-4) down to 128 — shrinking
them buys both readability and nesting headroom.

## Name interning (review item 9c)

All type/function tables are `HashMap<String, …>`; the typechecker alone has
~700 `.to_string()` calls with lookup-then-clone-then-relookup patterns
(`src/typechecker/expr_ops.rs`, `expr_call.rs`). An interner (u32 symbol ids)
is a large, cross-phase change — do it as its own spike with before/after
compile-time measurements, not opportunistically.

## Smaller residuals

- `stacker::maybe_grow` at the parser's three descent entries would decouple
  input-nesting acceptance from debug frame-size drift entirely (currently a
  fixed 128 ceiling sized to an 8 MiB stack — B-2026-08-16-4's SIZING NOTE).
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
