# Cartographer — Effect Graph Visualizer

**Proves:** a program's *effect graph* — every function, what resources it
touches, and which calls the compiler runs in parallel — is a first-class
**compiler artifact**, not a runtime guess or a hand-drawn diagram. Kāra emits
it; Cartographer just draws it.

This is the **compiler-half** cut of the Cartographer dogfooding project (see
[`docs/dogfooding.md`](../../docs/dogfooding.md)). It builds the part that
dogfoods the compiler — whole-program effect/concurrency graph emission — plus a
minimal static renderer as proof. The richer in-browser frontend (D3 force
layout, Monaco live-edit, compile-to-WASM) is deliberately deferred; it consumes
the exact same JSON this cut produces.

## The compiler wall this drove

Cartographer's design calls for `karac query effects src/` — a *whole-program*
effect report. But `karac query effects` / `karac query concurrency` only took a
**per-function** target (`file.kara.fn`); there was no way to ask for the whole
graph in one shot. Per the project's "fix the compiler, don't work around it"
rule, the fix landed in `karac`: both queries now accept a bare `<file>.kara`
target and emit a whole-program envelope.

```
karac query effects     src/service.kara   # nodes (effects + line) + call edges
karac query concurrency src/service.kara   # parallel bands, keyed to join on
```

- `query effects <file>` → `{ scope, functions:[{function, line, is_test,
  inferred_effects, declared_effects}], calls:[{caller, callee}] }`
- `query concurrency <file>` → `{ scope, functions:[{function, line,
  total_statements, parallel_groups}] }`

Function keys (`fn` or `Type.method`) join 1:1 across both envelopes and with
`karac query affected-by`, so a consumer overlays the parallel bands onto the
effect-colored call graph. Per-function targets still work exactly as before.

## What the subject program shows

[`src/service.kara`](src/service.kara) is a small dashboard API service whose
internals were chosen to exercise the whole effect surface:

| Function | Effect class | Why it's interesting |
|---|---|---|
| `build_dashboard` | **pure** (green) | no effects — assembles strings |
| `fetch_profile` / `fetch_orders` / `fetch_notifications` | **reads** (blue) | one distinct resource each |
| `record_access` / `double_audit` | **writes** (orange) | mutate `AuditLog` |
| `get_dashboard` | reads ×3 | **fans out** — the compiler runs the three reads in one parallel band |
| `double_audit` | writes ×2 | **same resource** — write/write conflicts, so it stays sequential |

The headline contrast is two functions written the same plain sequential way:

- `get_dashboard` → `parallel_groups:[{statements:[0,1,2], reason:"independent
  reads on different resources"}]` — three reads on `UserDB`/`OrderDB`/`NotifDB`
  with no data dependency, parallelized with **no `async`, no `par {}`, no
  annotation**.
- `double_audit` → `parallel_groups:[]` — two writes to `AuditLog` conflict, so
  the compiler keeps them in order, and the report says why.

## Run it

```bash
# 1. See the program run (tree-walk interpreter):
karac run src/service.kara
#   user-42: 84 orders, 43 notifications
#     [audit] access by 42
#     [audit] access by 42

# 2. Regenerate the effect graph from the compiler and view it:
./cartograph.sh                 # uses `karac` on PATH …
KARAC=../../target/debug/karac ./cartograph.sh   # … or a local build
open viewer.html                # macOS; or just open the file in any browser
```

`cartograph.sh` calls the two whole-program queries and writes their JSON into
`graph.js` as `window.GRAPH`. `viewer.html` is a self-contained static page
(vanilla JS + SVG, no build step, no CDN) that lays the call graph out in
layers, colors each node by its strongest external-resource effect, rings the
functions that carry a parallel band in dashed gold, and shows a clicked
function's inferred/declared effects and concurrency decision in a side panel.

## The live studio (`studio.html`)

The full frontend: **edit Kāra and watch the effect graph redraw on every
keystroke**, with the compiler running *in the browser tab* as WASM — no server
round-trip, no local `karac` process.

```bash
./studio.sh        # builds the WASM, serves http://localhost:8000/studio.html
```

- **Compiler-in-the-browser.** `studio.sh` builds the `karac-playground` WASM
  crate, which exports `cartograph(source)` — the same whole-program analysis
  the CLI runs (parse → resolve → typecheck → effect-check → concurrency),
  compiled to `wasm32`. The library entry point is `karac::effect_graph::cartograph_json`;
  the CLI and the studio share its JSON builders, so the graph is byte-identical
  across surfaces (pinned by `tests/cli.rs::test_cartograph_json_matches_cli_query_output`).
- **Monaco editor** (left) with Kāra syntax highlighting and **live compiler
  squiggles** — the type/effect errors the compiler finds are drawn as editor
  markers as you type.
- **D3 force-directed graph** (right): nodes colored by effect class, parallel-band
  functions ringed in dashed gold, draggable, click for a detail panel — effects,
  the function's parallel bands, **why it's serialized where it is**, **which
  callers it blocks** (see below), and call edges.
- A static file server is required only because browsers won't fetch a `.wasm`
  module over `file://` — it is **not** a `karac` backend; all analysis is client-side.

## Blocking attribution — the inverse of auto-concurrency

The compiler doesn't just parallelize for free; it can say *exactly why it
couldn't parallelize more, and whose fault it is*. The concurrency analysis now
emits **serialization points** alongside the parallel bands: for every pair of
statements that can't run together, the cause, the resource at issue, and — for
an effect conflict — the **specific callee** whose effect forced it
(`blocking_callees`). `karac query concurrency <file>` surfaces them:

```json
{ "function": "double_audit", "parallel_groups": [],
  "serialization_points": [
    { "statements": [0,1], "reason": "writes(AuditLog) conflicts with writes(AuditLog)",
      "resource": "AuditLog", "blocking_callees": ["record_access"] } ] }
```

Inverting `blocking_callees` across the program answers the design question
directly — **"which callers does function `f` block?"** Click `record_access`
in the studio (or static viewer) and the panel reads: *blocks parallelism in
`double_audit` (statements 0,1) on `AuditLog`*. A pure/`reads` function blocks
nothing. (Pinned by
`tests/concurrency.rs::test_cli_query_concurrency_serialization_points_attribute_blocking_callee`.)

## Scope

- **Built (compiler half):** whole-program `query effects`/`query concurrency`
  emission in `karac`; a runnable subject service; a static SVG viewer.
- **Built (frontend half):** the live `studio.html` — D3 force-directed graph,
  embedded Monaco editor with live re-query + diagnostic squiggles, and the
  whole analysis pipeline compiled to WASM (via `karac-playground`) so it runs
  with no local `karac`.
- **Built (blocking attribution):** per-callee serialization-point attribution in
  the concurrency analysis, surfaced in `query concurrency` and inverted in both
  viewers to "which callers does this function block." Cartographer's design
  points are now all covered.
