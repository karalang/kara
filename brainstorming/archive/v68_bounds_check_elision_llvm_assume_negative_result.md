# v68 — Bounds-check elision: `llvm.assume` doesn't fire across loop back-edges

**Status:** Lesson archived 2026-05-15. Source-level elision shipped (see `src/codegen.rs::compile_vec_index`'s split bounds-check path + `compile_short_circuit`'s fact propagation); this entry preserves *why* the LLVM-side approach was rejected so the next perf investigation doesn't retry it.

**Trigger.** Kata #5 (longest palindromic substring) at 1.21× of Rust — measured the gap to one redundant `cmp x10, x8 ; b.hs panic` per inner-loop iteration. The bounds check fires on the loop's phi result; the loop guard at the loop tail proves the same fact on `phi + step`. Same operands, same predicate (after CSE), different SSA values across the back-edge.

**What I tried.** Three `llvm.assume` calls in `compile_vec_index` after the bounds-check ok-branch:

```rust
call llvm.assume(icmp sge len, 0)      // len is always >= 0 (loaded from Vec header)
call llvm.assume(icmp sge idx, 0)      // bounds check just proved idx u< len, so idx s>= 0
call llvm.assume(icmp slt idx, len)    // and idx s< len in signed form
```

Hypothesis: with all three signed-form facts in scope, LLVM's instcombine / SCEV / IndVarSimplify would propagate across the phi merge and elide the per-iter bounds check. ~30 LoC change, all 915 codegen tests green, fmt + clippy clean.

**Result: zero perf movement.** Five hyperfine runs of 15 measurements each, baseline 45.7 ± 2.4 ms vs assume-version 46.0 ± 2.6 ms — within noise. The `len_nonneg` assume *did* take effect (loop guard branch flipped from `b.lt` signed to `b.lo` unsigned in the disasm) but the bounds check at the loop body's top survived unchanged.

**Why it didn't work — from the optimized IR dump.**

```llvm
sc.rhs6:                                          ; preds = %sc.rhs6.lr.ph, %while.body
  %hi.051 = phi i64 [ %2, %sc.rhs6.lr.ph ], [ %add, %while.body ]
  ...
vidx.ok:
  tail call void @llvm.assume(i1 %len.nonneg)
  %bounds18.not = icmp ult i64 %hi.051, %vec.len   ; survives — the bounds check
  br i1 %bounds18.not, label %vidx.ok17, label %vidx.oob16

while.body:
  %add = add nuw nsw i64 %hi.051, 1
  %lt = icmp ult i64 %add, %vec.len                ; loop guard — same predicate
  br i1 %sc.result, label %sc.rhs6, label %while.exit
```

The bounds check checks `%hi.051` (a phi), the loop guard checks `%add` (= hi.051 + 1, which becomes next-iter's hi via the phi). To fold them, LLVM would need to JumpThread: split `sc.rhs6` by predecessor, and on the `while.body` back-edge predecessor recognize `hi.051 = %add` and propagate the proof.

**JumpThreading didn't fire.** Most likely cause: the panic block in the bounds check's `oob` arm calls `puts` + `exit` + `unreachable` — observable side effects. JumpThreading's cost-benefit treats splitting around an instruction that drives a side-effecting branch as expensive, especially when the side-effecting path is `cold` but not provably unreachable. The same factor blocked SCEV from propagating the assume-derived facts as loop-invariant.

**What worked instead.** Source-level dominator-aware elision in `compile_vec_index`:
1. Parse the `while` guard for signed-comparison conjuncts → push `AssertedIndexBound` facts on a stack.
2. Propagate facts across short-circuit `and` (push LHS facts before compiling RHS).
3. At `compile_vec_index`, check the stack — split the runtime bounds check into lower (`slt 0`) + upper (`sge len`) halves and skip whichever the stack proves.

End result: 9-instruction inner loop, statistically identical to Rust on the kata #5 bench (0.98× of Rust, 36.9 ± 2.4 ms vs Rust's 37.6 ± 2.5 ms).

**Lesson.** `llvm.assume` is good for telling the optimizer about facts on a *specific SSA value* that it can't derive locally — e.g. `assume(ptr != null)` on a pointer the compiler can't range-track. It is *not* a tool for "this loop's bounds check is redundant" when the redundancy crosses a phi merge with a side-effecting OOB branch. For that pattern, emit IR that doesn't have the redundant check in the first place — either via source-level proof (the path shipped) or by restructuring the bounds-check shape to put both compares on the same SSA value (still open as a generalization to compound indices, see scope notes in the original kata-5 wip doc before its deletion).

**Files this lesson affects.** None canonical — `src/codegen.rs::compile_vec_index` carries the resulting design via the `AssertedIndexBound` enum + `emit_split_bounds_check` helper. Next perf investigation that considers `llvm.assume` for a loop-resident redundancy should consult this entry first.
