# Spike: source-level overflow-check elision via integer range analysis

**Status:** ✅ **RAN — resolved 2026-07-03. Decision: NOT WORTH BUILDING.** Source-level
range analysis is redundant with LLVM `-O` for the checks it can prove, and the residual
cost sits on `counter × unbounded-heap-value` multiplies that need array-content +
loop-trip analysis far beyond a safe prototype. If the speed matters, use the existing
`wrapping_{add,sub,mul}` opt-out (see [independence-noalias-ilp.md](independence-noalias-ilp.md))
or add a scoped block-level opt-out — do **not** grow the prover.

**Question this spike gates:** Kāra traps on `i64` overflow by default, so codegen emits
`llvm.s{add,sub,mul}.with.overflow` + a trap branch for every checked op
(`emit_checked_int_arith`, `src/codegen/expr_ops.rs`). On multiply-heavy kernels these
checks leave Kāra ~1.19–1.34× behind unsafe `clang -O3` / `rustc -O` (kata
[#52](../../../kara-katas/leetcode/1-100/52-n-queens-ii/),
[#54](../../../kara-katas/leetcode/1-100/54-spiral-matrix/); note kata #53 is a dead tie and
*safety-matched* `rustc -C overflow-checks=on` is **slower** than Kāra on #54). Could a
compile-time value-range analysis prove some of these checks redundant and drop them to
plain `nsw` ops, closing the gap without weakening the default-safe trap?

## Method

Two prototype layers were built on branch `feat-overflow-elision` (`21177ba9`, `aa65ca15`;
dropped 2026-07-03, reflog-recoverable), each a conservative, sound interval analysis in
`src/codegen/overflow_elision.rs` that only ever *removes* a check it can prove redundant:

1. **Modulo / const ranges.** `const_int_bindings` (singly-`let`, non-`mut` int constants)
   + `mut_int_ranges` (a singly-declared `mut` local's range = union of its initializer and
   every plain-assignment RHS, where `x % C` for a positive constant `C` bounds the result
   to `[-(C-1), C-1]` regardless of dividend — the rule that bounds an LCG state variable).
2. **Loop-counter ranges.** A scope stack pushed around each `while` body, reusing
   `collect_monotone_index_vars`: an increasing counter is `[init.lo, guard-upper]`, a
   decreasing one `[guard-lower, init.hi]`, with the guarded end slackened by one
   iteration's total step (the guard holds at body *entry*, so a counter stepped before its
   use can move past it) and counters stepped inside a nested loop dropped.

Verified sound: all 1997 codegen + 16 overflow-trap + 146 par_codegen tests green, and a
`while i<3 { i=i+1; i*i64::MAX }` still traps at `i==3`. Measured with the katas'
`bench.sh` + `hyperfine`, comparing baseline vs elision binaries and counting `smulh` /
`b.vs` in `_main` via `otool -tvV`.

## Findings

- **Layer 1 is redundant with LLVM `-O`.** The pass correctly elides 5–6 checks on the #54
  spiral and #53 LCG kernels (output bit-exact), but the emitted machine code is
  *identical* and runtime is **1.00×**. Disassembly shows why: LLVM's own
  correlated-value-propagation already sees the `srem` and const-folds these exact checks —
  kata-53's baseline has `smulh=0` for `state*m` **before** the pass runs.
- **Layer 2 works but doesn't help either.** On #54 it lifts elisions 5 → 12 and removes
  one hot-loop `smulh` (7 → 6), but runtime is again **1.00×**. The `counter × const` checks
  it proves are ones LLVM already elides on simpler shapes; the one it uniquely removes is
  in a cheap traversal loop, amortized to nothing.
- **The residual cost is structurally out of reach.** The 4 dominant surviving `smulh` on
  #54 are all `(pos+1) * grid[i]` — a bounded counter times an **unbounded heap array
  element**. Eliding them needs *both* (a) array-content range analysis to prove every
  `grid` element is in `[-50, 49]` (LLVM treats a heap load as fully unknown), and (b) a
  bound on `pos`, which has **no loop guard** (it is incremented across four sibling passes)
  and would need whole-loop trip-count / scalar-evolution analysis. Array-content analysis
  alone is insufficient because `pos` stays unbounded.
- **Caveat on the benchmark.** The `(pos+1)*value` weighting is a bench construct (added to
  make traversal order observable); real spiral code just pushes into a `Vec` with no
  multiply, so this kernel somewhat overstates the traversal-multiply component of the gap.

## Decision

Do not build the array-content + trip-count analysis. Bad ROI, and high soundness risk —
the *easy* layer already hid a use-after-increment miscompile hole (fixed via the step
slack), and array/trip analysis is where these get genuinely subtle; every bug there is a
"we promised it traps on overflow and it didn't" correctness hole. Chasing zero-cost safety
via an ever-smarter prover is a treadmill LLVM/GCC have not won outright.

If the ~1.3× on arithmetic-heavy kernels is worth closing, the lever is a **user-visible
opt-out**, not a prover: `wrapping_{add,sub,mul}` already exist and produce straight-line
non-trapping arithmetic (and, per [independence-noalias-ilp.md](independence-noalias-ilp.md),
also unblock autovectorization — 1.25× there). A scoped block-level `#[wrapping]` / an
`unchecked_mul` would give the full win, trivially soundly (the user opts in), mirroring
Rust's `-C overflow-checks` + `wrapping_*` + `unchecked_*`. That is the honest tradeoff:
safe by default, fast on request — and Kāra already sits level with checked-Rust everywhere
else.
