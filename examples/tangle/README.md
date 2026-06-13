# Tangle — the borrow checker's hard cases

A small, real program built entirely from the data structures that torture
borrow checkers — a mutable graph with cross-edges, a tree with parent
back-pointers, a doubly-linked list, an undo/redo history over shared state, and
a tiny tree-walking interpreter with a shared environment. Not contrived tests:
the *internals* of a usable artifact happen to be exactly the aliasing-heavy
shapes that force `<'a>` lifetime parameters, `Rc<RefCell>`, arenas, or `unsafe`
in Rust.

In Kāra these compile with **no lifetime syntax**, and the one real cost — RC at
the genuinely cyclic/shared cases — is made **visible, never silent**, by
`karac query ownership`.

This is the *targeted* leg of the ownership soundness story (the hard shapes the
model is most likely to get wrong). The *organic at-scale* leg is **Chronicle**
(the self-hosted compiler); the *adversarial* leg is the soundness corpus in
`docs/implementation_checklist/phase-9-verification.md`. See the roster entry in
[`docs/dogfooding.md`](../../docs/dogfooding.md) (Tier 2, build-order #4).

## Structures

| Structure | File | Proves | Ownership signal |
|---|---|---|---|
| Parent-pointer tree | `src/parent_tree.kara` | up/down cycle without `Rc<RefCell>`+`Weak` | `representation:"shared (Rc)"` (declared RC) |
| Cross-edge graph (diamond) | `src/cross_graph.kara` | shared descendant the checker can't linearize | `rc_values` + trigger line (RC fallback) |
| Doubly-linked list | _planned_ | the classic `Rc<RefCell>` shape | TBD |
| Undo/redo over shared state | _planned_ | back-references to shared history | `rc_values` (RC fallback) |
| Tree-walking interpreter (shared env) | _planned_ | shared mutable environment | TBD |

## Running

```bash
karac run   examples/tangle/src/parent_tree.kara     # interpret
karac check examples/tangle/src/parent_tree.kara     # typecheck only
karac query ownership examples/tangle/src/parent_tree.kara.<fn>   # per-fn ownership

karac run   examples/tangle/src/cross_graph.kara
karac query ownership examples/tangle/src/cross_graph.kara.build_diamond
```

`parent_tree.kara` prints:

```
depth of b from root: 2
depth of c from root: 1
depth of root:        0
```

`cross_graph.kara` prints `diamond reachable-sum (d counted twice): 14` — the
shared node `d` is visited on both paths (1 + (2+4) + (3+4)), which is the
observable proof the cross-edge is one shared node, not two copies.

## Reading `karac query ownership` (the demo's core artifact)

There are **two distinct RC signals**, and Tangle is designed to show both:

- **`representation:"shared (Rc)"`** — the value is RC because it is a
  `shared struct` (reference semantics, *declared* by the author). The
  parent-pointer tree's nodes carry this. It is RC by design, so it does **not**
  appear in `rc_values`.

  ```jsonc
  // karac query ownership .../parent_tree.kara.add_child
  {"function":"add_child",
   "parameters":[{"name":"parent","mode":"own","representation":"shared (Rc)"},
                 {"name":"child","mode":"own","representation":"shared (Rc)"}],
   "rc_values":[],"closures":[]}
  ```

- **`rc_values`** (with the trigger) — an *owned* (non-`shared`) value the
  compiler was **forced to escalate to RC** because it couldn't prove the
  aliasing safe (RC fallback). This is leg #2's real payload — "exactly where it
  escalated, with the trigger line." The cross-edge graph exercises it: the
  diamond's shared node `d` is stored into one parent's edge list, then used
  again to link the second parent, which the checker can't linearize.

  ```jsonc
  // karac query ownership .../cross_graph.kara.build_diamond
  {"function":"build_diamond","parameters":[],
   "rc_values":[{"binding":"d","kind":"Rc",
                 "trigger":"container_store_with_subsequent_use",
                 "consume_line":43,"other_use_line":44}],
   "closures":[]}
  ```

  The node is plain `struct` — nothing in the source says `shared`, `Rc`, or
  `'a`. The compiler took the one RC it needed and pointed at both lines: where
  `d` was stored (43) and where it was used again (44).

So: the parent-pointer tree proves "the cyclic shape works, and its RC is
declared and visible"; the owned-aliasing structures will prove "and where the
compiler *infers* RC, it tells you, with the line."

## Status

Front-end legs (typecheck · `karac query ownership` · interpret) are the current
focus. Tangle's leg #4 — "runs leak-/use-after-free–clean under ASAN (codegen
path)" — is deferred until the active codegen leak cluster
(`bugs.md` B-2026-06-12-6 / -10) settles, so the clean ASAN run is a meaningful
verification pass rather than a re-hit of in-flight leaks.

**Surfaced tooling gap (Tangle dogfooding find).** The per-function query kinds
(`ownership` / `effects` / `concurrency`) parse their target with
`rsplit_once('.')` on `<file>.<function>` (`src/cli/args.rs:1590`), so they
**cannot address an impl method** — `…kara.add_edge` resolves the method's bare
name (not found, methods are keyed `Type.method`) and `…kara.GraphNode.add_edge`
is misread as a filename. The sibling `karac query affected-by` already supports
the qualified `file.kara:Type.method` form (`args.rs:1611`); the per-function
kinds should accept the same. Free functions (e.g. `build_diamond`) query fine,
so this doesn't block the demo — but a Tangle demo over methods needs it fixed.
