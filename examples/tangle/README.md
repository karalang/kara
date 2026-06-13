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
| Undo/redo over shared state | `src/undo_redo.kara` | shared **mutable** state, undo writes back through the shared handle | `representation:"shared (Rc)"` (declared RC) |
| Tree-walking interpreter (shared env) | _planned_ | shared mutable environment | TBD |

## Running

```bash
karac run   examples/tangle/src/parent_tree.kara     # interpret
karac check examples/tangle/src/parent_tree.kara     # typecheck only
karac query ownership examples/tangle/src/parent_tree.kara.<fn>   # per-fn ownership

karac run   examples/tangle/src/cross_graph.kara
karac query ownership examples/tangle/src/cross_graph.kara.build_diamond

karac run   examples/tangle/src/undo_redo.kara
karac query ownership examples/tangle/src/undo_redo.kara.Editor.set   # impl method
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

`undo_redo.kara` prints `30 / 20 / 10 / 20` — two edits, two undos, one redo,
each restoring the value by writing *through the shared cell handle* held by the
history command. In Rust this is `Rc<RefCell<Cell>>` (shared ownership +
interior mutability); in Kāra it is a `shared struct` with a `mut` field — and
`karac query ownership .../undo_redo.kara.Editor.set` shows the `cell` parameter
as `mut_ref` + `representation:"shared (Rc)"`.

> **Bug found *and fixed* by this structure.** undo/redo first read stale
> values: a write to a `shared struct` field through a projection receiver
> (`cmd.cell.value = …`, `v[i].value = …`) was silently dropped — the
> interpreter's `set_field` only handled bare-identifier / `self` receivers.
> Fixed in `src/interpreter.rs` (write through the projected Arc); regression
> tests in `tests/interpreter.rs`. Dogfooding working as intended.

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
verification pass rather than a re-hit of in-flight leaks. When that leg is
picked up, **verify codegen has the projection-write fix too** — the
shared-struct field-write bug below was fixed in the interpreter; the codegen
path needs the same check.

Follow-ons from this work:
1. **Fixed.** Writing through a *plain* value-type struct projection
   (`o.inner.x = v`, depth ≥ 2; `v[i].field = v`) and compound assignment on
   field/index targets (`o.count += 1`) were silently dropped in the
   interpreter — a broad pre-existing miscompile, not Tangle-specific. Fixed in
   `src/interpreter.rs` (`assign_to_place` write-back), commit `62a92b39`;
   regression tests in `tests/interpreter.rs`.
2. **Fixed.** The same miscompile in **codegen** is now fixed too (`47c0dff5`):
   `compile_field_store` GEPs to the nested place in-place, and `CompoundAssign`
   routes field/index targets through the place-store path. E2E regressions in
   `tests/codegen.rs`; verified under ASAN. The one narrow sub-case that was left
   tracked — a `ref`/`mut ref`-param-rooted nested store (`p.inner.x = v`,
   `self.inner.x = v`) — is now **also fixed**: the nested-store path resolves
   its base pointer through `nested_store_place_ptr`, which derefs a ref-param
   root via `get_data_ptr` (the same deref the read path does), so the store
   targets the caller's struct. Both backends now write through every nested
   place shape.

**Tooling gap surfaced *and fixed* by Tangle dogfooding.** The per-function
query kinds (`ownership` / `effects` / `concurrency`) split their target with a
last-dot `rsplit_once('.')`, so an impl method (keyed `Type.method`) was
unreachable — `…kara.GraphNode.add_edge` mis-split into file
`…kara.GraphNode`. Fixed: the target now splits at the `.kara.` extension
boundary, keeping the `Type.method` qualifier intact. Both forms work:

```bash
karac query ownership examples/tangle/src/cross_graph.kara.GraphNode.add_edge
karac query effects   examples/tangle/src/cross_graph.kara.GraphNode.add_edge
```

This is the dogfooding loop working as intended — the demo exercised a real
slice of the tooling hard enough that the gap fell out, and the fix is in the
compiler, not worked around.
