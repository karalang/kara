# Freeze points: sharing an RC value across `par` branches (B-2026-08-01-33, mechanism 3)

**Status:** design call; **stage 1 partly built and inert** — the `frozen`
parameter surface and the escape checker have landed, `par` admission and RC
suppression have not, so the mode still changes no program's behaviour. Proposes
a language-surface addition, so adopting it into
[`docs/design.md`](../design.md) is the owner's step — this document exists to
make the call concrete enough to accept, reject, or amend, and to record why the
alternatives lose. Build log and two corrections to the staging are at the
bottom; read those before starting from the staging list.

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

## Stage 1 build log, and a correction to the staging above

**Landed so far (inert):** `TypeKind::Frozen` with all 16 exhaustive-match sites
handled; `frozen T` parsed as a *contextual* keyword (so no program using the
name breaks); accepted only in the top-level type of a **Kāra function's**
parameter and rejected elsewhere — including on a foreign-import (`extern`)
parameter, where an ABI signature has no callee body to check and the mode
could never mean anything; the mode **recorded on `Param::is_frozen`** and
round-tripped by
`karac fmt`; verified that a `frozen N` parameter compiles and runs identically
to a plain `N` one, and that `E_CONCURRENT_SHARED_STRUCT` still fires on a
multi-branch capture of a `frozen` binding.

**Three things learned by building it. Findings 2 and 3 supersede the staging
above; read them before starting anything from that list.**

**1. Per-site transparency is the wrong shape; normalize once.** Teaching
downstream phases to see through `Frozen` was tried first and found three
separate rounds of `TypeKind::Ref | MutRef` unwrap sites — `call_dispatch`,
`functions`, `mono`, then the param type-name registry — with no reason to think
the next round was the last. Each is a place a later phase could disagree about
what `frozen` means. Stage 1's contract is that `frozen T` **is** `T`, and the
honest implementation of an identity is one decision at the point of
construction, which cannot disagree with itself. (That decision was first an
*erasure*; finding 3 replaces it with a *recording* — same single point, but it
keeps the information instead of throwing it away.)

**2. THE ESCAPE CHECKER CANNOT BE BUILT ON WHAT STAGE 1 FIRST SHIPPED, and the
staging above reads as though it can.** As first landed, stage 1 erased the mode
at parse time, so by the time any checking phase ran there was nothing left to
check. Un-erasing is a *prerequisite* for the escape checker, not a follow-on.

**3. Un-erasing does NOT mean putting the mode back in the type tree.** The
first plan for (2) was: parser retains `TypeKind::Frozen`, the checking phases
see it, and one normalization pass strips it after ownership and before codegen.
That plan is wrong in a way worth recording, because it looks right.

Its problem is the strip pass. For codegen never to see `Frozen`, the strip has
to run on **every** path that reaches a backend — and there are many:
`Pipeline::run_all_checks` in `cli.rs` covers the CLI, but `lib.rs` has four
more interpreter drivers, plus `repl.rs`, `test_jit_dispatch.rs`, and
`drop_differential.rs`. Each takes `&Program` immutably at the backend
boundary, so the strip cannot live at the boundary itself; it has to be
installed correctly at each entry point, and a single missed one puts the
compiler straight back into the whack-a-mole of finding (1) — with a *silent*
failure mode, since a missed strip only shows up on programs that use the
keyword.

What replaced it: **the mode is recorded on the parameter, as
`Param::is_frozen`, and never enters the type tree at all.** The precedent is
already in the AST — `Param::is_comptime` is exactly this: a parameter-position
prefix modifier, recorded as a bit, with its rule landing later. `parse_type`
still recognizes the keyword (so the misplaced-use diagnostic stays focused
wherever it appears) and reports it back to `parse_param` through a one-shot
flag.

This is strictly better on the axis that matters:

* **Every checking phase can see it immediately** — resolver, typechecker,
  ownership, `concurrency` all have the `Param` in hand. That is the whole
  point of un-erasing, delivered without a new pass.
* **Codegen cannot see it, by construction** — not "because a pass removed
  it", but because it was never in the structure codegen reads. No entry-point
  audit, no strip pass, no silent failure mode. Codegen will learn which values
  are non-counting the way `rc_elide.rs` already does it: a plain-data hint set,
  per the codegen-containment invariant.
* **`karac fmt` round-trips** — the param printer writes the keyword back.
  (Adding that printer is what turned up B-2026-08-04-21: `karac fmt` was
  silently deleting `unsafe fn`, `comptime fn`, and `comptime` param prefixes,
  the same class of bug for three modifiers that had shipped without one.)

The cost, stated plainly: **a bit on `Param` only spans parameter position.**
That is exactly the surface stage 1 accepts, so nothing is lost today — but
widening `frozen` to `let` annotations, struct fields, or generic arguments in
stage 2 does need a type-level mode. `TypeKind::Frozen` and its ~16
exhaustively-checked walk arms are retained unconstructed for that, and the
variant's doc comment says so; re-deriving them later would be redoing verified
work. Widening a position and giving it a checker are the same task, which is
the reason to keep the restriction until stage 2 does both.

So the revised stage 1 remainder is: **escape checker → freeze-site
immutability check → par admission**, in that order, with admission still last
for the reason already given. The un-erasure that used to head this list is
done — it is `Param::is_frozen`, and it cost no pass at all.

## Stage 1: the escape checker (landed)

`src/ownership/frozen_escape.rs`, emitting `E0511` from the per-function
ownership driver. It runs while the mode is still inert, which is the whole
point: nothing yet depends on the rule, so this is the cheapest moment to find
out whether the rule can actually be written.

**It is a whitelist.** Walks are exhaustive with no `_` arm, so a new AST node
breaks the build rather than opening an escape route, and a frozen identifier is
reported at the *leaf* — only the two positions stage 1 permits consume their
operand without recursing. Anything nobody enumerated reports. The failure
direction is a false positive, never a missed escape. That shape is a direct
response to B-2026-08-04-13/-14/-15, which were one failure mode (a walk that
recognized some spellings and ignored the rest) in three subsystems in a day.

Permitted: reading an **immutable scalar** field (the predicate
`concurrent_shared.rs` already admits, for the same reason — an immutable field
races nobody and a scalar read copies a register), and **whole-handle
pass-through to another `frozen` parameter**, which is what lets the guarantee
compose across a call instead of being re-derived at each site. That second one
is the property #133 needs.

**The design's own claim got tested and one hole turned up.** The first draft
accepted a scalar field read inside a **closure body**. The read is safe; the
capture that enabled it is not — the closure's environment holds the handle and
can outlive the call by being returned, stored, or handed to `spawn`. Both
permitted positions are now suppressed inside a closure, using the same
`in_closure` flag and the same argument as `result_escape.rs`. `par` / `seq`
branches are deliberately *not* closures for this rule: they join before the
function returns, and admitting exactly that sharing is the feature.

Conservatism that is documented rather than hidden: shadowing is untracked; only
free-function calls compose (a method call reports, because resolving one needs
the typechecker's callee map); an unresolvable type yields an empty scalar-field
set, so every projection off it reports. All three over-report.

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
