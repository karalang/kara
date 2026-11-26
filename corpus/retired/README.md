# Retired corpus entries

Programs moved out of `corpus/fixtures/` by the B-2026-11-25-70 triage (probe wk49g, 2026-11-26). Each expected to run, `karac check` refuses it, and `karac fix` has no machine-applicable edit that makes it check clean with its recorded output. They were extracted from the pre-M3 Rust test suites and are written against rules the language no longer has. They are kept, not deleted, so a later fix or rewrite can start from them; each `meta.toml` has `expect = "skip"`, `retired` (the reason) and a note naming its old path. The runner skips them. They are not rewritten by hand (review-thread ruling 2026-11-26).

The full triage, including the entries that stayed, is `probe-fuzzers/wk49g/TRIAGE.md` in the project's shared folder.

## Moves the spec forbids (34)

Reason: moves out of borrowed/collection/Drop places (core-semantics §3.7, §4.6, §5.4).

- `codegen/closures/e2e_iter_adaptor_destructuring_closure_collect_codegen`: source.kara:7:26: cannot move a non-`Copy` value out of a borrowed place: the items are `ref String`, and collecting them as `String` would move each borrowed part out of the colle
- `codegen/closures/heap_env_binding_copied_runs`: source.kara:2:53: value 'f' moved here, used again here (moved at line 2:42)
- `codegen/closures/heap_env_binding_copy_chain_runs`: source.kara:2:67: value 'f' moved here, used again here (moved at line 2:42)
- `codegen/closures/heap_env_binding_copy_stored_in_struct_with_data_field_runs`: source.kara:3:85: value 'f' moved here, used again here (moved at line 3:42)
- `codegen/closures/heap_env_binding_reassigned_copy_runs`: source.kara:2:87: value 'f' moved here, used again here (moved at line 2:63)
- `codegen/closures/heap_env_binding_stored_in_array_runs`: source.kara:2:81: value 'f' moved here, used again here (moved at line 2:69)
- `codegen/closures/heap_env_binding_stored_in_struct_field_runs`: source.kara:3:62: value 'f' moved here, used again here (moved at line 3:49)
- `codegen/closures/heap_env_binding_stored_in_tuple_runs`: source.kara:2:61: value 'f' moved here, used again here (moved at line 2:43)
- `codegen/closures/heap_env_binding_stored_in_vec_runs`: source.kara:2:97: value 'f' moved here, used again here (moved at line 2:85)
- `codegen/closures/heap_env_binding_stored_then_source_returned_runs`: source.kara:3:75: value 'f' moved here, used again here (moved at line 3:70)
- `codegen/closures/heap_env_copy_passed_to_borrows_only_fn_runs`: source.kara:3:85: value 'f' moved here, used again here (moved at line 3:42)
- `codegen/closures/heap_env_owner_struct_copied_runs`: source.kara:3:66: value 'a' moved here, used again here (moved at line 3:51)
- `codegen/closures/heap_env_owner_struct_copy_chain_runs`: source.kara:3:78: value 'a' moved here, used again here (moved at line 3:52)
- `codegen/closures/heap_env_owner_struct_copy_with_data_field_runs`: source.kara:3:102: value 'a' moved here, used again here (moved at line 3:64)
- `codegen/closures/heap_env_struct_field_reassigned_copy_runs`: source.kara:3:103: value 'g' moved here, used again here (moved at line 3:75)
- `codegen/closures/heap_env_tuple_owner_copy_with_string_field_runs`: source.kara:2:133: value 'a' moved here, used again here (moved at line 2:81)
- `codegen/closures/heap_env_vec_element_reassigned_copy_runs`: source.kara:2:142: value 'g' moved here, used again here (moved at line 2:110)
- `codegen/families/b_2026_10_05_108/e2e_flat_map_with_tuple_param_or_mapped_inner`: source.kara:13:51: the result borrows a temporary value, which this function owns (borrow at 13:51) [E0509]
- `codegen/iter_range/e2e_chars_iterator_bound_to_variable_codegen`: source.kara:22:24: value 'it3' moved here, used again here (moved at line 21:24)
- `codegen/iter_range/e2e_iter_adaptor_collect_enumerate_codegen`: source.kara:22:34: cannot move a non-`Copy` value out of a borrowed place: the items are `(i64, String)` with parts lent by the collection they walk, and `collect` would move each 
- `codegen/iter_range/e2e_iter_adaptor_collect_fresh_temp_source_codegen`: source.kara:9:34: cannot move a non-`Copy` value out of a borrowed place: the items are `(i64, String)` with parts lent by the collection they walk, and `collect` would move each o
- `codegen/iter_range/e2e_iter_adaptor_enumerate_heap_tuple_downstream_copy_codegen`: source.kara:5:33: cannot move a non-`Copy` value out of a borrowed place: the items are `(i64, ref String)`, and collecting them as `(i64, String)` would move each borrowed part ou
- `codegen/map_set/e2e_iter_adaptor_flat_map_collect_codegen`: source.kara:10:26: cannot move a non-`Copy` value out of a borrowed place: the items are `ref String`, and collecting them as `String` would move each borrowed part out of the coll
- `interpreter/iter_range/test_chars_iterator_bound_to_variable_interpreter`: source.kara:16:32: value 'it3' moved here, used again here (moved at line 15:32)
- `memory_sanitizer/closures/asan_b04_2_destructuring_closure_heap_collect_no_leak`: source.kara:10:33: cannot move a non-`Copy` value out of a borrowed place: the items are `ref String`, and collecting them as `String` would move each borrowed part out of the coll
- `memory_sanitizer/iter_range/asan_b04_4_heap_enumerate_conditional_tuple_collect_no_double_free`: source.kara:11:37: cannot move a non-`Copy` value out of a borrowed place: the items are `(i64, String)` with parts lent by the collection they walk, and `collect` would move each 
- `memory_sanitizer/iter_range/asan_chars_bound_iterator_collect_clone_no_leak_no_double_free`: source.kara:9:28: value 'it' moved here, used again here (moved at line 8:28)
- `memory_sanitizer/iter_range/asan_fresh_temp_source_enumerate_collect_no_double_free`: source.kara:11:38: cannot move a non-`Copy` value out of a borrowed place: the items are `(i64, String)` with parts lent by the collection they walk, and `collect` would move each 
- `memory_sanitizer/map_set/asan_b04_2_flat_map_adaptor_outer_heap_no_leak`: source.kara:10:30: cannot move a non-`Copy` value out of a borrowed place: the items are `ref String`, and collecting them as `String` would move each borrowed part out of the coll
- `memory_sanitizer/map_set/asan_b04_2_flat_map_heap_collect_no_leak`: source.kara:10:30: cannot move a non-`Copy` value out of a borrowed place: the items are `ref String`, and collecting them as `String` would move each borrowed part out of the coll
- `memory_sanitizer/map_set/asan_b04_4_heap_enumerate_downstream_map_collect_no_double_free`: source.kara:11:31: cannot move a non-`Copy` value out of a borrowed place: the items are `ref String`, and collecting them as `String` would move each borrowed part out of the coll
- `memory_sanitizer/map_set/asan_b05_1_nonterminal_map_tuple_no_double_free`: source.kara:9:37: cannot move a non-`Copy` value out of a borrowed place: the items are `(i64, ref String)`, and collecting them as `(i64, String)` would move each borrowed part ou
- `memory_sanitizer/map_set/asan_b05_1_retuple_map_no_double_free`: source.kara:9:37: cannot move a non-`Copy` value out of a borrowed place: the items are `(i64, ref String)`, and collecting them as `(i64, String)` would move each borrowed part ou
- `memory_sanitizer/map_set/asan_heap_enumerate_map_collect_no_double_free`: source.kara:10:30: cannot move a non-`Copy` value out of a borrowed place: the items are `ref String`, and collecting them as `String` would move each borrowed part out of the coll

## Library API the docs do not have (13)

Reason: method/API not in the library as documented (iterators.md, collections.md, strings.md, io.md).

- `interpreter/iter_range/test_iter_chunk_by_after_filter_only_groups_kept_items`: source.kara:4:33: no method 'chunk_by' on type 'IterFilter'
- `interpreter/iter_range/test_iter_chunk_by_collects_into_vec_of_vec`: source.kara:4:33: no method 'chunk_by' on type 'VecIter'
- `interpreter/iter_range/test_iter_chunk_by_groups_consecutive_equal_keys`: source.kara:4:14: no method 'chunk_by' on type 'VecIter'
- `interpreter/iter_range/test_iter_chunk_by_key_fn_fires_once_per_item`: source.kara:4:33: no method 'chunk_by' on type 'VecIter'
- `interpreter/iter_range/test_iter_chunk_by_on_empty_yields_no_groups`: source.kara:4:33: no method 'chunk_by' on type 'IterTake'
- `interpreter/iter_range/test_iter_chunk_by_one_group_when_all_keys_equal`: source.kara:5:14: no method 'chunk_by' on type 'VecIter'
- `interpreter/iter_range/test_iter_chunk_by_singleton_groups_when_all_keys_differ`: source.kara:5:14: no method 'chunk_by' on type 'VecIter'
- `interpreter/iter_range/test_iter_chunk_by_state_persists_across_next_calls`: source.kara:4:18: no method 'chunk_by' on type 'VecIter'
- `interpreter/iter_range/test_iter_chunk_by_with_take_short_circuits`: source.kara:4:14: no method 'chunk_by' on type 'VecIter'
- `interpreter/iter_range/test_iter_chunks_after_filter_only_groups_kept`: source.kara:4:33: no method 'chunks' on type 'IterFilter', an iterator: `chunks` is a `Vec` / `Slice` / `Array` method; collect the items into a `Vec` first (`let items: Vec[i64] =
- `interpreter/iter_range/test_iter_cycle_preserves_pre_adaptors`: source.kara:4:14: no method 'cycle' on type 'IterFilter': `cycle` starts each round from a copy of the iterator, and this one holds a closure (`map`, `filter`, ..), which is not `C
- `interpreter/map_set/test_iter_chunk_by_after_map_uses_mapped_item`: source.kara:4:33: no method 'chunk_by' on type 'IterMap'
- `tls_codegen/test_ir_tls_stream_connect_dispatches_to_runtime_ffi`: source.kara:6:29: no associated function 'connect' on type 'TlsStream'

## Extraction artefacts (5)

Reason: single-file extraction of a multi-file package test (no package around it).

- `cli/test_build_project_alias_rewrite_yields_to_a_shadowing_local`: source.kara:1:1: unknown module `doer`: this file belongs to no package, so the only modules it can import are the library's (`std.*`); a program of several files is a package, wit
- `cli/test_build_project_surfaces_unknown_item_e0225`: source.kara:1:1: unknown module `greet`: this file belongs to no package, so the only modules it can import are the library's (`std.*`); a program of several files is a package, wi
- `cli/test_build_project_surfaces_unknown_module_e0224`: source.kara:1:1: unknown module `greeet`: this file belongs to no package, so the only modules it can import are the library's (`std.*`); a program of several files is a package, w
- `cli/test_module_binding_import_defers_to_a_local_of_the_same_name`: source.kara:1:1: unknown module `db`: this file belongs to no package, so the only modules it can import are the library's (`std.*`); a program of several files is a package, with 
- `cli/test_unused_module_binding_import_is_accepted`: source.kara:1:1: unknown module `db`: this file belongs to no package, so the only modules it can import are the library's (`std.*`); a program of several files is a package, with 

## Programs that break a rule check reports with an internal message (5)

Reason: break/continue inside a closure cannot leave it (design.md:2244); NOTE internal 'not lowered yet' message.

- `codegen/control/e2e_with_provider_enclosing_loop_break_pops_frame`: source.kara:10:18: this `break` is not lowered yet
- `interpreter/control/test_break_through_provider_body_compound_assign_target_untouched`: source.kara:10:39: this `break` is not lowered yet
- `interpreter/control/test_break_through_provider_body_let_else_does_not_run_else`: source.kara:10:39: this `break` is not lowered yet
- `interpreter/control/test_break_through_with_provider_body_does_not_corrupt_assign_target`: source.kara:10:39: this `break` is not lowered yet
- `interpreter/control/test_continue_through_with_provider_body_skips_iteration_only`: source.kara:10:39: this `continue` is not lowered yet

## Type errors against the documented rules (4)

Reason: type error against documented rules (views vs owned, parse returns Result, collect target).

- `codegen/families/b_2026_10_03_12/e2e_assert_eq_compares_by_type`: source.kara:19:5: type 'T3' does not implement PartialEq; add #[derive(PartialEq)] to use == or !=
- `codegen/families/b_2026_10_03_23/e2e_narrow_tuple_literal_into_container_keeps_declared_layout`: source.kara:40:46: expected '(f64, i32)', found '(i64, i64)'
- `interpreter/families/b_2026_10_02_71/test_closure_capture_skips_item_scope_semantics`: source.kara:32:20: cannot infer type parameter 'C', this call's result: nothing in the body fixes it (design.md § Type inference); bind the result with a type annotation
- `mir_iterators/a_map_adaptor_in_kara_type_checks_up_to_its_stored_closure`: source.kara:6:30: parameter `f` is a non-escaping function value and cannot be stored in a struct field: a function-typed parameter may only be called or passed to another non-esca
