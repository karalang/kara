# Name interning — front-end name-handling cost, measured

**Status:** Stages 0–2 DONE (2026-08-17). Stage 3 (the `Symbol(u32)` interner
proper) is scoped below with measured expectations, not started. Origin:
`docs/spikes/structural-debt.md` § "Name interning (review item 9c)".

## The question

All type/function tables are `HashMap<String, …>`; the typechecker alone has
~700 `.to_string()` calls. Is a `u32`-symbol interner worth the large,
cross-phase conversion? Rule from the backlog: decide with before/after
compile-time measurements, not opportunistically.

## Instrument

`cargo run --release --example bench_frontend -- file.kara [iters]` — times
each front-end phase separately (parse / prepare / resolve / typecheck /
effectcheck / ownership) and counts heap allocations per phase via a counting
global allocator. `karac build` numbers (bench/compile_speed) are diluted by
LLVM; this is the front-end-resolution instrument. Attribution via
`valgrind --tool=callgrind` on one iteration
(`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` for symbols).

Corpus: `bench/compile_speed/synthetic.kara` (10,858 lines, 100 clusters,
34 traits × 100 impls — call-graph-heavy by design) and
`kara-katas/apps/cumulus/cumulus.kara` (2,037 lines of real application code).

## Stage 0 — baseline (synthetic, median of 10)

| phase | med_ms | allocs |
|---|---:|---:|
| parse | 15.2 | 162k |
| resolve | 8.8 | 33k |
| typecheck | 35.0 | 165k |
| **effectcheck** | **136.6** | **2,442k** |
| ownership | 44.0 | 336k |
| **TOTAL** | **240.8** | **3,138k** |

Callgrind, whole run: **~37% allocator, ~13% SipHash string hashing, ~11%
`format!` machinery, ~6% memcpy/memcmp** — i.e. ~60%+ of all front-end
instructions were string-identity overhead. Inclusive attribution put ~32% of
the entire run inside `format!` calls issued by
`effectchecker::inference::collect_calls_in_expr`.

## Stage 1 — the quadratic the measurement found (B-2026-08-17-1)

`collect_calls_in_expr`'s name-only method fallback scanned **every**
`method_bodies` key per method-call expression, allocating
`format!(".{}", method)` **per key probed** — O(call sites × impl methods)
allocations, multiplied again by the passes that re-walk bodies (target gate,
GPU gate, bounds). Fix: a `method_name_index` (bare name → matching
`Type.method` keys) built once at the end of `collect_function_info`,
consulted per call site.

Result (synthetic): effectcheck **136.6 → 76.1 ms (−44%)**, allocs 2.44M →
0.64M; front-end total **240.8 → 177.8 ms (−26%)**; total instructions
1.62B → 1.00B (−38%). On cumulus (real code, effectcheck a small phase) the
effect is negligible — the quadratic needed the many-impls shape to bite.

## Stage 2 — FxHash on the effectchecker's internal tables

After stage 1 the profile had no single hot site left: ~23% SipHash + ~32%
allocator spread across every phase's `HashMap<String, …>` traffic — the
textbook interning signature. Cheapest counter-measure first: swap the
effectchecker's *internal* String-keyed working tables (checker struct fields,
call graph, Tarjan SCC state, bounds indexes) to `rustc_hash::FxHashMap` /
`FxHashSet`. Public result structs and `pub fn` signatures stay
`std::HashMap` — one `.into_iter().collect()` per result field at the check()
boundary (once per compile, ~thousands of entries; noise). Span-keyed maps
stay std: SipHash on a fixed-size key is not the per-byte problem.

Result (synthetic): effectcheck **76.1 → ~67 ms (−12%)**; front-end total
**177.8 → ~168 ms (−6%)**; instructions 1.00B → 0.96B. Verified: full
non-LLVM suite 8,959/0; codegen E2E 3,000/0, par_codegen 258/0,
memory_sanitizer 1,109/0 (all under `KARAC_REQUIRE_RUNTIME_ARCHIVE=1`).
FxHash is deterministic where RandomState was per-process random — iteration
order changes were already unobservable-by-construction, and determinism is a
strict improvement.

**Session net (synthetic): front end 241 → ~168 ms (−30%), allocations
3.14M → 1.34M (−57%).**

## Stage 3 — the interner proper (NOT STARTED; scope before starting)

What the remaining profile says (post-stage-2 callgrind): ~22% allocator,
~18% SipHash (typechecker/ownership/resolver tables + the effectchecker's
`HashSet<Effect>` web, whose `Effect` hashes String fields), ~5%
memcpy/memcmp. The clone traffic FxHash cannot touch — 1.34M allocs, largely
`String` key clones and `Type`/`Token` clones — is the interning target.

- **Ceiling:** eliminating all string hash+clone+compare overhead is worth
  roughly 30–40% of the *current* front end (diffuse; no single site).
- **Shape:** `Symbol(u32)` + per-compilation interner; convert one phase's
  key space at a time (effectchecker first — its keys are already
  boundary-isolated by stage 2's std/Fx seam), AST identifiers last.
- **Cost honestly stated:** the checkers take `&str` from AST nodes at
  hundreds of sites; every one becomes an intern() or a Symbol-carrying AST.
  This is the "large, cross-phase change" the backlog predicted — multiple
  dedicated sessions, each ending green.
- **Cheaper intermediate first:** extending the stage-2 FxHash swap to the
  typechecker/ownership/resolver internal tables is the same mechanical
  recipe (~1 session, est. another 5–10%), and shrinks the eventual Symbol
  diff since field types are already seam-isolated.

## Lifecycle

Stages 0–2 are landed; this doc stays as the measurement record and the
stage-3 scoping. If stage 3 is taken up, measure per phase converted with
`bench_frontend` and append; if it is declined, record the decision here and
close the structural-debt entry with a pointer to these numbers.
