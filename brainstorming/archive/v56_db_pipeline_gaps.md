# Language Design Gaps — DB Pipeline

Gaps found while building `examples/db_pipeline/` — a query parse → plan → execute
pipeline using Kāra's effect resource and provider injection system.
This project exercises the most Kāra-specific features of any of the three examples.

---

## GAP-M — Recursive enums require shared enum (RC)

**File:** `src/query.kara`

**Observed:** A realistic query AST needs recursive nodes:
```kara
enum Expr {
    And(Expr, Expr),   // ERROR: infinite size
    Or(Expr, Expr),
    Eq { column: String, value: Value },
    Literal(Value),
}
```
A plain recursive enum has infinite size. In Rust, you box the inner variant:
`And(Box<Expr>, Box<Expr>)`. In Kāra, the equivalent is `shared enum Expr`, which
uses RC to give each node a fixed-size pointer to its children.

**The gap:** The spec does not state whether a plain recursive enum is a compile error
(with a diagnostic pointing to `shared enum`) or is silently promoted to RC. If it's
a compile error, users need to know the `shared enum` pattern before they can write any
tree-shaped data structure. If it's silently promoted, the performance cost is invisible.

**Recommendation:** Make it a compile error with a clear diagnostic:
```
error: enum `Expr` is recursive without indirection
  note: variant `And` contains `Expr`, which contains `And`, which...
  help: use `shared enum Expr` for reference-counted tree nodes, or
        wrap the recursive field in a `Vec` for spine-only recursion
```
The `shared enum` pattern should be documented as the idiomatic ADT tree form.

**Status:** RESOLVED. Added to design.md §Feature 3: Algebraic Data Types. Plain recursive enum is a compile error with a structured diagnostic showing the cycle and pointing to `shared enum` as the fix. `shared enum` is documented as the idiomatic form for tree-shaped ADTs (query ASTs, expression trees, JSON values).

---

## GAP-N — Effect resource method dispatch syntax is unspecified

**File:** `src/executor.kara`, `src/db.kara`

**Observed:** Inside a function with `with reads(Db)`, calling `Db.query_table(...)` is
assumed to dispatch to the current provider. The spec shows this form in its canonical
example (`fn save_user with writes(UserDB) { UserDB.save(...) }`) but does not specify:

1. Whether `Db.query_table(args)` is syntactic sugar for
   `<current_Db_provider>.query_table(args)` or something else.
2. What happens when no provider is in scope — compile error, or panic at runtime?
3. Whether the provider is resolved at compile time (monomorphized) or at runtime
   (dynamic dispatch through a vtable).
4. Whether calling `Db.query_table` without a `with reads(Db)` annotation is a
   compile error (capability-checking) or just an untracked call.

**Spec ref:** `design.md § Feature 2 — Provider-Rooted Resources`,
`§ providers { } in { } Block`.

**This is the most critical ambiguity in the entire effect system.** Without specifying
the desugaring, it is impossible to reason about whether a program is correct.

**Proposal:** Add a "Resource call desugaring" subsection to Feature 2 that specifies:
- The ambient provider lookup mechanism (thread-local slot? lexical scope? implicit parameter?)
- The type of a resource call expression
- The error for calling without a provider in scope

**Status:** RESOLVED. Added "Resource call desugaring", "Capability requirement", and "No provider in scope → runtime panic" paragraphs to design.md §Provider-Rooted Resources. Summary: (1) `Db.method(args)` desugars to vtable call through per-task provider stack — runtime dispatch; (2) calling `Db.method(...)` contributes `reads/writes(Db)` to inferred effect set; omitting it on a public function is an effect-mismatch compile error; (3) no provider on stack at call time → runtime panic with structured diagnostic; (4) program-rooted resources (`FileSystem`, etc.) never panic because they have default providers installed at program start.

---

## GAP-O — Parameterized resources + provider injection interaction unspecified

**File:** `src/db.kara`

**Observed:** `effect resource Db: DatabaseProvider` is a single resource. Two
concurrent writes to different tables both carry `writes(Db)` and serialize —
even if the tables are logically independent.

The spec mentions `effect resource UserDB[user_id: i64]` for finer granularity
(parameterized resources). A `Db[table: String]` form would let:
```kara
fn write_users(...) with writes(Db["users"]) { ... }
fn write_orders(...) with writes(Db["orders"]) { ... }

par {
    write_users(u);    // writes(Db["users"])
    write_orders(o);   // writes(Db["orders"]) — different resource, no conflict
}
```

**The unspecified interaction:** `with_provider[Db](provider, || ...)` binds the
*base* resource `Db`. When `Db` is parameterized, how does `with_provider` bind it?

- `with_provider[Db](provider, || ...)` — binds all parameterizations of `Db`?
- `with_provider[Db["users"]](users_provider, || ...)` — binds one parameterization?
- Both `Db["users"]` and `Db["orders"]` require separate providers?

**Spec ref:** `design.md § Parameterized Resources`, `§ Provider-Rooted Resources`.

**This gap prevents parameterized resources from being usable in practice** until
the interaction with provider injection is fully specified.

**Status:** RESOLVED. Added "`with_provider` and parameterized resources" paragraph to design.md §Provider-Rooted Resources. Rule: `with_provider[Db](provider, ...)` always binds the base resource name; all `Db[key]` parameterizations route through the same provider instance. There is no per-key binding — if different keys need different backends, use separate base resources.

---

## GAP-P — with_provider scoping forces assertions inside closures

**File:** `src/executor_test.kara`

**Observed:** The spec states provider-rooted resources must not leak past their scope.
This means `with_provider` returns only `T` where `T` does not capture the provider.

In tests, this forces a pattern where assertions must live inside the closure:
```kara
// Cannot do this:
let rows = with_provider[Db](db, || execute(plan(q)));
assert_eq(rows.len(), 2);   // ERROR: rows may capture Db

// Must do this:
with_provider[Db](db, || {
    let rows = execute(plan(q)).unwrap();
    assert_eq(rows.len(), 2);   // OK: assertion inside scope
});
```

**Impact on test ergonomics:** Every test has an extra level of nesting. Multiple
independent assertions require either one large closure or multiple `with_provider`
calls (each seeding the database again). The second option defeats the purpose of a
shared test fixture.

**Options:**
1. **Accept the nesting** — it is an honest reflection of the provider's scope.
2. **Test-specific `scope_provider[R](provider)`** that returns a guard which drops
   the provider when it goes out of scope (RAII style). Safer ergonomics, same semantics.
3. **Allow `with_provider` to return `T` when the compiler proves `T` does not
   capture a provider reference.** This is the most ergonomic but requires escape
   analysis that may be too complex.

**Recommendation:** Option 2. A `ScopedProvider` guard type follows Kāra's RAII
model and makes test setup one line instead of a nested closure.

**Status:** RESOLVED (with clarification). The gap's example is based on a misreading: `with_provider` CAN return plain data (`T` that does not capture the resource). `let rows = with_provider[Db](db, || execute(q))` where `rows: Vec[Row]` is valid — the escape restriction only applies to closures/functions that would invoke the resource after scope exit. Added a clarifying "Plain data can always be returned" paragraph to design.md §Provider-Rooted Resources. For test ergonomics, `#[with_provider(Db, InMemoryDb.new)]` on the test function eliminates nesting entirely and gives each test a fresh provider instance. `ScopedProvider` is not needed.

---

## GAP-Q — No Map.entry API

**File:** `src/db.kara`

**Observed:** `InMemoryDb.insert_row` needs to append to an existing Vec in the map.
Without an entry API, the pattern is:
```kara
let mut rows = self.tables[table].clone();   // clone the Vec
rows.push(row);
self.tables.insert(table.clone(), rows);    // re-insert
```
This is O(n) in the number of rows for each insert (cloning the entire Vec) and
requires cloning the key string.

Rust's `HashMap.entry().or_insert_with(Vec::new).push(item)` is O(1) for the
map lookup and avoids all cloning. Kāra's `Map[K, V]` has no equivalent.

**Spec ref:** `design.md § Standard Data Structures — Map[K, V]`.

**Proposal:**
```kara
fn entry(mut ref self, key: K) -> Entry[K, V]

enum Entry[K, V] {
    Occupied { value: mut ref V },
    Vacant { key: K, map: mut ref Map[K, V] },
}

impl[K: Hash + Eq, V] Entry[K, V] {
    fn or_insert(self, default: V) -> mut ref V { ... }
    fn or_insert_with(self, f: Fn() -> V) -> mut ref V { ... }
    fn and_modify(self, f: Fn(mut ref V)) -> Entry[K, V] { ... }
}
```
This is a direct port of Rust's entry API and is one of the most commonly needed
Map operations in real code.

**Status:** RESOLVED. Added `Map.entry()` row to the Map method table in design.md §Collection Core Methods, plus an `Entry[K, V]` enum and `or_insert` / `or_insert_with` / `and_modify` impl paragraph with example (`self.table.entry(key).or_insert_with(Vec.new).push(row)`).

---

## GAP-R — Map.keys() / Map.values() do not produce owned collections

**File:** `src/planner.kara`

**Observed:** Converting a `Row` (Map[String, Value]) into parallel `Vec[String]`
(keys) and `Vec[Value]` (values) requires a manual loop. The spec's `Map.keys()`
returns `impl Iterator[Item = ref K]`, which is correct for iteration but cannot
be directly collected into `Vec[String]` without consuming (cloning) each key.

The pattern `keys.push(k)` inside a `for (k, v) in row` loop works but requires
explicit `k.clone()` since `k: ref String`. Without the `clone()`, the loop body
borrows `k` from `row` while simultaneously trying to own it in the Vec.

**Impact:** This reveals a general tension between the borrow model and collection
construction: building a `Vec[T]` from elements borrowed from another collection
always requires clone, even when the source collection is being consumed. An
`into_iter()` that yields owned `(K, V)` pairs (moving out of the map) would
avoid this — which is what `into_iter()` is supposed to do per the spec's iteration
table, but the interaction with destructuring `for (k, v) in row` is not shown.

**Status:** RESOLVED. Fixed a spec inconsistency (line 1863 claimed `for (key, value) in map` yields `Item = (K, V)` — incorrect, since bare `for` calls `.iter()` and yields `(ref K, ref V)`). Replaced with a "Destructuring in for loops" paragraph in design.md §Iterator Traits showing both forms: bare `for` (borrows, `ref K`/`ref V`) and `for ... in map.into_iter()` (consuming, owned `K`/`V`). The consuming form was already in the method table; it just needed an example.

---

## GAP-S — effect resource reads(Db) on parallel plans limited by write conflicts

**File:** `src/executor.kara`

**Observed:** `execute_pair` runs two plans in `par {}`. This is valid for two
FullScan/FilterScan plans (reads(Db) + reads(Db) = safe). But the function signature
accepts any `QueryPlan`, including Insert/Delete. If a write plan is passed,
the `par {}` block becomes a compile error at the use site, not at `execute_pair`'s
declaration.

This means callers of `execute_pair` get a confusing error ("par block contains
conflicting effects") rather than a clear error at the call site ("Insert plan is
not allowed in execute_pair").

**The root cause:** `QueryPlan` is a single enum covering both read and write
operations. There is no way in the current type system to express "this function
only accepts read plans."

**Options:**
1. **Split QueryPlan** into `ReadPlan` (Select, SelectWhere) and `WritePlan`
   (Insert, Delete). `execute_pair` takes two `ReadPlan`s.
2. **Encode read-only at the effect level:** `execute_pair` is declared
   `with reads(Db)` (no `writes(Db)`). Any write plan passed to `execute` inside
   the function would cause a verification failure at compile time. This works but
   the error appears inside `execute_pair`'s body, not at the caller.
3. **Accept the current behaviour** and document that `execute_pair` is for
   read-only plans; the compiler enforces this at the `par {}` site.

**Recommendation:** Option 1 is the cleanest API design and a good example of using
Kāra's type system to encode operation capabilities. Option 2 is a useful fallback
when splitting the enum would create too much duplication.

**Status:** NOT A SPEC GAP. The effect system correctly serializes conflicting writes. This is an API design choice for the project: split `QueryPlan` into `ReadPlan` and `WritePlan` so `execute_pair` can accept only read plans. No spec change needed.

---

## GAP-T — Type-level enum variant restriction

**File:** `src/executor.kara`

**Observed:** There is no way to express "this function accepts only the FullScan and
FilterScan variants of QueryPlan" in the type system. The only options are:

1. Runtime match with panic on disallowed variants.
2. Wrapping variants in separate types (GAP-S option 1).
3. Refinement types on the enum (not supported for exhaustiveness — GAP-3 from v53).

This is a general limitation: Kāra inherits the common ADT design constraint that
you cannot "subset" an enum without creating a new type. Some languages address this
with row-polymorphic variants (OCaml's polymorphic variants) or extensible enums.

**This is a deliberate design tradeoff**, not an oversight — nominal ADTs are simpler
and more toolable than structural/row-polymorphic variants. Documenting the
workaround pattern (split enum) in the book is the right response.

**Status:** DELIBERATE DESIGN TRADEOFF. No spec change. Row-polymorphic variants are not planned for v1. The book should document the "split enum" pattern as the idiomatic solution.
