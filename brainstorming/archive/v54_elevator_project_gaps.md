# Language Design Gaps — Elevator Full Project

Gaps found while building `examples/elevator_project/` — a multi-module elevator
controller with a generic `Scheduler` trait and two concrete strategies (SCAN, FCFS).
These are in addition to the single-file gaps already in `v53_elevator_gaps.md`.

---

## GAP-A — Effect polymorphism ceiling in trait methods

**File:** `src/scheduler.kara`

**Observed:** The `Scheduler` trait declares `next_stop` as pure (no `with` clause).
Any implementation that needs to log decisions (writes(Log)) or read configuration
(reads(Config)) cannot implement the trait — it fails verification.

The design supports effect-polymorphic trait methods via `fn m[with E](...) with E`
but the semantics of "ceiling" vs "floor" in a trait context are underspecified:

- **Ceiling (maximum allowed effects):** `fn next_stop[with E where E ⊆ {writes(Log)}]`
  — implementors may use any subset.
- **Floor (minimum required effects):** not meaningful for callers; callers need
  a maximum guarantee to reason about what calling `next_stop` does.
- **Effect-polymorphic callers:** an `Elevator[S]` calling `s.next_stop[with E]`
  would propagate `E` to `step`'s effect set. This is correct but makes `Elevator`
  itself effect-polymorphic, which cascades to every call site.

**Spec ref:** `design.md § Effect Polymorphism`, `§ Closures and Effect Capture`.

**Proposal:** Specify the ceiling syntax for trait methods explicitly:
```kara
pub trait Scheduler {
    fn next_stop[with E: EffectSet](ref self, ...) -> Option[i64] with E;
}
```
And define what "ceiling" means: callers see `with E` propagated; the bound
`E ⊆ SomeSet` (if declared) constrains which effects implementors may use.

**Status:** RESOLVED. The spec's §Effect Polymorphism fully covers `with _` (anonymous ceiling — implementors may use any subset) and `with E` (named variable — effects thread to callers). The worked `Processor` example shows exactly the trait method form. The bounded variant `[with E: EffectSet]` is already deferred to Phase 7 as Effect Variable Bounds (deferred.md §Effect Variable Bounds).

---

## GAP-B — Unit struct syntax

**File:** `src/scheduler.kara`

**Observed:** `Scan` and `Fcfs` carry no state. The spec shows `struct Foo { field: T }`
but does not mention a zero-field form. Two candidates:
- `pub struct Scan {}` — empty brace struct (assumed valid)
- `pub struct Scan;` — Rust-style unit struct (not mentioned in spec)

Constructing a unit struct: `Scan {}` vs just `Scan`. Neither is explicitly in the spec.

**Impact:** Minor syntax ambiguity; the compiler could reasonably accept both forms,
but the spec should define the canonical form to avoid ecosystem divergence.

**Status:** RESOLVED. Canonical form is `struct Scan {}` / construction `Scan {}`. The semicolon form is explicitly invalid. Added to design.md §Struct Field Visibility ("Unit structs" paragraph) and syntax.md §3.2 (bullet note). The grammar's `[ STRUCT_FIELDS ]` already admitted empty braces; the new prose locks in the construction form.

---

## GAP-C — Generic impl block syntax

**File:** `src/elevator.kara`

**Observed:** `impl[S: Scheduler] Elevator[S] { ... }` is inferred from the spec's
`impl[E: Display] Display for Contextual[E]` example but only appears in *trait* impl
blocks in the spec. Whether a non-trait `impl[S: Scheduler] Elevator[S]` block uses
the same `impl[Bound] Type[Param]` form is not explicitly confirmed.

**Spec ref:** `design.md § Type System — Generics`.

**Proposal:** Show at least one example of a generic non-trait impl block in the spec.

**Status:** RESOLVED. Added to design.md §Conditional impl Blocks ("Generic non-trait (inherent) impl blocks" paragraph with `impl[S: Scheduler] Elevator[S]` example) and syntax.md §3.5 Impl Blocks (two examples: generic inherent impl and multi-bound inherent impl).

---

## GAP-D — Field shorthand in struct literals

**File:** `src/elevator.kara`

**Observed:** Rust allows `Elevator { floor, direction, stops, scheduler }` when local
variable names match field names. Kāra's spec is silent. Without shorthand, long
struct initialisations repeat each name twice (`scheduler: scheduler`), which is
mechanically redundant and error-prone to read.

**Spec ref:** `design.md § Type System and Object Model — Structs`.

**Proposal:** Add field-init shorthand as a syntactic sugar. Non-breaking addition.

**Status:** RESOLVED. Already in syntax.md §5.15 Struct Literals (grammar `FIELD_INIT = IDENT ":" EXPR | IDENT`, with examples). design.md already uses shorthand in the User constructor example; added a prose note to the §Type Inference "Struct literal" bullet cross-referencing §5.15.

---

## GAP-E — No Clone propagation through Vec in for loops

**File:** `src/main.kara`

**Observed:** After `for req in requests { ... }` the `requests` Vec is consumed
(into_iter moves each element). To replay the same request set for the FCFS elevator,
the literal must be duplicated. `requests.clone()` would work only if `Request`
derives `Clone`, which requires `Direction` to also derive `Clone`, which is fine —
but the chain needs to be explicit (`#[derive(Clone)]` on both), and the spec's
derive macro must support `Vec<CloneableT>.clone()` transitively.

**Impact:** Not a gap in the type system itself, but the lack of documentation
on derive transitivity (does `#[derive(Clone)]` on an enum containing a Vec auto-derive
Vec's Clone?) creates uncertainty. Rust requires the bound to be propagated; Kāra
should be explicit on whether it does the same or infers it.

**Status:** RESOLVED. Already specified in design.md §Conditional impl Blocks: "`#[derive(Eq)]` on `Pair[T]` generates `impl[T: Eq] Eq for Pair[T]` — the compiler infers the minimal bound from field types." The same rule applies to `Clone`: `#[derive(Clone)]` on any type generates `impl[T: Clone] Clone for MyType[T]`. Both `Direction` and `Request` need `#[derive(Clone)]`; there is no implicit chain.

---

## GAP-F — Test setup boilerplate for owned collections

**File:** `src/scheduler_test.kara`

**Observed:** Every test that needs a pre-populated `Vec[i64]` must build it with
three or four `push` calls. There is no `Vec.of(1, 2, 3)` or `[1, 2, 3].into()` idiom.

**Spec ref:** `design.md § Standard Data Structures — Vec[T]` (constructor table).

**Proposal:** `Vec.of[T](items: T...) -> Vec[T]` variadic constructor, or ensure
that the `[1, 2, 3]` bare literal produces `Vec[i64]` (which the spec does say —
"bare `[e1, e2, ..., eN]` synthesizes to `Vec[T]`"). Test code should be able to
write `let stops = [3, 5, 7];` directly. **This may already be valid per the spec
and the tests are written unnecessarily verbosely.** Worth verifying.

**Status:** RESOLVED. Already specified in design.md §Standard Data Structures: "The bare form `[e1, e2, ..., eN]` synthesizes to `Vec[T]`." `let stops = [3, 5, 7];` produces `Vec[i64]`. The verbose push-based test setup was unnecessary.

---

## GAP-G — Scheduler cannot control queue ordering

**File:** `src/scheduler_test.kara`

**Observed:** `Elevator.add_request` sorts the stops Vec after every push (to support
SCAN's sorted-ascending invariant). This silently breaks FCFS, which needs
insertion-order semantics. The `Scheduler` trait has no hook for queue management —
the scheduler decides *which* stop to visit next, but not *how the queue is stored*.

**Options:**
1. Add `fn on_enqueue(ref self, stops: mut ref Vec[i64], new_stop: i64)` to the
   `Scheduler` trait, letting each strategy manage its own queue structure.
2. Make `Elevator` not sort the queue at all; each `Scheduler::next_stop`
   implementation handles the sorting it needs (SCAN sorts to find the sweep
   position; FCFS uses the Vec as-is).
3. Let the Scheduler own the stops entirely: replace `stops: Vec[i64]` in `Elevator`
   with a generic `queue: S::Queue` where `S: Scheduler` defines an associated type.
   This is the cleanest but requires associated types in traits (not yet specified).

**Spec ref:** `design.md § Type System — Traits` (associated types: deferred?).

**Recommendation:** Option 2 in the short term. Option 3 when associated types land.

**Status:** RESOLVED. Associated types are now in the spec (design.md §Associated Types in Traits). Option 3 (`type Queue` associated type on `Scheduler`) is fully available. The language gap no longer applies; the elevator project can adopt whichever option the author prefers.
