# WIP — List 2 (parallel-safe work)

Migrated home of the parallel-safe List-2 entries from the main `README.md` WIP
section, kept here so List-1 work doesn't share a file with List-2 work and
either side can commit independently. Currently contains one in-flight
initiative (Iterator trait full adaptor surface, below). New parallel-safe
items get appended as their own sections; once an initiative completes, its
section is removed and the canonical phase entry is marked `[x]`. When this
file has no remaining sections, delete it.

## Iterator trait — full adaptor surface

Multi-commit work for the canonical phase-8 entry _(`phase-8-stdlib-floor.md`,
search `Iterator trait — full adaptor surface`)_. Once all subtasks land, the
phase-8 entry gets flipped to `[x]` (subtask 15) and this section is deleted.

**Why this file exists.** The canonical `README.md` L2 entry estimates "5–10
commits" and assumes the baseline adaptors (`filter`, `map`, `collect`, `fold`,
`any`, `all`) are already in place. They aren't — the codebase has only
trait-shell registration (`src/typechecker.rs:2074-2093`), `Item` assoc-type
metadata for collections, and for-loop integration that bypasses `iter()`
entirely (walks raw `Value::Vec` / `Value::Map` / etc. in
`src/interpreter.rs:1674-1730`). No `Value::Iterator` exists; no adaptor methods
dispatch through `eval_method_call`. The realistic count is 13–15 commits, with
the first three being foundation work before any adaptor lands.

**Scope.** All 16 long-tail adaptors named in the L2 entry plus the 6 baseline
adaptors that the design.md § Iterator Adaptors section spells out (the
canonical L2 entry assumes these exist). Codegen is deferred — interpreter +
typechecker only, per the L2 conflict-avoidance rule (stay out of
`src/codegen.rs` and `tests/codegen.rs`).

**Repo conventions.** No Co-Authored-By trailer; prefer `--amend` for tight
follow-ups; commit per subtask (or peer-grouped subtask) so each diff is
reviewable in isolation.

---

## Foundation (commits before any adaptor)

- [x] **1. `Value::Iterator` + `iter()` / `into_iter()` plumbing on collections.**
  Landed alongside this tracker file's introduction. `Value::Iterator { items,
  cursor }` snapshots the source elements eagerly at construction (Map yields
  `(K, V)` tuples; SortedSet flattens to ascending order; Vec/Array/Slice/Set
  pass through). The typechecker registers `iter()` / `into_iter()` as a
  cross-collection dispatch arm in `infer_method_call` (one place, before the
  per-type handlers) using a free `iterator_item_type_for` helper that
  recognizes `Vec`, `Set`, `SortedSet`, `VecDeque`, `Map`, `Array`, `Slice`,
  and `ref` / `mut ref` borrows of those. Both methods return
  `Type::Named { "Iterator", [Item] }`; the borrow-vs-consume distinction is
  immaterial at this layer. New `infer_iterator_method` handler dispatches on
  the Iterator receiver and registers `next() -> Option[T]` (the adaptor
  surface lives in subtasks 3+). The interpreter's `eval_method_call` adds
  `iter` / `into_iter` and `next` arms; `next` writes the advanced cursor
  back through the binding so successive calls observe the new state. 8
  typechecker tests + 6 interpreter tests cover Vec/Array/Map/Set/SortedSet,
  the empty-source `None` case, Map's `(K, V)` tuple shape, and arity errors
  on `iter(arg)` / `next(arg)`. **Note:** `Vec.new()` is not wired in the
  interpreter today; tests on empty Vecs use the `Vec[]` prefix-literal form.
  Subtask 2 (for-loop integration) is the next foundation step.

- [x] **2. `for` loop consumes `Value::Iterator`.** New arm in the for-loop
  driver (`src/interpreter.rs`) drains the iterator via `items.into_iter().
  skip(cursor)` — observably identical to a `next()`-call loop today because
  subtask 1's iterator is eager (no closure-bearing adaptors yet). When
  closures land in subtask 3+, this arm migrates to a true `next()` pull so
  adaptor closures fire per step. Raw-collection arms (Array/Tuple/Set/
  SortedSet/Map) preserved for backwards compat. Typechecker side: `Iterator`
  registered in `env.structs` + `impl_assoc_types` alongside Vec/Array/Set/
  SortedSet so `element_type_of` resolves the bound element via the same
  generic-substitution path as collections; `for x in v.iter()` and
  `for (k, v) in m.iter()` typecheck and bind correctly. 3 typechecker tests
  + 5 interpreter tests cover Vec/Map/Set, cursor resumption (manually
  advance with next() then drop into for-loop), and break/continue inside
  iterator-driven loops.

## Baseline adaptors (the 6 the canonical L2 entry assumes already exist)

- [x] **3. `map(f)` + `filter(pred)`.** New `IteratorStep` enum (`Map(Value)`
  + `Filter(Value)`) extends `Value::Iterator` with a lazy adaptor chain.
  `eval_method_call` arms for `map` / `filter` evaluate the closure once at
  construction, push it as the matching step, and return the modified
  iterator. New `iterator_step` helper drains items one at a time, applying
  steps in order — `Filter` may reject; the loop retries until an item passes
  every step or the source exhausts. New `invoke_function_value` helper
  invokes a `Value::Function` against pre-evaluated args (no CICO write-back,
  no default-eval, no type-substitution stack). The for-loop arm and `next()`
  arm both route through `iterator_step` so adaptor closures fire per pull.
  Typechecker side: `infer_iterator_method` adds `map(f: Fn(T) -> U) ->
  Iterator[U]` (U solved from the closure's actual return type via
  check_expr's closure-pushdown — `Type::Function { return: TypeParam(U) }`
  pushed in, body inferred freely, return type read back) and `filter(pred:
  Fn(T) -> bool) -> Iterator[T]` (return type known so plain check_expr
  suffices). 6 typechecker tests + 7 interpreter tests cover map's element
  type-change, filter's preservation, stacked maps threading types
  (i64 → bool → String), explicit closure-param annotations, non-bool
  predicate rejection, arity errors, lazy step-by-step pulls, the
  empty-after-filter case, and `(K, V)` destructuring on `Map.iter().map(...)`.

- [x] **4. `collect()` + `fold(init, f)` + `count()`.** Three terminals,
  all routed through `iterator_step` so adaptor chains fire during the
  drain. `count()` walks the iterator and returns the element count as
  `i64`. `collect()` v1 is `Vec`-only — drains into `Value::Array` with
  typechecker return `Vec[T]`; `FromIterator`-driven dispatch into other
  collections is a follow-up CR. `fold(init, f)` infers the accumulator
  type `A` from `init`, then `check_expr`s the closure against
  `Fn(A, T) -> A` (both params and return concrete, so plain check_expr
  suffices). 10 typechecker tests + 10 interpreter tests cover plain
  walks, empty-source cases, composition with filter / map adaptors,
  String accumulator, type-mismatch on closure return, and arity errors.

- [x] **5. `any(pred)` + `all(pred)`.** Short-circuit terminal predicates,
  routed through `iterator_step` so adaptor closures fire only for the
  prefix the predicate has to inspect. `any` returns true on the first
  `true`; `all` returns false on the first `false`. Both share the same
  `Fn(T) -> bool` signature so closure-pushdown via `check_expr`
  suffices (no fresh type variable). Empty-source semantics are the
  identity element of each: `any` → `false`, `all` → `true`. 7
  typechecker tests + 9 interpreter tests cover positive / negative /
  empty-source cases, short-circuit (predicate println side effects
  observed for the prefix only), composition with map for the
  predicate's element type, and arity / non-bool errors.

## Long-tail adaptors (the 16 named in the L2 entry)

- [x] **6. `enumerate()` + `take(n)` + `skip(n)`.** Three positional /
  count adaptors. Required extending `IteratorStep` with stateful
  variants (`Enumerate(idx)` / `Take(remaining)` / `Skip(remaining)`)
  and refactoring `iterator_step` to mutate the cloned step chain
  in-place and write it back into the iterator's `steps` field before
  return — earlier closure-only steps were stateless. `take`
  exhaustion drains the source cursor so subsequent pulls return
  `None` without re-evaluating downstream adaptors. Negative `n` clamps
  to zero at the runtime layer (typechecker accepts any i64). 8
  typechecker tests + 11 interpreter tests cover index plumbing,
  bound semantics for `take(0)` / `take(n>len)` / `skip(n>len)`,
  state persistence across separate `next()` pulls, composition
  (`skip.take` window, `filter.take` first-n-passing,
  `map.enumerate` mapped tuple), tuple type threading, and arity /
  non-int errors.

- [x] **7. `chain(other)` + `zip(other)`.** Two-source combinators.
  Required restructuring `Value::Iterator`: previous `items` + `cursor`
  fields collapsed into a new `IteratorSource` enum with three
  variants — `Eager { items, cursor }` (the existing `coll.iter()`
  snapshot path), `Chain { parts, current }` (sequential
  concatenation), and `Zip { left, right }` (synchronous pair). New
  `pull_source` helper does the source-layer pull; `iterator_step`
  now layers the adaptor chain on top of whatever `pull_source`
  yields. Each side of a chain / zip retains its own step chain;
  downstream adaptors append to the wrapping iterator's empty steps,
  applying uniformly to all yielded items. Chain / zip use
  `mem::replace` to take parts out across recursive `iterator_step`
  calls (avoids aliasing `&mut self` with the iter binding); state
  flows back via writeback after the recursive call. 7 typechecker
  tests + 12 interpreter tests cover order, shorter-side stop on
  zip, per-side adaptors firing inside chain / zip, downstream
  steps applying to both chained sides, state persistence across
  separate `next()` pulls, and arity / element-type errors.

- [x] **8. `take_while(pred)` + `skip_while(pred)`.** Predicate-bounded
  adaptors. Two new `IteratorStep` variants — `TakeWhile { pred, done }`
  and `SkipWhile { pred, done }`, both carrying a closure plus a
  one-shot transition flag. `take_while` evaluates `pred(item)` per
  pull; on the first false it sets `done = true`, signals stop, and
  drains the source so subsequent pulls also return None (sticky-stop).
  `skip_while` rejects items while `pred(item)` is true; on the first
  false it flips `done = true` and yields the trip element AND every
  subsequent element unconditionally without re-firing the predicate
  (sticky-pass). Both share `filter`'s `Fn(T) -> bool` signature so
  closure-pushdown via `check_expr` suffices. 7 typechecker tests + 12
  interpreter tests cover the prefix-only / first-fails-yields-nothing
  / all-pass / first-fails-yields-all axes, the sticky semantics
  (predicate side-effect prefixes prove no re-fire after trip), state
  persistence across separate `next()` pulls, composition with filter
  and with each other (`skip_while.take_while` while-window), and
  arity / non-bool errors.

- [x] **9. `flat_map(f)`.** Closure returns an iterator; flatten the result.
  New `IteratorSource::FlatMap { outer, f, current_inner }` variant —
  source-layer not step-layer, same shape as `Chain` / `Zip`. The
  outer is itself a `Value::Iterator` (so its own adaptor chain
  fires); `f` is the boxed closure (`Value::Function`); `current_inner`
  is the in-flight inner iterator across `next()` pulls. `pull_source`
  drains the in-flight inner first; on exhaust, advances the outer
  via recursive `iterator_step`, applies `f` to the outer item, and
  retries. `f` is `Box<Value>` because `Value::Iterator` embeds
  `IteratorSource` inline — without indirection the size of `Value`
  recurses through the closure. Typechecker pushes
  `Fn(T) -> TypeParam("__iter_flatmap_U")` so the closure's actual
  return type flows back; the resulting type is unwrapped from
  `Iterator[U]` for the new Item, with explicit TypeMismatch on
  non-iterator returns. 5 typechecker tests + 10 interpreter tests
  cover concatenation order, empty outer, empty-inner-skipping, state
  persistence across separate `next()` pulls, inner-with-adaptors,
  composition with downstream filter / map / take / count and
  upstream filter, and the take-short-circuit case (verified via
  side-effect prefixes that outer N+1 is never visited when take(N)
  is satisfied earlier).

- [x] **10. `step_by(n)` + `cycle()`.** `step_by` is a new
  `IteratorStep::StepBy { n, remaining_skip }` variant — yields the
  current item when `remaining_skip == 0` and resets to `n - 1`;
  otherwise rejects and decrements. n is clamped to `n.max(1)` at the
  dispatch site so the post-yield reset never underflows; the
  typechecker accepts any i64 (matching `take` / `skip`'s clamp
  policy). `cycle` is a new `IteratorSource::Cycle { template,
  current, exhausted }` variant — `template` is the snapshot taken at
  construction (deep-clone via Value's derived Clone), `current` is
  the in-flight clone being drained; on exhaust, `current` is
  replaced by a fresh `template.clone()`. The `exhausted` sticky
  flag flips true if the template itself is empty (avoids the
  infinite-empty-loop trap) — detected at runtime via "if a fresh
  template clone yields None on first pull, stop forever". The
  "cloneable source" requirement noted in design.md is implicit:
  every Value derives Clone, so any iterator can cycle. 7 typechecker
  tests + 14 interpreter tests cover stride correctness (including
  n=0/1/larger-than-length clamps), state persistence across
  `next()` pulls, cycle-with-take, cycle-on-empty (no infinite
  loop), pre-cycle adaptors that re-run each cycle (filter,
  enumerate counters reset), post-cycle adaptors that apply
  uniformly across cycles, composition (`step_by.cycle`,
  `enumerate.cycle`), and arity / non-int errors.

- [x] **11. `inspect(f)` + `scan(state, f)`.** Two new `IteratorStep`
  variants — `Inspect(closure)` (invokes f on each item then passes
  through unchanged; closure return is discarded) and `Scan { f,
  state, done }` (thread mutable state, yield + short-circuit on
  None). Closure signature for scan is `Fn(A, T) -> Option<(A, U)>` —
  closure returns the new state in the first tuple slot and the
  yielded value in the second. This deviates from Rust's
  `Fn(&mut St, T) -> Option<B>` because tree-walk closures snapshot
  captures and there's no `mut ref` parameter mode at the value
  layer; threading state via the return tuple matches the existing
  `fold` "closure returns new accumulator" pattern. The `done` flag
  flips sticky-true after the first None so subsequent pulls
  short-circuit without re-firing the closure. Typechecker for
  `inspect` leaves the closure return free via `TypeParam` pushdown
  (any return type accepted, all discarded). For `scan`, A is
  inferred from `init`; the closure body's actual return is
  pattern-matched for `Option<(A, U)>` with explicit TypeMismatch on
  non-Option / wrong-shape returns. 7 typechecker tests + 10
  interpreter tests cover passthrough, post-filter inspect, scan
  running-sum / running-max / String state / short-circuit on None /
  no-re-fire after stop / state independent of yielded value /
  composition with filter / arity errors.

- [x] **12. `peekable()`.** Wraps the source so `peek()` returns the next
  element without consuming it. Implementation buffers one element. The
  resulting iterator type exposes `peek()` as an additional method beyond the
  Iterator trait.
  Closure: typechecker uses a distinct `Peekable[T]` named type
  (registered alongside Iterator in `register_builtin_types`) so
  `peek()` is dispatchable only on the result of `peekable()`. Adaptor
  methods (`map`, `filter`, `take`, …) on a `Peekable[T]` return
  `Iterator[U]` (peekable-ness is lost), which makes `peek()`
  type-unavailable downstream — matching Rust's Peekable<I> semantics
  via `Map<Peekable<I>>`. The dispatch route at `infer_method_call`
  matches both `Iterator` and `Peekable` names and forwards to
  `infer_iterator_method` with an `is_peekable` flag; the `peek` arm
  rejects with TypeMismatch when `is_peekable=false`. Interpreter
  models the wrapper as a new `IteratorSource::Peekable { inner,
  buffered }` variant — pull_source drains `buffered` first then falls
  through to `iterator_step(inner)`, so any inner steps (map / filter
  / etc.) run before peek/next observe the value. The new
  `peek_value` helper buffers one element and returns
  `Option<T>` (cloned), with binding writeback identical to `next()`.
  Because the typechecker forbids adaptors after peekable() (they
  return Iterator[U]), the wrapping Value::Iterator's `steps` field
  is always empty — peek and next see the same item type without
  walking outer steps. 7 typechecker tests + 10 interpreter tests
  cover Peekable[T] type, peek-after-map sees mapped value,
  peek-on-bare-Iterator rejected, peek-after-adaptor-chain rejected,
  peek-idempotent-until-next, peek-no-re-pull-after-buffered,
  peek-on-drained returns None, count/collect/for-loop drain the
  buffer, peekable+filter chain works at runtime, arity errors.

- [x] **13. `chunk_by(key_fn)`.** Buffering adaptor — allocates a `Vec[T]`
  per group when consecutive elements share the same key. Carries
  `allocates(Heap)` in its effect set per the canonical scope note.
  Closure: typechecker uses TypeParam pushdown for the key — `Fn(T)
  -> __iter_chunk_by_K`, K is left free (equality is enforced at
  runtime via `Value::PartialEq`, matching the permissive
  scan/inspect pattern). Returns `Iterator[Vec[T]]`. Interpreter
  models this as `IteratorSource::ChunkBy { inner, key_fn,
  pending_item, pending_key, exhausted }` — built as a Source
  rather than a Step because each pull consumes many inner items
  and the boundary requires one-item lookahead. The lookahead item
  that triggered the boundary becomes the seed of the next group
  via `pending_item`, with its already-computed key cached in
  `pending_key` so we don't fire the closure twice on the same
  item. Effect: `allocates(Heap)` seeded on `Iterator.chunk_by` in
  the effectchecker stdlib alloc list and routed via
  STDLIB_METHOD_MAP so user fns transitively pick it up. 5
  typechecker tests + 10 interpreter tests + 1 effectchecker test
  cover Iterator[Vec[T]] return shape, post-map element type,
  free K typing, collect to Vec[Vec[T]], consecutive-equal
  grouping, singleton-per-key, single-group-when-constant-key,
  state across next() pulls, post-filter only-kept-items, key_fn
  fires once per item, take(n) short-circuits inner pulls,
  empty-source → no groups, allocates(Heap) propagation.

- [ ] **14. `windows(n)` + `chunks(n)` on Iterator.** Both currently exist as
  inherent methods on `Vec` (`src/interpreter.rs:3491` and `:3505`); this
  subtask adds the Iterator-trait variants. Confirm whether to promote or
  duplicate during implementation — the Vec inherent methods may stay as
  fast-path shortcuts. Both buffer; both `allocates(Heap)`.

## Wrap-up

- [ ] **15. Close out.** Flip canonical `phase-8-stdlib-floor.md` line 21
  checkbox to `[x]` with a closure note pointing at the final adaptor commit.
  Delete this WIP file. Reference the test count + adaptor list in the closure
  note so future readers can audit coverage without rebuilding history.
