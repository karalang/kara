# design.md § 11 Ownership and sharing: sentence-to-cell map

Status: covered, partial, FAILS (filed, the row named), not testable, or "→ drop schedule" (a borrow-checker or destruction sentence that the drop schedule thread is writing cells for; review's split of 2026-11-24). Cell names are in `conformance/design/ownership/`; `corpus/core/` names the core pins. Line numbers are docs/design.md at the audit (2026-11-24).

Run with `python3 conformance/cases.py conformance/design/ownership` and, for the package cells (directories), `python3 conformance/packages.py conformance/design/ownership`.

**Totals (2026-11-24):** 165 normative sentences: 153 covered, 5 partial and 7 not testable. The one that failed, the defining package named in the `deprecated` warning, is fixed (B-2026-11-24-52). Three covered sentences failed when the audit started and are fixed with it: `ref_eq` on frozen handles (B-2026-11-24-50), the constructor named in the `#[non_exhaustive]` literal refusal (B-2026-11-24-51), and the wording of two `freeze` refusals (B-2026-11-24-53). A dependency's own lint warnings printed in a dependent's check is fixed too (B-2026-11-24-54, pinned in tests/dependency_warnings.rs). The 2026-11-22 pass's fixes are pinned in tests/design_11_conformance_2026_11_22.rs; its cells were not kept. The 23 borrow-check and drop sentences are covered by the drop schedule's cells (f9557a6bfe), run on interp, build and ASan.

## Introduction and the core rules in brief

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4812 | core-semantics.md is normative for moves, borrows, views, sharing and destruction | not testable | (a cross-reference) |
| 4816 | Using a moved place is E0500 | covered | use_of_a_moved_place_is_e0500 (new, drop schedule, interp/build/ASan); also plain_struct_moved_then_used_refused (the struct form); corpus/core |
| 4816 | Duplication is always written `.clone()`; nothing is copied, counted or shared implicitly | covered | duplication_is_written_clone (new, drop schedule, interp/build/ASan); also corpus/core |
| 4817 | A bare parameter borrows (a `Copy` type is copied); `own T` is owned; `frozen T` is a read-only shared handle | covered | shared_handle_passed_to_own_stays_usable, frozen_graph_is_read_by_two_par_branches; §7 parameter modes in conformance/design/functions |
| 4818 | Nothing moves out of a borrowed or shared place (borrowed parameters, `ref` places, `shared` / `sync` / `frozen` values, index projections, fields of a `Drop` type) | covered | no_move_out_of_a_borrowed_parameter, no_move_out_of_a_ref_place, no_move_out_of_an_index_projection, no_move_out_of_a_shared_field, no_move_out_of_a_field_of_a_drop_type (new, drop schedule, interp/build/ASan) |
| 4818 | The fixes are `.clone()`, `mem.take`, `mem.replace`, `mem.swap` and `Option.take()` | covered | moving_out_of_a_borrowed_place_with_the_fixes (new, drop schedule, interp/build/ASan) |
| 4819 | References are decided by signatures, and views cannot escape their origins | covered | a_view_may_not_escape_its_origin (E0509) (new, drop schedule, interp/build/ASan) |
| 4820 | The compiler never turns an owned value into a shared or atomically counted one | covered | plain_struct_moved_then_used_refused, shared_graph_without_freeze_in_par_refused |
| 4821 | Values are destroyed at scope end in reverse declaration order; temporaries at the end of their statement | covered | values_drop_in_reverse_declaration_order_temporaries_at_the_statement_end (new, drop schedule, interp/build/ASan) |
| 4822 | A panic ends the process without running drops | covered | a_panic_ends_the_process_without_running_drops (new, drop schedule, interp/build/ASan) |
| 4826 | Each step of the escalation path is written in the source, never a compiler decision | covered | shared_graph_without_freeze_in_par_refused (no implicit freeze), not_cross_task_sync_form_is_not_a_machine_fix (no implicit sync) |

## Shared types

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4839 | There is no ambient global mutable state | not testable | (module-level `let mut` is §13's; a philosophy sentence here) |
| 4841 | `shared struct` gives reference semantics with a reference count | covered | shared_struct_handles_name_one_object |
| 4852 | `root.left = Some(child)` is accepted because `left` is `mut`; `child` stays usable | covered | shared_struct_handles_name_one_object |
| 4854 | `let alias = root;`: `alias` and `root` name the same node | covered | shared_struct_handles_name_one_object |
| 4859 | Using a regular struct as a value moves it | covered | plain_struct_moved_then_used_refused |
| 4859 | Using a shared value (assign, store, pass to `own`) increments the count; the source stays usable | covered | shared_handle_passed_to_own_stays_usable |
| 4860 | A shared type is mutated through `mut` fields, through any handle | covered | shared_mut_field_written_through_any_handle |
| 4861 | A shared object is destroyed when the last handle is released | covered | a_shared_object_is_destroyed_when_its_last_handle_is_released (new, drop schedule, interp/build/ASan) |
| 4862 | A cycle of strong handles leaks unless a back-edge is `weak` | covered | a_cycle_of_strong_handles_is_never_freed (LSan reports it; `detect_leaks=0` via `.env`), a_weak_back_edge_lets_the_pair_be_freed (clean under LSan) (new, drop schedule, interp/build/ASan) |
| 4865 | Each binding, field or temporary holding a handle releases it when dropped; Drop body and fields run at zero | covered | a_shared_object_is_destroyed_when_its_last_handle_is_released (new, drop schedule, interp/build/ASan) |
| 4866 | A handle aggregate counts the same way, so `cur = node.next` walks a list without `.clone()` | covered | a_handle_aggregate_counts_so_a_list_walks_without_clone (new, drop schedule, interp/build/ASan) |
| 4867 | Any other type containing a handle (`Vec[Node]`, a user struct) is move-only; `.clone()` increments | covered | a_vec_of_handles_is_move_only, a_user_struct_holding_a_handle_is_move_only (new, drop schedule, interp/build/ASan) |
| 4869 | A shared type's fields are fixed at construction unless declared `mut`; assigning a non-`mut` field is an error | covered | shared_non_mut_field_assignment_refused |
| 4869 | A `mut` field can be written through any handle, including through a `ref` or a borrowed parameter | covered | shared_mut_field_written_through_any_handle |
| 4872 | A `Copy` or handle-aggregate `mut` field is read as a value and holds no borrow, including as a scrutinee | covered | a_handle_aggregate_mut_field_is_read_as_a_value_peek_then_mutate (new, drop schedule, interp/build/ASan) |
| 4888 | Any other `mut` field is borrowed: `ref` reads, `mut ref` and `mut ref self` calls write, `match` / `for` read for the construct | covered | a_ref_into_a_mut_field_conflicts_with_a_write_through_the_same_handle, a_loop_over_a_mut_field_borrows_it_so_a_push_through_the_same_handle_is_refused (new, drop schedule, interp/build/ASan) |
| 4889 | A conflict through the same handle is a compile error (the `requeue` example), and the clone-first fix compiles | covered | a_loop_over_a_mut_field_borrows_it_so_a_push_through_the_same_handle_is_refused, cloning_the_field_first_ends_the_borrow (new, drop schedule, interp/build/ASan) |
| 4914 | Each `mut` field neither `Copy` nor a handle aggregate carries a borrow flag; a conflict through another handle panics | covered | a_conflict_through_another_handle_panics_naming_the_field (new, drop schedule, interp/build/ASan) |
| 4924 | The panic message names the field and the source spans of both accesses | covered | a_conflict_through_another_handle_panics_naming_the_field (new, drop schedule, interp/build/ASan); also tests/design_11_conformance_2026_11_22.rs (d11_the_flag_panic_names_where_a_ref_holder_was_made, the 11-22 pass) |
| 4924 | A flag costs one byte per field before padding; `Copy`, handle-aggregate and immutable fields have none | covered | explain_reports_a_shared_types_borrow_flags |
| 4924 | `karac explain` reports the total for each type | covered | explain_reports_a_shared_types_borrow_flags |
| 4926 | Methods take a borrowed `self` only: `mut ref self` is an error | covered | shared_method_mut_ref_self_refused |
| 4926 | A consuming `own self` is an error | covered | shared_method_own_self_refused |
| 4926 | A method changes the object through its `mut` fields | covered | shared_mut_field_written_through_any_handle |
| 4928 | A `shared` scrutinee counts as a `ref` scrutinee; binding a part borrows it; moving it is an error fixed by `.clone()`; a whole-handle binding copies the handle | covered | binding_a_part_of_a_shared_scrutinee_borrows_it_moving_the_binding_is_refused, a_shared_scrutinee_part_clones_and_a_whole_handle_binding_copies (new, drop schedule, interp/build/ASan) |
| 4930 | A `shared` type has no default `==` | covered | shared_type_has_no_default_equality_refused |
| 4930 | `#[derive(PartialEq, Eq)]` gives structural equality | covered | shared_derive_eq_is_structural_and_ref_eq_is_identity |
| 4930 | `a.ref_eq(b)` compares identity, on every `shared`, `sync` and `frozen` handle type, with no bound | covered, FAILED at the audit | ref_eq_needs_no_bound_on_shared_sync_and_frozen (new): `a.ref_eq(b)` on two `frozen` handles was refused as an escape (E0511); fixed, B-2026-11-24-50 |
| 4932 | `Identity[T]` keys a `Map` or `Set` by handle identity | covered | identity_keys_a_map_by_handle |
| 4932 | `Identity.of(h)` (or `h.identity()`) copies the handle | covered | identity_keys_a_map_by_handle |
| 4932 | The count rises, so the object cannot be freed while the key exists | covered | an_identity_key_keeps_the_object_alive (new, drop schedule, interp/build/ASan) |
| 4932 | `Identity[T]` is `Hash + Eq + Clone`, never `Ord` | covered | identity_keys_a_map_by_handle, identity_is_never_ord_refused, identity_is_not_a_sorted_set_element_refused |
| 4932 | Two keys are equal exactly when `.ref_eq` holds | covered | identity_keys_a_map_by_handle |
| 4932 | Iteration order is the per-process hash order | covered | (the `Map` rule; conformance/collections) |
| 4932 | `.get()` returns a copy of the handle | covered | identity_keys_a_map_by_handle |
| 4932 | It is `CrossTask` exactly when `T` is | covered | identity_of_a_shared_handle_is_not_cross_task_refused, identity_of_a_sync_handle_crosses_tasks |
| 4932 | `Identity[T]` is for shared, sync or frozen handle types | covered | identity_of_a_plain_struct_refused |
| 4932 | A plain address (`ref_id`) is not offered | covered | no_ref_id_address_is_offered_refused |
| 4934 | A `shared` handle never crosses a task boundary (`E_NOT_CROSS_TASK`) | covered | shared_handle_cannot_enter_a_par_branch_refused, shared_handle_cannot_enter_a_task_group_task_refused |
| 4936 | `shared enum` has the same semantics | covered | shared_enum_counts_like_shared_struct |
| 4938 | Field visibility follows the rules for ordinary structs | covered | shared_field_visibility_follows_struct_rules_refused, shared_pub_mut_field_written_from_another_package |
| 4940 | Writing a `mut` field is not an effect | covered | writing_a_shared_mut_field_is_not_an_effect |
| 4944 | Deduplicated self-referential data uses `SlotMap[T]` and `Handle[T]`, not `weak` | not testable | (advice) |

## Cycles and `weak`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4948 | A cycle of strong handles is never freed; there is no cycle collector | covered | a_cycle_of_strong_handles_is_never_freed, a_weak_back_edge_lets_the_pair_be_freed (new, drop schedule, interp/build/ASan) |
| 4948 | Reading a `weak` field yields `Option[T]` | covered | weak_field_reads_as_option_and_stores_a_strong_handle |
| 4960 | A strong handle is stored as it is (`c.parent = p`) and downgrades | covered | weak_field_reads_as_option_and_stores_a_strong_handle |
| 4960 | `None` stores an empty weak (`Kid { parent: None }`) | covered | weak_field_reads_as_option_and_stores_a_strong_handle |
| 4960 | The field is `weak Parent`, and a read gives `Option[Parent]` either way | covered | weak_field_reads_as_option_and_stores_a_strong_handle |
| 4962 | A graph keeps every edge weak (`Vec[weak GraphNode]`) and pushing a strong handle converts it | covered | weak_vec_edges_convert_on_push |
| 4976 | A self-reference with no way to end is an error | covered | self_reference_with_no_end_refused |
| 4976 | A self-reference through a type with no empty case (`(Node, i64)`) is an error | covered | self_reference_through_a_tuple_refused |
| 4976 | A self-reference through `Option`, `Vec`, `Map` or `Set` is accepted | covered | self_reference_through_an_empty_case_is_accepted |
| 4976 | Built without a cycle they free normally | covered | a_handle_aggregate_counts_so_a_list_walks_without_clone (new, drop schedule: an acyclic `Option[Node]` list, leak-checked on the ASan lane), with a_weak_back_edge_lets_the_pair_be_freed for the cyclic shape |
| 4976 | The compiler does not infer `weak` | not testable | (an absence) |
| 4979 | Strong to weak is implicit when assigning or passing | covered | weak_field_reads_as_option_and_stores_a_strong_handle, weak_accepts_a_strong_handle_passed_to_a_weak_parameter |
| 4980 | A read checks liveness: `Some(handle)` (counted) or `None` | covered | weak_read_is_none_once_the_object_is_gone |
| 4981 | `.upgrade()` does the same as reading the field | covered | weak_field_reads_as_option_and_stores_a_strong_handle, weak_vec_edges_convert_on_push |
| 4982 | `weak` combines with `pub` and `mut` | covered | weak_combines_with_pub_and_mut |
| 4983 | A `weak` reference to a `sync` type is atomic | partial | sync_derive_eq_and_weak_work_as_for_shared runs one; atomicity of the liveness check and increment is a TSan property, not a cell |
| 4986 | `Option.expect(msg)` panics like `.unwrap()` and the message appears in the report | covered | weak_expect_panics_with_its_message |
| 4987 | `while let Some(p) = current.parent { current = p; }` upgrades once per iteration | covered | weak_upgrade_once_per_iteration_walks_to_the_root |
| 4989 | `.unwrap()` directly on a `weak` read (`node.parent.unwrap()`, `w.upgrade().unwrap()`) is a check warning | covered | weak_read_unwrap_is_fixed_to_expect, weak_upgrade_unwrap_warns |
| 4989 | Its machine-applicable fix rewrites it to `.expect("…")` | covered | weak_read_unwrap_is_fixed_to_expect |
| 4989 | On any other `Option`, the rewrite is never a check warning | covered | unwrap_on_an_ordinary_option_is_not_a_check_warning |
| 4991 | There is no unsafe escape hatch such as `assume_valid` | covered | no_assume_valid_escape_hatch_refused |

## `sync` types

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4995 | A `sync` type's count is atomic | partial | sync_fields_read_and_change_across_tasks (handles used from two branches); atomicity is a TSan property |
| 5014 | No field is written `mut`: `mut val: i64` is an error | covered | sync_mut_plain_field_refused |
| 5014 | `mut count: Atomic[u64]` is an error too | covered | sync_mut_atomic_field_refused |
| 5015 | Plain fields are fixed at construction and read from any task | covered | sync_fields_read_and_change_across_tasks |
| 5016 | `Atomic[T]` fields change through atomic methods | covered | sync_fields_read_and_change_across_tasks, sync_handles_enter_par_for_and_task_groups |
| 5017 | `Mutex[T]` fields change through a lock guard | covered | sync_fields_read_and_change_across_tasks, sync_struct_with_a_mutex_around_plain_state |
| 5018 | Every field's type must be `CrossTask`, so a `sync` type cannot hold a `shared` handle | covered | sync_field_holding_a_shared_handle_refused |
| 5020 | `sync enum` follows the same rules in every variant | covered | sync_enum_mut_field_refused, sync_enum_variants_hold_atomics_and_mutexes |
| 5030 | Handles count like shared handles; passing one to an `own` parameter increments it | covered | sync_enum_variants_hold_atomics_and_mutexes (`let x = w` then `w` used) |
| 5030 | Patterns, equality and `weak` fields work as for `shared` types | covered | sync_derive_eq_and_weak_work_as_for_shared, ref_eq_needs_no_bound_on_shared_sync_and_frozen |
| 5032 | Methods take a borrowed `self` only | covered | sync_method_mut_ref_self_refused, sync_fields_read_and_change_across_tasks |
| 5041 | `sync` handles enter `par` branches, `par for` iterations and `TaskGroup` tasks freely | covered | sync_fields_read_and_change_across_tasks, sync_handles_enter_par_for_and_task_groups |
| 5043 | A `sync` type adds no effects; atomic operations have none | covered | sync_atomics_add_no_effects_and_lock_blocks |
| 5043 | `Mutex.lock` has `blocks` | covered | sync_atomics_add_no_effects_and_lock_blocks |
| 5055 | The compiler suggests the conversion as a note showing the declaration before and after | covered | not_cross_task_note_shows_the_sync_form |
| 5065 | The note puts `bool`, `i32`, `i64`, `u32`, `u64` fields in `Atomic[T]` and every other in `Mutex[T]` | covered | not_cross_task_note_shows_the_sync_form |
| 5065 | It is not a machine fix, and `karac fix` does not apply it | covered | not_cross_task_sync_form_is_not_a_machine_fix |
| 5067 | A mostly single-task type keeps its state in a plain struct behind one `Mutex` | covered | sync_struct_with_a_mutex_around_plain_state |
| 5080 | Not decided for v1: `sync` as the default | not testable | |

## Frozen handles

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5084 | Two tasks reading one `shared` handle are rejected | covered | shared_graph_without_freeze_in_par_refused |
| 5088 | The page's example: a frozen graph read by two `par` branches | covered | frozen_graph_is_read_by_two_par_branches |
| 5107 | 1. non-counting | partial | no cell observes a count; the guarantee shows as 2 and 3 below |
| 5108 | 2. deeply immutable: nothing reachable through it may be written | covered | frozen_scalar_field_write_refused, frozen_mut_method_write_refused |
| 5109 | 3. non-escaping: it may not outlive its owner | covered | frozen_handle_returned_refused, frozen_handle_stored_in_a_non_frozen_slot_refused, frozen_binding_returned_from_its_scope_refused (it called the frozen `let` a parameter; fixed, B-2026-11-24-53) |
| 5110 | 4. a parameter mode usable across signatures | covered | frozen_graph_is_read_by_two_par_branches, frozen_place_reads_from_the_table |
| 5114 | `frozen self` is the receiver form | covered | frozen_self_receiver |
| 5114 | `freeze` is written only as a `let` initializer, not a block tail or an `if` arm | covered | freeze_in_an_if_arm_refused, freeze_as_a_block_tail_refused |
| 5114 | Its source is a local with no other live name, or a call whose arguments hold no handle | covered | freeze_of_a_unique_local_and_of_a_handle_free_call, freeze_of_a_source_used_before_refused, freeze_of_a_call_whose_argument_holds_a_handle_refused (its duplicate "temporary" error removed, B-2026-11-24-53), freeze_of_a_struct_literal_refused |
| 5114 | Pushing such a value into a field of the source leaves the source unique | covered | freeze_of_a_unique_local_and_of_a_handle_free_call |
| 5114 | A value never becomes frozen by inference | covered | shared_graph_without_freeze_in_par_refused |
| 5120 | Table: a scalar read at any depth; a read through an index; a whole handle into another `frozen` slot; `len` / `is_empty`; both `for` spellings; `let k = n.kids[i]`; `match` on an `Option` field | covered | frozen_place_reads_from_the_table |
| 5127 | Table: `n.count = 5` and every other write is refused | covered | frozen_scalar_field_write_refused, frozen_mut_method_write_refused |
| 5128 | Table: capturing `n` in a closure is permitted; the closure is a view and may not escape | covered | frozen_place_reads_from_the_table, closure_over_a_frozen_handle_may_not_escape_refused |
| 5129 | Table: `return n;` and storing the handle in a non-frozen slot are refused | covered | frozen_handle_returned_refused, frozen_handle_stored_in_a_non_frozen_slot_refused |
| 5131 | A scalar read out of a frozen place may go anywhere | covered | frozen_scalar_read_goes_anywhere |
| 5133 | A `mut` field is readable through a frozen place and never writable | covered | frozen_place_reads_from_the_table, frozen_mut_method_write_refused |
| 5133 | Reading a frozen root while writing fresh copies does nothing wrong | covered | frozen_reads_beside_writes_to_fresh_copies |
| 5135 | A `Vec` local of frozen handles: `push`, `len`, `is_empty`, `q[i]` (itself a frozen place) | covered | frozen_worklist_in_a_vec_local |
| 5135 | It is `Vec`-only; a `VecDeque` cannot keep frozen-ness | covered | frozen_worklist_in_a_vec_deque_refused |
| 5137 | `freeze` refuses when a type cannot be resolved | not testable | (an unresolved type stops the typechecker first) |
| 5138 | Refuses a non-`shared` type | covered | freeze_of_a_plain_struct_refused |
| 5139 | Refuses a `sync` type | covered | freeze_of_a_sync_type_refused |
| 5140 | Refuses a non-unique source for a type with a `mut` field | covered | freeze_of_a_non_unique_source_with_a_mut_field_refused |
| 5140 | A type with no `mut` field anywhere needs no uniqueness | covered | freeze_of_an_aliased_type_with_no_mut_field |
| 5142 | A `par` branch or `par for` iteration may use a frozen handle; without `freeze` it is refused | covered | frozen_graph_is_read_by_two_par_branches, shared_graph_without_freeze_in_par_refused |
| 5144 | Cost: a frozen parameter changes no count | partial | as 5107 |
| 5146 | Not decided for v1 (scoped freezes, a type-position spelling) | not testable | |

## Handles in closures

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5152 | Any closure may capture a `shared`, `sync` or `frozen` handle | covered | the four cells below |
| 5153 | `shared` and `sync` handles are counted when captured, so the closure holds its own | covered | closure_capturing_a_shared_handle_counts_it_and_may_escape |
| 5153 | A closure that captures a `shared` handle is task-local | covered | closure_capturing_a_shared_handle_is_task_local_refused, closure_capturing_a_sync_handle_enters_par |
| 5154 | A closure over a `frozen` handle may be called, passed down, and enter `par` branches and `TaskGroup` tasks | covered | closure_over_a_frozen_handle_is_called_passed_and_enters_par, closure_over_a_frozen_handle_enters_a_task_group_task |
| 5154 | It may not escape | covered | closure_over_a_frozen_handle_may_not_escape_refused |

## Typestate

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5158 | An operation on a value in the wrong state is rejected | covered | typestate_operation_in_the_wrong_state_refused, typestate_protocol_runs_in_order |
| 5178 | Once `connect` consumes the old value, using it is E0500 | covered | a_consumed_typestate_value_used_again_is_e0500 (new, drop schedule, interp/build/ASan) |

## `#[must_use]`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5182 | On a type, silently dropping a value of that type warns (the page's `connect(conn)?;` example) | covered | must_use_type_warns_with_its_reason |
| 5196 | `let _ = connect(conn)?;` suppresses the warning | covered | must_use_let_underscore_suppresses |
| 5199 | On a type, it applies wherever a value would be dropped without being bound or used | covered | must_use_type_warns_with_its_reason, must_use_without_a_reason_uses_a_default |
| 5200 | The warning text is the string argument; without one, a default message | covered | must_use_type_warns_with_its_reason, must_use_without_a_reason_uses_a_default |
| 5201 | On a function, it warns when the return value is discarded | covered | must_use_on_a_function_warns_on_a_discarded_return |
| 5202 | `let _ = expr;` drops the value at the `;` | covered | let_underscore_drops_the_value_at_the_semicolon (new, drop schedule, interp/build/ASan) |
| 5205 | 1. `Result` and `Option` are must-use by a compiler rule, in user code too | covered | must_use_result_and_option_in_user_code |
| 5206 | 2. Every iterator adapter | covered | must_use_iterator_adapter_guard_builder_and_pure_transforms (`map`; tests/ cover the rest) |
| 5207 | 3. Every guard type, with a message naming the resource | covered | must_use_iterator_adapter_guard_builder_and_pure_transforms |
| 5208 | 4. Every builder step before the final call | covered | must_use_iterator_adapter_guard_builder_and_pure_transforms (`Command.arg`) |
| 5209 | 5. Pure transformations (`to_lowercase`, `trim`, `replace`, `iter`) | covered | must_use_iterator_adapter_guard_builder_and_pure_transforms |
| 5211 | The displaced-value exception (`Map` / `SortedMap` insert and remove, `Vec.pop`, `remove_first`, `VecDeque` pops, `PriorityQueue.pop`, `pop_first` / `pop_last`, `String.pop`) | covered | must_use_displaced_value_exceptions |
| 5211 | A user-defined `insert` returning `Option` still warns | covered | must_use_user_insert_returning_option_still_warns |
| 5213 | `m.try_insert(k, v)?;` is exempt; without `?` the `Result` warns | covered | must_use_try_insert_follows_its_twin |
| 5215 | The mandate is enforced by a standard library lint | covered | (tests/ for `missing_must_use_lint`) |

## `#[non_exhaustive]`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5221 | A `match` outside the defining package needs a wildcard arm, even naming every variant | covered | non_exhaustive_enum_match_without_wildcard_refused, non_exhaustive_enum_match_with_wildcard_runs |
| 5221 | Inside the defining package, matches are exhaustive as usual | covered | non_exhaustive_enum_match_with_wildcard_runs (lib's `name`) |
| 5222 | A struct literal outside the package is an error whose diagnostic names the package's constructor function | covered, FAILED at the audit | non_exhaustive_struct_literal_names_the_constructor_refused (new): the refusal said "commonly `Options.new(...)`" whatever the package declares; fixed, B-2026-11-24-51 |
| 5222 | A pattern there must end with `..` | covered | non_exhaustive_struct_pattern_without_rest_refused, non_exhaustive_struct_pattern_with_rest_runs |
| 5223 | A `match` without the wildcard gets a machine-applicable fix that adds `_ => panic("handle new variant")` | covered | non_exhaustive_match_fix_adds_a_wildcard (the page said `_ => todo()`; review ruled 2026-11-24 to change the page to what `karac fix` writes) |
| 5234 | The standard library marks its growing public error enums and option structs | partial | not every such type checked; std's `#[non_exhaustive]` uses are covered by tests/ |

## `#[deprecated]`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5238 | On a function, struct, enum, variant, trait, method, associated function, type alias or constant | covered | deprecated_applies_to_every_item_kind |
| 5238 | Every use (a call, a type name, a variant constructor, a pattern) warns at the use site | covered | deprecated_applies_to_every_item_kind |
| 5238 | The lint warns by default and code keeps compiling | covered | deprecated_bare_note_and_since_forms_warn_at_each_use |
| 5251 | The bare form gives a default warning; `since:` and `note:` are labeled and optional | covered | deprecated_bare_note_and_since_forms_warn_at_each_use |
| 5253 | The diagnostic shows the item, the note and the suppression on one line | covered | deprecated_bare_note_and_since_forms_warn_at_each_use (the page's block showed `= note` / `= help` lines; review ruled 2026-11-24 that typecheck warnings are one line, and the block now shows that) |
| 5263 | With `since`, the diagnostic adds the version | covered | deprecated_bare_note_and_since_forms_warn_at_each_use |
| 5263 | For a `pub` item it names the defining package | covered | deprecated_pub_item_names_its_defining_package: the warning names the item by its path in its package and the package (`lib.load_config` from package `lib`), fixed by B-2026-11-24-52 |
| 5265 | `#[allow(deprecated)]` works at a use site, on a function or on a module | covered | deprecated_allow_at_use_site_function_and_module, deprecated_allowed_for_a_whole_module |
| 5265 | `#[expect(deprecated)]` reports itself once the use is gone | covered | deprecated_allow_at_use_site_function_and_module |
| 5266 | It is not transitive: callers and a struct's methods are not marked | covered | deprecated_is_not_transitive |
| 5266 | It does not remove the item or change its behavior | covered | deprecated_bare_note_and_since_forms_warn_at_each_use (runs) |
| 5265 | (implied) the defining package's own uses warn in that package | covered | they warn when that package is checked, and not in a dependent's `check` or `run` (B-2026-11-24-54, tests/dependency_warnings.rs) |
| 5267 | Rejected on an `impl` block (`E_DEPRECATED_ON_IMPL`) | covered | deprecated_on_an_impl_block_refused |
| 5267 | Rejected on a struct field | covered | deprecated_on_a_struct_field_refused |
