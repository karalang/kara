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
| Doubly-linked list | `src/doubly_linked.kara` | both-way links + neighbor-relink splice, no `Weak` | `representation:"shared (Rc)"` (declared RC) |
| Undo/redo over shared state | `src/undo_redo.kara` | shared **mutable** state, undo writes back through the shared handle | `representation:"shared (Rc)"` (declared RC) |
| Tree-walking interpreter (shared scope) | `src/interp.kara` | recursive `shared enum` AST + shared mutable scope threaded through `eval` | `representation:"shared (Rc)"` (declared RC) |

## Running

```bash
karac run   examples/tangle/src/parent_tree.kara     # interpret
karac check examples/tangle/src/parent_tree.kara     # typecheck only
karac query ownership examples/tangle/src/parent_tree.kara.<fn>   # per-fn ownership

karac run   examples/tangle/src/cross_graph.kara
karac query ownership examples/tangle/src/cross_graph.kara.build_diamond

karac run   examples/tangle/src/undo_redo.kara
karac query ownership examples/tangle/src/undo_redo.kara.Editor.set   # impl method

karac run   examples/tangle/src/doubly_linked.kara
karac query ownership examples/tangle/src/doubly_linked.kara.link   # shared(Rc) node params

karac run   examples/tangle/src/interp.kara
karac query ownership examples/tangle/src/interp.kara.eval   # scope param: shared(Rc)
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

`doubly_linked.kara` prints `forward: 1 2 3 4` / `backward: 4 3 2 1` (the
backward walk reverses the forward list, proving the `prev` pointers are genuine
shared handles, not copies), then after splicing out the middle / tail / head
both directions print `3`. In Rust the doubly-linked list is the textbook
`Rc<RefCell<Node>>` with a `Weak` back-pointer (strong `prev` would cycle-leak),
and the splice juggles several `RefCell` borrows at once; in Kāra it is a
`shared struct` with `mut prev` / `mut next` fields and no `Weak`. The honest
cost — strong `prev` forms an RC cycle that plain RC won't reclaim — is the kind
of thing Tangle exists to surface rather than hide.

> **Bug found *and fixed* by this structure.** The `Vec.new()` + `push` + return
> idiom inside `to_vec_forward` / `to_vec_backward` triggered a spurious
> `expected 'Vec[i64]', found 'Vec[?T]'` type warning: `let mut out = Vec.new();`
> records the binding as `Vec[?T]`, a later `out.push(x)` pins `?T` in the
> substitution map but not in the local-scope snapshot, so the return-position
> check compared the stale unresolved binding. Fixed in `src/typechecker.rs`
> (`resolve_identifier_type` resolves a binding's typevars against the
> substitution map at every use — genuinely-unresolved vars stay vars, so it
> never over-resolves); regression tests in `tests/typechecker.rs`. This was a
> general inference gap, not list-specific — any `Vec.new(); …push…; return v`
> hit it.

`interp.kara` prints `result: 40` then `scope x: 10` / `scope y: 30`. It
evaluates the program `x = 10; y = (x + 5) * 2; x + y` over a recursive
`shared enum Expr` AST (children carried through `Vec[Expr]`), threading a
`shared struct Scope` through `eval`. The last two lines are the proof: the scope
handle `main` still holds after `eval` returns sees every assignment made *deep
inside the recursion* — one shared, mutable environment, not a copy per frame. In
Rust the recursive AST needs `Box<Expr>` (sized recursion) and the
mutable-environment-through-recursion is the textbook `Rc<RefCell<Env>>`; in Kāra
both are declared `shared` with no `Box`, no `RefCell`, no `'a`.
`karac query ownership .../interp.kara.eval` shows both the `e` (AST node) and
`scope` parameters as `representation:"shared (Rc)"`. Verified through **both** the
interpreter and codegen (it compiles and runs as a native binary).

> **Findings from this structure** (the recursive interpreter exercised more cold
> surface than any other Tangle program — two fixed, three tracked):
>
> 1. **`Vec.new()` + `push` + return inference gap** — *fixed* (see the
>    doubly-linked note above; same fix, surfaced again here in `scope_get`).
> 2. **Recursive enums need `shared`** — `enum Expr { Add(Expr, Expr) }` is
>    rejected (`E_ENUM_NESTED_ENUM_PAYLOAD`); `shared enum` makes the recursive
>    payload an RC pointer. The diagnostic names this remedy directly — working
>    as designed.
> 3. **Shared-type cycle check rejects a *direct* recursive `shared enum`**
>    *(tracked)*. Even as `shared enum`, a *direct* `Add(Expr, Expr)` is rejected
>    at `karac build` (`shared-type cycle detected: Expr → Expr … will leak`) —
>    although the enum has a non-recursive base variant (`Num`), so expression
>    *trees* are acyclic and free fine. Worse, the same recursion *through*
>    `Vec[Expr]` / `Option[Expr]` passes — even though those indirected forms are
>    the ones that form *real* runtime cycles (parent_tree's `parent ↔ child`).
>    The check rejects the acyclic tree and waves through the genuine cycles. The
>    example routes the AST recursion through `Vec[Expr]` (the accepted form).
>    Tracked in
>    [`phase-7-codegen.md`](../../implementation_checklist/phase-7-codegen.md).
> 4. **`self.field[i]` on a *shared* struct miscompiled** *(FIXED)*. A `ref
>    self` shared receiver indexed into a `Vec` field (`self.values[i]`, read or
>    store) read the wrong buffer — it needs a double-load the index path skipped
>    (and the *store* path didn't even reach the field-index helper from `self`).
>    Originally worked around here by writing `get` / `set` as free functions
>    over a named `Scope` parameter. **Now fixed in codegen** (the index-store
>    path normalises `self`, and the field-index helper resolves a shared
>    receiver via `compile_expr`, mirroring `compile_field_store`'s double-load),
>    so this example uses the idiomatic `impl Scope { get/set }` method form.
>    See [`phase-7-codegen.md`](../../implementation_checklist/phase-7-codegen.md).
> 5. **f-string interpolation isn't string-aware** *(tracked)*. The natural
>    `f"{ get("x") }"` (plain nested quotes) works, but an **escaped** quote
>    `f"{ get(\"x\") }"` is silently emitted as literal text instead of
>    evaluating — the `{…}` extractor balances braces only. Worked around by
>    binding the lookup to a local first. Tracked in
>    [`phase-1-lexer.md`](../../implementation_checklist/phase-1-lexer.md).
> 6. **RC-fallback false positive on sibling match arms** *(tracked)*. A pattern
>    binding reused under the same name in two `match` arms (the `Var`/`Assign`
>    arms of `eval` both bind `name`) is given one binding identity, so a consume
>    in one arm and a use in the sibling arm pair as dominance-incomparable and
>    spuriously fire the RC predicate — even though the arms never both execute.
>    Soundness-safe (conservative over-escalation), but it RC-boxes a movable
>    value and makes `karac query` report a cross-arm `direct_reuse_after_consume`.
>    Tracked in
>    [`phase-7-codegen.md`](../../implementation_checklist/phase-7-codegen.md)
>    with a minimal repro; the fix mirrors the existing per-site alpha-rename
>    used for defer-body inner locals. (A user `shared struct Env` also collides
>    with the built-in ambient `Env` resource — renamed to `Scope` here; a
>    clearer "reserved resource name" diagnostic would help but isn't blocking.)

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
