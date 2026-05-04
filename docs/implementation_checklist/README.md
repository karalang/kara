# Implementation Checklist

Items to validate, benchmark, or revisit during specific implementation phases. These are not design decisions — they are implementation concerns that should not be forgotten.

Sourced from open gaps identified during design review that don't require design decisions but do require action during implementation.

---

## Work in Progress (updated 2026-05-04)

**Theme: HashMap/HashSet completion.** Finish `Map[K, V]` and `Set[T]` codegen so real test programs and benchmarks run on compiled binaries. Work splits into a serial **List 1** (active session, one agent) and a parallel-safe **List 2** (any agent can pick up without coordination — file/function boundaries don't conflict with List 1).

*Scoping context (audit 2026-05-04): `compile_map_method` (`src/codegen.rs:4667`) originally handled 6 of 11 typechecker-blessed methods and fell through to a silent-`0` catch-all for the rest. `compile_index` (line 5009) handled only Array/Vec/Slice. No `karac_set_*` runtime; `Set[T]` interpreter-only. Existing `runtime/src/map.rs` supports `val_size = 0` correctly (line 71's `(key_size + val_size).max(1)`), so Set lowers to `Map[T, ()]` with no new C code. Map gap closure (subtasks 1–7) closed by 2026-05-04 across commits `4a3bc3e`, `ca94b9f`, `3f08a39`, `8806883`, `b150d8c`, `1678d0a`; subtask 6 (Display) split into its own canonical bullet because recursive Display codegen is its own scope. Map E2E codegen tests grew 12 → 27.*

### List 1 — Active (serial, this session)

- [~] **Display for collections (recursive codegen).** _(canonical: [phase-7-codegen.md](phase-7-codegen.md#phase-72-compiled-stdlib-types--layout-codegen), search `Display for collections (recursive codegen)`)_
  - [x] **1. Per-type Display function emission machinery** — `emit_display_fn_for_type` cached by type, parallel to `emit_hash_fn_for_type` (commit `8123a8e`)
  - [x] **2. Primitive Display fns** — i8…i64 / u8…u64 / f32/f64 / bool / char / String (commit `8123a8e`)
  - [x] **3. `Vec[T]` Display fn** — `[` + loop with recursive elem call + `]`
  - [ ] **4. `Map[K, V]` Display fn** — `{` + iterator loop with recursive K, V calls + `}`
  - [ ] **5. `Set[T]` Display fn** — depends on Set codegen landing; format aligned with interpreter
  - [ ] **6. Tuple Display fn** — `(` + recursive per-field calls + `)`
  - [ ] **7. `compile_print` integration** — recognize Vec/Map/Set/Tuple types, dispatch to emitted Display fn
  - [ ] **8. Test coverage** — E2E covering primitives + nested collections + interpreter-codegen parity

- [ ] **`Set[T]` LLVM codegen.** _(canonical: [phase-8-stdlib-floor.md](phase-8-stdlib-floor.md), search `Set[T] LLVM codegen`)_
  - [ ] **1. Codegen state** — `set_elem_types` side-table + `extract_set_elem_type` helper
  - [ ] **2. `Set.new()` path-call dispatch** — `karac_map_new(elem_size, 0, ...)` (val_size=0)
  - [ ] **3. `compile_set_method`** — `len` / `is_empty` / `insert` / `contains` / `remove` / `clear`
  - [ ] **4. `for x in s`** — `compile_for_set_var` mirrors `compile_for_map_var`
  - [ ] **5. `union` / `intersection` / `difference`** — via iteration, requires `T: Clone`
  - [ ] **6. E2E codegen tests** — 12 cases mirroring the Map suite
  - [ ] **7. ASAN test** — `Set.new + insert + scope-exit free`
  - [ ] **8. `Set[v1, v2, ...]` prefix-literal element type inference** — typechecker only; closes `phase-4-interpreter.md` line 12

- [ ] **`Map.entry(k)` + `Entry[K, V]` enum — in-place insert-or-modify.** _(canonical: [phase-8-stdlib-floor.md](phase-8-stdlib-floor.md), search `Map.entry(k)`)_

  Queued — touches parser/AST/typechecker/interp/codegen/runtime. Round-scoped subtasks added when round opens.

### List 2 — Parallel-safe (pick up without coordination)

These bullets touch files / functions that don't conflict with the active List-1 work. Any agent can pick them up in parallel; merge-conflict risk is minimal.

- [x] ~~**Effect-checker wiring for `Map[K, V]` and `Set[T]` methods.**~~ ✓ DONE (2026-05-04) — `src/effectchecker.rs` seed list and `STDLIB_METHOD_MAP` extended with the full Map/Set allocator surface (`with_capacity`, `try_insert`, `entry`, `clone`, `from_iter`, `extend`, `merge`, `keys`, `values`, `entries` for Map; `with_capacity`, `clone`, `from_iter`, `union`, `intersection`, `difference` for Set; missing `Set.insert` dispatch entry also fixed). `Heap` and `panics` already wired through preludes. 11 new tests in `tests/effectchecker.rs`; 208/208 effectchecker tests pass. Index-op `panics` already covered by the existing `__builtin_index` path. _(canonical: [phase-8-stdlib-floor.md](phase-8-stdlib-floor.md), search `Effect-checker wiring`)_

- [ ] **Hash codegen for compound key types.** _(canonical: [phase-7-codegen.md](phase-7-codegen.md#phase-72-compiled-stdlib-types--layout-codegen), search `Hash codegen for compound key types`)_

  **Files:** `src/codegen.rs` — extends `emit_hash_fn_for_type` (`src/codegen.rs:4282`) and `emit_eq_fn_for_type`. Distinct functions from Display's `emit_display_fn_for_type` and from List-1's `compile_print` integration; no textual collision.
  **Estimate:** ~3–4 commits (tuples → enums → user `#[derive(Hash)]`).
  **Scope:** 5 subtasks already scoped in canonical.

- [x] ~~**For-loop bindings don't propagate Vec/String/Slice element type for method dispatch.**~~ ✓ DONE (2026-05-04) — `src/codegen.rs` gains `var_elem_type_exprs` / `map_key_type_exprs` side-tables (carrying the element/value `TypeExpr` per collection variable), populated at param + let-stmt sites for `Vec[T]` / `Slice[T]` / `Map[K, V]`. New `register_var_from_type_expr` helper drives the side-tables off a `TypeExpr`, and `register_for_loop_bindings` is called from `compile_for_vec_var` / `compile_for_slice_var` / `compile_for_map_var` after `bind_pattern` so each per-iteration binding inherits the right Vec/String/Slice/Map registrations. 4 new codegen E2E tests (`for s in v: Vec[String]`, `for inner in v: Vec[Vec[i64]]`, `for (k, _v) in m: Map[String, i64]`, `for elem in s: Slice[String]`) + 1 interpreter parity test. _(canonical: [phase-7-codegen.md](phase-7-codegen.md#phase-72-compiled-stdlib-types--layout-codegen), search `For-loop bindings don't propagate`)_

---

## Contents

- [Phase 1: Lexer](phase-1-lexer.md)
- [Phase 2: Parser & AST](phase-2-parser-ast.md)
- [Phase 3: Effect Checker](phase-3-effect-checker.md)
- [Phase 4: Tree-Walk Interpreter](phase-4-interpreter.md)
- [Phase 5: Structured Diagnostics and Language Refinements](phase-5-diagnostics.md)
- [Phase 6: Auto-Concurrency Runtime](phase-6-runtime.md)
- [Phase 7: LLVM Code Generation](phase-7-codegen.md)
  - [Phase 7.2: Compiled Stdlib Types + Layout Codegen](phase-7-codegen.md#phase-72-compiled-stdlib-types--layout-codegen)
- [Phase 8: Standard Library — Floor](phase-8-stdlib-floor.md)
- [Phase 9: Gradual Verification Enforcement](phase-9-verification.md)
- [Phase 10: Additional Targets](phase-10-targets.md)
- [Phase 11: Standard Library — Long-Tail](phase-11-stdlib-longtail.md)
