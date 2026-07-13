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

## Flip criteria (default-ON)

1. ~~Close the known residual (callee-consume-aware check).~~ **Done** —
   condition 4 (`payloads_never_move_out`), unit-tested + LSan-verified.
2. A corpus-wide re-bench confirming the win generalizes with no compile-time
   regression from the whole-program analysis.
3. CI `memory-sanitizer` green with the flag forced on (would make the
   `asan_rc_elide_*` pins load-bearing on every change).
4. Affirm the design shift: codegen consuming a `param_modes`-derived hint widens
   the current "modes are a checking aid, not a codegen input" scoping
   (CLAUDE.md, Architecture). Sound and contained here, but a direction to own.
