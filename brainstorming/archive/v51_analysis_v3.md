# v51 Q1 — Independent Analysis (v3)

**Question:** What is the scope of the fix for the effect system / data-race gap? Options A (framing only), B (framing + guardrails), C1/C2/C3 (design extension), D (arc-promotion boundary check).

---

## How I read the design before forming a view

Before evaluating options, I looked at what the design actually *already commits to*, not just what it aspires toward. Two passages matter most:

**design.md:5281** — `Arc and synchronization`

> "Per-field runtime borrow tracking remains sufficient under `Arc` because **the compiler rejects concurrent mutation without `Mutex`** — two tasks never race on the same borrow flag."

**design.md:5423** — `Interior mutability for mut fields`

> "concurrent mutation from multiple tasks **requires** explicit `Mutex[T]` with `lock` blocks, so two tasks never race on the same borrow flag."

Both passages use strong language: "rejects," "requires." The design isn't framing this as aspirational — it's presenting it as a guarantee. But the mechanism behind that guarantee is the *runtime borrow flags*, not a compile-time check. This creates a gap between what the design claims and what v1 actually delivers.

That gap is the real Q1. The options aren't "how much do we want to promise?" — the design already promises the stronger thing. The options are "how do we close the gap between what the design says and what the compiler actually does?"

---

## Stressing each option against the design

### Option A — Framing only

A says: the design is sound as-is, sharpen the pitch language, don't change the spec.

**Problem:** A doesn't address the design doc itself. Lines 5281 and 5423 use "rejects" and "requires" — that's not pitch copy, it's specification language. If we take A, we must also soften *the spec*, not just the marketing. The brainstorm doesn't note this. A as described leaves the design internally inconsistent: the spec says "compiler rejects concurrent mutation without Mutex" while the actual mechanism is a runtime panic.

**Verdict:** A is underscoped. It's not just a pitch fix — it requires editing the design's own guarantee language to match what's actually implemented. That's a harder change to accept than it appears.

---

### Option B — Framing + guardrails (note at signature)

B says: when a function declares `writes(R)` on a parameterized resource *and* captures a `shared struct` by reference, emit a compiler note.

**Critical flaw:** design.md:5439 establishes that within-project mutation of `mut` fields on `shared struct` doesn't require any effect annotation:

> "within the project, mutating `mut` fields does not require an effect annotation; the compiler tracks these mutations through effect inference on non-`pub` functions"

This means a private function can mutate a `shared struct` `mut` field while declaring only `reads(R)` — or no effects at all. B's note trigger is "writes(R) + shared struct capture" — it never fires for a function that declares nothing. The most dangerous pattern (a helper that looks pure but mutates shared state) is exactly the one B misses.

B is also positionally wrong: it's a *note* not an *error*, so the compiler can't make the design's "rejects" language true. A suppressible note doesn't equal rejection.

**Verdict:** B has the right instinct (surface the gap) but the wrong mechanism. The trigger is too narrow and the severity is too weak.

---

### Option C1 — Shared-mutation as a per-struct effect resource (`Shared[T]`)

C1 introduces `Shared[UserState]` as an implicit resource; reads/writes to `mut` fields contribute effects to the inferred row.

**Critical flaw — reverses design.md:5439:**  The line says internal `mut` field mutations don't require effect annotations. C1 makes them contribute `writes(Shared[T])` to the inferred row, which is precisely the behavior design.md:5439 decided against. The decision wasn't accidental — it was intentional to avoid effect-row explosion through internal mutation.

**Stress test — false conflicts on different instances:** Two tasks operating on different `shared struct UserState` instances both get `writes(Shared[UserState])` in their effect row. The effect conflict matrix sees two `writes` on the same *type-level* resource → conflict → serialized. But these two tasks never touch the same memory. C1 type-level granularity can't distinguish instances.

C2 (instance-keyed) solves this but requires carrying allocation identity into the effect system — essentially pointer provenance in the type system. That's a qualitatively different level of analysis complexity, not an incremental extension of the current parameterized resource mechanism (which uses statically nameable identities like TCP connection handles, not heap allocation addresses).

**The masking problem (Q3) is load-bearing:** If `lock` doesn't mask `writes(Shared[T])`, then correctly-locked code still "conflicts" with itself and gets serialized. The effect system would lie: "these two tasks conflict" on code that the programmer already serialized correctly with Mutex. To fix this, `lock` must mask `writes(Shared[T])` — but design.md:5427–5434 establishes `lock` as effect-transparent:

> "The `lock` block acquires the lock on entry and releases on exit... No `.lock()` method or guard values — scope is always visible."

The current spec has no effect-masking behavior for `lock`. Adding it for `Shared[T]` only creates a special case that undermines the uniformity of `lock`'s behavior.

**Verdict:** C1 is off the table. It reverses design.md:5439, creates false conflicts across instances, and forces a lock-masking special case onto an intentionally effect-transparent construct. The brainstorm identifies all three correctly but understates how serious the instance-granularity problem is.

---

### Option C3 — Capability-style marker types

C3 lifts Pony-style reference capabilities (`iso`, `val`, `ref`, etc.) into the type system.

This is a complete type-system redesign, not an incremental fix. The type variance rules, generic bounds, and subtyping all change. It also makes the Kāra pitch move from "effects give you concurrency safety" to "capabilities give you concurrency safety" — an entirely different language identity. The brainstorm correctly identifies this as the largest surface change.

**Verdict:** Off the table for a targeted fix. Could be considered as a future language evolution, but it's not answering Q1.

---

### Option D — Arc-promotion boundary check

D proposes: when the arc-promotion pass promotes an `Rc` to `Arc`, walk every write to a `mut` field of that `shared struct` inside the parallel region. If any write is not inside a `lock` block → compile error.

**What D gets right:**

1. It's consistent with the design's existing language. Lines 5281 and 5423 already say "compiler rejects concurrent mutation without Mutex." D operationalizes this as an actual compile-time check, making the spec language true rather than aspirational.

2. It keeps the effect system clean. No `Shared[T]` resource family, no new effect verbs, no effect-row bloat, no reversal of design.md:5439. Effects remain for I/O resource conflicts; the type/ownership layer handles memory concurrency.

3. The arc-promotion pass is the right enforcement site — it already knows which values cross parallel region boundaries. D is an additional check on already-computed information.

4. With D in place, the runtime borrow-flag panic path (design.md:5423's "v1 mechanism") is closed for the cross-task case. The compiler refuses to produce the binary, so the panic cannot be reached.

**What D gets wrong or underspecifies:**

**1. The interprocedural problem is not a "Con" to note — it's the core design question.**

The brainstorm lists interprocedural analysis as a Con and moves on. But consider:

```kara
fn update_node(n: ref shared Node) {
    n.val = 42   // mut field write, no lock
}

fn run(node: shared Node) {
    par {
        update_node(ref node)   // Arc-promoted; write happens inside update_node
        update_node(ref node)
    }
}
```

If D only checks for `lock` ancestors within the `par {}` block's syntactic body, `update_node`'s field write is invisible to the check. The Arc-promoted value escapes into a function call; the write happens interprocedurally. D as described doesn't catch this.

The brainstorm says "errors fire at the parallel region, not at the function that performs the write" as if this is merely a UX concern. But actually, for the interprocedural case, D needs to *see into the called function* — which requires an interprocedural analysis that the brainstorm doesn't spec out.

**2. The read side is unaddressed.**

D only checks writes. But if task A writes to `node.val` inside a `lock` block, and task B reads `node.val` without a `lock` block, the runtime borrow flag would normally catch this (the flag tracks both readers and writers). Under Arc, the borrow flag is still per-task because `shared struct` is `Send` but not `Sync` — so two tasks actually share the same borrow flag storage?

Wait — no. Under `Arc`, the borrow flag lives in the heap allocation, shared between both tasks. Task A's write acquires the borrow flag (exclusive). Task B's read also checks the borrow flag. If B reads while A is writing (even with A inside a `lock`), B's borrow flag check would... actually, this depends on whether the borrow flags are themselves synchronized. If they're not atomic, the borrow flag check itself is a race.

D should address whether reads of `mut` fields inside parallel regions also require `lock` blocks. The current v1 mechanism (borrow flags) may not be sufficient under Arc for the read side, depending on implementation.

**3. `spawn()` lifetime is unbounded — "parallel region" is underspecified for D.**

The arc-promotion pass handles `spawn()` by conservative promotion: if a value's live range overlaps a parallel region that includes `spawn()` calls, it promotes. But for detached tasks (if they're ever supported), the "parallel region" extends indefinitely. D's check needs a clear definition of when the region ends. The current brainstorm assumes structured concurrency (`TaskGroup` joins); unstructured `spawn()` needs separate treatment.

**4. The error site problem is worse than noted.**

If D fires an error at the parallel region when a called function contains an unlocked write, the programmer sees: "parallel region uses shared struct without Mutex" — but the actual fix needed is inside `update_node`, possibly in a different module. The diagnostic needs to point to both: the parallel region (the location that triggered the check) and the specific unlocked write inside the called function (the root cause). This requires preserving the interprocedural call chain in the diagnostic.

---

## Synthesis: what the options get right and what the real path is

The brainstorm's lean is D alone. I agree D is directionally correct but think the analysis undersells the work required.

**D is the right destination. The path to get there has two phases:**

**Phase 1 (intraprocedural D):** Catch unlocked `mut` field writes that occur *directly inside* a parallel region's syntactic body — calls within `par { }`, `spawn()` closures, `TaskGroup` task bodies. This is purely syntactic and local. It catches the obvious cases, matches the design's "compiler rejects concurrent mutation without Mutex" intent, and can ship as v1.

**Phase 2 (interprocedural D):** Extend the check to see through call boundaries. When a function is called from inside a parallel region and it receives an Arc-promoted `shared struct` argument, the compiler checks that any `mut` field writes inside that function are wrapped in `lock`. This requires marking functions at their public boundary: a function that writes `mut` fields on a `shared struct` argument without locking is only safe to call *outside* parallel regions. Inside a parallel region, it must lock. This is a per-argument annotation, not a full effect extension — it's closer to ownership mode than to effect declarations.

---

## Items the brainstorm doesn't address that matter for Q1

**1. The design doc's guarantee language needs to match the implementation tier.**

Lines 5281 and 5423 use "compiler rejects" and "requires." If D is Phase 1 only (intraprocedural), the design should be updated to say "the compiler rejects concurrent mut field writes that are syntactically inside a parallel region without a lock block; interprocedural cases are statically checked at call boundaries [Phase 2]." Leaving the current language makes the spec a forward promise the implementation doesn't fully keep.

**2. The read side under Arc needs explicit resolution.**

Do `mut` field *reads* inside parallel regions require `lock` blocks? The borrow flag mechanism tracks both readers and writers. Under single-task Rc, a read while a write borrow is active panics. Under Arc (multi-task), if the borrow flags are not atomically accessed, there's a potential race on the flag itself before we even get to the field. D only addresses writes; the spec should explicitly state whether reads are also gated.

**3. Immutable fields are D's natural safe case and should be stated.**

`shared struct` fields that are not `mut` are immutable after construction. Multiple tasks reading immutable fields from an Arc-promoted value is safe with no lock. D implicitly handles this (nothing to check), but the design should state it explicitly as a property: "immutable fields of `shared struct` are safe to read concurrently across tasks without a lock block."

**4. B's diagnostic note is not superseded by D for all users.**

The brainstorm says B becomes redundant once D gives a hard error. But B's note fires at the *declaration site* of `shared struct` with `mut` fields, educating the programmer at the moment they define the type. D's error fires at the *use site* inside a parallel region, potentially far removed from the declaration. Both touch different moments in the programmer's workflow. B's `shared struct` declaration-site note is still useful as a learning tool independent of D.

---

## Line reference discrepancy

The brainstorm cites `docs/design.md:338` for the `lock` effect-transparency claim. That line in the current design doc is in the error-handling section (inside the `.context()` discussion). The actual `lock` block semantics and effect-transparency behavior is at lines 5427–5434. This is a stale line reference — the design has been edited since the brainstorm was written.

---

## Conclusion

**Q1 lean: D, in two phases, with design doc language audit.**

- D is the right mechanical answer. It operationalizes the design's already-stated guarantee ("compiler rejects concurrent mutation without Mutex") as an actual compile-time check.
- Phase 1 (intraprocedural): ship with the parallel region syntax. Catches direct writes inside `par {}` / `spawn` closures without `lock`.
- Phase 2 (interprocedural): extend to call boundaries. Requires a per-argument annotation on functions that write `mut` fields on `shared struct` arguments, indicating safety only outside parallel regions without a lock.
- Alongside D: audit and update design.md lines 5281 and 5423 to match the phased delivery. The spec should say what each phase guarantees, not uniformly use "compiler rejects" when Phase 1 doesn't catch the interprocedural case.
- A is insufficient: it requires softening the design's own language, not just the pitch copy, and that's a harder change than it appears.
- B has the right intuition for the diagnostic story but the wrong trigger and severity. Its declaration-site note survives as a *complement* to D, not a *replacement*.
- C1 is correctly off the table.
