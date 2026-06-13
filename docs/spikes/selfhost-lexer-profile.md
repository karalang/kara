# Spike: profile the self-hosted lexer — real-world Kāra codegen hotspots

**Status:** scoped, not yet run. Filed 2026-06-12.
**Decision this spike gates:** where to spend `karac` codegen-perf effort next. The
kata corpus (leetcode) over-represents tiny allocation-bound algorithmic puzzles and
under-represents the workloads Kāra is actually positioned for (bulk-data/analytics,
systems/parsing, latency-bound small-tensor ML). It served as a *bug-finder*, not a
perf oracle. `selfhost/src/main.kara` (1864 lines: byte-scan + tokenize +
String-build) is a real Kāra systems program **we already have** — profiling it gives
the first honest signal of where real-world Kāra code, and karac's codegen for it,
spends time.

## The question

On a realistic input (lex hundreds of KB of `.kara` source), where does the
self-hosted lexer spend its time — and how much of that is **karac codegen quality**
(slow generated code for a common Kāra pattern) versus the algorithm itself?

**Hypothesis (to be measured, not assumed):** allocation / String-building bound, not
compute. If confirmed, the highest-leverage codegen lever is **reducing allocations in
String/Vec-heavy code** — which *every* real program hits — NOT vectorization, which
this session showed already reaches Rust parity (see
[independence-noalias-ilp.md](independence-noalias-ilp.md)) but only matters for the
narrow bulk-data class.

## Method

1. **Snapshot** `selfhost/src/main.kara` from `main`. Another session actively edits
   the `selfhost-lexer` worktree — do **not** profile a live worktree.
2. `karac build` it (release codegen, `-O2` default). Feed a **large** real input —
   concat of the compiler's own `.kara` sources / `examples/`, ≥ a few hundred KB,
   lexed in a loop for a stable sample window.
3. Sample under macOS `sample <pid>` (or Instruments / `xctrace`); rank functions by
   **self-time**. Cross-check **allocation behavior** (count `karac_alloc_or_panic`
   calls, or `leaks`/`heap`) — String-build sites are the prime suspects
   (`escape_for_render`, `strip_underscores`, `render`, token `Vec` growth).
4. **Parity number:** wall-time versus the Rust lexer (`src/lexer.rs`, or the
   `kara-katas` oracle) driven on the same input. A self-hosted compiler many× slower
   than its Rust self is a *credibility* problem, not just a perf one
   (`project_self_hosting_v1_credibility`).
5. **Trust the profile + asm** of the top function, not just wall-clock.

## Decision rule (set before measuring — no post-hoc goalposts)

- **Allocation/String-build dominates** (> ~40 % self-time, or the parity gap traces to
  alloc traffic) → next codegen-perf slice is **allocation reduction** (String-builder
  reuse, small-string optimization, `Vec` pre-sizing, ref-not-copy on hot string
  paths). File each concrete hotspot as its own tracked entry.
- **Compute/branch-bound** → different levers (branch hints from effect analysis, the
  cost model) — and a surprise worth knowing.
- Either way the deliverable is a **ranked list of real codegen-quality hotspots** —
  the honest replacement for the leetcode perf signal.

## Caveats

- Profiles the **lexer** specifically (the only self-hosted component today): a fair
  but partial slice of "real Kāra." Re-run as the parser / typechecker get ported — the
  signal gets richer and more representative.
- This is a *measure-first* spike: produce the profile + parity number first; do not
  pre-commit a codegen slice until the hotspots are known.

## Cross-references

- [independence-noalias-ilp.md](independence-noalias-ilp.md) — resolved 2026-06-12:
  vectorization/aliasing is **not** the real-world lever (param `noalias` inert;
  `wrapping_*` was the actual autovec enabler; at Rust parity; alias-scope metadata
  deferred). This profiling spike is the follow-on that asks "then what *is* the lever?"
- `roadmap.md` § Codegen Optimization (IR quality pass).
- `feedback_optimize_for_production_not_kata`, `feedback_simulate_demand_dont_wait`.
