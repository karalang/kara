# The Kāra Book: sentence-to-cell map

Each stated output, error code and behaviour claim of `docs/book/src`, mapped to the cell that checks it. Status: covered, FAILS (a ledger row names it), not testable. Line numbers are the book's (2026-11-25).

A cell is `conformance/book/<chapter>/<name>.kara` and what it is checked against, run by `tests/book_conformance.rs`:

| File | Check |
|---|---|
| `.out` | `karac run --interp` prints exactly this (default leg); `karac run` prints it compiled (`--features llvm` leg) |
| `.exit` | the run's exit code (default 0) |
| `.stderr` | lines that must each appear in the run's stderr |
| `.refused` | `karac check` fails, with each listed code (or message text) among its errors |
| `.warns` | `karac check` passes and warns each listed lint or code |
| `.clean` | `karac check` reports nothing |
| `.explain` | `karac explain main.kara.<first line>` prints each following line |
| `.open` | a ledger row: the cell fails today; once it passes the harness fails until the file is deleted |
| `.args` | extra arguments for the run (`--output=jsonl`) |
| `.has` | lines that must each appear in the run's stdout or stderr |
| `.test` | `karac test` passes, and prints each listed line |

A cell can also be a package: a directory `<name>/` with a `kara.toml`, run in place with no file argument.

A cell is named for the claim it pins, so `ch02/narrowing_needs_as` is the one to read when that sentence changes. `tests/book_snippets.rs` still checks that every `kara` block compiles; this map is about what the text says the code does. A REPL claim is pinned by `tests/repl_v2/book_transcript.rs` and `conformance/repl/`.

**Totals (2026-11-25), all chapters:** 188 claims: 180 covered, 8 not testable. 231 cells, all passing on the interpreter and compiled. B-2026-11-25-60, the one FAILS row, is fixed. Chapters 1 to 7 hold 70 of the claims and 89 of the cells; their one FAILS row, B-2026-11-25-38, is fixed.

**The book changed where it contradicted the spec** (each marked FIXED in its row):
- ch01: `karac run hello.kara  # interpret it` now says it builds and runs the program; design.md §16 leaves the backend to the implementation and names `--interp` for the interpreter.
- ch01b: "the compiler will warn you if you forget a case" now says a non-exhaustive `match` is refused (`E0205`); design.md: a `match` must be exhaustive. "exploring a new crate" now says a library.
- ch02: `HTTPClient` was listed with the compile errors; it is the `acronym_case` warning. design.md CN-4 did not say which, and now says it is a lint (the B-2026-11-16-260 ruling).
- ch03: "`karac fix` removes the redundant `ref`" now says this happens once `redundant_ref_param` is turned on, since style lints are off by default (design.md §7). `char_count` returning `text.len()` is renamed `byte_count`, because `len` counts bytes.
- ch04: "No parentheses around the condition" now says they are not needed but allowed.
- ch05: the linked-list example lost two `0i64`, which warned `redundant_suffix`.

## ch01-hello-kara.md: Hello, Kāra

| Line | Claim | Status | Cells |
|---|---|---|---|
| 34 | The first program prints `Hello, world!` | covered | ch01/hello_world |
| 41 | `karac run` builds and runs it (FIXED: said "interpret it"; design.md §16 `karac run`) | covered | ch01/hello_world (`run` and `run --interp`) |
| 48 | `karac run` and `karac build` print the same thing | covered | every `.out` cell runs on `--interp` and, on the llvm leg, compiled |
| 65 | The bigger program greets Kāra, then world | covered | ch01/greet_twice |
| 74 | The last expression of a block is its value | covered | ch01/last_expression_is_the_value |
| 82 | Block comments nest | covered | ch01/nested_block_comments |
| 89 | `greet` is inferred `writes(Stdout)`; `karac explain main.kara.greet` prints it and where it comes from | covered | ch01/greet_effect_explained (.explain) |
| 90 | A bare parameter borrows; `own` keeps; `mut ref` changes | covered | ch01/bare_param_borrows, ch01/own_param_takes_the_value (E0500) |
| 23 | `karac build` needs a C linker; `karac run` does not | not testable | the container always has one |

## ch01b-two-surfaces.md: Getting Started, Part 2: Two Surfaces

| Line | Claim | Status | Cells |
|---|---|---|---|
| 12 | `search.kara` prints `found at index 3` | covered | ch01b/binary_search_file |
| 48 | A match that forgets a case is refused (FIXED: said "warns") | covered | ch01b/forgotten_case_is_refused (E0205) |
| 78 | The REPL transcript: `Some(3)`, `None`, `Some(2)`, shadowed `nums`, `sum=6, owned still here: 3` | covered | tests/repl_v2/book_transcript.rs, repl/book_two_surfaces |
| 125 | A use after move across cells names both cells; a refused cell changes nothing | covered | tests/repl_v2/book_transcript.rs; repl/panicking_cell_keeps_nothing |
| 149 | `:effects` lists the session and each function | covered | tests/repl_v2/book_transcript.rs (repl_effects_lists_the_session_functions), repl/effects_report |
| 161 | `:save` writes a file that `karac run`s unmodified; a refused cell is left out; a bare expression becomes `let _ = …;`; `:provide` scopes become `with_provider` | covered | tests/repl_v2/book_transcript.rs (repl_save_writes_a_file_that_runs_unmodified), repl/provider_state_persists |

## ch02-variables-and-types.md: Variables and Types

| Line | Claim | Status | Cells |
|---|---|---|---|
| 27 | Reassigning an immutable binding is refused; `let mut` allows it | covered | ch02/immutable_reassign_refused (E0506), ch02/mut_reassign |
| 50 | A literal is checked against its annotated type: `255` fits `u8`, `256` does not | covered | ch02/literal_fits, ch02/literal_overflow_refused (E0200) |
| 59 | Underscores and binary, hex and octal literals | covered | ch02/literal_bases |
| 66 | A literal coerces to its binding, parameter or field type | covered | ch02/literal_coerces_to_context |
| 80 | Suffixes pin `u8`, `f32`, `u64`; the shift happens in `u64` | covered | ch02/literal_suffixes |
| 87 | A suffix that repeats the default warns `redundant_suffix` | covered | ch02/redundant_suffix_warns |
| 98 | A string literal is a `String`; `"""` dedents by the closing delimiter | covered | ch02/multiline_string |
| 115 | `f"{x} + {y} = {x + y}"` prints `10 + 20 = 30` | covered | ch02/interpolation |
| 120 | Widening and ≤32-bit integer to `f64` are implicit; narrowing and `i64` to `f64` need `as` | covered | ch02/widening_is_implicit, ch02/narrowing_needs_as (E0200), ch02/i64_to_f64_needs_as (E0200) |
| 138 | `300 as u8` is 44, `3.9 as i64` is 3, `as f64` before dividing | covered | ch02/as_semantics |
| 148 | `u8 as char` works; a wider integer is refused; `char.try_from` returns a `Result` | covered | ch02/byte_as_char, ch02/wide_int_as_char_refused (E_INT_AS_CHAR), ch02/char_try_from |
| 160 | Shadowing gives 6, then 12 | covered | ch02/shadowing |
| 180 | `fn ReadFile`, `struct user_account`, `const pi` are compile errors; `HTTPClient` warns (FIXED: listed with the errors) | covered | ch02/fn_pascal_refused, ch02/struct_snake_refused, ch02/const_lower_refused (E0001), ch02/acronym_type_warns (acronym_case) |
| 172 | A module-level `let` is an error | covered | ch02/module_let_refused (E_MODULE_LET) |
| 182 | `_tmp` is a name; `_` alone is a wildcard | covered | ch02/leading_underscore_names |
| 192 | Constants take constant expressions; a function call is refused | covered | ch02/module_constants, ch02/const_fn_call_refused (E_MODULE_BINDING_EFFECTFUL_INIT) |

## ch03-functions.md: Functions

| Line | Claim | Status | Cells |
|---|---|---|---|
| 8 | Last expression is the value; `return` exits early | covered | ch03/functions_and_early_return |
| 37 | `if` and `match` are expressions | covered | ch03/if_and_match_are_expressions |
| 66 | Bare borrows, `own` moves (a `Copy` value is copied), `mut ref` writes and needs `mut` at the call | covered | ch03/parameter_modes, ch03/own_moves_the_argument (E0500), ch03/mut_ref_needs_mut_at_call (E0218) |
| 61 | `String.len()` counts bytes (FIXED: the function was named `char_count`) | covered | ch03/parameter_modes (`héllo` is 6) |
| 88 | `ref` on a parameter is redundant; the lint is off until turned on, then `karac fix` removes it (FIXED: said fix always removes it) | covered | ch03/redundant_ref_param (.clean), ch03/redundant_ref_param_lint_on |
| 95 | `Vec` and `Array` coerce to `Slice[T]` | covered | ch03/slice_param_takes_vec_and_array |
| 139 | `self` reads only; `mut ref self` writes; `Circle.new`; UFCS calls are the same | covered | ch03/methods_and_ufcs, ch03/self_method_cannot_write (E0517) |

## ch04-control-flow.md: Control Flow

| Line | Claim | Status | Cells |
|---|---|---|---|
| 8 | `if` is an expression; `else if` chains | covered | ch04/if_is_an_expression |
| 23 | Parentheses are optional, braces required (FIXED wording: parentheses are allowed) | covered | ch04/parenthesized_condition (.clean), ch04/braces_required (E0002) |
| 39 | `while`, `for` over a list, `0..5`, `0..=5` | covered | ch04/loops |
| 79 | `break value` is the loop's value | covered | ch04/loop_break_value |
| 91 | The break/continue loop prints 1, 3, 5, 7, 9 | covered | ch04/break_and_continue |
| 107 | `|` alternatives in a match | covered | ch04/match_or_pattern |
| 116 | `|>` is refused, saying there is none | covered | ch04/pipe_operator_refused |
| 125 | A method chain reads left to right | covered | ch04/method_chain |

## ch05-structs-and-enums.md: Structs and Enums

| Line | Claim | Status | Cells |
|---|---|---|---|
| 15 | The Point program prints `(3.0, 4.0)` | covered | ch05/point |
| 21 | Struct and field names are case-checked | covered | ch02/struct_snake_refused, ch05/field_name_case_refused (E0001) |
| 50 | The Rectangle program prints `Area: 50.0`, `Square? false` | covered | ch05/rectangle |
| 63 | A `distinct type` does not mix with another; `#[derive(Arithmetic)]` restores `+` | covered | ch05/distinct_types_do_not_mix (E0200), ch05/distinct_type_has_no_arithmetic (E0208), ch05/distinct_type_derive_arithmetic |
| 79 | Enums, enums with data, `match` on them | covered | ch05/enums |
| 112 | A program's own `Option` shadows the prelude's, and the compiler warns | covered (B-2026-11-25-38, fixed) | ch05/own_option_shadows_prelude |
| 157 | A `shared` field is read-only unless `mut`; a `mut` field changes for every owner | covered | ch05/shared_field_readonly (E0269), ch05/shared_mut_field |
| 169 | A value struct is changed through a `mut` binding | covered | ch05/value_struct_needs_mut_binding (E0506), ch05/value_struct_mut_binding |
| 185 | The linked list builds, sums and counts, with no warning (FIXED: two redundant `0i64`) | covered | ch05/linked_list, ch05/linked_list_is_clean |

## ch06-pattern-matching.md: Pattern Matching

| Line | Claim | Status | Cells |
|---|---|---|---|
| 11 | `|`, `..=` and `_` | covered | ch06/classify |
| 36 | Destructuring enum variants | covered | ch06/destructure_enum |
| 54 | Destructuring structs, with literal fields | covered | ch06/destructure_struct |
| 75 | `own` lets the match move `name` out; a borrowed one is refused; `ref` or `.clone()` keeps it borrowed | covered | ch06/nested_own, ch06/nested_borrowed_cannot_give_away (E0200), ch06/nested_ref_binding, ch06/nested_borrowed_clone |
| 84 | Guards | covered | ch06/guards, ch06/guard_does_not_count (E0205) |
| 114 | A non-exhaustive match is refused, naming `Blue` | covered | ch06/exhaustiveness (E0205) |
| 126 | `let` patterns and `if let` | covered | ch06/let_and_if_let |

## ch07-error-handling.md: Error Handling

| Line | Claim | Status | Cells |
|---|---|---|---|
| 17 | `?` returns an `Err` at once | covered | ch07/question_mark_result |
| 31 | `?` on `Option` | covered | ch07/question_mark_option |
| 45 | `a?.b?.c` is `?` step by step; the chained-method fallback | covered | ch07/nested_question_marks |
| 73 | `??` falls back on `None` or `Err`, lazily | covered | ch07/default_operator |
| 80 | `unwrap` on `None` panics | covered | ch07/unwrap_panics (exit 101) |
| 83 | `panics` is inferred and `karac explain` shows where; a public function need not declare it; `#[no_effect(panics)]` refuses it | covered | ch07/unwrap_panics_explained (.explain, .clean), ch07/no_effect_panics_refuses_unwrap (E_NO_EFFECT_VIOLATED) |
| 101 | `defer` always runs, `errdefer` only on `Err` (also through `?`), last declared first | covered | ch07/defer_order, ch07/defer_on_question_mark |
| 120 | A discarded `Result` warns `must_use` | covered | ch07/ignored_result |

**Chapters 0 and 8 to 20 and the appendices: the book changed where it contradicted the spec** (each marked FIXED in its row):
- ch00: "owned by default" now says parameters borrow by default (design.md D5).
- ch05: the `shared struct` / `sync` wording follows core-semantics §6.
- ch09: a redundant `0i64` (it warned `redundant_suffix`), and the grid prose named a `ref Vec[Vec[i64]]` parameter the code no longer wrote.
- ch09b: `s[a..b]` is a `Str` view, not a copy (design.md §5); `substring` is the copy, and it saturates. `s.slice(a, b)` is gone from the text (it is not in library/strings.md; B-2026-11-25-63).
- ch11: the six names are conflict verbs, not "resource verbs"; the capabilities are separate (design.md § Effect verbs).
- ch12: tier 3 is "Shared", declared by the type; the "RC fallback" section, which said the compiler adds RC on its own, is now "No hidden sharing" (core-semantics §6.4). Every argument to a `mut ref` parameter is marked `mut`, a forwarded one included. The summary no longer points at `cost-summary` for RC operations.
- ch13: a `tests/` root became a colocated `_test.kara` file (design.md §4: there is no `tests/` root).
- ch14: the E0418 hint is quoted as the compiler gives it.
- ch18: calls into an `unsafe extern` block need no `unsafe` (design.md § unsafe extern blocks); only the block carries the `# Safety` comment.
- ch19: `ct_eq` works on `Vec[u8]`, and zeroization is in (B-2026-11-16-192).
- Appendix A: `suspends` is a contextual keyword, not deferred; design.md's keyword table had the same slip and is fixed (the 2026-11-16 ruling).
- Appendix B: integer overflow traps in every profile; `??` takes `Ok` as well as `Some`; `?.` is `?` then `.`, not a rejected operator; `{x:?}` is reserved, not working.
- Appendix C: the compiler does not derive traits on its own (the implied derives are deprecated, design.md § Derive); an enum's `Default` needs `#[default]` on a variant, not the first variant (design.md § Default).
- Appendix D: rewritten to design.md's attribute list. Attribute paths are one segment, with no tool namespaces and no `@name` shorthand; the lint names are real ones; `repr(packed)` / `repr(align)`, the linker attributes and `#[profile]` are deferred. B-2026-11-25-61 lists what the compiler still accepts beyond the list.

## ch00-introduction.md: The Kāra Programming Language

| Line | Claim | Status | Cells |
|---|---|---|---|
| 16 | Parameters borrow by default; `own` and `mut ref` are written (FIXED: said "owned by default") | covered | ch01/bare_param_borrows, ch12/parameter_modes |
| 7 | The compiler parallelizes independent work without annotations | not testable | automatic parallelism changes nothing observable (design.md §13); its own suite is tests/auto_par*.rs |

## ch08-traits-and-generics.md: Traits and Generics

| Line | Claim | Status | Cells |
|---|---|---|---|
| 25 | Traits and `impl Trait for Type`; a bound calls the trait method | covered | ch08/traits_and_impls |
| 52 | A default method comes for free and can be overridden; a missing required method is refused | covered | ch08/default_methods, ch08/missing_required_method_refused (E0203; its extra E0236 is B-2026-11-25-62) |
| 64 | A generic function and a generic struct work for any type | covered | ch08/generics |
| 93 | Bounds with `+`; `items[0]` borrows, so a non-`Copy` element is cloned, not moved | covered | ch08/trait_bounds, ch08/index_cannot_move_out (E_INDEX_MOVE_NON_COPY), ch08/bound_missing_warns (deprecated in 0.8.x) |
| 114 | A call takes no type arguments | covered | ch08/no_turbofish |
| 139 | `find(names, "Bob")` prints `Found at index 1` | covered | ch08/find_generic |

## ch09-collections.md: Collections

| Line | Claim | Status | Cells |
|---|---|---|---|
| 9 | `[..]` is a `Vec`; the expected type builds an `Array` or a `Set`; `[k: v]` is a `Map`; `Set.from` | covered | ch09/literals |
| 22 | The prefix form `Set[1, 2, 3]` warns and `karac fix` rewrites it | covered | ch09/prefix_literal_deprecated (fix checked by hand: `Set.from([1, 2, 3])`) |
| 41 | `Vec` push, iterate, index, `len` | covered | ch09/vec_basics |
| 65 | `with_capacity` never changes output | covered | ch09/with_capacity_is_a_hint |
| 71 | The capacity speedups and the automatic pre-sizing | not testable | performance claims; no output changes |
| 98 | `[0; 4]` is an `Array`; the size is part of the type | covered | ch09/arrays, ch09/array_size_is_the_type (E0200) |
| 121 | `Map` insert and get; `Set` keeps one of each | covered | ch09/map_and_set |
| 148 | Tuples; `min`/`max` return `Option` | covered | ch09/tuples |
| 168 | Grids: sizes, cell reads and writes, independent `Vec.filled` rows, borrowing a row by `ref`, jagged rows | covered | ch09/grids, ch09/row_by_value (E_INDEX_MOVE_NON_COPY), ch09/filled_suffix_warns (FIXED: the book's `0i64` warned) |
| 214 | The grid parameter borrows (FIXED: said `ref Vec[Vec[i64]]`, which the code no longer wrote) | covered | ch09/grids |

## ch09b-strings-and-bytes.md: Strings and Bytes

| Line | Claim | Status | Cells |
|---|---|---|---|
| 13 | `s[i]` is refused | covered | ch09b/no_scalar_index (E0268) |
| 29 | `s[a..b]` is a `Str` view in bytes; `.to_string()` or `substring` copies, and `substring` saturates (FIXED: said `s[a..b]` copies into a `String`, same as `substring`) | covered | ch09b/range_index_is_a_view, ch09b/range_view_is_read_only (E0236) |
| 57 | A cut inside a character, or past the end, panics | covered | ch09b/range_mid_char_panics, ch09b/range_out_of_bounds_panics |
| 67 | The character view; `char_at` is an `Option` | covered | ch09b/char_view |
| 113 | The byte view and byte literals | covered | ch09b/byte_view |
| 138 | Roman numerals: `MCMXCIV` is 1994; a literal passes straight in | covered | ch09b/roman |
| 168 | Byte match arms, building with `push`/`push_str`, `reverse`, `char.try_from` | covered | ch09b/byte_match_and_building |
| 228 | The which-view table (FIXED: `s.slice(a, b)` is not in library/strings.md; B-2026-11-25-63) | covered | ch09b/range_index_is_a_view |

## ch10-closures-and-iterators.md: Closures and Iterators

| Line | Claim | Status | Cells |
|---|---|---|---|
| 9 | Closures, captures, closure parameters: 5, true, 12 | covered | ch10/closures |
| 16 | A closure with no context needs a parameter type; a later call is context | covered | ch10/closure_without_context_refused (E0238), ch10/unannotated_closure |
| 33 | A closure's effects reach the enclosing function; a public one must declare them | covered | ch10/closure_effects (.explain), ch10/public_closure_effect_must_be_declared (E0400) |
| 38 | `sort`, `sort_by` ascending and descending, by a key; `sort` needs `mut` | covered | ch10/sorting, ch10/sort_needs_mut (E0510) |
| 75 | The iterator chain gives `[6, 8, 10]`; the common adapters | covered | ch10/iterators |

## ch11-effects.md: The Effect System

| Line | Claim | Status | Cells |
|---|---|---|---|
| 24 | The conflict verbs and the capabilities (FIXED: called all six "resource verbs" that answer whether operations conflict; design.md § Effect verbs) | covered | ch11/allocates_is_default_permitted |
| 26 | User resources; one verb over several resources | covered | ch11/user_resources, ch11/several_resources_one_verb (.clean) |
| 51 | Execution verbs are declared on public functions | covered | ch11/public_must_declare_blocks (E0400), ch11/public_declares_blocks |
| 61 | Private effects are inferred | covered | ch11/inferred_private_effects (.explain) |
| 88 | A public body may not exceed its `with` clause | covered | ch11/declared_effects_bound_the_body (E0400) |
| 113 | Non-conflicting calls run in parallel | not testable | no observable change by design (§13) |
| 121 | Effect groups are refused | covered | ch11/effect_group_refused |

## ch12-ownership.md: Ownership Without the Fight

| Line | Claim | Status | Cells |
|---|---|---|---|
| 13 | Sharing is declared; the compiler never adds it (FIXED: said the compiler adds RC automatically; core-semantics §6.4) | covered | ch12/no_implicit_sharing (E0500), ch12/shared_tree |
| 35 | The three modes; consuming a bare or `mut ref` parameter is refused | covered | ch12/parameter_modes, ch12/consuming_borrowed_param_refused, ch12/consuming_mut_ref_refused (E0200) |
| 39 | Receivers: `own self` consumes, `self` reads, `mut ref self` writes | covered | ch12/receivers, ch12/own_self_consumes (E0500) |
| 49 | A redundant `ref` (FIXED, as in ch03) | covered | ch03/redundant_ref_param, ch03/redundant_ref_param_lint_on |
| 53 | `karac query ownership` reports the modes | covered | checked by hand (`{"function":"add_suffix","parameters":[{"name":"name","mode":"mut_ref"...}]}`); tests/query_*.rs |
| 68 | Every argument to a `mut ref` parameter is marked, a forwarded one included; methods and assignments are not; `f(ref v)` is refused | covered | ch12/mut_marker_through_mut_ref, ch12/mut_marker_missing_through_mut_ref (E0218), ch12/ref_at_call_site_refused |
| 90 | A moved binding is gone | covered | ch12/move_semantics (E0500), ch12/move_then_use_new_owner |
| 96 | A second owner is refused, not turned into RC (FIXED: the "RC fallback" section; core-semantics §6.4 removed it) | covered | ch12/no_implicit_sharing (E0500) |
| 129 | `Vec`, `Array` and a sub-range pass as `Slice`; `as_slice` for a value; `mut Slice` sorts in place, whole or a range | covered | ch12/slices |
| 169 | The philosophy bullets (FIXED: said forwarded `mut ref`s stay unmarked, and pointed at `cost-summary` for RC operations) | covered | ch12/mut_marker_through_mut_ref |

## ch13-modules.md: Modules and Visibility

| Line | Claim | Status | Cells |
|---|---|---|---|
| 19 | A module directory must be snake_case | covered (B-2026-11-25-60, fixed) | ch13/module_name_case |
| 27 | Default visibility reaches the project; `private` stays in the directory | covered | ch13/visibility, ch13/private_is_directory_only (E0111) |
| 55 | Imports, renames, grouped imports | covered | ch13/visibility, ch13/reexport, ch13/prelude |
| 74 | `pub import` re-exports, same type | covered | ch13/reexport |
| 82 | The prelude names need no import | covered | ch13/prelude |
| 95 | Tests sit beside the code (FIXED: showed a `tests/` root, which design.md §4 says does not exist) | covered | ch16/colocated_tests |
| 109 | Dependencies in `kara.toml` | not testable | needs a registry or network; tests/package_*.rs |

## ch14-concurrency.md: Concurrency

| Line | Claim | Status | Cells |
|---|---|---|---|
| 20 | Automatic parallelization | not testable | no observable change by design |
| 44 | The `par` counter prints 3 | covered | ch14/par_counter |
| 48 | A plain write shared by two branches is refused (FIXED: said the error suggests Atomic, Mutex or sync) | covered | ch14/par_plain_write_refused (E0418) |
| 71 | `TaskGroup` spawn and join give 30 | covered | ch14/task_group |
| 85 | A `par` with `?` returns the source-earliest error | covered | ch14/par_source_earliest_error |
| 88 | A panic in a branch ends the process with 101 and runs no `defer` | covered | ch14/par_panic_exits_101 |
| 91 | Without `?`, every result comes back in source order | covered | ch14/par_results_in_source_order |

## ch15-data-layout.md: Data Layout

| Line | Claim | Status | Cells |
|---|---|---|---|
| 26 | A `layout` block is refused | covered | ch15/layout_block_refused |
| 37 | The struct of arrays works | covered | ch15/struct_of_arrays |

## ch16-testing.md: Testing

| Line | Claim | Status | Cells |
|---|---|---|---|
| 7 | `test "name"` blocks in `_test.kara` files run; the test file sees `private` items | covered | ch16/colocated_tests (.test) |
| 37 | `assert_eq` shows both values | covered | ch16/assert_eq_shows_both |
| 48 | The filter is a substring of the case name | covered | ch16/test_filter (.test) |

## ch17-ssr.md: Server-Side Rendering

| Line | Claim | Status | Cells |
|---|---|---|---|
| 43 | The server half renders the counter as HTML through its provider | covered | ch17/server_render |
| 136 | A resource no native provider serves is refused, naming the call chain | covered | ch17/native_without_provider (E0411) |
| 18 | The browser half | not testable | deferred past 0.8, as the chapter says |

## ch18-interop.md: FFI and Interop

| Line | Claim | Status | Cells |
|---|---|---|---|
| 22 | The libc program prints 42 and 17, with no warning | covered | ch18/call_c, ch18/call_c_is_clean |
| 33 | A call to an imported function needs no `unsafe` (FIXED: said every call site is `unsafe`; design.md § unsafe extern blocks) | covered | ch18/extern_call_needs_no_unsafe, ch18/undocumented_unsafe_warns |
| 29 | `#[repr(C)]` results and libm imports | covered | ch18/repr_c_struct |
| 35 | An extern fn declares its effects | covered | ch18/extern_puts_effect |
| 39 | Exporting is refused in 0.8 | covered | ch18/export_refused (E_ARCHIVED) |

## ch19-secrets.md: Handling Secrets

| Line | Claim | Status | Cells |
|---|---|---|---|
| 9 | `Secret` needs its import | covered | ch19/not_in_prelude (E0100) |
| 37 | `.expose()` reads it | covered | ch19/expose |
| 44 | `==` is refused; a `Display` impl is refused; printing is refused | covered | ch19/eq_refused (E0208), ch19/impl_display_refused (E_SECRET_TRAIT_FORBIDDEN), ch19/println_refused (E0232) |
| 80 | A derived `Display` shows `<redacted>`, also through `to_string`, f-strings and nesting | covered | ch19/redacted_in_struct, ch19/redacted_everywhere |
| 112 | `ct_eq` compares, on `String` and `Vec[u8]` secrets (FIXED: said bytes were not covered yet) | covered | ch19/ct_eq, ch19/ct_eq_bytes |
| 133 | A dropped secret is wiped (FIXED: said not complete; B-2026-11-16-192 landed it) | not testable | not observable from a program; B-2026-11-16-192's own test |

## ch20-debugging-crashes.md: The 3am Runbook: Debugging Crashes

| Line | Claim | Status | Cells |
|---|---|---|---|
| 13 | A panic exits 101 and runs no `defer`; an error from `main` exits 1 | covered | ch14/par_panic_exits_101, ch20/main_error_exits_1 |
| 40 | The index panic record | covered | ch20/index_panic |
| 83 | The `unwrap` record and its error-return trace | covered | ch20/unwrap_trace |
| 115 | A panic in a `par` branch | covered | ch20/par_branch_panic |
| 140 | The JSONL events | covered | ch20/unwrap_trace_jsonl |
| 174 | The panic handler runs after the record; a panic inside it ends the process | covered | ch20/panic_handler, ch20/panic_in_handler |
| 186 | A shared borrow conflict records kind `borrow_conflict` | covered (B-2026-11-25-25, fixed) | ch20/borrow_conflict_kind |

## appendix-a-keywords.md: Appendix A: Keywords

| Line | Claim | Status | Cells |
|---|---|---|---|
| 74 | Contextual keywords are ordinary names elsewhere (FIXED: `suspends` was listed as deferred; design.md had the same slip, now fixed) | covered | appendix_a/contextual_words_are_names |
| 100 | Reserved words are refused; `r#` escapes them; keywords work as member names | covered | appendix_a/reserved_word_refused, appendix_a/raw_identifiers, appendix_a/move_keyword_refused (E_MOVE_NOT_USED), appendix_a/priv_cannot_be_escaped |
| 36 | `defer` runs on every exit, `errdefer` on `Err` or `None` | covered | appendix_a/defer_on_every_exit, ch07/defer_order |

## appendix-b-operators.md: Appendix B: Operators and Symbols

| Line | Claim | Status | Cells |
|---|---|---|---|
| 11 | The arithmetic, bitwise, comparison, logical and compound assignment operators | covered | appendix_b/arithmetic_bitwise_compare |
| 15 | Overflow traps in every build; `wrapping_*` wraps (FIXED: said only in `app` and `lib` profiles) | covered | appendix_b/overflow_traps, appendix_b/arithmetic_bitwise_compare |
| 50 | `&&`, `||`, `!` are refused | covered | appendix_a/symbol_logic_ops_refused |
| 72 | Ranges, `..base`, `??`, `@`, `as` | covered | appendix_b/ranges_and_symbols |
| 93 | `|>` and `::` are refused (FIXED: listed `?.` as rejected; it is `?` then `.`) | covered | ch04/pipe_operator_refused, appendix_b/double_colon_refused, ch07/nested_question_marks |
| 123 | `{x:?}` is reserved (FIXED: listed as working) | covered | appendix_b/debug_format_not_yet |
| 115 | The suffixes; a literal takes its sibling's type | covered | appendix_b/literal_suffixes |

## appendix-c-derivable-traits.md: Appendix C: Derivable Traits

| Line | Claim | Status | Cells |
|---|---|---|---|
| 6 | Derived equality, hashing and `Display` | covered | appendix_c/derive_basic |
| 15 | A missing implied derive warns in 0.8.x (FIXED: said the compiler derives them itself) | covered | appendix_c/implied_derive_is_deprecated, appendix_c/copy_implied_clone_deprecated |
| 13 | An `f64` field rules out `Eq` | covered | appendix_c/float_field_no_eq (E0200), appendix_c/float_field_partial |
| 41 | `Ord` is lexicographic | covered | appendix_c/ord_lexicographic |
| 64 | Enum `Display`, `snake_case`, data in parentheses | covered | appendix_c/enum_display |
| 85 | `Default` on structs and enums (FIXED: said the first variant) | covered | appendix_c/defaults, appendix_c/enum_default_needs_marker |
| 116 | `Copy` needs `Copy` fields and `Clone` | covered | appendix_c/copy_derives_clone, appendix_c/copy_needs_copy_fields (E0200) |
| 106 | Cloning a `shared` value gives another handle | covered | appendix_c/shared_clone_is_a_handle |
| 131 | `Arithmetic` on a distinct type only | covered | appendix_c/arithmetic_distinct, appendix_c/arithmetic_only_distinct (E0200) |

## appendix-d-attributes.md: Appendix D: Attributes

| Line | Claim | Status | Cells |
|---|---|---|---|
| 14 | An unknown attribute or a tool namespace is refused (FIXED: the appendix documented tool namespaces and the `@name` shorthand; B-2026-11-25-61 for what the compiler still accepts) | covered | appendix_d/tool_namespace_refused, appendix_d/unknown_attribute_refused |
| 52 | Lint levels, file-level `#![...]`, `expect` | covered | appendix_d/lint_levels, appendix_d/expect_unfulfilled |
| 90 | `packed` and `align` are deferred (FIXED: documented as working) | covered | appendix_d/repr_packed_deferred |
| 107 | `kara_name` | covered | appendix_d/kara_name |
| 119 | `#[cfg]` is refused, naming `#[target]` | covered | appendix_d/cfg_refused |
| 131 | `no_effect` | covered | appendix_d/no_effect_mix, appendix_d/no_effect_heap, appendix_d/no_effect_on_struct_refused, ch07/no_effect_panics_refuses_unwrap |
| 142 | `#[must_use]` on a type warns, with its reason as a note | covered | appendix_d/must_use_type |
| 153 | `#[track_caller]` | covered | appendix_d/track_caller |
| 161 | `#[deprecated]` | covered | appendix_d/deprecated_item |
| 171 | `#[test]` on a fn is refused; the test-block attributes | covered | appendix_d/test_attr_on_fn_refused; test attributes in tests/test_runner*.rs |
| 205 | The `diagnostic::` namespace shapes a bound error; an unknown member is silent | covered | appendix_d/diagnostic_namespace |
