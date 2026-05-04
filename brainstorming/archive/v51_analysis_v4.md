# v51 Analysis v4 — Structural Diagnosis: `shared struct` Is Doing Two Jobs

**Status:** COMPLETE
**Date:** 2026-04-25
**Scope:** Root-cause analysis of why D-lite + phased attribute plan are patches rather than proper language design, and what a structurally sound alternative looks like.

---

## What the Prior Analyses Actually Revealed

v1 found that D-lite overstates its guarantee and leaves three real gaps (opaque cross-module calls, `dyn Trait` dispatch, returned closures). v2 refined D-lite's scope, proposed a two-attribute family (`#[writes_shared_locked]` / `#[writes_shared_unlocked]`), and corrected the false borrow-flag retirement claim.

Both analyses produced the right engineering — but the accumulation of patches (D-lite + attribute family + Phase 2 boundary declarations + line 5281 revision) is itself a signal. Every new concurrent use pattern finds another edge in the same gap. The patches work, but they don't fix the model that created the gap.

---

## The Root Cause

`shared struct` is doing two structurally different jobs, and the language does not distinguish them at the definition site.

**Job 1 — Reference semantics within a single task.**
Multiple owners, one logical value, all mutations sequential. This is the RC use case. `mut` fields are correct here; runtime borrow flags handle aliasing. Tree nodes, graph nodes, UI component hierarchies, AST nodes — this is the dominant use of `shared struct` in practice.

**Job 2 — Shared mutable state across concurrent tasks.**
Arc-promoted, concurrent access from multiple tasks. `mut` fields here require explicit synchronization (`Mutex[T]` + `lock` block, or `Atomic[T]`). The mutation story is fundamentally different.

The language uses one keyword for both, then tries to patch the difference at the use site — at the parallel region boundary — with D-lite structural checks and attribute declarations. That is enforcement at the wrong layer. The type carries no information about which job the struct is doing, so the compiler cannot reason about it until it sees how the value is used.

---

## Why the Patch Stack Is the Wrong Shape

Each patch addresses a symptom of the missing type-level distinction:

| Patch | What it addresses | Why it's a symptom |
|---|---|---|
| D-lite compile error | Direct unlocked writes in parallel regions | Fires at the use site because the type doesn't encode the constraint |
| `#[writes_shared_locked(T)]` attribute | Opaque cross-module function calls | Required because the type signature carries no concurrent-safety information |
| `#[writes_shared_unlocked(T)]` attribute | `dyn Trait` dispatch | Required because trait bounds carry no concurrent-safety information |
| Phase 2 boundary declarations | Full coverage of interprocedural cases | Required because the type doesn't propagate the constraint structurally |
| Line 5281 revision | False guarantee in spec | Required because the design made a promise the type system cannot keep |

None of these patches would be needed if the type-level distinction existed. The stack grows because each new concurrent use pattern — opaque calls, dynamic dispatch, stored closures, returned closures — finds the same missing seam.

---

## What Proper Language Design Looks Like

The industry has solved this cleanly at the type level, in two different flavors:

**Rust** — `Rc<T>` vs `Arc<T>` are distinct types. `T: Send` and `T: Sync` auto-traits propagate structurally through the type graph. You cannot accidentally use an `Rc` across thread boundaries because the type system rejects it at the boundary — no runtime flags, no use-site checks, no attributes.

**Pony** — Reference capabilities (`iso`, `val`, `ref`, `box`, `tag`, `trn`) classify mutation and sharing rights on every value. The type of a value encodes exactly what can be done with it and where it can be sent. Concurrent mutation requires the capability that permits it; the wrong capability is a type error.

Both approaches encode the distinction **at the type level, propagated structurally**, not at use sites via ad-hoc checks.

---

## A Structurally Sound Alternative for Kāra

Draw the distinction at the definition site with two separate concepts:

```kara
// Job 1 — reference semantics, single-task only
// RC internally. mut fields permitted. Borrow flags handle aliasing.
// Cannot be Arc-promoted. Cannot cross a parallel region boundary.
shared struct TreeNode {
    mut val: i64,
    children: Vec[TreeNode],
}

// Job 2 — explicitly concurrent-safe shared state
// Arc internally. mut fields must be Atomic[T] or Mutex[T] — no bare mut.
// Can cross parallel region boundaries. No D-lite check needed.
concurrent struct Counter {
    count: Atomic[i64],
    state: Mutex[CounterState],
}
```

The keyword names are not fixed — `concurrent struct`, `sync struct`, `shared struct with ...` — that's a vocabulary decision. The structural requirement is: **the two roles are distinct types, and the compiler knows which is which at the definition site.**

### What this eliminates

- **D-lite** — not needed. `shared struct` (Job 1) cannot enter a parallel region; `concurrent struct` (Job 2) can, and its fields are already constrained by the type.
- **`#[writes_shared_locked]` / `#[writes_shared_unlocked]` attribute family** — not needed. A function taking a `concurrent struct` parameter carries that in its type signature. The caller knows.
- **The opaque library function gap** — closed. `pub fn process(node: ConcurrentNode)` signals at the type level that `ConcurrentNode` is a concurrent-safe type. The caller can reason about it without seeing the body.
- **The `dyn Trait` gap** — closed. Trait bounds carry the type distinction. `dyn Updater` where `Updater` requires a `concurrent struct` argument propagates the constraint through dynamic dispatch without a separate attribute mechanism.
- **Line 5281's false guarantee** — there is nothing to falsely guarantee. The type system enforces the constraint structurally; the spec can accurately describe what the type system does.
- **The Phase 2 phased plan** — not a phase, just a consequence of the type design.

### What it preserves

- Single-task `shared struct` (Job 1) is unchanged: `mut` fields, borrow flags, RC, no overhead.
- `concurrent struct` (Job 2) is the natural home for patterns that were previously `shared struct` + `Mutex` anyway. The naming makes the intent explicit.
- The effect system is untouched. No `writes(Shared[T])` resource family, no effect-row bloat, no C1. Effects remain resource I/O scheduling; concurrent-safety is the type system's job.
- `Mutex[T]` / `Atomic[T]` / `lock` block syntax unchanged — they're the mechanism inside `concurrent struct` fields, as today.

---

## The Arc-Promotion Pass Under This Design

The arc-promotion pass (`docs/design.md:5262–5283`) currently promotes `Rc` to `Arc` when a value's live range overlaps a parallel region. Under the two-type design, the pass changes role:

- `shared struct` values (Job 1): **cannot be promoted**. If a `shared struct` value's live range overlaps a parallel region, that is a compile error — not a promotion. The type doesn't support concurrent use.
- `concurrent struct` values (Job 2): **always `Arc`**. No promotion needed; they're `Arc` by definition. The pass simplifies to: verify `concurrent struct` fields satisfy the mutex/atomic constraint (which the definition site already enforced).

The result is a simpler pass with a cleaner contract, not a more complex one.

---

## Interaction with the Existing Design

### `shared struct` today

The design doc currently conflates both jobs under `shared struct`. Separating them is a breaking change to the surface syntax but not to the semantics — code using `shared struct` for single-task reference semantics stays `shared struct`; code using it for concurrent shared state migrates to `concurrent struct` (or the chosen keyword).

The migration is mechanical: any `shared struct` that currently requires `Mutex[T]` fields for concurrent access becomes a `concurrent struct`. Any `shared struct` used purely within a single task stays `shared struct`.

### RC fallback (Feature 4 Part 4)

The RC fallback pass (`docs/design.md:5258–5274`) promotes `Rc → Arc` when a value crosses a parallel region. Under the two-type design, this pass applies only to `concurrent struct` values (which start as `Arc`) and to non-shared values that happen to be RC (if any). The `shared struct` type is ineligible for promotion by definition.

### Effect system

No interaction. The two-type distinction is entirely in the type/ownership layer. Resource effects (`reads`, `writes`, `sends`, etc.) are unchanged. The scheduler sees the effect row; the type system sees the concurrent-safety constraint. They remain independent, matching the design's existing two-layer architecture.

### Kernel / embedded profiles

The kernel profile already forbids `allocates(Heap)`. `shared struct` (Job 1, RC) requires heap; `concurrent struct` (Job 2, Arc) also requires heap. Both are already excluded from the kernel profile. No change needed.

---

## The Scope Question

The patch path (D-lite + Phase 2 attributes) is implementable against the current design with bounded scope. The two-type design requires a surface syntax change and a reclassification of existing `shared struct` uses. The scope difference is real.

The decision framing:

**Ship the patch path as v1 debt.** D-lite + Phase 2 land. Line 5281 is corrected. The gaps for `dyn Trait` and opaque calls are documented and covered by Phase 2. The structural seam remains; future concurrent use patterns will find it again. The debt is known and bounded.

**Redesign now.** The two-type split is a smaller surface change than it appears — most `shared struct` uses are single-task and stay unchanged. Concurrent uses are already forced to use `Mutex[T]` by good practice; the rename is renaming an intent that was already there. The payoff is: no patch stack accumulates, the pitch language becomes honest by construction, and the compiler architecture is simpler (arc-promotion pass shrinks, D-lite never ships, attribute family never ships).

The argument for redesigning now: the patch stack is already three layers deep (D-lite, attributes, line 5281) before v1 ships. Each layer adds surface area that future contributors have to understand, maintain, and extend. The two-type design is less total surface area than the patch stack, and it is the surface area you'd want anyway.

---

## Links

- Source brainstorming: `v51.md`
- Prior stress tests: `v51_analysis_v1.md`, `v51_analysis_v2.md`
- Arc-promotion pass: `docs/design.md:5258–5283`
- Line 5281 false guarantee: `docs/design.md:5281`
- Borrow flag v1 mechanism: `docs/design.md:5423`
- `dyn Trait` effect contract pattern: `docs/design.md:4258–4275`
- Module-level parallel write rule: `docs/design.md:672`
- RC fallback Rc→Arc algorithm: `docs/design.md:5258–5274`
- Effect conflict matrix: `docs/design.md:3530–3556`
