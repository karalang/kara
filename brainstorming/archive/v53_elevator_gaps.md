# Language Design Gaps — Elevator Example

Gaps and improvement opportunities surfaced while writing `examples/elevator.kara`
(SCAN algorithm elevator controller). Each item references the relevant design.md section.

---

## GAP-1 — Vec has no remove-by-value

**Observed:** Every "remove this specific value from a Vec" operation requires a
manual index-search loop followed by `remove(idx)`. This pattern appeared twice
in the elevator example and is nearly universal in algorithm code.

**Spec ref:** `design.md § Standard Data Structures — Vec[T]` (method table).

**Options:**
- `Vec.remove_first(predicate: Fn(ref T) -> bool) -> Option[T]` — removes the
  first element satisfying the predicate, returns it.
- `Vec.retain(predicate: Fn(ref T) -> bool)` — keeps only elements where
  predicate is true (in-place filter). Mirrors Rust's `retain`.
- Both. They are complementary: `retain` for bulk removal, `remove_first` for
  targeted single removal.

**Recommendation:** Add both. `retain` is particularly important because it
avoids the "find index, then remove, shifting elements" two-step that is
O(n²) when done in a loop.

**Status:** RESOLVED — `Vec.retain` and `Vec.remove_first` added to design.md §Vec[T] method table. Implementation tracked in checklist (Phase 8 stdlib).

---

## GAP-2 — No sorted set or priority queue

**Observed:** The SCAN algorithm is naturally expressed as two priority queues
(min-heap for upward stops, max-heap for downward stops). The current stdlib
forces a `Vec` with manual `sort()` after each `push()`.

**Spec ref:** `design.md § Standard Data Structures` (collection table).

**Options:**
- `SortedSet[T: Ord]` — B-tree-backed sorted set; O(log n) insert, remove,
  min, max, and range iteration. Equivalent to Rust's `BTreeSet`.
- `MinHeap[T: Ord]` / `MaxHeap[T: Ord]` — binary heap; O(log n) push/pop,
  O(1) peek. No membership test, no arbitrary removal.

**Recommendation:** `SortedSet[T: Ord]` as the primary addition. It supports
all the operations the elevator needs (insert, remove-by-value, ordered
iteration) and generalizes `MinHeap`. A `MinHeap` alias or separate type can
come later for pure priority-queue workloads where BTree overhead matters.

**Status:** RESOLVED — `SortedSet[T: Ord]` added to design.md stdlib collection table with full method spec. Implementation tracked in checklist (Phase 8 stdlib).

---

## GAP-3 — Refinement types cannot narrow pattern exhaustiveness

**Observed:** A `type ValidFloor = i64 where self >= 1 && self <= 10` would
make invalid floors a construction-site error. But the exhaustiveness checker
still treats `ValidFloor` as the full `i64` domain, requiring a wildcard arm
in any match. The constraint is verified at construction but invisible to the
pattern system.

**Spec ref:** `design.md § Refinement Types`, `design.md § Pattern Exhaustiveness
— Refinement types`.

**The spec explicitly acknowledges this:** "Reasoning about which base values
are excluded by an arbitrary constraint is SMT territory and is reserved for
Level 3-4 gradual verification." This is not an oversight — it is a deliberate
deferral with a known resolution path.

**Short-term improvement (no SMT required):** For refinements over a *bounded
integer range* (`where self >= A && self <= B` where A and B are constants),
the compiler could enumerate the range and treat the refinement type like an
enum for exhaustiveness purposes. This is decidable without SMT and covers the
floor-numbering pattern directly.

**Longer-term:** Full gradual verification (Feature 6) resolves this generally.

**Status:** RESOLVED (short-term) — Bounded-range finite-domain exception (`self >= A && self <= B` → no wildcard required) added to design.md §Refinement Types — Pattern Exhaustiveness and syntax.md §5.6. Implementation tracked as sub-item (8) of the Maranget exhaustiveness upgrade in checklist (Phase 3). General SMT case remains deferred to Level 3-4 gradual verification as intended.

---

## GAP-4 — distinct types prohibit arithmetic

**Observed:** If `FloorNum` were `distinct type FloorNum = i64`, then
`floor + 1` is a compile error. Every arithmetic operation requires explicit
`FloorNum(i64(floor) + 1)` unwrap/rewrap, making arithmetic-heavy domains
(floor arithmetic, pixel coordinates, indices) verbose.

**Spec ref:** `design.md § Distinct Types (Newtypes)`.

**Tension:** The whole point of `distinct type` is to prevent accidental mixing
(e.g., adding a `FloorNum` to a `UserId`). Allowing arithmetic re-introduces
the mixing risk if both are `distinct type _ = i64`.

**Option A — Opt-in `Arithmetic` trait:** `distinct type FloorNum = i64
implements Arithmetic` allows `+`, `-`, `*`, `/` on `FloorNum` values among
themselves but still rejects `FloorNum + UserId`.

**Option B — Structured offset types:** Distinguish the *quantity* (`FloorNum`)
from the *delta* (`FloorDelta = i64`). `FloorNum + FloorDelta -> FloorNum` is
allowed; `FloorNum + FloorNum` is not. This matches physics-style dimensional
analysis and is the most type-safe option, at the cost of requiring two type
declarations per domain.

**Recommendation:** Option A first (lower friction); Option B as a design
pattern documented in the book.

**Status:** RESOLVED — `#[derive(Arithmetic)]` (Option A) added to design.md §Distinct Types and syntax.md §3.13. Implementation tracked in checklist (Phase 3).

---

## GAP-5 — Invariant blocks only fire on pub methods

**Observed:** The elevator's core internal invariant —
`!stops.is_empty() => direction != Idle` — is a private-state concern that
never surfaces at a public boundary. The design's `invariant` blocks (Feature 6)
are checked at `pub` method exits only, so this invariant has no enforcement
mechanism.

**Spec ref:** `design.md § Contracts (requires / ensures / invariant)` — Rule 3:
"Invariants are checked at the exit of every `pub` method."

**Proposed change:** Allow `invariant` to be scoped to `impl` (fires on all
method exits) in addition to `pub` (current rule). An `impl`-scoped invariant
is an internal contract: not visible to callers, not part of the public API,
but checked by the compiler (or at runtime under gradual verification) on every
method return. Syntax could be `impl invariant { ... }` vs the current `pub
invariant { ... }` (or the default becoming all-methods).

**Note:** This is especially valuable for `shared struct` and `par struct` where
field mutations can happen across many methods and the invariant is the only
centralized correctness statement.

**Status:** RESOLVED — `impl invariant` block added to design.md §Struct invariants and syntax.md §3.2 grammar. Implementation tracked in checklist (Phase 9 sub-item 5b).

---

## GAP-6 — No incremental path from single-task to concurrent

**Observed:** The elevator state is a plain struct. Moving to a real system
where floor-button presses come from concurrent tasks requires switching to
`par struct` with `Mutex[Vec[i64]]` fields and `lock` blocks at every queue
access — a non-trivial rewrite with no migration path.

**Spec ref:** `design.md § Feature 4 Part 5 — Shared Types`, `Part 5b — par struct`.

**The structural gap:** The language makes the concurrent form *correct* but
not *reachable* without rewriting the API. This punishes early-design exploration:
you write the clean sequential version, get it right, then throw away the
structure to go concurrent.

**Option A — Compiler-assisted migration:** When the compiler detects a plain
struct being accessed from multiple concurrent tasks, emit a structured error
with a machine-applicable fix-diff that wraps contested fields in `Mutex` and
inserts `lock` blocks. This keeps the migration manual but makes it mechanical.

**Option B — `#[shareable]` annotation:** Marks a struct as "make this
thread-safe when the compiler detects cross-task sharing." The compiler
auto-wraps fields in `Mutex` at the IR level; source code stays identical. The
public API is unchanged; callers see no difference.

**Option C — Accept the gap; document the pattern:** The sequential → concurrent
rewrite is inherent in the `par struct` design. Document it as an intentional
two-stage workflow in the book: "write it sequentially, make it concurrent when
you need to." This is honest but will feel like friction to users coming from
languages with implicit shared-memory concurrency.

**Recommendation:** Option A is the most Kāra-consistent approach — the compiler
already emits structured fix-diffs for effect annotation mismatches. Extending
that to ownership tier migrations is a natural evolution.

**Status:** RESOLVED — Compiler-assisted migration diagnostic (Option A) added to design.md §Compiler-assisted migration from plain `struct` to `par struct`. Implementation tracked in checklist (Phase 6).

---

## GAP-7 — Effect granularity stops at the struct boundary

**Observed:** Reads of `floor` and writes to `stops` both attribute to a single
inferred "writes Elevator" effect. If they were tracked as distinct synthetic
effect resources, the compiler could determine that a pure read of `floor`
alongside a write of `stops` is non-conflicting.

**Spec ref:** `design.md § Module-Level Bindings — Effect attribution` (the
per-binding synthetic resource). That mechanism applies only to module-level
`let mut`, not struct fields.

**Why it matters:** In `par {}` regions, the coarse field-bundling prevents
parallelism the compiler could otherwise exploit. Large structs with logically
independent sub-systems (config, metrics, queue, cache) all serialize against
each other.

**Option:** Extend synthetic per-binding resources to `mut` fields of `par struct`
(where fields are already individually accessed through `Mutex`/`Atomic`). For
plain structs, the field-level granularity is less useful because the receiver
mode (`ref self` vs `mut ref self`) already controls access at the struct level.

**Status:** DEFERRED (v1.5) — Per-field synthetic effect resources for `par struct` fields are architecturally correct but require reworking the effect system from binding granularity to field granularity. Documented in design.md §`par struct` — Field-level effect granularity and added to docs/deferred.md as a P1 v1.5 item. v1 mitigation: split independent subsystems into separate `par struct` types.

---

## GAP-8 — Enum variants have no auto-derived Display

**Observed:** Printing `Direction.Up` in an f-string required a manual
`impl Display for Direction` block. The spec's `#[derive(Display)]` covers
struct fields but is silent on how it handles enum variants.

**Spec ref:** `design.md § Derive`.

**Proposed:** `#[derive(Display)]` on an enum should emit the variant name as
the string representation (e.g., `Up`, `Down`, `Idle`). A
`#[derive(Display(snake_case))]` variant could emit `up`, `down`, `idle`.
This covers the overwhelming majority of debug/logging use cases with zero
boilerplate and matches the ergonomics of Python's `str(enum_value)` and
Rust's `#[derive(Debug)]`.

**Status:** RESOLVED — `#[derive(Display)]` on enums with variant-name output and `snake_case` option added to design.md §Derive and syntax.md §3.3. Implementation tracked in checklist (Phase 3).

---

## GAP-9 — Channel[T] API is absent from the stdlib spec

**Observed:** The effect system tracks `sends(Ch)` and `receives(Ch)`, but
`Channel[T]` constructors and method signatures are not in the stdlib collection
table. A real elevator controller dispatches requests via a `Channel[Request]`
between floor-button tasks and the controller loop — this pattern cannot be
written from the spec alone.

**Spec ref:** `design.md § Feature 2 — Execution Effects`, `design.md § Feature 5
— Auto-Concurrency`.

**Needed (minimum):**
```
Channel.new[T]() -> (Sender[T], Receiver[T])
  with allocates(Heap)

Sender[T].send(self, val: T)
  with sends(self) allocates(Heap)

Receiver[T].recv(ref self) -> T
  with receives(self) suspends

Receiver[T].try_recv(ref self) -> Option[T]
  with receives(self)    // non-blocking
```

The effect annotations on `send` / `recv` are what make channels the model
case for `sends` / `receives` effects — they should appear in the spec alongside
the effect verb definitions, not be left implicit.

**Status:** RESOLVED — Full `Channel[T]` / `Sender[T]` / `Receiver[T]` API spec with effect annotations added to design.md §Channel[T] API. Implementation tracked in checklist (Phase 8 stdlib).

---

## GAP-10 — Iterator adaptors are unspecified

**Observed:** `next_stop` and `mark_served` use manual index loops because
`iter()` / `into_iter()` / `iter_mut()` return types are specified but no
adaptor methods are listed. With standard adaptors the methods collapse:

```kara
// next_stop (Up arm):
self.stops.iter().find(|f| f >= self.floor)

// mark_served:
let idx = self.stops.iter().position(|f| f == target);
if let Some(i) = idx { self.stops.remove(i); }
```

**Spec ref:** `design.md § Standard Data Structures — Iteration`. The table
ends at `into_iter()` Item types; no `Iterator` trait methods are listed.

**Impact:** This is the highest-frequency gap in the example. Almost all
non-trivial code needs `map`, `filter`, `find`, `position`, `fold`, `any`,
`all`, `enumerate`, `zip`, `flat_map`, `take`, `skip`. Without them every
collection operation degrades to a while loop. The `collect_all(...)` builtin
is mentioned but the standard adaptor chain is entirely absent from the spec.

**Recommendation:** Specify `Iterator[Item]` trait methods as a stdlib section
parallel to the collection method tables. This is not a language change — the
types and mechanics are already in place via `impl Iterator` return types.
It is a documentation and stdlib completeness gap.

**Status:** RESOLVED — Full `Iterator[Item]` adaptor table (`map`, `filter`, `flat_map`, `fold`, `enumerate`, `zip`, `take`, `skip`, `take_while`, `collect`, `count`, `any`, `all`, `find`, `position`, `skip_while`) added to design.md §Iterator Adaptors. Implementation tracked in checklist (Phase 8 stdlib).
