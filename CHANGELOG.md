# Changelog

All notable changes to the Kāra language and compiler will be documented in this file.

> **Maintenance note (2026-10-28).** This file is a coarse milestone record.
> The 0.9.0 entry below is a draft until the release. Progress is in
> [`docs/roadmap.md`](docs/roadmap.md), the bug ledger
> [`docs/bugs-v2.jsonl`](docs/bugs-v2.jsonl) (rollup in
> [`docs/bugs-v2.md`](docs/bugs-v2.md); the pre-M3 ledger is archived in
> [`docs/legacy/`](docs/legacy/)) and `git log`. The 2026-08-15 entry below
> describes the legacy compiler, deleted at M3 on 2026-10-24: its tree-walk
> interpreter, layout blocks, contracts, GPU and self-hosting claims do not
> describe v1.

---

## 0.9.0 (beta) — draft

The first release, and a beta: 1.0 follows outside feedback (decided 2026-10-31). It is the first release of the v2 compiler. Every program lowers to MIR, which the
interpreter (`karac __mir-run`) and the LLVM backend (`karac build`, `karac
run`) both execute; the legacy pipeline was deleted at M3 (2026-10-24).
Normative rules are in [docs/core-semantics.md](docs/core-semantics.md), the
rest of the language in [docs/design.md](docs/design.md), and what waits for
later in [docs/deferred.md](docs/deferred.md).

### Language

- **Ownership without lifetimes.** Parameters borrow by default; `own T`
  takes ownership and `mut ref T` writes. A returned view's source is inferred
  from the signature. `shared struct` / `shared enum` are reference-counted.
- **Effects.** Declared on public functions, inferred for private ones, and
  checked against `par { }`, `par for` and `TaskGroup`. Independent
  statements and loops are parallelised automatically where the effects prove
  it safe (`KARAC_AUTO_PAR=0` opts out).
- **Effect variables** (`[with E]`) for stored callbacks: a struct or enum
  declares one, solved from what is bound to it within a function.
- **`dyn Trait`**: a two-word, never-null owned handle with no `Box`, coerced
  only where a `dyn` type is expected; effects through its `with` clause;
  `dyn T + CrossTask` to cross tasks. No upcasts or downcasts yet.
- **Providers.** `effect resource R: Trait`, `with_provider`, `providers { }
  in { }` and `#[with_provider]` on tests swap a resource's implementation with
  no mocking framework. A missing provider is a compile error. The file system,
  network (HTTP client and TCP), standard streams, clock, environment and
  random source can all be overridden; a program may declare its own
  program-rooted resources with defaults.
- **Concurrency.** Stackless coroutines on a multi-threaded work-stealing
  scheduler; `TaskGroup` with panic isolation, deadlines, channel `select` and cooperative
  cancellation; blocking-style I/O with no `async`/`await`.

### Standard library

- HTTP/1.1, TLS and WebSocket servers and an HTTPS client; files, processes
  and environment; `String`, `Vec`, `Map`, `Set` and their sorted forms;
  iterators; JSON via `Serialize`.

### Tooling

- `karac check --output=json` with machine-applicable fixes, applied by
  `karac fix` (also over a whole package); `karac query` for effects,
  ownership, concurrency, costs and monomorphisation; `karac explain`.
- `karac fmt` (one canonical format, `--check`, package mode), `karac test`
  (colocated `_test.kara` files), `karac new` (`--backend`, `--cli`, `--lib`)
  and `karac init`, `karac lint --list`.
- A VS Code extension with the language server bundled, one `.vsix` per
  platform.

### Targets

- Linux x86_64, Linux aarch64 and Apple-silicon macOS. Windows follows in a
  later release. Plain `wasm32` (`--target=wasm_wasi`) for compute programs.

### Measured before release

- The corpus of runnable programs with expected outputs is the gate, on the
  interpreter, the compiled backend and an AddressSanitizer lane.
- Run time and build time against the earlier compiler, binary size, an
  automatic-parallelism speedup floor, and a load test of 100,000 idle
  connections with tail-latency limits.

### Known limits in 0.9

- A callback stored in a field cannot use an overridden provider
  (core-semantics §14.5); test it by calling it directly.
- Under a `Network` override, TLS, WebSocket, UDP and `Server.serve` are
  refused at check (§14.12).
- An HTTP response with no headers is sent with a JSON `content-type`, and
  standard output to a pipe is block-buffered.

---

## [Unreleased] — state as of 2026-08-15

The compiler is far past the parser milestone recorded below. Working today:

- **Full pipeline:** lexer → parser → resolver → typechecker → effect checker →
  ownership checker, with three execution backends — a tree-walk interpreter,
  an LLVM 18 AOT backend (`karac build`), and an LLJIT engine (the `karac run`
  default). `run` == `build` output parity is a tested invariant, as is a third
  surface: the default build auto-parallelizes via effect analysis
  (`KARAC_AUTO_PAR=0` opts out).
- **Language:** effects (declared on public functions, inferred for private),
  tiered ownership (owned → `ref` → RC, `shared struct`/`enum`, RC elision),
  generics `[T]` with trait bounds, pattern matching with exhaustiveness,
  layout blocks (SoA), contracts, refinement/distinct types.
- **Targets:** native (macOS/Linux/Windows CI-tested), `wasm_wasi` (component
  model emission) and `wasm_browser` including a threaded dual artifact, and
  GPU compute — `#[gpu]` kernels compiled to WGSL and dispatched through wgpu
  (`gpu.dispatch`), proven on Metal and Vulkan. The CUDA/NVPTX path is not
  built.
- **Runtime:** static-linked Rust runtime with a tokio-backed event loop
  (TCP/TLS/HTTP/WebSocket), a native work-stealing scheduler, and opt-in
  gpu/regex/arrow archive variants.
- **AI-first tooling:** structured JSON diagnostics (`karac check
  --output=json`), machine-applicable fixes (`karac fix`), and the Mend
  authoring loop that dogfoods both.
- **Self-hosting (in progress):** ~20k lines of Kāra in `selfhost/` (lexer,
  parser, resolver, typechecker, codegen underway), differentially tested
  against the Rust implementation as oracle in CI.
- **Verification:** ~8,900 non-codegen tests, ~3,200 codegen E2E tests, and an
  ASAN/LeakSanitizer fixture suite (~1,100 programs) run at two optimization
  levels across a 26-job CI matrix (x86 + arm64, three OSes).

---

## 2026-06 — language redesign + parser milestone (historical)

### Language Design

- **Complete language redesign.** Replaced the original `fn`/`flow`/`record`/`->` pipeline design with a Rust-inspired systems language featuring:
  - **Effect system** with six built-in verbs (`reads`, `writes`, `sends`, `receives`, `allocates`, `panics`) and user-defined resources
  - **Auto-concurrency** via effect analysis — no async/await, no colored functions
  - **Tiered ownership** — parameter mode inference, owned returns by default, explicit `ref` for borrows, RC fallback with budget controls. No lifetime annotations.
  - **Data layout separation** — logical struct vs physical memory layout (opt-in SoA)
  - **Algebraic data types** — Rust-style enums with exhaustive pattern matching
  - **AI-first compiler interface** — structured JSON diagnostics, compiler query API, canonical formatting
  - **Phased runtime** — v1 blocking I/O, v1.1 network event loop, v2 full hybrid

### Compiler

- **Lexer:** Complete tokenizer for all keywords, symbols, and literals
- **Parser:** Recursive-descent parser with Pratt expression parsing, producing a full AST
  - All expressions: literals, binary/unary operators, function/method calls, field/index access, closures, ranges, casts, `?` operator
  - All statements: `let`/`let mut` bindings with patterns, assignments, parallel/destructuring assignment (`a, b = b, a` — every RHS evaluated before any target is written, so it swaps), expression statements
  - All items: functions, structs, enums, traits, impl blocks, effect declarations, layouts, modules, imports, constants, type aliases, extern functions, alias/independent declarations
  - Effects syntax: resource declarations, effect groups, `with`/`with _`, transparent effects, parameterized resources
  - Ownership types: `ref`, `mut ref`, `weak`, pointer types
  - Pattern matching: wildcards, bindings, literals, struct/tuple destructuring, qualified paths
  - Generics with trait bounds
  - Attributes with arguments
  - Error recovery: continues parsing after errors, reports multiple diagnostics with spans
  - 89 parser tests + 27 lexer tests = 116 total tests
- **AST:** Complete node types with span tracking on every node

### Documentation

- **design.md:** Complete language specification with all committed features
- **syntax.md:** Parser implementer's grammar reference
- **roadmap.md:** 10-phase implementation plan aligned with current design

### Next Steps

- Implement semantic analysis (Phase 3): name resolution, type checking, effect inference, ownership analysis
