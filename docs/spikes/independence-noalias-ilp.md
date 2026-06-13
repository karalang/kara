# Spike: independence → backend alias metadata (Tier-0 ILP / autovectorization)

**Status:** ✅ **RAN — resolved 2026-06-12. Decision: Tier-0 *aliasing* is v1.x, not P0.**
Filed 2026-06-09.
**Decision this spike gates:** whether "independence feeds the backend" (Tier 0 in
design.md § Feature 5) is **P0** (ships at v1, becomes the launch headline) or stays
**v1.x**. See `implementation_checklist/phase-6-runtime.md` (cost-model + Tier-0
entries) and `phase-7-codegen.md` (Tier-0 lowering mechanism).

## Resolution (2026-06-12)

Ran the measure-first probe (the A-vs-B method below), plus a correction to its
premise. Full detail recorded in `roadmap.md` § Codegen Optimization:

- **The premise was incomplete.** This spike assumed "BCE already removes the
  per-iteration exits that block the autovectorizer, so the open question is the
  *marginal* win of the aliasing half." But BCE removes the **bounds-check** exit, not
  the **overflow-trap** exit — every checked `+`/`-`/`*` emits a `b.vs → panic`
  side-exit (`emit_checked_int_arith`) that blocks vectorization *before aliasing is
  ever considered*. Proven: a pure-copy kernel (no arithmetic) vectorizes; the `+`
  kernel stays fully scalar; no runtime alias-check is even emitted for it.
- **The actual autovec enabler is non-trapping arithmetic, not alias info.**
  `wrapping_add`/`sub`/`mul` (landed 2026-06-12) make the kernel body straight-line, so
  it auto-vectorizes (NEON `add.2d`) — **1.25× over the scalar trap version**.
- **The aliasing half (this spike's subject) adds ≈ 0.** Rust oracle, indexed
  (runtime alias-check) vs `zip` (disjointness conveyed → no check): 184.7 vs 184.4 ms
  on a memory-bound add kernel — identical. Kāra already matches Rust 1.00× at the
  ~130 GB/s memory wall; removing a *compute* branch can't beat a *memory* limit.
- **Param-level `noalias` is inert on its own** (shipped as groundwork, commit
  `397e4d7b`): inlining exposes the caller's allocas, so alias analysis needs no help
  in the common single-TU case.

**Decision:** Tier-0 *aliasing → backend metadata* is **v1.x / deferred**, NOT the P0
launch headline. The launch-worthy autovec story is `wrapping_*` (shipped) + the
hand-vectorized `Vector[T,N]` spine. Alias-scope metadata is filed deferred in
`roadmap.md` with a concrete build-when trigger (auto-vec-heavy non-hand-vectorized
code, or a compute-bound / many-slice kernel where the check bites — e.g. latency-bound
small-tensor inference). The open follow-on — *what the real-world codegen lever
actually is* — moves to [selfhost-lexer-profile.md](selfhost-lexer-profile.md).

The original scoping below is kept for the record.

## The one question
How much performance does Kāra's effect/ownership-derived no-alias information add
**beyond what LLVM's own alias analysis + Kāra's existing bounds-check elision (BCE)
already achieve**? BCE already removes the per-iteration exits that block the
autovectorizer, so the open question is specifically the *marginal* win of supplying
the **aliasing** half on top of that. If that delta is large → Tier 0 is the crux and
P0 is justified. If marginal → it's a v1.x refinement, not the foundation.

**This is a measure-first gate on a strategic decision** — do not re-architect the
launch narrative around Tier 0 until this number exists.

## Method — cheap upper-bound probe first (no sound lowering, no harness yet)
The goal is a *ceiling* number: "given perfect alias info, how much faster does this
get?" Hand-inject the metadata; do **not** build the general, correct lowering or the
differential-equivalence harness for the spike (those are the real-implementation
gates if the number justifies it).

### Kernels (3)
1. **(a) Canonical cross-arg case** — a loop over 2–3 *separate* arrays where the
   compiler must conservatively assume the arguments may alias: AXPY
   (`out[i] = a[i]*s + b[i]`), a stencil, or a small N-body force loop. This is where
   `noalias` classically unlocks vectorization.
2. **(b) Marginal-add control** — a single-array loop that BCE *already* lets LLVM
   vectorize, to isolate how much `noalias` adds *on top of what Kāra ships today*.
3. **(c) Negative control** — a pointer-chasing kernel (linked list / tree walk).
   Expect ≈ zero win; confirms the model (pointer density = no Tier-0 benefit).

### Procedure per kernel
1. **Baseline (A):** current `karac build`. Read the hot loop's **LLVM IR + asm** —
   is it vectorized? are there runtime alias checks / scalar fallback?
2. **Treatment (B):** hand-inject `noalias` / `!alias.scope`+`!noalias` on the
   kernel's pointer args — a manual IR edit on the emitted module, or a throwaway
   codegen hack behind `KARAC_SPIKE_NOALIAS=1`. Confirm via **asm** that B now
   vectorizes / drops the alias checks. (The injected facts must be genuinely correct
   for the kernel — keep operands trivially disjoint — or the number is garbage.)
3. **Measure A vs B** wall-clock: same load, repeated runs, hard timeouts, stable
   medians (per the bench discipline — one kernel at a time, report headline delta).
4. **Ceiling check:** bench B against a C-with-`restrict` (and/or Fortran) mirror of
   the same kernel. Does the no-alias win even *exist* here, and does B reach that
   ceiling? If C-`restrict` ≈ C-no-`restrict`, the kernel isn't a fair test — discard.
5. **Target reality:** build for the real launch ISA with vectors (cpu-baseline `v3`
   / the M5's NEON) and measure on the machine that ships. Confirm vectorization in
   the asm — **trust the asm, not just wall-clock** (a wall-clock delta could be noise).

## Decision rule — SET NOW, before measuring (no post-hoc goalposts)
- Kernel (a) shows a **repeatable speedup the asm confirms comes from newly-enabled
  vectorization** (bar: ≥ ~1.3× on the kernel, or "vectorized where the baseline was
  scalar") **AND** it is a meaningful add *over BCE* (delta on kernel b) →
  **Tier 0 is P0**; re-tag phase-6/phase-7 and make it the launch headline.
- Delta over BCE is **marginal** (< ~5–10%; LLVM + BCE already captured it) →
  **keep Tier 0 at v1.x**; it's a refinement, not the crux.
- Kernel (c) ≈ 0 either way (sanity).

## If P0 is justified, the real-implementation gates (NOT part of this spike)
1. The general independence→`noalias` lowering in `src/codegen.rs`, sound by
   construction from the effect-distinctness conflict graph + ownership facts.
2. **The differential-equivalence harness** — `noalias`-on vs `noalias`-off
   observationally identical across a fuzzed corpus. This is the correctness gate, and
   it lands on the **launch critical path** under P0. An over-broad `noalias` is a
   *silent miscompile*, not a perf regression (Rust `-Zmutable-noalias` precedent).

## Caveats
- Measure **marginal-over-BCE**, not over a crippled baseline, or the win is overstated.
- Hand-injected `noalias` must be correct for the kernel or the measurement (and any
  conclusion) is meaningless.
- Confirm the transform in the **asm**; don't infer vectorization from wall-clock alone.
