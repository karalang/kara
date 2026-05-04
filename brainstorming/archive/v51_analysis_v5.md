# v51 Analysis v5 — Stress Test: `concurrent struct`

**Status:** COMPLETE
**Date:** 2026-04-25
**Scope:** Seven-category stress test of the `concurrent struct` proposal from `v51_analysis_v4.md`. Tests whether the two-type split (`shared struct` = RC single-task, `concurrent struct` = Arc concurrent-safe) holds under real usage patterns, generics, `dyn Trait`, receiver rules, and the effect system.

---

## Recap of the Proposal

`v4` diagnoses the root cause: `shared struct` does two structurally different jobs and the language doesn't distinguish them at the definition site. The proposed fix:

- **`shared struct`** — unchanged semantics. RC, single-task only, bare `mut` fields with borrow flags. Cannot cross parallel region boundaries.
- **`concurrent struct`** — new keyword. Always Arc. `mut` fields must be `Atomic[T]` or `Mutex[T]` — no bare `mut`. Can cross parallel region boundaries freely.

Migration is mechanical: any `shared struct` that already uses `Mutex[T]` for concurrent access becomes `concurrent struct`; everything else stays `shared struct`.

---

## Case 1: Single-Owner `shared struct` Moved Into One Parallel Task

```kara
shared struct Node { mut val: i64 }

fn process(node: Node) {    // sole owner
    par {
        task_a(node)        // moved in — task_a is the only holder
        task_b()            // never touches node
    }
}
```

Under `v4`'s rule — "`shared struct` live range overlapping a parallel region → compile error" — this is rejected. But it is safe: only one task receives the value, and no two tasks access the same allocation.

The current arc-promotion pass has the same ambiguity. It solves it by promoting: if the live range overlaps a parallel region, use Arc, regardless of whether the value is cloned into multiple tasks or moved into one. Under `concurrent struct`, the analogous conservative rule is: any overlap → compile error, even for single-owner moves.

That is over-rejection. A move into exactly one task does not create the sharing hazard that `concurrent struct` is designed to prevent. The precise error condition is not "live range overlaps a parallel region" but **"live range overlaps a parallel region AND the value can be accessed from multiple concurrent paths simultaneously"** — which is what the promotion pass already computes (the dominance/fork-join analysis). Under `concurrent struct`, the same analysis drives an error instead of a promotion.

**Finding:** The rule in v4 is underspecified. The implementation needs the same reachability analysis as the current promotion pass — it just delivers a compile error instead of a promotion when `shared struct` is the type. Not a gap in the design; a gap in the spec language. The error message should be: *"cannot move `shared struct Foo` into a parallel region where the same allocation may be accessed from multiple tasks; if `Foo` needs concurrent access, define it as `concurrent struct Foo`."*

---

## Case 2: "Sometimes Concurrent" Types

```kara
// Used single-task in request handlers, but cached and
// shared across tasks in a connection pool.
shared struct Session {
    mut request_count: i64,
    mut last_active: Timestamp,
}
```

Under D-lite, this stays `shared struct`. Fields that need concurrent access get `Mutex[T]`; single-task code accesses bare fields with borrow flags. Under `concurrent struct`, the type is forced to one tier everywhere: all uses pay `Atomic[T]` or `Mutex[T]` overhead — including the single-task hot path in request handlers.

This is a real performance cost. It has the same shape as the Rust `Rc` vs `Arc` problem — in Rust, if a type is used in both single-threaded and multi-threaded contexts, the programmer often ends up using `Arc` everywhere and paying the atomic reference count cost on the single-threaded hot path.

The standard mitigation in the two-type world: separate the data from the handle.

```kara
// Inner data: plain struct, cheap, no overhead
struct SessionState {
    mut request_count: i64,
    mut last_active: Timestamp,
}

// Handle: concurrent struct wraps the state
concurrent struct Session {
    state: Mutex[SessionState],
}
```

Single-task code that needs exclusive access takes `mut ref SessionState` inside a lock block. The overhead (Mutex acquisition) is on the session handle, not on the hot-path fields. This is idiomatic in the two-type world, but it is a design constraint the programmer must learn.

**Finding:** Real tradeoff. Types at the single-task/concurrent boundary pay Mutex/Atomic overhead on single-task uses. The inner-data pattern mitigates it. Under D-lite, the programmer pays Mutex only at the concurrent access sites and nowhere else. This is the most significant practical cost of the two-type design vs the patch path, and the spec should state it explicitly rather than leaving it as an implication.

---

## Case 3: `dyn Trait` With Mixed Implementations

```kara
trait NodeProcessor {
    fn process(ref self, node: ???)
}
```

If the trait is implemented by both a `shared struct AstNode` impl and a `concurrent struct Counter` impl, the method signature cannot carry a single type for `node` — they are different types. A `dyn NodeProcessor` at the call site needs a concrete type for `node`.

This is the correct behavior. A trait whose method signature carries a `shared struct` argument cannot have `concurrent struct` implementations of that argument type — they are distinct types with distinct concurrent-safety guarantees. The type error fires at the trait definition or the impl site, not silently at the call site.

The common pattern — a `dyn Trait` whose methods do not carry `shared struct` or `concurrent struct` parameters in the signature — has no issue:

```kara
trait Worker {
    fn run(ref self)   // no shared type in the signature — fine
}
```

Traits that need to be implemented by both kinds need to be generic over the type (`trait Worker[T]`), which is the same pattern used for effect polymorphism in the existing design.

**Finding:** Not a gap. The type error is the correct outcome for a trait whose signature conflates the two kinds. The `dyn Trait` effect-bound pattern at `docs/design.md:4258–4275` is the direct precedent: traits commit to a contract at definition time, and impls are checked against it. Same rule applies to `shared struct` vs `concurrent struct` in method signatures. No new machinery needed.

---

## Case 4: Generic Bounds — "Concurrent-Safe" Constraint

```kara
fn parallel_map[T: ???](items: Vec[T], f: Fn(T) -> T) -> Vec[T] {
    par { items.map(f) }
}
```

The instinct: `T` needs a `Concurrent` bound to prevent `shared struct` from leaking into parallel operations. This instinct is **wrong for `parallel_map`**.

`parallel_map` distributes items — each task receives one item, a different allocation. Moving `items[0]` to task 0 and `items[1]` to task 1 is safe even if T is `shared struct`, because no two tasks access the same allocation. The constraint needed is just "T can be sent to exactly one task." In Kāra, every type satisfies this: owned values can move, `concurrent struct` is Arc, `shared struct` Rc pointers can be moved into a single task. No bound is needed for distribute-style generic parallel algorithms.

The case that actually needs a bound:

```kara
fn broadcast[T: ???](value: T, n_tasks: i64) {
    // SAME allocation, N tasks
    par { for _ in 0..n_tasks { process(value.clone()) } }
}
```

Here `value.clone()` gives each task a handle to the same underlying data. For `concurrent struct` this is safe (Arc + Mutex/Atomic). For `shared struct` it is a data race (Rc clones to the same allocation, multiple tasks writing). This is the pattern that genuinely needs a bound.

**How common is this in Kāra's concurrency model?**

Not common. In Kāra's design, state shared across multiple tasks is supposed to be explicit (`concurrent struct`). A generic `broadcast[T]` is either:
1. Broadcasting a `concurrent struct` — already explicit, no generic needed; just take the concrete type
2. Broadcasting read-only immutable data — a narrower constraint (no `mut` fields); different bound
3. A pattern that shouldn't be generic — "what type can I freely share across N tasks" is answered at the definition site by `concurrent struct`

The auto-trait system (Rust's `Send`/`Sync`) exists because Rust's `Arc<T>` is a generic wrapper — any `T` can be placed inside an `Arc`, so the auto-traits guard the generic escape hatch. In Kāra, `concurrent struct` is a definition-site keyword, not a generic wrapper. The equivalent of "putting any T inside an Arc" doesn't exist as a user-written operation; the compiler controls the Rc/Arc decision based on the type keyword. So the generic escape hatch that `Send`/`Sync` guards against doesn't arise in the same way.

**The right answer for v1:** Do not design the auto-trait now. State three concrete rules in the spec:

1. `concurrent struct` values cross parallel region boundaries freely — always Arc, fields already constrained at definition
2. Owned plain-struct values can be moved into exactly one parallel task — distribute pattern, no bound needed
3. `shared struct` values cannot cross parallel region boundaries — compile error, with a diagnostic pointing toward `concurrent struct`

These three rules cover the overwhelming majority of real parallel code without any generic bound machinery. For the rare `broadcast[T]` pattern in library code, the answer is: don't make it generic over T — take `concurrent struct` explicitly. The API is honest; the broadcaster is designed for concurrent use, and `concurrent struct` says so at the type level.

**If the language later needs the generic bound**, the anchor is already clean: `concurrent struct` at the definition site is exactly where a structural auto-trait derivation would start. A `Concurrent` auto-trait derived from `concurrent struct` (plus structural propagation through plain structs whose fields are all `Concurrent`) is a well-defined future extension, simpler than Rust's `Send`/`Sync` because the intent is declared at the keyword level rather than inferred entirely from field types.

**Finding:** Case 4 is not a fatal gap in `concurrent struct`. It is a deferred design question about generic code over concurrent-safe types, and the use cases that motivate it are narrower than the Rust analogy suggests. The three-rule concrete spec handles the common case. The deferred auto-trait has a clean anchor point when the need materializes.

---

## Case 5: `Mutex[shared struct]` Inside `concurrent struct`

```kara
shared struct AppState { mut user_count: i64 }

concurrent struct Server {
    state: Mutex[AppState],
}
```

`Mutex[T]` is valid for any `T`, including `shared struct T`. The Mutex serializes access; inside a `lock` block, only one task operates on `AppState`. The `AppState` borrow flags fire within the locked section, but they are redundant — the Mutex already serializes access so same-task aliasing on `AppState` is impossible while the lock is held.

This is valid and correct. The borrow flags are harmless overhead. In practice, idiomatic code would define `AppState` as a plain `struct` (not `shared struct`) when it lives inside a `Mutex` in a `concurrent struct`, because the RC borrow-flag machinery serves no purpose there. The compiler could emit a lint: *"`AppState` is `shared struct` but is only accessed through `Mutex[AppState]` — consider using a plain `struct` to avoid borrow-flag overhead."*

**Finding:** Valid, minor overhead, idiomatic code naturally avoids it. Optional lint covers the case. Not a design gap.

---

## Case 6: `mut self` Receiver on `concurrent struct`

```kara
concurrent struct Counter { count: Atomic[i64] }

impl Counter {
    fn increment(ref self) { self.count.fetch_add(1, Relaxed) }  // fine
    fn reset(mut self) { ... }    // mut self = exclusive ownership — contradiction
}
```

`mut self` on a method means "the caller transfers exclusive ownership to this call." `concurrent struct` is always Arc — there may be multiple handles. `mut self` on a `concurrent struct` method would require Arc strong count == 1 at the call site (equivalent to `Arc::try_unwrap`), which is runtime-conditional and rarely the programmer's intent.

`shared struct` today already restricts `self` to shared reference semantics (always an RC increment, never consumed). `concurrent struct` should carry the same restriction: **`concurrent struct` methods accept only `ref self` receivers.** Exclusive mutation goes through `lock` blocks on `Mutex[T]` fields, not through ownership transfer.

**Finding:** Clean rule to add to the spec: `concurrent struct` methods may not declare `mut self` receivers — the compiler rejects them with *"concurrent struct values are always Arc-shared; use `lock` blocks on `Mutex[T]` fields for exclusive mutation instead of `mut self`."*

---

## Case 7: Effect System Interaction

A function that takes a `concurrent struct Counter` and calls `count.fetch_add(1, Relaxed)`:
- `Atomic[T]` operations are effect-free (`docs/design.md:307–340`)
- No resource effect is generated; the function's effect row is unchanged
- Correct — atomics handle their own synchronization; the effect system does not need to know

A function that takes a `concurrent struct Server` and uses a `lock` block:
- `lock` is effect-transparent — effects inside flow through to the enclosing function's row
- If the body reads a database, the function gets `reads(DB)`; the lock's existence is invisible to the effect system
- Correct — the two layers (effects = resource I/O scheduling; type system = memory concurrency safety) remain independent

**Finding:** `concurrent struct` interacts with the effect system cleanly. No new effects, no effect-row growth, no lock-masking questions, no reversal of `docs/design.md:5439`. The two-layer architecture holds without modification.

---

## Summary Table

| Case | Finding | Severity |
|---|---|---|
| Single-owner move into one task | Error condition in v4 is underspecified; needs "accessible from multiple concurrent paths," not just "overlaps parallel region" — same analysis as current promotion pass | Medium — implementation detail, not a design gap |
| "Sometimes concurrent" types | Forced to `concurrent struct` everywhere; Mutex/Atomic overhead on single-task hot paths; mitigated by inner-data pattern | Medium — real tradeoff, must be documented |
| `dyn Trait` mixed impls | Correctly rejected; traits commit at definition; exact precedent in existing dyn Trait effect-bound design | Low — expected, no new machinery |
| Generic bounds | Real only for broadcast-style (same allocation, N tasks), not distribute-style; defer auto-trait; three concrete rules cover common case | Low — deferred, not blocking |
| `Mutex[shared struct]` inside `concurrent struct` | Valid, minor unused borrow-flag overhead; optional lint; idiomatic code avoids it | Low |
| `mut self` receiver | Should be a compile error on `concurrent struct` methods; clean rule: `ref self` only | Low — simple rule addition |
| Effects | Clean. No new effects, two-layer architecture holds | None |

---

## Net Assessment

The `concurrent struct` design is **implementable and correct for the concrete-type case**, which is the dominant use case. No case in the stress test reveals a fatal gap.

**The two non-trivial findings:**

**Case 1** (single-owner move) is a spec precision issue, not a design flaw. The compile error condition needs to be phrased in terms of the reachability analysis the arc-promotion pass already computes, not in terms of live-range overlap alone.

**Case 2** (sometimes-concurrent types) is a genuine tradeoff between the two-type design and the D-lite patch path. The D-lite path lets programmers pay Mutex overhead only at concurrent sites; `concurrent struct` forces it everywhere. The inner-data pattern mitigates this but requires design discipline. The spec should state the tradeoff honestly rather than presenting `concurrent struct` as strictly superior to D-lite on all dimensions.

**Case 4** (generic bounds) is smaller than the Rust analogy suggests. Distribute-style parallel algorithms (the common case) need no bound. Broadcast-style algorithms (rare in Kāra's concurrency model) can take `concurrent struct` explicitly rather than a generic T. The deferred auto-trait has a clean anchor when the need materializes.

**Recommendation:** `concurrent struct` is the right design direction. The stress test does not surface any case that D-lite handles correctly and `concurrent struct` does not. The inverse is not true — `concurrent struct` cleanly closes the `dyn Trait` gap, the opaque-library-call gap, and the false-guarantee in lines 5281/5423, without any attribute family or phased plan. The spec should be updated with:

1. `concurrent struct` keyword definition and field constraints (`Atomic[T]` or `Mutex[T]` only for `mut` fields)
2. Precise error condition for `shared struct` in parallel regions (reachability, not just overlap)
3. `ref self`-only receiver rule for `concurrent struct` methods
4. Documented tradeoff for sometimes-concurrent types (Case 2)
5. Deferred item for `Concurrent` auto-trait

---

## Links

- Source brainstorming: `v51.md`
- Prior analyses: `v51_analysis_v1.md` through `v51_analysis_v4.md`
- Arc-promotion two-phase algorithm: `docs/design.md:5262–5280`
- Lines 5281 and 5423 (corrected in this session): `docs/design.md`
- `dyn Trait` effect-bound pattern (precedent for Case 3): `docs/design.md:4258–4275`
- `Atomic[T]` effect-free rationale: `docs/design.md:307–340`
- `lock` effect-transparency: `docs/design.md:5427–5434`
- RC fallback Rc→Arc algorithm: `docs/design.md:5258–5274`
- `shared struct` receiver rule (ref self only): `docs/design.md:5435`
