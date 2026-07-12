# RC-elision for read-only borrow parameters (`KARAC_RC_ELIDE_REF_PARAMS`)

**Status:** landed **gated OFF** (env opt-in). Full macOS suite + full Linux
LSan verified: **flag-on is byte-for-byte the same pass/fail as flag-off** on the
whole `memory_sanitizer` corpus (no new leak / UAF / double-free). The default-ON
flip is the owner's call — same pattern as the headerless reshaper
(`KARAC_HEADERLESS_RESHAPER`). See "Flip criteria" and "Known residual".

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

`OwnershipMode::Ref` is necessary but **far from sufficient** — three distinct
ways it lies, each of which Linux LSan caught during development:

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

- macOS `codegen` (2240), `memory_sanitizer` (647), `par_codegen` (159) — all 0
  failed. `leaks`/guardmalloc on #101: 0 leaks. #101 win preserved (3.74 B).
- **Full Linux LSan** (`scripts/lsan-local.sh` `memory_sanitizer`, the
  authoritative leak gate): flag-on **646 passed / 1 failed** — identical to
  flag-off. The single failure, `owned_vec_param_let_move_interval_merge`, is a
  **pre-existing baseline leak on `main`** (fails with the flag off too; my
  elision gives it an empty set), i.e. NOT introduced here — flagged separately.

## Known residual (before default-ON)

A match-binding passed **by value to a consuming callee** — `match a { Some(n) =>
consume(n) }` where `consume` owns and frees `n` — would move `a`'s node out
without tripping conditions 1–3. It does not occur in the LSan corpus (flag-on ==
flag-off), and the flag is off by default, but it is the one route not closed by
the current syntactic guards. Closing it needs a callee-consume-aware
borrowed-set analysis (type + method/callee mode info) — the next slice, required
before flipping the default.

## Flip criteria (default-ON)

1. Close the known residual above (callee-consume-aware check).
2. A corpus-wide re-bench confirming the win generalizes with no compile-time
   regression from the whole-program analysis.
3. CI `memory-sanitizer` green with the flag forced on.
4. Affirm the design shift: codegen consuming a `param_modes`-derived hint widens
   the current "modes are a checking aid, not a codegen input" scoping
   (CLAUDE.md, Architecture). Sound and contained here, but a direction to own.
