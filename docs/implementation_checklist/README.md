# Implementation Checklist

Items to validate, benchmark, or revisit during specific implementation phases. These are not design decisions — they are implementation concerns that should not be forgotten.

Sourced from open gaps identified during design review that don't require design decisions but do require action during implementation.

---

## Work in Progress (updated 2026-05-04)

**Theme: HashMap/HashSet completion.** Finish `Map[K, V]` and `Set[T]` codegen so real test programs and benchmarks run on compiled binaries. Active work below is the serial **List 1** (this session, one agent). Parallel-safe work has its own tracker — [`wip-list2.md`](wip-list2.md).

*Scoping context (audit 2026-05-04): `compile_map_method` (`src/codegen.rs:4667`) originally handled 6 of 11 typechecker-blessed methods and fell through to a silent-`0` catch-all for the rest. `compile_index` (line 5009) handled only Array/Vec/Slice. No `karac_set_*` runtime; `Set[T]` interpreter-only. Existing `runtime/src/map.rs` supports `val_size = 0` correctly (line 71's `(key_size + val_size).max(1)`), so Set lowers to `Map[T, ()]` with no new C code. Map gap closure (subtasks 1–7) closed by 2026-05-04 across commits `4a3bc3e`, `ca94b9f`, `3f08a39`, `8806883`, `b150d8c`, `1678d0a`; subtask 6 (Display) split into its own canonical bullet because recursive Display codegen is its own scope. Map E2E codegen tests grew 12 → 27.*

### List 1 — Active (serial, this session)

- [x] **Display for collections (recursive codegen).** _(canonical: [phase-7-codegen.md](phase-7-codegen.md#phase-72-compiled-stdlib-types--layout-codegen), search `Display for collections (recursive codegen)`)_
  - [x] **1. Per-type Display function emission machinery** — `emit_display_fn_for_type` cached by type, parallel to `emit_hash_fn_for_type` (commit `8123a8e`)
  - [x] **2. Primitive Display fns** — i8…i64 / u8…u64 / f32/f64 / bool / char / String (commit `8123a8e`)
  - [x] **3. `Vec[T]` Display fn** — `[` + loop with recursive elem call + `]`
  - [x] **4. `Map[K, V]` Display fn** — `{` + iterator loop with recursive K, V calls + `}` (typed entry `emit_map_display_fn` since two type params don't recover from a flat name)
  - [x] **5. `Set[T]` Display fn** — typed entry `emit_set_display_fn(elem_te)` mirrors `emit_map_display_fn` minus the value-side Display (Set lowers to `Map[T, ()]`; iterator's value out-slot is sized 0 and discarded). Format `Set{a, b, c}` matches the interpreter at `src/interpreter.rs:292`. Wired through `emit_display_fn_for_type_expr` (`Path("Set")` branch) and `compile_print` (Set identifier dispatch via `set_elem_type_exprs`). Multi-entry tests deferred — runtime iteration order is unspecified.
  - [x] **6. Tuple Display fn** — `(` + recursive per-field calls + `)`. Typed entry `emit_tuple_display_fn(elems)`; per-field recursion goes through the new `emit_display_fn_for_type_expr` dispatcher so nested compound shapes (`Vec[(i64, String)]`, `Map[String, Vec[i64]]`) compose.
  - [x] **7. `compile_print` integration** — `compile_print` recognizes `ExprKind::Identifier` args whose type is Vec, Map, or Set (via `vec_elem_types`+`var_elem_type_exprs` / `map_key_type_exprs`+`var_elem_type_exprs` / `set_elem_type_exprs` side-tables) and dispatches to `emit_vec_display_fn_te` / `emit_map_display_fn` / `emit_set_display_fn`. Non-collection cases keep the existing `is_struct_value` / `is_pointer_value` / numeric fallbacks. Refactor introduced `emit_display_fn_for_type_expr` as the canonical TypeExpr-aware dispatcher; the by-name `emit_display_fn_for_type` Vec_/Map_/Set_/tuple_ arms now panic-redirect to the TypeExpr API; dead `resolve_ty_for_display_name` removed.
  - [x] **8. Test coverage** — 13 codegen E2E tests in `tests/codegen.rs` (search `test_e2e_display_`): Vec — `vec_i64`, `vec_empty`, `vec_string`, `vec_nested` (`Vec[Vec[i64]]`); Map — `map_string_i64_singleton`, `map_i64_i64_singleton`, `map_empty`, `map_with_vec_value_singleton` (`Map[String, Vec[i64]]`); Tuple — `vec_tuple_i64_i64` (Vec of tuple via `Map.entries()`), `vec_tuple_i64_string` (heap-bearing tuple field); Set — `set_i64_singleton`, `set_string_singleton`, `set_empty`. Multi-entry Map / Set output is order-dependent (runtime walks bucket array) so multi-entry tests use single entries.

- [~] **`Set[T]` LLVM codegen.** _(canonical: [phase-8-stdlib-floor.md](phase-8-stdlib-floor.md), search `Set[T] LLVM codegen`)_
  - [x] **1. Codegen state** — `set_elem_types` / `set_elem_type_names` / `set_elem_type_exprs` side-tables in `Codegen`. New `extract_set_elem_type`, `extract_set_elem_name`, `set_inner_type_expr` helpers paralleling the Map versions. Side-tables populated at `let`-statement type-annotation registration and via `register_var_from_type_expr`.
  - [x] **2. `Set.new()` path-call dispatch** — `is_set_new_call` predicate + `compile_set_new_stmt` mirror the Map path. Emits `karac_map_new(elem_size, 0, hash_fn, eq_fn)` — `val_size = 0` produces a key-only table, the runtime's `(key_size + val_size).max(1)` keeps the bucket allocation valid. Hash/eq fns via `emit_hash_fn_for_type_expr` so compound element types (tuples, derived-Hash structs) compose correctly. Cleanup tracked under the existing `FreeMapHandle` action.
  - [x] **3. `compile_set_method`** — `len` / `is_empty` / `insert` / `contains` / `remove` / `clear` implemented as a parallel match to `compile_map_method`. `insert` returns `!existed` (`true` on fresh insert, matches Rust `HashSet::insert`); `remove` returns the existed bit. Dummy unit/out slot is a single `i8` alloca per fn — `val_size = 0` makes the runtime store a no-op. Dispatch wired in the method-call routing site alongside Map's.
  - [x] **4. `for x in s`** — `compile_for_set_var` mirrors `compile_for_map_var` with the val out-slot replaced by a single shared `i8` alloca and the body binding the element directly (no `(k, v)` destructuring). Dispatch added in the for-loop routing site.
  - [ ] **5. `union` / `intersection` / `difference`** — DEFERRED. Requires per-type clone fn infrastructure for non-Copy elements (String, etc.), which doesn't exist yet. Not on the critical path for unblocking Display subtask 5; landing as a follow-up.
  - [x] **6. E2E codegen tests** — 9 codegen E2E tests in `tests/codegen.rs` (search `test_e2e_set_`): `i64_insert_contains`, `i64_insert_returns_bool`, `i64_remove`, `i64_len_is_empty`, `i64_for_loop_sum`, `string_insert_contains`, `string_remove`, `string_for_loop_count`, `clear`. Set-op tests (`union` / `intersection` / `difference`) gated on subtask 5 landing. Compound-key coverage (`Set[(i32, i32)]`, `Set[(String, (i32, i32))]`) deferred — Hash machinery is shared with the Map test suite which already exercises both compound-key recursion and cache reuse.
  - [x] **7. ASAN test** — `tests/memory_sanitizer.rs::asan_set_new_insert_scope_exit_free` and `asan_set_string_scope_exit_free`. The latter exercises Set[String] with a static-buffer literal (cap = 0) to verify the bucket-array free fires while the static String buffer is left alone.
  - [x] **8. `Set[v1, v2, ...]` prefix-literal element type inference** — typechecker pass added in `infer_expr` for `ExprKind::PrefixCollectionLiteral { type_name: "Set", items }`. Element type unified from items via `check_assignable`; empty `Set[]` falls to `Type::Error` for the element and recovers via the binding-site annotation. 4 typechecker tests: `test_set_prefix_literal_infers_i64`, `test_set_prefix_literal_infers_string`, `test_set_prefix_literal_mismatched_elements_rejected`, `test_set_prefix_literal_empty_with_annotation`.

- [ ] **`Map.entry(k)` + `Entry[K, V]` enum — in-place insert-or-modify.** _(canonical: [phase-8-stdlib-floor.md](phase-8-stdlib-floor.md), search `Map.entry(k)`)_

  Queued — touches parser/AST/typechecker/interp/codegen/runtime. Round-scoped subtasks added when round opens.

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
