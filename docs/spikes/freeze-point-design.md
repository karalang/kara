# Freeze points: sharing an RC value across `par` branches (B-2026-08-01-33, mechanism 3)

**Status:** design call, nothing built. Proposes a language-surface addition, so
adopting it into [`docs/design.md`](../design.md) is the owner's step — this
document exists to make the call concrete enough to accept, reject, or amend,
and to record why the alternatives lose.

## The gap, stated as the constraint it actually is

A `shared struct` uses non-atomic refcounting, so it cannot be reached from two
`par {}` branches: `E_CONCURRENT_SHARED_STRUCT`. The reason is **refcount
traffic, not payload mutation** — `emit_rc_inc` is a plain load/add/store, so
even pure reads race the header (B-2026-07-28-13's SIGSEGV).

One shape is already admitted (B-2026-08-01-33, `66769bf`): reading an immutable
**scalar field** off a captured handle, which lowers to a plain deref and emits
no refcount traffic at all (measured: atomic 2 / plain 0). Everything else —
passing the handle, binding a nested handle, calling a method — is a
*materialization* and is rejected.

The motivating program, LeetCode #133, is blocked on materialization:

```kara
shared struct Node { val: i64, mut neighbors: Vec[Node] }

fn sum_clones(root: Node, count: i64) -> i64 { … clone_graph(root) … }

let (s1, …, s18) = par {
    let s1 = sum_clones(root, 28);
    …
};
```

**The traversal is in a callee.** That single fact disqualifies most designs:
any mechanism that does not put frozen-ness *in the type* must infer, across
function boundaries, that `clone_graph` never writes through `root` and that the
handles it materializes are safe. That is mechanism 1's instance-level escape
analysis, whose failure mode is a racing refcount — non-deterministic and
arch-sensitive (this repo has an arm64-only RC leak precedent, B-2026-07-12-29,
that x86 CI ran green).

So: **the freeze must be expressed in the type system, or it is mechanism 1
wearing a hat.**

## The call

Add `frozen T` as a **type-level mode**, a sibling of `ref T` / `mut ref T`
rather than a new generic type or a library wrapper.

A `frozen T` is a **non-owning, non-counting handle to a deeply-immutable
`shared` value whose lifetime is guaranteed to span the current region.**

Four properties, and each one is load-bearing:

1. **Non-counting.** Codegen emits no `rc_inc`/`rc_dec` for a `frozen` value or
   anything projected from it. This is what makes concurrent reads safe — it
   removes the raced header rather than making it atomic. It is the same
   argument `rc_elide.rs` already ships default-ON for read-only non-escaping
   `ref` params (B-2026-07-15-21): a balanced retain/release pair whose lifetime
   is dominated by a longer-lived owner is a no-op, so it can be skipped.
2. **Deep / sticky.** Projection preserves the mode: if `g: frozen Node` then
   `g.neighbors: frozen Vec[frozen Node]` and indexing it yields `frozen Node`.
   Without stickiness the interior handles a traversal materializes are ordinary
   `shared` values and the race returns — this is exactly why mechanism 2's
   atomic promotion had to close over the whole reachable type set rather than
   promote the root alone.
3. **Non-escaping.** A `frozen` value may not be returned, stored into a
   non-`frozen` container, or captured by anything outliving the region. This is
   the fail-closed condition that pays for property 1: a non-counting handle
   that outlives its owner is a use-after-free.
4. **Crosses signatures.** `fn sum_clones(root: frozen Node, count: i64) -> i64`.
   This is the property that makes #133 reachable at all, and the reason a mode
   beats a local annotation.

Creation is an explicit statement — declared, never inferred:

```kara
let g = freeze graph;     // `graph` is read-only for the rest of the scope
par { sum_clones(g, 28); sum_clones(g, 28); … }
```

The check at the freeze site is the one genuinely new piece: the value must be
**deeply immutable for the region** — no live mutable handle to the instance or
to anything reachable from it. For the build-then-read shape that motivates
this, that reduces to "no other live binding of this instance", which is local
and cheap. `graph` stays usable read-only; freezing does not consume it.

## Why not the alternatives

| | why it loses |
|---|---|
| **Mechanism 1** (inferred instance-level escape analysis) | Same fact, inferred instead of declared. A blind spot is a racing refcount, not a compile error. Large, all-or-nothing, and the repo's three most recent analysis bugs (B-2026-08-04-13/-14/-15, one day) were all "a walk silently missed a case" — with *reproducible* symptoms, the easy kind. |
| **Mechanism 2** (whole-program atomic promotion) | Built and shipped inert (`60f14b8`). Triggers on one multi-branch use anywhere and then imposes atomic RC on every **sequential** use of the type program-wide — measured ~9.5x on RC-saturated sequential code. Already settled as opt-in-only for that reason. |
| **`par struct` migration** (today's answer) | Correct and one `karac fix` away for an immutable type, but buys mutual exclusion for a structure nobody mutates and pays the same atomic-RC cost. |
| **`Frozen[T]` as a library generic** | Cannot suppress RC emission and cannot enforce non-escape; both need compiler support, so a mode is the honest spelling and composes with the existing borrow vocabulary. |
| **Per-binding `readonly_scalar_fields`** | A recorded dead end — see B-2026-08-01-33. Widens *which fields may be projected*, while every remaining case is blocked on *whether the handle may be materialized at all*. Orthogonal axis. |

## Staging

Each stage is independently useful and independently testable.

1. **`frozen` as a parameter mode + par-branch admission + escape checking.**
   No stickiness: only whole-handle pass-through and scalar projection. Closes
   "share an immutable structure read-only across branches" for non-traversing
   shapes. This is the stage that validates the escape checker, which is where
   the unsafety lives.
2. **Stickiness** through field access and indexing. This is what #133 needs,
   and it is the bulk of the type-checker work.
3. **Freezing a `mut`-bearing type** — the deep-immutability check at the freeze
   site. #133 needs this too (`Node.neighbors` is `mut`), but it is separable
   from stage 2 and much smaller.

## Risks, stated plainly

- **Non-counting handles are a new unsafety surface.** If escape checking has a
  hole, the symptom is use-after-free. This is the same exhaustiveness class
  that produced three bugs in a day, so the escape check must be written
  fail-closed and no-wildcard, the way `region_bindings` was after
  B-2026-07-04-13 — and stage 1 exists to shake it out before stickiness
  multiplies the surface.
- **Stage 2 touches the type checker broadly.** Mode propagation through
  projection is not a contained edit.
- **It is language surface.** design.md is authoritative and this needs owner
  buy-in; that is the point of writing it down rather than building it.
- **Unmeasured.** No prototype, so the claimed win (#133's 1.28x punch loop,
  plus the sequential 9.5x that stays unpaid) is projected from the existing
  measurements cited above, not observed. Stage 1 should carry a measurement
  before stage 2 starts.

## What this does not attempt

Lifetimes. `frozen` is region-scoped and non-escaping by construction; there is
no lifetime parameter, no variance story, and no attempt to let a frozen handle
outlive its region. That restriction is what keeps the checking local, and
relaxing it later is a separate decision.
