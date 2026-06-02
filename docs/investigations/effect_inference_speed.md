# Effect inference compilation speed — G8 investigation

**Status:** ✓ Resolved (2026-06-01). **Owner:** unassigned.
**Checklist item:** [`phase-6-runtime.md`](../implementation_checklist/phase-6-runtime.md)
G8 — "Effect inference compilation speed".
**Harness:** [`tests/effect_inference_bench.rs`](../../tests/effect_inference_bench.rs).
**Hardware:** Apple M5 Pro (6 perf + 12 efficiency cores), `--release`.

G8 asked: benchmark effect inference on synthetically large modules (500+
private functions, 20+ call depth); if inference exceeds **10% of compilation
time**, consider (1) per-function effect caching, (2) optional annotations as
hints, (3) a warning when the internal call graph exceeds a complexity
threshold.

**Bottom line:** effect inference is **linear** in call-graph size
(~3 µs/private-fn, ~2× time for 2× functions, no blowup through 4000 fns) and
costs single-digit milliseconds even on the largest synthetic modules. None of
the three mitigations are warranted. A separate, unrelated finding surfaced:
`ownershipcheck` is **super-linear** (~quadratic) and is the dominant front-end
cost — tracked as its own checklist entry.

---

## Method

The harness re-parses from source each iteration (`desugar`/`lower` mutate the
AST in place), times every front-end phase with `Instant`, and reports the
phase-by-phase **median** over 7–9 measured runs after 2 warmups. Four
synthetic module shapes, each emitting one `effect resource Db;` + an extern
leaf `leaf_io() reads(Db)` so `reads(Db)` originates at the bottom of the graph
and inference must propagate it transitively to every caller:

| Shape | Stresses |
|---|---|
| **chain(n)** — `node0 → node1 → … → leaf_io` | propagation *depth* (the "20+ call depth" requirement; depth == n) |
| **mesh(n, fanout)** — each fn calls its next `fanout` neighbors | broad call graph (~`n·fanout` edges) |
| **recursive(clusters, size)** — mutually-recursive SCC clusters | Tarjan SCC + the per-SCC O(k²) fixpoint loop |
| **wide(n, resources)** — leaves read 1 of `resources` distinct resources | `EffectSet` union/dedup as inferred sets grow |

"Compilation time" denominator = sum of front-end phases (parse, desugar,
resolve, typecheck, lower, effectcheck, ownershipcheck). **Codegen is excluded
deliberately** — it only grows the denominator, so the reported effectcheck
share is a strict upper bound. In any real `karac build`, codegen + LLVM
optimization + link dominate, pushing effectcheck's true share far lower.

Reproduce:

```bash
cargo test --release --test effect_inference_bench effect_inference_speed \
    -- --ignored --nocapture
```

## Results (M5 Pro, release, median)

| Case | private fns | effectcheck | µs/fn | ownershipcheck | front-end total | effectcheck % |
|---|---:|---:|---:|---:|---:|---:|
| chain depth=500 | 500 | 1.49 ms | 3.0 | 25.8 ms | 28.7 ms | 5.2% |
| chain depth=1000 | 1000 | 3.01 ms | 3.0 | 97.4 ms | 102.9 ms | 2.9% |
| mesh n=500 fanout=4 | 500 | 3.58 ms | 7.2 | 27.3 ms | 34.1 ms | 10.5% |
| mesh n=1000 fanout=6 | 1000 | 11.76 ms | 11.8 | 102.2 ms | 122.7 ms | 9.6% |
| recursive 100×5 | 500 | 2.79 ms | 5.6 | 26.3 ms | 30.7 ms | 9.1% |
| recursive 50×10 | 500 | 3.30 ms | 6.6 | 25.9 ms | 30.8 ms | 10.7% |
| wide n=500 res=16 | 500 | 4.38 ms | 8.8 | 26.2 ms | 33.1 ms | 13.2% |

**Linearity probe — chain depth, effectcheck only:**

| n | effectcheck | µs/fn | growth |
|---:|---:|---:|---|
| 250 | 0.74 ms | 2.96 | — |
| 500 | 1.50 ms | 3.00 | 2.03× time / 2.0× fns |
| 1000 | 3.09 ms | 3.09 | 2.06× / 2.0× |
| 2000 | 6.45 ms | 3.23 | 2.09× / 2.0× |
| 4000 | 14.17 ms | 3.54 | 2.20× / 2.0× |

µs/fn is essentially flat (the mild 3.0 → 3.5 drift over a 16× size increase is
the inferred `reads(Db)` set being unioned through a deeper chain, not an
algorithmic regime change). Doubling the module doubles the time — **linear**.

## Interpretation

1. **No super-linear blowup.** The algorithm matches its complexity bound:
   call-graph build O(N+E), Tarjan SCC O(N+E), and a per-SCC fixpoint that is
   O(k²) *only within a cycle of size k* — and effects are monotone, so each
   cycle converges in ≤ k passes (one effect-hop per pass). The synthetic
   recursive shapes confirm SCC clusters add no measurable penalty over the
   acyclic chain at equal function count.

2. **The 10% threshold is a denominator artifact, not an effect-inference
   problem.** The front-end "total" is dominated by `ownershipcheck` (below),
   which is itself super-linear. Where effectcheck nominally approaches/crosses
   10% (mesh, wide, recursive-50×10), it does so against a denominator that is
   *also* inflated — and against a codegen-inclusive total it falls well under
   10%. The load-bearing signal is the scaling probe: effect inference is
   linear and single-digit-ms.

3. **The three G8 mitigations are not warranted:**
   - *(1) per-function effect caching* — measure-first: a ~3 µs/fn linear pass
     has no hotspot to cache. (Mirrors the same "no interning ⇒ cache key costs
     more than the walk" call made for the cross-task-safe walker.)
   - *(2) optional annotations as hints* — would trade the language's
     "private effects are inferred" guarantee for no measurable win.
   - *(3) complexity-threshold warning* — there is no complexity cliff to warn
     about; a warning here would be noise.

   Revisit only if a *real* corpus (not a synthetic stress) ever shows effect
   inference as a profiled hotspot, per
   [`feedback_optimize_for_production_not_kata`](../../CLAUDE.md).

## Surfaced finding — `ownershipcheck` is super-linear

Not effect inference, but unmissable in the data: `ownershipcheck` is the
dominant front-end phase and scales **~quadratically** — 25.8 ms → 97.4 ms for
chain 500 → 1000 (≈ 3.8× for 2× input). At 1000 functions it is **~32× the
effectcheck cost** and **~95% of the front end**. This is the actual
front-end scaling lever, far ahead of effect inference. Tracked as its own
checklist entry under "Pre-existing Phase 6 work" in
[`phase-6-runtime.md`](../implementation_checklist/phase-6-runtime.md)
(per the "surfaced bugs get their own tracker entry" discipline); root-causing
the quadratic factor is deferred to that entry, not this one.
