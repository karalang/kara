# Changelog

All notable changes to the Kāra language and compiler will be documented in this file.

---

## [Unreleased]

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
