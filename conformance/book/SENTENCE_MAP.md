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

A cell is named for the claim it pins, so `ch02/narrowing_needs_as` is the one to read when that sentence changes. `tests/book_snippets.rs` still checks that every `kara` block compiles; this map is about what the text says the code does. A REPL claim is pinned by `tests/repl_v2/book_transcript.rs` and `conformance/repl/`.

**Totals (2026-11-25), chapters 1 to 7:** 70 claims: 68 covered, 1 FAILS (B-2026-11-25-38), 1 not testable. 89 cells, all passing on the interpreter and compiled except the one `.open` cell.

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
| 112 | A program's own `Option` shadows the prelude's, and the compiler warns | FAILS (B-2026-11-25-38) | ch05/own_option_shadows_prelude (.open) |
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
