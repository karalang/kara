# Freeze points: sharing an RC value across `par` branches (B-2026-08-01-33, mechanism 3)

**Status:** design call; **stage 1 COMPLETE ON BOTH PARALLELISM SURFACES**
(surface, escape check E0511, freeze-site check E0512, RC suppression,
explicit-`par` admission, auto-par admission), **stage 2 LANDED for PLACES**
— projection through field access, indexing, and tuple indexing is sticky, so
a recursive traversal of a `shared` graph compiles and runs from several `par`
branches (measured 3.7x wall on 4 cores) — and **stage 2.5 LANDED for
BINDINGS** (`let k = n.kids[i]` is a non-counting alias rather than a
materialised handle), **stage 2.6 for `for` LOOPS** (`for k in n.kids`) and
**stage 2.7 for METHODS** (`frozen self`, and calls on a frozen place), so
every spelling of a traversal compiles — and **stage 3a for the `freeze`
STATEMENT** (`let g = freeze root;`), which introduces a frozen root from an
ordinary local instead of requiring a `frozen` parameter to carry one in. See § "Stage 2.5", which corrects
stage 2's sizing of that work — it is not mechanism 1 for a parameter-rooted
place; § "Stage 2.6", which retracts stage 2.5's unmeasured reason for
refusing `for`; § "Stage 2.7", which records a hole found in its own first
draft; and § "Stage 3a", which reuses stage 2.5's binding class rather than
building the codegen the stage-3 section below sizes. **Stage 3b remains** —
the per-instance freeze — and #133 still needs it: `Node.neighbors` is `mut`,
so the motivating program is refused at the freeze site whether the freeze is
spelled as a parameter mode or as a statement. Proposes a
language-surface addition, so adopting it into [`docs/design.md`](../design.md)
is the owner's step — this document exists to make the call concrete enough to
accept, reject, or amend, and to record why the alternatives lose. Build log
and two corrections to the staging are at the bottom; read those before
starting from the staging list.

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
   the unsafety lives. **LANDED.**
2. **Stickiness** through field access and indexing. This is what #133 needs,
   and it is the bulk of the type-checker work. **LANDED for places** (see
   § "Stage 2") — and it turned out *not* to be type-checker work at all: the
   place walk is answered structurally from `struct_info`, in the escape
   checker, with no type-level mode and no `TypeKind::Frozen`. The ~~binding
   half is deferred and belongs to mechanism 1, for the reason the measurement
   gives~~ — **wrong, and landed as stage 2.5**: for a place rooted at a
   `frozen` parameter the owner needs no analysis at all, so the retain could
   simply be removed. Mechanism 1 is what the *general* case needs, not this
   one.
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

## Stage 1: the freeze-site check (landed)

`src/ownership/frozen_freeze_site.rs`, emitting `E0512`. The escape checker asks
"can this handle get out?"; this asks the prior question — **may this type be
frozen at all?**

**Where the freeze site turned out to be.** This document sketches
`let g = freeze graph;` and puts the check there. Stage 1 has no `freeze`
expression: declaring `frozen T` on a parameter is the only way to obtain a
frozen value, so the *parameter declaration* is the freeze site, and that is
where the check lives. When the `freeze` statement lands, the check moves to it
— the rule is the same, only the site changes.

**Why the stage-1 form is structural.** The rule above is a property of the
*instance*: no live mutable handle to it or to anything reachable, for the
region. Stage 1 has no region, no freeze statement, and no instance-level
liveness, so it cannot evaluate that. What it can do is require the type to make
such a handle impossible: no `mut` field anywhere in the reachable closure. That
is strictly stronger, hence sound as a stand-in, and it converts staging item 3
from a silence into a diagnostic — `frozen M` where `M` has a `mut` field is
refused *now*, rather than accepted while the check that would justify it does
not exist.

Also refused, both fail-closed: a **non-`shared`** type (no refcount to skip, so
the mode asserts a representation the value does not have) and a **`par
struct`** (already atomically shareable — `frozen` would be a no-op dressed as a
guarantee). An unresolvable type name is refused, not assumed freezable.

The predicate is `deep_immutability_closure` — mechanism 2's, renamed from
`atomic_promotion_closure` and shared rather than reimplemented. Promotion uses
the returned set as what to promote; the freeze check uses only `is_some()`.
Two predicates could drift into two opinions about what "deeply immutable"
means.

**What this costs #133, stated plainly:** `Node.neighbors` is `mut`, so the
motivating program is refused at the freeze site. That is what the staging
always said (item 3 defers exactly this), but it is now a diagnostic that names
the missing piece rather than a program that quietly type-checks.

## Stage 1: the measurement, and what it changes about the last slice

Measured IR for one pass-through chain across three parameter modes:

| `fn inner(n: ? N)` / `fn outer(n: ? N) { inner(n) }` | `rc_inc` | `rc_dec` | `atomicrmw` |
|---|---|---|---|
| owned `N` | 4 | 9 | 0 |
| **`frozen N`** | **4** | **9** | **0** |
| `ref N` | **0** | **0** | 0 |

Three readings, and the third changes the plan.

**1. `frozen` is byte-identical to owned.** The mode is fully inert in codegen —
the containment claim confirmed from the *output* side rather than argued from
the source side.

**2. The traffic is non-atomic.** Plain load/add/store. That is exactly why a
multi-branch capture SIGSEGVs, and therefore why admission cannot land before
the traffic is gone: **admission and RC suppression are one deliverable, not
two.** Landing admission alone reproduces B-2026-07-28-13.

**3. `ref` already reaches zero** — and *why* it does turned out to matter more
than the fact itself.

My first reading was that `rc_elide` (default ON since B-2026-07-15-21) elides
the balanced pair, so suppression would mean routing `frozen` params into that
channel. **That was wrong**, and one probe disproves it: with
`KARAC_RC_ELIDE_REF_PARAMS=0`, `ref` *still* emits 0/0. The zero is the **borrow
convention** — codegen emits no retain/release for a borrow because the caller
keeps ownership — not the rc-elide pass. Everything that followed from the wrong
reading (carrying over `safe_elidable_ref_params`' fresh-rvalue proof) is moot:
that pass is not on this path at all.

## Stage 1: RC suppression (landed)

`frozen T` **lowers to `ref T`** at parse time. The design calls a frozen value
"non-owning, non-counting"; *non-owning is exactly what `ref` already means*, so
the mode is expressed in the existing borrow vocabulary instead of new
machinery. Measured after: owned `rc_inc=4/rc_dec=9`, frozen **0/0**, ref
**0/0** — frozen is now byte-identical to `ref` rather than to owned.

**Why `param_modes` was not enough**, since this is the trap: ownership already
inferred `OwnershipMode::Ref` for these params (a read-only param infers `Ref`)
while codegen still emitted the owned traffic. Codegen drives the calling
convention from the **declared type**, per CLAUDE.md's rule that body-level
ownership analysis "is not a signature-derivation mechanism". A mode that wants
borrow semantics has to say so in the declared type.

**Containment is preserved.** `TypeKind::Frozen` is still never constructed.
Codegen sees `Ref` — a form it already handles on a shipped, ASAN-verified path
— so this adds no unwrap site anywhere, and the frozen-ness stays on
`Param::is_frozen` where the escape check, the freeze-site check, and admission
read it. The formatter unwraps the borrow so `karac fmt` still round-trips
`frozen N`, not `frozen ref N`.

Pinned by `frozen_param_emits_no_refcount_traffic_like_a_borrow`, which asserts
frozen == 0, frozen == ref, and owned > 0 (the positive control that keeps the
zero assertion non-vacuous).

## Stage 1: `par` admission (landed) — and the bug it uncovered

Admission itself is small: a `frozen: bool` on `TrackedBinding`, plus an admit
arm in `detect_par_block_conflicts` that fires before the mechanism-2 promotion
arm. It is **unconditional** — no env gate — because the two checks that license
it are always on. E0512 proved every reachable `shared` type is free of `mut`
fields, so there is no *payload* to race; the borrow lowering removed the
refcount traffic, so there is no *header* to race.

That is the contrast with mechanism 2 in one line: **promotion makes the header
race safe by making it atomic (~9.5x on sequential code); freezing removes the
traffic, so there is nothing to pay for.**

**Writing it uncovered a live miscompile that had nothing to do with `frozen`**
(B-2026-08-05-10, fixed in `93b1a81`). A `ref`-borrowed `shared` handle captured
into a par branch read as **zero**: interpreter 108, AOT 101, `karac check`
silent.

The classifier picks `ParCaptureMode::SharedRc` from the capture's *type name*
alone, and the head-name helper strips `ref` on the way there — so `ref N`
classified exactly like owned `N`. But a borrow's slot holds a
**pointer-to-handle**, so `emit_arc_inc` did not touch a refcount at all: it
atomically added 1 to *the owner's stored handle pointer*. The callee then
dereferenced a pointer one byte off. The branch-exit `dec` subtracted the 1
back, so the corruption left no trace and the symptom was a wrong answer rather
than a crash — which is why a memory-corruption bug sat undetected.

**One branch is enough**, so it never reached the `E_CONCURRENT_SHARED_STRUCT`
gate: the gate refuses the two-branch shapes loudly, and the one-branch shape it
permits was the broken one.

Two things worth carrying forward from that:

**Traffic counts cannot see a wrong answer.** The RC-suppression commit shipped
a test that counted `rc_inc`/`rc_dec` in the IR, and the full 13k-test suite
passed with this miscompile live, because nothing in it executed a borrowed
capture in a par block. Every RC-shape test in this family now pairs the IR
assertion with an execution that checks the computed value.

**A loud gate can hide a quiet bug behind it.** The shapes the gate refused were
the ones anyone would have tested by hand; the shape it allowed was the one
nobody looked at. Worth remembering when the next gate is relaxed.

## Stage 1: the auto-par arm (landed), and a claim worth checking before repeating

The ledger row for this bug says auto-par **silently** declines a `shared`
value. That is **wrong**, and it was repeated downstream — including in this
document's own build log — before anyone measured it. `karac query concurrency`
reports the decline explicitly:

```json
{"gate": "not_cross_task_safe",
 "reason": "the body touches a non-`par` `shared` value whose refcount is not atomic"}
```

A named gate and a specific reason, on the surface built for exactly that. A
plain `karac build` prints nothing, but that is true of *every* auto-par decline
— warning on each non-parallelized loop would be noise.

**The real gap was a disagreement between the two surfaces.** Once explicit
`par {}` admitted a `frozen` handle, auto-par still declined the identical
hazard and forced the loop sequential. Same facts, same reasoning, opposite
answers. That is now closed: a frozen-touching loop reports `gate: "proven"`,
fans out, and computes what sequential execution computes.

**Keyed on place roots, never on types** — the load-bearing detail. Exempting by
type would have been shorter and would have opened a real race: a body holding
both a `frozen S` parameter and an ordinary `S` parameter would have had *both*
cleared, and the second is exactly the refcount race B-2026-07-16-6 documents.

One thing that looks like a hole and is not: a `shared` value allocated and
consumed **inside one iteration** passes the gate on `iter_local`'s own
independent evidence (B-2026-07-30-1), not through the frozen whitelist. A hole
test has to use a value that outlives the iteration — the first attempt at that
test did not, and passed for the wrong reason.

## Does it actually buy anything? (the measurement gating stage 2)

Same program, three parameter modes, 4096 iterations x 200000 inner ops, 4
cores, all printing the identical answer:

| mode | gate | wall | cpu | |
|---|---|---|---|---|
| owned `S` | `not_cross_task_safe` | 0.400s | 0.396s | sequential |
| `ref S` | `not_cross_task_safe` | 0.482s | 0.481s | sequential |
| **`frozen S`** | **`proven`** | **0.162s** | 0.514s | **2.5x** |

**2.5x wall-clock from adding one keyword**, and the cpu/wall ratio confirms
real parallelism rather than a folding artifact.

Two caveats that keep this honest:

**The work had to be verified real first.** At the original size everything
finished in ~0.05s with no separation between modes — LLVM can close-form that
inner sum. Scaling the inner loop 10x scaled the time 10x, which is what proved
there was work left to parallelize. A "no speedup" conclusion from the first
run would have been an artifact.

**`ref` is not faster than owned here, and that is not evidence against
suppression.** The refcount traffic is once per call while the body runs 200000
inner iterations, so RC cost is noise at this ratio. What this measures is
*parallelism*; suppression's own value needs a body-to-call ratio nearer 1 and
is not answered here.

**A reporting trap found while measuring**, filed as B-2026-08-05-13: the same
loop reports `fanned_out: true` whether the accumulator is a local (genuinely
parallel, 3.9x cpu) or a `mut ref` parameter (single-threaded, `user` ==
`real`). The field describes the analyzer's verdict, not the emitted code. It
caught a test in this very work — named `..._fans_out_...` on the strength of
that field, over a program that ran sequentially. Check with
`bash -c 'time ./binary'` and compare `user` to `real`; `nm` cannot tell you,
because the `karac_par_*` statics are stripped at link.

## Stage 2: stickiness through projection (landed) — PLACES, not bindings

Stage 1 permitted exactly two shapes: an immutable **scalar** field one level
deep, and the **bare handle** as an argument to another `frozen` slot. That is
why the shape this whole entry is about did not compile: a traversal needs
`n.kids.len()` and `n.kids[i]`, and both were "cannot be projected through
`.kids`".

Stage 2 generalises the *place*. A **frozen place** is the parameter, or any
chain of `.field` / `[i]` / `.0` rooted at it, and the mode survives every
step. A frozen place may be:

1. **read**, when it resolves to a scalar — `n.val`, `n.inner.deep.d`,
   `n.kids[i].val`;
2. **passed whole to another `frozen` parameter** — `g(n.inner)`,
   `g(n.kids[i])`, at any depth;
3. **queried with `len()` / `is_empty()`** when it reaches a builtin container.

It may *not* be **bound**. `let k = n.kids[i]`, `for k in n.kids`, returning
it, storing it, a user method receiver, a non-`frozen` slot — all still report.
*(Stages 2.5 and 2.6 below admit the first two; the rest still report.)*

### The boundary is where the measurement put it

Per-function refcount traffic in the emitted IR, borrow-mode receiver,
counted inside `outer`'s own frame (`main` and the `__karac_*` drop helpers
carry traffic in every variant, so a module-wide count cannot see this):

| body | `rc_inc` | `rc_dec` |
|---|---|---|
| `readd(o.inner.deep)` — chained projection into a frozen slot | 0 | 0 |
| `readi(o.kids[i])` — index projection into a frozen slot | 0 | 0 |
| `o.kids.len()` | 0 | 0 |
| `o.inner.get()` — method on a projection | 0 | 0 |
| `let k = o.inner; k.v` — **bound to a local** | **2** | **3** |

A projection is a deref; a binding materialises a counted handle. Two branches
racing that non-atomic refcount is exactly B-2026-07-28-13's SIGSEGV, so the
binding forms stay refused — admitting them needs the retain *removed*, which
is mechanism 1 proper, not a widening of this rule.

Pinned in both directions by
`frozen_place_projection_emits_no_refcount_traffic` (tests/par_codegen.rs):
the three admitted shapes must be `(0, 0)` **and** the binding shape must be
non-zero, so the test cannot pass vacuously if codegen ever stops emitting
refcount traffic for an unrelated reason. Mutation-checked — feeding the
binding body to the projection assertion fails with `left: (2, 3)`.

### Two guards, deliberately independent

Every step of the place walk refuses a `mut` field, even though the
freeze-site check (E0512) has already refused any type whose reachable closure
contains one. The projection rule's safety argument is "concurrent readers
cannot race a payload nobody may write", and that should not silently depend
on a check in a different module. (Assignment *through* a place is refused
further upstream still: the typechecker rejects `n.val = 5` with "shared
struct field `Node.val` is not declared mut".)

### `len` / `is_empty`, the one carve-out

Without a length there is no bounded traversal, so the rest of stage 2 would
be unreachable for the shape that motivates it. The exception is narrow on
three independent axes at once: the method name is one of exactly two; the
receiver must resolve to a **builtin** container, so no user body ever
receives the handle; and any type carrying a user `impl` block is excluded, so
`impl Vec { fn len(…) }` cannot smuggle a body in through the builtin name.

### What it buys, measured

The motivating shape, now expressible: a recursive traversal of a 2^15-node
`shared` tree, run from four `par` branches over one shared root. Same source,
same answer (`2863154246400`, identical across 20 repeat runs and across
interp / `karac run` / AOT):

| build | wall | cpu | |
|---|---|---|---|
| sequential (no `par`, `KARAC_AUTO_PAR=0`) | 0.765s | 0.765s | baseline |
| **explicit `par` over `frozen`, `KARAC_AUTO_PAR=0`** | **0.206s** | 0.762s | **3.7x wall, 4 cores** |

CPU time is unchanged between the two, which is what makes this parallelism
rather than a folding artifact; the work was also confirmed to scale linearly
(doubling the repeat count doubled the time) before any ratio was read off it.
valgrind on the same program: 0 definitely lost, 0 indirectly lost (the
1,216-byte "possibly lost" is the glibc `pthread_create` DTV this entry has
already characterised, 4 blocks for 4 branches).

**The control that matters:** the identical program written with `ref` instead
of `frozen` is still refused at the par capture (`E_CONCURRENT_SHARED_STRUCT`).
Borrow semantics alone do not license the admission — the freeze-site
guarantee does.

### One honest caveat about auto-par — since FIXED, and the mechanism was not what this said

With auto-par left ON, the frozen build additionally recognised *parallel
reductions inside the traversal itself* (`sum`'s accumulator loop, `repeat`'s)
and nested them inside the outer parallel group. The answer stayed correct, but
on this shape it cost ~60% more CPU (1.19s vs 0.75s) for no wall-clock gain
over the explicit-`par`-only build. This entry called that "a cost-model
question about nesting a reduction inside an already-parallel region".

**The cost was real; "nesting" was the wrong mechanism.** Chased down as
B-2026-08-06-30 and fixed. The same ~60% appears with only ONE level of
parallelism — a sequential driver whose single loop fans out, with `sum`'s
reduction running inline under the fork-depth cap, still measured 0.206s →
0.330s CPU. Nothing is nested there. The cost is that `sum`'s loop body is
**outlined** into a worker fn at codegen time, so every recursive call pays an
indirect call through a descriptor instead of running an inlined loop —
whether or not the runtime later decides to fan out.

Why it was lowered at all: `CostEstimator` unrolled the self-recursive body
`INLINE_DEPTH_CAP` times, compounding `RUNTIME_NESTED_LOOP_MULTIPLIER` at each
level, and scored one iteration of a two-child loop at ~14,900,000 units
against a floor of 64. A cycle guard in the estimator drops that to 24 (the
loop now declines) while the driver's loop still scores ~3,600 and still fans
out. Not specific to `frozen`: any self-recursive reduction paid it.

### What stage 2 does NOT close

`Node.neighbors` is `mut`, so #133 is still refused at the freeze site — that
is stage 3, unchanged. And a real BFS binds every node it pops, so #133 needs
the binding case too, which is mechanism 1. Stage 2 makes *recursive
traversals over deeply-immutable graphs* work; it does not make every
traversal work.

## Stage 2.5: bindings (landed) — the boundary moved by removing the traffic

Stage 2 refused every binding form, on the measurement in its own table: `let
k = o.inner` was `rc_inc=2 / rc_dec=3` even in borrow mode, and two branches
racing that non-atomic refcount is B-2026-07-28-13's SIGSEGV. It recorded the
fix as "admitting them needs the retain *removed*, which is mechanism 1
proper".

That was half right. Removing the retain **is** what was needed, but not
mechanism 1's interprocedural elision — for a place rooted at a `frozen`
parameter the owner is known without any analysis at all: it is the caller's
value, and its lifetime is the whole call. So codegen compiles such a binding
as a **non-counting alias** — no `Vec`-element clone, no receive-inc, no
scope-exit dec — and the ownership pass adds the local to the frozen set, so
it inherits every restriction the parameter has.

**A frozen place may now be bound to an immutable local**, and the local is a
frozen root in its own right: it can be read, projected further, passed to
another `frozen` slot, queried with `len`/`is_empty`, and aliased again.

Re-measured, same method (per-function traffic in `outer`'s own frame):

| body | `rc_inc` | `rc_dec` |
|---|---|---|
| `let k = o.inner; k.v` — **frozen** | **0** | **0** |
| `let k = o.kids[0]; k.v` — **frozen** | **0** | **0** |
| `let k = o.inner; readi(k)` — **frozen** | **0** | **0** |
| the same three written `ref` | 2 | 3 |

The `ref` row is the discriminator, not decoration: without it the zeros would
also be produced by a build that emitted no refcount traffic anywhere. Pinned
by `frozen_alias_binding_emits_no_refcount_traffic` (tests/par_codegen.rs),
mutation-checked — disabling the codegen guard turns it red with `left: (2,
3)`.

### What is still refused, and why each is not a widening

- **`let mut k = …`** — a mutable alias could be repointed at a handle from
  anywhere. The assignment would report on its own, but refusing the
  *declaration* makes the alias immutable by construction rather than by an
  argument a later change to the assignment walk could break.
- ~~**`for k in n.kids`** — the loop lowers an element copy, not an alias.~~
  **Wrong, and asserted without being measured — landed as stage 2.6 below.**
- **a place that is not a `shared struct`** — a `Vec[T]` / `String` place is a
  `{ptr,len,cap}` aggregate whose binding codegen registers as a buffer owner,
  and a generic type resolves its fields through unsubstituted parameter
  names. Neither is a single refcounted pointer, which is what the alias class
  is; both fail closed.
- **every escape** — returning the alias, storing it in a struct/tuple/Vec,
  a user method receiver, a non-`frozen` slot, a closure capture. This is the
  half that pays for the non-counting representation, and it is unchanged:
  the alias is in the frozen set, so the same whitelist judges it. Its
  diagnostics say "`frozen` **alias** `k`" rather than "parameter", since the
  line to change is the `let`, not the signature.

### The auto-par surface needed no change — but the reason is not the obvious one

`spans_rooted_at` whitelists places rooted at a `frozen` *parameter*, and an
alias is rooted at itself, so it is **not** in that whitelist. Measured
anyway: a loop body that binds a child off a frozen root gates `proven`, and
the byte-identical body written `ref` still declines.

The reason is that the gate's span sweep records no cross-task-unsafe entry
for the alias at all — in the probed body the only unsafe span is the bare
`s`, which the root whitelist does cover. So the admission rests entirely on
codegen emitting no traffic for the alias, which the IR test above pins
directly and which was confirmed on the emitted worker itself:
`__karac_disjoint_worker_*` for that shape contains zero `rc_inc` / `rc_dec` /
`atomicrmw`. `frozen_param_names` is left parameter-only deliberately — the
alias admission lives in the ownership pass, which the concurrency analysis
does not receive, and re-deriving it there would be a second opinion about
what "frozen" means. Pinned by
`test_disjoint_write_admitted_for_frozen_alias_binding`, with the `ref`
control.

### What it buys

Not a new parallel win — the same traversal was already expressible with
inline projections, and stage 2 measured that at 3.7x. What stage 2.5 adds is
that the *natural* spelling compiles: a walk that binds the node it is about
to recurse into, which is how a person writes one and how every iterative
traversal must.

Timed anyway, since a new binding class could plausibly cost something.
Four `par` branches x 400 repeats over a 2^16-node tree, 4 cores, linear
scaling verified first (200 → 400 repeats: 0.113s → 0.233s, 2.06x), three runs
each, identical answers:

| spelling | real | user |
|---|---|---|
| `let k = n.kids[i]; sum(k)` | **0.221s** | 0.80s |
| `sum(n.kids[i])` | 0.352s | 1.26s |

So the alias form did not cost anything — it measured *faster*, by ~1.6x.
Filed as its own observation (B-2026-08-06-30) rather than claimed as a
benefit of this stage, which was the right call: **the alias form was never
faster, and the gap is now closed in both directions** (0.219s/0.804u vs
0.217s/0.789u).

The 1.6x was the auto-par cost above, and this entry's reading of it was
backwards. It recorded "`sum`'s emitted body is larger for the alias spelling
(81 IR lines vs 42), so this is an optimizer interaction downstream of
codegen." The slower spelling's `sum` is *smaller* precisely **because** its
loop body was outlined into a worker fn — the missing 39 lines were the cost,
not the absence of one. The alias spelling escaped only because its `let`
makes the analyzer not recognise the loop as a reduction at all
(`loop_reductions: []`), so it was never outlined. Two spellings, one of which
accidentally avoided a defect.

The lesson worth keeping: **an IR line count is not a cost**, because
lowerings move code out of the frame you are counting. The count said "more
code is faster", which reads as an optimizer mystery, and stayed a mystery
until the emitted binaries were compared for *symbols*
(`__karac_reduce_worker_2` present in one, absent in the other) rather than
for size.

## Stage 2.6: `for k in <frozen place>` (landed) — ownership-only

Stage 2.5 listed `for k in n.kids` among the shapes still refused "each for a
stated reason rather than by omission", and gave the reason as *"the loop
lowers an element copy, not an alias"*. **That reason was asserted without
being measured, and it is wrong.** The emitted loop is

```llvm
%for.v.elem = load ptr, ptr %for.v.elem.ptr   ; the handle, out of the Vec
store ptr %for.v.elem, ptr %k                 ; into the loop variable's slot
```

— a pointer load and a store, with no clone, no retain, no release and no
cleanup at `for.exit`, in borrow mode and `frozen` mode alike:

| body | frozen | `ref` |
|---|---|---|
| `for k in n.kids { s = s + k.v }` | 0 / 0 | **0 / 0** |
| `for k in n.kids { s = s + g(k) }` | 0 / 0 | **0 / 0** |
| `for k in n.nums { s = s + k }` | 0 / 0 | **0 / 0** |
| `for k in n.kids { let j = k; … }` | 0 / 0 | 2 / 3 |

There was never any traffic here to remove; only the ownership walk was
refusing. So stage 2.6 is **one admission arm in `frozen_escape.rs` and zero
codegen change**.

Two element cases, differing in what the loop variable becomes: a non-generic
`shared struct` element makes it a frozen root, judged thereafter by the same
whitelist as a `let` alias; a **scalar** element leaves it an ordinary
binding, since a register copy of an `i64` has nothing to escape with.
Everything else falls through and reports — a `Map`/`Set`, an element that is
neither a handle nor a scalar, a destructuring pattern, the whole surface
inside a closure. Classification runs *before* the iterable is consumed, so a
rejection walks it exactly once rather than reporting index operands twice.

**The IR test's limit is stated in the test.** Because `ref` is also `(0, 0)`
for a plain `for`, that column cannot discriminate here. The test's final row
— a `let` alias *of* a loop element, where frozen is `(0, 0)` and `ref` is
`(2, 3)` — is the only leg that does, and it is what keeps the zeros from
passing on a build that emits no refcount traffic anywhere.

**Auto-par is untouched, and the reason is the loop form.** `karac query
concurrency` reports `loop_reductions: []` for a container-iterating `for` in
*both* modes, while the same body written `for j in 0..64` over an indexed
element is recognized and fans out. Container iteration is simply not an
auto-par candidate shape today; the two modes agree, so there is no asymmetry
to fix.

## Stage 2.7: `frozen self` and method calls (landed) — and a hole in its own first draft

The last spelling gap. A method call on a frozen place is admitted exactly
when the resolved declaration says `frozen self`:

```kara
impl Node {
    fn total(frozen self) -> i64 {
        let mut s: i64 = self.val;
        for k in self.kids { s = s + k.total(); }
        return s;
    }
}
```

The two halves are one slice because neither is sound alone. Admitting
`place.m()` requires the callee's **body** to be checked — a `ref self` method
may store or return `self`, and doing that with a non-counting handle is a
use-after-free — and only a declared receiver mode gets it checked.

**The measurement is why the rule keys on the declared mode, not on the IR.**
A `ref self` receiver on a `shared struct` is `(0, 0)` in the caller's frame,
the callee's frame, and a nested `ref self` call. `frozen self` lowers to `ref
self`, so the two are *indistinguishable in the emitted code* — including the
one that must be refused. The par_codegen test asserts both zeros deliberately
and says so; its discriminating leg is an alias bound off the receiver
(frozen `(0, 0)` vs ref `(2, 3)`).

### The hole, recorded because it is the kind that ships

`self` is its own `ExprKind::SelfValue`, **not** an `Identifier` whose name
happens to be `"self"`. The first draft's `frozen_place_root` had no arm for
it, so the walk never judged a receiver at all: `frozen self` parsed,
type-checked, ownership-checked clean — and protected nothing. Every
accept-row test written for it passed **vacuously**, because the pass was not
looking at `self`.

It was found by probing the one case whose expected answer was "must report":
a closure capturing `self`. That row is now the regression pin, and it is
mutation-checked — stubbing the `SelfValue` arm to `None` turns it red.

The lesson is the same one this document keeps re-learning from the other
direction: **an accept-only battery cannot distinguish "admitted" from "never
examined."** Every stage here needs at least one row whose expected answer is
a rejection.

### Implementation

`Function::self_is_frozen: bool` beside the receiver rather than a fourth
`SelfParam` variant — stage 1's call, for stage 1's reason: a new variant
would have to be handled at every one of ~140 `self_param` sites, including
backends that must never see the mode. Ten construction sites needed the
field; trait methods get `false` (stage 2.7 is impl-only, and a
trait-dispatched call has no single declaration to check anyway).

`frozen` stays contextual, disambiguated by one token of lookahead, so `fn
frozen(ref self)` is still a method named `frozen`. Method resolution needs no
typechecker callee map: the pass already computes the receiver place's type,
so `(type, method)` names exactly one declaration — keyed by the pair, never
by method name alone, so two types with a same-named method cannot borrow each
other's guarantee. The freeze-site check covers the receiver through the *same*
classifier the parameter uses, with the impl target as its type.

### Known limit, measured and not introduced here

Passing `self` to a borrow parameter is a type error — `frz(self)`,
`byref(self)` from `ref self`, and from owned `self`, all report `expected
'ref Inner', found 'Inner'`. The typechecker types `self` as `T` regardless of
receiver mode. Filed as B-2026-08-07-8. Method-to-method composition — the
path the OO traversal takes — is unaffected.

### Why this matters for stage 3

Stage 3 below sizes the freeze *statement* as codegen work, because it needs
"a binding class whose slot aliases an existing owner without retaining".
Stage 2.5 **is** that binding class, built for the case where the owner is
known by construction. What stage 3 still has to add is the harder half: a
frozen local whose owner is an ordinary local in the same frame, where the
owner's lifetime is a real question rather than a given.

## Stage 3a: the `freeze` statement (landed) — the binding class already existed

```kara
let root = build();
let g = freeze root;
par { sum(g); sum(g); }
```

The section below sized this as codegen work: "a frozen local has to be a
genuinely non-counting binding, which needs a codegen binding class whose slot
aliases an existing owner without retaining." **Stage 2.5 built that class**,
for the case where the owner is the caller's value. Stage 3a reuses it
unchanged for the case where the owner is a local in the same frame —
measured, `let g = freeze t` takes `rc_inc = 0` and registers no cleanup, so
the function carries only `t`'s own release, while the plain rebind `let g =
t` pays for two owners.

So the freeze statement needed **no codegen change either**. That is the third
stage in a row this has been true of, and the pattern is worth naming: each
time, the doc's cost estimate was written before the enabling measurement, and
each time the measurement was cheaper than the estimate.

### Two names come out frozen

The handle, and the **source's place root**. The design says "`graph` stays
usable read-only; freezing does not consume it" — and that second half is not
a courtesy, it is what pays for the non-counting alias. Once the root is in the
frozen set, the ordinary whitelist refuses to move, return, or capture it, so
the owner whose refcount the handle is skipping cannot go away while the handle
is live. Without it the alias would dangle the moment the source was consumed.
The source gets its own diagnostic noun — the fourth — because the line to
change is the `freeze`, not a signature or a `let`.

### Every refusal reports

A `freeze` this pass will not honour is an error, never a silent downgrade to
an ordinary binding: a temporary operand, a `mut` binding, a destructuring
pattern, a `freeze` inside a closure body. The reason is a failure this stage
hit **twice in its own construction**:

1. `check_frozen_param_escape` early-returns for a function with no `frozen`
   parameter — which is `main`, where a freeze statement most naturally lives.
   Every negative probe "passed".
2. An unsupported operand shape fell through to the ordinary walk, where the
   source was not yet frozen, so nothing reported and the `freeze` quietly did
   nothing.

Both were caught by probing the cases whose expected answer was "must report",
and both are now pinned. Combined with stage 2.7's `SelfValue` hole, that is
three vacuous-pass bugs in three stages, all of the same shape: **the accept
rows cannot tell you the check ran.**

### `karac fmt` was deleting the keyword

Found by running it, not by reasoning: the first cut rewrote `let g = freeze
t;` to `let g = t;`. That is not cosmetic — it removes the guarantee and turns
a non-counting binding into a counted one. The keyword lives on
`Program::freeze_spans` (a parser-set side table, so no new `ExprKind` and no
walk churn), which means `format_expr` can never see it and the `let` printer
is the only thing that can emit it.

### What stage 3a does NOT close

#133, unchanged: `Node.neighbors` is `mut`, so it is refused at the freeze site
whether the freeze is spelled as a parameter mode or as a statement. What the
statement buys is that the region is now **explicit and local**, which is the
precondition the section below gives for a per-instance check ever being
cheaper than mechanism 1 — not the check itself.

## Stage 3: the obvious route is refuted, and this sizes the work

Stage 3 needs the **`freeze` statement**, not just a relaxed E0512. With
`frozen` available only as a *parameter* mode, "is this instance immutable for
the region" is a question about every call site — whole-program analysis, i.e.
mechanism 1, the thing this design exists to avoid. The statement makes the
region explicit and the check local, exactly as § "The call" describes.

**The cheap way to add it does not work.** Stage 1's winning move was to keep
the mode out of the type tree and reuse the borrow vocabulary: `frozen T`
lowers to `ref T`, and codegen emits no refcount traffic for a borrow. The
obvious extension is to desugar `let g = freeze graph;` into a `ref`-typed
local plus a side record, which would need no new `ExprKind` and no
exhaustive-match churn.

Measured, refcount traffic counted inside the enclosing function only:

| binding form | `rc_inc` | `rc_dec` |
|---|---|---|
| `let r = g;` — plain | 4 | 6 |
| `let r: ref N = g;` — `ref`-typed local | **4** | **6** |

**Identical.** A `ref`-typed *local* is not a borrow at codegen level; it still
materialises a counted handle. The parameter case works because `frozen T` →
`ref T` rides the **calling convention** — the caller keeps ownership and the
callee's slot holds a pointer-to-handle. A local has no caller to borrow from,
so the annotation buys nothing.

`let r: ref N = g;` does typecheck, build, run, and pass to a `frozen`
parameter today — so the route looks viable right up until the traffic is
counted. That is why it is recorded here.

**What stage 3 therefore costs.** A frozen local has to be a genuinely
non-counting binding, which is codegen work, not a desugar: codegen needs a
binding class whose slot aliases an existing owner without retaining. That is
the same capability mechanism 1 would need, which is worth noticing — the
statement is not a way of avoiding that work, only of making the *check* local
once the work exists.

Two guards must move together when it lands, and they are deliberately
independent (§ "Two guards"): the freeze-site check (E0512) refuses any type
whose reachable closure holds a `mut` field, and stage 2's place walk refuses a
`mut` field at every projection step. #133 needs both relaxed — `Node.neighbors`
is `mut` *and* is the traversal path — so neither can be relaxed alone.

## Risks, stated plainly

- **Non-counting handles are a new unsafety surface.** If escape checking has a
  hole, the symptom is use-after-free. This is the same exhaustiveness class
  that produced three bugs in a day, so the escape check must be written
  fail-closed and no-wildcard, the way `region_bindings` was after
  B-2026-07-04-13 — and stage 1 exists to shake it out before stickiness
  multiplies the surface.
- ~~**Stage 2 touches the type checker broadly.** Mode propagation through
  projection is not a contained edit.~~ **Wrong, and worth recording as
  wrong.** Stage 2 landed entirely inside `frozen_escape.rs`: the mode does
  not propagate through the *type* at all, it is re-derived per place by
  walking `struct_info` one projection at a time. The type checker was not
  touched, `TypeKind::Frozen` is still never constructed, and the retained
  ~16 walk arms were still not needed. The generalisation that made it small
  is that a place's frozen-ness is a property of its **root**, which is
  already how the auto-par arm keyed its whitelist. Stage 3 (per-instance
  freeze) may still want a type-level mode; stage 2 did not.
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
