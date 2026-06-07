# Bounds-check elision via `llvm.assume` on monotone loop variables

**Date:** 2026-06-07 · **Status:** mechanism validated in isolated IR; implementation blocked on AOT integer-overflow trapping (soundness prerequisite — see `implementation_checklist/phase-7-codegen.md` § AOT integer-overflow trapping).

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
