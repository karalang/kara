# Changelog

All notable changes to the Kāra language and compiler will be documented in this file.

> **Maintenance note (2026-08-15).** This file is a coarse milestone record, not
> the project's living history. Day-to-day progress is tracked in
> [`docs/roadmap.md`](docs/roadmap.md) (phase status),
> [`docs/implementation_checklist/`](docs/implementation_checklist/) (per-phase
> slice trackers), and [`docs/bug-ledger.md`](docs/bug-ledger.md) (every bug
> found and fixed, 1,200+ rows) — those, plus `git log`, are authoritative. The
> entry below summarizes the state as of 2026-08-15; the section after it is
> the original 2026-06 entry, kept as the record of the design milestone.

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
