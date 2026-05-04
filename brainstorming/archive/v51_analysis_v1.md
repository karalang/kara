# v51 Analysis v1 — Q1 Stress Test: Is Option D a Good Fit?

**Status:** COMPLETE — stress test of the "revised lean: D alone" recommendation from `v51.md`.
**Date:** 2026-04-24
**Scope:** Q1 only (scope of fix). Q2–Q6 not re-examined here.

---

## What Was Tested

The brainstorming doc's revised lean is **D alone**: use the arc-promotion pass to emit a compile error when a `mut` field of an Arc-promoted `shared struct` is written inside a parallel region without a `lock` block. The claim is that D "delivers the same static guarantee without touching the effect vocabulary."

The stress test works through five categories of cases to find where D holds and where it breaks.

---

## Cases D Handles Correctly

**Direct writes in the parallel region body** — the intended core case. D's syntactic check fires correctly.

```kara
shared struct Counter { mut val: i64 }

par {
    counter.val += 1   // ERROR under D: unlocked mut write in parallel region
    counter.val += 1
}
```

**Inline closures passed to `par {}`/`collect_all_vec`** — the closure body is visible at the same site as the Arc-promotion decision, so D can look inside and check.

**Same-module function calls with visible bodies** — intra-project interprocedural checks over visible source are tractable. The Arc-promotion pass already does live-range overlap analysis over the same scope.

**Generic functions** — at monomorphization time, when `T = ConcreteSharedType`, the compiler can check the monomorphized body. Same timing window as Arc promotion.

---

## Cases D Fails or Strains

### Case 1: Opaque cross-module / library function calls

```kara
// lib.kara (external — body not visible)
pub fn process(node: SharedNode) { node.val += 1 }  // no lock, no effect required

// user.kara
par {
    process(node_clone_1)   // Arc-promoted
    process(node_clone_2)   // same underlying allocation — data race
}
```

D cannot see through `process`. The only way to rescue this case without seeing the body is a conservative type rule: "require `Mutex` wrapping for any `shared struct` with `mut` fields passed to any opaque function in a parallel region." But that rule rejects safe read-only accesses:

```kara
pub fn read_name(node: SharedNode) -> String { node.name }  // perfectly safe
par { read_name(n1); read_name(n2); }  // conservative rule wrongly rejects this
```

There is no way to distinguish `process` (unsafe) from `read_name` (safe) at the call site without a declaration from the library function. D is blind here.

### Case 2: `dyn Trait` method calls

```kara
trait Updater { fn update(ref self, node: SharedNode) }

fn update_all(nodes: Vec[SharedNode], updater: dyn Updater) {
    par {
        for n in nodes { updater.update(n) }
    }
}
```

Dynamic dispatch eliminates the monomorphization pass. D has no path to check any concrete impl — it cannot know whether `updater.update` writes unlocked `mut` fields. Every concrete impl, present and future, is opaque. D either blindly accepts or blindly rejects all `dyn Trait` calls in parallel regions with `shared struct` arguments, and neither is correct.

The design already solves the analogous problem for effects via trait-level effect bounds (line 4258 — `dyn Trait` requires an effect contract at the trait definition). D has no equivalent mechanism.

### Case 3: `mut` closure fields on `shared struct`

```kara
shared struct Processor {
    mut handler: Fn(i64) -> i64,
}
```

Two parallel tasks both invoke `p.handler(x)` after Arc-promotion. The write side (replacing the handler) is covered by D — `handler` is a `mut` field and D fires on writes. But invocation — calling through the field — may write captured state inside the closure body. D does not reach into closure-captured state unless the closure is statically resolved. A narrow but real gap for `mut` closure fields that store effectful closures.

### Case 4: Nested `shared struct` chains

```kara
shared struct Inner { mut val: i64 }
shared struct Outer { mut inner: Inner }

fn writes_nested(outer: Outer) {
    outer.inner.val = 42  // write to Inner.val, reachable from Arc-promoted Outer
}
```

The Arc-promoted value is `Outer`, but the write is to `Inner.val` reached through the `outer.inner` field chain. D needs to follow field-access chains to determine whether a write target is reachable from an Arc-promoted `shared struct` — not just "is this value Arc-promoted." Implementable, but the check cannot be a flat membership test; it requires a reach-through-field analysis.

---

## The Critical Finding: Design Inconsistency

The most important result from stress testing is that **two lines of the design doc make claims that cannot both be true without additional mechanism:**

**Line 5281** (current claim):
> "the compiler rejects concurrent mutation without `Mutex` — two tasks never race on the same borrow flag"

**Lines 5439–5440** (current rule):
> "within the project, mutating `mut` fields does not require an effect annotation"
> "from outside the project … mutation must go through a `pub fn` method that declares `writes(...)` effects"

The gap: a `pub fn process(node: SharedNode)` that writes `node.val` internally carries no obligation to declare any effect (line 5439 says internal mutation is annotation-free, and line 5440 only governs `pub mut` field access, not method implementations). At the call site, D has no declaration to read. The claim at 5281 is therefore aspirational under D-lite.

Closing the gap at public function boundaries requires one of:
- **C1** — `writes(Shared[T])` in the effect vocabulary, declared at public function boundaries
- **A new non-effect boundary marker** — same signaling role as C1 but outside the effect vocabulary (still requires user-visible syntax at public function boundaries)
- **The conservative type rule** — require `Mutex` wrapping for all opaque `shared struct` arguments in parallel regions — produces false positives on read-only accesses (Case 1 above)

None of these is "D alone."

---

## Net Assessment

### What D-lite delivers (ship-worthy)

D for direct writes in the parallel region body — including inlined closures and same-module visible function calls — is correct, implementable against the arc-promotion pass, and addresses the most common case that currently hits the runtime borrow-flag panic. This is real and should land.

### Where "D alone" overstates the guarantee

The brainstorming doc states D "delivers the same static guarantee" as C1. This is true only for the intra-function scope. The stress test shows three categories that remain uncovered:

| Gap | Root cause | Severity |
|---|---|---|
| Opaque library function calls | No declaration at public boundary | High — any library function can be the hazard |
| `dyn Trait` method calls | No monomorphization, no trait-level contract | High — dynamic dispatch is a first-class pattern |
| `mut` closure fields with effectful closures | Closure body not reachable via field | Low — narrow edge case |

Nested `shared struct` chains are not a gap in D's logic — they require a field-reach analysis rather than a flat check, but the information is available.

### `dyn Trait` is the hardest blocker

Unlike opaque function calls (where in-project visibility and monomorphization partially rescue D), dynamic dispatch has no monomorphization pass. D has no path to cover `dyn Trait` without a trait-level declaration mechanism — which is exactly what the effect system already provides for every other effect. Ignoring this case means any interface that uses `dyn Trait` with `shared struct` parameters is D's blind spot.

### The interprocedural gap converges with C1

For D to close the public-boundary gap, it needs some form of declaration at public function signatures. Whether that declaration uses effect vocabulary (C1) or a new syntax, it is functionally equivalent at the API level. The brainstorming doc's framing — "D preserves the effect vocabulary while C1 changes it" — is correct, but it obscures that D + full coverage requires *some* declaration mechanism at public boundaries that does not currently exist.

---

## Revised Recommendation for Q1

**D-lite + honest framing + phased plan**, not "D alone":

1. **Land D-lite first.** Direct writes in parallel region body (including inlined closures, same-module interprocedural where bodies are visible). This is mechanical, correct, and satisfies the common case. Revise line 5281 to accurately scope the guarantee: *"the compiler rejects concurrent mutation without `Mutex` for direct writes and same-project function calls in parallel regions."*

2. **`dyn Trait` coverage requires trait-level declarations regardless of C1 vs. D.** This is not optional if the design wants to cover dynamic dispatch. The mechanism already exists in the effect system (line 4258 establishes the pattern for trait-level effect bounds). Whether it's called `writes(Shared[T])` or a new non-effect marker is a vocabulary decision, not an architecture decision.

3. **Public-boundary coverage requires a declaration.** The minimum viable form: any `pub fn` that takes a `shared struct` with `mut` fields as a parameter and writes those fields without a lock must declare something at its boundary. C1 is one shape; a new `#[writes_shared]` attribute is another. Neither is "D alone."

4. **The two-layer architecture (effects = resource I/O scheduling; type system = memory concurrency safety) is still correct.** D-lite preserves it. The declarations needed for Cases 1 and 2 should be in the type/attribute layer, not the effect layer — keeping with the brainstorming doc's instinct to avoid C1's effect-vocabulary expansion.

5. **Line 5281 should be revised now**, even before implementation, to not claim a guarantee the current design does not fully deliver. The revision should name D-lite's scope and flag the phased plan for full coverage.

---

## Links

- Source brainstorming: `v51.md`
- Design lines referenced: 5281, 5439–5440, 4258, 5275, 5272–5274
- Conflict matrix: `docs/design.md:3534–3545`
- `dyn Trait` effect bounds pattern: `docs/design.md:4258`
- Arc-promotion conservative default: `docs/design.md:5275`
