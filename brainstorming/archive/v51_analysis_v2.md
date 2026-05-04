# v51 Analysis v2 — Stress Testing the D-lite + Phased Plan

**Status:** COMPLETE
**Date:** 2026-04-24
**Scope:** Four stress tests on the revised recommendation from `v51_analysis_v1.md`: (1) D-lite boundary conditions, (2) the boundary declaration mechanism for cross-module / `dyn Trait` coverage, (3) the `Atomic[T]` field carve-out, (4) borrow-flag retirement claims.

---

## Quick recap of what v1 recommended

- **D-lite:** hard compile error when a `mut` field of an Arc-promoted `shared struct` is written inside a parallel region without a `lock` block, for cases where the write is syntactically visible (inline body, same-module function call, inlined closure).
- **Phased plan:** D-lite ships first; cross-module and `dyn Trait` coverage requires a boundary declaration mechanism (not yet specified); `#[writes_shared]` attribute named as one shape.
- **Design doc line 5281 overstates the guarantee** — needs to be scoped down.

---

## Stress Test 1: D-lite Boundary Conditions

**Question:** What exactly counts as "inside a parallel region, visible body"? Where does D-lite fire and where does it silently pass?

### Case 1a: Direct `TaskGroup.spawn` closures

```kara
let group = TaskGroup.new()
loop {
    let conn = listener.accept()?
    group.spawn(|| {
        conn_state.counter += 1  // conn_state is a shared struct, Arc-promoted
    })
}
```

The closure body is inline at the call site. The `spawn` call site is outside the `par {}` / auto-concurrency region in the CFG sense — it is in a loop. However, the spawned task runs concurrently with subsequent iterations. Does D-lite fire here?

**Finding:** `TaskGroup.spawn` is structurally a parallel region per the two-phase algorithm. Line 5272 defines a parallel region as "any span of code where concurrent execution occurs — an auto-concurrency region the compiler chose to parallelize, an explicit `par {}` block, or a scope containing `spawn()` / `TaskGroup` tasks." So `group.spawn` closures ARE inside a parallel region (the `TaskGroup`'s scope). D-lite fires correctly.

**But:** the Arc-promotion pass (Phase 2) fires on the `conn_state` value's live range. If `conn_state` is captured by the closure and the closure outlives the loop iteration, the live range overlaps the `TaskGroup` parallel region → Arc promotion → D-lite check. This is structurally correct. No gap here.

### Case 1b: `#[inline]` functions

```kara
#[inline(always)]
fn increment(s: SharedCounter) { s.val += 1 }

par {
    increment(c1)
    increment(c2)
}
```

`#[inline(always)]` causes the function body to be inlined at the call site before the parallel region analysis runs. Post-inlining, the write is direct. D-lite fires correctly.

What about `#[inline(never)]`? Now the body is opaque at the call site — same as any private function without a visible body for the parallel region analysis. D-lite cannot fire. But `increment` is a private function; same-project visible bodies are tractable. The question is whether D-lite's "same-module visible function calls" extends to same-project private functions regardless of `#[inline]`.

**Finding:** `#[inline]` is a codegen hint, not a semantic boundary. D-lite's interprocedural reach should be defined in terms of *project visibility* (same-crate private function), not inlining. `#[inline(never)]` on a private function does not hide the body from the compiler's analysis pass. This is consistent with how effect inference already reaches into private function bodies.

### Case 1c: Closures stored in `let` bindings before the parallel region

```kara
let worker = || { node.val += 1 }  // node is shared struct
par {
    worker()
    worker()
}
```

`worker` is a closure defined outside the `par {}` block. The closure body is visible. Does D-lite look inside closures that are not written inline at the `par` site?

**Finding:** This is a real boundary condition. The Arc-promotion pass fires on `node` because its live range overlaps the `par {}` region. D-lite's "look inside the closure body" requirement extends to stored closures whose call site is inside the parallel region. This is not "inline closure at the par site" — it requires D-lite to chase closure call sites, which is one step beyond direct inline analysis.

This is implementable — the closure's body is syntactically available at the definition site and its type is known — but it requires D-lite to track that `worker` is a closure-type variable and resolve its call to the original definition. The flat "syntactically inside the parallel region body" framing in the recommendation undersells this.

**Revised scope for D-lite:** Direct writes and closure-type calls (stored or inline) where the closure definition is visible in the same compilation unit. Not just "inline at the par site."

### Case 1d: Closures returned from functions

```kara
fn make_worker(node: SharedNode) -> Fn() {
    || { node.val += 1 }
}

let w = make_worker(node)
par { w(); w() }
```

The closure is returned from a function. D-lite sees a call to `w` in the parallel region. `w` has type `Fn()`. The body is not visible at the call site — it came from `make_worker`. This is the same as Case 2 (opaque library function) except the opacity comes from a function return, not a library boundary.

**Finding:** D-lite cannot cover this case without interprocedural analysis that traces returned closures back to their definition sites. This is a real gap. Private function? Still opaque if the closure is returned through a function call. Severity: medium — returned closures are a common pattern.

---

## Stress Test 2: The Boundary Declaration Mechanism

**Question:** What does the declaration mechanism for cross-module / `dyn Trait` coverage look like, and does it fit the existing design without requiring C1?

### Background: the design already has the exact pattern needed

`docs/design.md:4258–4288` establishes that `dyn Trait` requires a trait-level effect contract. The pattern:

```kara
trait Storage with reads(Data) writes(Data) {
    fn load(ref self, key: Key) -> Option[Value] with reads(Data);
    fn save(ref self, key: Key, value: Value) with writes(Data);
}
```

Trait-level bound = ceiling. Per-method = actual. Impls must stay within the per-method declaration. This is exactly the mechanism needed for D's cross-module and `dyn Trait` coverage — if we add a "writes shared struct fields" marker with the same two-level structure, the design precedent is already there.

### The attribute shape

Proposed: a `#[writes_shared(T)]` attribute (or `#[mutates_shared(T)]`) on `pub fn` signatures and trait method declarations.

```kara
#[writes_shared(SharedNode)]
pub fn process(node: SharedNode) { node.val += 1 }
```

At D's check site: if `process` is called in a parallel region with an Arc-promoted `SharedNode` argument, and `process` declares `#[writes_shared(SharedNode)]`, D requires the call to be inside a `lock` block.

Wait — that can't be right. `lock` blocks in Kāra are on `Mutex[T]` values. If `process(node)` takes a `SharedNode` (not `Mutex[SharedNode]`), the caller cannot wrap the call in a `lock` block. The enforcement has to be: **`#[writes_shared(T)]` on a public function requires the argument to be a `Mutex[T]` at the call site.**

Revised rule: if `pub fn process(node: Mutex[SharedNode])` declares `#[writes_shared(SharedNode)]`, D accepts calls to `process` inside a parallel region. If the function takes `SharedNode` (not mutex-wrapped), the only correct design is to require the mutex at the function boundary, not at the call site.

**But this is a stronger requirement.** It forces library authors to take `Mutex[SharedNode]` instead of `SharedNode`. That's an API contract change. It also means you can't call a library function that internally uses a `lock` block unless the library exposes the mutex-taking interface.

**Alternative:** the attribute on a method signals "this method only writes via `lock` internally" — meaning the caller does NOT need a `lock` block at the call site, because the callee handles it. In this reading, `#[writes_shared_locked(T)]` means "writes T fields but always under a lock." D at the call site then treats this as safe, like calling any other properly-locked function.

```kara
#[writes_shared_locked(SharedNode)]
pub fn process(node: SharedNode) {
    lock node_mutex { node.val += 1 }  // internal lock, safe
}
```

D sees: call to `process` in parallel region. `process` declares `#[writes_shared_locked(SharedNode)]` → internal locking guaranteed → no error.

Contrast:

```kara
// No attribute
pub fn process(node: SharedNode) { node.val += 1 }
```

D sees: call to `process` in parallel region. No `#[writes_shared_locked]` declaration. The write could be unlocked → **error: call to opaque function that may write shared fields without lock in parallel region**.

**Finding:** Two distinct marker concepts emerge:
- `#[writes_shared_unlocked(T)]` — writes fields without a lock (requires caller to handle synchronization, e.g., by providing `Mutex[T]` or ensuring no parallelism)
- `#[writes_shared_locked(T)]` — writes fields but under a lock internally (safe to call from parallel regions on bare `SharedNode`)

The two-level distinction maps cleanly to the trait-level / per-method pattern in the design. A trait method declared `with reads(Data)` and `#[writes_shared_locked(SharedNode)]` signals: "you can call this through `dyn Trait` in a parallel region and the locking is the impl's responsibility."

### The `dyn Trait` coverage

```kara
trait NodeUpdater {
    #[writes_shared_locked(SharedNode)]
    fn update(ref self, node: SharedNode);
}
```

Impls that write `node.val` without a lock violate the declaration → compile error at the impl site. D at the call site to `updater.update(node)` in a parallel region sees `#[writes_shared_locked(SharedNode)]` → treats as safe. Full coverage.

This is mechanically identical to how trait-level effect bounds work. The only new machinery is: (1) a new attribute family, (2) a check at impl sites that write-without-lock impls carry the unlocked marker, not the locked one, and (3) D at call sites reads the marker.

**Is this simpler than C1?** Yes, in one important dimension: it doesn't enter the effect row. The `conflict matrix` at `docs/design.md:3534–3545` is unchanged. The scheduler doesn't see `writes_shared_locked` — the attribute is purely for D's structural check. No effect-row bloat. The viral propagation IS still present at public function boundaries (a `pub fn bar` that calls `process` in its body without a lock must also declare `#[writes_shared_unlocked(SharedNode)]`), but the propagation is narrower: it only matters when the function is called from a parallel region context, and the attribute has no scheduling implications.

### Gap: what about functions that are sometimes locked and sometimes not?

```kara
pub fn conditional(node: SharedNode, use_lock: bool) {
    if use_lock { lock mutex { node.val += 1 } }
    else { node.val += 1 }
}
```

The function is safe when called with `use_lock = true` and unsafe when called with `use_lock = false`. Neither `locked` nor `unlocked` is accurate. D has no way to handle this. The rule has to be: **if any code path through the function writes a `mut` field without a lock, the function must declare `#[writes_shared_unlocked(T)]`.** Conditional locking is the programmer's responsibility.

This is strict but correct. A function with a conditional locking bug fails at the attribute declaration site rather than at the call site.

---

## Stress Test 3: `Atomic[T]` Field Carve-out

**Question:** Should writes to `Atomic[T]` fields require a `lock` block under D? What exemption rule is needed?

### Current design

`docs/design.md:7100`: "`Atomic[T]` with `load`/`store` and the `Ordering` enum ships in v1 to close the ISR-to-main signaling gap for embedded targets." `Atomic[T]` fields are safe for concurrent access without `Mutex`.

`docs/design.md:5350`: "the compiler never silently promotes owned data to a shared tier... `Atomic[T]` for lock-free signaling."

Module-level `let mut` bindings already have an explicit exemption (line 672): concurrent writes are a compile error "unless the binding's type is an explicit concurrency primitive — `Atomic[T]`, `Mutex[T]`, `RwLock[T]`."

### What D's rule should say

The rule for module-level bindings at line 672 is the exact precedent. D's equivalent rule for `shared struct` fields:

> Writing to a `mut` field of an Arc-promoted `shared struct` inside a parallel region is a compile error, unless:
> (a) the write is syntactically inside a `lock` block on a `Mutex`-wrapped value, OR
> (b) the field's declared type is `Atomic[T]`.

This is a simple type-level check: is the field's static type `Atomic[T]`? If yes, D's check does not fire. No lock block required. The atomic operations (`load`, `store`, `fetch_add`) are safe by construction.

### Edge case: `Atomic[T]` field accessed via a method call

```kara
shared struct Counter { mut count: Atomic[i64] }

par {
    counter.count.fetch_add(1, Relaxed)  // method call on Atomic[i64] field
}
```

This is NOT a "write to a `mut` field" in the assignment sense — it's a method call on a value reached through a field. D's check as stated ("write to a `mut` field") would need to distinguish field assignment from method calls on field values.

If D checks "is this a field assignment expression `lhs.field = rhs`", then `counter.count.fetch_add(...)` is a method call and doesn't trigger D. That's correct — the method call is on an `Atomic[T]` and is safe. But it requires D to not conflate "write to a field" with "method call on a field value."

**Revised enforcement scope:** D's "write to a `mut` field" check applies only to:
1. Direct field assignment: `shared_val.field = expr`
2. Compound assignment: `shared_val.field += expr`, `shared_val.field -= expr`, etc.
3. Mutable borrow of a field for passing to a `mut ref` parameter: `f(mut shared_val.field)`

Method calls on field values (`shared_val.field.method(...)`) are NOT field writes and do not trigger D. If `field` is `Atomic[T]`, the `store`/`fetch_add` methods are on the atomic type itself — they are safe, and D ignores them.

The `Atomic[T]` exemption in rule (b) above is actually redundant with this scope definition — if D only fires on direct assignment expressions, then `atomic_field.store(...)` is out of scope regardless. But explicit exemption is better documentation.

### What about `mut` fields that are NOT `Atomic[T]` and NOT `Mutex[T]`?

A `mut val: i64` field. In a parallel region without a `lock` block: D fires. The programmer either:
- Wraps in `Mutex[i64]` and uses a `lock` block, or
- Changes the field type to `Atomic[i64]`, or
- Restructures to avoid shared mutation

This is the correct behavioral constraint. Non-atomic, non-locked `mut` field writes in parallel regions are the exact target of D's guarantee.

---

## Stress Test 4: Borrow Flag Retirement

**Question:** The v51 doc says "once D is in place, the runtime borrow-flag panic path is closed." Is this accurate?

### What borrow flags are for

`docs/design.md:5423`: "because `shared struct` values may have multiple RC holders within a single task, `mut` field access uses per-field runtime borrow tracking inserted automatically by the compiler. Reads are shared (multiple simultaneous readers allowed). A write is exclusive — if any other borrow of the same field is active when a write begins, the runtime panics."

This is designed for the **same-task aliasing case**: two code paths in the same task hold live borrows on the same `mut` field. Example:

```kara
let r = ref node.val    // read borrow active
node.val = 42           // write borrow — runtime panic: read borrow still active
```

This has nothing to do with parallel regions or Arc. It's a single-task intra-scope aliasing check.

### D-lite's scope

D-lite fires on **parallel region** `mut` field writes without `lock` blocks. It does not fire on same-task aliasing. Therefore:

- D-lite closes the "Arc-promoted shared struct, parallel region, unlocked write → runtime panic" path.
- D-lite does NOT close the "single-task, two live borrows on same mut field → runtime panic" path.

**The v51 claim "the runtime borrow-flag panic path is closed" is wrong.** D-lite closes *one* panic path. The borrow flags remain necessary for single-task aliasing detection.

### Can we eliminate borrow flags under D-lite?

No. Here's a case that survives D-lite and still needs borrow flags:

```kara
shared struct Node { mut val: i64 }
let node = Node { val: 0 }
let r = ref node.val            // read borrow
node.val = 42                   // write on same-task, non-parallel → D-lite doesn't fire
// borrow flag: "read borrow is still active" → runtime panic
```

This is a valid single-task bug. D-lite is orthogonal to this. Borrow flags remain the mechanism.

### Is there a path to eliminating borrow flags statically?

The design doc (`docs/design.md:5423`) says: "This runtime tracking is a v1 mechanism — Phase 7-8 will revisit whether the effect system can statically prove exclusive access for some fields, eliminating the runtime flags where the compiler can verify safety at compile time."

The deferred work is effect-system-based static proof of exclusive access, not D. D doesn't contribute to this path.

**Conclusion:** The v51 claim about borrow flag retirement should be rewritten: "D-lite closes the parallel-region path to the borrow-flag panic. Same-task aliasing panics remain runtime-detected under v1; static elimination is deferred to Phase 7-8."

---

## Stress Test 5: The Lock Semantics Consistency Check

The v51 doc's Q3 asks whether `lock` should mask effects under C1. Under D-lite, Q3 is moot for effect masking — D doesn't touch the effect row. But there is a related consistency question D-lite introduces.

**The `lock` block is already the escape hatch for the module-level binding rule (line 672).** The design says concurrent writes to module-level `let mut` bindings are a compile error "unless the binding's type is an explicit concurrency primitive — `Atomic[T]`, `Mutex[T]`, `RwLock[T]`." The escape hatch is not a `lock` block — it's the type.

**For `shared struct` fields, D-lite's escape hatch is a `lock` block.** This is consistent with the `shared struct` mutex pattern (`docs/design.md:5429–5432`): the only way to write `node.val` is via `lock node { node.val = 42 }`, which requires `node` to be `Mutex[TreeNode]`. So D-lite's syntactic "write inside a lock block" check implies `Mutex[T]` wrapping, because a `lock` block on a non-`Mutex` value is a compile error.

The two mechanisms (module-level type-based check vs shared-field lock-block check) are structurally consistent — both require a concurrency primitive at the mutation site. They differ in how they're expressed:
- Module-level: type-of-binding constraint
- `shared struct` field: syntactic lock-block constraint (which implies mutex wrapping)

This is not a contradiction, but it's a design asymmetry worth noting. A uniform rule would be: "concurrent writes require a concurrency primitive at the write site." The form the "at the write site" constraint takes differs by context.

**No conflict with effect-transparent `lock`.** D-lite's check is structural (is this write inside a `lock` block?), not effect-based. The `lock` block staying effect-transparent (resource effects flow through unchanged) is unrelated to D-lite using the syntactic presence of `lock` as a structural marker. These are orthogonal properties of the `lock` block.

---

## Summary of Findings

| Stress test | Finding | Impact on recommendation |
|---|---|---|
| D-lite case 1a (TaskGroup closures) | D-lite fires correctly | No change |
| D-lite case 1b (`#[inline]`) | D-lite should use project visibility, not inlining | Clarify scope in spec |
| D-lite case 1c (stored closures) | Requires chasing closure-type variables | D-lite scope is wider than "inline at par site" |
| D-lite case 1d (returned closures) | Real gap: opaque at the call site | Confirm as out-of-scope for D-lite; phased plan covers |
| Boundary mechanism shape | Two attributes needed: `writes_shared_locked` vs `writes_shared_unlocked` | New naming decision; fits design's trait-level contract pattern exactly |
| `dyn Trait` coverage | Trait-level `#[writes_shared_*]` bound is the correct mechanism; design precedent already exists | High confidence; compatible with existing `dyn Trait` effect contract design |
| `Atomic[T]` carve-out | D fires on assignment expressions only; `Atomic[T]` method calls are out of scope by definition; explicit exemption is good docs | Simple; clarify "write to a mut field" means assignment, not method call |
| Borrow flag retirement | D-lite closes only the parallel-region panic path; same-task aliasing flags remain | Correct the v51 claim; flags are not retired by D-lite |
| `lock` semantic consistency | D-lite's lock-block check is structural, not effect-based; no conflict with effect-transparent `lock` | No design change needed; add a one-line note in spec |

---

## Revised Recommendation for Q1 (full picture)

**Phase 1 — D-lite (ship first):**
- Hard compile error for direct `mut` field writes (assignment expressions) on Arc-promoted `shared struct` values inside parallel regions, without a syntactic `lock` block ancestor.
- Coverage: inline bodies, inlined closures, stored closures (by chasing closure-type variable calls), same-project private function calls with visible bodies.
- Excludes: opaque cross-module calls, returned closures from opaque functions, `dyn Trait` dispatch.
- `Atomic[T]` fields: exempt by scope (method calls on field values are not assignment expressions; add explicit exemption for documentation clarity).
- Does NOT retire runtime borrow flags; closes only the parallel-region panic path.

**Phase 2 — Boundary declarations (phased):**
- Two-attribute family: `#[writes_shared_locked(T)]` (function handles its own locking internally — safe to call from parallel regions) and `#[writes_shared_unlocked(T)]` (function may write without internal locking — requires caller-level synchronization).
- Private functions: declarations inferred from the body analysis (same rule as effect inference for private fns).
- Public functions: declarations required at the boundary (same rule as effect declaration at public boundaries).
- Trait methods: two-level (trait-level ceiling + per-method declaration), identical pattern to trait-level effect bounds at `docs/design.md:4262–4275`. This closes `dyn Trait` coverage.
- Does NOT enter the effect row. Not a resource effect. No scheduling implications. Purely a structural marker for D's check.

**Line 5281 fix (immediate, no implementation needed):**
Revise from: "the compiler rejects concurrent mutation without `Mutex` — two tasks never race on the same borrow flag"
To: "for Phase 1, the compiler rejects direct unlocked concurrent mutation in parallel regions for visible-body writes; full coverage of opaque cross-module and `dyn Trait` calls requires Phase 2 boundary declarations."

---

## Links

- Source brainstorming: `v51.md`
- Prior stress test: `v51_analysis_v1.md`
- Design lines referenced: 672, 702, 4258–4275, 4183–4185, 5272–5274, 5281, 5350, 5423, 5427–5434, 5439–5440, 7100
- Module-level parallel write rule (precedent for D-lite): `docs/design.md:672`
- `dyn Trait` trait-level effect contract (precedent for Phase 2): `docs/design.md:4258–4275`
- Borrow flag v1 mechanism + Phase 7-8 deferred elimination: `docs/design.md:5423`
