# Kāra Compiler Roadmap

Development plan for the `karac` compiler, aligned with [design.md](docs/design.md).

## Core Strategy

1. **Effect types + auto-concurrency first** — the differentiating features, everything else composes on top.
2. **LLVM codegen is the single execution backend.** The tree-walk interpreter (Phase 4) served its original purpose as a semantic-validation step before codegen (Phase 4 → Phase 7) and is retained as a *dev/debug tool* — useful for stepping through compiler internals, validating semantics during compiler work, and any future reflection-style introspection. It is **not** the runtime backend for user-facing `karac repl` / `karac test` workflows. Those workflows use **always-JIT** via LLJIT (`llvm-sys::orc2`): every function lazy-compiled on first call, fast on subsequent calls. Single execution model across `karac build`, `karac repl`, and `karac test` — no semantic divergence between interactive and compiled execution. (Locked 2026-05-05; see brainstorming archive for the alternatives considered and the principle-driven argument for "always-JIT over hybrid tree-walk + JIT.")
3. **Incremental phases** — each phase produces a working compiler for a growing subset of the language.
4. **Diagnostics are incremental** — structured error output is built alongside each phase, not deferred. Every compiler feature ships with its diagnostics. JSON output format exists from the first error the compiler can report.
5. **North star: self-hosting — pulled into v1 as the pivot (2026-06-10).** The Kāra compiler is rewritten in Kāra *before* the long-tail stdlib, not after v1. Self-hosting (Phase 12) is sequenced after the Phase 8 floor + Phase 9 enforcement (both effectively done), Phase 10 targets (mostly done), and the **LLJIT-productionization spike** (core complete 2026-07 — see below); Phase 11 is then built *on* the self-hosted compiler, so every compiler-internal feature (f16 lowering, shape-kinded generics, inline asm, the codegen IR pass) is written once, in Kāra, never twice. Execution order is therefore 8 → 9 → 10 → **LLJIT-productionization** → 12 → 11; numeric order no longer equals execution order. **The LLJIT spike inserts before Phase 12 (owner-confirmed 2026-07-09):** self-hosting's bootstrap loop leans on `karac run`/`test` as the execution path, so making LLVM the single backend (`run`/`repl`/`test` default to the JIT; `run == build` by construction, no interpreter-vs-codegen divergence) is a prerequisite that hardens that path *before* the self-hosted compiler depends on it — otherwise the pivot would inherit the very divergence tax the spike removes. See [Phase 12](#phase-12-self-hosting), the [Phase Dependency Graph](#phase-dependency-graph), and [`spikes/lljit-productionization.md`](spikes/lljit-productionization.md).
6. **Interactive is first-class, not an afterthought.** REPL and Jupyter kernel are positioned as differentiators, not convenience tooling — Kāra is one of few systems-grade statically-typed languages whose interactive surface runs at compiled-binary speed (lazy LLJIT amortizes the ~100 ms cold-compile across cell lifetime; subsequent calls are native code). The advantage over `evcxr`-style recompile-per-cell or JShell's JVM startup tax is **execution model parity** — REPL cells exhibit the same effect / ownership / perf behavior as `karac build` artifacts. Trivial REPL cells (`let x = 1+1`) cost ~100 ms instead of <1 ms — but the cost is *expected and uniform*, never a mystery slowdown; honest framing for users: "REPL has built-in compile latency by design."

---

## Contents

- [Phase 0: Proof of Value](#phase-0-proof-of-value--complete) — COMPLETE
- [Phase 1: Lexer](#phase-1-lexer--complete) — COMPLETE
- [Phase 2: Parser & AST](#phase-2-parser--ast--complete) — COMPLETE
- [Phase 3: Semantic Analysis](#phase-3-semantic-analysis--complete) — COMPLETE
- [Phase 4: Tree-Walk Interpreter](#phase-4-tree-walk-interpreter--complete) — COMPLETE
- [Phase 5: Compiler Query API & Tooling](#phase-5-compiler-query-api--tooling--complete) — COMPLETE
- [Phase 6: Auto-Concurrency Runtime](#phase-6-auto-concurrency-runtime) — COMPLETE (6.1 + 6.2)
- [Phase 7: LLVM Code Generation](#phase-7-llvm-code-generation) — COMPLETE
  - [Phase 7.1: Core Code Generation](#phase-71-core-code-generation--complete) — COMPLETE
  - [Phase 7.2: Compiled Stdlib Types + Layout Codegen](#phase-72-compiled-stdlib-types--layout-codegen--complete) — COMPLETE
- [Phase 8: Standard Library — Floor](#phase-8-standard-library--floor)
- [Phase 8.5: V1 Ship Readiness](#phase-85-v1-ship-readiness) — parallel track (Interactive Development, Build & Dependency Tooling, Discovery)
- [Phase 9: Gradual Verification Enforcement](#phase-9-gradual-verification-enforcement)
- [Phase 10: Additional Compilation Targets](#phase-10-additional-compilation-targets)
- [Phase 12: Self-Hosting](#phase-12-self-hosting) — **the v1 pivot; executes before Phase 11**
- [Phase 11: Standard Library — Long-Tail](#phase-11-standard-library--long-tail) — **built on the self-hosted compiler; END = v1 release**
- [Future: Gradual Verification](#future-gradual-verification-feature-6)
- [Future: Comptime](#future-comptime-compile-time-code-execution)
- [Future: Language Server and Reactive Query-Based Compilation](#future-language-server-and-reactive-query-based-compilation)
- [Resolved Design Primitives](#resolved-design-primitives)
- [Phase Dependency Graph](#phase-dependency-graph)

---

## Phase 0: Proof of Value — COMPLETE

**Goal:** Demonstrate *why Kāra exists* before the compiler is fully built.

Build a hand-compiled demo that shows the effect system + auto-concurrency story end-to-end:

- [x] **Write a Kāra program** (~50 lines) that fetches data from three independent sources, processes it, and writes results — using effect annotations
- [x] **Hand-translate to Rust** showing: (a) the sequential version, (b) the auto-parallelized version the Kāra compiler would generate
- [x] **Benchmark both** — show the speedup from auto-concurrency with zero programmer effort
- [x] **Show the compiler output** — mock the structured JSON diagnostics, concurrency report, and effect query output

This is a *pitch artifact*, not a compiler. It validates the thesis concretely: "here's what the compiler does for you, and here's the output it gives you."

**Done when:** The demo can be presented in under 5 minutes and clearly shows: (1) the programmer writes sequential-looking code with effects, (2) the compiler parallelizes it, (3) the diagnostics explain what happened and why.

---

## Phase 1: Lexer — COMPLETE

**Goal:** Tokenize Kāra source code with the current keyword set.

- [x] All current keywords (struct, enum, trait, impl, effect, reads, writes, etc.)
- [x] All symbols (=>, .., ..=, ?, #, etc.). `&&`/`||`/`!` retained in the lexer for deprecation diagnostics — user code uses `and`/`or`/`not`.
- [x] Span tracking (line, column, byte_offset) on every token
- [x] Numeric literals: hex, binary, octal, underscore separators
- [x] Block comments with nesting
- [x] Character literals: `'a'`, escape sequences (`\n`, `\t`, `\r`, `\\`, `\'`, `\0`), unicode escapes (`\u{1F600}`)
- [x] String interpolation: `f"..."` with `{expr}` blocks and nested brace tracking
- [x] Multi-line strings: `"""..."""`
- [x] Reserved keywords: `where` (gradual verification), `dyn` (trait objects)
- [x] Pipe operator token: `|>` as a distinct token (not `|` followed by `>`)
- [x] 35 integration tests covering all constructs

**Done when:** Every token defined in design.md is recognized, with source location tracking. All tests pass.

---

## Phase 2: Parser & AST — COMPLETE

**Goal:** Recursive-descent parser producing an AST for the core language.

### 2.1: Expressions and Statements
- [x] Literals: integers, floats, strings, booleans, characters
- [x] Operators: arithmetic, comparison, logical, bitwise
- [x] Variable bindings: `let` (immutable), `let mut` (mutable)
- [x] Assignment: `=` for `let mut` bindings
- [x] Blocks: `{ ... }` as expressions
- [x] If/else: `if condition { ... } else { ... }`
- [x] While loops: `while condition { ... }`
- [x] While let: `while let Pattern = expr { ... }`
- [x] For loops: `for item in collection { ... }`
- [x] Loop: `loop { ... }` with `break` / `break value` / `continue`
- [x] Return: `return expr` and implicit returns
- [x] `?` operator: postfix error propagation
- [x] Cast expressions: `expr as Type`
- [x] Tuple access: `tuple.0`, `tuple.1`

### 2.2: Functions and Types
- [x] Function definitions: `fn name(params) -> ReturnType { body }`
- [x] Struct definitions: `struct Name { field: Type, ... }`
- [x] Enum definitions: `enum Name { Variant { fields }, ... }`
- [x] Impl blocks: `impl Type { fn method(self, ...) { ... } }`
- [x] Trait definitions: `trait Name { fn method(self, ...) -> T; }`
- [x] Trait implementations: `impl Trait for Type { ... }`
- [x] Generics: `fn name[T: Bound](x: T)`, `struct Name[T] { ... }`
- [x] `where` clauses: parse after return type/effects/contracts on `fn`, `impl`, `struct`, `enum`, `trait`
- [x] Distinct type declarations: `distinct type Name = BaseType` with optional `where` constraint and `#[derive]`
- [x] Refinement type declarations: `type Name = BaseType where constraint` — numeric comparisons, `len()`, boolean combinators
- [x] Contracts: `requires`/`ensures` clauses on functions; `invariant` on struct bodies
- [x] Default parameter values: `param: Type = expr` syntax; trailing-only rule
- [x] Destructuring in function/closure parameters: `fn add((a, b): (i64, i64))`, struct destructuring

### 2.3: Effects Syntax
- [x] Effect resource declarations: `effect resource UserDB: DatabaseProvider;`
- [x] Effect annotations on functions: `fn f() with reads(UserDB) writes(OrderDB) { ... }`
- [x] Effect groups: `effect group Name = reads(X) + writes(Y);`
- [x] `with` keyword: `pub fn f() with OrderProcessing { ... }`
- [x] `with _` (effect polymorphism): on closures and trait methods
- [x] `transparent` effect modifier: `transparent effect verb traces;`
- [x] Parameterized resources: `effect resource UserDB[user_id: u64];`

### 2.4: Ownership Syntax
- [x] `ref` keyword: `fn f(s: ref String) -> ref String`
- [x] Multi-ref borrow returns: `fn f(a: ref String, b: ref String) -> ref String` (compiler uses conservative overapproximation — return borrows from all `ref` params)
- [x] `mut ref`: `fn f(s: mut ref String)`
- [x] `weak` annotation: `struct Child { parent: weak Parent }`
- [x] `shared struct` and `shared enum`: reference-semantics types with RC

### 2.5: Modules and Visibility
- [x] Imports: `import path.to.Item;` (brace-grouped multi-item, renames, `pub import` re-exports)
- [x] Visibility: `pub` keyword
- [x] Fully qualified paths: `module.Type`
- [n/a] Module declarations: design.md v41 resolved that modules come from the directory tree — no `mod` declarations. Parser rejects `mod name;` with a diagnostic.

### 2.6: Other Syntax
- [x] Match expressions: exhaustive pattern matching with `=>`
- [x] Named/labeled arguments: parse `label: expr` at call sites; store in `CallArg { label, value }` AST node
- [x] Pipe operator `|>`: parse as left-associative binary expression; `_` placeholder in argument position; lower precedence than call `()`, higher than `=`
- [x] `defer`/`errdefer`: parse as statements; `errdefer(e) { ... }` binding form; store cleanup list in AST scope node; no `?` inside block (compile error)
- [x] Subscript syntax: desugar `expr[expr]` into `Index.index` / `IndexMut.index_mut` call in AST; `expr[expr] = expr` assignment form routes to `IndexMut.index_mut`
- [x] Map literals: `["key": val, ...]` syntax; disambiguate from array literals by `:` after first expression
- [x] `seq {}` block: parse as expression; all statements execute in source order; auto-parallelism suppressed
- [x] Closures: `|params| expr` and `|params| { body }`
- [x] Tuples: `(a, b, c)`, destructuring
- [x] Array literals: `[1, 2, 3]`, empty `[]`, nested `[[1, 2], [3, 4]]`
- [x] Attributes: `#[no_rc]`, `#[rc_budget(max: N)]`, `#[concurrency(max_tasks: N)]`
- [x] Unsafe blocks: `unsafe { ... }`
- [x] FFI: `extern "C" fn name(...) effect_list;`
- [x] Layout blocks: `layout name: Collection[T] { group name { fields } }`
- [x] Constants: `const NAME: Type = value;`
- [x] Type aliases: `type Name = ExistingType;`
- [x] Comments: `//`, `/* */`, `///` (doc comments) — handled by lexer

### 2.7: AST Design
- [x] Span tracking: Every AST node carries source location
- [x] Canonicalization-ready: AST structure supports deterministic formatting
- [x] Parser tests: 101 tests validate correct AST construction for all constructs
- [x] Error recovery: Parser continues after errors to report multiple diagnostics

### 2.8: Diagnostics (built alongside parser)
- [x] Structured parse errors with source spans and context
- [x] Multiple error reporting (don't stop at first error)

**Done when:** Every syntactic construct in design.md parses into a well-typed AST. Parse errors include source locations and are validated by tests.

---

## Phase 3: Semantic Analysis — COMPLETE

**Goal:** Validate program correctness. Each sub-phase is independently shippable.

### 3.1: Name Resolution and Scoping
- [x] Symbol table: Track identifiers, types, and scopes
- [x] Module resolution: Resolve `use` imports and qualified paths
- [x] Visibility checking: Enforce `pub` vs private access
- [x] Diagnostics: "undefined variable", "private function accessed from outside module", with suggestions for typos

**Done when:** The compiler resolves all names, reports undefined/private access errors with source locations, and handles multi-module programs.

### 3.2: Type Checking
- [x] Basic type checking: Parameter types, return types, assignment compatibility
- [x] Generic type inference: Infer type parameters where unambiguous
- [x] Trait bound checking: Verify types satisfy trait constraints (includes `Eq` enforcement on `==`)
- [x] Pattern exhaustiveness: Verify `match` covers all enum variants
- [x] Pattern guards: `if EXPR` guard verified as `bool`; guarded arms excluded from exhaustiveness
- [x] `todo()`/`unreachable()`: special-cased in `infer_call`; return `Type.Never`; validate 0-or-1 `str` arg
- [x] Struct/enum field access: Validate field names and types
- [x] Diagnostics: type mismatch errors with "expected X, found Y", missing match arms with suggested additions
- [x] Named/labeled arguments: resolve labels against parameter names; enforce declaration order; error on unknown labels, out-of-order labels, and non-contiguous partial labels
- [x] Pipe operator `|>`: desugar to function call in type-checker; resolve `_` placeholder to left-hand value; union effects of all stages; verify `?` applies to pipe output not whole chain
- [x] `defer`/`errdefer`: verify no `?` inside block; type-check cleanup expr; `errdefer(e)` binding typed as enclosing function's `Err` variant; cleanup effects contributed to enclosing function
- [x] Subscript trait resolution: resolve `Index[Idx, Output]` and `IndexMut[Idx, Output]` trait bounds; verify `ref` / `mut ref` return modes; infer `panics` effect for `index` calls; type-check `.get()` fallible alternative
- [x] Integration tests — pattern guards: guard type error, guarded exhaustiveness error, guard effects
- [x] Integration tests — `todo()`/`unreachable()`: expression position, `panics` effect propagation, runtime panic message format
- [x] `where` clause bound verification: resolve type constraints in generic contexts
- [x] Associated types: declaration in traits, binding in impls, projection syntax `T.Assoc`, equality constraints in `where`
- [x] `Copy` trait: auto-derive validation (all fields must be `Copy`), implicit copy insertion at own-inferred call sites
- [x] Default parameter values: verify pure constant expressions; enforce trailing-only and no cross-parameter references
- [x] Integration tests — named arguments: unknown label, out-of-order label, partial label, UFCS receiver label (error), closure outer vs inner label scoping

**Done when:** Type-incorrect programs are rejected with clear errors. Generic functions type-check correctly. `match` exhaustiveness is enforced. A non-trivial program (100+ lines) type-checks correctly.

### 3.3: Effect System
- [x] Effect inference for private functions: Trace call graph, infer effects from body
- [x] Effect verification for public functions: Check declared effects match inferred effects
- [x] Effect conflict detection: Build conflict table (reads/writes × same/different resources)
- [x] Effect group expansion: Expand named groups to individual effects
- [x] Transparent effect handling: Exclude transparent effects from conflict analysis
- [x] Parameterized resource analysis: Static distinguishability of resource parameters
- [x] Diagnostics: compiler-suggested effect annotations with fix diffs, "undeclared effect originates from..." tracing

**Done when:** Private function effects are correctly inferred across a 5+ function call chain. Public functions with incomplete effect declarations produce errors with exact fix diffs. The conflict table correctly identifies reads/reads as safe and reads/writes on the same resource as conflicting. `karac query effects` returns structured JSON for any function.

### 3.4: Ownership Analysis
- [x] Parameter mode inference: Analyze function body → own / ref / mut ref per parameter
- [x] Move checking: Track consumed values, prevent use-after-move
- [x] Borrow checking: Validate `ref` and `mut ref` usage
- [x] Cycle detection: Analyze type graph for ownership cycles, require `weak` on back-edges
- [x] Diagnostics: "value moved here, used again here" with restructuring suggestions, RC fallback notes with `#[allow(rc_fallback)]` suggestion, RC budget violations

**Done when:** Parameter modes are correctly inferred for a set of test functions covering own/ref/mut ref cases. Use-after-move is caught. RC fallback triggers with a visible performance note. `#[no_rc]` rejects functions that would need RC. `karac query ownership` returns structured JSON.

---

## Phase 4: Tree-Walk Interpreter — COMPLETE

**Goal:** Execute Kāra programs from the AST, validate language semantics without codegen complexity.

- [x] Expression evaluator: Arithmetic, comparison, boolean logic, string operations (scaffolded, needs tests)
- [x] Integer overflow trapping: runtime error on overflow. Explicit-wrapping `wrapping_add`/`wrapping_sub`/`wrapping_mul` implemented + tested on the 64-bit widths (i64/u64/usize), 2026-06-12; the remaining `wrapping_*` ops, narrow/128-bit widths, and the `checked_*`/`saturating_*`/`overflowing_*` families are tracked under § Codegen Optimization (the wrapping methods are also the integer-kernel auto-vec unblocker).
- [x] String interpolation: `f"..."` desugaring and evaluation
- [x] Function/method execution: Call dispatch, UFCS
- [x] Closure execution: Capture semantics, `Fn` invocation
- [x] Pattern matching: Exhaustive match execution
- [x] Pattern guard evaluation: evaluate `if EXPR` guard, skip arm if false
- [x] Ownership simulation: Move semantics, borrow tracking at runtime (infrastructure added; enforcement deferred to Phase 7 codegen)
- [x] Effect tracking: Runtime effect tracking for validation
- [x] Standard library builtins: `print`, `println`, `read_line`, `read_file`, `write_file`, `env.args`, `env.get`, `env.set`, `exit`
- [x] `dbg()`: transparent debug printing (stderr, file/line/expression/value, stripped in release builds)
- [x] `Result`/`Option` and `?` operator (both types, matching error types only — cross-type conversion via `From` deferred to Phase 8), `?.` optional chaining, `??` nil coalescing
- [x] `defer`/`errdefer`: maintain a scope-exit cleanup stack; on normal exit run `defer` list LIFO; on `Err` exit run `errdefer` list first (LIFO), then `defer` list (LIFO)
- [x] `unwrap()` / `expect()` with `panics` effect tracking
- [x] `#[derive(Eq, Hash, Display, Clone)]`: Compiler-generated trait implementations (Eq at typechecker, Clone/Display implicit in interpreter)
- [x] Generic instantiation: Runtime monomorphization simulation (tree-walk interpreter is dynamically typed — generics work naturally)
- [x] E2E tests: `.kara` programs → interpreter → verify output

**Done when:** `karac run examples/word_count.kara` executes correctly and produces expected output. At least 10 end-to-end test programs covering: arithmetic, control flow, pattern matching, error handling with `?`, struct/enum usage, trait method dispatch, and effect-annotated I/O.

---

## Phase 5: Compiler Query API & Tooling — COMPLETE

**Goal:** Machine-friendly compiler interface for AI agents.

Note: Basic diagnostics (`--output=json`, source spans, error suggestions) are built incrementally in earlier phases. This phase adds the *query* interface and *formatter* — tools that expose the compiler's internal analysis programmatically.

### 5.1: Tooling (CLI, Formatter, Testing)

- [x] CLI: `karac build`, `karac run`, `karac check`, `karac fmt`, `karac query`
- [x] `karac check`: Type-check without executing (builds on Phase 3 analysis pipeline)
- [x] `karac fmt`: Canonical formatter with deterministic output
- [x] `--output=json` flag for machine-readable diagnostics across all phases
- [x] `--output=jsonl` streaming mode: newline-delimited JSON, six event types (`build_start`, `phase_start`, `phase_complete`, `phase_skipped`, `diagnostic`, `build_complete`). Strict superset of `--output=json` — collecting all events reconstructs the batch document
- [x] Phase boundary contract for streaming: `lex`, `parse`, `resolve`, `typecheck`, `effect`, `ownership` emit observable phase events in Phase 5. `concurrency` adds its events in Phase 6; `codegen` adds its events in Phase 7. Phase names are part of the public contract — renaming is a breaking change for AI clients
- [x] Fail-fast semantics under streaming: each phase runs unless its immediate predecessor produced zero usable output (predecessor-usable-output rule). Skipped phases emit `phase_skipped` with a `reason` string and a `blocking` array of diagnostic IDs from prior phases. Per-diagnostic `phase` field added to both streaming and batch modes (non-breaking)
- [x] Diagnostic snapshot tests: golden-file tests for error messages — freeze error output format and catch unintended regressions
- [x] Fuzz testing: `cargo-fuzz` harness for lexer and parser — catch panics, hangs, and crashes on malformed input
- [x] `karac query effects`: Return inferred + declared effects for a function
- [x] `karac query ownership`: Return parameter modes and RC values

### 5.2: Language Features

- [x] Labeled loops: `label: for/while/loop`, `break label`, `continue label`, `break label expr`
- [x] Named/labeled function arguments: `name: expr` at call sites, declaration-order enforcement, contiguity validation
- [x] `seq {}` block: suppress auto-parallelism, block expression semantics
- [x] Const generics — full surface: `[T, const N: i64]` declarations; `i64` / `i8`–`i128` / `bool` / `char` / fieldless-`enum` permitted param types; const-expression instantiation (`Array[T, N + 1]`); const-expression bounds in `where` clauses (`where N >= 0`); call-site inference for const params in argument positions; explicit-only solving for return-type-only and bounds-only const params; checked-arithmetic evaluation at type-check time. Spec in `design.md` § Type Inference > *Const generic parameters*. Const-`fn` and user-code calls in const-arg position remain deferred to comptime.

### 5.3: Advanced Diagnostics

- [x] Error return traces: ring buffer (depth 64) at each `?` site; `"error_return_trace"` field in JSON output; pushed on Err/None, cleared on Ok/Some
- [x] Enhanced per-SCC effect diagnostics: Tarjan's SCC detection, full effect resolution trace in `"mutual_recursion_groups"` JSON field

**Done when:** An AI agent can: (1) compile a program and get structured JSON errors, (2) apply the suggested fix diffs, (3) query the compiler for effect/ownership decisions, (4) format code canonically so diffs are semantic-only, (5) consume a streaming build in real time via `--output=jsonl` and react to each phase's completion — or stop reading at the first failing phase — before the full build finishes. All query and diagnostic outputs are valid JSON matching a documented schema; the streaming mode is a strict superset of the batch mode.

---

## Phase 6: Auto-Concurrency Runtime — backend-first v1 (6.1 + 6.2 COMPLETE; 6.3 core SHIPPED — v1-launch-gating items done)

**Goal:** Compiler-driven parallel execution using effect analysis. Under the v64 backend-first decision (2026-05-09), Phase 6.3 (network event loop + state-machine transform for network-boundary functions) is promoted from v1.1 to v1, with the concurrency target staged to 1M+ idle connections per process — see [`design.md § v1 Positioning — Backend-First`](design.md#v1-positioning--backend-first) and [`brainstorming/archive/v64_backend_first_v1_concurrency.md`](../brainstorming/archive/v64_backend_first_v1_concurrency.md).

### 6.1: Concurrency Analysis — COMPLETE
- [x] Data dependency graph: Build dependency graph from variable usage
- [x] Effect conflict analysis: Identify non-conflicting effect sets for parallelization
- [x] Task granularity heuristics: `ParallelGroup.is_trivial` flag marks pure-computation groups; codegen can skip thread dispatch for trivial groups
- [x] Sync point insertion: Insert join points where data dependencies require it
- [x] `seq {}` block enforcement: suppress auto-parallelism within block; execute in source order
- [x] Parameterized-resource distinctness rules: conservative collapse — two parameterized resources are distinct only when their partition key is a distinct literal or a variable provably bound to different values. When distinctness is ambiguous, the compiler serializes (safe default). Documented in implementation_checklist/.
- [x] Concurrency report: `karac query concurrency`
- [x] `karac query concurrency`: Return parallelization decisions with reasoning (moved from Phase 5 — requires concurrency analysis)

**Done when:** Given a function with three independent `reads` on different resources, the compiler identifies them as parallelizable. `karac query concurrency` shows the parallelization decision with reasoning. Given a function with conflicting `writes` on the same resource, the compiler correctly serializes them.

### 6.2: v1 Runtime (Blocking I/O) — COMPLETE
- [x] Parallel execution: `par {}` block spawns concurrent branches via `std.thread.scope`
- [x] Structured cancellation: Cancel sibling tasks on first error via `AtomicBool` flag (fail-fast)
- [x] Sequential fallback: `karac run --sequential` disables parallel execution in par blocks
- [x] Zero-cost when unused: No thread spawning for programs without `par {}` blocks, or single-statement par blocks
- [x] Output ordering: branch outputs merged in source order (deterministic)
- [x] Work-stealing task scheduler: Fixed-size thread pool (min(branches, available_parallelism) workers); atomic work-distribution counter; no external dependencies
- [x] Cooperative cancellation: Branch functions accept cancel_flag parameter; cancel checked at branch start. Effect-boundary cooperative checks (mid-branch insertion) deferred to implementation_checklist/.
- [x] Completion wins cancellation: a branch already executing its body completes naturally (cancel check is at entry, not mid-body). Full effect-boundary granularity deferred.
- [x] Cascading cooperative cancellation: works at branch granularity (outer cancel observed at nested branch start). Mid-execution propagation into running nested pars deferred.
- [x] Scheduler minimum invariants: no lost work (atomic counter ensures every branch is picked up exactly once); cancel eventually observed (checked before each new branch pickup); termination guaranteed (workers exit when counter >= count or cancel set); deadlock-free (no locks in hot path, only atomic operations).
- [x] `collect_all`: tracked as a language feature in implementation_checklist/phase-6-runtime.md. Surface = design.md's *function* form (`collect_all_vec` homogeneous `Vec[Fn() -> Result[T,E]] -> Vec[Result[T,E]]` + the heterogeneous fixed-arity `collect_all` tuple), dispatched compiler-side like `dbg`/`spawn` — NOT a `collect_all { }` block keyword (that framing was stale). `collect_all_vec` slice 1a (front-end + interpreter + codegen gate) shipped 2026-06-09; codegen slice 1b open.

**Done when:** A benchmark program with three independent I/O calls runs ~3x faster with auto-concurrency than with `--sequential`. Cancellation works: if one branch fails, siblings are cancelled and the first error is returned. Pure programs have zero scheduling overhead (measured).

### 6.3: v1 Runtime (Network Event Loop) — core SHIPPED (v1-launch-gating items done; debug-field + cost-model tails deferred)

> **Status reconciled 2026-07-13.** The five checkboxes below are the v1-launch-gating core; all shipped and were reconciled in the tracker 2026-07-11 (the roadmap boxes had simply not been flipped). The remaining open items are the `KaracWaitTarget` debug-field population (deferred with its only consumer, `KARAC_DEADLOCK_CHECK`) and the v1.x cost-model / alias-metadata tails (§ *Async-scheduler — remaining* / *Language features* in the tracker). Detailed evidence lives in [`implementation_checklist/phase-6-runtime.md`](implementation_checklist/phase-6-runtime.md) — the single source of truth.

**Pre-implementation design audit (P0, 4-6 weeks before runtime engineering starts). ✅ DONE.** Full `design.md` subsections landed for all six commitments (see [`design.md § Network Event Loop and State-Machine Transform`](design.md#network-event-loop-and-state-machine-transform), the six-subsection block opening "Six design commitments together specify the v1 network-event-loop story"): state-machine transform (network-boundary only); RAII-across-yield as compile error; panic-during-suspend semantics; debugger contract for parked tasks; FFI-across-yield; RC-drop ordering across yield points. The audit prevents language-surface decisions from being made under engineering deadline pressure — the cheapest time to lock the rules is *before* the codegen work starts. Tracker: [`implementation_checklist/phase-6-runtime.md`](implementation_checklist/phase-6-runtime.md).

- [x] Event loop integration: epoll (Linux) / kqueue (macOS) / IOCP (Windows) for network I/O. **DONE** — all three backends shipped; Windows IOCP validated natively 2026-06-17 ([`spikes/windows-iocp-eventloop.md`](spikes/windows-iocp-eventloop.md)); macOS kqueue closed by design.
- [x] Effect-routed execution: Compiler routes `sends(Network)` / `receives(Network)` to event loop instead of blocking on OS thread. **DONE** (reconciled 2026-07-11) — effect-routed task parking, all 16 sub-slices shipped (runtime FFI → parked-task ABI → poller → dispatcher → codegen identification+lowering → E2E → stdlib Tcp/WebSocket → Windows IOCP).
- [x] Task parking: Network I/O tasks park without blocking threads, resume on completion via the event loop. **DONE** (reconciled 2026-07-11; verified E2E — `ws_idle_holder` active bench 150 sent / 150 echoed / 0 failed, 1M+ idle at 12.1 KB/conn). Parking is implemented via event-loop fd registration + the default-on A2 LLVM-coroutine transform, **not** the `KaracFrame::wait_target` debug field — that field still ships single-variant `None` (`runtime/src/lib.rs:480`). **Populating `KaracWaitTarget` with real `PeerTask`/`IoHandle` variants remains deferred alongside its only consumer, `KARAC_DEADLOCK_CHECK`** — see [`implementation_checklist/phase-7-codegen.md` § `KARAC_DEADLOCK_CHECK`](implementation_checklist/phase-7-codegen.md), blocked 2026-06-05. In v1 no real blocking primitive parks indefinitely (`Mutex[T]` is type-shape-only), so an "all workers asleep" deadlock is neither reachable nor distinguishable from idle — nothing to detect until a blocking primitive lands, hence the debug field stays inert.
- [x] **RAII-across-yield as compile error** (promoted from warning under v64). **DONE** — implemented in `src/raii_check.rs` (1125 LOC), wired as the `raii_across_yield_check` phase in `src/lib.rs`, 39 tests in `tests/raii_check.rs`; spec in [`design.md § RAII Across Yield Points`](design.md#raii-across-yield-points). Functions with `sends(Network)` / `receives(Network)` in their effect set cannot hold a non-cancel-safe resource (e.g., `MutexGuard`, file handle without `cancel_safe` marker) across a suspension point. Resources that opt into cancel-safety via the `CancelSafe` marker trait are permitted; everything else is a hard error with a fix-it suggesting the lock-narrowing or scope-restructuring shape.
- [x] State machine transform: For network-boundary functions only. **DONE** — the A2 LLVM-coroutine network-async transform (`src/codegen/coro.rs`, default-on; spike [`network-async-coroutine-transform.md`](spikes/network-async-coroutine-transform.md)); LLVM CoroSplit handles arbitrary control flow within a network-boundary function by construction. Full-hybrid lowering of arbitrary `suspends` functions remains post-v1 (see [`deferred.md § Full-Hybrid State-Machine Transform`](deferred.md#full-hybrid-state-machine-transform-arbitrary-suspends-functions)).

**Concurrency staging — 100K → 250K → 1M+ (public v1 launch gated on M3).**

| Milestone | Target | Status |
|---|---|---|
| **M1** | 100K stable idle connections | ✅ done — subsumed by M3 (1M+), exceeded 20× |
| **M2** | 250K stable idle connections | ✅ done — comparator set measured at 250K; Kāra holds 1M+ with no P99 cliff |
| **M3 (count)** | **1M+ stable idle connections** | ✅ done — 1M & 2M idle (arm64), 1M idle (x86), 1M active cross-box, 0 failed, 12.1 KB/conn scale- & ISA-invariant ([`bench/REPORT.md`](../examples/ws_idle_holder/bench/REPORT.md)) |
| **M3 (parity)** | cross-platform parity | ✅ done — **Windows IOCP shipped + validated natively 2026-06-17** (10k loopback functional run + a 250k churn re-validation, zero handle/socket leak, no wedge; multi-shard default — [`docs/spikes/windows-iocp-eventloop.md`](spikes/windows-iocp-eventloop.md)); macOS kqueue closed (functionally validated, scale Linux-only by design); Linux file-I/O io_uring is separate P1 work — phase-8, not a parity item. A 1M-scale Windows idle-hold run is a future scale check, not a correctness gate (concurrency correctness surfaces at small N) |
| **v1 public launch** | **gated on M3 parity** | ✅ unblocked — M3 cross-platform-parity clause cleared (2026-06-17) |

The 1M+ headline number is consolidated reality at launch — "Kāra ships at 1M+" rather than "ship at 100K, promise 1M". CI benchmark gates run against the flagship demo at every PR — the `bench-gate` job (shipped 2026-07-11: `bench_gate.py` + a committed baseline, running the harness at a CI load tier since CI can't hold 1M): a regression on steady-state P50/P95/P99/P99.9 + per-connection density beyond the per-metric tolerance (tight on the machine-invariant density, wider on the noisy shared-runner latency) blocks merge without explicit override + justification. Correctness (all connections established, zero failures) is a separate non-overridable gate.

**Flagship demos (verification gates).** Layered to insure the launch:

- **Demo 1 (P0): minimal HTTP+WebSocket server** — proves runtime can hold 1M+ idle WebSocket connections under TLS.
- **Demo 2 (P0): Parallax (full)** — proves auto-concurrency under realistic load (four upstreams + provider story). Parallax-lite is the conditional fallback if cost-model tuning has not resolved by launch.
- **Demo 3 (P1): data-engineering pipeline** (Kafka → S3 → DuckDB-shape) — proves the compounding-into-other-personas claim that v64 stakes (REPL / data-eng share the same runtime floor).

**Done when:** M3 gate clears — flagship demo (Parallax or Parallax-lite) sustains 1M+ concurrent idle connections per process on Linux + macOS + Windows under HTTPS + WebSocket; CI benchmark suite enforces no >5% regression. Pre-implementation design audit subsections are landed in `design.md` and `implementation_checklist/phase-6-runtime.md` reflects shipped status. The same server code runs identically (correct output) under `--sequential` mode, just slower.

---

## Phase 7: LLVM Code Generation — COMPLETE

**Goal:** Compiled, high-performance output replacing the tree-walk interpreter.

Phase 7 splits into two sub-phases by the nature of the work:

- **7.1 Core Code Generation** — language-construct codegen: types, control flow, generics, closures, RC, FFI, par blocks. COMPLETE.
- **7.2 Compiled Stdlib Types + Layout Codegen** — codegen for `Array[T, N]`, `Vec[T]`, `String` (memory layout and minimum methods only; full API surface remains in Phase 8), plus layout codegen which is blocked on them.

**Rationale for 7.2.** The original roadmap deferred all stdlib types to Phase 8 ("operates against the minimal stdlib surface introduced as interpreter builtins in Phase 4"). Layout codegen revealed this boundary was imprecise: `layout entities: Vec[Entity] { ... }` targets a collection type that must be compiled — not an interpreter builtin — and layout codegen itself is codegen, unambiguously Phase 7 work. Splitting the phase keeps codegen-work in the codegen phase and leaves Phase 8 narrowly scoped to API completeness (full method sets, iterator traits, I/O wrappers, provider impls).

### Phase 7.1: Core Code Generation — COMPLETE


- [x] LLVM IR emission: Integrate `inkwell` crate (LLVM 18, `inkwell` 0.9, optional `llvm` feature)
- [x] Function codegen: Translate functions to LLVM IR (arithmetic, control flow, recursion, `main` → `i32`)
- [x] Struct/enum codegen: Structs as LLVM struct types, enums as tagged unions (`{ i64 tag, i64... }`)
- [x] `karac build`: wire CLI build command to LLVM codegen → object file → native executable via system linker
- [x] Generic monomorphization: Generate specialized code per concrete type
- [x] Effect polymorphism resolution: Resolve `with _` through monomorphization (moved from Phase 3.3 — requires monomorphization infrastructure)
- [x] Closure compilation: Function pointer + captured environment
- [x] RC codegen: Reference counting increment/decrement insertion
- [x] Shared types: `shared struct` reference semantics with RC (moved from Phase 4)
- [x] Shared enums: `shared enum` with same RC semantics (moved from Phase 4)
- [x] RC fallback detection (Phase 1): triggers 1 (branch-divergent re-use after consume) and 2 (closure capture + outer use). Trigger 3 (container store + later use) deferred — see implementation_checklist/.
- [x] RC budget enforcement: `#[no_rc]` per-function, `@no_rc` per-type, `#[allow(rc_fallback)]` to suppress notes. `#[rc_budget(max: N)]` module-level deferred — needs module-level attribute parsing.
- [x] Rc vs Arc (Phase 2): per-function pass that promotes any Rc binding whose use-site lies inside a `par {}` block to Arc. Conservative live-range overlap; one decision per value.
- [x] Effect-based parallelism codegen (MVP): explicit `par {}` blocks lower to a `karac_par_run` call into the bundled runtime library (`libkarac_runtime.a`, statically linked). Branches run on one OS thread each; join before scope exit; captures by value. Cancellation, error propagation, and auto-parallelization of non-`par` regions deferred — tracked in implementation_checklist/ Phase 7. See [design.md § Runtime Distribution](design.md#runtime-distribution).
- [x] FFI codegen: C-compatible ABI for `extern` functions
- [x] Unsafe blocks: Disable ownership/bounds checking within unsafe

### Phase 7.2: Compiled Stdlib Types + Layout Codegen — COMPLETE

Scope: memory layouts and the minimum method set needed to exercise those layouts and make layout codegen possible. Full method sets (`map`, `filter`, `fold`, `retain`, `split`, string formatting, etc.) remain in Phase 8.

- [x] `Array[T, N]` codegen: fixed-size array as LLVM `[N x T]`; construction via literals, indexing (`a[i]`) with bounds check, `.len()` (compile-time constant fold), `for` iteration. Zero-init constructor (`new` / `[0; N]` repeat) deferred.
- [x] `Vec[T]` codegen: heap-allocated `{ ptr data, i64 len, i64 capacity }`; `Vec.new()`, `push` (with 2x/floor-4 growth), `pop`, `.len()`, indexing `v[i]` with bounds check, `for` iteration, scope-exit buffer free
- [x] `String` codegen: heap-allocated UTF-8 buffer `{ ptr, i64 len, i64 cap }`; `String.new()`, `.len()`, `push_str`, string literals as `{ global_ptr, strlen, 0 }` (cap=0 = static). Concatenation via `Add` and comparison deferred to Phase 8.
- [x] Drop/RC integration: owned Vec/String go out of scope → free the backing buffer (cap > 0 check skips static string data). `shared struct` RC interaction unchanged.
- [x] Move and ref semantics for collection parameters: `fn f(v: Vec[T])` moves (pass by value); `fn f(v: ref Vec[T])` borrows (pass pointer, callee reads through indirection). Works for Vec, Array, String. `mut ref` type-lowered but method mutation not yet wired.
- [x] Layout validation pass: resolver links `LayoutDef` → `StructDef`, validates field existence, rejects duplicates across groups, warns on unassigned fields, validates `split_by_variant` only on enums
- [x] Layout codegen: SoA (struct-of-arrays) physical representation. Layout-annotated `Vec[T]` compiles to `{ ptr_g0, ptr_g1, ..., i64 len, i64 cap }` with one heap allocation per group. Push decomposes struct into group fields; growth reallocates each group independently. SoA field-access translation (`entities[i].position` → group-indexed load) and iteration are future follow-ups.

**Done when:** `karac build examples/word_count.kara` produces a native binary that runs correctly. A layout-annotated program compiles to SoA memory and `karac query layout` (or equivalent introspection) shows the physical grouping. The binary is within 2x performance of equivalent hand-written Rust for a compute-bound benchmark (the gap is IR emission quality — naive emission without inlining hints, `noalias`/`nsw`/`nuw` flags, or LTO — not a fundamental limit; performance parity is the Phase 11 codegen-optimization goal). Auto-concurrency works in compiled output (not just interpreter).

---

## Phase 8: Standard Library — Floor

**Goal:** Floor standard library — the surface every non-trivial program needs regardless of domain. Full method sets for collections and core types, complete trait surface (operators, conversions, iterators, hash, ordering), I/O wrappers, providers, and auto-concurrency codegen, plus the universal modules `std.json`, `std.time`, `std.path`, `std.error`, `std.mem`, `std.bytes`, `std.cmp`, `std.hash`. Built on top of the compiled types from Phase 7.2.

Note: Core stdlib types (`Option`, `Result`, `Vec`, `String`, `Array[T, N]`) are introduced as interpreter builtins in Phase 4 with minimal APIs, and their codegen (memory layout + minimum method set) ships in Phase 7.2. Phase 8 adds the full method sets, plus the remaining collections, iterator traits, operator traits, I/O with effect annotations, error conversion (`From` trait for `?`), and provider implementations. **This phase owns the floor API surface, not type codegen and not domain-specific stacks.**

**Scope boundary:** Domain-specific stdlib (numerical/data-science, security, embedded primitives, codegen IR optimization pass) ships later in [Phase 11: Standard Library — Long-Tail](#phase-11-standard-library--long-tail). The split lets v1 ship semantically locked (Phase 9) and target-complete (Phase 10) before the long-tail stdlib lands; full v1 release is at the end of Phase 11. **v64 reshape (2026-05-09):** the backend-platform bundle (`std.http`, TLS, WebSocket, `std.tracing`, HTTP/2, `std.regex`, `std.process`, protobuf, file-system event loop, `Pool[T]`, backpressure primitives) was lifted from Phase 11 long-tail into Phase 8 floor under the backend-first lead-persona decision — see § Backend Platform below.

### Collections (Full APIs)

> **v1 design property — collections monomorphize.** `Vec[T]`, `Map[K, V]`, `Set[T]`, and future v1 collections (e.g., `BTreeMap[K, V]`) emit one specialized implementation per concrete type tuple at codegen, like Rust's `std::collections::HashMap[K, V]`. The original v0 design used a type-erased C runtime (function-pointer dispatch on hash/eq, byte-blob storage); v1 shifts to monomorphized source compiled per user crate. Eliminates indirect-call tax on hot collection operations (~25% of the Karac-vs-std hash_map gap measured 2026-05-06); restores LLVM's optimizer reach into collection internals. `libkarac_runtime.a` shrinks accordingly to non-monomorphizable primitives. See [`design.md § Generics and Monomorphization Strategy`](design.md#generics-and-monomorphization-strategy) for the design lock; trait-bounds-at-codegen enforcement is a P0 prerequisite (currently parsed/validated but not enforced — see `implementation_checklist/phase-7-codegen.md`).

- [ ] `Vec[T]` — full method set on top of Phase 7.2 codegen: `map`, `filter`, `fold`, `retain`, `sort`, `reverse`, `extend`, `concat`, iterator impls. Monomorphized per concrete `T`.
- [x] `Map[K, V]` — codegen + full API: hash table representation, `insert`, `get`, `remove`, `contains_key`, iteration. Monomorphized per concrete `(K, V)` tuple — direct hash/eq calls, full inlining, no function-pointer indirection.
- [x] `Set[T]` — codegen + full API: unique-value container built on `Map` infrastructure. Monomorphized per concrete `T`.
- [ ] `String` — full method set on top of Phase 7.2 codegen: `split`, `replace`, `trim`, `to_uppercase`, `chars()`, format specifiers, etc.
- [ ] `StringSlice` — borrowed view into a `String` (pointer + offset + length); zero-copy parsing/splitting
- [ ] `InternedString` — deduplicated handle via global intern table; O(1) equality
- [x] `Slice[T]` — full read-only + in-place method surface: `len`, `is_empty`, `first`, `last`, `get(i) -> Option[ref T]`, `contains`, `binary_search`, `chunks(n)`, `windows(n)`, `split_at(i)`, `sort`, `sort_by(cmp)`, `reverse`, `fill`, `swap(i, j)`. Typechecker `infer_slice_method` handles `Type::Slice { element, mutable }` dispatch; interpreter `eval_method_call` pattern-matches `Value::Array` for each arm with fallthrough for non-Array objects. `value_compare` free function added since `Value` does not implement `Ord`. 14 typechecker tests + 14 interpreter tests added.

### Core Types
- [x] `Option[T]` — nullable values (enum)
- [x] `Result[T, E]` — error handling (enum)
- [x] `ref_eq(a, b)` — reference identity comparison for `shared` types (free function, returns `bool`). `#[compiler_builtin]` in `runtime/stdlib/intrinsics.kara` + prelude; typecheck `infer_ref_eq_intrinsic` requires the same `shared` type on both args (non-shared → error pointing at `==`); interpreter compares `Arc::ptr_eq`, codegen emits `icmp eq` on the two heap pointers (non-consuming). interp==JIT==build, valgrind-clean.

### Operator Traits
> **Slice 1 shipped** (commit `1c8cb26`): trait registration + impl-table infrastructure (`env.impls_by_trait`, `find_impl`, `find_from_impl`); ~150 built-in stdlib impls (arithmetic, bitwise, Eq/Ord, String Add); arithmetic + `Neg` lowered through `src/lowering.rs`; resolver restriction on user operator-trait impls; `From[T]` dispatch + 19 numeric widening impls; `?` cross-error conversion via typechecker side-table.
>
> **Slice 2 shipped:** operator lowering extended to equality (`==`/`!=`), comparison (`<`/`<=`/`>`/`>=`), bitwise binary (`&`/`|`/`^`/`<<`/`>>`), and unary `~` (bitwise not) plus `not` (logical not) — all route through `Call(Path([T, method]))` and the interpreter/codegen fast-paths. Short-circuit `and`/`or` and range `..`/`..=` stay as `Binary` deliberately. v1 comparison shortcut: `<` lowers to `T.lt` directly (bool-returning) instead of `Ord.cmp(...).is_lt()` — the `Ordering`-detour form lands alongside Ord derivation. Eq/Ord impls register `ne`/`lt`/`le`/`gt`/`ge` as callable methods for API symmetry with `add`/`sub`; type-receiver method calls on primitives (`i32.lt(a, b)` etc.) route through the same fast-path as the lowered form. User-defined `impl Eq for MyStruct` / `impl Ord for MyStruct` are now accepted by the resolver and drive `==`/`<` dispatch through the lowering pass (`TypeCheckResult.trait_impls` exposes the registered (trait, target) set). `!=` on user types desugars to `not T.eq(a, b)` — user Eq impls only need to provide `eq`. Codegen gained a user impl-block pass: each method becomes an LLVM function named `Type.method`, and both `Call(Path([T, m]))` and receiver-form `obj.method(args)` route through it — so user-type operator dispatch works end-to-end through LLVM, not just the interpreter.

- [x] `Add`, `Sub`, `Mul`, `Div`, `Rem`, `Neg` — arithmetic (`a + b` lowers to `Add.add(a, b)` in the lowering phase, after type checking). Homogeneous in v1 — `fn add(self, rhs: Self) -> Self`, no associated `Output`, no heterogeneous `Rhs`. Typed-variable-to-typed-variable mixes require explicit `as` cast (`i32 + i64` with both operands typed is an error); literal-involved promotion is permitted (`arr + 1`, `x: i32; x + 5` — the literal takes the typed operand's type). *(Slice 1: numeric primitives + String Add lowered; effect tracking for String Add's `allocates(Heap)` pending. Literal promotion lands with Phase 11 numerical stdlib.)*
- [x] `Eq`, `Ord` — `a == b` lowers to `T.eq(a, b)`; `a != b` to `T.ne(a, b)`. For comparison, v1 takes a shortcut: `a < b`/`a <= b`/`a > b`/`a >= b` lower directly to `T.lt`/`T.le`/`T.gt`/`T.ge` (bool-returning), sidestepping the `Ord.cmp(a, b).is_lt()` detour through `Ordering`. `Ord` as a derivable trait (lexicographic field/variant-declaration-order comparison) works for the OPERATORS on `#[derive(Ord)]` structs/enums, AND for the `.cmp() -> Ordering` METHOD form: a derived-Ord `p.cmp(q)` resolves in the typechecker (`expr_method_call` intercept on `type_supports_ord`, gated to the derived case) and evaluates via the SAME lexicographic order the operators use — `value_compare` (interpreter) / the `karac_cmp_<T>` comparator + sign-select (codegen `compile_user_cmp_to_ordering`). This unblocks `min`/`max`/`clamp` and `sort_by` on struct/enum types (their bodies call `.cmp`). The relational methods borrow both operands (`ref self, other: ref Self`), so `p.cmp(q)` no longer false-rejects on reuse under the ownership checker (`use_classifier` + `borrow.rs` treat `is_relational_operator_method` calls as reads). Remaining gap: `Variant.cmp(x)` with a bare qualified-variant LITERAL receiver (`Priority.Low.cmp(...)`) — a pre-existing qualified-variant-receiver parse/dispatch limitation, orthogonal to Ord (bind to a variable as a workaround). *(Slice 2: interpreter `dispatch_lowered_op` + codegen `compile_assoc_call` extended with method-name maps. `.cmp` method form: Phase 8 Eq/Ord slice.)*
- [x] `BitAnd`, `BitOr`, `BitXor`, `Shl`, `Shr`, `Not` — bitwise operators on integer primitives and `bool`. `and`/`or` stay as distinct short-circuit keywords (not trait-dispatched) — their semantics can't be faithfully expressed as a strict method call. *(Slice 2: `~int` → `T.not`, `not bool` → `bool.not`; runtime value disambiguates `UnaryOp::Not` vs `BitNot` in interpreter, `type_name == "bool"` disambiguates in codegen.)*
- [ ] `Index[Idx]` / `IndexMut[Idx]` with associated `type Output` — indexing operator (`a[i]`, `a[i] = v`); `Pool[T]` implements `Index[Handle[T]]` with `Output = T`; range indexing (`a[lo..hi]`) via separate `Index[Range[i64]]` impl with `Output = Slice[T]`. *(Trait names registered in slice 1; no impls yet.)*
- [x] `Display` — string conversion for `f"..."` interpolation (`to_string()`). A user `impl Display for T { fn to_string(ref self) -> String { … } }` is dispatched by `f"{x}"` and `x.to_string()` in both backends; `#[derive(Display)]` synthesizes a `Type { field: value, … }` renderer; a non-Display type in an f-string is rejected at typecheck (`type 'T' does not implement Display; cannot interpolate in f-string`); a generic `fn f[T: Display](x: T) { f"{x}" }` dispatches through the bound to the concrete `Display` (interpreter + codegen). *(Slice 1 registered the trait; the impl/derive/dispatch surface shipped incrementally. A generic-mono tail `f"…"` return double-freed in codegen — the mono path lacked the InterpolatedStringLit-tail cap suppression `compile_function` has — fixed B-2026-07-09-18.)*
- [ ] Stdlib implementations: numeric primitives (`i8..i64`, `u8..u64`, `usize`, `isize`) for all arithmetic/comparison/bitwise traits; `f32`/`f64` for arithmetic and `PartialEq`/`PartialOrd` only (no `Eq`/`Ord`/`Hash` — IEEE NaN); `F32`/`F64` total-order wrappers implementing `Eq`/`Ord`/`Hash` with NaN sorting last; `String` for `Add` (heap concatenation, `allocates(Heap)`); `String`/`StringSlice`/`InternedString` for `Eq`/`Ord`; `Vec[T]`, `Option[T]`, `Result[T, E]`, tuples for `Eq`/`Ord` under the obvious conditional bounds. *(Slice 1: primitives + String + F32/F64 registered; `PartialEq`/`PartialOrd` for `f32`/`f64`, `StringSlice`/`InternedString`, generic `Vec`/`Option`/`Result`/tuples pending.)*
- [x] Compound assignment (`+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`) desugars to `a = a op b` in v1 — no separate `AddAssign` etc. traits. Deferred additive extension.
- [x] **Resolver restriction:** user-defined `impl Add for MyType` (and peers) rejected in v1 with a clear diagnostic pointing at the stdlib trait. Restriction is a one-line feature flag; lifting it is a non-breaking additive change. Associated `Output` + heterogeneous `Rhs` land alongside the lift for mixed-type arithmetic (SIMD, decimal, duration).
- [x] **No `impl Add for Vec[T]`.** `vec1 + vec2` is a compile error; diagnostic points at `.concat(other)` or `.extend(other)`. Ambiguity between concatenation and elementwise addition is deliberate.

### Conversion Traits
- [x] `From[T]` / `Into[T]` — infallible conversions; blanket impl derives `Into` from `From`. *(Slice 1: `T.from(x)` dispatch shipped via source-typed lookup; user `impl From` resolves and runs. Slice 3a: `.into()` with expected-type threading at let-annotation, let-else-annotation, assignment, call-arg, return, and function-body-final positions — rewritten to `Target.from(x)` by the lowering pass via `TypeCheckResult.into_conversions`. Slice 3b: resolver rejects user `impl Into` / `impl TryInto` with a suggestion to implement `From` / `TryFrom` instead.)*
- [x] `TryFrom[T]` / `TryInto[T]` — fallible conversions with associated `Error` type. *(Trait names registered in slice 1; no impls or dispatch yet.)*
- [x] `?` cross-error-type propagation via `From` impl chain. *(Slice 1: typechecker validates, `TypeCheckResult.question_conversions` side-table records target err type, interpreter calls `<Target>.from(e)` at propagation. Codegen shipped: `compile_question` reads the side-table, calls the user-impl `<Target>.from(e)` at the propagation site, and repacks into the outer `Result` Err slot. Multi-word error payloads — a `Result[T, E]` where E is or contains a `String` / `Vec` / multi-field struct — round-trip through the uniform i64-word Err representation: `rebuild_value_from_payload_words` / `coerce_to_payload_words` consume as many words as each field's LLVM width demands, so e.g. `struct AppError { msg: String }` + `impl From[String] for AppError` propagates correctly, including the `main() -> Result[(), E]` exit path (B-2026-07-09-20). Verified value-correct and valgrind-clean across same-error, cross-error String→struct{String}, i64→struct{i64}, Vec[i64], and 4-word struct{i64,String} cases.)*
- [x] Standard impls: numeric widening/narrowing, `String` from literals, `Option`/`Result` wrapping. *(Slice 1: numeric widening table shipped (19 impls: signed→signed, unsigned→unsigned, unsigned→wider-signed, f32→f64). Narrowing (needs TryFrom), `String` from literals, `Option`/`Result` wrapping pending.)*

### Associated Types
- [x] Associated type declarations in traits (`type Item`) and binding in impls (`type Item = i64`). Full across parse → resolve → typecheck → both backends: `trait Container { type Item; fn first(ref self) -> Self.Item; }` + `impl Container for B { type Item = i64; … }` works; the typechecker stores bindings in `TypeEnv.impl_assoc_types` keyed `(type, assoc)`. This is what backs `TryFrom`/`TryInto` (associated `Error`) and the Iterator protocol (associated `Item`).
- [x] Projection syntax (`I.Item`) in type position and `where` clauses. A projection is a 2-segment `TypeKind::Path` lowered to `Type::AssocProjection` (typechecker) when the head is a generic param in scope; `resolve_assoc_projections` substitutes it. Works in a fn signature (`fn get[C: Container](c: C) -> C.Item`), param/return/local positions, and where clauses — interpreter throughout, and codegen after the mono type-lowering resolved the projection to the concrete associated type (`assoc_type_bindings`; previously a projection return type collapsed to `segments.first()` and failed the LLVM verifier). Generic-impl (GAT) bindings whose RHS references the impl's own params are a follow-on.
- [x] Equality constraints in `where` clauses (`where I.Item = i64`) — accepted and enforced in both backends.

### Iterator Traits
- [ ] `trait Iterator { type Item; fn next(mut ref self) -> Option[Self.Item] }` — core iteration protocol using associated types
- [ ] `trait Iterable { type Item; fn iter(ref self) -> impl Iterator[Item = Self.Item] }` — collection protocol
- [ ] `filter`, `map`, `collect`, `fold`, `any`, `all` — standard iterator combinators
- [ ] Implementations for `Vec[T]`, `Map[K, V]`, `Set[T]`, ranges

### Auto-Concurrency Codegen
- [ ] **Auto-parallelization of non-`par` regions.** The concurrency analysis already identifies parallelizable statement groups outside explicit `par {}` blocks (`ConcurrencyAnalysis.function_decisions`), but codegen currently ignores them and emits those groups sequentially. Wire codegen to honor `parallel_groups` on non-`par` blocks: for each group of two or more statements the analysis marks parallel, emit the same `karac_par_run` call path as explicit `par {}`. Requires threading `ConcurrencyAnalysis` into `Codegen` (not currently passed to `compile_to_object`). Guard with the Phase 6.1 granularity heuristic — don't spawn threads for trivial pure statements. This is the feature that makes the "write sequential code, compiler parallelizes it" story true in compiled binaries.

- [x] **Debugger Contract — runtime metadata emission.** Co-developed with auto-concurrency codegen because this is the moment the runtime first emits `par`/`suspend` code; the contract has to be in place or it gets locked in by accident. Four runtime structures required, per design.md § AI-First Compiler Interface > Debugger Contract: (1) static `SpawnSiteId` (`u32`) per `par {}` block, embedded in the executable's metadata table; (2) parent-frame reference field on every worker frame produced by `par`/`spawn`/`TaskGroup`, with a `"root"` sentinel for the root task; (3) await-chain pointer on every suspended task pointing to its `WaitTarget` (peer task or typed I/O handle); (4) `std.runtime::list_tasks()` and `std.runtime::list_par_blocks()` enumeration functions plus `std.runtime::has_debug_metadata() -> bool` for runtime detection. Profile-gated: default-on for `[profile.dev]`, default-off for `[profile.release]`; controlled via `runtime_debug_metadata = true|false` in the active profile. Embedded/`isr` profiles default-off (incompatible with `panics_off` / `default_no_alloc`). The metadata is part of the language-level contract and stable within a major version.

### Performance Primitives
- [ ] `Arena[T]` — arena allocation for cache-friendly bulk allocation (stdlib, not language feature)
- [ ] `ArenaRef[T]` — non-owning index into an arena

### I/O (with effect annotations)

> **Interpreter MVP shipped (Phase 8 slice 1):** `IoError` enum registered in prelude + typechecker; `Stdin`, `Stdout`, `Stderr`, `FileSystem` added to `PRELUDE_EFFECT_RESOURCES`; interpreter builtins: `Stdin.read_line()` / `Stdin.read_to_string()` → `Result[String, IoError]`; `Stdout.flush()` / `Stderr.flush()` → `Unit`; `FileSystem.read_to_string(path)` / `FileSystem.write(path, contents)`. `env.args()` → `Vec[String]` and `env.var(name)` → `Result[String, VarError]` shipped; resolver registers lowercase `env` as a module alias. 5 typechecker tests + 4 interpreter tests added. Open: codegen path, `File` handle type, `BufReader`, `env.set`, `impl From[VarError] for IoError`.

- [ ] File I/O: `read_file`, `write_file` — `reads(FileSystem)` / `writes(FileSystem)` *(partial: `FileSystem.read_to_string` / `FileSystem.write` done in interpreter MVP above)*
- [ ] Console: `print`, `println` — `writes(Stdout)`; `eprintln` — `writes(Stderr)`; `io.read_line`, `io.read_to_string` — `reads(Stdin)` *(partial: `Stdin.read_line` / `Stdin.read_to_string` / `Stdout.flush` / `Stderr.flush` done in interpreter MVP)*
- [ ] Network: TCP/UDP primitives — `sends(Network)` / `receives(Network)`
- [x] Environment: `env.args`, `env.var(name)`, `env.set` — `reads(Env)` / `writes(Env)` *(partial: `env.args` + `env.var` done; `env.set` open)*
- [x] Clock: `now` — `reads(Clock)`
- [x] Random: `random` — `reads(RandomSource)`

### String Operations
- [ ] Concatenation, length, slicing, search, replace, split, join, formatting

### Math
- [ ] Integer and float math, constants, bitwise operations

### Provider Implementations
- [x] `with_provider[R]` — trait-based effect injection
- [x] In-memory test providers for standard resources

### Logging (`std.log`)
- [ ] `log.debug`, `log.info`, `log.warn`, `log.error` — structured logging with severity levels
- [ ] Uses `transparent effect verb traces;` and `traces(Logger)` — never propagates, never affects concurrency
- [ ] Configurable output destination (stderr, file, custom sink via provider trait)

### Diagnostics — `std.panic` and `std.runtime`

- [ ] **`std.panic` — crash report writer.** Implements the wire format specified in design.md § AI-First Compiler Interface > Crash Report Format. Eight required structured-JSON fields: panic site, panic kind discriminant, message, logical stack (per-block for `par`, per-task for `suspends`), provider stack, RC-fallback annotations, parallel context, build metadata. Output discipline: stderr 5–10 line summary + crash file path; default path `/tmp/kara-crash-{pid}-{timestamp}.json` (Unix) / `%TEMP%\...` (Windows); `KARA_CRASH_DIR` env var override; empty `KARA_CRASH_DIR` suppresses file output. Edge cases: panic-during-panic-report (fall back to abort + minimal stderr line, no loop), drop-time panic (capture as `panic_kind: "drop_during_unwind"` with `caused_by` preserving the original triggering panic), concurrent panics (each task writes its own file with cross-references in `concurrent_with`), embedded `panics_off` (panic-report path compiled out — zero overhead). Override hook follows Rust `set_hook` precedent.

- [x] **`std.runtime` — runtime introspection (Debugger Contract surface).** Companion to `std.panic`; co-developed because they share metadata sources. Exposes the four Debugger Contract elements as a Kāra-callable API: `list_tasks() -> Vec[TaskInfo]` (every suspended task with `WaitTarget`, source location, effect summary), `list_par_blocks() -> Vec[ParBlockInfo]` (every active `par {}` block with `SpawnSiteId`, worker count, per-worker source location), `has_debug_metadata() -> bool` (profile-gated). Both list functions return empty when the binary was built without `runtime_debug_metadata = true` — generic tooling can try-then-degrade. WASM target replaces filesystem-backed crash files with a JS-side handler hook (`window.karac_crash` default, configurable via `KARA_CRASH_HANDLER` import); GPU panics surface as host-side panics at the kernel-launch site with `panic_kind: "gpu_kernel_failed"` and a `gpu_marker` field. Full GPU stack reconstruction is post-v1; WASM/GPU adaptations land in Phase 10 alongside the respective backends.

- [ ] **`std.runtime.profiler` — sampling profiler core (P1, v1; graduated from brainstorm v69 § Gap 2, 2026-05-20).** Continuous CPU-time sampling via signal-based mechanism (`setitimer(ITIMER_PROF)` + `SIGPROF` on Linux and macOS, with macOS routing quirk handled by reading per-worker state from the cooperation hook rather than `ucontext_t`). 100Hz default rate, configurable via `KARA_PROFILER_HZ` env var. **Auto-concurrency requires a runtime cooperation hook**: per-worker atomic "current task" slot, updated by the scheduler on task entry/exit, readable from the signal handler in async-signal-safe context. Without it, samples can't be attributed to task / SpawnSiteId / parent-task chain. Output format: pprof-compatible protobuf (`profile.proto` v3, gzipped), symbolized-on-emit using LLVM debug-info; Kāra-specific data (`spawn_site_id`, repeated `effect` labels, `parent_task_chain`) layered via `Sample.label` — no custom protobuf fields. Value type at v1: `cpu` nanoseconds. **CLI-mode profiling first**: dumps profile to a file via `std.runtime.profiler.dump_to_file(path)`; no HTTP dependency. Lives under `std.runtime` to keep continuous profiling separate from static one-call introspection (`list_tasks` / `list_par_blocks`). Spec at [`design.md § std.runtime.profiler`](design.md#stdruntimeprofiler) (to be written during implementation); resolution archive at [`brainstorming/archive/v69_go_parity_gaps.md § Gap 2`](../brainstorming/archive/v69_go_parity_gaps.md). **v1.1 follow-ups**: wall-time profile (second value type, CLOCK_MONOTONIC timer source) and execution tracer (Go runtime/trace-equivalent, builds on the cooperation hook with per-worker ring buffers). Both additive, non-breaking, confined to runtime + stdlib — extending the v1 surface, not redesigning it.

### Script mode
- [ ] Files without `fn main` synthesize `fn main() -> Result[Unit, Error]` wrapping top-level statements. Aligns with v34 REPL cell-as-main-body model.

### `std.json`
- [x] `Json` enum + parse/stringify — universal config/API surface; every CLI / service / data-pipeline program needs it. Typed `(de)serialization` lands in v1.5.

### `std.time`
- [ ] `Duration` and `Instant` types; arithmetic (`Instant - Instant -> Duration`, `Instant + Duration -> Instant`); ISO 8601 parse/format. `Clock` resource provides the source via `reads(Clock)`.
- [ ] `std.time.sleep` — `blocks` execution verb.

### `std.path`
- [ ] `Path` type — separator-aware path manipulation (Windows `\` vs. Unix `/`); `join`, `parent`, `file_name`, `extension`, `components`; conversion to/from `String` with validation.

### `std.error`
- [ ] `Error` trait — `description() -> String`, `source() -> Option[ref dyn Error]`; structured chaining. `From` impls for cross-error `?` already in conversion-traits section.

### `std.mem`
- [x] `swap`, `replace`, `take` — ownership-driven idioms for value movement without consume. `swap` / `replace` are `#[compiler_builtin]` intrinsics in `runtime/stdlib/mem.kara`, intercepted at the call site in the interpreter (`eval_call`) and codegen (`compile_call`) — they move values through `mut ref` places via raw load/store (no destructor on the value that leaves the place). `take` (= `replace(dest, T.default())`) is a REAL generic Kāra body seeded from `std.mem` into codegen's `generic_fns`, so it monomorphizes per concrete `T` like any generic free fn — its `T.default()` dispatches to the derived/hand-written `Type.default` (structs/enums) or a built-in primitive-default (i64 / f64 / bool / char / String) in both backends. All three are correct and leak/double-free-free for i64 / String / struct values / `mut ref` param forwarding (incl. forwarding into `take` through a helper's `mut ref` param). Wiring: the `T: Default` bound now discharges against derive-only-builtin `Default` (`type_satisfies_bound`), generic `T.default()` resolves the concrete type in the monomorph (`compile_assoc_call` via `type_subst_names`), and baked-stdlib `mut ref` free fns propagate write-back through the interpreter's CICO path (`fn_param_mut_ref_flags` falls back to `STDLIB_PROGRAMS`).
- [x] `forget` (`unsafe`) — suppress destructor; reserved for FFI handoff. Shipped (`intrinsics.kara`; additive-interop Slice 4).

### `std.bytes`
- [ ] `Bytes` type — slice-into-shared-buffer with cheap clone; critical for parser internals, network-protocol code, request-handling perf without per-call allocation.

### `std.cmp`
- [x] `Ordering` enum (`Less`, `Equal`, `Greater`); `min`, `max`, `clamp` free functions. `Ordering` (plus `is_lt`/`is_le`/`is_gt`/`is_ge`/`is_eq`) lives in `runtime/stdlib/ordering.kara`; `min[T: Ord]` / `max[T: Ord]` / `clamp[T: Ord]` are ordinary generic stdlib free functions in the same file — the first plain (non-intercepted) generic stdlib free fns to reach codegen, monomorphized on demand via a `generic_fns` seed. Ties return the first argument for both `min` and `max`. Correct in the interpreter and compiled output for every `Ord` type, including heap-owning ones (String / heap structs) — the compiled heap-`Ord` leak these bodies surfaced (an owned param returned while its buffer went undropped) was fixed in both the non-generic and generic/monomorphized codegen paths, bringing the mono call/param path to ownership parity with the non-generic path (B-2026-07-08-6, fixed).

### `std.hash`
- [ ] `Hash` trait, `Hasher` interface, default hasher; `#[derive(Hash)]` codegen path (interpreter form already shipped).

### `std.cli` (v66 graduation, 2026-05-11 — P1 v1)
- [x] **Argument parser, builder-style API.** `Parser::new(name)`, `.arg(name, Arg)`, `.flag(name)`, `.subcommand(name, sub_parser)`, `.parse() -> Result[Args, CliError]`. Automatic `--help` / `--version`. Effect: `reads(Env)` on `.parse()`. API inspired by clap's builder pattern; the point at v1 is canonicality in stdlib so the ecosystem standardizes from day one. See `deferred.md § std.cli`.

### Compiler Queries Channel (P0 architectural commit)
- [x] **P0 architectural commit — stable item identity, per-phase queries field, `karac query queries` CLI surface, stability classification.** Ships the channel infrastructure even with zero query catalogue entries; subsequent P1 entries are non-breaking additions. Stable item identity (path-based DefId + structural-hash sub-item slots) is load-bearing — without it, every later query addition becomes a breaking change for tools storing resolved answers. Spec at [`design.md § Specification Layers > Compiler Queries`](design.md#compiler-queries) (graduated from brainstorm v63, 2026-05-08); tracker at [`phase-8-stdlib-floor.md`](implementation_checklist/phase-8-stdlib-floor.md).
- [x] **P1.1 RC fallback query** — first catalogue entry; reuses existing `RcFallbackNote` decision site. Resolution: existing `#[no_rc]` + new `#[prefer_rc]`.
- [x] **P1.2 Specialization query** — typechecker-driven; stress-tests fan-out queries (one decision, many monomorphizations). Resolution: `#[specialize(T = i64)]`.
- [ ] **P1.4 Effect-narrowing query** — function-exit hook. Resolution: existing effect declaration.
- [x] **P1.5 Layout query** — gated on layout-block stability (Phase 7.2 — shipped). Resolution: existing layout-block syntax.
- [x] **P1.3 Inlining + branch hints** — codegen-side hooks; tracked separately at [`phase-7-codegen.md`](implementation_checklist/phase-7-codegen.md).
- [x] **P1.6 Auto-concurrency fork threshold** — query surface shipped 2026-06-16 (`#[fork_at(N)]` resolution); numeric-threshold refinement follows the [Phase 11](#phase-11-standard-library--long-tail) cost-model.

### Backend Platform (v64-lifted)

> **Lifted from Phase 11 long-tail under the v64 backend-first decision (2026-05-09).** Full rationale at [`design.md § v1 Positioning — Backend-First`](design.md#v1-positioning--backend-first). This sub-section bundles the stdlib modules that the backend-first lead persona requires at v1 — co-located with the Phase 8 floor rather than split into a separate Phase 8.5 to keep the structure clean. P0 items load-bearing for the flagship 1M+ demo; P1 items ship at v1 launch sequenced after the P0 spine.

- [ ] **`std.http` — HTTP/1.1 server + client (P0).** Connection lifecycle (keep-alive, chunked transfer, Host routing), `Server::bind` / `Server::serve` / `Request` / `Response` / `Client::get` / `Client::post`, body streaming, header manipulation, basic routing. Stable v1 surface — minimal API exposing the 80% case; advanced extension points (connection-level customization, custom transport, low-level frame access) ship `#[unstable]`-gated. Pre-lock audit against Go `net/http`, Rust `hyper` + `axum`, Node `http` for known footguns (Go middleware composition, Rust body-ownership, Node error propagation).
- [x] **TLS — vendored rustls + aws-lc-rs default crypto provider (P0).** `std.tls` API exposes the cross-platform server + client surface; rustls-provider plug points private at v1 (no public crypto-provider extension API, revisited at v2 if FIPS / post-quantum forces it). Modern-TLS-only stance (no SSLv3, no insecure ciphers); legacy-interop callers use community wrappers. Audit posture: rustls + aws-lc-rs already audited upstream, but the FFI binding layer + `std.tls` API + verification callbacks + certificate-chain handling + error-mode coverage are *new* code and need their own audit pass before v1 ship.
- [x] **WebSocket — RFC 6455 (P0).** Server-side framing, handshake, ping/pong, close. Built on `std.http`. The canonical idle-keep-alive workload that grounds the 1M+ flagship benchmark — Demo 1 (minimal HTTP+WebSocket server) is shaped around this surface.
- [ ] **HTTP/2 — multiplexed streams + flow control (P1).** Required for gRPC. Ships at v1 launch sequenced after HTTP/1.1; not a P0 architectural commit because the 1M+ verification gate runs over HTTP/1.1 + WebSocket. HPACK header compression, server push (default-off), `Server-Sent Events` interop.
- [ ] **`std.tracing` — structured logging + span/trace context propagation, OTel-export-ready (P1).** Operational story is a v1 launch criterion (per the "ship reality" decision in v64). Span context + trace propagation primitives that *can* export to OTel collector at v1.x without API change. Comptime-generated trace-context plumbing is a Kāra-native opportunity (no proc-macro indirection like in Rust's `tracing`); land the comptime path alongside the surface. **Cross-link to `std.panic` (graduated from brainstorm v69 § Gap 2, 2026-05-20)**: when a panic occurs inside an active span, `std.panic` reads `trace_id` + `span_id` from the active span and includes them in the crash report's optional `tracing` block. Field names match OTel convention exactly so consuming tools (Jaeger, Tempo, Datadog) map directly. Graceful absence when no active span / std.tracing not compiled in.
- [ ] **`std.http.profiler` — HTTP endpoint for `std.runtime.profiler` (P1, v1; graduated from brainstorm v69 § Gap 2, 2026-05-20).** Pprof-style live-process profiling endpoint. **Env-var opt-in, not stdlib-import-driven**: `KARA_PROFILER_PORT=6060` enables; default is off. **Separate listener**, not piggy-backed on the app's HTTP server (mixing app and debug traffic creates security + ops issues). **Localhost-only by default**: binds to `127.0.0.1:KARA_PROFILER_PORT`; explicit `KARA_PROFILER_BIND=0.0.0.0:6060` to expose externally (env-var name makes security implications visible). Endpoints follow Go's `net/http/pprof` shape: `/debug/pprof/profile` returns CPU profile in pprof-compatible protobuf. Depends on `std.runtime.profiler` (data source) and `std.http` (server). README line: *"`go tool pprof http://localhost:6060/debug/pprof/profile` works against a Kāra HTTP server."* **Not at v1**: effect-typed exposure (e.g., `exposes(Profiler)` effect on the registering fn); env-var mechanism covers v1 needs without adding a new effect verb.
- [x] **`std.regex` — compile patterns, match / find / replace (P1, lifted from Phase 11).** Common backend need; lifted into v1 floor under v64.
- [ ] **`std.process` — `Command` / `Child`; new `ProcessTable` effect resource (P1, lifted from Phase 11).** Subprocess spawning + wait + I/O. Lifted into v1 floor under v64.
- [ ] **protobuf — wire format + codegen (P1).** gRPC-adjacent. Comptime-driven codegen from `.proto` files (no separate codegen tool — comptime parses the schema and emits the message types directly). gRPC itself is post-v1 (see [`deferred.md § gRPC`](deferred.md#grpc-streaming-reflection-server--client)).
- [ ] **File-system event loop — io_uring on Linux, sticky kqueue on BSD/macOS (P1).** Lifts disk-I/O ceiling on Linux beyond what epoll covers. Not load-bearing for the 1M+ socket benchmark (epoll/kqueue/IOCP are sufficient there) but matters for mixed-workload demos and disk-bound backends. **Single tracker: [phase-8-stdlib-floor.md](implementation_checklist/phase-8-stdlib-floor.md)** — not a phase-6 M3 parity item (de-scoped 2026-06-07).
- [ ] **`Pool[T]` — connection-pool primitive (P1).** `acquire / release`, bounded waiters, health checks. Library-shape; community database drivers build on this. Same `Pool[T]` primitive serves HTTP client connection reuse, Redis client pooling, custom resource pooling.
- [ ] **Application-layer backpressure primitives (P1).** `Semaphore` (with `acquire(timeout)` / `release`), `BoundedChannel[T]` (size limit + send-blocks-when-full / send-fails-fast configurable), `RateLimiter` (token bucket — "max N requests/sec per key"). Complementary to the deployment-layer providers story (which handles per-provider concurrency caps); user code routinely needs application-layer backpressure too.
- [ ] **`karac new <name>` default project template (P0).** Defaults to a backend HTTP server skeleton: `std.http` + `std.tracing` + a `/health` endpoint + a `/ws` WebSocket route. `--lib` for libraries, `--cli` for CLI tools, `--data` for data-pipeline scaffolding (Kafka consumer + processor + sink shape). Default-being-backend reinforces positioning at the friction-zero entry point.
- [ ] **Demo 3: data-engineering pipeline (Kafka → S3 → DuckDB-shape) (P1).** Verification artifact for the v64 second-order-positive claim that backend-first investment compounds into the data-engineering persona. Same v1 runtime, same `Pool[T]`, same TLS, same `std.tracing` — proves the personas share the floor rather than competing for it. Cheap incremental engineering, multiplies the v1 launch story.
- [ ] **`kara-postgres` — canonical Postgres driver, project-owned package (P1, v66 graduation 2026-05-11).** Lives at `karalang/kara-postgres`; published to the package registry; installed via `karac add kara-postgres`. **Firm P1, not soft P1** — doubles as internal infrastructure for the user to stress-test Kāra against real backend workloads during v1 development (effect system, `Pool[T]`, auto-concurrency, `with_provider`, structured errors). Dogfooding-grade, not minimum-viable: written to exercise the language's distinctive capabilities. Scope: TCP connection, prepared statements, simple-query protocol, basic type mapping (i64/String/f64/bool/bytes/NULL/timestamp/uuid), transactions, prepared-statement param binding, `Pool[T]` integration. No LISTEN/NOTIFY, COPY, async streaming at v1. Stdlib position (no `std.sql`) is unchanged — see `deferred.md § Stdlib Scope for Non-Primitive Resources`. Handover-to-community policy **explicitly deferred** to engineering-start. See `deferred.md § Canonical Postgres Driver (kara-postgres)` and `brainstorming/archive/v66_general_purpose_with_data_bonus.md § 2.3 and Q3`.

### Standard Library Layers (`core` / `alloc` / `std`)
- [ ] `core` layer: primitives, `Option`, `Result`, `Array[T, N]`, traits, effect system, math — no OS or allocator dependency
- [ ] `alloc` layer: `Vec[T]`, `Map`, `String`, `f"..."` interpolation, `shared struct`/`shared enum` (RC), `Pool[T]` — requires heap allocator
- [ ] `std` layer: file I/O, networking, threads, environment, channels — requires OS
- [ ] Profile mapping: `kernel` → `core` only; `embedded` → `core` + optional `alloc`; default → all three

### Parallax-lite — first ground-truth measurement workload

Parallax-lite is a stripped-down precursor to Parallax (the Auto-Concurrency API Gateway, see `docs/dogfooding.md`) — same shape (HTTP server, providers for upstream services, fan-out + join), narrower surface (one upstream instead of four, single resource per endpoint). It is the first program in the codebase with non-trivial Provider-Rooted Resources + auto-concurrency + (likely) RC fallback in one place — the right shape to ground-truth the spec's quantitative claims. Two measurements feed off the same workload:

- [x] **Cumulative Cost Surface validation.** Run `karac query cost-summary` against Parallax-lite to validate the static-count surface specified in `design.md § Performance Diagnostics > Cumulative Cost Surface`. Discrepancies between the table's order-of-magnitude estimates and observed counts feed back as edits to the table. Runtime attribution (sampling-profiler-driven %wall-clock against the same workload) lands as a separate post-v1 step; the static-count form ships in Phase 5.3.

- [ ] **Cost-model tuning (v1.x).** Use Parallax-lite to drive the empirical tuning that lets the v1.x auto-concurrency cost-model spec land. Today's interim cost model is degenerate (parallelize whenever distinctness allows and `ParallelGroup.is_trivial` is false). The v1.x specification work — per-call cost heuristic, fork threshold, loop-body parallelization rule, distinctness policy under dynamic keys — is tracked under `implementation_checklist/` Phase 6 ("Cost-model specification (v1.x)"); this entry is the workload that gives that work its measurement target. Same binary as the Cumulative Cost Surface item above; two analyses against one program.

- [ ] **`par struct` single-task overhead measurement.** Build a `par struct` variant of the Parallax-lite types that would otherwise be `shared struct`, run the same workload, and measure the overhead in single-task mode: per-field uncontended atomic load/store, `Mutex[T]` lock/unlock on the no-contention fast path, `Arc` refcount cost vs. `Rc`. The threshold question this answers: **if single-task overhead is below ~5 ns per field access, inverting the default (`par struct` becomes the default; `shared struct` becomes the narrow opt-in) is a credible v2 RFC.** Above that threshold, the v1 polarity stands and the migration tooling (`karac migrate shared-to-par`) is the right answer. Same binary as the Cumulative Cost Surface and cost-model-tuning items above; three analyses against one program.

- [ ] **`shared` → `par` transition frequency observation.** Track how often `shared struct` definitions in `examples/` and the demo programs are migrated to `par struct` over the v1 development window. Empirical signal: high frequency (>~1 per 500 LOC of new examples) raises the inverted-default proposal from "v2 RFC" to "should-have for v1.x"; low frequency confirms the v1 default polarity was correct. Tracked manually via grep over `git log` once the workload exists.

**Done when:** The floor stdlib is sufficient to write any non-domain-specific program (CLIs, services, libraries, data-processing pipelines that don't need numerical primitives) entirely in Kāra with no FFI escape hatches beyond what the stdlib wraps. Auto-concurrency works in compiled output. Specialty stacks (numerical, security, embedded primitives, regex/http/process) ship in [Phase 11](#phase-11-standard-library--long-tail).

---

## Phase 8.5: V1 Ship Readiness

**Goal.** Everything v1 needs to ship credibly beyond the demo-feature surface in Phase 8 — packaging / build tooling, the interactive surfaces that drive adoption, and a discovery bucket for items that emerge during demo build. **P1 priority within v1**: comes back to once Phase 8 demo features are built; lands before v1 ships at the end of Phase 11.

**Why a separate phase, not a Phase 8 parallel track.** Phase 8 is "build the language surface that makes the demos work." Phase 8.5 is "everything else v1 needs to ship credibly." The split keeps Phase 8 demo-driven and gives v1-but-not-demo-blocking work a coherent filing cabinet — particularly important for items that surface during demo build and need somewhere clean to land. Half-numbered phase signals the bucket isn't yet committed to a permanent shape; will graduate to a numbered Phase 9 (renumbering downstream) if/when the contents stabilize.

**Why v1, not v1.1.** Profile knobs (kernel / embedded / deterministic) are a differentiator pillar (`dogfooding.md` § Demo planning, pillar 4) — they need a real config surface. `kara-version` MSRV is a credibility table-stake. Reproducible-builds CI backs the `design.md` reproducibility pitch. Path / git deps are needed for self-hosting (Phase 12). None of this can ship as "parsed-but-ignored" without the half-ship being the broken signal. Resolver / registry / cache *implementation infrastructure* could have deferred to v1.1; pulled into v1-P1 on 2026-05-08 to avoid the cliff between "model exists" and "model works at adoption scale."

**Sequencing.** Runs as a parallel track that does not block Phase 9 / 10 / 11 progression. Items here are addressed once their gating demo work clears or in dedicated pre-ship windows. Discovery items (Track 4) are added as they surface during Phase 8–11 work — see Track 4 prose for filing protocol.

### Track 1: Interactive Development — REPL + Browser Playground (P0) + Jupyter Kernel (P1 delivery)

**Goal:** First-class interactive surface that positions Kāra as a notebook-friendly systems language. Delivery is split into two tiers — the `karac repl` binary and a browser playground ship in v1 (the frictionless first-try path that drives adoption); the Jupyter kernel ships in v1.1 alongside a stable stdlib (so first-run notebook users don't hit "function not found" on common types). Semantics are specified in [`docs/design.md § Interactive Evaluation Model`](design.md#interactive-evaluation-model). Execution backend: always-JIT via LLJIT (per Core Strategy #2) — the same code path users get from `karac build`, just compiled lazily per function on first call.

**Why P0.** The execution-model-parity story (REPL cells run on the same LLJIT path as `karac build` produces, not on a parallel interpreter) gives Kāra a differentiator that neither Rust nor Java can match cheaply:

- Rust's `evcxr` recompiles a dylib per cell — slow, fragile, not officially supported. Per-cell cold-start is comparable to Kāra's, but `evcxr` lacks `cargo`-managed dependencies in cell scope and can't surface effect/ownership analysis interactively.
- Java's JShell pays JVM startup + speaks Java verbosity — works but not notebook-native feeling. Subsequent calls are JIT'd, but the language doesn't have effect / ownership analysis to surface.
- Kāra: lazy LLJIT amortizes ~100 ms cold-compile across cell lifetime, syntax readable to Python-origin users, *and* surfaces the language's differentiators (effects, ownership) in cells where other languages have nothing to show. Trivial cells (`let x = 1+1`) pay the ~100 ms compile cost — uniform and expected, not a mystery slowdown. For data-science / engineering workloads where REPL serves as production tooling, that cold-start is amortized by the data-processing time itself; it hurts most for trivial exploratory cells where serious users don't dwell.

**Why the split.** Adoption is the dominant concern for a new language, not dev effort. The mental barrier to trying a systems language ("cargo new, edit TOML, fight IDE, *then* learn ownership") is what sends Python-origin users away. The REPL binary and browser playground remove that barrier at v1. The Jupyter kernel — while strategically important for data-science audiences and shareable notebook content — depends on a stable stdlib for a good first impression, and ships with v1.1 when that's in place.

#### Tier 1: REPL binary + browser playground (P0, ships in v1)
- [x] `karac repl` subcommand — line-based REPL over the LLJIT execution backend; multi-line continuation; persistent session bindings; `:help`, `:quit`, `:type`, `:effects`, `:save <file.kara>`, `:provide R = expr` / `:end-provide R` meta-commands. Cell semantics per `design.md § Interactive Evaluation Model > Cell Scope`; cross-cell provider scoping per `design.md § Cross-Cell Providers`. Lazy compilation: each defined function is compiled on first call, cached for subsequent calls — published cold-start latency is the v1 perf headline alongside binary size and steady-state perf.
- [x] Notebook-aware rendering of use-after-move diagnostics when consume and use straddle cells — strict semantics, softened presentation (names the consuming cell, suggests `.clone()` at call site).
- [x] `--auto-clone` opt-in flag for users who prefer Python-like ergonomics — inserts `.clone()` at consume sites, emits `perf[auto-clone-in-repl]` note. Never silent.
- [x] Session export (`:save session.kara`) that produces a `.kara` file compiling identically to the session. `:provide`/`:end-provide` pairs compile to `with_provider[R](expr, || { /* cells */ })` blocks in the saved file.
- [x] Browser playground (`play.kara-lang.org` or equivalent) — zero-install entry point. Server-side `karac repl` behind a WebSocket shim, or WASM-compiled interpreter in the browser (decide during implementation). Minimum UX: editor, run button, output pane, share-by-URL.

#### Tier 2: Jupyter kernel MVP (P0 priority, P1 delivery — ships in v1.1)
- [x] `jupyter_client` protocol compliance — ZMQ shell/iopub/stdin/control channels, cell execution, stderr diagnostics with **clickable source spans in JupyterLab**, Ctrl+C cooperative interrupt, tab completion over session + prelude.
- [x] `pip install karac-kernel` packaging — Python launcher + kernelspec registration; precompiled `karac` binaries per platform.
- [x] `%magic` surface (MVP): `%effects`, `%ownership`, `%explain <name>`, `%set auto-clone on|off`, `%provide R = expr` / `%end-provide R` (parity with REPL meta-commands — same compilation path). Per-cell effect footer rendered automatically on every execution. **This is where the language differentiators become visible in the notebook.** `%rc` is deferred to post-MVP — RC-fallback analysis is still settling and its introspection surface is not yet stable.

#### Tier 3: Rich interactive (stretch, post-MVP)
- [ ] Rich `text/html` display for structs and collections; `image/png` for any plotting primitive.
- [ ] Effect-conflict timeline — sidebar showing per-cell effect sets and cross-cell dependency arrows.
- [x] `%rc` magic — RC-fallback decision list with trigger reasoning, once the underlying analysis surface stabilizes.
- [ ] Widget protocol (IPython-widgets equivalent) — probably v2+.

#### Tier 4: Book coverage (P0 prose, v1)
- [x] **"Getting Started, Part 2: Two Surfaces"** — dedicated chapter positioned right after "Getting Started / Installation." `.kara` file and `karac repl` shown side by side on the same binary-search example; teaches session model, cell scope, ownership across cells, `:effects` / `:save` meta-commands. Browser playground gets a sidebar callout. Ownership is taught honestly from day one — Q2's softened diagnostic means no retraction later. v1 surfaces only (no notebook content yet). When the Jupyter kernel ships in v1.1, either extend this chapter to a third surface or add a standalone "Notebooks" chapter.

**Done when (v1):** `karac repl` gives a first-run Python user a productive session in under 5 minutes. Browser playground loads in under 2 seconds with no install. A user can save a REPL session to a `.kara` file that compiles and runs identically.

**Done when (v1.1):** `pip install karac-kernel && jupyter lab` gives a notebook environment where Kāra feels first-class, effect / ownership information shows up alongside normal cell output, and a user can save the session to a `.kara` file that compiles and runs identically.

---

### Track 2: Build & Dependency Tooling

**Goal:** Flip `[dependencies]` from parsed-but-ignored (v1 posture) to load-bearing. Formerly tracked as v1.1; pulled into v1-P1 on 2026-05-08 — see Phase 8.5 framing above for the rationale. Detailed implementation entries under [`implementation_checklist/phase-5-diagnostics.md § 5.5`](implementation_checklist/phase-5-diagnostics.md#55-package-manager-v11).

#### Tier 1: Resolver + lockfile (lands first; everything else builds on it)

- [x] **PubGrub-style resolver** — conservative semver, latest compatible by default, lockfile pins, full constraint-chain conflict diagnostics.
- [x] **`kara.lock`** — package name, exact version, source URL (proxy mirror or git URL), BLAKE3 content hash, dependency tree. Single lockfile across targets. Bin-yes / lib-no commitment.
- [x] **Registry proxy client** — Go-style decentralized identity (git URL) + immutable proxy mirror. Records both URLs in lockfile. `--no-proxy` for development.
- [x] **Build artifact cache** — global `~/.kara/cache/` keyed on `(compiler-version, package-version, edition, profile, target-triple)`. Per-project `dist/` already exists.
- [x] **`[package].kara-version` MSRV constraint** — enforced by resolver; mismatch is a structured diagnostic with the constraint chain.

#### Tier 2: CLI surface + cross-cutting

- [x] **`karac update` / `karac update <pkg>`** — bare form bumps everything within semver-compatible range; surgical form bumps one package.
- [x] **`karac install <bin-spec>`** — install a binary from path / git / proxy reference into `~/.kara/bin/`.
- [x] **`karac clean` / `karac clean --global`** — project `dist/` and global cache eviction.
- [x] **`karac vendor` + `karac build --offline`** — air-gap workflow; copies resolved deps into `vendor/`, refuses network on subsequent build.
- [x] **`[target.X.dependencies]` / `[target.X.profile]`** — per-target dependency and profile blocks for cross-compilation.
- [x] **`[dev-dependencies]` excluded from non-test builds** — wiring in the existing test/non-test split.
- [x] **`karac-toolchain.toml` reader** — `version` (required), `targets` (optional). Channels / components / install profiles deferred. Read by `karac` and by the eventual `karaup` toolchain manager.

#### Tier 3: Interactive parity

- [x] **`:dep` REPL meta-command** — adds a package to the session's in-memory manifest. State in-memory only; symmetric with `:provide`. Jupyter parity via the existing kernel meta-command channel.
- [x] **`karac run <script>` script-dir manifest discovery** — walk upward from the script's own directory (not cwd). `--manifest` / `--no-manifest` overrides.

#### Tier 4: Cross-compile UX (graduated from brainstorm v69 § Gap 3, 2026-05-20)

- [ ] **Five first-class targets at v1**: Linux x86_64, Linux arm64, macOS x86_64, macOS arm64, Windows x86_64. "First-class" means three things: (1) bundled sysroot + linker installable via `karaup target add <triple>`; (2) PR-time CI smoke build for each target (not the full test suite); (3) listed in `karac targets` output with installed/not-installed status. Windows arm64 deferred post-v1 (small market, not justifying CI matrix expansion). WASM is owned by Phase 10 track but integrates via the same `--target wasm32-...` flag.
- [ ] **`karaup target add <triple>`** — rustup-style on-demand toolchain install (NOT statically bundled in `karac`). Toolchain contents: sysroot + LLD + precompiled stdlib per target. **Hosting**: project-hosted CDN (primary) + GitHub Releases (free fallback) + enterprise mirror config (`karaup config mirror <url>`) for air-gapped environments. **Install trigger**: explicit `karaup target add` only — not auto-install on first `karac build --target X` (auto-install breaks CI / non-TTY workflows). **Version pinning**: each `karac` release ships with a known-good toolchain version; `karaup target add` defaults to that; mismatch is a warning, not error; override via `--toolchain-version`.
- [ ] **`karac build --target <triple>`** — canonical cross-compile UX. **`KARA_TARGET` env var as fallback** (single namespace consistent with `KARA_PROFILER_*` env vars; flag wins on precedence; host is the default). **Triple format**: short forms for the five first-class targets (`linux-arm64`, etc.) resolve to full LLVM triples (`aarch64-unknown-linux-gnu`); full LLVM triples accepted as aliases and required for community / advanced targets. **One target per invocation at v1** (no `--target a --target b` multi-target sweep). **No GOOS/GOARCH split** — Kāra uses LLVM, single-triple is natural; the Go-shaped two-var form would be a wart.
- [ ] **`karac targets`** subcommand — lists supported triples with installed/not-installed status per the toolchain manager. Discoverability surface; matches `rustup target list`.
- [ ] **Missing-toolchain diagnostic (structured, action-pointing)**: detected on `--target` parse, not on LLVM invocation — forbid cryptic LLVM linker errors by construction. Three E-codes: `E0789` (target not installed; points at `karaup target add <triple>`), `E0790` (target not installed AND `karaup` not found in PATH; two-step instructions covering karaup install URL + target add), `E0791` (toolchain install appears corrupted, checksum mismatch; points at `karaup target reinstall <triple>`). All diagnostics JSON-parseable via `--output=json` per the structured-diagnostic philosophy.
- [ ] **`karac test --target X --no-run`** — builds the test binary for the target without executing. Validates compile + link surface (catches API mismatches, missing symbols, target-ABI issues) without QEMU. **`karac test --target X` without `--no-run` errors with E0792** pointing at the workarounds: use `--no-run` for compile-validation or `--target host` for execution. **No QEMU / remote-runner integration at v1** (multi-month work, narrow audience, QEMU doesn't match real hardware anyway). **v1.x consideration**: `karac test --target X --runner <cmd>` matching cargo's pattern — `<cmd>` wraps QEMU, SSH-to-device, or custom launcher; lets cross-test land as composable extension.
- [ ] **GPU + WASM share `--target` surface**: `--target cuda` (NVPTX), `--target wasm32-wasip1` (WASM) flow through the same flag mechanism as CPU targets. **GPU-specific options layered as sub-flags**: `--target cuda --gpu-arch sm_80` (architecture generation), `--target cuda --ptx-version 8.0`, multiple archs via comma-separated list for fat binaries (`--gpu-arch sm_70,sm_80,sm_86`). **Toolchain install reused with vendor-toolkit caveat**: `karaup target add cuda` installs Kāra's NVPTX backend (parts we own); vendor SDKs (CUDA toolkit, ROCm) are user-installed separately — `E0793: CUDA toolkit not found at $CUDA_HOME; install from https://developer.nvidia.com/cuda-downloads`. **Targets and profiles stay orthogonal**: `--target linux-arm64 --profile embedded` is a valid combination, not a contradiction. **Phase ownership**: Phase 8.5 Track 2 (this section) ships the `--target` mechanism + first-class CPU targets + toolchain manager UX; Phase 10 ships GPU/WASM backends that plug into this surface — designed once, reused.

Resolution archive: [`brainstorming/archive/v69_go_parity_gaps.md § Gap 3`](../brainstorming/archive/v69_go_parity_gaps.md). **Done when (cross-compile UX)**: `karac build --target linux-arm64 my-server.kara` on a clean checkout either produces a working ARM binary (if the toolchain is installed) or emits `E0789` with the exact `karaup target add` command. Same for all five first-class targets. `karac targets` lists them all with status. `karac test --target X --no-run` validates test compile for non-host targets.

#### Tier 5: Library-artifact producer mode (additive interop)

**Graduated from [`spikes/additive-interop-adoption.md`](spikes/additive-interop-adoption.md) (Slices 2–5), 2026-07-08.** The *consume* side — Kāra calling C / C-ABI-wrapped Rust — **already ships and is `[x]`**: `extern "C" fn` + `unsafe extern { }` blocks + opaque types + FFI unions + calling conventions + effect integration + `[link]` manifest linking (roadmap L149 `FFI: extern "C" fn` / L379 `FFI codegen` `[x]`; `src/effectchecker/extern_ffi.rs`). **Do NOT rescope any entry below as "build C/Rust interop" greenfield** — the gap is the *producer* direction only: handing a C/Rust team a `.a` + `.h` and letting them link a Kāra kernel while keeping everything else. The export-ABI design fork is settled in [`design.md § Exported C ABI`](design.md#exported-c-abi); these entries are the mechanics. (Two clarifying corrections the spike settled: "call Rust crates cleanly" is un-cashable — Rust has no stable ABI, so the deliverable is the C-shim pattern; and the adoption thesis points at the producer direction, which is the actual hole.)

- [x] **Native library-artifact build mode (Slice 2).** `karac build --crate-type staticlib` (→ `.a`) and `--cdylib` (→ `.so`/`.dylib`), routing the [`design.md § Exported C ABI`](design.md#exported-c-abi) surface (`pub extern "C" fn`; `#[unsafe(no_mangle)]` is the forward-compat idiom — Kāra doesn't yet mangle) through the native link path with external linkage. Landed in `src/codegen/driver.rs::link_native_library` (thick archive via `ar -M`/`libtool`; shared lib via `cc -shared`/`-dynamiclib`, macOS `-install_name @rpath/lib<name>.dylib`, runtime lifecycle forced in via `-u`). Default output `lib<stem>.<ext>` so a lib build never clobbers a stray executable; `-o`/`--out` overrides. Wasm × crate-type rejected. **The `.a` is thick** — bundles the runtime; verified a C consumer links with **no karac toolchain present** (`tests/cli.rs`). **Project-mode too:** a `[lib]` manifest table (`name` + `crate-type = staticlib/cdylib`) drives a multi-module library build (`karac build` → `dist/lib<name>.a`/`.so` + `.h` from the merged super-program); CLI `--crate-type`/`-o` override the manifest. Verified with a 2-module project linked from a C host (ASAN-clean).
- [x] **C-header emitter (Slice 3).** Emits `lib<name>.h` (the cbindgen analogue) for the exported surface, scoped to the Slice-1 type mapping — primitives + `#[repr(C)]` structs transparent, everything else an opaque `KaraHandle` (`void*`). Header carries include guard, `<stdint.h>`/`<stddef.h>`, `extern "C"` C++ guard, the two `karac_runtime_init`/`_shutdown` lifecycle prototypes, dependency-ordered `#[repr(C)]` struct defs, and one `@effects`-annotated prototype per exported fn. `src/cheader.rs` (plain data, non-llvm; 6 unit tests). `--header`/`--no-header` override is a follow-up (header always rides along today).
- **Ownership handoff across the boundary (Slice 4) — COMPLETE: `forget` + Path-A round-trip + Path-B auto-boxing all shipped.**
  - [x] **`forget[T](value)` — the move-out primitive** (roadmap `std.mem` § `forget`). Consumes its arg + suppresses the destructor; the FFI handoff move. Sound by construction: the owned param makes the ownership checker AND the drop oracle consume the call (no scheduled drop), matched by codegen suppression — verified at 0 `drop_differential` divergences, plus observable drop-count tests (interpreter + codegen) and a use-after-forget move-error test. `runtime/stdlib/intrinsics.kara` + `prelude.rs` + `codegen/call_dispatch.rs` + interpreter. Co-designed with [`spikes/ownership-model-mechanization.md`](spikes/ownership-model-mechanization.md) (its slice-2 drop model, drafted).
  - [x] **Full allocate→use→free round-trip — Path A (v1).** Raw-pointer instance methods `.offset`/`.add`/`.read`/`.write` (+ `_unaligned`/`_volatile`) implemented in codegen (`B-2026-07-08-4`, closed). With these + `forget` + `malloc`/`free` FFI, the sound manual round-trip is now expressible — no per-type drop synthesis. **Verified: a Kāra kernel allocates a buffer, fills it via pointer writes, hands the pointer to C, and C frees it via a Kāra export — runs clean under ASAN/LeakSanitizer (no leak, no use-after-free).** This is the v1 completion of Slice 4's handoff; the spike's Slice-4 acceptance (`forget` + a stated handoff rule + a working round-trip) is met. `p[i]=x` pointer index-store stays deferred (`.offset(i).write(v)` covers it).
  - [x] **Auto-boxing / auto-destructor sugar — Path B (+ nesting follow-on).** The compiler auto-boxes a non-transparent aggregate return from a `pub extern "C" fn` and auto-emits its destructor — zero boilerplate. Kāra returns a `{data,len,cap}` value in registers (mismatches the SysV struct-return ABI), so the export heap-boxes it and returns an opaque **pointer** (a scalar return, C-compatible); the C side reads the fields transparently through the emitted struct and frees via `karac_free_<name>`. Covers `Vec[scalar]`, `String`, **and one level of aggregate nesting — `Vec[String]`, `Vec[Vec[scalar]]`** (nested transparent `{KaraString* data;…}` / `{KaraVec_int64_t* data;…}`; the destructor recursively frees each element's buffer before the outer). `codegen` (`current_fn_boxes_return` / `box_return_value` / `emit_export_destructors` / `emit_boxed_elems_drop_loop`) + `cheader` (`boxed_return_of` → `Flat`/`Nested` typedefs + `Struct*` return + destructor proto). An internal Kāra call to a boxed export is rejected (its LLVM return is `ptr`). **The `validate_exports` ABI-honesty gate** rejects any export whose return/param is non-transparent AND non-boxable (enum, `Option`, `Vec` by value, deeper nesting) with `E_EXPORT_ABI`, so the produced `.a`/`.so`/`.h` never ships a dishonest `KaraHandle` miscompile. **Verified: Vec[i64] / String / Vec[String] / Vec[Vec[i64]] round-trip from a C host clean under ASAN/LeakSanitizer; drop_differential 0 divergences; memory_sanitizer 558 pass; codegen + cli green.** Deeper nesting / `enum` / user-struct returns cross via a raw pointer to a Kāra-owned box (the manual Path-A pattern) — a further follow-on.
- [x] **Producer-side effect contract at the boundary (Slice 3½).** `suspends` exports rejected (`E0414` / `ExternExportSuspendsUnsupported`, `verify_extern_export_no_suspends` — sibling of the existing C-unwind rule, fatal for library builds since the export surface is the deliverable); v1 export boundary is synchronous-only. `panics` auto-abort at plain `extern "C"` + `extern "C-unwind"` rejection were already enforced in codegen. Producer-side effects are **KNOWN, not trust-not-verify** — the header states them precisely; the extern-*import* `{blocks}` default is not copied onto exports.
- [x] **Proof-point demo/kata (Slice 5).** A hot kernel written in Kāra, built as `.a` + `.h`, linked into an existing C **and** Rust host that keeps everything else — [`examples/interop/`](../examples/interop/) + E2E test `test_build_crate_type_staticlib_links_from_c_e2e` (`tests/cli.rs`, C links the `.a` with no karac toolchain present). **Finding:** a Rust host must link the *cdylib*, not the staticlib — the runtime bundles `std`, so a `.a` collides on `rust_eh_personality`; the `.so` encapsulates it. Book-snippet A/B verification is a follow-up.

- [x] **Windows library artifacts (v1 hardening).** The `--crate-type staticlib`/`cdylib` producer path now emits Windows-native artifacts: `.lib` (static, `llvm-ar` MRI-script archive merge — the thick-archive analog of the unix `ar -M`/`libtool` path) and `.dll` + companion `.lib` import library (shared, `clang -shared` over the object + runtime archive with the `WINDOWS_SYSTEM_LIBS` set + `/OPT:REF` dead-strip). The DLL-specific wrinkle: unlike a unix `.so`, a DLL exports **nothing** implicitly, so every symbol is named `-Wl,/EXPORT:<sym>` — the `pub extern "C" fn` names, their `karac_free_<name>` destructors, and the two runtime lifecycle entry points, collected AST-side by `cheader::export_symbols` (kept in lockstep with the header). `artifact_extension()` returns `.lib`/`.dll` on Windows. `src/codegen/driver.rs` (`link_static_library`/`link_shared_library` Windows arms mirror the proven `link_executable_windows` toolchain conventions; cfg(windows)-gated, so **CI-verified on the Windows runner only** — the Linux dev host compiles the unix arms + the `export_symbols` threading). `export_symbols` has a unit test.
- [x] **Rust-host `std`-collision smoothing (v1 hardening).** A Rust host that static-links the thick `.a` hits a cryptic consumer-side `duplicate symbol: rust_eh_personality` (+ other std symbols): the runtime is a Rust crate bundling `std`, and a `.a` carries those symbols to collide with the Rust host's own `std`, while a `.so`/`.dylib`/`.dll` encapsulates them. The gap was that the linker error pointed nowhere. Two smoothings, both verified end-to-end (staticlib note prints on stderr with clean `Built:` stdout; cdylib links clean into a Rust host running `add=42 fib=6765 mean=7.50`): (1) a `staticlib` build prints a stderr note steering Rust hosts to the cdylib (`print_staticlib_rust_host_note`, both single-file and project paths); (2) the generated header carries the same caveat so it travels with the artifact for a dev who only reads the `.h`. Asserted in `test_build_crate_type_staticlib_links_from_c_e2e`.
- [x] **Book chapter + A/B-verified snippet (v1.x).** [`docs/book/src/ch18-interop.md`](book/src/ch18-interop.md) ("Kāra as a Library: Additive Interop") walks the `examples/interop/` kernel from source through a staticlib/cdylib build, the emitted header, and into both a C and a Rust host — the A/B: both hosts print an identical `add=42 fib=6765 mean=7.50` (captured live, not asserted). Covers the type mapping (transparent / auto-boxed / rejected), the Rust-host cdylib caveat, the `malloc` + raw-pointer round-trip and `forget`, and the boundary effect contract. Every command, the header excerpt, and the `make_squares` snippet (`int64_t* make_squares(int64_t n)`) were built and verified before landing. Registered in `SUMMARY.md` under Advanced Topics.
- [x] **Category-specific export-rejection diagnostics + the deeper-auto-boxing decision (v1.x).** Investigating "auto-box `enum` / user-struct / deeper-nested returns" resolved most of it as **won't-fix-by-design**, not a build task: a boxed `enum`/`Option`/struct is an opaque pointer C *can't read*, so auto-boxing it would be a regression in ABI honesty — the honest fixes differ per shape. Instead of building unusable boxing, `cheader::abi_fix_hint` now points each rejected shape at *its* real path: `Option` → NULL-pointer / present-flag; `enum`/`Result` → `#[repr(C)]` struct with a tag field or an opaque handle + accessor exports; tuple → `#[repr(C)]` struct with named fields; `Vec`/`String` **param** → the C `(ptr, len)` idiom; over-deep `Vec` return → flatten or box by hand. Struct → `#[repr(C)]` (unchanged). `reject_hints_are_category_specific` unit test. **The genuinely valuable enum case — `#[repr(C)]` tagged-union enums crossing transparently like `#[repr(C)]` structs — is a real follow-on but a cross-cutting language feature (no repr(C)-enum layout/codegen/header support exists yet), scoped as its own spike, not bolted onto producer mode.** Deeper-nested `Vec` boxing (`Vec[Vec[Vec[scalar]]]`) stays a documented won't-do-yet (recursive-destructor risk for a near-zero-demand shape).
- [x] **`#[repr(C)]` all-unit enums cross transparently (v1.x, repr(C)-enum spike Slice 1).** An all-unit `#[repr(C)]` enum (`enum Status { Ok, NotFound, Denied }`) now crosses the C ABI transparently as an `int64_t` — its value is the discriminant — as both return and param. The header emits `typedef int64_t <Name>;` + an anonymous named-constant `enum { <Name>_<Variant> = tag, … }`; `cheader::is_transparent_boundary_type` / `validate_exports` accept it; a data-carrying `#[repr(C)]` enum stays rejected (the tagged-union case, Slice 2, deferred). No codegen change needed — a single-field `{i64}` enum value is register-identical to `int64_t` on SysV, confirmed by a C round-trip (`test_build_repr_c_enum_roundtrip_from_c_e2e`, asserts `0 1 2 2 1`). Design + slice plan in [`spikes/repr-c-tagged-union-enums.md`](spikes/repr-c-tagged-union-enums.md). **AArch64/Apple ABI CONFIRMED** (2026-07-09) — the `codegen-e2e-macos` CI leg runs this round-trip green on Apple silicon.
- [x] **Data-carrying `#[repr(C)]` enums cross as a boxed tagged union (v1.x, repr(C)-enum spike Slice 2a).** A `#[repr(C)]` enum with unit + single-scalar variants (`Msg { Ping, Data(i64), Ratio(f64) }`) now crosses as a heap-boxed pointer to a faithful C tagged union `{ int64_t tag; union { int64_t Data; double Ratio; } payload; }` + a `karac_free_<name>` destructor — the `Option`/`Result`-over-scalars shape. Feasible without a second enum layout because Kāra already stores a scalar payload in one i64 word (`coerce_to_i64`: ints zero-extended, floats bit-cast, pointers `ptrtoint`), which a per-variant-typed C union member reads faithfully. Reuses Path B boxing (new `boxed_enum_export_names` codegen set; `box_return_value` generalized to size by the value's struct type) with a **distinct box-only destructor** (`is_plain_box` — the Vec-box destructor would misfree the payload word as a `data` pointer). Verified: `test_build_repr_c_enum_tagged_union_from_c_e2e` (`0 1 4242 2.5`), ASAN/LeakSanitizer-clean, `memory_sanitizer` 561 pass, **and green on the `codegen-e2e-macos` Apple-silicon CI leg (arm64 ABI confirmed 2026-07-09)**. Multi-scalar (2b) / aggregate-payload (2c) variants stay rejected. [`spikes/repr-c-tagged-union-enums.md`](spikes/repr-c-tagged-union-enums.md).
- **CI matrix — 3 execution targets + Windows classifier via forced-arch (v1 hardening, 2026-07-09/10).** Stage 1 (macOS arm64, `codegen-e2e-macos`) confirmed the codegen + repr(C) ABI on Apple silicon and immediately paid for itself by catching a real arm64 `#[repr(C)]`-struct-by-value ABI bug (`B-2026-07-09-2`, **fully fixed** — ≤16 B register coercion + >16 B indirect/`sret` on arm64, and the paired >16 B x86-64 `byval`/`sret` hole it surfaced, all params + returns, hardware-confirmed). Stage 2 (`codegen-e2e-linux-arm64`, native `ubuntu-24.04-arm`) is the second arm64 execution leg (Linux AAPCS64, catches any Apple-vs-Linux divergence). Stage 3 (`codegen-e2e-macos-x86_64`, Intel `macos-13`) was **dropped 2026-07-09** — no unique coverage vs `codegen-e2e` (Linux x86-64) + `codegen-e2e-macos` (macOS arm64); scarce Intel runners left it queued 1h+ without allocating. **Stage 4 (`codegen-e2e-windows`) was designed and attempted 2026-07-09/10, then dropped as infeasible today**: `llvm-sys 181` requires `llvm-config.exe` at `<PREFIX>/bin/`, and the official LLVM Windows NSIS installer (and every downstream repackaging — Chocolatey `llvm`, `KyleMayes/install-llvm-action`, direct download) omits it, a long-standing upstream gap verified in-tree via a probe run against the extracted 18.1.8 install. Alternatives all cost more than they buy: shim script → perpetual maintenance surface against llvm-sys flag additions; MSYS2 LLVM → MinGW/MSVC ABI mismatch with our Rust toolchain; from-source LLVM → +1h per push. **The Microsoft x64 aggregate ABI classifier for `B-2026-07-09-8` (1/2/4/8-byte structs coerce to `iN`, else plain-ptr indirect / `sret` — no `byval`) is instead gated by a `KARAC_FORCE_TARGET_ARCH=windows_x86_64` signature-match step on the Linux `codegen-e2e` job**: identical IR lowers identically through LLVM, so a signature match IS an ABI match — the same trick catches arm64 regressions from Linux, and it goes green on every PR run of this work. Windows-side coverage that doesn't need llvm-sys stays in place: the existing `windows-lint` (cfg(windows) clippy) job and the standard `Test (windows-latest)` non-LLVM workspace suite. Reinstate a real Windows execution leg once one of: (a) LLVM ships `llvm-config.exe` on Windows, (b) an accepted community redistribution includes it, or (c) someone signs up to maintain a shim. The matrix is **3 execution targets** (Linux x86-64, Linux arm64, macOS arm64) + Windows lint/test jobs.

**Owner decision (2026-07-08): producer direction is v1.** The flagship adoption pitch; the capability is shipped and proven (Slices 2/3/3½/5 + `forget`), and the remaining polish is bounded. **v1 scope line:** the CI matrix hardening landed 2026-07-09/10 as 3 execution targets (Linux x86-64, Linux arm64, macOS arm64) plus the Windows classifier gated by a forced-arch signature-match on Linux (`B-2026-07-09-8`); the fourth execution leg (native Windows) is deferred pending the upstream LLVM Windows `llvm-config.exe` gap being closed. Windows library artifacts and Rust-host `std`-collision smoothing already shipped 2026-07-08. **Already shipped** (originally on the v1 or v1.x list): the core producer capability, project-mode `[lib]` table, the Path-A round-trip (`B-2026-07-08-4` closed), Path-B auto-boxing + nesting (pulled in from v1.x once the box-to-pointer design made it locally ASAN-verifiable), the book chapter + A/B snippet, and the category-specific rejection diagnostics. Deeper auto-boxing of `enum` / user-struct / >1-level nesting is resolved as won't-fix-by-design; `#[repr(C)]` tagged-union enums are a separately-scoped language feature.

#### Reproducibility CI

- [ ] **Build-twice-and-hash CI for the compiler itself** — enforces the bit-exact reproducible-builds promise (see `design.md § Package System > Reproducibility guarantee`). Failure on diff is a compiler bug, not a user issue.

**Out of scope for v1-P1** (v1.5+ or v2 RFC):
- `karac bench` (needs bench harness + statistical reporting).
- `karac publish` (needs registry publish protocol; gated on adoption signals).
- `karac audit` (needs vulnerability database with package-identity keys).
- Per-package feature flags / `[features]` axis (v2 RFC slot — opens if "ship multiple packages" pattern becomes widely lamented; rejected in v1 to avoid Cargo's worst pain point).
- Centralized registry (deferred indefinitely; git-URL identity + proxy stays canonical).

**Done when (v1-P1):** `karac build` in a project with non-trivial `[dependencies]` resolves through the proxy, writes `kara.lock`, caches compiled deps in `~/.kara/cache/`, and produces a bit-exact-reproducible artifact given a pinned `karac-toolchain.toml`. `karac repl` inside the project tree picks up project deps automatically; `:dep http = "1.2"` works in a session outside any project.

---

### Track 3: Language Server (`kara-lsp`) + IDE Integration

**Goal:** Editor integration as v1 ship-readiness — `kara-lsp` binary + VS Code extension working day-one. Neovim and JetBrains land at v1.x. Promoted from `roadmap.md § Future: Language Server` (post-self-hosting) to v1-P1 on 2026-05-11 under the v66 general-purpose-foundation graduation. See `deferred.md § Language Server (kara-lsp) — v1 Editor Surface` for full rationale.

**Why v1, not Future.** Editor friction kills momentum. Every successful general-purpose language post-2015 shipped editor integration at or before v1. The cohort that tries Kāra in week 1 leaves and does not come back if VS Code support is missing. The analysis is reused — `karac query` and structured-diagnostic JSON already exist; the LSP binary is plumbing over the existing analysis surface plus IDE-side glue, not new compiler design.

**Slice 1 landed 2026-07-11** — the `kara-lsp` workspace member (`lsp/`, mirrors `kernel/`/`playground/`), a stdio `lsp-server`/`lsp-types` transport, the `initialize`/`shutdown` handshake (advertising `textDocumentSync = FULL`), and **live diagnostics**: every `textDocument/didOpen`/`didChange`/`didSave` runs the new `karac::check_source` (the interpreter-free sibling of `run_playground`, extracted with it onto a shared `run_static_checks` so phase order stays single-sourced) and publishes the result via `textDocument/publishDiagnostics`; `didClose` clears them. Byte-span → LSP `Range` mapping is UTF-16-correct (matters for Kāra's non-ASCII source). Analysis is `catch_unwind`-guarded so a compiler-phase panic on half-typed source never drops the connection. No user code is executed (static feedback only). Verified end-to-end over an in-memory connection (`lsp/tests/server.rs`, 2 tests) plus 6 `analysis` unit tests; the by-value `serve(connection)` closes the clean-shutdown deadlock. Remaining floor items (hover, definition, completion, formatting, VS Code extension) are the next slices, each a thin layer on this connection.

**v1 floor (must ship):**
- [x] `kara-lsp` binary — long-lived process wrapping `karac` analysis surface; LSP protocol over stdin/stdout. **✓ (slice 1, 2026-07-11)** — `lsp/src/{main,lib}.rs`; stdio transport, initialize/shutdown handshake, FULL text sync.
- [ ] Syntax highlighting (TextMate grammar; book infrastructure mostly exists).
- [x] Diagnostics streaming via `textDocument/publishDiagnostics` over existing `karac` structured-diagnostic JSON. **✓ (slice 1, 2026-07-11)** — over `karac::check_source` (parse → desugar → resolve → typecheck → effect → ownership; interpreter-free), phase carried as the diagnostic `code`, `catch_unwind`-guarded, UTF-16 ranges.
- [x] Go-to-definition (resolver symbol table). **✓ (slice 3, 2026-07-11)** — `textDocument/definition` resolves the reference at the cursor (innermost `ResolveResult.resolutions` span) to its definition's source span (`symbol_table.get_symbol(id).span`), via the new `karac::goto_definition` query; prelude/builtins (synthetic zero-length span) return no definition. Single-document today.
- [x] Hover — type + effect signature (typechecker + effectchecker already produce this). **✓ (slices 2 & 6, 2026-07-11)** — `textDocument/hover` returns the inferred type of the innermost expression under the cursor (smallest containing `expr_types` span) as a fenced `kara` block, and — when the cursor is on a **function reference** — its effect signature on a line below (`**effects:** writes(Db)` / `pure` / `_` for polymorphic), via the new `karac::hover_at` library query. Effects come from `EffectCheckResult` (declared win over inferred), rendered by verb + resource; position→byte-offset mapping is the UTF-16-correct inverse of the diagnostics range mapping, `catch_unwind`-guarded.
- [x] Find references (resolver symbol table). **✓ (slice 5, 2026-07-11)** — `textDocument/references` returns every use-site resolving to the symbol under the cursor (the inverse scan of the `ResolveResult.resolutions` map used by go-to-definition), honoring `includeDeclaration`; works from a reference or the definition. New `karac::find_references` query. Single-document today.
- [x] Document symbols / outline (parser AST). **✓ (slice 3, 2026-07-11)** — `textDocument/documentSymbol` returns a flat outline of every top-level item (function/struct/enum/union/trait/const/type-alias/…) in source order, via the new `karac::document_symbols` query (root-scope symbols with a real span; prelude names and enum variants filtered out; `SymbolKind` mapped to the LSP kind).
- [ ] **Type-aware completion** — `.`-completion of methods/fields on the receiver type. Requires partial-parse + typecheck-of-incomplete-source (~4-6 weeks engineering). The line below which the LSP feels half-broken.
- [x] Formatting via LSP (wraps `karac fmt`). **✓ (slice 4, 2026-07-11)** — `textDocument/formatting` returns a single whole-document `TextEdit` from the new `karac::format_source` query (parse + `formatter::format_program`, no desugar so surface syntax round-trips); already-formatted source returns zero edits, parse errors return null.
- [ ] Signature help (parameter-info popup).
- [ ] VS Code extension wrapping `kara-lsp` — language identifier, file-watch, marketplace listing.

**v1 stretch (ship if engineering time allows, else v1.1):**
- [ ] Rename symbol (`textDocument/rename`).
- [ ] Code actions — apply structured fix-diffs from `karac` diagnostics.
- [ ] Semantic tokens (full semantic highlighting beyond TextMate).
- [ ] Workspace symbols / global search.

**v1.x explicitly (post-launch):**
- [ ] **Effect-aware completion** — `.`-completions filtered by effect compatibility with the surrounding `with`-clause. Kāra-specific differentiator; ~2-3 weeks on top of type-aware. Ship post-launch as a "Kāra LSP now does X" announcement when the v1 floor is solid.
- [ ] Inline-explain / type lens — surface `karac explain` reasoning in-editor.
- [ ] Refactoring (extract function, inline variable).
- [ ] Neovim built-in LSP client config.
- [ ] JetBrains plugin.

**Future direction (kept at `## Future: Language Server and Reactive Query-Based Compilation` below):** the reactive Salsa-style subscribe/notify LSP — sub-100ms live-edit re-computation, function-local incremental analysis — is post-self-hosting. The v1 LSP runs a batch query model over the existing `karac query` surface — sufficient for editor integration at launch; the reactive layer becomes necessary at scale.

**Done when (v1):** A first-run user opening a `.kara` file in VS Code gets working syntax highlighting, diagnostics on save, hover-for-type-and-effects, go-to-definition, and `.`-completion within their first session. No "extension marketplace tells me I need to install three things first" friction.

---

### Track 4: Discovery — items added as found during demo build

This subsection is intentionally empty at Phase 8.5's creation (2026-05-08). It accumulates items that surface during Phase 8 / 9 / 10 / 11 demo work and that are *v1 ship-blocking but not demo-blocking* — the kind of "we can't ship v1 without this, but it doesn't gate the demo" item that is hard to predict in advance.

**Filing protocol.** When adding an item: name it; record the demo-build date / context where it surfaced; explain why v1 needs it; note any sequencing constraints (prerequisite work, downstream demos that depend on it). Each entry should be terse — pointer to the durable design content in the relevant phase tracker rather than full design discussion inline.

*(No items yet.)*

---

### Track 5: Compile-speed CI gate (graduated from brainstorm v69 § Gap 1, 2026-05-20)

**Goal.** Compile speed as a tracked v1 metric with a regression gate, a curated corpus, and a published number. Today's situation: kata bench scripts measure cold *memory* footprint of `karac build` vs `rustc -O` but not elapsed time, no CI gate exists, no published number the README/blog can quote. This track lands the missing infrastructure.

**Why v1 ship-readiness.** The easiest dismissal vector for Go-shop engineers comparing Kāra to Go is "their compile loop is slow." Without a regression gate, the v1 number is whatever happens to be true the day it ships, not a number we protected over time. With a gate, every catastrophic compile-time regression (the 2026-05-12 Maranget O(N²) shape) fails the build instead of being caught accidentally by an unrelated memory measurement.

#### Corpus

- [x] **`bench/compile_speed/` directory + curated corpus + synthetic stress program.** *(Landed 2026-07-22: `gen_synthetic.py` emits deterministic ~10.9K-LOC `.kara`/`.rs` twins — 100 clusters of small fns in a linked call graph, generics with trait bounds, 34 traits × 100 impls, enums+match, declared-effect pub fn chains, Vec+closure bodies — twin checksums verified identical by `bench.sh`'s oracle; seed kata = two-sum hash_map pair copied plain from kara-katas. Backend-shape member still copies in when its kata lands.)* Three corpus members:
  1. **Curated kata subset** — copies of selected katas from `kara-katas`, lifted as plain files (no sync infrastructure; `kara-katas` evolves independently). Seeded with one kata to start; the set evolves over time as the template stabilizes. Specific kata selection deferred to corpus-setup time.
  2. **Synthetic 10K-LOC front-end-stress program** — many small functions, generics, effect declarations, trait impls. Designed to stress the typechecker / effectchecker / ownership-analyzer / monomorphization at scale (where the front-end cost surface lives, which algorithmic katas systematically under-sample). **v1-required, not deferred** — without it the gate has a known blind spot.
  3. **Backend-shape number from `kara-katas`** — when the backend-service kata lands in [`kara-katas/PLAN.md`](../../kara-katas/PLAN.md) (priority #1, v1-required), it becomes the curated subset's backend representative *and* the real-shape public-quote number ("on a 10K-LOC backend program, `karac build` runs at Nx of `rustc -O`"). Parallax-lite stays a demo, not benchmark infrastructure.

#### Measurement protocol

- [x] **`bench/README.md`** — canonical bench-setup instruction covering hyperfine discipline (warmup/runs for short vs long workloads), rusage discipline (single-sample memory), artifact-deletion-for-cold protocol, output format. Adds **compile-elapsed time** as a tracked measurement alongside the existing compile-memory measurement (literally one missing block in the existing kata `bench.sh`, swapping `/usr/bin/time -l` for `hyperfine --warmup 1 --runs 10`). Mirrored as [`kara-katas/BENCH.md`](../../kara-katas/BENCH.md) so kata authors have a template.
- [x] **Baseline: `rustc -O` (always in CI; the peer comparison on the same architectural surface — LLVM, monomorphization, ownership).** *(compile-speed.yml benches karac and rustc on every PR + main push.)* `clang -O2` measured opportunistically where C source exists (delta isolates karac-specific frontend cost from LLVM-backend cost); not a CI gate, no fresh-C-translation obligation. `go build` measured at launch-time only (published with explicit caveat: Go optimizes for compile speed by design; karac, like Rust, optimizes for runtime perf with proportional compile cost).
- [x] **Cold only at v1.** Incremental compile-speed deferred until reactive query model lands (post-self-hosting).

#### CI workflow

- [x] **Threshold: 30% initial, ≤5% long-term target.** *(Gate is on the karac/rustc RATIO per workload — stable across runner generations; `compare.py --threshold`.)* Generous at start — during active development, legitimate changes shift compile time by a few % easily, and a tight gate would flake without catching the regressions that matter. The 30% gate catches order-of-magnitude blowups (Maranget shape) without false-positives on routine work. Tighten as data accumulates: corpus + 10–20 baseline runs reveals noise floor + steady-state karac-vs-rustc ratio, which together set the next threshold step. Doc commits to the trajectory (generous → tight); CI carries the current value.
- [x] **PR-trigger workflow + main-merge baseline update.** *(One workflow, two legs — `.github/workflows/compile-speed.yml`: PR leg gates + sticky comment + step summary; main leg rewrites `baseline.json` only on >5% ratio drift, refresh commit paths-ignored against self-retrigger. Baseline bootstraps empty from the first main run.)* PR runs the corpus, parses hyperfine output to JSON, compares against `bench/compile_speed/baseline.json` (committed to repo), posts PR comment with verdict + per-benchmark ratio (karac vs rustc) + per-benchmark delta vs baseline. Fails the job if any benchmark exceeds 30% over baseline. Separate main-merge workflow re-measures and updates `baseline.json` as a follow-up commit/PR. Stock GH Actions cache; no benchmark-result caching at v1.
- [x] **`bench/bench.sh` top-level aggregator** *(landed 2026-07-22; globs every track's bench.sh, continues past failures, nonzero exit at end)* — thin shell wrapper invoking each track's own bench script (`hash_quality`, `hot_swap_cost`, `indirection_cost`, `compile_speed`). One command runs everything; each track stays independently runnable.

#### Publication

- [ ] **Compile-elapsed numbers published in both packages.** Per-kata READMEs in `kara-katas` gain a compile-elapsed table alongside the existing runtime + compile-memory tables (per-shape reference data); `bench/compile_speed/README.md` here publishes the curated + synthetic aggregate (gate-protected, public-quote-able, launch-blog headline). Both publish, neither defers to the other.

#### Related: kata corpus coverage

- [ ] **[`kara-katas/PLAN.md`](../../kara-katas/PLAN.md) tracks the multi-quarter sample-skew closure** (algorithmic-only corpus systematically under-samples backend / front-end-stress shapes — the cost surfaces that matter at 10K LOC). Five coverage axes (shape, scale, language-feature stress, stdlib breadth, comparison targets) with priority ordering and a "done when" criterion. The backend-service kata is **v1-required** (it's the real-shape public-quote source for this track's published numbers); other coverage closures land post-v1.

Resolution archive: [`brainstorming/archive/v69_go_parity_gaps.md § Gap 1`](../brainstorming/archive/v69_go_parity_gaps.md). **Done when (compile-speed gate)**: `bench/compile_speed/` corpus exists with seed kata + synthetic; `bench/bench.sh` aggregator runs it; PR-trigger CI workflow compares against `baseline.json` and fails on >30% regression; per-kata READMEs in `kara-katas` and `bench/compile_speed/README.md` publish elapsed-time numbers.

---

### Track 6: Effect-driven debugging polish (graduated from brainstorm v69 § Gap 4, 2026-05-20)

**Goal.** Turn the already-designed effect-driven debugging metadata (`std.panic`, `std.runtime::list_tasks`, SpawnSiteId, parent-task chain, RC-fallback annotations) into a polished user-facing surface. The metadata is the differentiator; the *experience of using it* is the polish gap. Few weeks of engineering, not few years — but the screenshot that goes on the launch blog post needs to exist.

**Audience priority**: operator-first (CLI / crash-report surface gets the differentiation polish), developer-second (IDE/LSP at v1 ships functional, not flashy). The 3am-pager argument is the v1 pitch; blast radius of bad operator UX is asymmetric (incident extension vs minute wasted on hover).

#### CLI surface

- [ ] **`karac debug <crash.json>`** — one-shot crash-report renderer. Reads JSON produced by `std.panic` (file path or `-` for stdin). Renders human-readable: panic site source line with column highlight, effect set in compact form, parent task chain as visual graph (`└──▶ fetch_user_dashboard ──▶ par_block@spawn_site_42`), RC-fallback annotations with explanations, build-metadata footer. `--output=json` re-emits structured form.
- [ ] **`karac inspect <pid>`** (Linux + macOS at v1) — attaches to a running process via `ptrace` (Linux) or `task_for_pid` (macOS); reads runtime metadata via the per-worker cooperation hook from Track-5-adjacent Gap 2 work (same surface, reused); dumps `list_tasks()` / `list_par_blocks()` output without requiring code changes. Equivalent to Go's `go tool stack`. `--once` (default) for one-shot; `--watch` for periodic re-dump (high incident-response value). **Windows `karac inspect` deferred to v1.x** (different debug APIs: `DebugActiveProcess` / `ReadProcessMemory`).
- [ ] **`karac explain-panic <crash.json>` deferred to v1.x** — AI-style natural-language explanation. Requires committing to an AI provider integration (heavier architectural decision); also depends on structured-rendering foundation being solid first.

#### Effect-set rendering library

- [x] **Three rendering modes, JSON as structured root:** *(compact + grouped + `effects_json` structured root landed in `src/effect_render.rs`, 2026-07-23; annotated-source view remains the v1.1 IDE deferral below.)*
  - **Compact** (CLI / crash reports): `effects: reads(UserDB) + sends(Network) + panics(IoError)` — one-line, matches source-declaration syntax (`+` is the language's effect-combination operator).
  - **Grouped** (IDE hover): multi-line, categorized into Resource / Execution / Panic. Empty groups omitted (no "Execution: (none)" visual noise).
  - **Annotated source view**: NOT at v1; v1.1 IDE feature (effect markers in gutter, click-to-explore).
- [x] **Stable group-first-then-alphabetical ordering** — same effect set always renders identically (load-bearing for diffability, e.g., `karac query effects --diff` between revisions). *(Keyed by `(verb_order, keyword)` so distinct user-defined verbs never collide — a latent bug in the old rank-only REPL ordering, now fixed by consolidation.)*
- [x] **Empty-effects rendering**: `(none)` in compact form; `(pure)` in grouped form — "pure" carries meaningful information.
- [x] **TTY-aware colors** (resource verbs cyan, execution verbs yellow, panic types red); `ColorChoice::Auto` auto-detects via std `IsTerminal` and honors `NO_COLOR`; `Always`/`Never` for explicit control. JSON output never has color codes (asserted).

#### Rendering crate

- [~] **Rendering logic lives in a shared crate/module used by `karac` binary and LSP server.** *(Slice 1, 2026-07-23: the logic lives in `src/effect_render.rs` — a module in the compiler lib crate the `lsp` member already depends on, so it is shareable without a separate crate; the REPL footer already consolidated onto it. Remaining: wire the LSP hover + `karac query effects` consumers onto it, and add the crash-report render entrypoints — next slice.)* Original intent: Extends the existing structured-diagnostic infrastructure (same machinery as compile-time error rendering: source-span highlighting, color/no-color, terminal width, ANSI). Don't fork; load-bearing primitives are shared between compile-time and runtime diagnostics. API exposes structured + rendered forms together (e.g., `render_crash_report(report, opts) -> String`, `crash_report_to_json(report) -> serde_json::Value`). LSP server calls the same functions and routes output into LSP-shaped responses. **Non-Rust LSP future**: via subprocess + JSON, not FFI; JSON contract is stable, Rust crate is an implementation detail.

#### `std.tracing` cross-link

- [ ] **Crash report carries `trace_id` + `span_id` when std.tracing context is active.** When `std.panic` constructs the crash report, asks std.tracing "is a span active?"; if yes, captures the IDs in an optional `tracing` block (OTel field-name convention exactly; consuming tools — Jaeger, Tempo, Datadog — map directly). Graceful absence when no active span / std.tracing not compiled in. CLI renders as a separate line when present: `trace: abc123def4567890 (span: 1234567890abcdef)` — copy-pasteable into any OTel backend. Skip the line entirely when absent. **Not at v1**: URL construction (requires per-org config), cross-process trace stitching, retroactive span enrichment, IDE-hover trace rendering. (Note: this is cross-implemented in the `std.tracing` line item under Phase 8 § Backend Platform — both items reference the same work.)

#### 3am-operator runbook

- [ ] **3am-operator runbook in the book** — v1 deliverable (not v1.x). Short and starter-shaped, not exhaustive. Contents: (a) entry point ("when paged with a crash JSON, run `karac debug <file>` first"); (b) 3–5 worked examples (panic on resource, panic on RC fallback, par-block stall) with actual rendered output; (c) common patterns and what to check next (`panics(IoError) + reads(UserDB)` → DB connectivity); (d) escalation (`--output=json` for AI agent / bug report). Grows from incident data once Kāra is in production.

Resolution archive: [`brainstorming/archive/v69_go_parity_gaps.md § Gap 4`](../brainstorming/archive/v69_go_parity_gaps.md). **Done when (effect-driven debugging polish)**: `karac debug` renders a real demo panic in a form screenshottable for the launch blog post. `karac inspect <pid>` attaches to a running Kāra HTTP server and dumps task state without code changes. LSP hover shows grouped-form effect signatures. The 3am-operator runbook exists in the book with at least 3 worked examples.

---

**Phase 8.5 done when:** v1 ships. The combined Track 1 v1 surface + Track 2 v1-P1 surface (resolver + lockfile + cross-compile UX) + Track 3 v1 floor (`kara-lsp` + VS Code) + Track 5 (compile-speed CI gate) + Track 6 (effect-driven debugging polish) + reproducible-builds CI + any Track 4 items that accumulated during Phase 8–11 demo build are landed. Track 1's v1.1 follow-ups (Jupyter kernel) and Track 3's v1.x follow-ups (Neovim, JetBrains, effect-aware completion, inline-explain) ship in their own windows after v1. Gap 2's v1.1 deferrals (wall-time profile, execution tracer) also ship post-v1 — additive extensions to the v1 sampling-profiler foundation.

---

## Phase 9: Gradual Verification Enforcement

**Goal:** Enforce the gradual verification features whose syntax was parsed in Phase 2. Adds the correctness layer on top of the working MVP compiler — language semantics are fully locked before new backends.

### Refinement Types (Level 2)
- [x] Constraint validation at construction boundaries
- [x] Compile-time elision when provable (v1 two-rule procedure)
- [x] `TryFrom` generation for fallible construction
- [x] Reject implicit runtime-value narrowing — require explicit `R.try_from(x)?` or `x as R`

### Distinct Types
- [x] Enforce opacity (no implicit operations on the underlying type)
- [x] Verify `#[derive]` compatibility
- [x] Interaction with refinement types: `distinct type ValidPort = u16 where self >= 1 and self <= 65535`

### Contracts (`requires` / `ensures` / `invariant`)
- [x] Verify contract expressions are pure (effect set ⊆ `{panics}`)
- [x] Insert runtime checks in debug builds
- [x] `old(expr)` desugaring with `Clone` requirement
- [x] Invariant insertion at every `pub` method exit
- [x] Strip all contract machinery in release builds

### Extended Patterns
- [x] Range patterns: `LITERAL "..=" LITERAL` in match arms (integer and `char` types). **Matching shipped end-to-end** — AST `PatternKind::RangePattern` (`src/ast/patterns.rs`), parser (`src/parser/patterns.rs`, all five forms: `lo..hi`, `lo..=hi`, `..hi`, `..=hi`, `lo..`), typechecker (`src/typechecker/patterns.rs`), interpreter (`src/interpreter/pattern_match.rs`), codegen (`src/codegen/control_flow_match.rs:357`, signed/unsigned-aware comparisons). Exhaustiveness *integration* for ranges shipped too — see the exhaustiveness item below.
- [x] `@` bindings: `IDENT "@" PATTERN` — capture value while testing pattern. **Shipped end-to-end** — parse + typecheck + interpreter were already complete; the codegen no-op (the `_ => Ok(())` catch-all in `bind_pattern_values` and the `_ => true` catch-all in `compile_pattern_condition`, which left compiled `@`-binding matches silently broken) is now fixed. `bind_pattern_values` (`src/codegen/pattern_binding.rs`) binds the outer alias to the whole scrutinee via a synthetic leaf `Binding` at the AtBinding's span, then recurses into the inner pattern so nested bindings (`whole @ Some(x)` → `x`) materialize; `compile_pattern_condition` (`src/codegen/control_flow_match.rs`) delegates to the inner pattern's condition (the alias itself is irrefutable). Composes with ranges, enum variants, and or-patterns. Tests: 3 E2E in `tests/codegen.rs` (`@` + range with alias reused, `@` + `Some(x)` binding both outer alias and inner payload, `@` + or-pattern).
- [x] Exhaustiveness: range patterns integrate with Maranget's algorithm (cover their value set). **Shipped** — and it was a *soundness* fix, not just precision: the prior `RangePattern => Pat::Wildcard` lowering made a lone range arm act as a catch-all, so `match n: i64 { 1..=10 => .. }` was wrongly reported exhaustive. Integer/`char` literals and ranges now lower to inclusive `PatCtor::IntRange { lo, hi }` intervals (i128 space) and integer/char columns are reasoned about by **interval splitting** (`int_column_useful` in `src/exhaustive.rs`): the type domain (or a query range) is partitioned at the endpoints present in a column into atomic sub-intervals, so an uncovered sub-interval is a missing-value witness (exhaustiveness) and a range tiled by the union of earlier ranges is correctly unreachable (precise reachability, incl. `1..=10 | 11..=20` subsuming `1..=20`). Full-domain coverage by ranges is exhaustive with no wildcard (e.g. `0..=127 | 128..=255` over `u8`, `..=0 | 1..` over `i64`). Witness rendering: singleton → value, bounded interior gap → the missing interval (`10..=19`), extreme-touching gap → a representative value. `i128`/`u128` (domains that don't fit i128) keep the sound open-domain default-matrix behaviour; float literal patterns still defer (no Eq/Hash story for `f64`). Tests: 11 in `tests/typechecker.rs`.
- [x] Composition: range + or-pattern, `@` + range, `@` + or-pattern, nested in struct/enum fields. Range compositions match correctly (or-pattern and nested struct/enum field cases verified in codegen); `@`-binding compositions (`@` + range, `@` + or-pattern) now compile correctly after the `@`-binding codegen fix above — covered by the new E2E tests. Range-pattern *exhaustiveness* across composed ranges (or-patterns, gaps, union coverage) is handled by the interval-splitting exhaustiveness item above.

**Done when:** `type Percentage = f64 where self >= 0.0 and self <= 100.0` compiles with boundary checks. `distinct type UserId = i64` rejects implicit operations. `requires`/`ensures` annotations produce runtime checks in debug and are stripped in release. All three features compose correctly (e.g., distinct + refinement types). Range patterns and `@` bindings work in match, `if let`, and `while let` contexts.

---

## Phase 10: Additional Compilation Targets

**Goal:** Same language compiles to multiple targets.

> **Status (2026-07-20) — most shipped Phase 10 work lives in the tracker; the checkboxes here have been reconciled but stay coarse.** Per `implementation_checklist/phase-10-targets.md` (≈88/93 done), these are **DONE**: TLS provider cross-compile to all v1 targets + CI gate, `std.web` / `std.wasi` gated effect modules, `host fn` (parse → typecheck → native lowering), WASM **strip-by-default** (482 KiB → 30 KiB browser hello), the dual / threaded WASM runtime archives + sequential cooperative scheduler, the **GPU compute-shader v1 ship gate** (MET on Metal 2026-07-10, also validated on Vulkan/lavapipe on Linux 2026-07-18 — WGSL codegen, wgpu integration, `gpu.dispatch`, `GpuSafe` type checking, `#[gpu]` effect enforcement, **multi-field layout groups → coalesced GPU buffers**, and **`KARAC_GPU` device-select + `KARAC_GPU_BACKEND=cpu` software adapter** all shipped), and **atomic RMW ops + hardware fences** (2026-06-04/05). The boxes that remain `[ ]` below are genuinely open: the **GPU CUDA/NVPTX opt-in path** (`--target cuda`); **FPGA** is unstarted; **WebAssembly**'s core lowering + threaded concurrency are in, but the sequential event-loop-yield scheduler refinement is deferred post-v1. Trust the tracker over the box state in this section.

> **v66 graduation update (2026-05-11):** **GPU compute shaders pulled forward to v1 ship-readiness** as a P1 gate, no longer Phase 10. The implementation tasks below stay in Phase 10's tracker for sequencing (codegen work proceeds during the Phase 8–11 window) but the gate is v1 ship, not "post-v1 target completion." Multi-vendor coverage already satisfied by the existing wgpu-primary design (Metal on macOS, Vulkan on Linux, DX12 on Windows, WebGPU in browser; CUDA opt-in via `--target cuda`). See `deferred.md § Additional Compilation Targets (Phase 10)` for the v1 pull-forward note, and `brainstorming/archive/v66_general_purpose_with_data_bonus.md § 5.2` for the decision rationale. WebAssembly and embedded targets stay at Phase 10 post-v1.

- [ ] **WebAssembly:** LLVM WASM backend. Concurrency lowering: sequential cooperative scheduling on the main thread by default; `--features wasm-threads` opts into Web Workers + SharedArrayBuffer + atomics (user deploys with COOP/COEP headers). Compiler-managed transparent threading (ownership-proven partitioning without opt-in) is deferred post-v1 — see `docs/deferred.md § Compiler-Managed Transparent Threading on WASM`. Source-level `go`/channel/`par` semantics are target-agnostic — see `design.md § Concurrency Across Targets`.
- [ ] **GPU compute shaders — v1 ship gate (P1).** Compile `#[gpu]`-annotated functions to GPU kernels and wire `gpu.dispatch` to invoke them. Full design of the `#[gpu]` constraint, `GpuSafe` type bound, and `gpu.dispatch` effect semantics is already in `design.md § GPU Subset Constraints`. Pulled forward from Phase 10 to v1 on 2026-05-11 (v66 graduation). Multi-vendor coverage via the wgpu-primary path (below) satisfies the dogfooding requirement (project leader develops on macOS — Metal coverage at v1 is non-negotiable) and the systems-language-target-completeness requirement.

  **Compilation strategy — two paths:**

  - **Primary path (wgpu/WGSL):** `#[gpu]` functions compile to WGSL shaders. At runtime, [wgpu](https://github.com/gfx-rs/wgpu) selects the best available GPU API for the platform and uses the highest-performance GPU device (discrete preferred over integrated):
    - macOS / iOS → Metal API
    - Linux → Vulkan API (works on NVIDIA, AMD, and Intel GPUs)
    - Windows → DX12 API, Vulkan fallback (works on NVIDIA, AMD, and Intel GPUs)
    - Browser (WASM target) → WebGPU API
    Vulkan and DX12 are APIs, not hardware — an NVIDIA GPU on Linux uses Vulkan by default and is fully utilized. No `--target` flag needed. The same compiled binary runs on all wgpu-supported platforms. Build normally: `karac build`.

  - **CUDA path:** `#[gpu]` functions compile to PTX via LLVM's NVPTX backend. Requires an explicit target flag: `karac build --target cuda`. NVIDIA hardware only. Use this path when you need NVIDIA-specific libraries (cuBLAS, cuDNN) or are squeezing out the last bit of NVIDIA-specific performance. For general GPU compute on NVIDIA hardware, the wgpu/Vulkan path already works — CUDA is not required just to run on an NVIDIA GPU.

  **Runtime GPU selection:**

  - wgpu auto-selects the highest-performance available device (discrete GPU preferred over integrated).
  - Users with multiple GPUs can override via the `KARAC_GPU=<index>` environment variable (0-indexed, ordered by wgpu's device enumeration). No API or recompile needed.
  - `KARAC_GPU_BACKEND=cpu` forces a **software** (CPU) adapter — lavapipe/llvmpipe on Linux, WARP on Windows — so a `gpu.dispatch` program can exercise the real GPU pipeline on a GPU-less host (a debug escape hatch, distinct from `karac run --interp`, which only reproduces the kernel *logic*). It is checked before `KARAC_GPU`; if no software adapter exists the program exits with a structured error naming the fix (`mesa-vulkan-drivers` on Linux), and any value other than `cpu` is rejected. No API or recompile needed.
  - If no compatible GPU device is found at runtime, the program exits with a structured error:
    ```
    error: no GPU device available
    hint: this program requires GPU support (gpu.dispatch called at runtime)
    hint: set KARAC_GPU_BACKEND=cpu to run kernels on CPU for debugging (performance will be severely degraded)
    ```
    No silent CPU fallback. The `KARAC_GPU_BACKEND=cpu` escape hatch is for debugging only and is explicitly labelled as such.

  **Implementation tasks:** — *granular slice breakdown with readiness/sequencing (front-end-now vs codegen-blocked-on-self-hosting) lives in [`implementation_checklist/phase-10-targets.md`](implementation_checklist/phase-10-targets.md) "GPU compute shaders — slice breakdown"; the coarse boxes below mirror it.*
  - [ ] WGSL codegen: lower `#[gpu]` function bodies to WGSL compute shaders
  - [x] wgpu integration: device initialization, buffer management, shader compilation, dispatch
  - [x] `gpu.dispatch` runtime call: pack arguments into GPU buffers, submit compute pass, read results back
  - [x] Layout groups → GPU buffers: `group physics { position, velocity }` maps to a single GPU buffer with coalesced access
  - [x] `GpuSafe` type checking: reject heap types (`String`, `Vec[T]`, etc.) in `#[gpu]` call graphs (already specified in design.md)
  - [x] Effect enforcement: reject `allocates(Heap)`, `panics`, I/O effects in `#[gpu]` call graphs (via existing effect checker)
  - [ ] CUDA path: NVPTX codegen for `--target cuda` builds
  - [x] `KARAC_GPU` / `KARAC_GPU_BACKEND` environment variable handling
- [ ] **FPGA bitstreams (future goal):** As described in design.md Feature 7; not yet designed in detail
- [x] **Atomic RMW operations:** `swap`, `compare_exchange`, `fetch_add`, `fetch_and`, `fetch_or` on `Atomic[T]` — shipped (2026-06-04/05; v1 originally shipped `load`/`store` only)
- [x] **Hardware fences:** `fence(Ordering)` (unsafe) / `compiler_fence(Ordering)` (safe) — hardware and compiler barriers

**Done when:** A compute-bound Kāra program compiles to WASM and runs in a browser. A data-parallel Kāra program with `layout` blocks compiles to a GPU compute shader and runs on a GPU. FPGA support is a stretch goal beyond this phase.

---

## Phase 11: Standard Library — Long-Tail

**Goal:** Domain-specific stdlib that programs need beyond the floor — numerical/data-science stack, security types, embedded primitives, plus codegen IR optimization. **End of this phase = v1 release.** The split from [Phase 8](#phase-8-standard-library--floor) lets v1 ship semantically locked (after Phase 9) and target-complete (after Phase 10) before the long-tail lands. Co-locating the long-tail with target work pays off concretely: the numerical stack composes with the GPU call-site backend, embedded primitives co-design with the embedded target, and WASM portability is already proven for new modules. **v64 reshape (2026-05-09):** the backend-platform stdlib bundle (`std.http`, TLS, WebSocket, etc.) was lifted into Phase 8 floor — see [Phase 8 § Backend Platform](#backend-platform-v64-lifted) — leaving Phase 11 narrowly scoped to the numerical / data-science / security / embedded long-tail. Working tracker: [`implementation_checklist/phase-11-stdlib-longtail.md`](implementation_checklist/phase-11-stdlib-longtail.md) (physically reorganized out of the Phase 8 tracker, 2026-06-06).

> **Built on the self-hosted compiler (2026-06-10 resequence).** Phase 11 executes *after* [Phase 12 Self-Hosting](#phase-12-self-hosting), not before it. **"Built on the self-hosted compiler" ≠ rebuild Phase 11.** Sort the work by the *language of the artifact* and whether it already exists — three buckets:
>
> - **Stdlib-in-Kāra — reused verbatim, zero rewrite.** `runtime/stdlib/tensor.kara`, `stats.kara`, and every other `*.kara` (plus future Column/DataFrame/`LazyDataFrame`, `std.embeddings`, `std.autograd`, `Secret[T]`/`ConstantTimeEq`/`Zeroize`, `CircularBuffer[T]`, data docs/examples) are already Kāra source. The self-hosted `karac` *compiles* them exactly as the Rust compiler did — it doesn't matter who compiled them before. No phase distinction applies.
> - **Compiler-internal already built in Rust — *ported* during self-hosting, not redesigned.** The Tensor type-system + codegen is substantially built: `Type::Shape` + shape-kind machinery, shape literal grammar, `src/codegen/tensor.rs`, `src/typechecker/expr_method_tensor.rs`, literal-involved promotion (Q4) — plus all of Phase 9 enforcement and Phase 10 target codegen. A Kāra compiler can't link Rust passes, so these are re-expressed in Kāra; but the working+tested Rust version is the spec, the **differential oracle** (same input through both, diff the output), and a near-line-for-line translation source. The hard part (design + debug) is sunk; the port is mechanical.
> - **Compiler-internal NOT yet built — built once, directly in Kāra.** `f16`/`bf16` *lowering* (keywords reserved, LLVM `half`/`bfloat` emission not), `Secret[T]` derive codegen, the entire **Embedded / Hardware Primitives** subsection (inline `asm`/`global_asm`, volatile intrinsics, `#[interrupt]`, linker control, `Atomic` codegen), the **Codegen Optimization (IR quality pass)**, and Phase 10's residual GPU codegen. **This is the only bucket the pivot saves work on** — don't add these to the Rust `karac` after the pivot; build them straight into the self-hosted compiler.
>
> The IR-quality pass is in the unbuilt bucket, so the self-hosted compiler's *own* speed is recovered by **bootstrap staging**, not by porting the pass into Rust first — see [§ Codegen Optimization](#codegen-optimization-ir-quality-pass) and [Phase 12](#phase-12-self-hosting).

### `f16` / `bf16` Numeric Primitives
- [x] Reserve `f16` and `bf16` as lexer-level keywords in v1 (compile error if used as identifiers — prevents future source-breaking rename). ✓ Shipped since the first commit; lexer tests + E2E re-verified 2026-06-06 — see `implementation_checklist/phase-11-stdlib-longtail.md`.
- [x] Type system: add `f16` (IEEE 754-2008 half-precision) and `bf16` (bfloat16) as primitive types with the same trait surface as `f32`/`f64` (`PartialEq`, `PartialOrd`, arithmetic traits, `Copy`) but NOT `Eq`/`Ord`/`Hash`.
- [ ] Codegen: lower `f16` → LLVM `half`, `bf16` → LLVM `bfloat`. Native instruction emission on capable targets; software promotion to `f32` on others with a `f16_software_emulated` performance lint.
- [x] Implicit widening: `f16` → `f32`, `bf16` → `f32` (both lossless).
- [x] Literal suffixes: `1.0f16`, `1.0bf16`.
- [ ] Stdlib: `F16`, `BF16` total-order wrappers (same pattern as `F32`/`F64`).
- [ ] `Tensor[f16, Shape]` and `Tensor[bf16, Shape]` valid once both this and the numerical stdlib ship.

See `design.md § f16 / bf16 Implementation` for full design shape.

### Numerical and data-science stdlib

Semantics in `design.md § Numerical Types`, `§ Numeric Semantics > Literal-involved promotion`. Implementation tasks in [`implementation_checklist/phase-11-stdlib-longtail.md § Numerical and data-science stdlib`](implementation_checklist/phase-11-stdlib-longtail.md#numerical-and-data-science-stdlib).

**Type system (forcing functions).**
- [x] `Tensor[T, Shape]` — shape-typed N-D container with static + dynamic (`?`) dims. Q1 (1A).
- [x] Shape as a new generic-parameter kind; shape literal grammar; `Dim`-kinded params with compile-time unification. Q2 (2C). Arithmetic on shape params (`[A + B]`) deferred to v1.5.
- [x] Implicit scalar-tensor broadcasting (`arr + 1`); explicit methods (`arr.broadcast_add(row_vec)`) for tensor-tensor. Q3 (3B+3C hybrid).
- [x] Literal-involved promotion in numeric binary operators — `arr + 1` works, `arr + typed_var` still requires matching types. Q4 (4B).

**Data types (Arrow commitment).**
- [x] `Column[T]` — bitmap-backed nullable 1D column, Arrow layout. Q5 (5A) + Q6 (6C).
- [x] `Tensor` is dense-only; nullability is a `Column` concern.
- [ ] `DataFrame` — schema-bearing table of named columns.
- [ ] Arrow IPC, Parquet, CSV readers/writers with effect annotations.
- [ ] **`LazyDataFrame` — minimum-viable query optimizer (v66 graduation, 2026-05-11; lifted from v1.5).** `df.lazy()` returns `LazyDataFrame`; expression API (`col("name")`, `col("a") + col("b")`, `col("x").mean()`, `when().then().otherwise()`); operations (`filter`, `select`, `group_by(...).agg(...)`, `join`, `sort`, `limit`); `.collect() -> DataFrame` materializes; `.explain() -> String` prints optimized plan. Optimizer passes at v1: predicate pushdown, projection pushdown, constant folding, CSE. Target ~2-3K LOC. See `deferred.md § Lazy DataFrame Query Planner — Option A v1 Scope`. Full optimizer (join reordering, push-through-joins, etc.) at P2 — see `deferred.md § Lazy DataFrame Query Optimizer Expansion`.
- [x] **Statistical methods on `Column` / `DataFrame` (v66 graduation, 2026-05-11).** `Column[T: Numeric]`: `mean`, `std`, `var`, `median`, `quantile(q)`, `min`, `max`, `sum`. `Column[f64]`: above + `corr(other)`. `DataFrame.describe() -> DataFrame` (count/mean/std/min/25%/50%/75%/max per numeric column). Trait-dispatched the same way as `std.stats` so future `GpuColumn` / `GpuTensor` implements the same surface. See `deferred.md § Statistical Methods on Column / DataFrame`.

**ML and AI-adjacent stdlib (v66 graduation, 2026-05-11).**
- [x] **`std.embeddings` — cosine similarity, top-k, l2-normalize, batched dot (P1).** Surface complete (2026-07-13): scalar + batched + Q×N-matrix cosine similarity, `top_k`, `l2_normalize`, batched dot over `Tensor[f32, ...]` for RAG, semantic-search, recommendation workloads. See `deferred.md § std.embeddings`.
- [ ] **`std.autograd` — reverse-mode automatic differentiation (P1).** `shared struct Tape` with `writes(GradTape)` effect; separate `Var[T, S]` wrapper over `Tensor[T, S]` (locked design — Q8); operator overloads on `Var`; activations (relu, sigmoid, tanh, softmax, gelu, silu); losses (mse, cross_entropy, bce); `grad(fn, args)` / `value_and_grad(fn, args)`; GPU-aware tape recording (records kernel launches via v1 GPU codegen). Reverse-mode only at v1; forward-mode and higher-order grads stay post-v1. See `deferred.md § std.autograd`. `std.nn` (layers) and `std.optim` (optimizers) — decision deferred to engineering-start (Q7); see `deferred.md § Neural Network Framework`.

**Data documentation (v66 graduation, 2026-05-11; lands in Phase 8.5 docs window).**
- [ ] **`docs/book/src/data.md` — dedicated data chapter.** Tensor / Column / DataFrame / lazy querying / Arrow IPC / Parquet / CSV. One end-to-end example (~50 lines). Pointers to `std.linalg`, `std.fft`, `std.einsum`, `std.embeddings`, `std.random.distributions`, `std.autograd`. Discoverability for the "quiet data bonus" positioning — depth that ships at v1 but is not promoted as the headline pitch. See `deferred.md § Data Documentation and Examples`.
- [ ] **`examples/data/` — worked programs.** `csv-to-parquet.kara` (basic ETL), `embeddings-rag.kara` (load corpus → embed via HTTP → top-k semantic search), `stats-summary.kara` (group-by + describe), `lazy-query.kara` (Polars-class analytical query). Double as integration tests against the data stdlib.

### Scripting-critical stdlib (data-science narrow surface)

> **Note (v64 lift, 2026-05-09):** `std.regex`, `std.http` (server + client), `std.websocket` (server + client), and `std.process` were lifted to [Phase 8 § Backend Platform](#backend-platform-v64-lifted) under the backend-first decision. Only `std.stats` (data-science specific) remains in this Phase 11 sub-section. Browser playground's WebSocket shim is now satisfied by the v1 `std.websocket` server which lives in Phase 8.

- [x] `std.stats` — mean, stddev, percentile, median, min/max, argmin/argmax, sort, argsort. Trait-dispatched via `Reduce` / `ElementwiseMap` / `ElementwiseOrd` so future `GpuTensor` implements the same surface.

### Security (`std.secret`)
- [ ] `Secret[T]` — compiler-enforced wrapper that blocks `Debug`/`Display`/`Serialize`/`Deserialize`/`PartialEq`/`Eq`/`PartialOrd`/`Ord`/`Hash`/`Deref`/`Borrow`/`AsRef`/`Copy` impls on itself; `.expose()` / `.expose_mut()` are the only access paths; `.clone()` re-wraps
- [ ] `ConstantTimeEq` trait — constant-time equality replacing `PartialEq` for `Secret[T]`; stdlib impls for `String`, `Vec[u8]`, fixed-size `[u8; N]`, integer primitives
- [ ] `Zeroize` trait (`fn zeroize(mut ref self)`) — stdlib impls for the same set; `Drop` on `Secret[T]` dispatches through it before field destructors
- [ ] Derive codegen: `#[derive(Debug)]` / `#[derive(Display)]` on containing types emits `Secret[T]` fields as `<redacted>`; `#[derive(Serialize)]` on containing types is a compile error with a pointer to `.serialize_expose()` for explicit wire transit
- [x] `undocumented_unsafe` lint — warn (default-on) on `unsafe` blocks without a preceding `// Safety:` comment; same rule for `unsafe fn` via `# Safety` doc-comment section

### Embedded / Hardware Primitives
- [x] `volatile_read[T: Copy]` / `volatile_write[T: Copy]` — unsafe intrinsics for MMIO register access
- [x] `VolatileCell[T: Copy]` — stdlib wrapper for ergonomic register map definitions
- [ ] Inline assembly: `asm` keyword expression inside `unsafe`; operand forms (`in`, `out`, `inout`); options (`nomem`, `nostack`, `pure`, `volatile`)
- [ ] `global_asm` — file-scope raw assembly for vector tables and bootstrap
- [x] `Atomic[T: Copy]` — `shared struct` for ISR-to-main signaling; v1 scope: `new`, `load(ord)`, `store(val, ord)` on `bool`/`u8`/`u16`/`u32`/`u64`/`usize`. Advanced RMW ops (`swap`, `compare_exchange`, `fetch_add`, `fetch_and`, `fetch_or`) and fences land alongside the embedded target work in Phase 10.
- [x] `Ordering` enum: `Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst` — C11/LLVM memory model
- [ ] `#[interrupt(NAME)]` — ISR attribute: interrupt calling convention, vector table placement, implicit `isr` profile restrictions
- [x] `CriticalSectionGuard` — RAII interrupt disable/re-enable; `#[must_use]`
- [x] Linker control: `#[link_section("name")]`, `#[no_mangle]`, `#[used]`
- [x] C calling convention variants: `extern "C"`, `"C-unwind"`, `"interrupt"` (implemented); `"stdcall"`, `"fastcall"`, `"win64"`, `"sysv64"` (reserved)
- [ ] `float_in_serialized_type` lint: warn when `#[derive(Serialize)]` or `#[derive(Deserialize)]` contains an `f32`/`f64` field — JSON has no NaN encoding, format consumers follow IEEE. Suppressible per-field with `#[allow(float_in_serialized_type)]`. (Lands alongside Serialize/Deserialize derives, post-v1.)

### Codegen Optimization (IR quality pass)

> **Real-world priority (measured 2026-06-12, [`selfhost-lexer-profile.md`](spikes/selfhost-lexer-profile.md)).** Profiling the self-hosted lexer — the first real Kāra systems program — on 441 KB of real source found the top two codegen-perf levers are **string-literal `match` dispatch** (46% self-time, lowered to a linear `memcmp` chain) and **allocation on hot String/byte paths** (38%), together accounting for the lexer's **4.6× instruction gap vs the Rust lexer** (token output bit-identical). These are the two items immediately below and are the highest-leverage real-world targets. The SIMD-class items further down (alias metadata, non-temporal stores, vectorization) measured ≈0 on real code and stay deferred — they target a bulk-arithmetic workload the corpus doesn't actually have.

- [x] **String-literal `match` dispatch lowering** — *#1 real-world lever (46% of self-hosted-lexer self-time, 2026-06-12); **shipped 2026-06-12**.* A `match` on string literals (e.g. `keyword_or_ident`'s ~90 `"kw" => Token` arms) lowered to a **sequential cascade of `memcmp`** — every identifier token walked up to ~90 string compares. Now lowered to a **length-bucket + first-byte `switch` tree with residual `memcmp`** (`src/codegen/control_flow_match.rs`, `analyze_string_dispatch` / `emit_string_dispatch`): `switch len → switch first-byte → ≤1–2 memcmp` per token, gated on ≥4 bare string-literal arms over one scrutinee (the cascade is kept for smaller / guarded / `Or`-pattern / mixed matches — conservative; the existing arm-body blocks are reused verbatim so all binding/drop/tail-move machinery is untouched). **Measured on the self-hosted lexer (same 441 KB input, token output still bit-identical): 111.7 B → 66.9 B instructions retired (−40%), Rust gap 4.58× → 2.74×; `memcmp` fell from the #1 self-time leaf (180 samples) to 0.** Tests: `tests/codegen.rs::{test_string_match_lowers_to_dispatch_switch, test_small_string_match_keeps_cascade, e2e_string_match_dispatch_matches_cascade_semantics}`. Follow-ups (own entries below): the `==`-against-literal-chain half, `Or`-pattern string arms, and a perfect-hash escalation if a re-profile shows residual `memcmp` still dominant.
- [ ] **`==`-against-string-literal chain lowering** — follow-up to the shipped `match` dispatch above. An `if x == "fn" || x == "let" || …` chain (and `!=` chains) still lowers through `compile_binop`'s per-comparison `memcmp`, the same linear shape the `match` tree replaced. Detect a chain of `==`/`!=` against string literals over one place-expression and route it through the same length/first-byte switch tree. Rarer on the lexer hot path than `match` (which is why it was split off), but completes the general string-dispatch story.
- [ ] **`Or`-pattern string-literal arms in the dispatch tree** — `analyze_string_dispatch` currently bails (cascade fallback) on `"a" | "b" => …` arms. Extend the analyzer to flatten an `Or` of string literals into multiple `(literal → shared body)` dispatch entries so keyword-group arms also get the switch tree.
- [ ] **Allocation reduction on hot String/byte paths** — *now the #1 real-world lever (was #2 at 38%; promoted after the string-match dispatch lever shipped — `malloc`/`free` is now the top self-time leaf, Rust gap 2.74×).* Two patterns dominate: (a) `substring` returns an **owned `String` copy** where a borrow/slice would do — the [`project_lexer_string_scan_shape`] zero-copy lesson, observed inside the lexer's own classify-only reads; (b) `Lexer.new` rebuilds the whole input into a `bytes: Vec[u8]` byte-by-byte (`for b in src.bytes()`). **Resolved into three concrete, separately-owned moves (2026-06-12):**
  - **SSO (small-string optimization) — the general, corpus-wide, compiler-owned lever. Staged campaign: [`spikes/small-string-optimization.md`](spikes/small-string-optimization.md). Slice 1 LANDED 2026-07-09.** Inline short strings in the `{ptr,len,cap}` struct → no `malloc` for the short-lexeme common case across *every* Kāra program. Central constraint: String shares `vec_struct_type` with `Vec`, so encode SSO via an in-struct tag (don't split the type). **Layout now settled — flag = sign bit of `cap`** (owned-heap ⇔ signed `cap > 0`; inline `cap < 0`; static `cap == 0`), 23-byte folly-style inline overlay. Slice 1 shipped the executable encoding contract (`runtime/src/sso.rs`, unit-tested), the codegen tag helpers (`src/codegen/sso.rs`), and hardened the six `{ptr,len,cap}` buffer-free gates (`UGT`→`SGT`) — all proven no-op (full suite + ASAN + codegen E2E green, zero perf delta). **Slice 2 (inline construction — the actual `malloc`-elimination win) is next**; its concrete checklist is in the spike. **This is the "close the gap / go further" lever.**
  - **Lexer source-slices — closes the self-host *number* fastest, but is selfhost-session-owned source, NOT a compiler fix.** Rewrite the lexer hot paths to classify on borrowed `s[a..b]` slices and clone only when an identifier is actually stored (`selfhost/src/main.kara:1239`, `:1260`, `:696/:703/:720`, the string/char-scan sites). **No compiler blocker: the shipped string-match dispatch tree already works zero-copy on a slice (reads ptr+len).** Filed for the selfhost session; intentionally not edited from a compiler-side worktree (two-sessions-one-file hazard). Complementary to SSO — slices give zero-copy on this one hot path; SSO gives no-malloc corpus-wide.
  - **Bulk-copy / pre-size for `for x in iter { v.push(x) }`** (pattern b) — contained compiler lever, lower `for b in src.bytes() { v.push(b) }` (trivially-copyable elements) to `reserve` + `memcpy`. ~8% on the lexer, general to any collection-building loop, but the smaller fish and a higher-risk loop→memcpy transform. Deferred behind SSO unless wanted as a standalone safe increment.
  - (Related defect: `B-2026-06-12-10`, a suspected per-iteration leak in the same paths — verify under LSan; SSO's Slice 2 re-runs the full leak gate regardless.)
- [ ] Inline hints: emit `alwaysinline` / `noinline` attributes based on call-site analysis
- Alias metadata — lower ownership facts to LLVM alias attributes/metadata. **Measured 2026-06-12: param-level `noalias` alone is inert.** Isolated old-vs-new on a `mut ref Vec` kata (generate-parentheses) plus textbook accumulator microbenches (both inlined and recursive) produced byte-identical or perf-identical binaries at the default `-O2` pipeline. Two structural reasons: (a) inlining exposes the caller's real allocas, so alias analysis proves disjointness *without* the param attribute in the common single-TU case; (b) where it isn't inlined, the payoff is gated on the metadata items below — and AOT overflow-trapping pins accumulator stores to memory (panic edges must observe consistent state), neutralizing the register-promotion `noalias` would otherwise enable. **So the two metadata items are the actual perf levers; the param-attribute items are correct, sound groundwork that only pays off in combination.** Ordered accordingly:
  - [ ] **`tbaa` type-based alias-analysis tags** — *lever.* Without TBAA, LLVM cannot assume two differently-typed pointers (e.g. `mut ref Acc` vs `ref Src`) are disjoint, so the param `noalias` has no partner fact to combine with on cross-type accumulator loops.
  - [ ] **Slice-kernel disjointness via `!alias.scope`/`!noalias` metadata on loads/stores** — *filed, deferred; not the auto-vec enabler (measured 2026-06-12).* Would let LLVM skip the runtime alias-check + scalar-fallback it auto-inserts on vectorized `mut Slice[T]`/`Slice[T]` kernels (by-value `{ptr,len}` fat structs a *parameter* attribute can't reach; metadata annotates the memory ops directly, independent of inlining barriers — design.md § Codegen → scoped-alias). **Correction:** the actual auto-vec enabler turned out to be non-trapping arithmetic (`wrapping_*`, landed — see the "Integer-kernel auto-vectorization" entry below), not this. **Runtime payoff measured ≈ 0:** a Rust oracle comparing indexed (runtime check) vs `zip` (disjointness conveyed → no check) on a memory-bound add kernel ran 184.7 vs 184.4 ms — identical, because the kernel sits at the memory-bandwidth wall (~130 GB/s of ~150–200 GB/s peak) where Kāra already matches Rust 1.00×; removing a *compute* branch can't beat a *memory* limit. **Only concrete win is binary size:** ~132 B per auto-vectorized kernel (the scalar-fallback duplicate + range-check; scales with body size) — but the current corpus has ≈0 auto-vectorized slice kernels, and the densest source (the v66 numerical rows) is slated for the hand-vectorized `Vector[T,N]` spine, which has no fallback to remove ([`deferred.md § Hand-Vectorized Data-Spine Commitment`](deferred.md#hand-vectorized-data-spine-commitment)). So < 1 % on anything we ship today. **Side effect (not free):** extends the optimizer's borrow-check soundness dependency (the `noalias` risk class) to slice disjointness — a borrow-checker hole becomes a silent miscompile; gate behind `mut Slice`/`Slice` exclusivity stress-testing. **Build when:** auto-vec-heavy code that is *not* hand-vectorized appears (size win compounds), or a compute-bound / many-slice kernel surfaces where the runtime check actually bites (O(k²) checks, or LLVM declines to vectorize). For memory-bound kernels the faster lever is non-temporal stores / fusion, not this (both tracked as their own entry below).
    - [ ] **Scaffold a representative alias-scope benchmark *before* building** — the concrete un-defer trigger, not just a vague "build when." Need a kernel that actually exists in shipping Kāra code and is one of: (a) auto-vec-heavy *and not* hand-vectorized (so the ~132 B/kernel size win compounds across many kernels), or (b) compute-bound / many-slice where the auto-inserted runtime alias-check actually bites (O(k²) checks, or LLVM declines to vectorize and drops to scalar). Until such a kernel is in the corpus the runtime payoff stays ≈0 (memory-wall; Kāra already 1.00× Rust) and only the size win applies — so building the metadata first is speculative. The [`selfhost-lexer-profile.md`](spikes/selfhost-lexer-profile.md) spike was the candidate hunt — **ran 2026-06-12, found no such kernel** (real Kāra is string-dispatch/allocation-bound, not auto-vec-heavy). Stays deferred; effort goes to the string-match + allocation levers at the top of this section.
  - [x] `noalias` on `mut ref T` parameters — *groundwork, landed* (commit `397e4d7b`, `emit_param_alias_attrs` in `src/codegen/functions.rs`; covers `mut ref self` receivers). Correct + sound (design.md § Variance invariance pin + § Part 4 RC/mutation exclusivity; shared types carved out). Inert in isolation per the measurement above — necessary building block for when TBAA / memory-op metadata land. (Original "noalias on owned parameters" framing was wrong — owned aggregates pass by value, so the ptr-shaped targets are `mut ref`/`ref`.)
  - [ ] `noalias` in `declare_mono_function` — *groundwork;* extends the landed param-attribute pass to monomorphized generics (the landed slice covers only the non-generic `declare_function` path).
  - [ ] `ref T` → `readonly` + `noalias` — *groundwork + a real type-walk,* gated on a transitive **Freeze predicate** (no `Atomic[_]`/`Mutex[_]` field reachable — those mutate through a shared `ref self`, design.md § Part 5, Kāra's `UnsafeCell` analogue). Emitting `readonly` on a non-Freeze `ref` is a miscompile.
- **Integer-kernel auto-vectorization — the overflow-trap branch is the *proven hard blocker*** (measured 2026-06-12). At the default `-O2` a slice kernel `out[i] = a[i] + b[i]` stays fully scalar: the per-element checked add emits a `b.vs → panic` side-exit (`emit_checked_int_arith`), and a loop with a side-exit cannot vectorize *regardless of aliasing* — no runtime alias-check is even emitted, legality fails first. A pure-copy kernel (`out[i] = a[i]`, no arithmetic) vectorizes to NEON `q`-registers under identical conditions, isolating the trap branch as the cause. Aliasing is only a *soft* blocker: LLVM handles it with an auto-inserted runtime range-check + scalar fallback (the alias-metadata items above merely remove that check).
  - [x] `wrapping_add`/`wrapping_sub`/`wrapping_mul` on the 64-bit widths (i64/u64/usize) — landed 2026-06-12. Lowers to a bare `add`/`sub`/`mul` (no `with.overflow`, no trap branch), so a `wrapping_*` kernel body is straight-line and **auto-vectorizes**: verified `out[i] = a[i].wrapping_add(b[i])` compiles to NEON `add.2d v0, v4, v0` (2×i64, 4× unrolled). This is the empirical unblocker that closes the noalias → trap-branch investigation.
  - [ ] Remaining `wrapping_*` (`div`/`rem`/`shl`/`shr`), narrow widths (i8..i32 / u8..u32 — need two's-complement masking in the interpreter + narrow-binop mirroring in codegen), and i128/u128 (interpreter `Value::Int(i64)` is lossy today).
  - [ ] `checked_*` / `saturating_*` / `overflowing_*` families (design.md:2142 — all four families × every integer type ship in v1).
  - [ ] Idiomatic-`+` auto-vec (post-v1 design call): a block/loop-level wrapping opt-in, or a vectorizer-friendly trap check (vector add + horizontal overflow-reduce + per-chunk scalar re-trap) so ordinary `+` vectorizes while keeping trap-by-default. LLVM does not do the latter automatically — it is a real custom-codegen project.
- [ ] **Non-temporal / streaming stores for write-heavy bulk kernels** — *memory-bandwidth lever, untriaged; the thing that can actually move a memory-bound kernel where the alias-metadata item above measured ≈0.* For kernels that write a large output stream once and never re-read it (memset/memcpy-shaped, bulk transforms over arrays larger than L2), a normal store first pulls the destination cache line in (read-for-ownership) and evicts live data; a non-temporal store (`stnp`/`st1 … ` with the NT hint on arm64, `movnt*` on x86, or LLVM `!nontemporal` metadata on the store) bypasses the cache and writes straight to memory — saving the RFO traffic (≈halving store-side bandwidth) and avoiding cache pollution. This attacks the *bandwidth wall* (the ~130/150–200 GB/s ceiling the slice-add kernel hit) rather than compute, which is why it can win where removing a compute branch can't. **Untriaged — do not build blind:** NT stores only win *above* the cache-resident threshold and can *lose* badly on data that's re-read soon (no cache line to hit), so this must be a gated/heuristic emission, never default. Needs (a) a write-heavy bulk kernel in the corpus and (b) an NT-vs-normal measurement on the M5's memory subsystem before any codegen. Candidate-kernel hunt + decision rule belonged with the [`selfhost-lexer-profile.md`](spikes/selfhost-lexer-profile.md) spike, which **ran 2026-06-12 and found no write-heavy bulk kernel** in real Kāra code — stays untriaged/deferred. Pairs with loop/kernel **fusion** (produce multiple outputs in one pass to amortize the bandwidth) as the two real memory-bound levers — fusion is the bigger and more broadly applicable of the two.
- [ ] Arithmetic flags: `nsw`/`nuw` on integer ops once AOT overflow trapping lands — the no-trap path provably doesn't wrap, making the flags sound. (Kāra overflow is never UB: defined-trap on `app`/`lib`, defined-wrap on `embedded` — see `design.md` § Arithmetic Overflow and the AOT-trapping entry in `implementation_checklist/phase-7-codegen.md`.)
- [ ] LTO: enable link-time optimization in `karac build --release`
- [ ] Static branch hints from effect analysis (`llvm.expect` emission): emit `llvm.expect` intrinsic on branch conditions where effect analysis can predict likelihood. **This is not PGO** — no instrumentation, no profile collection, no recompile loop. Real PGO (instrumented + AutoFDO) is deferred to post-v1; see [`deferred.md § Profile-Guided Optimization Loop`](deferred.md#profile-guided-optimization-loop).

**Goal of this pass:** Reduce the Phase 7 ≤2x gap to ≤10% of equivalent hand-written Rust on compute-bound benchmarks. Ships at the end of v1 because IR-quality polish only pays off once the long-tail stdlib is the last thing being measured.

**Bootstrap staging — why this pass does not need to precede self-hosting (2026-06-10).** Because Phase 11 runs *on* the self-hosted compiler, this pass is written in Kāra and lands *inside* the self-hosted `karac`. A self-hosted compiler built before the pass exists is ~2× slower than the Rust `karac` — but that slowness is confined to the *stage-1* binary and never reaches the shipped artifact:

1. Rust `karac` (no pass) compiles the Kāra compiler source → **stage-1** (slow binary, but it *contains* the pass logic).
2. **stage-1** recompiles the same source → **stage-2** (fast — stage-1 applied the pass while compiling it).
3. Ship **stage-2**; verify the fixpoint with **stage-3** (stage-2 and stage-3 must be byte-identical).

So the only cost is a slow stage-1 *during* Phase 11 development (re-stage periodically to keep iteration fast) — never a shipped-quality regression. This is standard GCC/rustc bootstrap discipline, and it retires the earlier idea of pulling the IR pass forward into the Rust compiler / Phase 8.

### Deferred from v1 (P1, ships post-v1)
- [ ] **v1.5 — Axis-indexed reductions.** `sum[AXIS]()`, `mean[AXIS]()`, `min[AXIS]()`, `max[AXIS]()`, `argmin[AXIS]()`, `argmax[AXIS]()` with fully typed return shapes (`remove_dim(Shape, AXIS)`). Held for v1.5 because shipping with `Tensor[T, [?]]` return types would be a breaking change when shape arithmetic tightens them. See `design.md § Axis-Indexed Reductions`.
- [x] ~~**v1.5 — Lazy evaluation / pipeline fusion.**~~ Lazy `LazyDataFrame` (Option A scope — predicate pushdown + projection pushdown + constant folding + CSE) pulled forward from v1.5 to v1 P1 on 2026-05-11 (v66 graduation). See `deferred.md § Lazy DataFrame Query Planner — Option A v1 Scope` and `brainstorming/archive/v66_general_purpose_with_data_bonus.md § 3.2 and Q1`. Full optimizer expansion (join reordering, push-through-joins, scan-time filters) stays post-v1 as P2 — see `deferred.md § Lazy DataFrame Query Optimizer Expansion`. `LazyColumn` / `LazyTensor` / `Iterator` specializations + kernel-fusion lazy stay v1.5+; this lift is `LazyDataFrame` only.
- [ ] **Phase 11+ (P1) — `std.einsum`.** String-notation Einstein summation. See `deferred.md § std.einsum`.
- [ ] **Phase 11+ (P1) — `std.linalg`.** SVD, eigendecomposition, QR, Cholesky, `lstsq`, norm, inverse, determinant, rank. See `deferred.md § std.linalg`.
- [ ] **Phase 11+ (P1) — `std.fft`.** 1D/N-D FFT, IFFT, `rfft`, `fftfreq`. See `deferred.md § std.fft`.
- [ ] **Phase 11+ (P1) — `std.random` distributions.** Normal, binomial, Poisson, exponential sampling on top of basic uniform. See `deferred.md § std.random`.
- [ ] **Phase 11+ (P1) — `Tensor.where`, boolean indexing, fancy indexing, `meshgrid`.** Conditional element selection, mask-based filtering, index-array access, coordinate grid generation. See `deferred.md` entries.
- [ ] **Phase 11+ (P1) — Tensor element-wise math and `clip`.** `exp`, `log`, `sqrt`, `abs`, `sign`, `floor`, `ceil`, `round`, `sin`/`cos`/`tan` and inverses, `atan2`, `clip(lo, hi)`. See `deferred.md § Tensor Element-Wise Math`.
- [ ] **Phase 11+ (P1) — Tensor construction functions.** `zeros`, `ones`, `full`, `eye`, `diag`, `arange`, `linspace`, `from_fn`. See `deferred.md § Tensor Construction Functions`.
- [ ] **Phase 11+ (P1) — Scan operations.** `cumsum`, `cumprod` (global and axis-indexed). Axis-indexed scans preserve input shape so they do not require v1.5 shape arithmetic. See `deferred.md § Scan Operations`.
- [ ] **Phase 11+ (P1) — Shape-manipulating ops.** `concat`, `stack`, `reshape`, `flatten`, `expand_dims`, `squeeze`. Ship with partially-dynamic return shapes; v1.5 shape arithmetic provides fully-typed versions. See `deferred.md § Shape-Manipulating Operations`.
- [ ] **Phase 11+ (P1) — Set-like operations.** `unique` (with counts and inverse), `searchsorted`. See `deferred.md § Set-Like Operations`.
- [ ] **Phase 11+ (P1) — NaN/Inf handling.** `is_nan`, `is_inf`, `is_finite`, `nansum`, `nanmean`, `nanmin`, `nanmax`, `fill_nan`, `f64.NAN`/`f64.INF` constants. See `deferred.md § NaN and Inf Handling`.
- [ ] **Phase 11+ (P1) — `.npy`/`.npz` file I/O.** `std.io.npy` — load/save single arrays and multi-array archives. See `deferred.md § .npy / .npz Array File I/O`.
- [ ] **Phase 11+ (P1) — `Complex[T]` stdlib struct.** Canonical complex number type shared across `std.fft`, `std.linalg`, and signal-processing libraries. Interleaved memory layout (FFTW/C99 compatible). `Tensor[Complex[f64], Shape]` as the FFT output type. See `deferred.md § Complex[T]`.
- [ ] **Phase 11+ (P1) — `RichDisplay` trait.** MIME-typed display protocol for the Jupyter kernel. Plotting and DataFrame libraries implement this to render charts and tables inline. See `design.md § Rich Output Display Protocol`.
- [ ] **Phase 11+ (P1) — `std.crypto`.** Constant-time cryptographic primitives: ChaCha20-Poly1305 (AEAD), X25519 (key exchange), Ed25519 (signatures), Argon2id (password hashing), BLAKE3 (general hashing). Delegates to a vetted C library via FFI. `reads(EntropySource)` effect on key-generation calls. See `deferred.md § std.crypto`.
- [ ] **Phase 11 (P1) — `CircularBuffer[T]`.** Fixed-capacity ring buffer; O(1) push/pop at both ends; allocation-free after construction. Enables audio DSP, networking packet queues, and embedded sensor pipelines to share a common type. See `deferred.md § CircularBuffer[T]`.
- [x] ~~**v1.5 — `std.http` server + `std.websocket` server.**~~ Lifted to v1 under v64 backend-first (2026-05-09); server-side HTTP and WebSocket land in [Phase 8 § Backend Platform](#backend-platform-v64-lifted) at v1 launch.
- [ ] **Post-Phase-10 — GPU call-site backend.** Revisit once Phase 10 codegen has ground truth. Expected shape: `GpuTensor[T, Shape]` with `.on(gpu)` / `.to_cpu()` boundary ops (CuPy / PyTorch / JAX semantics). Numerical stdlib composes with whatever GPU story lands — trait-dispatched ops keep API open.

**Done when:** The numerical/data-science stack (Tensor, Column, DataFrame, Arrow IPC, Parquet, CSV) is usable. Stats stdlib ships. Security types (`Secret[T]`, `ConstantTimeEq`, `Zeroize`) are enforced. Embedded primitives (`volatile_read`/`write`, inline `asm`, `Atomic`, `#[interrupt]`) compile and run on a target board. Codegen IR optimization closes the Rust performance gap to ≤10% on compute-bound benchmarks. **End of this phase = v1 release.** (Backend platform — `std.http`, TLS, WebSocket, `std.tracing`, HTTP/2, `std.regex`, `std.process`, protobuf — ships earlier in [Phase 8](#phase-8-standard-library--floor) under the v64 lift.)

---

## Phase 12: Self-Hosting

**Goal:** Rewrite the Kāra compiler in Kāra.

**The v1 pivot (2026-06-10 resequence; LLJIT insert 2026-07-09).** Self-hosting is no longer post-v1 tail work — it executes *after* the Phase 8 floor + Phase 9 enforcement (effectively done), Phase 10 targets (mostly done), and the LLJIT-productionization spike (core complete 2026-07 — `run`/`repl`/`test` default to the JIT, `run == build`; the bootstrap loop runs through `karac run`/`test`, so the single-backend guarantee is a prerequisite, see [`spikes/lljit-productionization.md`](spikes/lljit-productionization.md)), and *before* [Phase 11](#phase-11-standard-library--long-tail). Numeric order ≠ execution order; the real order is **8 → 9 → 10 → LLJIT-productionization → 12 → 11**. Everything Phase 11 still has to add lands *on* the self-hosted compiler, but in three different ways (see the Phase 11 banner): its stdlib (`tensor.kara`, `std.stats`, …) is **reused verbatim**; its already-built Rust passes (the Tensor type-system + codegen, plus Phase 9/10) are **ported** against the Rust differential oracle, near-line-for-line; and its **unbuilt** compiler-internal features (f16/bf16 lowering, embedded primitives, the IR pass, residual GPU codegen) are written **once**, in Kāra. **Rationale:** any *new* compiler feature implemented in the Rust `karac` after the pivot would have to be re-implemented in Kāra anyway; pivoting first deletes that double-work. Already-built Rust passes are ported regardless — so the pivot's savings are bounded to the *unbuilt* features, which means: pivot as soon as the Phase 8 floor is done and stop adding new compiler features to Rust.

**Prerequisite is only the Phase 8 floor.** A compiler-in-Kāra consumes `Vec`/`Map`/`Set`, `String` methods, file I/O, `std.json`, `std.error`, pattern matching — all Phase 8 floor, none of Phase 11. Phase 9 is done (semantics frozen). The single remaining gate is **finishing the Phase 8 floor** (tracker: `implementation_checklist/phase-8-stdlib-floor.md`).

**The bar is "production dev platform," not "passes the fixpoint."** Because Phase 11 (and Phase 10's residual GPU work) get built *on* the self-hosted compiler — shape-kinded generics, GPU codegen, the IR pass — the self-hosted `karac` must be pleasant to do real feature development in: usable diagnostics, fast iteration, complete language coverage. Budget this phase to reach that, not a minimal bootstrap.

**Codegen needs LLVM-C FFI bindings.** The self-hosted codegen module calls LLVM through Kāra FFI (`extern "C"` over the LLVM-C API) — the analogue of the Rust compiler's `inkwell`. FFI is Phase 7 (✅), so this is a large chunk of in-phase work but not a new dependency.

- [x] Lexer in Kāra
- [ ] Parser in Kāra
- [ ] Semantic analyzer in Kāra (resolver + typechecker + effect + ownership)
- [ ] Codegen in Kāra (LLVM-C via `extern "C"` FFI)
- [ ] Bootstrap: Kāra compiler compiles itself; **stage-2 = stage-3 byte-identical** (fixpoint)

**Done when:** `karac build src/main.kara` produces a binary that can itself compile Kāra programs, producing identical output to the Rust-based compiler, and the three-stage bootstrap reaches a byte-identical fixpoint. From here, all new compiler work (Phase 11 compiler-internal features, Phase 10 residual GPU codegen) lands in the Kāra compiler — the Rust `karac` is frozen as the bootstrap seed.

---

## Future: Gradual Verification (Feature 6)

**Goal:** Progressively stronger correctness guarantees beyond the type system — from constrained types to full formal verification.

**Level 2 (Refinement Types) and Level 2.5 (Contracts) are committed** — fully designed in design.md. Parsing is complete (Phase 2). Type-checker enforcement and interpreter runtime checks are tracked in Phase 9 (Gradual Verification Enforcement). Level 3-4 require an SMT solver (Z3) and are indefinitely deferred.

```
// Level 2: Refinement types (committed — no SMT solver needed)
type PositiveInt = i64 where self > 0;
type Percentage = f64 where self >= 0.0 and self <= 100.0;

// Level 2.5: Contracts (committed — runtime-checked in debug, stripped in release)
fn binary_search[T: Ord](haystack: ref Vec[T], needle: ref T) -> Option[i64]
    requires haystack.is_sorted()
    ensures(result) match result { Some(i) => haystack[i] == *needle, None => true }
{ ... }

// Level 3-4: Full formal verification (deferred — requires SMT solver, may never be built)
fn transfer(from: Account, to: Account, amount: Money)
    requires from.balance >= amount
    ensures from.balance + to.balance == old(from.balance) + old(to.balance)
{ ... }
```

- [x] **Level 2 — Refinement types (committed).** `type Percentage = f64 where self >= 0.0 and self <= 100.0`. Numeric comparisons + `len()`, boolean combinators, no SMT solver. Runtime checks at construction; compile-time elision when provable. Parsing complete (Phase 2); enforcement in Phase 9.
- [x] **Level 2.5 — Contracts (committed).** `requires`/`ensures` on functions, `invariant` on structs. Runtime-checked in debug builds, stripped in release. Pure expressions only. Parsing complete (Phase 2); enforcement in Phase 9.
- [ ] **Level 3 — Pre/post conditions with SMT.** Z3 integration for formal proof of contracts. Deferred indefinitely.
- [ ] **Level 4 — Full formal verification.** `old()` references for state before/after. Quantifiers. Proofs of complex invariants. Deferred indefinitely.

**Interaction with other features:** D8 (AI-first) noted that "formal specification as primary artifact" becomes more central if Level 3-4 ever ship. Effect annotations are a lightweight form of Level 2 verification — the gradient exists from day one.

**Done when (Level 2):** `type Percentage = f64 where self >= 0.0 and self <= 100.0` compiles, inserts boundary checks at assignment sites, and the constraint appears in `karac query` output.

---

## Future: Comptime (Compile-Time Code Execution)

**Goal:** Zig-inspired compile-time code execution in the Kāra language itself. Enables custom derives, compile-time validation, and code generation without a separate macro language.

**Not currently scheduled** — add when the built-in `#[derive]` trait set feels limiting. No current design decisions prevent this; purely additive.

- [ ] `comptime` keyword: mark functions that execute at compile time
- [ ] Type reflection: compiler provides struct/enum field information to comptime functions
- [ ] Custom derives: user-defined `#[derive(MyTrait)]` via comptime functions
- [ ] Compile-time validation: SQL query checking, regex compilation, config validation
- [ ] `const fn` calls in const-arg / module-binding positions (extends the v1 const-generic surface; see Phase 5.2)

**How it compares to Rust proc macros:**

| | Rust proc macros | Kāra comptime |
|---|---|---|
| Language | Separate Rust code manipulating token streams | Same Kāra language, runs at compile time |
| Separate package needed? | Yes | No — same file, same module |
| Access to type info | Indirect (parse token streams) | Direct (compiler provides type/field info) |
| Error messages | Often cryptic | Normal Kāra errors |
| Compile time impact | Major (#1 cause of slow Rust builds) | Bounded (resource limits on compile-time execution) |

**Done when:** A user can write a custom `#[derive(Validate)]` that generates validation code from struct field types and attributes, using normal Kāra syntax, in the same project.

---

## Future: Editions and Migration Pipeline

**Goal:** post-v1 editions (`2027`, `2030`, …) ship breaking language changes through a **warning-promotion pipeline** rather than as sudden hard breaks. Every edition-gated change graduates from soft signal to hard rejection across three stages: warn-by-default lint → deny-by-default lint → hard error at the next edition boundary.

**Not currently scheduled** — v1 ships under edition `2026` only. The full migration-pipeline tooling is needed when a second edition is ready to ship; building it before then would be infrastructure without a customer.

**The three-stage promotion** (specced in `design.md § Editions > Migration policy`):

1. **Warning stage** — the upcoming behavior change is detected and reported as a warn-by-default lint in the *current* edition. The lint name is permanent (per the lint-namespace stability rule); programs can suppress with `#[allow(<lint_name>)]` per the lint-level attribute machinery. Authors see the warning during normal `karac build` runs and have lead time to migrate before any compilation failure.
2. **Deny stage** — the lint promotes to deny-by-default at a chosen point during the current edition's lifetime, typically one minor compiler release after stage 1. Programs that haven't addressed the lint now require an explicit `#[allow(<lint_name>)]` to compile. The opt-in is a deliberate "I see the warning and choose to defer."
3. **Hard-error stage** — at the next edition boundary, the lint disappears and the underlying behavior change becomes a hard error. Code under the new edition must conform; un-migrated source fails to compile until updated.

**Tooling support:**

- [ ] **`karac explain --edition <NEXT>`** — projects the full migration timeline for the current package: every warn-stage lint that will become deny-stage in the next minor release, and every deny-stage lint that will become a hard error at the next edition boundary.
- [ ] **`karac fix --edition <NEXT>`** — applies mechanical migrations where the warning's help line provides a direct fix (rewriting `expr_2027` to `r#expr_2027` for keyword reservations, inserting per-binding allows for breaking changes the user has chosen to defer, applying type-swap suggestions, etc.). Non-mechanical changes are left for the user.
- [ ] **`[lints]` table in `kara.toml`** — per-lint policy override: `[lints] foo = "deny"` to escalate immediately, `[lints] bar = "allow"` to defer past the deny stage. Project-level policy in the manifest, parallel to the existing per-attribute `#[allow]` / `#[deny]` machinery.

**Concrete examples already in v1 design** (these will be the first migrations the pipeline carries):

- **Private-function effect inference broadening across edition boundaries** (per `design.md § Specification Layers, Reported behavior`) — the canonical motivating case for the pipeline.
- **Future fragment-specifier reservations** (per `design.md § Reserved Fragment-Specifier Identifier Namespace`, item 62) — extending the reservation to additional prefixes (`pat_<NNNN>`, `ty_<NNNN>`, etc.) post-v1 is a warn-stage addition under the current edition, deny in a later release, hard error at the next edition.
- **Future keyword reservations** — when a new edition reserves a token (e.g., `async`), warn-stage lint flags every binding using the name in user code (suppressible via `r#async`); deny-stage forces the explicit `r#` escape; edition-boundary hard error rejects the bare form.

**Why three stages, not a single hard break.** The warning stage gives ecosystem authors lead time to migrate before any user-visible compilation failure. The deny stage forces awareness without forcing immediate migration (the `#[allow]` opt-out is the escape valve for code that genuinely needs to defer). The edition-boundary hard error is the final stop — code under the new edition must conform, but at that point every author has had a deny-stage warning + a deliberate `#[allow]` to suppress it, so the hard error is never a surprise. The pipeline matches Rust's edition-migration discipline (Rust 2021 → 2024 graduates patterns through warn → deny → error across edition boundaries) and is the load-bearing answer to "how do post-v1 breaking changes ship without breaking everyone overnight?"

**Done when:** the migration pipeline tooling (`karac explain --edition <NEXT>`, `karac fix --edition <NEXT>`, `[lints]` manifest table) ships alongside the first post-v1 edition (`2027` or whichever year is next), with the canonical migrations from the v1 design (effect-broadening, fragment-specifier reservations, keyword reservations) carried through the pipeline as the first real test of the discipline.

---

## Future: Language Server (Reactive Query-Based Layer)

**Goal:** rust-analyzer-class reactive tooling for Kāra — broad query surface, subscribe model, incremental re-computation driven by file watchers. **The batch v1 LSP (`kara-lsp` + VS Code at v1, Neovim / JetBrains at v1.x) was pulled forward to Phase 8.5 Track 3 on 2026-05-11** under the v66 graduation; see `## Phase 8.5: V1 Ship Readiness > Track 3: Language Server`. This `Future` section now tracks only the *reactive* layer — the Salsa-style subscribe/notify model that runs on top of the v1 batch LSP.

**Not currently scheduled — post-self-hosting.** Phase 5 ships a narrow batch query surface (`karac query effects|ownership|concurrency`) that answers one question per invocation. The v1 LSP (Phase 8.5 Track 3) speaks LSP protocol over that batch surface — sufficient for editor integration at launch. The reactive model becomes necessary only at scale — large codebases where re-running even a function-local pipeline on every edit is too slow, or IDE integrations where sub-100ms query latency matters.

The Compilation Model principles (function-local analysis, SCC as cache unit, named inter-phase dependencies) are the substrate that makes this layer feasible later. Building it now would double compiler complexity for a prototype that does not yet need it.

- [ ] Broad query surface: type of expression at position, resolved name at position, visible trait impls at position, monomorphizations of a generic function, effect derivation chain for a call site
- [ ] Subscribe protocol: clients register interest in a query and receive notifications when the answer changes due to file edits
- [ ] Incremental re-computation: Salsa-style or rustc-queries-style dependency tracking across pipeline phases, keyed on function-local units and SCCs
- [ ] Language Server Protocol binary: separate `kara-lsp` (or similar) speaking LSP over stdin/stdout; shares analysis code with the `karac` compiler but runs as a long-lived process
- [ ] IDE integrations: VS Code, Neovim, JetBrains — shipped after the LSP binary stabilizes

**Done when:** An editor user can hover over an expression and see its inferred type and effects within 100ms, and the answer updates live as they edit surrounding code without re-running the full pipeline.

---

## Resolved Design Primitives

Design decisions for language primitives. Most are resolved; see design.md for canonical definitions:

| Item | Question | Likely answer |
|---|---|---|
| Integer sizes | Just `i64` or full set (`i8`/`i16`/`i32`/`i64`/`u8`/.../`usize`)? | Full set (systems language needs them). `usize` is FFI-only; idiomatic Kāra uses `i64` for indices/sizes (decided) |
| Default integer type | What type is `42`? | `i64` (decided). Explicit annotation for smaller types |
| Integer overflow | Trap or wrap? | Always trap. `wrapping_add` etc. for explicit wrapping (decided) |
| Float sizes | Just `f64` or `f32` + `f64`? | Both (GPU/games need `f32`) |
| Float NaN semantics | `NaN == NaN`? Total order? | `f32`/`f64` are IEEE (`NaN != NaN`, no `Eq`/`Ord`/`Hash`); `F32`/`F64` are stdlib total-order types (`NaN` sorts last, implement `Eq`/`Ord`/`Hash`) — decided |
| Numeric widening | Implicit conversions? | Guaranteed-lossless only. `i64→f64` blocked (decided) |
| Entry point | `fn main()` or `fn main(args: Vec[String])`? | `fn main()` with `env.args()` via `reads(Env)` |
| Import syntax | `use path.item` or `import path.item`? | `import path.item` (v41: mainstream-syntax tiebreaker; see `docs/design.md § Module System`) |
| File extension | `.kara` | `.kara` (already used in examples) |
| String interpolation | `format!()` macro or `f"hello {name}"` syntax? | `f"..."` — language feature, compiler desugars to concatenation (decided) |
| Operator overloading | Via traits (`Add`, `Eq`, `Ord`)? | Yes (Rust model) |
| Type aliases | `type Name = ExistingType`? | Yes (standard) |
| Numeric literals | Underscores (`1_000_000`)? Hex (`0xFF`)? Binary (`0b1010`)? | Yes to all (standard) |
| Closure syntax | `\|x\| x + 1` or `fn(x) { x + 1 }`? | `\|x\| x + 1` (Rust convention) |
| Range syntax | `0..10`, `0..=10`? | Yes (Rust convention) |
| Variable shadowing | Allowed? | Yes, all scopes (decided) |
| Default values | Zero values or require initialization? | Require explicit initialization (decided) |
| Loop variable capture | Shared reference or fresh binding? | Fresh binding per iteration (decided) |
| Testing | Where do tests live? | `_test.kara` co-located files, `test_` function prefix, `karac test`. No `#[test]` attribute needed (decided) |
| Derive | Manual trait impls or compiler-generated? | `#[derive(Eq, Hash, ...)]` compiler built-in (decided) |
| Optional chaining | Syntax for nested Option access? | `?.` and `??` operators (decided) |
| `?` operator scope | Result only or also Option? | Both Result and Option (decided) |
| `unwrap()` safety | Tracked or untracked? | Produces `panics` effect, tracked through call chain (decided) |
| Debug printing | Separate from `print`? Effect behavior? | `dbg()` builtin — transparent `debugs` effect, stderr, shows file/line/expr/value, returns value, stripped in release builds. `print`/`println` are for program output (`writes(Stdout)`) (decided) |

---

## Phase Dependency Graph

```
Phase 0 (Proof of Value) ← can be done anytime, no compiler dependency

Phase 1 (Lexer)
  │
  ▼
Phase 2 (Parser)
  │
  ▼
Phase 3 (Semantic Analysis)    ← diagnostics built incrementally at each sub-phase
  │
  ▼
Phase 4 (Interpreter)          ← core stdlib types (Option, Result, Vec, String) introduced here
  │
  ▼
Phase 5 (Query API & Tooling)
  │
  ▼
Phase 6 (Concurrency Runtime)  ← concurrency analysis + runtime execution
  │
  ▼
Phase 7.1 (Core Codegen)       ← language-construct codegen (DONE)
  │
  ▼
Phase 7.2 (Compiled Stdlib Types + Layout Codegen)  ← Array[T,N], Vec[T], String codegen + layout SoA
  │
  ▼
Phase 8 (Stdlib — Floor)        ← full method sets, traits, I/O, providers, std.json/time/path/error/mem/bytes/cmp/hash, auto-concurrency codegen
  │
  ├──── Phase 8.5 (V1 Ship Readiness)  ← parallel track; runs alongside Phase 8–11.
  │                                       Track 1: REPL + Browser Playground + Jupyter (Jupyter ships v1.1).
  │                                       Track 2: Build & Dependency Tooling (formerly v1.1; pulled into v1-P1 2026-05-08).
  │                                       Track 3: Discovery — items added as found during demo build.
  │                                       Does not block Phase 9–11; lands before v1 ships at end of Phase 11.
  ▼
Phase 9 (Verification) ✅       ← refinement types, distinct types, contracts (DONE — semantics frozen)
  │
  ▼
Phase 10 (WASM/GPU Targets) ✅  ← mostly done in Rust (WASM P0 + GPU P1 gate); residual GPU codegen later lands on the self-hosted compiler
  │
  ▼
LLJIT productionization ✅      ← run/repl/test default to the JIT; run == build by construction (no interp-vs-codegen divergence).
  │                                CORE COMPLETE 2026-07 (spike: lljit-productionization.md). Inserted before Phase 12 because
  │                                self-hosting's bootstrap loop runs through karac run/test — this hardens that path first.
  │                                ALL acceptance criteria MET 2026-07-09 (macOS arm64 re-verified green; REPL cold-start
  │                                published: ~70 ms first cell, ~60 ms cold-compile) — no residuals.
  ▼
Phase 12 (Self-Hosting)         ← ★ THE v1 PIVOT. Rewrite karac in Kāra; 3-stage bootstrap to a byte-identical fixpoint.
  │                                Prereq = Phase 8 floor + LLJIT productionization. After this the Rust karac is frozen as the bootstrap seed.
  ▼
Phase 11 (Stdlib — Long-Tail)   ← built ON the self-hosted compiler, each item written once in Kāra:
                                   · stdlib-in-Kāra (compiled by self-hosted karac): Tensor/Column/DataFrame, std.stats,
                                     std.embeddings, std.autograd, Secret[T], CircularBuffer
                                   · compiler-internal (built INTO the Kāra compiler): f16/bf16 lowering, shape-kinded generics,
                                     inline asm/volatile/#[interrupt]/Atomic codegen, the IR-quality pass (bootstrap-staged)
                                   END = v1 RELEASE.

Notes:
- Phases are NO LONGER strictly linear. Execution order is 8 → 9 → 10 → LLJIT-productionization → 12 → 11; numeric order ≠ execution order (2026-06-10 resequence; LLJIT insert owner-confirmed 2026-07-09).
- Phase 7 splits into 7.1 (core codegen, done) and 7.2 (compiled stdlib type codegen + layout codegen). 7.2 owns memory layouts and minimum method sets; Phase 8 + Phase 11 own full API surface.
- Stdlib is split across two phases: **Phase 8** owns the floor; **Phase 11** owns the long-tail. **Phase 8 is the *only phase* self-hosting depends on** (plus the non-phase LLJIT-productionization spike, which hardens the `run`/`test` execution path the bootstrap loop uses); **Phase 11 is the *only* phase built on the self-hosted compiler.**
- Self-hosting (Phase 12) is the pivot: it executes after 8/9/10 + LLJIT-productionization and before 11, so every compiler-internal feature is written once, in Kāra. The IR-quality pass recovers the self-hosted compiler's own speed via the 3-stage bootstrap (stage-2 = stage-3 byte-identical), not by porting into Rust.
- Refinement types (Level 2) and contracts (Level 2.5) are committed — parsing complete (Phase 2), enforcement DONE in Phase 9.
- v1 release = end of Phase 11. **Self-hosting (Phase 12) ships IN v1**, sequenced before Phase 11 (was: post-v1).
- Phase 0 has no compiler dependency and can be done anytime.
- ⚠ roadmap.md checkbox state was reconciled against the trackers on 2026-07-20, but stays COARSER than them: an item is `[x]` here only when its whole bucket is done, and many buckets that are interpreter-complete-but-codegen-pending (or done in one backend / one platform) are still shown `[ ]`. Current tracker counts: Phase 8 ≈238/298, Phase 10 ≈88/93, Phase 11 ≈83/102, Phase 12 ≈88/106 done (Phase 9 fully done). For granular / partial state, trust `implementation_checklist/` + git over the [x]/[ ] marks in this file.
```
