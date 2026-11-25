# design.md §14 Testing sentence-to-cell map (lines 6123–6209)

One row per sentence of design.md §14 Testing, as of 2026-11-25, naming the cell in this directory (or the test elsewhere) that covers it. NEW marks a cell added by this audit; FAILS marks what is not yet true of the compiler, with its ledger row; FIXED marks a gap this audit fixed. Warn-only behaviour (a lint that warns and lets the program build) is mapped to its test, since a cases.py cell cannot assert a warning.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| s14.1 | 14. Testing | Tests live in `_test.kara` files and run with `karac test`. | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.2 | 14. Testing | `karac build` ignores these files. | NEW build_ignores_test_files |
| s14.3 | 14. Testing | `karac test` runs in a package: it finds the `kara.toml` from the working directory, as `karac build` with no path does, and refuses outside | NEW test_outside_a_package_refused (every other cell runs in a package) |
| s14.4 | Test blocks | A test case is a block at module scope: | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.5 | Test blocks | `test "case name" { body }`. | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.6 | Test blocks | ```kara // math.kara fn add(a: i64, b: i64) -> i64 { a + b }  // math_test.kara (same directory, so same module, with access to private item | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.7 | Test blocks | **Discovery is structural, not by name.** `karac test` collects the `test "..." { }` blocks in `_test.kara` files. | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.8 | Test blocks | Free functions in those files are helpers, whatever they are named: | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.9 | Test blocks | `fn test_helper()` and `fn make_pair()` are both helpers. | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.10 | Test blocks | The block form is the only thing that makes a test a test. | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.11 | Test blocks | Discovery by a name prefix (`test_`) fails silently in two ways: a test is skipped when the prefix is misspelled or forgotten, and an unrela | rationale |
| s14.12 | Test blocks | The block form fails loudly instead: | heading/intro |
| s14.13 | Test blocks | - a `test "..." { }` inside a function body is `E_TEST_BLOCK_NOT_TOP_LEVEL`; | NEW a_test_block_inside_a_function_refused |
| s14.14 | Test blocks | - a misspelled keyword (`tset "..." { }`) is a parse error, not a missing test; | NEW a_misspelled_test_keyword_is_a_parse_error |
| s14.15 | Test blocks | - `pub` or `private` on a test case is a parse error. | NEW pub_or_private_on_a_test_case_is_a_parse_error |
| s14.16 | Test blocks | A test case is not callable and has no identity outside its module. | NEW a_test_case_is_not_callable |
| s14.17 | Test blocks | **Identity and names.** A test case's source line identifies it within its module, so case names need not be unique. | NEW discovery_is_structural_helpers_named_test_never_run (two cases named "dup" both run), NEW filtering_keeps_matching_names_and_counts_only_them |
| s14.18 | Test blocks | The case name is the string between `test` and `{`. | NEW discovery_is_structural_helpers_named_test_never_run (two cases named "dup" both run), NEW filtering_keeps_matching_names_and_counts_only_them |
| s14.19 | Test blocks | It is what appears in every runner event and what filtering matches. | NEW discovery_is_structural_helpers_named_test_never_run (two cases named "dup" both run), NEW filtering_keeps_matching_names_and_counts_only_them |
| s14.20 | Test blocks | **A test file is part of its module.** `math_test.kara` beside `math.kara` is in the same module and can use its private items ([§4 Modules] | NEW discovery_is_structural_helpers_named_test_never_run (math_test.kara calls the private `hidden`) |
| s14.21 | Assertions | `assert`, `assert_eq` and `assert_ne` are prelude functions. | NEW assertions_work_in_production_code |
| s14.22 | Assertions | They need no import and may be used everywhere, production code included: | NEW assertions_work_in_production_code |
| s14.23 | Assertions | / Function / Signature / Failure message / /---/---/---/ | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.24 | Assertions | / `assert(cond)` / `fn assert(cond: bool) with panics` / `"assertion failed: <source text of cond>"`; the compiler captures the source expre | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.25 | Assertions | / `assert_eq(a, b)` / `fn assert_eq[T: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.26 | Assertions | PartialEq + Debug](a: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.27 | Assertions | T, b: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.28 | Assertions | T) with panics` / `"assertion failed: a == b\n  left:  <a>\n  right: <b>"` / | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.29 | Assertions | / `assert_ne(a, b)` / `fn assert_ne[T: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.30 | Assertions | PartialEq + Debug](a: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.31 | Assertions | T, b: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.32 | Assertions | T) with panics` / `"assertion failed: a != b\n  value: <a>"` / | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.33 | Assertions | - `assert` takes a `bool` and has no bound; any other condition is a type error. | NEW assert_on_a_non_bool_refused |
| s14.34 | Assertions | `assert_eq` and `assert_ne` need `PartialEq` to compare and `Debug` to print the operands ([§9 `Display` and `Debug`](#display-and-debug)),  | NEW assert_eq_on_two_incomparable_types_refused |
| s14.35 | Assertions | They borrow both operands, so neither moves: after `assert_eq(v, expected)`, `v` is still usable. | NEW assert_eq_borrows_both_operands |
| s14.36 | Assertions | - Each takes an optional last argument, a `String` or `Str` message, as Rust's assertions do: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.37 | Assertions | `assert(cond, msg)`, `assert_eq(a, b, msg)`, `assert_ne(a, b, msg)`. | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.38 | Assertions | It is evaluated only when the assertion fails, and it replaces the failure message's first line, the `assertion failed: ...` text; `assert_e | NEW the_message_is_evaluated_only_when_the_assertion_fails, NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.39 | Assertions | Any other number of arguments is an error. | NEW assert_with_three_arguments_refused |
| s14.40 | Assertions | - A failed assertion panics ([§10 Panics](#panics)). | NEW assertions_work_in_production_code (exit 101), NEW each_test_runs_alone_panic_kinds_and_isolation |
| s14.41 | Isolation | **Each test runs in its own process** ([core-semantics.md §10.4](core-semantics.md#10-panics-and-errors-c8)). | NEW each_test_runs_alone_panic_kinds_and_isolation (every cell here runs on the JIT and under --interp) |
| s14.42 | Isolation | A panic ends the process ([core-semantics.md §10.2](core-semantics.md#10-panics-and-errors-c8)), so a panicking test, including one whose as | NEW each_test_runs_alone_panic_kinds_and_isolation (every cell here runs on the JIT and under --interp) |
| s14.43 | Isolation | No state carries from one test to the next. | NEW each_test_runs_alone_panic_kinds_and_isolation (every cell here runs on the JIT and under --interp) |
| s14.44 | Isolation | On the JIT, `karac test`'s default, each case is a `karac_jit_runner` process of its own. | NEW each_test_runs_alone_panic_kinds_and_isolation (every cell here runs on the JIT and under --interp) |
| s14.45 | Isolation | Under `--interp` each case is a fresh run of the MIR interpreter inside `karac`, with the same outcome: a panic fails that case alone. | NEW each_test_runs_alone_panic_kinds_and_isolation (every cell here runs on the JIT and under --interp) |
| s14.46 | Test attributes | Three attributes go on a test case (review-thread ruling 2026-11-16): | heading/intro |
| s14.47 | Test attributes | - **`#[with_provider(R, e)]`** runs the body inside `with_provider[R](p, // body)`, with `p` evaluated afresh for each case ([core-semantics | NEW with_provider_runs_the_body_under_the_fake |
| s14.48 | Test attributes | Several stack, outermost first. | NEW stacked_with_provider_attributes_apply_outermost_first |
| s14.49 | Test attributes | If `e` panics or evaluates to `Err`, the case fails before its body runs, with reason `provider_construction_failed`. | NEW with_provider_runs_the_body_under_the_fake (ctor err, ctor panics) |
| s14.50 | Test attributes | - **`#[test(requires: [R, ..])]`** names program-rooted resources the case uses for real ([core-semantics.md §14.13](core-semantics.md#14-pr | NEW an_unmet_requires_skips_the_case |
| s14.51 | Test attributes | A resource is available when `KARA_RESOURCE_<NAME>` is set and not empty, `<NAME>` being its path with `.` as `_`, upper-cased (`db.UserDB`  | NEW kara_resource_env_makes_a_required_case_run, NEW an_empty_kara_resource_variable_is_unset, NEW a_passing_health_check_makes_the_resource_available, NEW a_failing_health_check_skips_the_case (a dotted path's name is tests/test_attr_requires_rooted.rs) |
| s14.52 | Test attributes | An unavailable one skips the case with reason `unsatisfied_requires`; `karac test --all` fails it instead. | NEW an_unmet_requires_skips_the_case, NEW all_fails_an_unmet_requires |
| s14.53 | Test attributes | A resource in both `requires` and a `#[with_provider]` of the same case fails it with reason `requires_and_with_provider_conflict`. | NEW requires_and_with_provider_on_one_resource_conflict |
| s14.54 | Test attributes | - **`#[test(timeout_seconds: | NEW timeout_attribute_kills_the_case_with_a_test_timeout_event |
| s14.55 | Test attributes | N)]`** bounds the case's run. | NEW timeout_attribute_kills_the_case_with_a_test_timeout_event |
| s14.56 | Test attributes | Without it the limit is `kara.toml`'s `[test] timeout_seconds`, else `KARAC_TEST_TIMEOUT_SECS`, else 30 seconds. | NEW the_timeout_attribute_beats_kara_toml, NEW kara_toml_timeout_beats_the_env_variable, NEW the_env_timeout_applies_without_an_attribute (the 30 s default is not a cell) |
| s14.57 | Test attributes | A case that runs out is killed and reported by a `test_timeout` event. | NEW timeout_attribute_kills_the_case_with_a_test_timeout_event |
| s14.58 | Runner output | `karac test` writes **JSONL to stdout**: one JSON object per line, one event per object. | NEW discovery_is_structural_helpers_named_test_never_run (every .err line is one JSON object per line) |
| s14.59 | Runner output | Each line has a `"type"` field naming the event, with the same envelope as the compiler's streaming events ([§17 Tooling contract](#17-tooli | NEW discovery_is_structural_helpers_named_test_never_run (every .err line is one JSON object per line) |
| s14.60 | Runner output | This is a stable public contract: | rule |
| s14.61 | Runner output | CI dashboards, editor plugins, AI tooling and flaky-test trackers depend on it. | rule |
| s14.62 | Runner output | A text mode may come later as `--output=text`; JSONL is the default. | rule |
| s14.63 | Runner output | / Event / Required fields / Notes / /---/---/---/ | heading/intro |
| s14.64 | Runner output | / `run_start` / `total_tests: int` / Once, at the start of the run, after discovery. / | NEW discovery_is_structural_helpers_named_test_never_run (run_start first, total_tests) |
| s14.65 | Runner output | / `test_pass` / `test: string`, `duration_ms: int` / `test` is the case name, verbatim. / | NEW discovery_is_structural_helpers_named_test_never_run |
| s14.66 | Runner output | / `test_fail` / `test: string`, `duration_ms: int`, `location: {file, line, col}`, `message: string` / Optional: | NEW assertion_failures_carry_message_assertion_left_right_kind |
| s14.67 | Runner output | `assertion: string` (the assertion's source text); `left: string` and `right: string` for `assert_eq` and `assert_ne`; `reason: string` for  | NEW assertion_failures_carry_message_assertion_left_right_kind, NEW each_test_runs_alone_panic_kinds_and_isolation (kind) |
| s14.68 | Runner output | A failure before the body ran has `duration_ms: 0`, a `location` at the case's `test` keyword, and a `reason`: | NEW with_provider_runs_the_body_under_the_fake (duration_ms 0, location at the `test` line, resource), NEW all_fails_an_unmet_requires, NEW requires_and_with_provider_on_one_resource_conflict (resources) |
| s14.69 | Runner output | `provider_construction_failed` with `resource: string`, or `unsatisfied_requires` (under `--all`) or `requires_and_with_provider_conflict` w | NEW with_provider_runs_the_body_under_the_fake (duration_ms 0, location at the `test` line, resource), NEW all_fails_an_unmet_requires, NEW requires_and_with_provider_on_one_resource_conflict (resources) |
| s14.70 | Runner output | / `test_skip` / `test: string`, `reason: string` / A case skipped before it ran: today `unsatisfied_requires`, with `resources: [string]`. | NEW an_unmet_requires_skips_the_case |
| s14.71 | Runner output | Consumers must accept `reason` values they do not know. / | NEW an_unmet_requires_skips_the_case |
| s14.72 | Runner output | / `test_timeout` / `test: string`, `timeout_s: int`, `elapsed_ms: int` / A case killed at its time limit ([Test attributes](#test-attributes | NEW timeout_attribute_kills_the_case_with_a_test_timeout_event (counts as failed) |
| s14.73 | Runner output | It counts as failed in `summary`. / | NEW timeout_attribute_kills_the_case_with_a_test_timeout_event (counts as failed) |
| s14.74 | Runner output | / `summary` / `total: int`, `passed: int`, `failed: int`, `skipped: int`, `duration_ms: int` / Once, at the end of the run. / | NEW discovery_is_structural_helpers_named_test_never_run, NEW a_filter_matching_nothing_runs_nothing_and_exits_0 |
| s14.75 | Runner output | **Compatibility.** Later versions may add fields to an event, add event types, and add `test_skip` reasons. | rule (compatibility contract) |
| s14.76 | Runner output | Consumers **must** ignore unknown fields and event types, and **must not** depend on the order of fields within a line. | rule (compatibility contract) |
| s14.77 | Runner output | Field names and meanings do not change once shipped; a rename or a change of meaning is a breaking schema version. | rule (compatibility contract) |
| s14.78 | Runner output | **Exit code.** `0` if every test passed or was skipped. | NEW discovery_is_structural_helpers_named_test_never_run, NEW an_unmet_requires_skips_the_case (only skips: exit 0) |
| s14.79 | Runner output | Non-zero if any `test_fail` or `test_timeout` was emitted. | NEW each_test_runs_alone_panic_kinds_and_isolation, NEW timeout_attribute_kills_the_case_with_a_test_timeout_event |
| s14.80 | Runner output | **Filtering.** `karac test <substring>` runs only the cases whose name contains `<substring>`. | NEW filtering_keeps_matching_names_and_counts_only_them, NEW a_filter_matching_nothing_runs_nothing_and_exits_0 |
| s14.81 | Runner output | Filtering happens after discovery. | NEW filtering_keeps_matching_names_and_counts_only_them, NEW a_filter_matching_nothing_runs_nothing_and_exits_0 |
| s14.82 | Runner output | Filtered-out tests produce no events (not even `test_skip`), and `run_start`'s `total_tests` counts only the tests that remain. | NEW filtering_keeps_matching_names_and_counts_only_them, NEW a_filter_matching_nothing_runs_nothing_and_exits_0 |
| s14.83 | Runner output | Property tests, snapshot tests, fuzzing and benchmarks ([deferred.md](deferred.md#tooling)), and a test that passes only by panicking (`shou | deferred.md |
| s14.84 | Runner output | --- | heading/intro |
