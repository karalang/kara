# design.md §6 Expressions and statements sentence-to-cell map (lines 1912-2340)

One row per sentence of design.md §6 Expressions and statements, as of 2026-11-24, naming the cell in this directory (or the test elsewhere) that covers it. NEW marks a cell added by this audit; FAILS marks what is not yet true of the compiler, with its ledger row; FIXED marks a gap this audit fixed. Warn-only behaviour (a lint that warns and lets the program build) is mapped to its test, since a cases.py cell cannot assert a warning.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| s6.1 | 6. Expressions and statements | Kāra is expression-oriented: blocks, `if`, `match` and `loop` produce values. | NEW blocks_if_match_loop_produce_values |
| s6.2 | 6. Expressions and statements | Operands and arguments are evaluated left to right ([core-semantics.md §8](core-semantics.md#8-evaluation-order)), and every value is destro | eval_order_left_to_right; destruction: corpus/core drop_* programs |
| s6.3 | Statements and blocks | A block is a sequence of statements optionally followed by a final expression, the block's value. | NEW blocks_if_match_loop_produce_values |
| s6.4 | Statements and blocks | A block with no final expression has type `()`. | NEW blocks_if_match_loop_produce_values |
| s6.5 | Statements and blocks | ```kara fn example() -> i64 { let a = 1;          // statement let b = 2;          // statement a + b               // the block's value: no | NEW blocks_if_match_loop_produce_values |
| s6.6 | Statements and blocks | The statements are `let`, `let…else`, assignment, `defer`, `errdefer` and expression statements. | NEW first_assignment_initializes_later_ones_need_mut, NEW if_let_else_if_let_and_let_else, NEW defer_and_errdefer_statements |
| s6.7 | Statements and blocks | `return`, `break` and `continue` are expressions of type `Never`, so `return x;` is an expression statement. | conformance/design/functions tail_is_the_result_and_return_returns_early, conformance/design/types unit_value_and_never_in_expression_positions |
| s6.8 | Statements and blocks | **`;` is required after every expression statement that does not end in a block.** The block-like expressions need none: | NEW missing_semicolon_after_an_expression_statement_refused, NEW block_like_statements_need_no_semicolon_and_drop_their_value |
| s6.9 | Statements and blocks | `if`, `match`, `while`, `for`, `loop`, a plain or labeled block, `unsafe { }`, `par { }` and `par for`. | NEW missing_semicolon_after_an_expression_statement_refused, NEW block_like_statements_need_no_semicolon_and_drop_their_value |
| s6.10 | Statements and blocks | A missing `;` anywhere else is an error; the parser never inserts one. | NEW missing_semicolon_after_an_expression_statement_refused, NEW block_like_statements_need_no_semicolon_and_drop_their_value |
| s6.11 | Statements and blocks | **A block-like expression in statement position drops its value** at the end of the statement. | NEW block_like_statements_need_no_semicolon_and_drop_their_value |
| s6.12 | Statements and blocks | The `must_use` lint applies as usual ([§11](#must_use)), so a discarded `par { … }` whose value is a tuple of `Result`s warns. | §11 must_use cells (tests/lint tests; warn-only) |
| s6.13 | Statements and blocks | **A block-like expression in statement position ends at its `}`.** The next token starts a new statement, even one that could otherwise appl | NEW block_like_statement_ends_at_its_brace_refused, NEW parenthesized_block_like_takes_an_operator |
| s6.14 | Statements and blocks | To apply an operator to a block-like expression in statement position, parenthesize it: | NEW block_like_statement_ends_at_its_brace_refused, NEW parenthesized_block_like_takes_an_operator |
| s6.15 | Statements and blocks | `(if c { a } else { b }).len()`. | NEW block_like_statement_ends_at_its_brace_refused, NEW parenthesized_block_like_takes_an_operator |
| s6.16 | Statements and blocks | Since `.`, `?.` and `?` cannot start a statement, one written there is an error whose fix adds the parentheses. | NEW block_like_statement_ends_at_its_brace_refused, NEW parenthesized_block_like_takes_an_operator |
| s6.17 | Statements and blocks | **Other separators.** Match arms are separated by `,`, which is optional after an arm whose body is a block. | NEW trailing_commas_in_fields_parameters_and_arms |
| s6.18 | Statements and blocks | Fields and parameters are separated by `,`, and a trailing comma is allowed. | NEW trailing_commas_in_fields_parameters_and_arms |
| s6.19 | Statements and blocks | Top-level items have no separator. | every multi-item cell |
| s6.20 | Variable binding rules | **Immutable by default.** `let` declares an immutable binding. | NEW reassigning_an_immutable_binding_refused |
| s6.21 | Variable binding rules | Reassigning it, calling a `mut ref self` method on it, or passing it as a `mut` argument ([§7](#7-functions-and-closures)) is a compile erro | NEW reassigning_an_immutable_binding_refused, NEW immutable_binding_mut_method_and_mut_argument_refused, conformance/design/functions the_marker_does_not_grant_mutability_let_v_then_f_mut_v |
| s6.22 | Variable binding rules | `let mut` opts in to mutability: | NEW reassigning_an_immutable_binding_refused, NEW immutable_binding_mut_method_and_mut_argument_refused, NEW let_mut_destructuring_shadowing_and_scopes |
| s6.23 | Variable binding rules | ```kara,uses=d-kv let x = 5; x = 10;              // ERROR: x is immutable  let mut y = 5; y = 10;              // OK  let seen = Map.new(); | NEW reassigning_an_immutable_binding_refused, NEW immutable_binding_mut_method_and_mut_argument_refused, NEW let_mut_destructuring_shadowing_and_scopes |
| s6.24 | Variable binding rules | **`let mut` with a destructuring pattern** makes every binding the pattern introduces mutable. | NEW let_mut_destructuring_shadowing_and_scopes; conformance/design/types mut_name_pattern_refused (no per-binding `mut`) |
| s6.25 | Variable binding rules | There is no per-binding `mut` inside a pattern: | NEW let_mut_destructuring_shadowing_and_scopes; conformance/design/types mut_name_pattern_refused (no per-binding `mut`) |
| s6.26 | Variable binding rules | ```kara,uses=d-p-result let mut (a, b) = (1, 2);                    // a and b are both mutable let mut Point { x, y } = p;                  | NEW let_mut_destructuring_shadowing_and_scopes; conformance/design/types mut_name_pattern_refused (no per-binding `mut`) |
| s6.27 | Variable binding rules | When only some bindings need to be mutable, shadow them after destructuring: | NEW let_mut_destructuring_shadowing_and_scopes; conformance/design/types mut_name_pattern_refused (no per-binding `mut`) |
| s6.28 | Variable binding rules | ```kara let (a, b) = (1, 2);   // both immutable let mut a = a;         // shadow a as mutable; b stays immutable | NEW let_mut_destructuring_shadowing_and_scopes; conformance/design/types mut_name_pattern_refused (no per-binding `mut`) |
| s6.29 | Variable binding rules | **Shadowing** (`let x = …` again) is always allowed, whatever the old binding's mutability. | NEW let_mut_destructuring_shadowing_and_scopes, NEW shadowed_value_lives_to_its_scope_end |
| s6.30 | Variable binding rules | It introduces a new binding; it does not mutate the old one, and the old value stays alive until its scope ends ([core-semantics.md §7.3](co | NEW let_mut_destructuring_shadowing_and_scopes, NEW shadowed_value_lives_to_its_scope_end |
| s6.31 | Variable binding rules | Shadowing works across scopes too: | NEW let_mut_destructuring_shadowing_and_scopes, NEW shadowed_value_lives_to_its_scope_end |
| s6.32 | Variable binding rules | ```kara,uses=d-condition let x = "42"; let x = x.len();          // OK: shadows with a different type  let y = 10; if condition { let y = 20 | NEW let_mut_destructuring_shadowing_and_scopes, NEW shadowed_value_lives_to_its_scope_end |
| s6.33 | Variable binding rules | `let mut` does not affect move and borrow analysis, which is independent of local mutability. | rationale or cross-reference, no cell |
| s6.34 | Variable binding rules | **Explicit initialization is required.** A variable must be assigned before it is used. | NEW read_before_assignment_refused |
| s6.35 | Variable binding rules | There are no zero values and no default initialization: | NEW read_before_assignment_refused |
| s6.36 | Variable binding rules | ```kara let x: i64;          // declared, not yet usable print(x);            // ERROR: x is not initialized x = 5; print(x);            //  | NEW read_before_assignment_refused |
| s6.37 | Variable binding rules | **The first assignment initializes; it does not reassign.** The first assignment to a `let x: | NEW first_assignment_initializes_later_ones_need_mut, NEW second_assignment_to_an_initialized_let_refused |
| s6.38 | Variable binding rules | T;` binding needs no `mut`; later assignments do. | NEW first_assignment_initializes_later_ones_need_mut, NEW second_assignment_to_an_initialized_let_refused |
| s6.39 | Variable binding rules | This applies only to the form without an initializer. | NEW first_assignment_initializes_later_ones_need_mut, NEW second_assignment_to_an_initialized_let_refused |
| s6.40 | Variable binding rules | ```kara let x: i64; x = 5;     // OK: initialization, no `mut` needed x = 10;    // ERROR: x is immutable; write `let mut x: i64;`  let mut  | NEW first_assignment_initializes_later_ones_need_mut, NEW second_assignment_to_an_initialized_let_refused |
| s6.41 | Variable binding rules | **Definite assignment is flow-sensitive.** The compiler tracks whether each such binding is definitely assigned on every path that reads it: | the definite-assignment cells below; tests/assert_borrow_and_definite_assignment_check.rs |
| s6.42 | Variable binding rules | - **In sequence**, reading `x` before any assignment is an error. | NEW read_before_assignment_refused |
| s6.43 | Variable binding rules | - **Branches that all assign**: if every branch of an `if` assigns `x`, `x` is assigned after it. | NEW first_assignment_initializes_later_ones_need_mut (`pick`) |
| s6.44 | Variable binding rules | `if cond { x = 1; } else { x = 2; }` assigns `x`. | NEW first_assignment_initializes_later_ones_need_mut (`pick`) |
| s6.45 | Variable binding rules | - **Branches that do not all assign**: if only some branches assign `x`, it is possibly uninitialized afterwards, and using it is an error. | NEW possibly_uninitialized_after_one_branch_refused |
| s6.46 | Variable binding rules | - **`while` and `for`**: the body may run zero times, so assignments inside it do not count after the loop, even if the loop always runs in  | NEW assignment_in_a_for_body_does_not_count_refused |
| s6.47 | Variable binding rules | - **`loop`**: a body that assigns `x` on every path before every `break` makes `x` assigned after the loop, because the body runs at least o | NEW first_assignment_initializes_later_ones_need_mut (`loop`) |
| s6.48 | Variable binding rules | - **Struct fields**: assigning fields one by one (`p.x = 1; p.y = 2;`) does not assign the struct as a whole; use a struct literal. | NEW fields_assigned_one_by_one_do_not_initialize_refused |
| s6.49 | Variable binding rules | The same holds for the slots of an [array](#arrays). | conformance/design/types array_slot_by_slot_initialization_refused |
| s6.50 | Variable binding rules | ```kara,uses=d-condition let x: i64; if condition { x = 5; } print(x);              // ERROR: x is possibly uninitialized (condition may be  | NEW possibly_uninitialized_after_one_branch_refused, NEW first_assignment_initializes_later_ones_need_mut |
| s6.51 | Variable binding rules | **A loop variable is a fresh binding in each iteration.** A closure created in the loop captures that iteration's value, not one shared vari | NEW loop_variable_is_fresh_each_iteration |
| s6.52 | Variable binding rules | ```kara let mut callbacks = Vec.new(); for i in 0..3 { callbacks.push(// { print(i); }); } for f in callbacks { f(); } // prints 0, 1, 2 (no | NEW loop_variable_is_fresh_each_iteration |
| s6.53 | Assignment | `p = v` stores `v` into the place `p`. | assignment_place_then_value, NEW compound_assignment_operators |
| s6.54 | Assignment | `p op= v` is `p = p op v` with the place evaluated once. | assignment_place_then_value, NEW compound_assignment_operators |
| s6.55 | Assignment | **Places.** The target of an assignment must be a *place expression*: | assignment_place_then_value, parallel_assignment |
| s6.56 | Assignment | ``` PLACE_EXPR = VALUE_IDENT                              // a `let mut` binding / PLACE_EXPR "." IDENT                     // a field or tu | assignment_place_then_value, parallel_assignment |
| s6.57 | Assignment | Call results and literals are not places; they may appear on the right of `=`, never on the left. | NEW call_result_is_not_a_place_refused |
| s6.58 | Assignment | Nothing is written through a `ref`: a place reached through a `ref` is read-only ([core-semantics.md §5.9](core-semantics.md#5-references-an | NEW write_through_a_ref_refused |
| s6.59 | Assignment | Assigning through a `mut ref` needs an explicit `*`: | conformance/design/functions plain_assign_to_a_mut_ref_binding_refused, compound_assignment_through_mut_ref_and_plain_assign_needs_star |
| s6.60 | Assignment | `*counter = 0;`. | conformance/design/functions plain_assign_to_a_mut_ref_binding_refused, compound_assignment_through_mut_ref_and_plain_assign_needs_star |
| s6.61 | Assignment | **Order.** The operands of the place are evaluated once, left to right, then the value; then the old value is dropped and the new one stored | assignment_place_then_value, assignment_bounds_checked_after_value; corpus/core drop_assign |
| s6.62 | Assignment | **Compound assignment.** The operators are `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `/=`, `^=`, `<<=` and `>>=`. | NEW compound_assignment_operators (all ten) |
| s6.63 | Assignment | Each needs a mutable target, and its result type follows [Mixed-width operands](#mixed-width-operands). | NEW compound_assignment_operators; conformance/design/types compound_assignment_n_i32_total_i64_is_an_error, compound_assignment_into_the_wider_side |
| s6.64 | Assignment | **Parallel assignment.** Several places can be assigned several values in one statement: | parallel_assignment |
| s6.65 | Assignment | ``` MULTI_ASSIGN = PLACE_EXPR { "," PLACE_EXPR } "=" EXPR { "," EXPR } ";" | parallel_assignment |
| s6.66 | Assignment | ```kara,uses=d-swap a, b = b, a;                       // swap two locals v[i], v[j] = v[j], v[i];           // swap two slots of a Vec of a | parallel_assignment |
| s6.67 | Assignment | Every value on the right is evaluated, left to right, before any target is written; that is what makes `a, b = b, a` a swap. | parallel_assignment |
| s6.68 | Assignment | Then each target's place operands (an index, a field path) are evaluated and the target written, left to right, one target at a time. | parallel_assignment |
| s6.69 | Assignment | This is the one place where values come before places; a single `p = v` evaluates `p`'s operands first ([core-semantics.md §8.2](core-semant | parallel_assignment |
| s6.70 | Assignment | So `v[i], i = 0, 5` writes `v[i]` with the old `i`. | parallel_assignment |
| s6.71 | Assignment | (B-2026-11-05-23) Both sides must list the same number of items (a mismatch is a parse error), except that a single expression of tuple type | parallel_assignment_count_mismatch_refused, NEW parallel_assignment_from_a_tuple_value |
| s6.72 | Assignment | `lo, hi = if c { (lo, mid) } else { (mid + 1, hi) };` and `q, r = div_rem(a, b);` write each element to its target, left to right, after the | parallel_assignment_count_mismatch_refused, NEW parallel_assignment_from_a_tuple_value |
| s6.73 | Assignment | Rust's `(a, b) = e` is refused, as a parenthesized tuple is a value, with a fix that drops the parentheses (B-2026-11-09-38). | parenthesized_tuple_assignment_refused (with the fix) |
| s6.74 | Assignment | Each target follows the place rules above. | parallel_assignment |
| s6.75 | Assignment | This is the in-place idiom that swap-based sorts and permutation generators rely on. | rationale or cross-reference, no cell |
| s6.76 | Assignment | Assignment is a statement, not an expression: it has no value. | NEW assignment_is_not_an_expression_refused |
| s6.77 | Assignment | It may also be the body of a `match` arm ([Pattern matching](#pattern-matching)). | conformance/design/types arm_bodies_and_commas |
| s6.78 | Conditionals | `if` is an expression. | NEW blocks_if_match_loop_produce_values |
| s6.79 | Conditionals | It produces a value and can appear wherever an expression can: | NEW blocks_if_match_loop_produce_values |
| s6.80 | Conditionals | ```kara,uses=d-ab let max = if a > b { a } else { b }; | NEW blocks_if_match_loop_produce_values |
| s6.81 | Conditionals | **Conditions must be `bool`.** There is no truthiness: integers, `Option` and other types do not convert to `bool`. | NEW condition_must_be_bool_refused, NEW option_is_no_condition_refused |
| s6.82 | Conditionals | Write the predicate: | NEW condition_must_be_bool_refused, NEW option_is_no_condition_refused |
| s6.83 | Conditionals | `x != 0`, `x.is_some()`, `not items.is_empty()`. | NEW condition_must_be_bool_refused, NEW option_is_no_condition_refused |
| s6.84 | Conditionals | Conversions are always explicit. | NEW condition_must_be_bool_refused, NEW option_is_no_condition_refused |
| s6.85 | Conditionals | **`if` without `else` has type `()`.** Its block must then produce `()` too. | NEW if_without_else_producing_a_value_refused |
| s6.86 | Conditionals | A block that produces a value with no `else` branch is a type error, even in statement position, so a value is never silently discarded: | NEW if_without_else_producing_a_value_refused |
| s6.87 | Conditionals | ```kara,uses=d-cond-ab if cond { side_effect(); }           // OK: the block produces () if cond { 5 }                        // TYPE ERROR: | NEW if_without_else_producing_a_value_refused |
| s6.88 | Conditionals | **No postfix conditional.** `val if cond else other` (Python style) does not exist. | NEW postfix_conditional_refused |
| s6.89 | Conditionals | The prefix form is enough and avoids a precedence question. | rationale or cross-reference, no cell |
| s6.90 | Conditionals | **A struct literal in a condition must be parenthesized**, as in Rust. | NEW struct_literal_in_a_condition_parenthesized, NEW unparenthesized_struct_literal_in_a_condition_refused |
| s6.91 | Conditionals | In the condition of `if`, `while`, `if let` and `while let`, and in the collection of a `for`, a `{` after a name starts the body: | NEW struct_literal_in_a_condition_parenthesized, NEW unparenthesized_struct_literal_in_a_condition_refused |
| s6.92 | Conditionals | `if p == (Point { x: 0, y: 0 }) { … }`. | NEW struct_literal_in_a_condition_parenthesized, NEW unparenthesized_struct_literal_in_a_condition_refused |
| s6.93 | `if let` and `let…else` | **`if let`** runs a block when a pattern matches, with the pattern's bindings in scope inside that block only: | NEW if_let_else_if_let_and_let_else, NEW if_let_binding_not_in_scope_after_refused |
| s6.94 | `if let` and `let…else` | ```kara,uses=d-user-find-values if let Some(u) = user.find(id) { process(u); }  if let Some(u) = user.find(id) { process(u); } else { log("n | NEW if_let_else_if_let_and_let_else, NEW if_let_binding_not_in_scope_after_refused |
| s6.95 | `if let` and `let…else` | It is the one-shot counterpart of `while let`, and saves a full `match` for a single pattern. | rationale or cross-reference, no cell |
| s6.96 | `if let` and `let…else` | `else if let` is valid, because any `if` may follow `else`: | NEW if_let_else_if_let_and_let_else |
| s6.97 | `if let` and `let…else` | ```kara,uses=d-try-ab if let Some(a) = try_a() { observe(a); } else if let Some(b) = try_b() { observe(b); } else { fallback(); } | NEW if_let_else_if_let_and_let_else |
| s6.98 | `if let` and `let…else` | Or-patterns work as in `match` ([Or-patterns](#or-patterns)): | NEW if_let_else_if_let_and_let_else |
| s6.99 | `if let` and `let…else` | ```kara,uses=d-either-val if let Left(x) / Right(x) = val { observe(x); } | NEW if_let_else_if_let_and_let_else |
| s6.100 | `if let` and `let…else` | **`let…else`** destructures at the top of a scope and exits early when the pattern does not match: | NEW if_let_else_if_let_and_let_else (`start`) |
| s6.101 | `if let` and `let…else` | ```kara,uses=d-load-config-result fn start(path: Str) -> Result[Config, ConfigError] { let Ok(config) = load_config(path) else { return Err( | NEW if_let_else_if_let_and_let_else (`start`) |
| s6.102 | `if let` and `let…else` | - The `else` block **must diverge**: | conformance/design/types let_else_block_must_diverge_refused |
| s6.103 | `if let` and `let…else` | `return`, `break`, `continue`, or a call to a diverging function (`panic()`, `unreachable()`). | conformance/design/types let_else_block_must_diverge_refused |
| s6.104 | `if let` and `let…else` | The type checker enforces this. | conformance/design/types let_else_block_must_diverge_refused |
| s6.105 | `if let` and `let…else` | - The pattern's bindings are in scope **after** the statement, not inside the `else` block. | NEW let_else_binding_not_in_scope_in_else_refused, NEW if_let_else_if_let_and_let_else |
| s6.106 | `if let` and `let…else` | - `let…else` is a statement, not an expression; it has no value. | statement form; NEW if_let_else_if_let_and_let_else |
| s6.107 | `if let` and `let…else` | `let…else` is the idiomatic guard at the top of a function. | rationale or cross-reference, no cell |
| s6.108 | `if let` and `let…else` | It keeps the main path unindented. | rationale or cross-reference, no cell |
| s6.109 | `if let` and `let…else` | **Scrutinee temporaries.** Temporaries created while evaluating the expression after `=` drop where [core-semantics.md §7.4](core-semantics. | NEW scrutinee_temporaries_drop_points |
| s6.110 | `if let` and `let…else` | - an `if let` scrutinee lives to the end of the whole construct, `else` branch included. | NEW scrutinee_temporaries_drop_points |
| s6.111 | `if let` and `let…else` | To release a lock or a lease before the `else` branch runs, bind the needed value with `let` first; | NEW scrutinee_temporaries_drop_points |
| s6.112 | `if let` and `let…else` | - a `while let` scrutinee lives to the end of each iteration's body; | corpus/core drop_loop; NEW scrutinee_temporaries_drop_points |
| s6.113 | `if let` and `let…else` | - a `let…else` initializer's temporaries drop before the `else` block runs, or at the `;` when the pattern matches. | NEW scrutinee_temporaries_drop_points |
| s6.114 | `if let` and `let…else` | `if let` chains (several `and`-joined `let` tests in one condition) are deferred ([deferred.md](deferred.md)). | deferred (deferred.md), no cell |
| s6.115 | Loops and labels | There are four loop forms: | NEW loop_forms_and_iteration |
| s6.116 | Loops and labels | - **`while condition { … }`** runs while a condition holds. | NEW loop_forms_and_iteration |
| s6.117 | Loops and labels | - **`while let PATTERN = expr { … }`** runs while a pattern matches, for example `while let Some(job) = queue.pop() { … }`. | NEW loop_forms_and_iteration |
| s6.118 | Loops and labels | - **`for PATTERN in e { … }`** iterates. | NEW loop_forms_and_iteration |
| s6.119 | Loops and labels | The pattern may destructure (`for (key, value) in map`). | NEW loop_forms_and_iteration |
| s6.120 | Loops and labels | When `e` is an `Iterator`, the loop consumes it, so `for x in c.into_iter()` moves `c` and each item. | NEW for_over_into_iter_moves_the_collection_refused |
| s6.121 | Loops and labels | Otherwise the loop borrows `e` through `Iterable`, and `x` is the iterator's item, `ref T` for the standard collections ([core-semantics.md  | NEW loop_forms_and_iteration (`for x in v` keeps `v`) |
| s6.122 | Loops and labels | To loop over an iterator without consuming it, write `for x in it.by_ref()`. | NEW loop_forms_and_iteration (`it.by_ref()`) |
| s6.123 | Loops and labels | The iteration traits are in [§9](#iterator-and-iterable). | rationale or cross-reference, no cell |
| s6.124 | Loops and labels | - **`loop { … }`** runs until a `break`. | NEW blocks_if_match_loop_produce_values |
| s6.125 | Loops and labels | Each iteration is its own scope: what it creates drops at the end of the iteration, and `continue` ends it the same way ([core-semantics.md  | NEW iteration_scope_drops_at_continue |
| s6.126 | Loops and labels | **Ranges.** The range operators `..` and `..=` build values of six library types, depending on which ends are given: | NEW range_forms_iterate_and_index |
| s6.127 | Loops and labels | / Syntax / Type / Meaning / Iterable? / /---/---/---/---/ | header |
| s6.128 | Loops and labels | / `lo..hi` / `Range[T]` / exclusive end, `[lo, hi)` / Yes / | NEW range_forms_iterate_and_index |
| s6.129 | Loops and labels | / `lo..=hi` / `RangeInclusive[T]` / inclusive end, `[lo, hi]` / Yes / | NEW range_forms_iterate_and_index |
| s6.130 | Loops and labels | / `lo..` / `RangeFrom[T]` / no upper end, `[lo, ∞)` / Yes; the loop must end by `break`, `take` or the like. | NEW range_forms_iterate_and_index (`5..` with `break`) |
| s6.131 | Loops and labels | It yields every value up to `T.MAX` and panics with overflow when asked for the one after: | NEW range_from_panics_past_the_maximum |
| s6.132 | Loops and labels | `254u8..` yields 254, 255, then panics / | NEW range_from_panics_past_the_maximum |
| s6.133 | Loops and labels | / `..hi` / `RangeTo[T]` / exclusive end only, `(-∞, hi)` / No / | NEW range_forms_iterate_and_index (`..n`, `..=n`, `..` as indices) |
| s6.134 | Loops and labels | / `..=hi` / `RangeToInclusive[T]` / inclusive end only, `(-∞, hi]` / No / | NEW range_forms_iterate_and_index (`..n`, `..=n`, `..` as indices) |
| s6.135 | Loops and labels | / `..` / `RangeFull` / no ends / No / | NEW range_forms_iterate_and_index (`..n`, `..=n`, `..` as indices) |
| s6.136 | Loops and labels | Only the forms with a start can be iterated. | NEW range_to_is_not_iterable_refused |
| s6.137 | Loops and labels | `RangeTo`, `RangeToInclusive` and `RangeFull` appear mostly as slice indices (`items[..n]`, `items[..=n]`, `items[..]`) and as [range patter | NEW range_forms_iterate_and_index; conformance/design/types range_pattern_forms |
| s6.138 | Loops and labels | Every sequence that can be indexed by a `Range` can be indexed by all six forms, with no extra surface. | NEW range_forms_iterate_and_index; conformance/design/types range_pattern_forms |
| s6.139 | Loops and labels | This is a guarantee about the built-in sequences, not a trait: there is no nameable `SliceIndex` in v1. | statement of absence, no cell |
| s6.140 | Loops and labels | ```kara,uses=d-items-n for i in 0..10 { print(i); }       // 0, 1, …, 9 for i in 0..=10 { print(i); }      // 0, 1, …, 10 let middle = items | NEW range_forms_iterate_and_index |
| s6.141 | Loops and labels | Prefer `..=` to `lo..(hi + 1)` when the end must be visited, and the half-open forms (`items[2..]`, `items[..n]`) to `items[2..items.len()]` | rationale or cross-reference, no cell |
| s6.142 | Loops and labels | Both remove an off-by-one risk from the expression. | rationale or cross-reference, no cell |
| s6.143 | Loops and labels | **`loop` is an expression** when it exits with a value: | NEW blocks_if_match_loop_produce_values, NEW loop_types_from_breaks |
| s6.144 | Loops and labels | ```kara,uses=d-try-something let result = loop { let attempt = try_something(); if attempt.is_ok() { break attempt;   // the loop's value }  | NEW blocks_if_match_loop_produce_values, NEW loop_types_from_breaks |
| s6.145 | Loops and labels | **Labels.** Any loop may carry a label, written with a leading `'`, so that `break` and `continue` can target it from inside nested loops: | NEW labels_loops_and_blocks |
| s6.146 | Loops and labels | ```kara,uses=d-matrix 'outer: for row in matrix { for val in row { if *val == target { break 'outer;       // leaves the outer loop } } } | NEW labels_loops_and_blocks |
| s6.147 | Loops and labels | `break` leaves the innermost loop and `continue` starts its next iteration; `break 'label` and `continue 'label` target the named loop inste | NEW labels_loops_and_blocks |
| s6.148 | Loops and labels | The sigil keeps a label apart from a value: | NEW labels_loops_and_blocks |
| s6.149 | Loops and labels | `break name` always means a value named `name`, never a label. | NEW labels_loops_and_blocks |
| s6.150 | Loops and labels | **Labeled blocks.** Any block may carry a label, which makes it an early exit with a value: | NEW labels_loops_and_blocks (`'found`) |
| s6.151 | Loops and labels | ```kara,uses=d-matrix let result = 'found: { for row in matrix { for cell in row { if *cell == target { break 'found *cell;     // leaves th | NEW labels_loops_and_blocks (`'found`) |
| s6.152 | Loops and labels | A labeled block is an ordinary block with one extra ability: code inside it may write `break 'label value` (or a bare `break 'label`) to lea | NEW labels_loops_and_blocks (`'found`) |
| s6.153 | Loops and labels | Without a label there would be no way to name a block as a `break` target. | NEW labels_loops_and_blocks (`'found`) |
| s6.154 | Loops and labels | - The block's type is the least upper bound ([§8](#8-type-inference-and-generics)) of every reachable `break 'label value` and the final exp | NEW labels_loops_and_blocks (`'unused` is 7, `'u` is `()`); NEW loop_breaks_of_different_types_refused (join) |
| s6.155 | Loops and labels | A block whose final expression is `()` and whose only breaks are bare has type `()`. | NEW labels_loops_and_blocks (`'unused` is 7, `'u` is `()`); NEW loop_breaks_of_different_types_refused (join) |
| s6.156 | Loops and labels | - A label that is never targeted is allowed; the block is then a plain block. | NEW labels_loops_and_blocks (`'unused`) |
| s6.157 | Loops and labels | - `continue 'label` is **rejected** when `'label` names a block, because there is nothing to continue: | NEW continue_to_a_labeled_block_refused |
| s6.158 | Loops and labels | `continue label refers to a labeled block; continue is only valid for loops`, with a fix pointing at the label. | NEW continue_to_a_labeled_block_refused |
| s6.159 | Loops and labels | **Label scope.** A label is in scope only in the body of its loop or block, and it shadows an enclosing label of the same name. | NEW labels_loops_and_blocks (inner `'l` shadows the outer); conformance/design/functions a_label_given_twice_is_an_error |
| s6.160 | Loops and labels | An unknown label in `break` or `continue` is a compile error. | conformance/design/functions an_unknown_label_is_an_error |
| s6.161 | Loops and labels | A `break` inside a closure cannot target a label outside the closure, because the closure is not part of that control flow. | NEW break_out_of_a_closure_to_an_outer_label_refused |
| s6.162 | Loops and labels | **`break value` is valid only in `loop` and in labeled blocks.** `while` and `for` loops always have type `()`. | NEW break_value_in_a_for_loop_refused |
| s6.163 | Loops and labels | A `break` with a non-`()` value that targets a `while` or `for` loop, directly or by label, is a compile error. | NEW break_value_in_a_for_loop_refused |
| s6.164 | Loops and labels | A plain `break` works in every loop. | NEW loop_forms_and_iteration, NEW labels_loops_and_blocks |
| s6.165 | Loops and labels | **The type of a `loop`** comes from its `break`s: | NEW loop_types_from_breaks (`forever`) |
| s6.166 | Loops and labels | - **No reachable `break`**: the type is `Never`. | NEW loop_types_from_breaks (`forever`) |
| s6.167 | Loops and labels | The loop runs forever or leaves only by `return` or a panic. | NEW loop_types_from_breaks (`forever`) |
| s6.168 | Loops and labels | - **One or more `break value`**: the type is the least upper bound of the break values; values whose types do not unify are an error. | NEW loop_types_from_breaks, NEW loop_breaks_of_different_types_refused |
| s6.169 | Loops and labels | - **A conditional `break`** still contributes its type. | NEW loop_types_from_breaks (`x` is i32 and widens into `big: i64`) |
| s6.170 | Loops and labels | The loop may never reach it at run time, but its type is the break value's type: the compiler does not infer `Option` or `Never` because a ` | NEW loop_types_from_breaks (`x` is i32 and widens into `big: i64`) |
| s6.171 | Loops and labels | ```kara,uses=d-ready // A conditional break: the type is i32, not Option[i32] let x = loop { if ready { break 5i32; } };  // Two breaks of t | NEW loop_types_from_breaks (`x` is i32 and widens into `big: i64`) |
| s6.172 | Operators and precedence | From loosest to tightest: | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.173 | Operators and precedence | / Level / Operators / Associativity / /---/---/---/ | header |
| s6.174 | Operators and precedence | / 1 (loosest) / default `??` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.175 | Operators and precedence | / 2 / range `..` `..=` / None / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.176 | Operators and precedence | / 3 / logical `or` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.177 | Operators and precedence | / 4 / logical `and` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.178 | Operators and precedence | / 5 / comparison `==` `!=` `<` `<=` `>` `>=` / Left (no chaining) / | conformance/precedence.py (984 expressions, every operator pair); compare_chain_eq_refused, compare_chain_lt_refused, compare_chain_mixed_refused |
| s6.179 | Operators and precedence | / 6 / bitwise or `\/` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.180 | Operators and precedence | / 7 / bitwise xor `^` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.181 | Operators and precedence | / 8 / bitwise and `&` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.182 | Operators and precedence | / 9 / shift `<<` `>>` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.183 | Operators and precedence | / 10 / additive `+` `-` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.184 | Operators and precedence | / 11 / multiplicative `*` `/` `%` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.185 | Operators and precedence | / 12 / cast `as` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.186 | Operators and precedence | / 13 / unary `not` `-` `~` `*` / Prefix / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.187 | Operators and precedence | / 14 / error propagation `?` / Postfix / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.188 | Operators and precedence | / 15 (tightest) / field access, call and index: | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.189 | Operators and precedence | `.` `()` `[]` / Left / | conformance/precedence.py (984 expressions, every operator pair); operator_binding_examples |
| s6.190 | Operators and precedence | So `a + b as f64` is `a + (b as f64)`, `-x as u8` is `(-x) as u8`, and `x as u32 as u8` casts twice. | operator_binding_examples |
| s6.191 | Operators and precedence | Ranges do not chain: | range_chain_refused |
| s6.192 | Operators and precedence | `a..b..c` is an error. | range_chain_refused |
| s6.193 | Operators and precedence | **Logical operators are words.** `and`, `or` and `not` are the logical operators; `and` and `or` evaluate their right side only when needed  | short_circuit, conformance/precedence.py (984 expressions, every operator pair) |
| s6.194 | Operators and precedence | The symbols `&&`, `//` and `!` are rejected with a fix that substitutes the word. | NEW symbol_logical_operators_refused (and its fix) |
| s6.195 | Operators and precedence | `~` is bitwise not, and prefix `*` dereferences a `ref T` or `mut ref T`. | NEW bitwise_not_and_deref_prefix |
| s6.196 | Operators and precedence | **`not` binds tighter than comparison**, so `not x == y` is `(not x) == y`, as with `!` in C-family languages. | operator_binding_examples (`not p == q`) |
| s6.197 | Operators and precedence | Parenthesize for `not (x == y)`; the `ambiguous_not_comparison` lint warns about `not` directly before a comparison. | operator_binding_examples; ambiguous_not_comparison lint in tests/lint tests (warn-only) |
| s6.198 | Operators and precedence | **No automatic dereference in operators.** Field access and method calls look through `ref T` and `mut ref T`, so `r.field` needs no `*`. | conformance/design/functions r_copies_a_copy_value_comparisons_look_through_refs, compound_assignment_through_mut_ref_and_plain_assign_needs_star |
| s6.199 | Operators and precedence | Comparisons look through references too: | conformance/design/functions r_copies_a_copy_value_comparisons_look_through_refs, compound_assignment_through_mut_ref_and_plain_assign_needs_star |
| s6.200 | Operators and precedence | `==`, `!=`, `<`, `<=`, `>` and `>=` compare the values whether either side is a `T`, a `ref T` or a `mut ref T`, so `c.name == name` with a  | conformance/design/functions r_copies_a_copy_value_comparisons_look_through_refs, compound_assignment_through_mut_ref_and_plain_assign_needs_star |
| s6.201 | Operators and precedence | Other operators do not: use `*` for arithmetic on a non-`Copy` `T` and for assignment through a reference (`*r = v`) ([`*` and `ref` in expr | conformance/design/functions r_copies_a_copy_value_comparisons_look_through_refs, compound_assignment_through_mut_ref_and_plain_assign_needs_star |
| s6.202 | Operators and precedence | **Operators are trait calls.** Each operator calls the method of its operator trait for the operand types ([§9](#9-traits)); on numeric prim | §9 operator-trait cells (tests/core_operator_traits.rs); conformance/overflow.py for the numeric rules |
| s6.203 | Operators and precedence | **Multi-index.** `e[i, j, k]` is sugar for `e[(i, j, k)]`: the indices form a tuple, and `Index` or `IndexMut` is called with it. | NEW multi_index_is_a_tuple_index |
| s6.204 | Operators and precedence | A matrix type that implements `Index[(i64, i64)]` is indexed as `m[r, c]`. | NEW multi_index_is_a_tuple_index |
| s6.205 | Temporaries | A temporary is a value an expression creates without binding it to a name: an unbound call result, an intermediate `Option`, the lease retur | corpus/core drop_temp_stmt, drop_temp_match_guard |
| s6.206 | Temporaries | Where each one is dropped is fixed by its position, in the table of [core-semantics.md §7.4](core-semantics.md#7-destruction-c6-supersedes-t | corpus/core drop_temp_stmt, drop_temp_match_guard |
| s6.207 | Temporaries | Several temporaries that drop at the same point drop in reverse order of creation. | NEW temporaries_at_one_point_drop_in_reverse_creation_order |
| s6.208 | Temporaries | There is no temporary lifetime extension ([core-semantics.md §5.5](core-semantics.md#5-references-and-views-c5)). | NEW ref_of_a_temporary_refused |
| s6.209 | Temporaries | A reference or view into a temporary cannot outlive the statement that created it, so `let r = ref make_value();` is an error. | NEW ref_of_a_temporary_refused |
| s6.210 | Temporaries | Bind the value first: | NEW ref_of_a_temporary_refused |
| s6.211 | Temporaries | `let v = make_value(); let r = ref v;`. | NEW ref_of_a_temporary_refused |
| s6.212 | Temporaries | A fixed table, rather than "the longest reasonable scope", lets a reader (human or model) look up where a temporary drops instead of reasoni | rationale or cross-reference, no cell |
| s6.213 | `?` and `??` | **`e?`** unwraps an `Ok` or `Some`, and on `Err` or `None` returns from the enclosing function. | operator_binding_examples (`-f()?`); §10 cells |
| s6.214 | `?` and `??` | Its rules, including the `From` conversion of the error, are in [§10 Results and propagation](#results-and-propagation). | operator_binding_examples (`-f()?`); §10 cells |
| s6.215 | `?` and `??` | **`a ?? b`** supplies a default. | short_circuit (`??` on Option and Result) |
| s6.216 | `?` and `??` | It gives the success value inside `a` when `a` is `Some` or `Ok`, and `b` otherwise. | short_circuit (`??` on Option and Result) |
| s6.217 | `?` and `??` | The error in an `Err` is discarded; to inspect it, use `match` or `.unwrap_or_else(/e/ ...)`. | short_circuit (`??` on Option and Result) |
| s6.218 | `?` and `??` | - `b` is evaluated only when `a` is `None` or `Err` ([core-semantics.md §8.3](core-semantics.md#8-evaluation-order)). | short_circuit |
| s6.219 | `?` and `??` | - `b` must have the type of the value inside `a`, and so does the result. | NEW default_of_the_wrong_type_refused |
| s6.220 | `?` and `??` | One exception: when `a` is an `Option[ref V]` and `V: | NEW default_operator_types_and_copy_exception |
| s6.221 | `?` and `??` | Copy`, `b` may be a `V`, and the result is a `V`. | NEW default_operator_types_and_copy_exception |
| s6.222 | `?` and `??` | So `counts.get(word) ?? 0` works. | NEW default_operator_types_and_copy_exception |
| s6.223 | `?` and `??` | For other `V`, write `m.get(k).cloned() ?? d`. | NEW default_operator_types_and_copy_exception |
| s6.224 | `?` and `??` | - `??` has the loosest precedence of all operators. | operator_binding_examples (`o ?? 1 + 2`), conformance/precedence.py (984 expressions, every operator pair) |
| s6.225 | `?` and `??` | ```kara,uses=d-config-port-value let port = config.port ?? 8080;     // config.port: Option[i64] | NEW default_operator_types_and_copy_exception (`port ?? 8080`) |
| s6.226 | `defer` and `errdefer` | `defer` schedules cleanup for the end of the current scope; `errdefer` schedules cleanup for an error exit only. | NEW defer_and_errdefer_statements; §10 cells (tests/defer_early_exit.rs) |
| s6.227 | `defer` and `errdefer` | Both are statements: | NEW defer_and_errdefer_statements; §10 cells (tests/defer_early_exit.rs) |
| s6.228 | `defer` and `errdefer` | ``` DEFER_STATEMENT    = "defer" ( EXPR ";" / BLOCK ) ERRDEFER_STATEMENT = "errdefer" ( EXPR ";" / BLOCK ) / "errdefer" "(" IDENT ")" BLOCK  | NEW defer_and_errdefer_statements; §10 cells (tests/defer_early_exit.rs) |
| s6.229 | `defer` and `errdefer` | When they run, what `errdefer(e)` binds, and what a cleanup block may contain are in [§10 Cleanup with `defer` and `errdefer`](#cleanup-with | NEW defer_and_errdefer_statements; §10 cells (tests/defer_early_exit.rs) |
| s6.230 | `defer` and `errdefer` | --- | rule |
