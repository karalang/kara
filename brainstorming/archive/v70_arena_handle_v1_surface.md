# 70. Arena / Handle Stdlib Surface — What Survives v1

**Status:** Synthesis closed 2026-06-01 (Q1–Q4 resolved). Implementation follow-ons listed at *Synthesis* §6 below — gates brainstorm move from `archive/` open to closed.

**Trigger:** Glossary line at `docs/glossary.md:312` markets `Pool[T]` + `Handle[T]` as the answer for *"ECS engines, compilers (AST arenas), connection IDs"* — three workloads with materially different lifecycle and access patterns. Meanwhile `Arena[T]` + `ArenaRef[T]` sit as one-line unchecked items on `docs/roadmap.md:460-461` with no design study, and the active Phase 8 implementation work on `Pool[T]` (`phase-8-stdlib-floor.md:152-159`) is being driven entirely by connection-pool semantics (`acquire(timeout)`, health checks, mutex+condvar, slot reuse).

The risk: a v1 user reaching for the documented AST-arena story finds an API shape built for connection pools, the shape doesn't fit, and the gap surfaces as a v1.1 backpedal on stdlib primitives — exactly the kind of "ship reality, not promises" failure the priority-tier discipline exists to prevent.

This brainstorm decides:
- What stdlib arena/handle surface (`Pool[T]`, `Arena[T]`, possibly `Symbol`/`Interner`) v1 commits to, validated against multiple workloads rather than driven by one.
- Whether the glossary's "one primitive serves all" framing holds, or whether bulk-arena and generational-arena are structurally distinct primitives that both need to ship.
- Which gaps Kāra's runtime surface has when stressed by a workload class (compilers) that the existing backend-persona design studies don't cover — triaged into v1 / v1.x / deferred under the priority-tier rules.

Per stored priority-tier definitions, **v1 = P0 + P1**. Anything labeled P2 here is post-v1.

The brainstorm is split into two sections with deliberately different output shapes:

- **Section A (Primitive API surface).** Output: API decisions that land in `docs/design.md` and `docs/implementation_checklist/phase-8-stdlib-floor.md`. Driven by four narrow workload lenses, each chosen to stress a specific shape of the arena/handle story.
- **Section B (Compiler-as-lens gap audit).** Output: a triaged gap list. Mechanism: walk through parser + typechecker + one codegen pass *on paper* in Kāra, surface every gap, and apply the discipline rule: a gap is in-scope only if *some other v1 workload also wants it*. Compiler-only gaps get a deferred entry. Backend-overlapping gaps escalate into Section A or a sibling brainstorm.

The two sections feed each other: if Section B surfaces an API requirement that overlaps with a backend workload, it lands in Section A's decision matrix; if Section A's chosen surface fails Section B's compiler walkthrough on a non-compiler-only dimension, Section A re-opens.

---

## Section A — Primitive API Surface

### Problem A1. The workloads being stressed

Four lenses, deliberately narrow, chosen so each one isolates a distinct property of the arena/handle surface. None of them is "build a compiler" — that's Section B's job.

| Lens | Lifecycle | Reuse | Concurrency | Stress point |
|---|---|---|---|---|
| Connection pool | acquire / release pairs, indefinitely | Yes — slots return to the pool | Multi-threaded; condvar + timeout | Bounded waiters, health-check hook, timeout |
| AST bulk arena (one compilation) | Allocate-only; freed in bulk at end | No — never released individually | Single-writer during build, optional read-parallel after | Cheap allocation, cheap indexing, no per-slot overhead |
| Regex NFA construction | Allocate-only during compile; immutable after | No | Single-threaded build, multi-reader match | Same as AST arena + immutable-after-build handoff |
| ECS / generational reuse | Allocate + free individual slots | Yes — slots reused with generation bump | Often multi-threaded; one writer per archetype | `Handle[T]` staleness detection, ABA-safe indexing |

**What each lens tells us if it succeeds against a candidate surface:**
- Connection pool succeeds → the bounded-waiters / timeout / health-check story holds. Already in flight (`Pool[T]` partial impl).
- AST bulk arena succeeds → no required per-slot mutex or generation counter; allocation path is one bump + return-index.
- Regex NFA succeeds → arena freezes correctly for read-only sharing; handles survive the freeze.
- ECS succeeds → `Handle[T]` generation semantics are correct under reuse; stale-handle detection is sound.

**Stress points that no single lens covers but multiple workloads imply:**
- Multi-arena programs (an HTTP server with a connection `Pool`, a per-request scratch `Arena`, a long-lived metric `Pool`). The surface must compose without forcing all-or-nothing decisions.
- The Send/Sync story (`marker trait Send`, `marker trait Sync`). `Pool[T]` for connections needs Send + Sync; an AST arena lives on one thread.

### Problem A2. The candidate surfaces

**Candidate 1 — One primitive (`Pool[T]` + `Handle[T]`) for all four workloads.**

This is the current glossary framing. `Pool[T]` carries bounded slots, mutex+condvar, generation counters, health-check hook. The AST-arena use case opts out of acquire/release semantics by just never calling `release` (handles are valid until the pool drops), opts out of bounded waiters by setting `max_connections = unbounded`, opts out of the health-check hook by not supplying one.

*Pro:* one API to learn; one implementation to maintain; doc surface narrower.

*Con (and the load-bearing one):* the user pays for *API-shape cost*, not just implementation cost. Monomorphization (once codegen lands; currently `Pool[T]` is interpreter-only at `src/interpreter/method_call_pool.rs` with no entry in `src/codegen/`) can erase the data-structure overhead — the hashmap-keyed-by-handle becomes a direct struct holding `Vec[T]` + counters; the `Value::Function` dispatch becomes a direct call. But three costs are structural to the API shape and survive any codegen optimization:

| Cost | Source | Can codegen elide? |
|---|---|---|
| `create_fn` closure as constructor | `Pool.new(create_fn, ...)` signature requires it | No — the API forces "construct via callback," not "construct at site" |
| `Result[PooledConnection, PoolError]` return | `acquire()` signature returns `Result` | No — every call site pays for unwrap + branch |
| `PooledConnection { pool_handle_id, val }` wrapper | Identity for cross-pool `release()` detection | No — wrapper is part of the type signature |

The AST allocation site under Candidate 1 would look like `let node = ast_pool.acquire().expect("pool full").val` vs. Candidate 2's `let idx = arena.push(Node::Lit(42))`. Even with perfect codegen, Candidate 1's call site is wordier *and* carries an unwrap branch on every alloc. The `create_fn` slot is the load-bearing failure: you cannot construct an AST node at the call site under `Pool`'s API — you have to either (a) construct it elsewhere and return it from `create_fn` (extra indirection per alloc), or (b) push parameters through closure capture per call. Generation counters on every slot are additional dead weight in the bulk-free case. Teaching surface is worse, not better — "use `Pool[T]` for your AST, but ignore these five parameters" is more friction than "use `Arena[T]` for bulk; use `Pool[T]` for reusable."

*Verdict (closed under Q1, 2026-06-01):* fails AST and NFA lenses on API-shape grounds, not implementation-cost grounds; survives connection pool and ECS.

**Candidate 1.5 — `Pool[T]` with a `pool.alloc(value: T) -> Handle[T]` method that bypasses `create_fn`.**

Extension of Candidate 1: keep one named type, add a second alloc path that takes the value directly instead of going through `create_fn`, returns a bare `Handle[T]` instead of `Result[PooledConnection, _]`.

*Pro:* erases two of Candidate 1's three structural costs (constructor callback, Result wrapping). AST alloc site becomes `let h = arena.alloc(Node::Lit(42))` — comparable to Candidate 2.

*Con:* still pays the generation-counter overhead on every slot for bulk-free workloads; still bifurcates the API ("when do I use `acquire` vs `alloc`?") which is the teaching-surface argument that killed Candidate 3; doesn't unify the `Handle[T]` semantics across reuse-vs-bulk (a `Handle` from `alloc` has no meaningful generation since the slot is never freed individually). The bifurcation specifically reproduces Candidate 3's "one type with a knob" failure mode — the knob is now method-choice rather than constructor-parameter, but it's the same teaching problem.

*Verdict (closed under Q1, 2026-06-01):* worth flagging for completeness; does not change the recommendation. Candidate 2 still wins on teaching-surface and on not paying for generation tracking the AST workload doesn't need.

**Candidate 2 — Two primitives (`Arena[T]` + `ArenaRef[T]` for bulk; `Pool[T]` + `Handle[T]` for generational).**

The roadmap entries at `roadmap.md:460-461` already imply this split — `Arena[T]` exists separately from `Pool[T]`. The glossary line is the only place that conflates them.

*Pro:* each primitive carries only the machinery its consumers need. AST arena = `Arena[T]`, bump-allocate, indices stay valid until drop, no generation overhead, no mutex. Connection pool and ECS = `Pool[T]`, full machinery. Both lenses succeed on their natural primitive.

*Con:* two primitives to spec, implement, document, test. More API surface. Some workloads (e.g. "compile a regex, then freeze it") sit between the two and the user has to pick.

*Verdict to validate:* the most likely answer; the question is whether `Arena[T]` is v1 (P0/P1) or v1.x.

**Candidate 3 — One primitive with two modes (a `mode: Bulk | Generational` parameter at construction).**

Compromise. One named type, one teaching surface ("you allocate things in storage, you get a handle back"), but two compiled hot paths via comptime dispatch on the mode.

*Pro:* fewer named types, similar perf to Candidate 2 if comptime dispatch is real.

*Con:* leans on comptime in a way the Phase 8 floor doesn't fully commit to yet. The user-facing teaching is "one primitive with a knob" which historically (Rust `Vec` vs `SmallVec`, Go `sync.Pool`) confuses people more than two clearly-named primitives. The constructor parameter becomes load-bearing — getting it wrong silently degrades perf with no compile-time signal.

*Verdict to validate:* probably the wrong shape; included for completeness so the brainstorm doesn't look one-sided.

**Candidate 4 — Two primitives + a `Symbol`/`Interner` third primitive.**

Candidate 2 plus an interner because every compiler, regex builder, comptime evaluator, and `f"..."` formatter wants deduplicating string storage with handle-based identity comparison. Currently absent from the roadmap.

The interner is *not* an arena variant — it's a Pool/Arena consumer that adds a dedup hashmap and identity-based comparison on handles. But it belongs in the same brainstorm because (a) it's the obvious next question after "arena/handle," (b) it's missing, (c) the same v1/v1.x/deferred triage applies.

*Pro:* covers a real gap that none of the other candidates address. The four lenses don't surface it because none of them are about deduplication; Section B will.

*Con:* expands v1 scope. The triage question is whether *enough* backend workloads also want symbol interning (regex, log-key deduplication, HTTP-header canonicalization, JSON-key interning, comptime identifier resolution) to justify v1 inclusion under the discipline rule.

*Verdict to validate:* the strongest candidate if Section B confirms backend overlap.

### Problem A3. The decision matrix

For each candidate, score against the four lenses + the multi-arena composition stress point + Send/Sync.

| Candidate | Connection pool | AST bulk arena | Regex NFA | ECS | Multi-arena composition | Send/Sync clarity | Teaching surface |
|---|---|---|---|---|---|---|---|
| 1 (one primitive) | ✓ | ✗ (API-shape cost) | ✗ (API-shape cost) | ✓ | ✓ | ✓ | ✗ (parameters-to-ignore problem) |
| 1.5 (one + `alloc` method) | ✓ | ~ (loses 2 of 3 API costs; gen-counter overhead remains) | ~ | ✓ | ✓ | ✓ | ✗ (`acquire` vs `alloc` bifurcation reproduces #3's failure mode) |
| 2 (split) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 3 (one + mode) | ✓ | ~ (depends on comptime) | ~ | ✓ | ✓ | ✗ (mode-as-runtime-knob smell) | ✗ (one-with-a-knob confusion) |
| 4 (split + interner) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ + new "what's interning for" surface |

*Candidate 1 failure mode was originally flagged as "hot-path cost" pending a bench. Closed under Q1 (2026-06-01) on API-shape grounds without bench: the `create_fn` constructor callback, `Result` return, and `PooledConnection` wrapper are structural to the type signature and survive any codegen optimization. See Candidate 1 body for the cost table.*

### Problem A4. Priority tier assignment

The triage that has to happen regardless of which candidate wins:

| Primitive | v1 (P0) | v1 (P1) | v1.x | Deferred |
|---|---|---|---|---|
| `Pool[T]` connection-pool consumer (current) | | ✓ (already P1) | | |
| `Pool[T]` codegen path (multi-thread backing) | | ✓ (gated on codegen for `std.process` / `Pool[T]`) | | |
| `Arena[T]` + `ArenaRef[T]` | TBD — depends on whether v1 backend workloads need bulk-arena | TBD | TBD | TBD |
| `Symbol`/`Interner` | TBD — depends on Section B + backend-workload audit | TBD | TBD | TBD |
| `#[generational_fallback]` annotation | | | ✓ (Phase 4+; design.md:12633) | |

The TBD rows are what Section B is for.

---

## Section B — Compiler-as-Lens Gap Audit

### Problem B1. The walkthrough

The exercise: write a parser, a typechecker, and a single codegen pass (`AST → simple IR`) in Kāra, *on paper*. Not implement; not even pseudo-code in detail. Walk through what each phase needs and write down every primitive, language feature, or stdlib surface the walkthrough reaches for. Then triage.

**Discipline rule, restated:** a surfaced gap is in-scope for v1 *only if some other v1 workload also wants it*. "Only a compiler needs this" → deferred entry. The triage check is the load-bearing thing; the gap list itself is just raw material.

### Problem B2. Walkthrough — Parser

What the walkthrough reaches for:

| Need | What it stresses | Other v1 workload that also wants it? |
|---|---|---|
| AST node storage (thousands of small nodes, one compilation lifetime, never freed individually) | `Arena[T]` (Section A candidate 2 or 4) | JSON parser, protobuf decoder, HTTP request parser, regex NFA — all want bulk-arena per parse |
| Source spans (compact, copyable, point into a source-map) | `Pool[T]` adjacent — small structs, identity-stable | std.tracing span IDs, log correlation IDs, request IDs |
| Identifier interning (every identifier in source becomes a `Symbol`) | `Symbol`/`Interner` primitive | HTTP header canonicalization, JSON-key dedup, log-key dedup, regex character classes |
| Error accumulation (collect many parse errors before bailing) | Not one-error-at-a-time `Result`; an accumulating diagnostic channel | HTTP request validation, form parsing, batch import validation |
| Recursive-descent with backtracking-light (peek + commit) | Iterator with putback / lookahead | Lexer for any structured format (regex, JSON, protobuf, URL parsing) |

**B2 verdict:** every parser need has a backend-workload twin. Nothing parser-specific surfaces as compiler-only. Arena, Symbol, accumulating diagnostics, lookahead iterators — all four have v1 backend justification.

### Problem B3. Walkthrough — Typechecker

What the walkthrough reaches for:

| Need | What it stresses | Other v1 workload that also wants it? |
|---|---|---|
| Type representation as `shared struct` with back-edges (recursive types) | `shared struct` + `#[cyclic]` + `weak` | Game ECS components with back-references, GraphQL schema with cycles, dependency graphs |
| Visitor pattern over the AST (walk, transform, accumulate) | Generic visitor traits + `shared struct` traversal ergonomics | Any tree/graph rewrite (HTML DOM manipulation, AST-based templating, ORM query trees) |
| Symbol table (scope-stacked, with shadowing, with snapshot/restore for backtracking) | Stacked-map data structure; probably `Pool[T]`-backed | HTTP middleware context stack, request-scoped value injection, comptime evaluation context |
| Unification (graph of type variables, occurs check, deterministic representative) | Union-find data structure | Constraint solvers in routing/dispatch, ECS query matching, comptime trait resolution |
| Fixed-point iteration (recursive type defs require multi-pass) | Iterate-until-stable patterns | Cache invalidation propagation, dependency resolution, configuration reload |
| Error accumulation with rich diagnostics + suggested fixes | Diagnostic channel from B2, extended | Same backend workloads from B2 |

**Stress points specific to the typechecker that didn't surface in the parser:**

1. **Visitor over `shared struct` with `weak` back-edges.** The walkthrough reveals that Kāra's `#[cyclic]` + `weak` story (design.md:12591) is sound for *storage* but the *traversal* ergonomics aren't documented. Does the visitor see `weak` edges as `Option[ref T]`? Does upgrading a `weak` inside the visit force the traversal to handle cleared references? This is a documentation/spec gap, not a primitive gap — but it's load-bearing for any backend workload that uses cyclic graph data (and `dep-graph` style workloads will).

2. **Union-find efficiency.** Path compression + union-by-rank wants flat-array-backed storage. `Pool[T]` with `Handle[T]` works but generation counters are wasted overhead. This is the AST-arena lens repeating itself — supports Section A candidate 2.

3. **Snapshot/restore for symbol-table backtracking.** Mid-parse error recovery sometimes wants "rewind scope to here." `Pool[T]` doesn't natively support snapshot; `Arena[T]` does (just remember the high-water mark). This is a real differentiator between the two primitives.

**B3 verdict:** typechecker needs overlap heavily with backend workloads (every row has a twin). Two non-primitive gaps surface — visitor-over-`weak` documentation, and snapshot/restore semantics on `Arena[T]` — both belong in design.md updates rather than new primitives.

### Problem B4. Walkthrough — One codegen pass (AST → simple IR)

What the walkthrough reaches for:

| Need | What it stresses | Other v1 workload that also wants it? |
|---|---|---|
| IR node storage (same shape as AST: many small nodes, one-compilation lifetime) | `Arena[T]` again | Same as B2 |
| Multi-arena composition (AST arena + IR arena + symbol table arena, all alive simultaneously, IR references symbols and AST nodes) | Section A stress point | HTTP server with connection Pool + per-request Arena + long-lived metric Pool |
| Deterministic iteration over a parallel pass (codegen functions in parallel, deterministic output for golden tests) | `parallel_fanout` design study + determinism contract | Snapshot tests, reproducible builds, log-output golden tests, any test-emitting workload |
| Builder-pattern for IR construction (long fluent chains; want `mut self` returning `Self`) | Builder ergonomics in Kāra's ownership system | std.http response builder, std.tracing span builder, SQL query builder |
| Pretty-printing with proper indent/line-break threading (Wadler-style) | Coroutine / streaming-formatter primitive | std.json pretty-print, std.tracing structured output, error-diagnostic rendering |

**Stress points specific to codegen that didn't surface earlier:**

1. **Determinism contract on auto-par.** This is the load-bearing one. If codegen-over-functions runs in parallel via `parallel_fanout` and emits a hash to verify a golden file, the iteration order must be deterministic or the test silently fails. Need to check `brainstorming/design_studies/parallel_fanout` for whether this is spec'd. If not, it's a v1 commitment that's currently implicit — and the same gap hits any test-emitting backend workload (snapshot tests, log-output golden tests, reproducible builds).

2. **Builder pattern + ownership.** Long fluent chains (`Builder.new().with_a(x).with_b(y).build()`) work cleanly in Rust because `self` moves through the chain. Kāra's owned-by-default + `mut ref` story should accommodate this but the walkthrough surfaces it as a thing to validate. Every stdlib builder (response, span, request) needs this to be ergonomic.

3. **Pretty-printer / streaming formatter.** Wadler-style pretty-printing wants a lazy/coroutine shape. Kāra has coroutines (ws_idle_holder kata uses them). The question is whether the formatter API in `f"..."` interpolation and std.tracing structured output uses the same coroutine substrate, or whether they're each ad-hoc.

**B4 verdict:** codegen surfaces the determinism contract as the highest-value gap — it's load-bearing for backend test workloads, not compiler-only. Builder ergonomics and streaming formatters are mid-priority and overlap broadly.

### Problem B5. Triaged gap list

Applying the discipline rule. *Backend overlap* column means "some v1 backend workload also wants this" — gap survives v1 triage only if this column is non-empty.

| Gap | Surfaced in | Backend overlap | Disposition |
|---|---|---|---|
| `Arena[T]` + `ArenaRef[T]` proper spec | B2, B3, B4 | JSON / protobuf / HTTP / regex bulk parsing | **Escalate to Section A** — likely v1 (P1) |
| `Symbol`/`Interner` primitive | B2, B3 | HTTP headers / JSON keys / log keys / regex char classes | **Escalate to Section A** — likely v1 (P1) |
| Accumulating diagnostic channel | B2, B3 | Form/request validation, batch imports | **New brainstorm item** — sibling to Section A; not primitive |
| Determinism contract on auto-par | B4 | Snapshot tests, golden tests, reproducible builds | **Substantively present, scattered. Closed under Q2 (2026-06-01).** Four pieces in `design.md` (8828 source-order serialization, 8899 compile-time parallelization-graph determinism, 9565 "deterministic by construction," 9672 par-error source-order precedence) together imply: any user-observable behavior mediated by the effect system is deterministic. **Three follow-ups land as design.md edits, not new primitives:** (i) consolidated "Determinism Contract" section pulling the four scattered pieces into one referenceable place; (ii) one-line `collect_all_vec` ordering clarification at design.md:9612 (input `Vec[Fn() -> ...]` → output `Vec[Result[T, E]]` is element-wise); (iii) `parallel_fanout/findings.md` audit row added for the determinism axis (currently uncovered in the design study that exists to stress-test concurrency). The `for`-loop iteration parallelization rule remains v1.x-deferred via the cost model (design.md:8893) — the determinism contract should explicitly note "for-loop iterations follow the same effect-conflict rule." |
| Visitor-over-`shared struct`-with-`weak` documentation | B3 | dep-graph workloads, GraphQL schemas, ECS back-refs | **Closed under Q3 (2026-06-01).** Existing guidance at design.md:8323 ("upgrade once per scope") covers the canonical traversal pattern. Top-down visit (no parent) and parent-walking traversal both work cleanly. No new gap surfaced; no design.md update required for this row. |
| **AST-rewrite idiom — per-field borrow flag footgun** (new, from Q3) | Q3 walkthrough Case 3 | DOM manipulation, AST-based templating, ORM query rewrite, any tree-rewrite workload | **Design.md spec gap.** The standard tree-rewrite pattern `match parent.field { Lit { .. } => parent.field = new_node }` panics at runtime: the match arm holds a shared borrow on `parent.field` (per design.md:8339 per-field flag tracking) while the body attempts an exclusive write. Fix-idiom: peek-and-drop (`let is_lit = matches!(parent.field, Lit{..}); if is_lit { parent.field = new_node }`). Land as a worked example at design.md:8339 alongside the per-field borrow flag spec. Backend-workload overlap is real — every tree-rewrite use case hits this. |
| **Recursive type representation guidance** (new, from Q3) | Q3 walkthrough Case 4 | Typechecker recursive types; any structurally-recursive data the compiler/runtime needs to deduplicate (schema definitions, GraphQL types, protobuf descriptors) | **Confirms Section A Candidate 2 from a second direction.** Trying to encode `enum Type { Struct { fields: Vec[(Symbol, Type)] }, ... }` with `shared struct` + `weak` produces wrong semantics (`weak Type` models "the type may have been freed," but a typechecker holds types strongly through a symbol table). Right shape: `Pool[Type]` + `Handle[Type]` for the recursive position. Land as a callout in design.md:8330-8335 (the existing Arena/Pool comparison block). |
| Snapshot/restore semantics on `Arena[T]` | B3 | Transactional middleware, request-scoped rollback | **Folds into `Arena[T]` spec** in Section A |
| Multi-arena composition story | B4 | HTTP server with multiple arenas | **Folds into Section A candidate validation** |
| Builder ergonomics in owned-by-default | B4 | std.http / std.tracing / SQL builders | **Likely already works; add walkthrough kata** |
| Streaming pretty-printer / coroutine formatter | B4 | std.tracing structured output, JSON pretty, error rendering | **v1.x** — useful but not v1-blocking |
| Lexer with peek/putback iterator | B2 | All structured-format parsers | **Likely already in std.iter**; verify |

**Compiler-only gaps (deferred per discipline rule):** none surfaced. Every walkthrough need has a backend twin. That's a strong signal — it means the v64 backend-first investment is genuinely paying compounding dividends into adjacent workloads, exactly as the v64 second-order-positive claim predicts.

If a future revision of the walkthrough surfaces compiler-only gaps (e.g. comptime/macro chicken-and-egg for self-hosting), those land here as deferred entries.

---

## Synthesis — what this brainstorm commits to (preliminary; revisit before close)

1. **Section A candidate 4 (split primitives + interner) is the strongest candidate.** Conditional concern resolved under Q1 (2026-06-01): Candidate 1's failure under the AST/NFA lenses is structural to the API shape (`create_fn` constructor callback, `Result` return, `PooledConnection` wrapper), not a hot-path cost that monomorphization could rescue. Candidate 1.5 (Pool with a side-channel `alloc` method) erases two of the three structural costs but reintroduces the teaching-surface failure mode and still pays generation-counter overhead. Candidate 2 / Candidate 4 win on shape and teaching surface. **Confirmed from a second direction under Q3 (2026-06-01):** recursive type representation in a typechecker forces a choice between `shared struct + weak` (wrong semantics — `weak` models "may have been freed," not "deduplicated through a symbol table") and `Pool[T] + Handle[T]` (right semantics). The compiler walkthrough independently lands on Candidate 2's split as the only correct shape for the recursive-data case.

2. **`Arena[T]` + `ArenaRef[T]` should escalate to v1 (P1)** with a real spec, given the JSON / protobuf / HTTP / regex overlap. The roadmap one-liner at `roadmap.md:460-461` is insufficient.

3. **`Symbol`/`Interner` should escalate to v1 (P1)** given the HTTP-header / JSON-key / log-key / regex-char-class overlap. Currently absent from roadmap and stdlib floor.

4. **Glossary fix is required regardless of candidate choice.** The line at `glossary.md:312` conflating `Pool[T]` with AST arenas is a documentation bug — narrow it to "ECS / connection IDs / incremental reuse" and add a separate `Arena[T]` line for bulk allocation.

5. **Determinism contract on auto-par is substantively present but scattered.** Closed under Q2 (2026-06-01): four pieces in `docs/design.md` (lines 8828 / 8899 / 9565 / 9672) together imply that any user-observable behavior mediated by the effect system is deterministic. Three follow-up design.md edits land as docs work, not new primitives: (i) consolidated "Determinism Contract" section, (ii) `collect_all_vec` ordering one-liner at 9612, (iii) determinism-axis row in `parallel_fanout/findings.md`. The cost-model deferral of `for`-loop body parallelization (design.md:8893) is unchanged but the determinism contract should note iterations follow the same effect-conflict rule.

6. **Sibling brainstorm needed for accumulating diagnostics.** Not in scope for v70; flag for v71.

7. **Compiler-only gaps surfaced: none.** The compiler-as-lens audit validates that v64's backend-first investment compounds into compiler-workload coverage — same pattern as v64's data-engineering second-order claim.

## Implementation follow-ons (gate brainstorm close)

Each item below is an Edit / new entry that lands before this brainstorm moves from "synthesis closed" to "closed." None is design work; all are documented commitments the v70 conclusions imply.

1. **`docs/glossary.md:312` — narrow `Pool[T]` line** to "ECS engines, incremental compilation slot reuse, connection IDs." Remove "compilers (AST arenas)." Add a new "**`Arena[T]`**" glossary entry pointing to the bulk-allocation use case (parse trees, frame-scoped data, AST per compilation).
2. **`docs/implementation_checklist/phase-8-stdlib-floor.md` — add two `[ ] P1` checklist entries:**
   - `**P1 — Arena[T] + ArenaRef[T] — bulk-allocation primitive.**` with sub-items for interpreter intrinsic, codegen lowering, drop-at-arena-end semantics, snapshot/restore high-water-mark API.
   - `**P1 — Symbol + Interner — deduplicating string handle primitive.**` with sub-items for interpreter intrinsic, codegen lowering, hash/eq via handle identity, integration with `f"..."` formatter and HTTP-header / JSON-key canonicalization paths.
3. **`docs/design.md:8330-8335` (Arena vs Pool comparison block) — extend** with the recursive-type-representation guidance surfaced under Q3 Case 4 (typechecker recursive types use `Pool[Type]` + `Handle[Type]`, not `shared struct` + `weak`).
4. **`docs/design.md:8339` (per-field borrow flag spec) — add worked example** for the AST-rewrite peek-and-drop idiom surfaced under Q3 Case 3. Walks through the `match parent.field { ... => parent.field = new_node }` footgun and the `matches!()` peek-then-mutate fix.
5. **`docs/design.md` — add consolidated "Determinism Contract" section** pulling the four scattered pieces (8828 source-order serialization, 8899 compile-time parallelization-graph determinism, 9565 deterministic-by-construction, 9672 par-error source-order precedence) into one referenceable place. Q2 closure docs the property; this consolidates it.
6. **`docs/design.md:9612` (collect_all_vec signature) — one-line clarification** that the result `Vec[Result[T, E]]` is element-wise ordered by input position.
7. **`brainstorming/design_studies/parallel_fanout/findings.md` — add a "Determinism" axis row** to the four-language scoring table. Currently absent; Kāra's source-order-on-conflict + position-bound result tuple is a real differentiator vs Java/Python/Rust (where preserving ordering across parallel branches requires user discipline).
8. **Sibling brainstorm `v71_*.md` — accumulating diagnostics primitive.** Triaged in Section B2/B3 as "real backend overlap." Not in v70 scope; flag for next brainstorm.

## Open questions blocking close

- ~~**Bench `Pool[T]` with `max_connections=∞` and `health_check=None`** for AST-allocation hot path.~~ **Closed 2026-06-01.** No bench needed — `Pool[T]` has no codegen path yet (interpreter-only at `src/interpreter/method_call_pool.rs`), and by inspection the failure mode is structural to the API shape (mandatory `create_fn` callback, `Result` return, `PooledConnection` wrapper), not implementation cost. Monomorphization would erase the hashmap-keyed-by-handle and the `Value::Function` dispatch but cannot erase the constructor-callback / Result-unwrap / wrapper costs that show up at every alloc site. Candidate 1 fails the AST/NFA lenses on call-site shape; Candidate 1.5 (Pool + side-channel `alloc` method) erases 2 of 3 costs but reintroduces the teaching-surface bifurcation. See Candidate 1 and Candidate 1.5 bodies in Section A2 for the full cost table.
- ~~**Audit `parallel_fanout` design study** for an explicit determinism contract.~~ **Closed 2026-06-01.** Contract is substantively present in `docs/design.md` (four scattered pieces at lines 8828, 8899, 9565, 9672) and together implies "any user-observable behavior mediated by the effect system is deterministic." Not a v1 primitive gap; lands as three design.md edits: (i) consolidated "Determinism Contract" section, (ii) one-line `collect_all_vec` ordering clarification, (iii) determinism-axis row in `parallel_fanout/findings.md`. The `for`-loop iteration cost-model rule (design.md:8893) remains v1.x-deferred but the determinism contract should note "for-loop iterations follow the same effect-conflict rule" so the deferral doesn't ambiguate the property. See B5 row "Determinism contract on auto-par" for the full disposition.
- ~~**Validate `shared struct` + `#[cyclic]` + `weak` visitor ergonomics** with one written-out walkthrough.~~ **Closed 2026-06-01.** Four-case walkthrough run: (1) top-down visitor with no parent access — clean; (2) parent-walking traversal via `weak` field upgrade — works, matches "upgrade once per scope" guidance at design.md:8323; (3) AST rewrite pass — works for the canonical "parent rewrites child's field" pattern, but the `match parent.field { Lit{..} => parent.field = new_node }` shape panics at runtime due to per-field borrow flag (design.md:8339) holding a read borrow over the arm body; (4) recursive type representation — `shared struct` + `weak` produces wrong semantics; the right shape is `Pool[Type]` + `Handle[Type]`, confirming Section A Candidate 2 from a second direction. Two design.md docs gaps surfaced (AST-rewrite peek-and-drop idiom; recursive-type guidance pointing at `Pool[T]`); both have backend-workload overlap and land as design.md edits, not new primitives. See B5 rows "AST-rewrite idiom" and "Recursive type representation guidance" for full disposition.
- ~~**Confirm `Pool[T]` codegen-path work in `phase-8-stdlib-floor.md:157`** is actually on the v1 critical path.~~ **Closed 2026-06-01.** Pool[T] is formally `P1` in `phase-8-stdlib-floor.md:153` with no `v1.x` / `deferred` marker — per stored priority-tier doctrine ("P1 is NOT v1.x"), `[ ]` sub-items under a P1 parent ship at v1. Codegen-path implementation depends on (i) a new `src/codegen/pool.rs` lowering, (ii) user-`impl Drop` dispatch in codegen (broadly needed for many stdlib types, not Pool-specific — `run_cleanup` currently treats `CleanupAction::Drop` as a trace-only no-op per `src/interpreter/eval_stmt.rs`), (iii) parking_lot::Mutex + condvar runtime extension. None blocked. **Subtle finding:** the Section A Candidate 4 recommendation adds two new P1 primitives (`Arena[T]` + `ArenaRef[T]`; `Symbol`/`Interner`) that are not currently on the Phase 8 checklist. Without explicit `[ ] P1` entries in `phase-8-stdlib-floor.md`, the v70 recommendation is a promise the tracker doesn't reflect — violates the `v1 ships reality not promises` discipline. **Follow-on (gating brainstorm close):** add `[ ] P1` checklist entries for `Arena[T]` + `ArenaRef[T]` and `Symbol`/`Interner` to `phase-8-stdlib-floor.md` before this brainstorm lands.

## Cross-references

- `docs/glossary.md:46` (`Arena Allocation` definition — needs cross-reference fix)
- `docs/glossary.md:312` (`Pool[T]` / `Handle[T]` — needs narrowing)
- `docs/roadmap.md:460-461` (`Arena[T]` / `ArenaRef[T]` line items — need real spec)
- `docs/implementation_checklist/phase-8-stdlib-floor.md:152-159` (`Pool[T]` current implementation state)
- `docs/design.md:8330-8333` (`shared struct` vs Arena rationale)
- `docs/design.md:12591` (`#[cyclic]` + `weak` for `dyn Trait`)
- `docs/design.md:12626` (Arena[T] stdlib type — Phase 8)
- `docs/design.md:12633` (`#[generational_fallback]` annotation — Phase 4+)
- `brainstorming/archive/v64_backend_first_v1_concurrency.md` (priority-tier framing; second-order-positive claim)
- `brainstorming/design_studies/parallel_fanout/` (determinism contract — audit needed)
