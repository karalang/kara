# design.md §8 sentence-to-cell map (lines 2834-3159)

One row per sentence of design.md §8 Type inference and generics, as of 2026-11-24, naming the cell in this directory (or the test elsewhere) that covers it. NEW marks a cell added by this audit; GAP and FAILS mark what is not covered.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| s8.1 | Generic parameters and arguments | Generic parameters are written in square brackets: `Vec[i32]`, `fn sort[T: Ord](list: own Vec[T]) -> Vec[T]`. | const_generics_array, bound_unsatisfied_at_call |
| s8.2 | Generic parameters and arguments | `[` is not a comparison operator, so there is no ambiguity with less-than (prior art: Go 1.18). | rationale or cross-reference, no cell |
| s8.3 | Generic parameters and arguments | Const parameters use the `const` prefix: `fn rotated[T, const N: i64](arr: own Array[T, N]) -> Array[T, N]` ([Const generic parameters](#con | const_generics_array |
| s8.4 | Generic parameters and arguments | **`[T]` names and declares types. | every generic cell |
| s8.5 | Generic parameters and arguments | It is never applied at a call site.** `[` is also the index operator, and `f[x](args)` is already a legal index followed by a call (an eleme | index_then_call_is_legal, fn_type_args_at_call_is_index |
| s8.6 | Generic parameters and arguments | A call-site `f[T](args)` could only be told from indexing by asking whether the bracket holds a type and the callee is generic, which breaks | rationale or cross-reference, no cell |
| s8.7 | Generic parameters and arguments | So types at a call come from one of two places: | header |
| s8.8 | Generic parameters and arguments | - **A qualified receiver:** `Vec[i64].new()`, `u32.size_of()`. | qualified_receiver_and_annotation |
| s8.9 | Generic parameters and arguments | A type in receiver position cannot be an index, because a type cannot be indexed. | rationale or cross-reference, no cell |
| s8.10 | Generic parameters and arguments | - **An annotation:** `let v: Vec[i64] = Vec.new();`, `let p: *const u32 = ptr.null();`. | qualified_receiver_and_annotation |
| s8.11 | Generic parameters and arguments | **Every type parameter must be decidable at a call.** A function's, method's or trait method's type parameter that appears in no parameter t | undecidable_type_param_refused, undecidable_method_param_refused, param_in_other_bound_is_decidable (control) |
| s8.12 | Generic parameters and arguments | It is an error at the declaration, naming the parameter (review-thread ruling 2026-11-16). | undecidable_type_param_refused |
| s8.13 | Generic parameters and arguments | A trait impl's method is reported at the trait. | undecidable_trait_method_param_reported_at_trait |
| s8.14 | Generic parameters and arguments | The same holds for a trait's associated function called as `Trait.name()`: its `Self` is decided only by the expected type through the retur | trait_assoc_fn_decided_by_expected, trait_assoc_fn_undecided_refused |
| s8.15 | Generic parameters and arguments | **Generic arguments against indexing.** The parser decides by syntax alone: | header |
| s8.16 | Generic parameters and arguments | - **In a type position** (annotations, return types, field types, bounds), `Name[T1, ..., Tn]` is always a generic type. | every annotated cell |
| s8.17 | Generic parameters and arguments | - **In an expression**, `e[i]` is always an index, except that a Type-class identifier or a primitive type name followed by `[` always begin | qualified_receiver_and_annotation (Map[String, i64].new()) |
| s8.18 | Generic parameters and arguments | - **There is no generic function value by brackets.** `sort[i32]` alone is an index into `sort`. | fn_type_args_at_call_is_index; NEW fn_value_by_brackets_alone_is_index_refused |
| s8.19 | Generic parameters and arguments | For a function at a chosen type, annotate the binding (`let f: Fn(own Vec[i32]) -> Vec[i32] = sort;`) or write a closure with an annotated p | generic_fn_value_by_annotation |
| s8.20 | Type inference | Type inference is **bidirectional**: two judgments, `synthesize` and `check`, with fixed rules for which applies where. | literal_inference, lub_unifies_unknown_args |
| s8.21 | Type inference | Type variables that neither judgment can settle at once are solved by unification within one function body. | literal_inference, lub_unifies_unknown_args |
| s8.22 | Type inference | **Why this algorithm.** Global Hindley-Milner inference was rejected because effect sets (subset inclusion, not equality) combine awkwardly  | rationale or cross-reference, no cell |
| s8.23 | Type inference | Pure bottom-up synthesis was rejected because it cannot type a closure's parameters from context. | rationale or cross-reference, no cell |
| s8.24 | Type inference | Bidirectional typing is what Dotty and Swift use for a similar feature set. | rationale or cross-reference, no cell |
| s8.25 | Type inference | **The two judgments.** | header |
| s8.26 | Type inference | - **Synthesize:** `Γ ⊢ e ⇒ τ`. | definitions; exercised by the rows below |
| s8.27 | Type inference | From the context `Γ` and expression `e`, produce a type `τ`. | definitions; exercised by the rows below |
| s8.28 | Type inference | Used when no expected type is known. | definitions; exercised by the rows below |
| s8.29 | Type inference | - **Check:** `Γ ⊢ e ⇐ τ`. | definitions; exercised by the rows below |
| s8.30 | Type inference | Verify that `e` has type `τ` or a subtype of it ([Subtyping and variance](#subtyping-and-variance)). | definitions; exercised by the rows below |
| s8.31 | Type inference | Used when an expected type is known. | definitions; exercised by the rows below |
| s8.32 | Type inference | **Subsumption.** In check mode, `e ⇐ τ` succeeds if `e ⇒ τ'` and `τ' ≤ τ`. | definitions; exercised by the rows below |
| s8.33 | Type inference | This is the one rule connecting the judgments. | definitions; exercised by the rows below |
| s8.34 | Type inference | When both a synthesis and a check rule apply, check mode wins because it carries more information. | definitions; exercised by the rows below |
| s8.35 | Type inference | **Which mode applies.** | header |
| s8.36 | Type inference | / Context / Mode / | header |
| s8.37 | Type inference | / Expression with no binding and no expected type / synthesize / | literal_inference |
| s8.38 | Type inference | / `let x = e` (no annotation) / synthesize `e`, bind `x` to the result / | literal_inference |
| s8.39 | Type inference | / `let x: T = e` / check `e ⇐ T` / | literal_range_checked |
| s8.40 | Type inference | / Function body with declared return type `R` / check the body `⇐ R` / | literal_inference (f: u64 body) |
| s8.41 | Type inference | / `return e` in a function returning `R` / check `e ⇐ R` / | never_inference |
| s8.42 | Type inference | / Call `f(arg)` where `f`'s parameter type is `P` / check `arg ⇐ P` / | literal_inference (take_i32) |
| s8.43 | Type inference | / `if c { t } else { e }` with an expected type / check both branches / | join_of_literals |
| s8.44 | Type inference | / `if c { t } else { e }` with no expected type / synthesize both branches, join at their [least upper bound](#least-upper-bound) / | lub_unifies_unknown_args |
| s8.45 | Type inference | / `match` with an expected type / check every arm / | join_of_literals_range_checked |
| s8.46 | Type inference | / `match` with no expected type / synthesize each arm, join at their least upper bound / | lub_anything_else_error |
| s8.47 | Type inference | / Closure `\/x\/ body` against an expected function type `Fn(A) -> B` (or `MutFn`, `OnceFn`) / `x : A`; check `body ⇐ B`; the closure's kind | fn_subtyping, mutfn_where_fn_refused; NEW closure_checked_against_fn_type |
| s8.48 | Type inference | / Closure `\/x: A\/ body` with no expected type / synthesize `body ⇒ B`; the closure's kind and effects come from its body ([core-semantics. | generic_fn_value_by_annotation (|v: Vec[String]|) |
| s8.49 | Type inference | / Struct literal `S { f: e, ... }` / check each field against its declared type / | NEW struct_literal_field_checked, control struct_literal_field_takes_type |
| s8.50 | Type inference | **Core rules.** | literal_inference |
| s8.51 | Type inference | - **Literals.** An integer or float literal starts as a type variable that later uses may fix. | literal_inference |
| s8.52 | Type inference | If nothing fixes it by the end of the function body, it defaults to `i64` or `f64`. | literal_inference |
| s8.53 | Type inference | In check mode against a numeric type, a literal takes that type, and its value is range-checked at compile time. | literal_range_checked |
| s8.54 | Type inference | A string literal is `String`, or `Str` in check mode against `Str` ([§3](#string-literals)). | str_literal_beside_ref_string |
| s8.55 | Type inference | `true` and `false` are `bool`. | every cell |
| s8.56 | Type inference | Operands of different numeric types follow [§5 Mixed-width operands](#mixed-width-operands). | §5 Mixed-width cells |
| s8.57 | Type inference | An assignment, an argument, a return and an annotation fix a literal's type. | literal_inference (take_i32, u64, h ^ byte, s += y, rods |= bit) |
| s8.58 | Type inference | An arithmetic, bitwise or comparison operand fixes it only when the other operand's type does not widen into the default (`i64` or `f64`): b | literal_inference (take_i32, u64, h ^ byte, s += y, rods |= bit) |
| s8.59 | Type inference | Beside a type that does widen, the literal keeps its default and the narrower side widens, so with `byte: u8`, `let mut h = 2166136261; h =  | literal_inference (take_i32, u64, h ^ byte, s += y, rods |= bit) |
| s8.60 | Type inference | Write `let mut b: i32 = 0` when the narrower type is meant. | literal_inference (take_i32, u64, h ^ byte, s += y, rods |= bit) |
| s8.61 | Type inference | A compound assignment whose target's type is already fixed is an assignment to that target, so an open literal operand takes the target's ty | literal_inference (take_i32, u64, h ^ byte, s += y, rods |= bit) |
| s8.62 | Type inference | The rule looks at types only, never at the literal's value (review-thread rulings 2026-11-08, B-2026-11-07-57, B-2026-11-08-18). | literal_inference (take_i32, u64, h ^ byte, s += y, rods |= bit) |
| s8.63 | Type inference | An argument whose parameter type is a generic `T` fixes nothing by itself: when the call's result carries `T`, whether it returns `T` itself | literal_inference (id(5), id(200), id(3.14)); tests/scan_state_literal_open.rs (scan) |
| s8.64 | Type inference | **A join of literals is a literal.** A block's value is its tail's. | join_of_literals, join_of_literals_range_checked, join_int_and_float_mismatch |
| s8.65 | Type inference | An `if` with an `else`, or a `match`, whose every branch value is an unsuffixed literal of one kind (integer or float), or a block that ends | join_of_literals, join_of_literals_range_checked, join_int_and_float_mismatch |
| s8.66 | Type inference | Written inline as an operand it takes the other operand's type, so with `a: i32`, `a + if c { 1 } else { 0 }` is `i32`, as `a + 1` is. | join_of_literals, join_of_literals_range_checked, join_int_and_float_mismatch |
| s8.67 | Type inference | Bound, it stays open like `let x = 1`, so `let x = if c { 1 } else { 0 }; take_i32(x)` makes `x` an `i32`. | join_of_literals, join_of_literals_range_checked, join_int_and_float_mismatch |
| s8.68 | Type inference | A branch of a fixed type fixes the join as it would fix a bare literal unified with it. | join_of_literals, join_of_literals_range_checked, join_int_and_float_mismatch |
| s8.69 | Type inference | Every branch's literal is range-checked against the type the join is solved to, and a join that mixes integer and float literals is the exis | join_of_literals, join_of_literals_range_checked, join_int_and_float_mismatch |
| s8.70 | Type inference | A string literal follows the same rule: a branch that is a string literal, beside a branch of type `Str`, is a `Str`, as it would be against | str_literal_beside_ref_string |
| s8.71 | Type inference | Beside a `ref String` it is the existing mismatch, because a `ref String` is not a `Str` outside a call argument ([Strings](#strings)). | str_literal_beside_ref_string |
| s8.72 | Type inference | The fix depends on the other branches: beside a `Str` view it writes `.as_str()` on the `ref String` branch, making every branch a `Str` (re | str_literal_beside_ref_string (.clone() fix); NEW str_view_branch_beside_ref_string_refused (.as_str()) |
| s8.73 | Type inference | - **Variables.** `Γ ⊢ x ⇒ Γ(x)`. | every cell |
| s8.74 | Type inference | Variables synthesize and then subsume. | every cell |
| s8.75 | Type inference | - **`let`.** `let x = e` synthesizes and binds. | every cell |
| s8.76 | Type inference | `let x: T = e` checks `e ⇐ T` and binds at `T`. | every cell |
| s8.77 | Type inference | - **`if`.** The condition checks against `bool`. | NEW if_condition_not_bool_refused |
| s8.78 | Type inference | In check mode against `τ`, both branches check against `τ`. | join_of_literals_range_checked |
| s8.79 | Type inference | In synthesis mode, the result is the branches' least upper bound, and it is a compile error if there is none: Kāra has no `Any` type and doe | lub_anything_else_error, lub_differing_args_error |
| s8.80 | Type inference | - **`match`.** Like `if`, over all arms. | lub_anything_else_error |
| s8.81 | Type inference | Pattern bindings take their types from the scrutinee. | every match cell |
| s8.82 | Type inference | Exhaustiveness is a separate check ([§5](#5-types)). | §5 exhaustiveness cells |
| s8.83 | Type inference | - **Calls.** Synthesize the callee's type `Fn(P1, ..., Pn) -> R`, check each argument against its parameter type, and synthesize `R`. | literal_inference |
| s8.84 | Type inference | The callee's effects join the caller's ([§12](#12-effects)). | §12 effects cells |
| s8.85 | Type inference | - **Method calls.** `e.m(args)` synthesizes `e ⇒ τ`, resolves `m` ([§9](#9-traits)), and continues as a call. | bounds_body_uses_only_bounds |
| s8.86 | Type inference | - **Closures.** In check mode against `Fn(A1, ..., An) -> R`, each parameter takes its `Ai` and the body checks against `R`. | NEW closure_checked_against_fn_type |
| s8.87 | Type inference | In synthesis mode, each parameter needs an annotation or a type that later uses in the same function body fix; a parameter still unknown at  | closure_param_uninferred_error; tests/closure_param_operator_later_use.rs; FAILS for a later use at a narrower type: B-2026-11-24-15 |
| s8.88 | Type inference | - **Struct literals.** In check mode, each field checks against its declared type. | NEW struct_literal_field_checked; field_shorthand |
| s8.89 | Type inference | In synthesis mode, the literal names the struct. | NEW struct_literal_field_checked; field_shorthand |
| s8.90 | Type inference | **Field shorthand:** `Point { x, y }` means `Point { x: x, y: y }`, and the forms mix: `User { name, email, password_hash: hash(password) }` | field_shorthand |
| s8.91 | Type inference | - **Casts.** `e as T` synthesizes `T`. | NEW cast_synthesizes_target |
| s8.92 | Type inference | Only the numeric casts of [§5](#5-types) are legal outside `unsafe`. | NEW cast_non_numeric_refused |
| s8.93 | Type inference | **A collection's element type is not solved to a `ref` of a `Copy` type.** When an unannotated `Vec`, `Map`, `Set` or other collection's ele | collection_elem_not_ref_of_copy |
| s8.94 | Type inference | So `for x in nums.iter() { m.insert(x, 1) }` builds a `Map[i64, i64]`, as Rust's `for &x` idiom does. | collection_elem_not_ref_of_copy |
| s8.95 | Type inference | A written `Map[ref i64, V]` keeps its references (review-thread ruling 2026-11-08, B-2026-11-08-19), and they refer to the elements of `nums | collection_elem_not_ref_of_copy |
| s8.96 | Type inference | **Type variables are solved within one function body.** A generic call, a constructor whose parameters are unknown (`Vec.new()`), a numeric  | literal_inference, unsolved_var_error |
| s8.97 | Type inference | Any later use in the same function body may solve it by unification: | literal_inference, unsolved_var_error |
| s8.98 | Type inference | - **Literals default last.** A numeric type variable takes its default (`i64`, `f64`) only at the end of the body, after every use has had i | literal_inference; NEW filled_literal_defaults_last |
| s8.99 | Type inference | `Vec.filled(5, 0)` with no other use is `Vec[i64]`; `let x = identity(3.14);` is `f64`. | literal_inference (id(3.14)); NEW filled_literal_defaults_last (Vec.filled) |
| s8.100 | Type inference | - **No generalization at `let`.** A `let`-bound value, a closure included, has one type. | no_generalization_at_let |
| s8.101 | Type inference | - **An unsolved variable is an error at the end of the body**: `cannot infer type ...; annotation needed`, pointing at the expression that i | unsolved_var_error |
| s8.102 | Type inference | - **Inference never crosses a function boundary.** Signatures are always written, so a body never changes the types its callers see, and a c | structural (signatures are written); no cell |
| s8.103 | Type inference | **`Never` never silently widens to `()`.** When a `Never` value is the only thing that fixes a type variable, the variable is `Never`: | never_inference |
| s8.104 | Type inference | Two cases: a type variable whose only constraint is a `Never` value is `Never`, and a closure, function or block whose body always diverges  | never_inference |
| s8.105 | Type inference | Both follow from `Never ≤ T` for every `T`: `Never` is the only solution that meets every constraint. | never_inference |
| s8.106 | Type inference | Falling back to `()` would make a value's type depend on whether the caller annotated it, and that would change monomorphizations silently. | never_inference |
| s8.107 | Type inference | When a variable has both a `Never` constraint and a concrete one, the concrete type wins by the least-upper-bound rule (`LUB(Never, T) = T`) | never_inference |
| s8.108 | Type inference | Where the expected type is already known, a `Never` value simply converts to it. | never_inference |
| s8.109 | Type inference | The rule above governs only what a variable solves to when nothing else fixes it. | never_inference |
| s8.110 | Least upper bound | `LUB(A, B)` joins two branch types in synthesis mode (`if`, `match`). | lub_unifies_unknown_args |
| s8.111 | Least upper bound | The first matching rule applies: | header |
| s8.112 | Least upper bound | 0. | header |
| s8.113 | Least upper bound | **`Never` is the bottom.** `LUB(Never, T) = T` and `LUB(T, Never) = T`. | never_inference |
| s8.114 | Least upper bound | `Never` is the type of expressions that produce no value: `panic(...)`, `return`, `break`, `continue`, `todo()`, `unreachable()`, a `loop` w | never_inference |
| s8.115 | Least upper bound | A diverging branch contributes nothing to the join. 1. | never_inference |
| s8.116 | Least upper bound | **Identical types.** `LUB(T, T) = T`. 2. | join_of_literals |
| s8.117 | Least upper bound | **Same generic type, some arguments unknown.** Unify the unknown arguments: `LUB(Result[i64, ?E], Result[?T, String]) = Result[i64, String]` | lub_unifies_unknown_args |
| s8.118 | Least upper bound | A conflict is a type mismatch at the branch. 3. | lub_differing_args_error |
| s8.119 | Least upper bound | **Same generic type, known arguments that differ.** No LUB: `LUB(Vec[i64], Vec[String])` is an error. | lub_differing_args_error |
| s8.120 | Least upper bound | Annotate the binding to switch to check mode. 4. | rationale or cross-reference, no cell |
| s8.121 | Least upper bound | **Anything else.** No LUB: `type mismatch in branches: expected <first-branch-type>, found <other-branch-type>`, with a note listing every b | lub_anything_else_error |
| s8.122 | Least upper bound | For `match`, the join runs left to right: the first arm sets the running type, and each later arm joins it. | NEW match_first_arm_without_lub_is_error_site |
| s8.123 | Least upper bound | The first pair with no LUB is the error site. | NEW match_first_arm_without_lub_is_error_site |
| s8.124 | Bounds and generic bodies | **A generic body uses only what its bounds provide.** Inside `fn f[T: Ord + Display](...)`, a value of type `T` supports the methods and ope | bounds_body_uses_only_bounds |
| s8.125 | Bounds and generic bodies | The body is type-checked once, against its bounds. | bounds_body_uses_only_bounds |
| s8.126 | Bounds and generic bodies | **So instantiation adds no type errors.** Every instance of a well-typed generic body is well-typed. | bounds_body_uses_only_bounds |
| s8.127 | Bounds and generic bodies | What does run per instance: | header |
| s8.128 | Bounds and generic bodies | - the view checks of [core-semantics.md §5.8](core-semantics.md#5-references-and-views-c5), because a type parameter may be instantiated wit | core-semantics §5.8 view cells |
| s8.129 | Bounds and generic bodies | - effect computation: a call through a bound has the effects its trait method declares, or, when the trait method declares none, the effects | §12 trait-method effect cells |
| s8.130 | Bounds and generic bodies | **Bounds are checked at the call.** When a call solves `T` to a concrete type, each bound must have an applicable impl, chosen under the coh | bound_unsatisfied_at_call |
| s8.131 | Bounds and generic bodies | A missing impl is an error at the call site, naming the type and the unsatisfied bound and suggesting where an impl could go. | bound_unsatisfied_at_call |
| s8.132 | Bounds and generic bodies | This is what lets `Map[K, V]` rely on `K: Hash + Eq`. | NEW map_key_bound_checked_at_call |
| s8.133 | Bounds and generic bodies | `CrossTask`, the bound that gates channels and tasks, is computed by the compiler and cannot be implemented by hand ([§9](#9-traits)). | NEW crosstask_impl_refused |
| s8.134 | Bounds and generic bodies | **Comparison and interpolation without the bound (0.8.x).** `==`, `!=`, `<`, `<=`, `>`, `>=` and an f-string interpolation on a value of an  | unbounded_compare_deprecated, unbounded_compare_missing_impl_at_call, unbounded_interp_missing_display |
| s8.135 | Bounds and generic bodies | In 0.8.x each is a `deprecated` warning at the body whose machine fix adds the bound (`T: PartialEq`, `T: PartialOrd`, `T: Display`) where ` | unbounded_compare_deprecated, unbounded_compare_missing_impl_at_call, unbounded_interp_missing_display |
| s8.136 | Bounds and generic bodies | Until then the requirement is checked at each call as if the bound were written: a call that gives `T` a type without the trait, directly or | unbounded_compare_deprecated, unbounded_compare_missing_impl_at_call, unbounded_interp_missing_display |
| s8.137 | Bounds and generic bodies | So a `Secret[T]`, which has neither `Display` nor `PartialEq`, cannot be printed or compared through an unbounded generic (B-2026-11-18-25). | unbounded_compare_deprecated, unbounded_compare_missing_impl_at_call, unbounded_interp_missing_display |
| s8.138 | `where` clauses | Bounds may move to a `where` clause after the return type and effects, when they are long or many: | where_clauses (inline + where on largest/process) |
| s8.139 | `where` clauses | - **Inline and `where` bounds mix** in one declaration: | where_clauses (inline + where on largest/process) |
| s8.140 | `where` clauses | ```kara fn process[T: Ord, U](items: own Vec[T]) -> U where U: From[T] + Display { ... } ``` | where_clauses (inline + where on largest/process) |
| s8.141 | `where` clauses | - **On `impl` blocks**, with the same syntax: | where_clauses (impl Display) |
| s8.142 | `where` clauses | ```kara impl[T] Display for Pair[T] where T: Display { fn to_string(self) -> String { ... } } ``` | where_clauses (impl Display) |
| s8.143 | `where` clauses | - **On `struct` and `enum` definitions**, allowed but rarely needed: | where_clauses, where_on_struct_enforced |
| s8.144 | `where` clauses | ```kara struct SortedPair[T] where T: Ord { first: T, second: T, } ``` | where_clauses, where_on_struct_enforced |
| s8.145 | `where` clauses | - **Order in a function declaration:** signature, then effects, then `where`, then the body: | where_after_effects, where_before_effects_refused |
| s8.146 | `where` clauses | ```kara fn binary_search[T](haystack: Vec[T], needle: T) -> Option[i64] with reads(SearchIndex) where T: Ord { ... } ``` | where_after_effects, where_before_effects_refused |
| s8.147 | `where` clauses | - **`where` holds type bounds only.** | where_type_bounds_only |
| s8.148 | Default generic arguments | A generic parameter of a type or a trait may have a default. | default_generic_args, default_before_required_refused |
| s8.149 | Default generic arguments | An argument left out takes the default: | default_generic_args, default_before_required_refused |
| s8.150 | Default generic arguments | Parameters with defaults come after those without. | default_generic_args, default_before_required_refused |
| s8.151 | Default generic arguments | The operator traits rely on this ([§9](#operator-traits)), and so does the hasher parameter of `Map` and `Set` ([library/collections.md](lib | default_generic_args, default_before_required_refused |
| s8.152 | Default generic arguments | A function's generic parameters take no defaults, because a function never takes type arguments at a call ([Generic parameters and arguments | fn_generic_default_refused |
| s8.153 | Const generic parameters | A generic list may hold const parameters beside type parameters. | const_generics_array (N) |
| s8.154 | Const generic parameters | A const parameter is a value fixed per instance: | const_generics_array (N) |
| s8.155 | Const generic parameters | The `const` prefix is what makes the parameter a value. | const_generics_array (N) |
| s8.156 | Const generic parameters | Its name is Const class, and a single letter such as `N` is allowed ([CN-7](#rules)). | const_generics_array (N) |
| s8.157 | Const generic parameters | **Permitted types:** `i64` (and `i8`, `i16`, `i32`, `i128` where a bit width matters), `bool`, `char`, and enums whose variants all have no  | const_param_permitted_types |
| s8.158 | Const generic parameters | An `i128` argument is written as a literal within the `i64` range. | i128_const_param |
| s8.159 | Const generic parameters | Other types (`String`, floats, unsigned integers, structs, enums with fields, distinct types) are rejected at the declaration: "type 'f64' i | const_param_float_refused, const_param_type_refused |
| s8.160 | Const generic parameters | Floats are excluded because NaN payloads make equality of instances ambiguous; the others because a const argument must have a finite repres | rationale or cross-reference, no cell |
| s8.161 | Const generic parameters | `usize` is not permitted: sizes, counts and capacities are `i64` in Kāra ([§5](#5-types)), in the type system too. | const_param_usize_refused |
| s8.162 | Const generic parameters | **Arguments** are integer literals, `true`/`false`, character literals and qualified fieldless variants: | const_param_permitted_types |
| s8.163 | Const generic parameters | A negative argument is accepted by the parameter (`const N: i64` takes `-1`); whether it makes sense is the type's concern. | const_negative_arg_accepted |
| s8.164 | Const generic parameters | `Array[T, N]` requires `N >= 0` as a bound on its definition. | array_negative_len_refused |
| s8.165 | Const generic parameters | **Constant expressions as arguments.** An argument may be any constant expression of the parameter's type: `Array[T, N + 1]`, `Matrix[T, M,  | not implemented in 0.8 (the page says so, or the command does not exist yet) (s8.180) |
| s8.166 | Const generic parameters | These are evaluated during type checking, with the same expressions allowed as in a `const` initializer ([Constants](#constants)). | not implemented in 0.8 (the page says so, or the command does not exist yet) (s8.180) |
| s8.167 | Const generic parameters | Calls wait for `const fn` (comptime track). | not implemented in 0.8 (the page says so, or the command does not exist yet) (s8.180) |
| s8.168 | Const generic parameters | A run-time value in an argument position is `const generic argument must be a compile-time constant expression`. | const_runtime_arg_refused |
| s8.169 | Const generic parameters | Evaluation uses checked arithmetic: overflow is a compile error. | not implemented in 0.8 (the page says so, or the command does not exist yet) |
| s8.170 | Const generic parameters | **Constant expressions as bounds.** `where` clauses may constrain const parameters: | not implemented in 0.8 (the page says so, or the command does not exist yet) (s8.180) |
| s8.171 | Const generic parameters | The compiler evaluates each bound at each instance, directly: no SMT solver and no symbolic reasoning across calls. | not implemented in 0.8 (the page says so, or the command does not exist yet) (s8.180) |
| s8.172 | Const generic parameters | A failed bound is reported at the call site with the predicate as written and the argument values: "const constraint violated: `N > 2` evalu | not implemented in 0.8 (the page says so, or the command does not exist yet) (s8.180) |
| s8.173 | Const generic parameters | **Inference.** A const parameter that appears in a parameter type is solved from the argument, like a type variable: `first(my_array)` solve | const_generics_array |
| s8.174 | Const generic parameters | A const parameter that appears only in the return type or in `where` clauses is solved from the expected type, since there are no call-site  | NEW const_param_from_expected_type; const_param_uninferred |
| s8.175 | Const generic parameters | The same `N` solved to two values is a const-argument mismatch at the conflicting position. | const_mismatch |
| s8.176 | Const generic parameters | **Instances.** Each distinct tuple of type and const arguments is a separate instance ([Monomorphization](#monomorphization)). | instances_per_type |
| s8.177 | Const generic parameters | Const parameters are erased to their values: they are not run-time data and do not appear in dispatch tables. | rationale or cross-reference, no cell |
| s8.178 | Const generic parameters | `karac query monomorphization` counts const arguments in the instance key `(T1..Tk, C1..Cm)`, so a function with const parameters called wit | tests/codegen_queries.rs |
| s8.179 | Const generic parameters | Only `Array[T, N]` is implemented so far. | statement of scope; the refusals above |
| s8.180 | Const generic parameters | Constant-expression arguments, const bounds in `where`, fieldless-enum parameters and solving several const parameters at once are not yet i | statement of scope; the refusals above |
| s8.181 | Subtyping and variance | Kāra has nominal types and little subtyping. | rationale or cross-reference, no cell |
| s8.182 | Subtyping and variance | The subtyping relation has exactly these sources: | header |
| s8.183 | Subtyping and variance | - **`Never ≤ T`** for every type `T`. | never_inference |
| s8.184 | Subtyping and variance | - **Function types.** Contravariant in parameter types, covariant in the result, covariant in the effect set: a function with fewer effects  | fn_subtyping; NEW more_effects_where_fewer_refused |
| s8.185 | Subtyping and variance | - **Function kinds.** An `Fn` may be used where a `MutFn` or `OnceFn` is expected, and a `MutFn` where an `OnceFn` is expected ([core-semant | fn_subtyping, mutfn_where_fn_refused; NEW oncefn_where_fn_refused |
| s8.186 | Subtyping and variance | - **Covariant built-in types**, below. | NEW covariant_builtins_and_fn_effects |
| s8.187 | Subtyping and variance | **Variance is never inferred from a type's structure.** It is fixed by position, and for built-in types by this specification. | rationale or cross-reference, no cell |
| s8.188 | Subtyping and variance | Inference can produce the wrong answer in the presence of interior mutability or generic instantiation, and explicit declaration is worth it | rationale or cross-reference, no cell |
| s8.189 | Subtyping and variance | **Variance by position:** | see rows below |
| s8.190 | Subtyping and variance | / Position / Variance / | see rows below |
| s8.191 | Subtyping and variance | / `mut ref T` target / invariant / | see rows below |
| s8.192 | Subtyping and variance | / `ref T` target / covariant / | see rows below |
| s8.193 | Subtyping and variance | / Function result / covariant / | see rows below |
| s8.194 | Subtyping and variance | / Effect set of a function type / covariant / | see rows below |
| s8.195 | Subtyping and variance | / Function parameter / contravariant / | see rows below |
| s8.196 | Subtyping and variance | **`mut ref T` is invariant.** `mut ref Sub` is not a subtype of `mut ref Super`, nor the reverse. | FAILS: a `mut ref` is not invariant over effect sets: B-2026-11-24-16 (no cell until fixed) |
| s8.197 | Subtyping and variance | A `mut ref Super` can write any `Super`, including one that is not a valid `Sub`; if `mut ref Sub` could become `mut ref Super`, a write thr | FAILS: a `mut ref` is not invariant over effect sets: B-2026-11-24-16 (no cell until fixed) |
| s8.198 | Subtyping and variance | A function taking `mut ref T` therefore receives exactly `T`. | FAILS: a `mut ref` is not invariant over effect sets: B-2026-11-24-16 (no cell until fixed) |
| s8.199 | Subtyping and variance | **Built-in types:** | header |
| s8.200 | Subtyping and variance | / Type / Variance / | header |
| s8.201 | Subtyping and variance | / `Option[T]`, `Result[T, E]`, tuples / covariant in every parameter / | NEW covariant_builtins_and_fn_effects (Option, tuple) |
| s8.202 | Subtyping and variance | / `TaskHandle[T]` / covariant (it only yields a `T`) / | GAP: TaskHandle covariance not covered |
| s8.203 | Subtyping and variance | / `Vec`, `Slice`, `Array`, `Map`, `Set`, `VecDeque`, `SortedMap`, `SortedSet` / invariant / | accepted on a move, where invariance is unobservable; the observable case is B-2026-11-24-16 |
| s8.204 | Subtyping and variance | / `Sender[T]`, `Receiver[T]` / invariant / | accepted on a move, where invariance is unobservable; the observable case is B-2026-11-24-16 |
| s8.205 | Subtyping and variance | / `Atomic[T]`, `Mutex[T]` / invariant (interior mutability) / | accepted on a move, where invariance is unobservable; the observable case is B-2026-11-24-16 |
| s8.206 | Subtyping and variance | / `MaybeUninit[T]`, `PhantomData[T]` / invariant / | accepted on a move, where invariance is unobservable; the observable case is B-2026-11-24-16 |
| s8.207 | Subtyping and variance | **User-declared generic types are invariant in every parameter.** Variance markers (`+T`, `-T`) are deferred ([deferred.md](deferred.md#unsc | same as above |
| s8.208 | Monomorphization | Kāra emits **one specialized implementation per tuple of type and const arguments** for every generic: user functions and types, and the sta | instances_per_type |
| s8.209 | Monomorphization | There is no type erasure, no vtable dispatch for generics and no function-pointer indirection inside collections. | instances_per_type |
| s8.210 | Monomorphization | This is a design property of code generation, not an implementation detail: it shapes the ABI, what the optimizer can see, and the runtime. | instances_per_type |
| s8.211 | Monomorphization | **What monomorphizes.** | instances_per_type |
| s8.212 | Monomorphization | - **Generic functions.** `fn identity[T](x: own T) -> T` called with `i64` and `String` produces two functions. | instances_per_type |
| s8.213 | Monomorphization | Instances are deduplicated by their argument tuple, so `identity` at `i64` is emitted once however many calls use it. | instances_per_type |
| s8.214 | Monomorphization | - **Generic types.** `Pair[i64, String]` and `Pair[String, String]` are two compiled types with their own methods. | instances_per_type |
| s8.215 | Monomorphization | - **Standard collections.** Each is compiled per argument tuple, with `K`'s hash and equality inlined into `Map[K, V]`, so the optimizer see | instances_per_type |
| s8.216 | Monomorphization | - **Trait method calls in generic code.** In `fn process[T: Display](xs: Slice[T])`, each instance calls its `T`'s `to_string` directly. | instances_per_type |
| s8.217 | Monomorphization | No vtable. | instances_per_type |
| s8.218 | Monomorphization | **What does not.** | §9 dyn cells |
| s8.219 | Monomorphization | - **[`dyn Trait`](#dyn-trait)** dispatches through a table of methods by design. | §9 dyn cells |
| s8.220 | Monomorphization | It is a separate feature from generics, chosen at the type. | §9 dyn cells |
| s8.221 | Monomorphization | - **Effects are not part of the instance key.** An instance is chosen by its type and const arguments alone. | §12 effects cells |
| s8.222 | Monomorphization | Its effects are computed on the instance, and the effects of a function passed to a non-escaping parameter are charged to each caller ([core | §12 effects cells |
| s8.223 | Monomorphization | **The standard library ships as source.** Collection code is compiled into each program at the instances it uses, like Rust's `std::collecti | implementation property, no cell |
| s8.224 | Monomorphization | The runtime library holds only non-generic primitives: panic support, the allocator interface, FFI bridges and OS bindings. | implementation property, no cell |
| s8.225 | Monomorphization | **Polymorphic recursion is an error.** A generic whose recursive path calls it again at a type argument that strictly contains its own param | polymorphic_recursion_refused |
| s8.226 | Monomorphization | `karac check` refuses it at the recursive call, naming the cycle and the growing argument. | polymorphic_recursion_refused |
| s8.227 | Monomorphization | Since `check` finds the cycle in the call graph rather than by expanding instances, it never runs forever on such a program. | polymorphic_recursion_refused |
| s8.228 | Monomorphization | Recursion at the same or a smaller type is ordinary and compiles. | recursion_same_type_ok |
| s8.229 | Monomorphization | **Cost.** Binary size grows with the number of distinct instances: a program using `Map[i64, i64]`, `Map[String, i64]`, `Map[String, Vec[i64 | rationale or cross-reference, no cell |
| s8.230 | Monomorphization | Stripping, LTO and dead-code elimination remove unused methods per instance, but each instance has a floor. | rationale or cross-reference, no cell |
| s8.231 | Monomorphization | A typical small program is about Rust's size; one with many instances grows linearly. | rationale or cross-reference, no cell |
| s8.232 | Monomorphization | In return, collections run at close to Rust's speed, the optimizer reaches into collection bodies (constant folding through hashing, vectori | rationale or cross-reference, no cell |
| s8.233 | Monomorphization | Kāra chooses run-time speed over archive size. | rationale or cross-reference, no cell |
| s8.234 | Monomorphization | There is no dynamic linking ([deferred.md](deferred.md), permanent omissions); monomorphization is the alternative to plugin-style extension | rationale or cross-reference, no cell |
| s8.235 | Monomorphization | --- | rule |
