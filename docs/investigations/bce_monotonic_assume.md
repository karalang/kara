# Bounds-check elision via `llvm.assume` on monotone loop variables

**Date:** 2026-06-07 · **Status:** mechanism validated in isolated IR; the soundness prerequisite (AOT integer-overflow trapping) **landed the same day** — assume emission is now unblocked, and more valuable than measured below: the `with.overflow` lowering's extractvalue blocks LVI phi-range reasoning, so one previously-LLVM-folded check per kata resurfaced (see the trap entry's bench probe in `implementation_checklist/phase-7-codegen.md`).

Companion to the promoted tracker entry `phase-7-codegen.md` § "Bounds-check elision: transitive bounds from induction-variable monotonicity". This note records the per-kata diagnostic and the `opt` experiments so the slice doesn't have to re-derive them.

## Diagnostic: which checks actually survive today

Disassembly of the kata bench binaries (karac installed 2026-06-07, `otool -tv`, hot loops inlined into `_main`):

**Kata #26 (remove-duplicates) — 1 of 3 checks survives.**

| Access | Check status | Why |
|---|---|---|
| `nums[i]` | folded | trip count provably equals the vec len post-inline (SCEV) |
| `nums[k−1]` | folded | LLVM combined cross-iteration facts: `k ≥ 1` monotone + previous iteration's `nums[k]` check |
| `nums[k]` | **survives** (`cmp x8, x22; b.hi`, per kept element) | needs the relational invariant `k ≤ i` — `k` is conditionally incremented, so its phi is not an AddRec |

**Kata #88 (merge-sorted-array) — 3 sites survive.**

| Access | Check status | Why |
|---|---|---|
| `nums2[j]` (both reads) | folded | `j ≥ 0` source guard is the loop exit; upper from monotone decrement off `n−1` |
| `nums1[i]` lower | folded | `i >= 0` source guard becomes the `tbnz` |
| `nums1[i]` upper | **survives** | `i ∈ [0, m−1]` is true but `i` decrements *conditionally* → not an AddRec; SCEV can't bound it, LVI goes overdefined on the self-referential phi |
| `nums1[k]` (both branches) | **survives** | `k ∈ [0, m+n−1]` needs the relational invariant `k = i + j + 1` |

**Root-cause class:** every surviving check guards a *conditionally-updated monotone* variable. Those phis are not AddRecs (SCEV gives up) and are self-referential (LVI widens to overdefined). LLVM legitimately cannot fold them with any pass ordering — this is not the v68 JumpThreading conservatism (panic sites are already outlined `cold noinline noreturn`; the v68 blocker is gone, and these checks still survive).

## Experiments (LLVM 18.1.8 `opt -passes='default<O2>'`, arm64-apple)

Reproducer: kata-88's post-inline merge-loop shape — three-phi header (`k` from 1999999, `i` from 999999, `j` from 999999), conditional decrements, `icmp uge … 2000000` checks branching to an outlined cold panic fn. Baseline: **all 3 checks survive O2**, matching the real binary.

- **Experiment A** — insert `%le = icmp sle i64 %i, 999999; call void @llvm.assume(i1 %le)` in the loop body (the fact karac can prove syntactically: monotone non-increasing from init). Result: **`nums1[i]`'s check folds** (CVP completes with the dominating `i >= 0` guard edge).
- **Experiment B** — additionally assume `k ≤ 1999999` and `k ≥ 0` (the facts a relational analysis would supply). Result: **all checks fold**, and `grep -c llvm.assume` on the optimized IR = **0** — the assumes are fully consumed; zero runtime residue on this shape.

Conclusion: **assume-the-monotone-range + CVP works where v68's assume-the-check-predicate + JumpThreading failed.** karac supplies the monotonicity fact it can prove syntactically; LLVM completes the proof with its post-inline constant knowledge (the `m`/`n`/`len` values karac cannot see intra-procedurally). Neither side can do it alone.

## Soundness gate

`assume(x <= init)` for a decreasing variable is sound only if the update cannot wrap: with today's AOT silent-wrap arithmetic (verified 2026-06-07: `i64::MAX + 1` wraps to MIN in AOT while `karac run` traps "integer overflow"), an underflowing `x = x - c` produces a huge positive value, the assume is violated, and the optimizer is licensed to miscompile — the pass would *inject* UB into a program whose behavior today is defined. Under design.md § Arithmetic Overflow (traps in app/lib), the wrap is unreachable on all defined executions and the assume is unconditionally sound. Hence the implementation order: AOT trapping arithmetic first, monotone assumes second.

## Repro commands

```bash
opt -passes='default<O2>' merge_base.ll -S -o out.ll && grep -c 'icmp ugt' out.ll
```

The `.ll` sources are small enough to reconstruct from the table above (three-phi loop, two checks, cold panic callee); the experiment is deterministic on LLVM 18.

## Midpoint idiom — the binary-search extension (landed 2026-06-16)

**Surfaced by kata #34** (find-first-and-last-position). The monotone tier above
does *not* reach the canonical binary search

```kara
while lo < hi { let mid = lo + (hi - lo) / 2; … nums[mid] … }
```

because the surviving check is on the **derived** `mid`, and folding `mid < len`
needs the **relational** invariant `mid < hi` (correlating `lo` and `hi` inside
`mid`'s definition). LLVM's CVP/LVI is interval-based — it bounds `mid`
componentwise as `[lo_min + div_min, lo_max + div_max]`, which cannot prove
`mid < hi`; the `mid = extractvalue(sadd.with.overflow …)` value is additionally
opaque to its range pass. Diagnosis on the real kata IR (`otool`/`opt`, LLVM 18):
the `nums[mid]` bounds check (`icmp ult mid, 4096`) survives `default<O2>`, and
kata #34 lands the widest equal-safety gap of the binary-search katas — **298 ms
vs equal-safety Rust 189 ms (1.58×)**, with the overflow tax *zero* here
(`rustc -O` == `-C overflow-checks=on`), so it is a pure codegen gap. `get_unchecked`
(no check) reaches 205 ms, pinning the bounds check as ~85 % of the gap.

**Experiments (LLVM 18.1.8 `opt`, real post-inline kata IR).** Injecting facts as
`llvm.assume` before the check:

- `assume(lo >= 0) ∧ assume(hi <= len)` (the monotone facts) — **does not fold**
  (the check is on `mid`, disconnected from `lo`/`hi` through the extractvalue).
- `assume(mid >= lo) ∧ assume(mid < hi)` — **folds** (`panic_site → 0`). The two
  *relational* facts alone suffice; no absolute monotone bound is needed.

**Mechanism (landed).** When codegen lowers a strict `while lo < hi` (two bare
identifiers) and a body `let mid = lo + (hi - lo) / 2` (or `(lo + hi) / 2`, and
the trait-lowered `i64.add/sub/div` forms) binds the midpoint, it emits
`assume(mid >= lo)` + `assume(mid < hi)` at the binding site —
`src/codegen/control_flow_bce.rs § midpoint` (`binsearch_guard_pair`,
`expr_is_midpoint`, `try_emit_binsearch_midpoint_assumes`), wired through a
`binsearch_guard_stack` pushed/popped in `compile_while`.

**Soundness** is *local* — no whole-loop monotonicity analysis. Under the
dominating `lo < hi` (so `d = hi - lo >= 1`), signed floor `(hi-lo)/2 ∈ [0, d-1]`,
hence `lo <= mid <= hi - 1 < hi`. AOT overflow trapping makes any wrapping
`hi - lo` / `lo + …` panic before the wrapped value exists, so the facts hold on
every defined execution (same gate as the monotone tier). Emitted at the binding
site, where `lo`/`hi` still hold the values `mid` was derived from, so later
mutation cannot retroactively falsify them. The strict guard is load-bearing: a
`lo <= hi` guard admits `lo == hi → mid == hi`, so the closed-interval style is
deliberately out of scope.

**Phase-ordering caveat.** A single `default<Ox>` does *not* fold even with the
assumes co-resident post-inline (callee-optimize-then-inline ordering); a second
pipeline run does (`3 → 0`, verified). Codegen therefore runs one extra
`default<O1>` pass **gated on emission** (`binsearch_assume_emitted`) — the baked
prelude has no midpoint binary search, so non-binary-search programs never pay it
(verified: the flag never sets for a `spawn`-only module).

**Result.** Kata #34 ★ two-bounds search: **298 ms → ~215 ms** (the residual gap
to Rust is one guard-proven `nums[lo]` check in `main`, a separate len-decoupling
issue, plus measurement noise). Regression-guarded by
`tests/codegen.rs::{test_ir_binsearch_midpoint_emits_assumes,
test_ir_non_midpoint_binding_no_binsearch_assume,
test_e2e_binsearch_midpoint_assume_is_sound}`. Bug ledger: `B-2026-06-16-1`.

## Two-pointer idiom — the sliding-window extension (OPEN, kata #76)

**Surfaced by kata #76** (minimum-window-substring), the M5 re-bench. The
canonical two-pointer sliding window

```kara
while r < n {
    let cr = s[r]; have[cr] = have[cr] + 1; …
    while formed == required {          // shrink
        let cl = s[l]; have[cl] = have[cl] - 1; …
        l = l + 1
    }
    r = r + 1
}
```

is the same *conditionally-updated monotone* class as the surviving `nums[k]` /
`nums1[i]` checks in the Diagnostic table above — now the whole hot loop rather
than one tail access. `l` (the window-left pointer) is incremented **only inside**
the conditional `while formed == required` shrink loop, so its phi is not an
AddRec and is self-referential: SCEV gives up, LVI widens to overdefined, and
LLVM cannot derive the **relational** invariant `l ≤ r < n` on any pass ordering.
So the `s[l]` bounds check survives. (`s[r]`, with `r` unconditionally `++`'d,
*may* also survive if the `let n = s.len()` capture blocks the `AddRec == len`
proof — to be confirmed on the IR.) The count-table indexes `have[cr]` / `need[cr]`
are **value**-indexed (`cr = s[r]` is in the 4-symbol alphabet `{0,1,2,3}` but
typed as unconstrained `i64`), so they are bounds-checked in **both** kāra and
Rust and are *not* the differentiator — a value-range narrowing is a separate,
secondary opportunity.

**Evidence (not yet IR-confirmed).** kāra is the **slowest of five** compiled
mirrors on the M5 seq lane (go 265 < c 283 < rust 286 < rust_ovf 312 < **kāra
351 ms**): **1.24× behind C**, **1.12× behind equal-safety `rustc -C
overflow-checks=on`** (both overflow-checked, so a pure non-overflow codegen
gap). Instruction counts (`/usr/bin/time -l`, warm): kāra **12.457 B** vs C
7.576 B (1.64×) vs rust_ovf 11.141 B (1.12×); IPC kāra 8.00 is the *highest* of
the three, so the gap is instruction **count** (surviving checks), not
pipelining. Binary/RSS/compile are all at C parity — the gap is purely the hot
loop.

**Fix direction (unimplemented).** Extend the relational-assume machinery in
`src/codegen/control_flow_bce.rs` (the `binsearch_guard_pair` /
`try_emit_binsearch_midpoint_assumes` path that landed the midpoint pair) to the
two-pointer shrink idiom: for a variable `l` conditionally incremented under a
guard inside a loop dominated by `l ≤ r` and `r < n`, emit `assume(l < n)`
(and/or `assume(l ≤ r)`) at the `s[l]` use — gated on emission and soundness-
checked identically (AOT overflow trapping makes the no-wrap precondition hold on
every defined execution). **Confirmation first:** `otool`/`opt`-dump `min_window`'s
post-inline IR (methodology in § Diagnostic), enumerate which of `s[r]` / `s[l]`
/ `have[]` / `need[]` checks survive `default<O2>`, and verify injected assumes
fold them (Experiment A/B style) before coding. Bug ledger: `B-2026-07-10-5`.
