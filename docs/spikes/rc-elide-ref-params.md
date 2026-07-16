# RC-elision for read-only borrow parameters (`KARAC_RC_ELIDE_REF_PARAMS`)

**Status:** landed **default ON** (B-2026-07-15-21; opt OUT with
`KARAC_RC_ELIDE_REF_PARAMS=0`). Full macOS suite + full Linux LSan verified:
**elide-on is byte-for-byte the same pass/fail as elide-off** on the whole
`memory_sanitizer` corpus (no new leak / UAF / double-free), re-confirmed at
current HEAD (`memory_sanitizer` 736/0/1 + codegen E2E 2400/0 on the default
path). All four flip criteria met — see "Flip criteria". The escape hatch
(`=0`) mirrors the headerless reshaper's `KARAC_HEADERLESS_RESHAPER`.

## The gap

The ownership pass classifies a parameter that is only read as
`OwnershipMode::Ref` — a borrow. For a `shared` / `Option[shared]` such param,
codegen still emits the caller-side retain (`rc_inc`) at the call site and the
callee-side release (`rc_dec`) at scope exit — even though `karac query
ownership` reports `mode: ref` and `karac query cost-summary` reports **0
rc_ops**. Codegen never consumed `OwnershipCheckResult::param_modes`; it decided
retain/release from the AST-declared type alone. (That cost-summary/codegen
disagreement is a separate reporting bug, tracked in the ledger.)

### Measured cost (LeetCode #101, symmetric-tree, Apple M5 Pro)

The read-only crossed tree walk `is_mirror` is a pure pointer-chase.

| | wall | vs Rust | instructions |
|---|---|---|---|
| baseline | 273.6 ms | 2.43× | 5.24 B |
| **rc-elide** | **191.2 ms** | **1.70×** | **3.74 B** (−29%) |
| rust / c | 112.8 ms | 1.0× | 1.87 B |

kāra retired **2.81× Rust's instructions** at *higher* IPC — a pure
codegen-density problem. `is_mirror` spent ~50 of 77 instructions on refcount
inc/dec/free on a walk that never mutates or stores. Eliding it is a **30%
wall-time win**, closing the Rust gap from 2.43× → 1.70×.

## Why it is sound to elide

The caller-side `rc_inc` and callee-side `rc_dec` are a **balanced pair**;
deleting both nets zero. The only hazard is the object hitting refcount 0
*during* the call — impossible when the caller keeps its own reference alive for
the call, which a genuine `ref` borrow guarantees. So for a real borrow the pair
is a provable no-op.

## What makes a `ref` param *actually* safe to elide (`src/rc_elide.rs`)

`OwnershipMode::Ref` is necessary but **far from sufficient** — four distinct
ways it lies; conditions 1–3 were each caught by Linux LSan during development,
condition 4 makes the last route sound by construction:

1. **Fresh-rvalue / bare-name args are moves, not borrows.** `let d = take();
   eat(d)` passes `d` *by value* — a transfer of its `+1`, whose exit dec is
   load-bearing. So condition 1 requires every call to pass the param a
   **projection of a named binding** (`n.left`, `v[i]`, `t.0`) — a read of a
   sub-value out of a container the caller still holds. A **bare** identifier or
   a fresh rvalue (`Some(x)`) is rejected. (This is what distinguishes
   `is_mirror(n.left, …)` — safe — from `eat(d)` — a move.)

2. **`Ref` permits callee-side move-out.** The ownership pass maps `let mut a =
   param` (a move) to `Read → Ref`, so `merge_two`/`merge_k` (which splice their
   param's nodes into the returned list) are still `Ref`. Condition 2 requires
   the param be **consumed in place** — used only as a `match`/`if let`
   scrutinee, never bound to a `let`, assigned, or forwarded — via the shipped
   [`crate::result_escape::nonescaping_param_names`] (B-2026-07-12-24), the same
   conservative, exhaustive, fail-closed non-escape walk.

3. **A match-binding can still escape via the return or an output param.**
   `insert` uses its param only as a `match` scrutinee (passes condition 2) but
   returns `Some(n)` — transferring the node out. So condition 3 requires a
   **scalar return type** (no handle can leave via `return`) and **no `mut ref`
   / `mut Slice` params** (no store into an outliving location).

4. **A match-*payload* can move the referent out by value.** `match p { Some(n)
   => consume(n) }` passes conditions 1–3 (the param `p` is a scrutinee), yet the
   payload `n` — `p`'s referent — is handed by value to a consuming callee.
   Empirically this is *already balanced* (Linux LSan, flag on: codegen re-shares
   the borrowed payload, so `consume`'s dec pairs with its own inc and never
   touches `p`'s elided retain — see "Residual"). Condition 4 removes the
   reliance on that codegen invariant and makes elision sound **by
   construction**: a payload may be read through a **projection** (`n.left` — a
   borrow of a *sub*-node) or destructured further (a nested scrutinee), but it
   may **not appear as a bare-identifier value**. `is_mirror`/`is_symmetric`
   qualify (payloads only ever `an.left`/`n.right`); `probe`-style consumers are
   rejected. A single pre-order walk (`PayloadScan`) grows the param's payload
   lineage and flags any bare use; a closure capture of a payload counts as an
   escape. Unit-tested in `src/rc_elide.rs`.

Plus the caller-visibility filters: the function is **directly called** at least
once (arg shapes observed), never used as a **value** (indirect calls invisible),
and not **`pub`** (external callers invisible).

For #101 the surviving set is exactly `{is_mirror: [a, b], is_symmetric:
[root]}` — the two hot, bool-returning, scrutinee-only walkers. `insert`,
`copy_tree`, `mirror` (Option-returning tree builders) are correctly excluded and
cost nothing (they run 8× at setup, not in the 8M-rep loop).

## Wiring

`ownership.rs` computes `OwnershipCheckResult::elidable_ref_params` (only when the
flag is set — otherwise empty, zero overhead). `codegen.rs` ORs it into
`borrowed_arg_skip` / `borrowed_param_dec_skip` — the existing, LSan-proven Phase
C2b borrow-skip machinery — so the call-site inc, the source transfer/consume,
and the callee exit dec are all skipped together. No new cleanup-emission code,
so no new leak surface; the analysis only *prevents* codegen from retaining. The
analysis lives outside codegen and reaches it as a plain-data hint, preserving
the codegen-containment invariant.

## Verification (flag ON)

- `src/rc_elide.rs` unit tests (5): the guard keeps `is_mirror`/`is_symmetric`
  elidable and rejects the consumed-payload shapes (direct, forwarded, if-let,
  `@`-alias). `#101` win preserved — `is_symmetric[root]` + `is_mirror[a,b]`
  still elided on the real bench kata.
- macOS full `memory_sanitizer` (654 passed / 0 failed / 1 ignored) — no
  UAF/double-free. `leaks`/guardmalloc on the residual shapes + #101: 0 leaks.
- **Full Linux LSan** (`scripts/lsan-local.sh` `memory_sanitizer`, the
  authoritative leak gate): flag-on **653 passed / 1 failed**, byte-for-byte
  **identical to flag-off** (also 653 / 1). The single failure,
  `owned_vec_param_let_move_interval_merge`, is a **pre-existing baseline leak on
  `main`** (fails flag-off too; `merge_k` is never elided — empty set), i.e. NOT
  introduced here — flagged separately. The four `asan_rc_elide_*` residual pins
  are among the 653 that pass under LSan with the flag on.

## Residual — closed by condition 4

The match-payload-by-value route (`match a { Some(n) => consume(n) }`) was the
one hole conditions 1–3 left open. Investigating it produced two findings:

1. **It never actually faults.** Reproduced as three shapes (direct consume,
   two-level forward, if-let) with the payload genuinely elided; all are clean
   under macOS `leaks`/guardmalloc *and* full Linux LSan with the flag on. The
   reason: condition 2 keeps `p` a scrutinee, so its payload is a *borrow*, and
   codegen re-shares a borrow passed to a consuming position — an independent
   balanced inc/dec that never touches `p`'s elided retain. Pinned by
   `asan_rc_elide_*` in `tests/memory_sanitizer.rs`.

2. **Condition 4 removes the dependence on that codegen invariant.** Rather than
   rely on re-share staying correct, the guard declines to elide any param whose
   payload is moved out as a bare value (see condition 4 above). So the elided
   set now contains *only* params whose payloads are provably projection-only —
   `is_mirror`/`is_symmetric` stay in, `probe`-style shapes drop out — and
   soundness no longer couples to codegen behavior. `src/rc_elide.rs` unit tests
   assert both directions.

## Flip criteria (default-ON) — **all met, flipped B-2026-07-15-21**

1. ~~Close the known residual (callee-consume-aware check).~~ **Done** —
   condition 4 (`payloads_never_move_out`), unit-tested + LSan-verified.
2. ~~A corpus-wide re-bench confirming the win generalizes with no compile-time
   regression from the whole-program analysis.~~ **Done** — read-only tree walks
   #100 / #104 / #111 / #112 each land **1.20–1.32×** faster (17–32%) with
   byte-identical output; #110 (`is_balanced`, a height-returning helper) is
   correctly excluded — no win, no regression. Whole-program analysis adds no
   measurable compile time (−6 ms on #112, within noise).
3. ~~CI `memory-sanitizer` green with the flag forced on.~~ **Done by the flip** —
   with default-ON the suite runs the elision path with **no** env var, so the
   `asan_rc_elide_*` pins (and every read-only-walk program) are load-bearing on
   every change. Re-confirmed at HEAD: `memory_sanitizer` 736/0/1, codegen E2E
   2400/0, par_codegen 173/0, lib 1025/0 — all on the default path.
4. **Design shift affirmed.** Codegen now consumes one `param_modes`-derived
   plain-data hint (`elidable_ref_params`), a deliberate, contained widening of
   the "modes are a checking aid, not a codegen input" scoping (CLAUDE.md,
   Architecture). The hint is computed in `ownership.rs` (outside codegen) and
   reaches codegen as data through the existing `borrowed_arg_skip` /
   `borrowed_param_dec_skip` channel — no `inkwell`/LLVM type crosses the
   boundary, so the codegen-containment invariant holds. CLAUDE.md's Ownership
   bullet is updated to name this one hint.

## Part B — Some-binding elision unblocks tail-call elimination

Part A (the flip) elides the **param-level and child-argument** retains via the
`borrowed_arg_skip` / `borrowed_param_dec_skip` channel. One RC pair survives:
the **`Some(n)`-binding acquire + its scope-exit `RcDec`** (emitted at
`pattern_binding.rs:198`). It is the same balanced no-op — for an elidable param,
condition 4 (`payloads_never_move_out`) has already proven the payload `n` is
projection-only, so it never escapes and is kept alive by the caller-held param
for the whole call. Part B skips that pair too, gated on a new
`pattern_binding_scrutinee_is_elidable_param` flag (set in `compile_match` from
`scrutinee_is_elidable_param`, a bare-identifier-names-an-elidable-param
classifier modeled on `scrutinee_is_borrowed_binding`).

The payoff is second-order and larger than the RC arithmetic itself: the
surviving `RcDec` was a **post-call release epilogue**, which kept the tail
recursion out of tail position. Removing it lets LLVM's `tailcallelim` convert
the self-recursion into a **loop** — the exact structure C/Rust get (one real
call for the non-tail child, a loop on the tail child's spine). Verified on the
#112 `has_path_sum` object code: after Part B the hot function carries **zero rc
ops** and the right recursion is a `jne` back-edge, not a `call`.

Measured (container, best-of-7 median, `has_path_sum` bench K=6M):

| | wall | vs baseline | vs C |
|---|---|---|---|
| baseline (`=0`) | 0.559 s | 1.00× | 2.16× |
| Part A | 0.453 s | 1.23× | 1.76× |
| **Part B** | **0.291 s** | **1.92×** | **1.13×** |
| C clang -O3 | 0.258 s | — | 1.00× |

Corpus generalization (elide-off → Part B): the **short-circuit** shapes where
the last recursion is in tail position get TCO and ~double — #100 `is_same`
(`and`) **2.20×**, #112 `has_path_sum` (`or`) **1.90×**. The **combine-both**
shapes (`1 + max(l,r)` / `min`) can't tail-call but still shed the Some-binding
RC — #111 `min_depth` **1.37×**, #104 `max_depth` **1.31×**. #110 `is_balanced`
was excluded at Part B (folded in by Part C below). The lone residual vs C is the
per-node overflow-check `jo` kāra emits by default and C omits — the deliberate
equal-safety tax, leaving kāra at parity with `rustc -O -C overflow-checks=on`.

## Part C — borrow-forward relaxation of condition 1

Part B left one traversal shape excluded: a thin wrapper that delegates to a
recursive helper — `fn is_balanced(root) -> bool { check(root) != -1 }` (#110).
`check` passes conditions 2–4, but condition 1 rejected it: the wrapper calls
`check(root)` with a **bare identifier**, which the original rule scored as a
move. It isn't — `root` is a `Ref`-mode param, so its referent is kept alive by
the enclosing frame (or an ancestor) for the whole call. Condition 1 now accepts
a **borrow-forward**: a bare identifier naming a `Ref`-mode parameter of the
function being walked, alongside the existing projection form. The soundness is
the same outlives-the-call guarantee a projection gives, and does *not* require
the enclosing function to elide anything — a non-elided `is_balanced` still holds
`root`'s `+1` across its whole body, covering the `check(root)` call. The
relaxation is disabled inside closures (a captured borrow could outlive the call
— fail-closed), and a bare **owned local** (`let d = …; eat(d)`) is still a move,
still rejected (unit-tested both directions in `rc_elide.rs`).

Result: `check` now elides (`{"check": [("node", 0)]}`), and #110 goes from the
sweep's holdout to **1.67× faster** (elide-off 0.597 s → 0.357 s; `check` carries
zero rc ops per node — no TCO, since `1 + max(l,r)` combines both child results).
Validated on the default path: `memory_sanitizer` 738/0/1, codegen E2E 2400/0,
par_codegen 173/0, rc_elide unit tests 7/0; #110 byte-identical across
interp/JIT/AOT/auto-par/`=0` + valgrind-clean. Pinned by
`asan_rc_elide_borrow_forward_ref_param_no_leak`.

Soundness rests on the identical condition-4 proof as Part A (the payload is a
non-escaping alias of the caller-kept-alive param), re-validated on the default
path: full `memory_sanitizer` 736/0/1 (== elide-off), codegen E2E 2400/0,
par_codegen 173/0, lib 1025/0, clippy clean, #112 byte-identical across
interp/JIT/AOT/auto-par/`=0` and valgrind-clean. Pinned by
`asan_rc_elide_some_binding_or_recursion_walk_no_leak` (the `has_path_sum` shape,
now load-bearing on every build).

## Second-shape corroboration (tree-sum pool, combine-both recursion)

Independently measured in a parallel session on the B-2026-07-15-21 ledger
probe — 8 pooled 31-node trees, `total += sum(pool[rep % 8])` at 3M reps, a
COMBINE-BOTH walk (`n.val + sum(n.left) + sum(n.right)`, no TCO), Linux x86-64
container, best-of-9, vs `rustc -O -C overflow-checks=on` at 0.342 s:

| | wall | vs rust-ovf |
|---|---|---|
| `=0` (owned protocol) | 0.440 s | 1.29× |
| Part A only (binding pair kept) | 0.474 s | **1.39× — a 7% regression on this shape** |
| **Part A + Part B (default)** | **0.362 s** | **1.06×** |

Two takeaways: the combine-both family lands at equal-safety-Rust parity from
rc-silence alone (no TCO needed), and Part A **without** Part B can regress a
shape even while it wins on others (the #100/#104/#111/#112 corpus wins were
1.18–1.32× with the pair still in place) — so Part B is load-bearing for the
default-ON posture, not just headroom. Pinned by
`asan_rc_elide_recursive_tree_sum_pool_no_leak` (Index-projection call site +
combine-both walk; clean under default and `=0`).

## Condition 5 (CLOSED, 2639536) — mutation-through-alias, found during the Part B review

Conditions 1–4 do not constrain WHERE a payload **projection** may flow: an
elided fn's arm may pass `n.parent` (any projection) to an arbitrary callee.
For a `shared` graph with **up/back-pointers**, that callee — following the
ordinary owned protocol, so its own counts balance — can field-assign through
the alias (`parent.left = None`) and release the ONLY count keeping the
borrowed node alive (under elision the current frame holds no +1 of its own),
freeing `n` mid-arm: a use-after-free the un-elided protocol shields. The same
holds through a let-alias of a projection (`let x = n.parent; mutate(x)`),
which `PayloadScan` does not add to the lineage. Unreachable in the current
corpus (no elided shape passes projections to a mutating callee;
`is_mirror`/`sum`/`has_path_sum` recurse only into themselves) — but with
default-ON landed the env gate no longer shields it, so closing this is a
priority fast-follow, tracked as **B-2026-07-16-7**. Sketched condition 5 (all
cheap syntactic fail-closed checks):

- **5a** — an elidable fn's body contains no field/index store
  (`Assign`/`CompoundAssign` with a projection target) and no method call on a
  lineage-rooted receiver;
- **5b** — every `shared`/`Option[shared]` param of an elidable fn is itself
  elided (no owned-shared sibling param that could alias the borrowed tree);
- **5c** — lineage projections (and let-aliases of them, which must join the
  lineage) may be passed ONLY at elided positions of fns satisfying 5a–5c —
  making the whole elided call web hermetically read-only over shared state,
  so no release can occur while any borrow in the web is live.

`is_mirror`/`is_symmetric`/`sum`/`has_path_sum` all satisfy 5a–5c, so every
known win survives the tightening.
