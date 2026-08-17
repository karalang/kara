# Name interning — front-end name-handling cost, measured

**Status:** Stages 0–3f DONE (2026-08-17). Stage 3a is the first slice of the
`Symbol(u32)` interner proper (the effectchecker's full key space); 3b–3d are
the DHAT-guided allocation/hashing tails that followed it. Remaining stage-3
phases (ownership, typechecker, AST identifiers) scoped below. Origin:
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

## Stage 2½ — the FxHash seam extended to the other phases

The "cheaper intermediate" from the original stage-3 scoping, done as its own
slice: the same recipe applied to the typechecker (`TypeEnv`'s lookup tables,
`LocalTypeScope`'s per-scope variable maps, the unification substitution
maps), the ownership checker (per-callee/per-binding tables +
`UseClassifier` prelude), and the resolver's three internal maps. Seam
discipline held: public result structs (incl. `SymbolTable`/`Scope`) and pub
signatures stay std; helpers called from both sides of the seam
(`is_cross_task_safe_with`) became `BuildHasher`-generic.

Result (interleaved A/B, 3 rounds, synthetic corpus): **~3–5% wall**
(median 175–184 ms vs 181–190 ms), **−1.9% instructions** (0.915B →
0.898B), allocations unchanged. Low end of the 5–10% estimate. The reason,
per the post-change profile: remaining SipHash is **~13%** of instructions,
and most of it is in the maps the seam rule deliberately keeps std — the
`SpanKey`-keyed side tables (`expr_types` alone takes 165k inserts and is
then consulted by every later phase through the std `TypeCheckResult`
field) and `Scope.names`. "SipHash on a fixed-size key is not the per-byte
problem" is true but incomplete: it is still ~10× an FxHash per operation,
and the SpanKey tables are the highest-traffic maps in the compiler.
Swapping them means moving the std/Fx seam *through* the public result
structs (~50 `TypeCheckResult` fields and every consumer annotation in
interpreter/codegen/concurrency) — a separate decision recorded here, not
taken opportunistically. Landed as `cccfc6d9`.

## Stage 2¾ — the seam moved through the result structs

The SpanKey decision, taken: every `HashMap<SpanKey, _>` /
`HashSet<SpanKey>` in the crate went Fx — public result structs
(TypeCheckResult, OwnershipCheckResult, ResolveResult, EffectCheckResult)
included, with consumers in interpreter/codegen/lowering/monomorphization
following mechanically (the compiler found every mismatch site; nothing
needed judgment beyond keeping `Program`'s lowering-mirrored tables
consistent end-to-end). The stage-2½ boundary collects on the typechecker
result reverted to direct moves — moving the seam *removed* code.

Result (interleaved A/B, 3 rounds): front end **157–169 ms vs 168–184 ms
(−6–8% wall)** — typecheck −9%, ownership −7%, effectcheck −4% —
instructions **0.863B (−3.9%)**. Roughly double the stage-2½ win, i.e. the
original "span-keyed maps stay std" rule was leaving more on the table
than the String-keyed swap it accompanied. Remaining SipHash: ~9.5%
(Scope.names, the effectchecker's Effect sets, AST-side tables — all
colder). Landed as `739eecb8`.

## Stage 3 — the interner proper (3a landed; scope below)

What the post-stage-2¾ profile said: ~33% allocator, ~9.5% SipHash
(Scope.names, Effect sets, AST tables — the cold tail), ~4% memcpy. The
clone traffic FxHash cannot touch — 1.37M allocs, largely `String` key
clones and `Type`/`Token` clones — is the interning target.

- **Ceiling:** eliminating all string hash+clone+compare overhead is worth
  roughly 30–40% of the *current* front end (diffuse; no single site).
- **Shape:** `Symbol(u32)` + per-compilation interner; convert one phase's
  key space at a time (effectchecker first — its keys are already
  boundary-isolated by stage 2's std/Fx seam), AST identifiers last.
- **Cost honestly stated:** the checkers take `&str` from AST nodes at
  hundreds of sites; every one becomes an intern() or a Symbol-carrying AST.
  This is the "large, cross-phase change" the backlog predicted — multiple
  dedicated sessions, each ending green.

### Stage 3a — effectchecker key space → `Symbol(u32)` (LANDED)

`src/intern.rs`: `Symbol(u32)` + per-compilation `Interner` (`Rc<str>`
backed, `RefCell` interior mutability so `&self` walkers can mint), with
two design points that carried the slice: **`get` vs `intern`** (a
non-inserting probe whose miss proves any symbol-keyed table misses — used
for every "is this identifier a function?" check against arbitrary AST
names) and a **dotted-pair cache** (`(Symbol, Symbol) → Symbol` for
`"Type.method"` composites, so the `format!` allocation happens once per
distinct pair instead of once per call-site probe; `get_dotted` is its
non-inserting sibling, sound against tables whose keys are all
dotted-minted). The whole checker key space converted: every
function-name-keyed table in `EffectChecker` + all 10 submodules, the
calls vectors, call graph, Tarjan (visit order sorts by *resolved* text —
symbol order is mint order, and the alphabetical processing order was
deliberate), `method_callee_types` values (interned once at the
typechecker seam, killing a per-method-call-site `String` clone), modbind
synthetic keys (pre-minted into `ModBindingInfo`), and the old
`STDLIB_METHOD_MAP` 36-entry linear scan-with-string-compares per method
call site (now one `Symbol` map probe). `infer_function_effects`
additionally dedupes callees per pass — repeated calls to the same callee
could only re-contribute the same effects and `EffectSet::add` keeps the
first origin anyway — which drops the per-call-site `Vec<Effect>`
clone-storm. Boundary: `EffectCheckResult` stays String-keyed; symbols
resolve exactly once, at result construction. `Effect.resource` and
`EffectOrigin` stay String this slice (they cross into the public result).

Result (interleaved A/B, 3 rounds, synthetic): effectcheck
**65–67 → 42–44 ms (−35%)**, its allocations **643k → 202k (−69%)**;
front-end total **159–168 → 139–141 ms (−13–16%)**; instructions
**0.862B → 0.671B (−22%)** — the largest single-slice instruction drop of
the spike. Cumulus (real code, effectcheck small): effectcheck −13–16%,
total ~−2%. Verified: fmt + clippy both legs; full non-LLVM suite
8,969/0; codegen 3,002/0, par_codegen 258/0, memory_sanitizer 1,110/0,
cli 590/0 (all under `KARAC_REQUIRE_RUNTIME_ARCHIVE=1`).

**Session net (synthetic): front end 240.8 → ~140 ms (−42%), instructions
1.62B → 0.671B (−59%), allocations 3.14M → 0.93M (−70%).**

### Stage 3b — effectcheck's per-pass allocation tail (LANDED)

Post-3a DHAT (the honest attribution the phase timings could not give):
the top allocation sources were *still* effectcheck — `collect_calls_in_expr`
15.2% of all front-end bytes (the calls-vector churn) and
`get_callee_effects` 9.4% (it built a `HashSet<Effect>` + `Vec<Effect>`
with String-resource clones per distinct callee per pass — also a chunk
of the SipHash tail, since `Effect` hashes its String resource) — plus a
hidden `fn_bounds_index.get(..).cloned()` cloning a whole per-fn bounds
map on every `infer_function_effects` call.

The fix, contained in the effectchecker: `callee_effect_sets` returns
the underlying `EffectSet`s **by reference** (`CalleeEffectSets` +
zero-alloc iterator; the `PolymorphicWithFixed` union preserved by
filtering the inferred side against the fixed side), so an `Effect` is
cloned only when genuinely new to the accumulating set; the polymorphic
marker is hoisted out of the effects loop so it holds no `&mut self`;
and the bounds map is borrowed, not cloned.

Result (interleaved A/B, 3 rounds, synthetic): effectcheck
**42–44 → 38–39 ms (−9%)**, its allocations **202k → 117k (−42%)**;
front-end total ~−2.5%; instructions **0.671B → 0.630B (−6.1%)**.
Effectcheck's allocations are now down **82%** from the spike's start
(643k → 117k post-stage-1). Same full verification battery as 3a.

### Stage 3c — rc_predicate's quadruplicated sites build (LANDED)

The other DHAT standout: `rc_predicate`'s four candidate passes
(`rc_candidates`, `direct_uam_candidates`,
`direct_uam_all_consume_sites`, `loop_of_consume_candidates`) each
rebuilt the same by-binding use-sites map from the same CFG — cloning
every binding String and every `UseSite` (place vectors included) per
build — and `run_predicate_for_function_with` runs three of them per
function, plus ownership.rs's UAM pair two more. One borrowed
`UseSitesByBinding<'_>` (`&str` keys, `&UseSite` triples) is now built
once per CFG and shared; the public per-pass entry points remain as
thin wrappers.

Result (interleaved A/B, 3 rounds, synthetic): ownership
**38–41 → 33–37 ms (−11–13%)**, its allocations **365k → 252k (−31%)**;
instructions **0.630B → 0.593B (−5.9%)**. Same full verification
battery.

### Stage 3d — FxHash on rc_predicate/cfg working sets (LANDED)

Tail cleanup: the candidate passes' BFS visited-sets, natural-loop
block sets, the shared use-sites map, and the CFG builder's
binding/name sets were still std RandomState — `BlockId` (a usize)
hashed through SipHash per edge probe. Internal-only swap per the
stage-2 seam rule; public witness-map signatures unchanged.
Instructions **0.593B → 0.588B (−0.75%)**, wall within noise; the
module's last per-process-random iteration order is now deterministic.

### Stage 3e — the parser's cloning peek (LANDED)

Fresh DHAT by BLOCK COUNT (mallocs, not bytes) put `Token::clone` at
**16.7% of ALL front-end allocations** — 130k of 785k blocks. Cause:
`peek_token()` returns the current token BY CLONE, and `check()` (the
`eat`/`expect` backbone) called it just to compare discriminants — so
every kind-probe while sitting on an identifier/string token cloned its
String payload and threw it away. Several literal match arms even
cloned twice (payload out of the already-cloned scrutinee).

Fix: `peek_token_ref` / `peek_token_ref_at` (borrowed peeks; the
kind-only probes, `==` comparisons, `matches!`, and all 40
`match self.peek_token()` scrutinees now go through them), with payload
arms cloning only what they keep, before `advance`. The cloning
`peek_token` remains for the handful of sites that genuinely move a
token out.

Result (interleaved A/B, 3 rounds, synthetic): parse **21–22 →
15–17 ms (−25%)**, parse allocations **162k → 90k (−44%)**;
instructions **0.590B → 0.548B (−7.1%)** (same-tree A/B; sibling
features had moved the baseline from 3d's 0.588B). Verified on fresh
runtime archives after a sibling runtime symbol (`karac_par_run_auto`)
staled all four — the loud undefined-symbol failure mode working as
designed (B-2026-07-28-1).

### Stage 3f — the effectchecker stops cloning the whole AST (LANDED)

The other DHAT standout: 89% of all `Expr::clone` allocations traced to
ONE line — `collect_function_info`'s `Rc::new(f.clone())`, which
deep-cloned EVERY function and impl-method body in the program into the
checker's body tables. The `Rc` (review item 9b) had solved per-pass
cloning but left the one-time whole-AST clone. `FnHandle<'a>` replaces
it: a direct `&'a Function` borrow for real functions/methods (the
checker already holds `&'a Program`, and a borrowed handle even drops
the old need to detach via `Rc` — the borrow points into the program,
not into `self`), with `Rc<Function>` kept only for the synthesized
trait-default stubs. `Deref<Target = Function>` keeps all ~45 access
sites untouched; five whole-program snapshot sites retype to
`Vec<FnHandle>`.

Result (interleaved A/B, 3 rounds, synthetic): effectcheck
**48–52 → 35–41 ms (−25%)**, its allocations **117k → 64k (−45%)**;
total allocations 657k → 605k; instructions **0.548B → 0.520B (−5.1%)**.
Effectcheck allocations are now down **97%** from the spike's start
(2.44M → 64k). Same full verification battery.

### Remaining stage-3 phases (not started)

Post-3d: session net front end **240.8 → ~130 ms (−46%), instructions
1.62B → 0.588B (−64%), allocations 3.14M → 0.73M (−77%)**. What
remains, per DHAT: parse (token/`Expr` allocs, ~18% of bytes — the "AST
identifiers" leg), typecheck (`infer_expr`/`infer_binary`/`Type`
clones), ownership's residual (`check_function` binding tables — the
String-keyed maps Fx'd in 2½ but still cloning keys; its module tree is
~29k lines across 13 submodules — 3–4× the effectchecker conversion
surface, so plan a full session for the Symbol conversion), and the
~9% SipHash tail (`Scope.names`, remaining std maps). Each is its own
session-sized slice with 3a as the template. Post-3e the remaining
allocation profile by blocks: `Pattern::collect_bindings` (~50k,
Vec<String> per call across four phases), `Expr::clone` (~40k,
contract-clause and typechecker clones), lexer token payloads (~37k,
inherent until AST interning), `Type::clone` (~36k).

## Lifecycle

Stages 0–3a are landed; this doc stays as the measurement record and the
remaining stage-3 scoping. Measure per phase converted with
`bench_frontend` and append; if the remaining phases are declined, record
the decision here and close the structural-debt entry with a pointer to
these numbers.
