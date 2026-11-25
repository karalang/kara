# design.md § 12 Effects: sentence-to-cell map

Status: covered, partial, FAILS (filed, the row named), not testable. Cell names without a prefix are in `conformance/design/effects/`; `traits/` and `modules/` name other directories under `conformance/design/`, `corpus/core/` the core pins, and `tests/` the Rust suites. Line numbers are docs/design.md lines at the audit (2026-11-24). `(new)` marks a cell the audit wrote; every new refusal has a control beside it where one exists. The earlier §12 pass (shared folder `conformance/design-12/`, 2026-11-22) and the probe's trait-mediated conflict sweep (`probe-fuzzers/ef/`) are moved in here; the providers half of the earlier pass belongs to core-semantics.md §14 and stays with `tests/mir_provider_*.rs`.

Run with `python3 conformance/cases.py conformance/design/effects` and, for the two package cells, `python3 conformance/packages.py conformance/design/modules`. Kinds this directory adds to cases.py: `.effects` (the sets `karac query effects` infers), `.cmd` (a karac command's output), `.json` (a predicate over a JSON document), `.notes` and `.absent` (text `karac check` must or must not print).

**Totals (2026-11-24):** 219 normative sentences, 204 covered, 5 partial, 10 not testable. Two of the covered failed when the audit started and are fixed with it: a `par` branch was not charged the program impls a native runs on what it holds (B-2026-11-24-44), and a function declaring a keyed resource was charged nothing for it (B-2026-11-24-45). 39 sentences had no cell before the audit; the rest were covered by the earlier pass, the probe's sweep and the Rust suites.

## Introduction

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5273 | Every function has an effect set | covered | pure_function_has_no_effects, private_functions_infer_transitively_through_private |
| 5274 | A `pub fn` declares what it may do, and by omission what it may not | covered | pub_fn_that_prints_without_declaring_is_an_error, declaration_that_misses_an_effect_is_an_error_naming |
| 5274 | `#[no_effect(...)]` forbids effects below a boundary | covered | no_effect_allocates_heap_rejects_a_transitive_allocation |
| 5275 | Two `par` branches that conflict on a resource are a compile error | covered | two_par_branches_that_both_print_conflict, reads_writes_same_resource_conflict_refused (new) |
| 5276 | `karac query effects` shows every function's effect set | covered | every `.effects` cell |
| 5276 | `karac explain` shows why it has each effect | covered | explain_shows_default_permitted_effects_and_their_chain (new), explain_traces_blocks_edge_by_edge (new) |
| 5278 | Private functions infer their effects | covered | private_functions_infer_transitively_through_private |
| 5278 | The body may not exceed a public declaration | covered | declaration_that_misses_an_effect_is_an_error_naming |
| 5278 | A declaration may name effects the body does not perform (E0401 retired) | covered | pub_fn_may_declare_more_than_its_body_does_e0401 |
| 5289 | A function without `reads(FileSystem)` cannot read files: the body is rejected | covered | pub_fn_without_filesystem_cannot_read_files_refused + control pub_fn_with_filesystem_reads_files (new) |
| 5289 | No runtime capability tokens or object-capability enforcement | not testable | a statement of the design |

## Effect verbs

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5293 | The verb table: each verb's meaning and what it conflicts with | covered | the Conflict rules rows below |
| 5304 | `reads`, `writes`, `sends`, `receives` are the conflict verbs | covered | reads_writes_same_resource_conflict_refused, sends_sends_same_resource_conflict_refused, receives_receives_same_resource_conflict_refused (new) |
| 5304 | `allocates` and `panics` never conflict and are default-permitted | covered | allocates_panics_blocks_never_conflict (new), pub_fn_need_not_declare_allocates_heap_or_panics |
| 5304 | `blocks` says where a function may wait, not what it touches | covered | allocates_panics_blocks_never_conflict (new) |
| 5306 | `suspends` takes no resource and never conflicts | covered | allocates_panics_blocks_never_conflict (new: both branches sleep, so both suspend) |
| 5306 | `suspends` is inferred for private functions and declared on public ones | covered | std_time_sleep_is_reads_clock_blocks_suspends; tests/effects_blocks_admits_suspends.rs |
| 5306 | A declared `blocks` admits `suspends`, never the reverse | covered | declared_blocks_admits_suspends, blocks_must_be_declared_on_a_pub_fn (declares `suspends`, refused for `blocks`) |
| 5306 | `time.sleep` is `reads(Clock) blocks suspends` | covered | std_time_sleep_is_reads_clock_blocks_suspends |
| 5308 | A clause follows the return type; effects are written side by side | covered | one_verb_lists_several_resources (new) |
| 5308 | A verb may list several resources: `reads(UserDb, OrderDb)` | covered | one_verb_lists_several_resources (new) |
| 5308 | Every effect word but `with` is an ordinary identifier outside a clause | covered | effect_words_are_ordinary_identifiers_outside_a_clause (new) |

## Conflict rules

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5314 | `reads` + `reads`: safe | covered | reads_reads_same_resource_par_ok (new), two_clock_reads_in_par_do_not_conflict |
| 5315 | `reads` + `writes`, same resource: conflict | covered | reads_writes_same_resource_conflict_refused (new) |
| 5316 | `writes` + `writes`, same resource: conflict | covered | writes_writes_same_resource_conflict_refused (new), two_par_branches_that_both_print_conflict |
| 5315 | Different resources: safe | covered | reads_writes_different_resources_par_ok (new) |
| 5317 | `sends` + `sends`, same resource: conflict | covered | sends_sends_same_resource_conflict_refused (new), one_channel_in_two_branches_is_a_conflict |
| 5318 | `receives` + `receives`, same resource: conflict | covered | receives_receives_same_resource_conflict_refused (new); tests/channel_effect_clause.rs channel_effect_clause_keeps_two_receives_apart |
| 5319 | `sends` + `receives`: safe | covered | sends_receives_and_state_vs_messages_par_ok (new), send_and_a_receive_on_one_channel_do_not_conflict |
| 5320 | `reads`/`writes` + `sends`/`receives`: safe | covered | sends_receives_and_state_vs_messages_par_ok (new) |
| 5321 | `allocates` + anything: safe | covered | allocates_panics_blocks_never_conflict (new) |
| 5322 | `panics`, `blocks` never conflict | covered | allocates_panics_blocks_never_conflict (new) |
| 5324 | Sends keep their order, and so do receives | covered | as 5317, 5318 |
| 5325 | A send and a receive on one resource are independent | covered | as 5319 |
| 5326 | State and communication are independent | covered | as 5320 |
| 5327 | `FileSystem` and `Network` effects never conflict | covered | filesystem_is_a_capability_two_path_writes_do_not, network_is_a_capability_two_dials_do_not_conflict (new) |
| 5328 | Conflicts are checked between `par` branches | covered | every `par` conflict cell; the 28 `par_conflict_via_*_refused` cells reach the print through an operator, a library generic or a built-in that runs a program impl, each with its `par_apart_via_*` control |
| 5328 | ... and between the iterations of a `par for` | covered | par_for_iterations_that_print_conflict_refused + control par_for_iterations_apart_ok (new) |
| 5328 | A conflict is an error that names both accesses | covered | reads_writes_same_resource_conflict_refused (new: names `reads(Db)` and `writes(Db)`) |
| 5328 | `TaskGroup` tasks are not checked for effect conflicts | covered | taskgroup_tasks_are_not_conflict_checked (new) |
| 5328 | (built-ins that run a program impl: `sort`, `clone`, `dedup`, a `Map` / `Set` key) | covered, FAILED at the audit | par_conflict_via_sort_calls_cmp_refused, _clone_via_vec_clone_, _dedup_eq_, _map_insert_calls_hash_, _set_contains_hash_: accepted until B-2026-11-24-44 |

## Resources

### Built-in resources

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5336 | `FileSystem`: `reads` / `writes` | covered | builtin_resource_verbs (new) |
| 5337 | `Stdin`: `writes` (a read advances the stream) | covered | builtin_resource_verbs (new) |
| 5338 | `Stdout`: `writes` | covered | private_functions_infer_transitively_through_private |
| 5339 | `Stderr`: `writes` | covered | built_in_resources_clock_randomsource_env_stderr |
| 5340 | `Clock`: `reads` | covered | built_in_resources_clock_randomsource_env_stderr |
| 5341 | `Env`: `reads` / `writes` | covered | built_in_resources_clock_randomsource_env_stderr, builtin_resource_verbs (new: `env.set` writes) |
| 5342 | `Network`: `sends` / `receives` | partial | builtin_resource_verbs (new: `sends(Network)` on connect); `receives(Network)` on a read needs a peer, tests/effects_socket_waits.rs |
| 5343 | `RandomSource`: `writes` | covered | built_in_resources_clock_randomsource_env_stderr |
| 5344 | `Heap`: `allocates` | covered | allocation_is_inferred_as_allocates_heap |
| 5346 | Two `par` branches that both print conflict | covered | two_par_branches_that_both_print_conflict |
| 5348 | Path-taking functions do not conflict | covered | filesystem_is_a_capability_two_path_writes_do_not |
| 5348 | Two branches that write one `File` conflict; separate files do not | covered | one_file_written_in_two_branches_conflict_refused + two_files_written_in_two_branches_ok (new) |
| 5348 | A connection is keyed by its value | partial | channels stand in: two_channels_are_two_resources_par_over_separate; tests/effect_value_roots.rs |
| 5350 | Two branches that write one path by name are not detected | covered | filesystem_is_a_capability_two_path_writes_do_not (one path) |
| 5352 | `CompileTimeEnv` and `CompileTimeHeap` are reserved | covered | compile_time_env_resource_name_reserved_refused, compile_time_heap_resource_name_reserved_refused + control ordinary_resource_name_declares (new) |

### Value-rooted resources

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5357 | Two channels, connections or files are two resources | covered | two_channels_are_two_resources_par_over_separate, two_files_written_in_two_branches_ok (new) |
| 5358 | When the compiler cannot tell two values apart, they conflict | covered | values_not_known_apart_conflict_refused (new) |
| 5359 | A parameter-rooted resource is substituted by the argument at each call | covered | two_channels_are_two_resources_par_over_separate, one_channel_in_two_branches_is_a_conflict |
| 5360 | Effects on a function's own locals never appear in its signature | covered | effects_on_a_function_s_own_locals_are_not_effects_of |
| 5360 | Inside its body they still decide which branches conflict | covered | one_channel_in_two_branches_is_a_conflict |
| 5361 | A function type writes `sends(Channel)` / `receives(Channel)` | covered | fn_type_clause_names_some_channel (new); tests/channel_effect_clause.rs |
| 5361 | `Channel` conflicts with every channel | covered | tests/channel_effect_clause.rs channel_effect_clause_keeps_two_receives_apart |
| 5385 | Automatic parallelism keeps one connection's writes in order | not testable | a property of the auto-parallel gate, which reads the same keys |

### Nondeterminism resources

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5389 | Every source of nondeterminism enters through a named resource | covered | built_in_resources_clock_randomsource_env_stderr |
| 5391 | `Clock` covers wall-clock and monotonic time, one resource | partial | built_in_resources_clock_randomsource_env_stderr (monotonic `Instant`); `Clock.wall_ns` through a provider, tests/mir_provider_overrides.rs |
| 5392 | A draw is `writes(RandomSource)`; two `par` branches that draw conflict | covered | two_random_draws_in_par_conflict |
| 5392 | A generator seeded from an argument does not touch `RandomSource` | covered | seeded_generator_touches_no_random_source (new) |
| 5393 | Reading the environment is `reads(Env)`, setting a variable `writes(Env)` | covered | builtin_resource_verbs (new) |
| 5397 | A private function infers `reads(Clock)` transitively | covered | built_in_resources_clock_randomsource_env_stderr |
| 5397 | A public function declares it | covered | declaration_that_misses_an_effect_is_an_error_naming |
| 5397 | `#[no_effect(reads(Clock, Env), writes(RandomSource))]` forbids all three | covered | no_effect_clock_env_random_forbids_all_three_refused (new), no_effect_reads_clock_rejects_a_clock_read |
| 5399 | A clock read inside an observability operation is transparent | covered | pub_fn_that_only_logs_declares_nothing (a log record carries a timestamp) |
| 5399 | The exemption cannot launder a clock read: only library observability operations have it | covered | observability_attribute_is_reserved_refused (new), no_effect_reads_clock_rejects_a_clock_read |
| 5401 | Locale, scheduling order, thread and process ids are not resources | not testable | an absence |

### User-declared resources

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5405 | `effect resource Name;` at module level, with the visibility rules | covered | user_resource_inferred_through_callers (new); modules/cross_package_effect_resources_distinct |
| 5405 | It enters where a function declares it, and inference carries it to callers | covered | user_resource_inferred_through_callers (new), inference_stops_at_a_pub_fn_the_caller_gets_its |
| 5420 | Two `par` branches that both call `record` conflict on `AuditLog` | covered | two_branches_calling_record_conflict_on_auditlog_refused (new) |
| 5420 | Parameterized resources are deferred | covered, FAILED at the audit | keyed_resource_is_its_whole_resource_refused (new): the deferred `effect resource R[k: T]` form parses, and a function declaring `writes(R[k])` was charged nothing, so two branches writing it were accepted (B-2026-11-24-45); now the whole resource |

## Inference and declarations

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5424 | Private functions infer from bodies and callees, transitively | covered | private_functions_infer_transitively_through_private |
| 5424 | A private function may declare a set, and it is checked | covered | private_function_may_declare_a_set_and_it_is_checked |
| 5425 | `main` is private, so its effects are inferred | covered | build_prints_the_program_effects_line (new) |
| 5425 | A build prints `Effects: ` and the set, sorted, or `Effects: none` | covered | build_prints_the_program_effects_line, build_prints_effects_none (new) |
| 5425 | ... on stdout, the line before `Built:` | covered | tests/build_effects_line_2026_11_22.rs |
| 5425 | `"program_effects"` in `--output=json` | covered | build_output_json_carries_program_effects, check_output_json_carries_program_effects |
| 5426 | A public function's declaration is checked against its body | covered | declaration_that_misses_an_effect_is_an_error_naming |
| 5426 | A mismatch is an error with a machine-applicable fix | covered | missing_declaration_error_carries_a_machine (`.json` and `.fixed`), mismatched_declaration_is_fixed_by_the_exact_diff (new) |
| 5426 | A declaration may name more than the body; `unused_effect`, allowed by default, reports it | covered | unused_effect_is_allowed_by_default; tests/unused_effect.rs unused_effect_warn_reports_the_reserved_effect |
| 5427 | `allocates(Heap)` and `panics` are inferred and shown, never required | covered | pub_fn_need_not_declare_allocates_heap_or_panics, allocation_is_inferred_as_allocates_heap |
| 5428 | Effects through a non-escaping function parameter are not declared, charged to the caller | covered | non_escaping_parameter_s_effects_are_charged_to_the, each_itself_needs_no_declaration |
| 5428 | ... nor through a type parameter's unclaused trait method | covered | pub_generic_fn_through_unclaused_bound_charges_instantiator (new), call_through_a_bound_with_no_clause_has_the_instance |
| 5440 | Inference stops at public functions: a call contributes the declared set | covered | inference_stops_at_a_pub_fn_the_caller_gets_its |
| 5440 | Summaries are cached by body and callee signatures | not testable | an implementation property |
| 5441 | A stale or wrong declaration is an error | covered | declaration_that_misses_an_effect_is_an_error_naming |
| 5441 | An `extern` fn: its clause, none if `pure`, every effect if neither | covered | extern_fn_with_a_clause_has_exactly_it, extern_pure_fn_has_no_effects, extern_fn_with_neither_has_every_effect_blocks, extern_pure_fn_runs_on_every_lane, pure_is_refused_outside_extern |
| 5442 | Inference may change across releases only under the edition rules | not testable | a release policy |
| 5444 | `public_effects` is a package setting; `declared` is the default | covered | modules/public_effects_declared_default_refused; tests/public_effects_manifest.rs |
| 5452 | Under `inferred`, public functions infer, and callers are charged what they perform | covered | modules/public_effects_inferred; tests/public_effects_manifest.rs inferred_owes_no_declaration_and_charges_callers_what_the_function_performs |
| 5452 | `karac build` prints each public function's set before `Effects:` | covered | modules/public_effects_inferred_shown; tests/public_effects_manifest.rs build_shows_each_public_functions_inferred_set_before_built |
| 5462 | `public_function_effects` in JSON; a written clause is still checked | covered | tests/public_effects_manifest.rs inferred_keeps_no_effect_trait_ceilings_and_written_clauses |
| 5462 | `#[no_effect]` and trait clauses hold in both modes | covered | tests/public_effects_manifest.rs inferred_keeps_no_effect_trait_ceilings_and_written_clauses |
| 5462 | Any other value of the key is a manifest error | covered | modules/public_effects_other_value_refused |
| 5464 | Each package is held to its own setting; the standard library is `declared` | covered | tests/public_effects_manifest.rs each_package_is_held_to_its_own_setting |
| 5464 | `inferred_dependency_effects` at the first import of an `inferred` dependency | covered | modules/public_effects_dependency_inferred_warns |
| 5473 | A call whose callee cannot be resolved statically has every effect | covered | escaping_value_with_no_clause_has_every_effect_pub |
| 5474 | `extern` defaults | covered | as 5441 |
| 5475 | A drop's effects are its drop glue's, charged where the drop runs | covered | drop_s_effects_are_charged_to_the_scope_where_it_runs, drop_glue_is_transitive_through_fields, pub_fn_whose_local_s_drop_prints_must_declare_writes; corpus/core/err_drop_effects_* |
| 5476 | Recursive functions: an SCC fixed point; callees by definition, not name | covered | mutual_recursion_an_scc_fixed_point_gives_both_members, callees_are_identified_by_definition_not_name_a_method |
| 5477 | A value-rooted resource is keyed by its value | covered | as 5357 |

### Effect lattice

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5483 | Atoms: resource atoms, `panics`, `blocks`; observability contributes none | covered | log_and_dbg_are_transparent |
| 5491 | The lattice is the powerset ordered by inclusion; subtyping is its order | covered | effect_set_subtyping_reader_writer_and_pure_fit_a_reads |
| 5503 | A private callee contributes its inferred set, a public one its declared set | covered | inference_stops_at_a_pub_fn_the_caller_gets_its |
| 5514 | `panic`, `todo`, `unreachable` are `panics` atoms | covered | panics_atoms_panic_todo_unreachable_assert_expect_slice_rem (new) |
| 5515 | The `assert` family | covered | panics_atoms_panic_todo_unreachable_assert_expect_slice_rem (new) |
| 5516 | `unwrap()` and `expect(msg)` | covered | panics_atoms_index_division_overflow_unwrap, panics_atoms_panic_todo_unreachable_assert_expect_slice_rem (new) |
| 5517 | Indexing and slicing with `[]` | covered | panics_atoms_index_division_overflow_unwrap, panics_atoms_panic_todo_unreachable_assert_expect_slice_rem (new) |
| 5518 | Integer `/` and `%` | covered | panics_atoms_index_division_overflow_unwrap, panics_atoms_panic_todo_unreachable_assert_expect_slice_rem (new) |
| 5519 | Checked arithmetic that may overflow | covered | panics_atoms_index_division_overflow_unwrap; tests/effects_spec_atoms_and_clauses.rs non_trapping_integer_forms_are_not_panics_atoms (the control) |
| 5520 | A `mut` field of a `shared` value that carries a borrow flag | covered | panics_atoms_panic_todo_unreachable_assert_expect_slice_rem (new: a `Vec` field; a `Copy` field carries no flag) |
| 5522 | Allocation failure is not a `panics` atom | covered | allocation_alone_is_not_a_panics_atom (new) |
| 5524 | Monotonicity and a unique least fixed point from the empty assignment | covered | self_recursion_with_no_effects_stays_empty_least_fixed |
| 5526 | A group that exceeds `n · m` iterations is an internal compiler error | not testable | a guard against a monotonicity bug |
| 5528 | `karac explain` reports the chain `φ₀ ⊊ … ⊊ φ*` with the step that added atoms | covered | explain_reports_the_fixpoint_chain_of_a_recursive_group (new): a `Fixpoint:` section names the group and each step's added atoms with the callee or operation that added them, ending at φ*; explain_shows_default_permitted_effects_and_their_chain for the per-effect chain; B-2026-11-24-46 (fixed) |

### Mutual recursion

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5532 | Components are processed leaves first and iterated from empty sets | covered | mutual_recursion_an_scc_fixed_point_gives_both_members |
| 5538 | A public function is a firewall: inference never flows back through it | covered | public_function_is_a_firewall_in_a_cycle (new) |
| 5540 | A note lists each recursive group, in the terminal | covered | mutual_recursion_note_is_emitted |
| 5540 | ... and under `"mutual_recursion_groups"` in JSON | covered | recursive_group_is_reported_under_mutual_recursion |
| 5540 | `#[allow(mutual_recursion_note)]` suppresses it | covered | mutual_recursion_note_allowed (new) |
| 5542 | A cycle through a function used as a value is reported | covered | value_reference_cycle_is_reported_as_such (new) |
| 5543 | `"kind"`: `mutual_recursion` / `value_reference_cycle` / `mixed` | partial | recursive_group_is_reported_under_mutual_recursion, value_reference_cycle_is_reported_as_such (new); `mixed` untested |
| 5544 | `"call_recursive_members"`: all, none, or the recursing subset | partial | recursive_group_is_reported_under_mutual_recursion (all), value_reference_cycle_is_reported_as_such (none, new); `mixed` untested |

### Annotation workflow

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5553 | No declaration: an error with the annotation to add | covered | missing_declaration_error_carries_a_machine (`.fixed`) |
| 5554 | A declaration that does not match: an error with the exact diff | covered | mismatched_declaration_is_fixed_by_the_exact_diff (new) |
| 5556 | After the fix the build is clean | covered | the `.fixed` texts check clean (their `.kara` twins' expectations) |

## Default-permitted effects

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5564 | Inferred and propagated; `karac explain` shows them | covered | explain_shows_default_permitted_effects_and_their_chain (new) |
| 5565 | A `pub fn` never has to declare them, although it may | covered | pub_fn_need_not_declare_allocates_heap_or_panics, pub_fn_may_declare_allocates_and_panics_explicitly |
| 5566 | Only `#[no_effect]` forbids them | covered | no_effect_allocates_heap_rejects_a_transitive_allocation, no_effect_panics_rejects_checked_arithmetic |
| 5570 | `allocates` never conflicts, even with itself | covered | allocates_panics_blocks_never_conflict (new) |
| 5572 | `#[no_effect(allocates(Heap))]` follows the whole call graph | covered | no_effect_allocates_heap_rejects_a_transitive_allocation; corpus/core/err_no_effect_heap_allocations, no_effect_heap_free_loop |
| 5578 | Allocation failure is covered by `allocates(Heap)`, not `panics` | covered | allocation_alone_is_not_a_panics_atom (new) |
| 5580 | `#[no_effect(panics)]` proves nothing below panics | covered | no_effect_panics_rejects_checked_arithmetic; corpus/core/no_effect_panics_wrapping_arithmetic |

## `#[no_effect]`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5584 | It declares the named effects absent from the transitive set | covered | no_effect_allocates_heap_rejects_a_transitive_allocation |
| 5586 | The section's examples (`render_frame`, `mix`) | covered | no_effect_render_frame_example (new), no_effect_allocates_heap_panics_accepts_float_arithmetic |
| 5600 | The arguments use the grammar of a `with` clause | covered | no_effect_user_resource_uses_with_grammar_refused (new: `reads(Config)`) |
| 5604 | `#[no_effect(allocates)]` rejects `allocates(Heap)` and `allocates(Arena)` | covered | no_effect_bare_allocates_rejects_heap_and_arena_refused (new), bare_verb_forbids_every_resource_no_effect_writes_vs |
| 5605 | `#[no_effect(allocates(Heap))]` rejects Heap, accepts Arena | covered | no_effect_allocates_heap_rejects_heap_refused + no_effect_allocates_heap_accepts_arena (new) |
| 5606 | `#[no_effect(allocates(Arena))]` accepts Heap, rejects Arena | covered | no_effect_allocates_arena_rejects_arena_refused + no_effect_allocates_arena_accepts_heap (new) |
| 5612 | The check is transitive | covered | no_effect_allocates_heap_rejects_a_transitive_allocation |
| 5612 | It applies to a declared set: declared `panics` with `#[no_effect(panics)]` is reported | covered | declared_panics_with_no_effect_panics_is_a_contradiction |
| 5612 | A violation is `E_NO_EFFECT_VIOLATED` | covered | the new `no_effect_*_refused` cells name the code |
| 5614 | Anywhere but a function it is `E_NO_EFFECT_INVALID_TARGET` | covered | no_effect_on_a_struct_is_e_no_effect_invalid_target, no_effect_on_a_const_or_enum_is_invalid_target_refused (new); on a statement the general E_ATTRIBUTE_NOT_VALID_ON_STATEMENT fires first |
| 5614 | An empty list, `#[no_effect]` or `#[no_effect()]`, is rejected | covered | empty_no_effect_is_rejected, empty_no_effect_parens_is_rejected |

## `blocks`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5620 | No resource | covered | std_time_sleep_is_reads_clock_blocks_suspends |
| 5621 | Never conflicts | covered | allocates_panics_blocks_never_conflict (new) |
| 5622 | Declared on public functions; private ones infer it | covered | blocks_must_be_declared_on_a_pub_fn, std_time_sleep_is_reads_clock_blocks_suspends |
| 5623 | Forbidden with `#[no_effect(blocks)]` | covered | no_effect_blocks_rejects_a_sleep_below_it |
| 5626 | The library's `sleep`, file reads, `Mutex.lock` and channel operations declare it | covered | blocks_comes_from_mutex_and_channels_not_cpu_loops (new), builtin_resource_verbs (new: file and stdin I/O) |
| 5627 | A foreign function without a clause has `blocks` | covered | extern_fn_with_neither_has_every_effect_blocks |
| 5628 | A CPU loop is not blocking | covered | blocks_comes_from_mutex_and_channels_not_cpu_loops (new) |
| 5632 | `karac explain f` traces `blocks` edge by edge | covered | explain_traces_blocks_edge_by_edge (new) |

## Observability

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5636 | Logging and `dbg()` never change an effect set and never conflict | covered | log_and_dbg_are_transparent, two_par_branches_that_log_do_not_conflict |
| 5636 | `std.log` is declared with no effects | covered | pub_fn_that_only_logs_declares_nothing |
| 5636 | The library's internal attribute cannot be written in user code | covered | observability_attribute_is_reserved_refused (new) |
| 5638 | Observability output from concurrent branches may interleave; ordered output uses `println` | covered | two_par_branches_that_log_do_not_conflict, two_par_branches_that_both_print_conflict |
| 5640 | `dbg()` prints file, line, source and value to stderr and returns the value | covered | tests/effects_conformance.rs (dbg cases) |
| 5640 | `dbg()` is removed from release builds | covered | tests/build_v2.rs build_v2_release_strips_dbg_and_contracts |
| 5642 | In a `par` branch the line carries a task id, numbered from 1 in spawn order | covered | tests/effects_conformance.rs effects_dbg_in_a_branch_carries_its_task, effects_dbg_task_ids_count_from_one_on_every_lane |
| 5650 | `--output=json` / `jsonl`: one object per call, `task_id` null outside | covered | tests/effects_conformance.rs effects_dbg_structured_output_is_one_object_per_call |
| 5656 | A compiled binary reads `KARAC_DBG_OUTPUT` | covered | tests/effects_conformance.rs (dbg lanes) |
| 5657 | A line is written in one `write(2)` | not testable | below `PIPE_BUF`, interleaving cannot be forced |
| 5658 | Snapshot-testing `dbg()` output is unsupported | not testable | advice |

## Function values and effects

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5664 | A closure's effects are what it does when called, not what it captures | covered | closure_s_effects_are_what_it_does_when_called_not |
| 5666 | A function type may carry a clause; kind, `escaping` and clause are independent | covered | fn_type_kind_escaping_and_clause_are_independent (new) |
| 5668 | A value with fewer effects fits a slot expecting more | covered | effect_set_subtyping_reader_writer_and_pure_fit_a_reads |
| 5682 | The reverse is an error | covered | reverse_is_an_error_a_writes_slot_value_into_a |
| 5685 | A non-escaping parameter has the argument's effects, charged to the caller | covered | non_escaping_parameter_s_effects_are_charged_to_the |
| 5685 | With no clause it accepts any value; a clause is an upper bound | covered | each_itself_needs_no_declaration, clause_on_a_non_escaping_parameter_is_an_upper_bound |
| 5686 | An escaping value has its type's clause; storing one that exceeds it is an error | covered | storing_a_value_whose_effects_exceed_the_slot_s_clause, call_through_a_stored_callback_has_its_clause_s |
| 5686 | With no clause, a call through it has every effect | covered | escaping_value_with_no_clause_has_every_effect_pub |
| 5686 | `with ()` accepts only default-permitted effects; a call through it has none | covered | with_accepts_only_default_permitted_effects, with_accepts_an_allocating_panicking_callback, two_par_branches_calling_stored_with_callbacks_do_not; corpus/core/effect_empty_clause_par_apart |
| 5686 | `with ()` is written only on a function type | covered | empty_clause_on_a_fn_declaration_refused (new) |
| 5704 | Default-permitted effects do not constrain a slot | covered | with_accepts_an_allocating_panicking_callback |
| 5704 | A clause may still name them | covered | slot_clause_may_name_default_permitted_effects (new) |
| 5706 | `blocks` is checked in a slot | covered | blocks_is_checked_a_blocking_value_does_not_fit_a_slot |
| 5708 | A collection literal of function values takes the union of their sets | covered | collection_literal_of_fn_values_takes_their_union, collection_literal_of_fn_values_fits_the_union_type + collection_literal_of_fn_values_exceeding_a_narrow_type_refused (new) |

### Effect variables

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5718 | An effect variable names a set the caller supplies | covered | effect_variable_is_solved_at_the_use_and_charged; corpus/core/effect_vars_event_bus |
| 5739 | Declared on functions, methods, impls, structs and enums | covered | tests/core_effect_vars_front_end.rs (an_enum_declares_an_effect_variable, the_spec_event_bus_parses_and_resolves) |
| 5739 | A struct or enum declares at most one; a function several | covered | struct_declares_at_most_one_effect_variable_refused (new); tests/core_effect_vars_front_end.rs a_struct_declares_at_most_one_effect_variable_and_a_function_several |
| 5739 | A trait takes none | covered | trait_takes_no_effect_variable_refused (new) |
| 5740 | Used in a function type's or `dyn` type's clause and the declaring function's clause | covered | effect_variable_is_solved_at_the_use_and_charged; tests/effects_dyn.rs dyn_effect_variable_is_solved_from_the_values_coerced_into_it |
| 5740 | A clause may combine a variable with named effects | covered | effect_variable_combined_with_a_named_effect (new); tests/core_effect_vars_front_end.rs |
| 5741 | Solved at each use from what fills it; several values give the union | covered | tests/core_effect_vars_solving.rs the_construction_joins_the_union_of_its_values |
| 5741 | A value bound later must fit, or an error names the extra effect | covered | corpus/core/err_effect_var_unfit; tests/core_effect_vars_solving.rs a_later_value_within_the_set_fits |
| 5741 | For a type, bindings in one function union in either order | covered | tests/core_effect_vars_solving.rs bindings_in_one_function_union_in_either_order |
| 5741 | A binding in another function must fit the parameter's set | covered | tests/core_effect_vars_solving.rs a_written_type_argument_fixes_the_set |
| 5741 | A variable nothing fills is the empty set | covered | corpus/core/effect_vars_par_apart |
| 5742 | A call is charged the solved set where it was solved | covered | effect_variable_is_solved_at_the_use_and_charged; tests/core_effect_vars_solving.rs the_spec_event_bus_solves_e_and_charges_audit |
| 5743 | The solved set may be written as a type argument and must be in a public signature | covered | corpus/core/err_effect_var_public_clause; tests/core_effect_vars_solving.rs a_public_function_must_declare_the_solved_set |
| 5743 | Building or storing a closure performs none of its effects | covered | tests/core_effect_vars_front_end.rs a_public_signature_spells_the_effect_set_of_a_type_that_declares_one |
| 5744 | Instances are keyed by types only | not testable | no observable difference |
| 5745 | Effect variables and type parameters are kept apart | covered | effect_variable_beside_a_type_parameter (new); tests/core_effect_vars_front_end.rs an_effect_variable_used_as_a_type_is_a_kind_error, a_type_parameter_used_as_an_effect_set_is_a_kind_error |

## Trait methods and generic calls

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5751 | A trait method's clause is a ceiling each impl stays within | covered | impl_method_may_not_exceed_its_trait_method_s_clause, traits/effect_ceiling_exceeded_refused |
| 5751 | A call through a bound has the clause's effects; default-permitted effects add nothing | covered | call_through_a_bound_with_a_clause_is_charged_the; tests/design_effects_probe_rows.rs design_effects_ceiling_admits_default_permitted_effects |
| 5751 | Impls may narrow, never widen | covered | impl_may_do_less_than_the_ceiling_readonlyview_save |
| 5774 | The diagnostic points at the impl method, names the extra effect and quotes the clause | covered | impl_method_may_not_exceed_its_trait_method_s_clause; tests/effects_conformance.rs effects_ceiling_diagnostic_writes_blocks_bare |
| 5776 | No clause: the instance's effects, charged to whoever instantiates | covered | call_through_a_bound_with_no_clause_has_the_instance, pub_generic_fn_through_unclaused_bound_charges_instantiator (new) |
| 5778 | A generic method is charged the instance's impl of each trait its signature bounds | covered | tests/effects_library_method_bounds.rs; par_conflict_via_iter_map_closure_refused, _user_iterator_collect_, _user_iterator_for_, _fold_closure_ |
| 5778 | An adapter is charged through its own impl's bounds; an `impl Trait` result those of the type behind it | covered | tests/effects_library_method_bounds.rs an_opaque_iterator_is_charged_the_next_behind_it; impl_trait_return_s_ceiling_is_enforced_b_2026_11_20 |
| 5780 | A built-in that runs a trait on what it holds has that impl's effects | covered | tests/effects_builtin_element_impls.rs; the `par_conflict_via_*_refused` cells (FAILED at the audit for `sort`, `clone`, `dedup`, `Map.insert`, `Set.contains`: B-2026-11-24-44) |
| 5780 | A derived impl has the effects of its fields' impls | covered | tests/effects_builtin_element_impls.rs a_derived_impl_is_charged_what_its_fields_run |
| 5780 | A program impl is charged only where the operation holds that type | covered | tests/effects_builtin_element_impls.rs a_keyed_collection_is_not_charged_what_it_does_not_run; the `par_apart_via_*` controls |
| 5782 | Default method bodies are checked against their clause | covered | tests/effects_spec_atoms_and_clauses.rs a_private_trait_default_body_is_checked_against_its_clause; par_conflict_via_trait_default_calls_required_refused |
| 5782 | Associated functions follow the same rules | covered | tests/effects_assoc_fn_through_bound.rs |
| 5784 | An orphan impl of a foreign trait is bound by its clauses | covered | modules/orphan_impl_widening_a_foreign_trait_clause_refused + control modules/orphan_impl_within_a_foreign_trait_clause (new) |
| 5786 | Instances are keyed by types only | not testable | as 5744 |
| 5788 | `dyn Trait` calls follow the rules for function values | covered | dyn_value_s_clause_is_a_ceiling_on_the_coerced_type; tests/effects_dyn.rs; corpus/core/effect_dyn_* |
| 5788 | A `with` clause on a trait header (experimental) is the ceiling of each method without one | covered | tests/effectchecker.rs test_impl_trait_ceiling_satisfied (`pub trait Repo with reads(Db)`) |

## Design principles

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 5797 | The substrate set is exactly `{allocates(Heap), panics}` | covered | blocks_must_be_declared_on_a_pub_fn (blocks is not substrate), pub_fn_need_not_declare_allocates_heap_or_panics |
| 5799 | Logging and metrics are transparent; `reads(Db)` / `writes(Db)` are not | covered | as Observability |
