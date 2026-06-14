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

## Scope

- **Built here (compiler half):** whole-program `query effects`/`query
  concurrency` emission in `karac`; a runnable subject service; a static SVG
  viewer reading the emitted JSON.
- **Deferred (frontend half):** D3 force-directed layout, an embedded Monaco
  editor with live re-query on edit, and compiling the viewer itself to WASM so
  it runs without any local `karac`. None of these change the compiler; they
  consume the JSON this cut already emits.
