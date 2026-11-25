# design.md §5 Types sentence-to-cell map (lines 984-1912)

One row per sentence of design.md §5 Types, as of 2026-11-24, naming the cell in this directory (or the test elsewhere) that covers it. NEW marks a cell added by this audit; FAILS marks what is not yet true of the compiler, with its ledger row; FIXED marks a gap this audit fixed. Warn-only behaviour (a lint that warns and lets the program build) is mapped to its test, since a cases.py cell cannot assert a warning.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| s5.1 | 5. Types | Kāra has no classes and no inheritance. | statement of absence; struct and enum cells below |
| s5.2 | 5. Types | Data is structs and enums, built from primitives, tuples and arrays; behaviour is `impl` blocks and [traits](#9-traits). | struct and enum cells below; traits in §9 |
| s5.3 | 5. Types | A method call and its qualified form are the same call: | NEW method_call_is_its_qualified_form |
| s5.4 | 5. Types | `user.validate()` is `User.validate(user)`. | NEW method_call_is_its_qualified_form |
| s5.5 | 5. Types | Generic types are written with square brackets (`Vec[i32]`, `Map[String, i64]`) and are monomorphized ([§8](#8-type-inference-and-generics)) | type_aliases_are_transparent (`Map[T, i64]`), §8 generics cells |
| s5.6 | 5. Types | How values of these types are copied, moved, borrowed and destroyed is set by [core-semantics.md §1–§3](core-semantics.md#1-values) and [§7] | rationale or cross-reference, no cell |
| s5.7 | 5. Types | This section says what each type is, how its values are written, and how they are matched. | rationale or cross-reference, no cell |
| s5.8 | Primitive types | / Type / What it is / /---/---/ | header |
| s5.9 | Primitive types | / `i8` `i16` `i32` `i64` `i128` / Signed two's-complement integers. | NEW integer_and_float_literals_default_to_i64_and_f64 (i128), integer_as_casts_sign_extend_truncate_low_bits_kept |
| s5.10 | Primitive types | `i64` is the default integer type and the index type. / | NEW integer_and_float_literals_default_to_i64_and_f64; a_computed_negative_index_panics (i64 index) |
| s5.11 | Primitive types | / `u8` `u16` `u32` `u64` `u128` / Unsigned integers. / | NEW integer_and_float_literals_default_to_i64_and_f64 (u128), checked_saturating_and_overflowing_families (u8) |
| s5.12 | Primitive types | / `isize` `usize` / Pointer-sized integers for FFI only (C's `ptrdiff_t` and `size_t`). | usize_to_i64_at_a_binding_needs_as; NEW isize_usize_widen_as_32_to_64_bits |
| s5.13 | Primitive types | Kāra code uses `i64` for every size, length, count and index, I/O included. / | usize_to_i64_at_a_binding_needs_as (the refusal says so); i64 lengths in every cell |
| s5.14 | Primitive types | / `f32` `f64` / IEEE 754 binary floats. | floats_print_with_a_or_exponent_nan_nan, f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.15 | Primitive types | `f64` is the default float type. / | NEW integer_and_float_literals_default_to_i64_and_f64 (1.0 / 3.0 prints as f64) |
| s5.16 | Primitive types | / `bool` / `true` or `false`. / | NEW bool_enum_and_struct_matches_are_exhaustive |
| s5.17 | Primitive types | / `char` / One Unicode scalar value: | char_try_from_rejects_a_surrogate_accepts_a_scalar |
| s5.18 | Primitive types | `0..=0xD7FF` or `0xE000..=0x10FFFF`. / | char_try_from_rejects_a_surrogate_accepts_a_scalar |
| s5.19 | Primitive types | / `()` / The unit type. | NEW unit_value_and_never_in_expression_positions |
| s5.20 | Primitive types | Its one value is also written `()`. / | NEW unit_value_and_never_in_expression_positions |
| s5.21 | Primitive types | / `Never` / The bottom type, with no values. | NEW unit_value_and_never_in_expression_positions, NEW empty_match_on_never_is_exhaustive |
| s5.22 | Primitive types | It is the type of `return`, `break`, `continue` and diverging calls (`panic()`, `unreachable()`, `todo()`), and it coerces to any type, so i | NEW unit_value_and_never_in_expression_positions, NEW empty_match_on_never_is_exhaustive |
| s5.23 | Primitive types | Every primitive is `Copy` ([core-semantics.md §1.1](core-semantics.md#1-values)). | NEW primitives_copy_to_string_and_clone |
| s5.24 | Primitive types | Every scalar primitive has `to_string(self) -> String`, which renders it the way f-string interpolation does, and `clone(self) -> Self`, whi | NEW primitives_copy_to_string_and_clone |
| s5.25 | Primitive types | The numeric primitives have a closed method surface: calling a method a numeric type does not have is an error, never a silently accepted no | a_numeric_primitive_has_a_closed_method_surface, unsigned_abs_is_a_no_method_error |
| s5.26 | Primitive types | `String` and `Str` are under [Strings](#strings); `F32` and `F64` are under [Floats](#floats). | rationale or cross-reference, no cell |
| s5.27 | Numeric literals | How numeric literals are written (bases, `_`, exponents, suffixes) is in [§3](#numeric-character-and-boolean-literals). | rationale or cross-reference, no cell |
| s5.28 | Numeric literals | This section says what type they get. | rationale or cross-reference, no cell |
| s5.29 | Numeric literals | **Default types.** Integer literals (`42`) default to `i64`. | NEW integer_and_float_literals_default_to_i64_and_f64 |
| s5.30 | Numeric literals | Float literals (`3.14`) default to `f64`. | NEW integer_and_float_literals_default_to_i64_and_f64 |
| s5.31 | Numeric literals | An annotation overrides the default: | an_integer_suffix_that_fits_takes_the_annotation_i32_5i64 (annotation types the binding) |
| s5.32 | Numeric literals | `let x: i32 = 42;`. | an_integer_suffix_that_fits_takes_the_annotation_i32_5i64 (annotation types the binding) |
| s5.33 | Numeric literals | Literal types are settled by inference over the whole function body ([§8](#8-type-inference-and-generics)). | NEW literal_type_settled_by_a_later_use |
| s5.34 | Numeric literals | **Literal suffixes.** A type suffix forces the type of a literal where inference cannot reach it from a binding: | NEW suffix_types_an_inline_argument |
| s5.35 | Numeric literals | `1i32`, `1u8`, `1.0f32`. | NEW suffix_types_an_inline_argument |
| s5.36 | Numeric literals | Use an annotation when there is a binding, and a suffix when the literal is inline, for example an argument: | NEW suffix_types_an_inline_argument |
| s5.37 | Numeric literals | `Vec.filled(n, 0i32)`. | NEW suffix_types_an_inline_argument |
| s5.38 | Numeric literals | A suffix that matches the default type (`42i64`, `3.14f64`) is valid, but the compiler warns; `#[allow(redundant_suffix)]` silences it. | tests/lint_own_codes.rs, tests/fix_suffix_and_cast_same_pass.rs (redundant_suffix warning and fix; the shared-folder cell was a warn-only check, not a cases.py shape) |
| s5.39 | Numeric literals | **A suffix cannot contradict a destination the compiler already knows.** Where the destination type is fixed (a binding annotation, a parame | a_float_suffix_wider_than_the_destination_is_refused_let_d_f32_0_1f64; NEW suffix_contradicting_a_parameter_refused, NEW suffix_contradicting_a_return_type_refused, NEW literal_payload_out_of_range_refused |
| s5.40 | Numeric literals | The reason is the one behind the widening table below: narrowing needs an explicit `as`. | rationale or cross-reference, no cell |
| s5.41 | Numeric literals | `let d: f32 = 0.1f64` is rejected. | a_float_suffix_wider_than_the_destination_is_refused_let_d_f32_0_1f64 |
| s5.42 | Numeric literals | Drop the suffix and let `f32` type the literal, or write `0.1f64 as f32` if the rounding is intended. | a_float_suffix_wider_than_the_destination_is_refused_let_d_f32_0_1f64 (the diagnostic names both fixes) |
| s5.43 | Numeric literals | Widening stays implicit, so `let w: f64 = 0.1f32` is accepted and holds the `f32` value widened: a suffix narrower than its destination choo | a_narrower_float_suffix_widens_let_w_f64_0_1f32 |
| s5.44 | Numeric literals | Integer suffixes are governed by the range check instead: | rationale or cross-reference, no cell |
| s5.45 | Numeric literals | `let d: u8 = 300i64` is out of range, and `let d: i32 = 5i64` fits, so the annotation types the binding. | an_integer_suffix_out_of_the_destination_s_range_is_refused_u8_300i64, an_integer_suffix_that_fits_takes_the_annotation_i32_5i64 |
| s5.46 | Numeric literals | **A literal in a binary operator takes the other operand's type**, even when that differs from the literal's default. | literal_in_an_operator_takes_the_other_operand_s_type_u8_1_overflows_a, literal_typing_i32_2_f32_2_comparison_in_i32 |
| s5.47 | Numeric literals | This holds for the arithmetic operators (`+`, `-`, `*`, `/`, `%`) and the comparisons (`<`, `<=`, `>`, `>=`, `==`, `!=`). | literal_in_an_operator_takes_the_other_operand_s_type_u8_1_overflows_a, literal_typing_i32_2_f32_2_comparison_in_i32 |
| s5.48 | Numeric literals | Elsewhere (arguments, patterns, field initializers) the literal takes the expected type, which is already precise. | NEW enum_payload_widens_and_literal_payload_is_range_checked (payload), pattern cells (literal patterns at the scrutinee's width) |
| s5.49 | Numeric literals | ```kara let i: u8 = 0; let n: i32 = 10; let x: f32 = 1.5;  let a = i + 1;       // u8: the literal takes the type of `i` let b = n * 2;      | literal_in_an_operator_takes_the_other_operand_s_type_u8_1_overflows_a, literal_typing_i32_2_f32_2_comparison_in_i32 |
| s5.50 | Integer overflow | **Integer arithmetic traps on overflow.** `+`, `-`, `*`, `/`, `%`, unary `-` and `abs()` panic when the exact result does not fit the type ( | i64_max_1_panics_wrapping_add_names_the_wrap, unary_minus_of_i32_min_panics, abs_of_i64_min_panics, in_min_1_traps_as_overflow_wrapping_div_gives_min, NEW literal_type_settled_by_a_later_use |
| s5.51 | Integer overflow | Arithmetic that can overflow contributes `panics` to the function's effects ([§12](#12-effects)). | NEW overflowing_arithmetic_is_a_panics_effect_refused, control NEW wrapping_arithmetic_has_no_panics_effect |
| s5.52 | Integer overflow | The behaviour is fully defined and the same in every build; no flag turns the check off. | statement of absence (no flag exists); every overflow cell runs in both lanes |
| s5.53 | Integer overflow | ```kara let x: i64 = i64.MAX; let y = x + 1;                  // panics: integer overflow let z = x.wrapping_add(1);      // i64.MIN: the wr | i64_max_1_panics_wrapping_add_names_the_wrap |
| s5.54 | Integer overflow | **Why trap by default.** The check costs a little on arithmetic-heavy code, and two reasons make it worth paying. | rationale or cross-reference, no cell |
| s5.55 | Integer overflow | First, silent two's-complement wrapping is a live class of logic bugs and security holes: an overflowed `count * price` or `w * h * bytes_pe | rationale or cross-reference, no cell |
| s5.56 | Integer overflow | Second, machine-written code is the code most likely to carry a latent overflow, because a model rarely reasons about the bounds of `iN` on  | rationale or cross-reference, no cell |
| s5.57 | Integer overflow | Under a wrapping default that mistake is silent. | rationale or cross-reference, no cell |
| s5.58 | Integer overflow | Under a trap it fails loudly on the first run that reaches it, and becomes a diagnostic that a program can act on. | rationale or cross-reference, no cell |
| s5.59 | Integer overflow | So the escape hatch is narrow and local: the named method families ([below](#checked-wrapping-saturating-and-overflowing-arithmetic)), at th | rationale or cross-reference, no cell |
| s5.60 | Integer overflow | A project-wide switch would remove the guarantee invisibly. | rationale or cross-reference, no cell |
| s5.61 | Integer overflow | Attribute regions that change the default for a function or block (`#[wrapping]`, `#[checked]`) are deferred ([deferred.md](deferred.md)). | deferred (deferred.md), no cell |
| s5.62 | Division, remainder and shifts | - **Division `/` truncates toward zero.** `-7 / 2 == -3`, `7 / -2 == -3`, `-7 / -2 == 3`. | truncates_toward_zero_takes_the_dividend_s_sign_euclid_forms |
| s5.63 | Division, remainder and shifts | This matches C, Java, Go and Swift and the instruction every integer CPU issues. | truncates_toward_zero_takes_the_dividend_s_sign_euclid_forms |
| s5.64 | Division, remainder and shifts | `/` is a machine-level primitive; for Euclidean division call `x.div_euclid(n)`. | truncates_toward_zero_takes_the_dividend_s_sign_euclid_forms |
| s5.65 | Division, remainder and shifts | - **Remainder `%` takes the sign of the dividend.** `-7 % 2 == -1`, `7 % -2 == 1`, `-7 % -2 == -1`. | truncates_toward_zero_takes_the_dividend_s_sign_euclid_forms |
| s5.66 | Division, remainder and shifts | Once `/` truncates, the identity `(a / b) * b + (a % b) == a` forces this. | truncates_toward_zero_takes_the_dividend_s_sign_euclid_forms |
| s5.67 | Division, remainder and shifts | The always-non-negative remainder is `x.rem_euclid(n)`; the explicit name settles "which kind of modulo is this?" at the call site. | truncates_toward_zero_takes_the_dividend_s_sign_euclid_forms |
| s5.68 | Division, remainder and shifts | - **`iN.MIN / -1` traps as overflow**, in the same family as `iN.MAX + 1`: the exact result does not fit `iN`. | in_min_1_traps_as_overflow_wrapping_div_gives_min |
| s5.69 | Division, remainder and shifts | `x.wrapping_div(-1)` wraps to `iN.MIN`. | in_min_1_traps_as_overflow_wrapping_div_gives_min |
| s5.70 | Division, remainder and shifts | **`iN.MIN % -1` traps the same way**, although its exact result, 0, fits: the hardware computes the remainder with the same division, and Ru | in_min_1_traps_the_same_way |
| s5.71 | Division, remainder and shifts | `x.wrapping_rem(-1)` returns 0. | in_min_1_traps_as_overflow_wrapping_div_gives_min (wrapping_rem) |
| s5.72 | Division, remainder and shifts | **Division by zero** is a separate trap with its own `"division by zero"` message, so the diagnostic stays specific. | division_by_zero_has_its_own_message, remainder_by_zero_has_the_division_by_zero_message |
| s5.73 | Division, remainder and shifts | - **Shift operands.** In `x << n` and `x >> n`, `n` may have any integer type, and the result has the type of `x`. | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.74 | Division, remainder and shifts | The operands are not widened to a common type ([Mixed-width operands](#mixed-width-operands)). | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.75 | Division, remainder and shifts | - **Right shift `>>` on a signed integer is arithmetic** (it extends the sign): | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.76 | Division, remainder and shifts | `(-8i32) >> 1 == -4`. | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.77 | Division, remainder and shifts | A logical shift of a signed value needs explicit casts: | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.78 | Division, remainder and shifts | `((x as u32) >> 1) as i32`. | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.79 | Division, remainder and shifts | Left shift `<<` has no signed variant. | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.80 | Division, remainder and shifts | - **A shift amount that is negative, or at least the bit width, traps.** `(x: i32) << 32` and `x << -1` panic with `"shift amount out of ran | a_negative_runtime_shift_amount_panics, a_runtime_shift_amount_of_the_bit_width_panics, shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type (`<< 31` legal) |
| s5.81 | Division, remainder and shifts | The same holds for `>>`. | a_negative_runtime_shift_amount_panics (`>>`) |
| s5.82 | Division, remainder and shifts | A constant amount out of that range is a compile-time error, as constant overflow is: | a_constant_shift_out_of_range_is_a_compile_error_1_64, a_constant_shift_out_of_range_is_a_compile_error_i8_1_8 |
| s5.83 | Division, remainder and shifts | `let y: i8 = 1 << 8;` and `let y = 1 << 64;` are refused, the amount judged at the type the shifted value ends with (review-thread ruling 20 | a_constant_shift_out_of_range_is_a_compile_error_1_64, a_constant_shift_out_of_range_is_a_compile_error_i8_1_8 |
| s5.84 | Division, remainder and shifts | `wrapping_shl` and `wrapping_shr` mask the shift amount modulo the bit width and never panic, so the masking that C performs silently must b | wrapping_shl_and_wrapping_shr_mask_the_amount |
| s5.85 | Division, remainder and shifts | - **Signedness follows the operand type.** `/`, `%`, `>>` and the ordered comparisons (`<`, `<=`, `>`, `>=`) use the source-level type of th | NEW unsigned_operands_take_unsigned_forms, shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.86 | Division, remainder and shifts | Unsigned types get unsigned division, unsigned remainder, logical right shift and unsigned comparison; signed types get the forms above. | NEW unsigned_operands_take_unsigned_forms, shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.87 | Division, remainder and shifts | So `0xFFFF_FFFFu32 < 1u32` is `false`; reading the bits as signed would invert the answer. | NEW unsigned_operands_take_unsigned_forms, shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.88 | Division, remainder and shifts | For `/`, `%` and the comparisons, when the two operands have different types, [Mixed-width operands](#mixed-width-operands) decides the comm | mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type, shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type (shift by a u8) |
| s5.89 | Checked, wrapping, saturating and overflowing arithmetic | Every integer primitive (`i8` to `i128`, `u8` to `u128`, `isize`, `usize`) has four method families, `checked_`, `wrapping_`, `saturating_`  | checked_saturating_and_overflowing_families, wrapping_shl_and_wrapping_shr_mask_the_amount |
| s5.90 | Checked, wrapping, saturating and overflowing arithmetic | They are listed in [library/core-types.md](library/core-types.md#numbers). | checked_saturating_and_overflowing_families, wrapping_shl_and_wrapping_shr_mask_the_amount |
| s5.91 | Checked, wrapping, saturating and overflowing arithmetic | A length is an `i64`, so `v.len().saturating_sub(n)` clamps at `i64.MIN`, not at 0 as Rust's `usize` does; the `len_saturating_sub` lint (wa | tests/spec_section5_checks.rs, the W0322 len_saturating_sub lint tests (warn-only; the shared-folder cell was not a cases.py shape) |
| s5.92 | Checked, wrapping, saturating and overflowing arithmetic | **`abs()`** is the magnitude of a signed integer or a float, returning `Self`. | abs_of_i64_min_panics, NEW float_abs_follows_ieee |
| s5.93 | Checked, wrapping, saturating and overflowing arithmetic | `iN.MIN.abs()` has no representable result and traps as integer overflow, like unary `-iN.MIN`. | abs_of_i64_min_panics |
| s5.94 | Checked, wrapping, saturating and overflowing arithmetic | Float `abs` follows IEEE 754. | NEW float_abs_follows_ieee |
| s5.95 | Checked, wrapping, saturating and overflowing arithmetic | Unsigned integers have no `abs`: calling it is a `no method 'abs' on type 'uN'` error. | unsigned_abs_is_a_no_method_error |
| s5.96 | Mixed-width operands | **Lossless widening is implicit; anything that can lose information needs `as`.** | mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type, i64_to_f64_at_a_binding_needs_as |
| s5.97 | Mixed-width operands | / Conversion / Implicit? / Reason / /---/---/---/ | header |
| s5.98 | Mixed-width operands | / `i8` → `i16` → `i32` → `i64` → `i128` / Yes / Always lossless / | NEW widening_at_bindings_arguments_and_compound_assignment, mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.99 | Mixed-width operands | / `u8` → `u16` → `u32` → `u64` → `u128` / Yes / Always lossless / | NEW widening_at_bindings_arguments_and_compound_assignment, mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.100 | Mixed-width operands | / `u8` → `i16`, `u16` → `i32`, `u32` → `i64`, `u64` → `i128` / Yes / An unsigned value fits the next signed size / | NEW widening_at_bindings_arguments_and_compound_assignment, mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.101 | Mixed-width operands | / `i8`, `i16`, `i32` → `f64` / Yes / Up to 32 bits fit `f64`'s 53-bit mantissa / | NEW widening_at_bindings_arguments_and_compound_assignment, mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.102 | Mixed-width operands | / `u8`, `u16`, `u32` → `f64` / Yes / Up to 32 bits fit `f64`'s 53-bit mantissa / | NEW widening_at_bindings_arguments_and_compound_assignment, mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.103 | Mixed-width operands | / `f32` → `f64` / Yes / Always lossless / | NEW widening_at_bindings_arguments_and_compound_assignment, mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.104 | Mixed-width operands | / `i64`, `u64` → `f64` / **No**: write `as f64` / Can lose precision above 2^53 / | i64_to_f64_at_a_binding_needs_as |
| s5.105 | Mixed-width operands | / Any narrowing / **No**: write `as` / Data loss is possible / | u64_i64_is_an_error, NEW return_some_of_u64_into_option_i64_refused |
| s5.106 | Mixed-width operands | Widening composes: | NEW widening_at_bindings_arguments_and_compound_assignment (u8 into an i64 parameter) |
| s5.107 | Mixed-width operands | `u8` → `i64` is lossless because `u8` → `i16` → `i64` is. | NEW widening_at_bindings_arguments_and_compound_assignment (u8 into an i64 parameter) |
| s5.108 | Mixed-width operands | **`isize` and `usize` widen as if they had 32 to 64 bits.** Their width is the target's pointer width, and a program must be type-correct on | NEW isize_usize_widen_as_32_to_64_bits |
| s5.109 | Mixed-width operands | Into them: | NEW isize_usize_widen_as_32_to_64_bits |
| s5.110 | Mixed-width operands | `i8` to `i32` and `u8`, `u16` widen to `isize`; `u8` to `u32` widen to `usize`. | NEW isize_usize_widen_as_32_to_64_bits |
| s5.111 | Mixed-width operands | Out of them: | NEW isize_usize_widen_as_32_to_64_bits |
| s5.112 | Mixed-width operands | `isize` widens to `i64` and `i128`; `usize` widens to `u64`, `u128` and `i128`. | NEW isize_usize_widen_as_32_to_64_bits |
| s5.113 | Mixed-width operands | Anything else needs `as`, among them `usize` → `i64`, `i64` → `isize` and `u32` → `isize`. | usize_to_i64_at_a_binding_needs_as, NEW i64_to_isize_needs_as_refused, NEW u32_to_isize_needs_as_refused |
| s5.114 | Mixed-width operands | The rows apply at a binding and in an operator alike, so `i_isize - k_i64` is an `i64` (review-thread ruling 2026-11-08, B-2026-11-08-48). | NEW isize_usize_widen_as_32_to_64_bits (`i - k` is i64) |
| s5.115 | Mixed-width operands | Both types exist for FFI signatures; Kāra code converts at the boundary and works in `i64`. | rationale or cross-reference, no cell |
| s5.116 | Mixed-width operands | **At a binding or an argument, the whole table applies.** A value widens implicitly wherever its destination type is known, for example a `l | NEW widening_at_bindings_arguments_and_compound_assignment |
| s5.117 | Mixed-width operands | `let b: i64 = a_i32;`, `let f: f64 = a_i32;` and `g(a_i32)` all widen. | NEW widening_at_bindings_arguments_and_compound_assignment |
| s5.118 | Mixed-width operands | An enum constructor's payload whose type the expected type fixes is a destination too: with `w: u64`, `let o: | some_w_with_w_u64_into_option_i64_is_refused, NEW return_some_of_u64_into_option_i64_refused, NEW enum_payload_widens_and_literal_payload_is_range_checked (u32 widens) |
| s5.119 | Mixed-width operands | Option[i64] = Some(w)` and `return Some(w)` from a function returning `Option[i64]` are refused as `let x: i64 = w` is, and need `as`, while | some_w_with_w_u64_into_option_i64_is_refused, NEW return_some_of_u64_into_option_i64_refused, NEW enum_payload_widens_and_literal_payload_is_range_checked (u32 widens) |
| s5.120 | Mixed-width operands | An unsuffixed literal or a constant expression in the payload (`Some(0 - 1)` into `Option[i32]`) takes the payload type and is range-checked | NEW enum_payload_widens_and_literal_payload_is_range_checked, NEW literal_payload_out_of_range_refused |
| s5.121 | Mixed-width operands | **In a binary operator, the narrower operand widens within its kind.** When the two operands have different numeric types of the same kind,  | mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.122 | Mixed-width operands | - integer to integer, by the integer rows of the table; | mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.123 | Mixed-width operands | - float to float, `f32` to `f64`. | mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.124 | Mixed-width operands | The result is always one of the two operand types, never a third: | mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.125 | Mixed-width operands | `i32 + u32` and `u64 + i64` are errors, because neither type converts losslessly to the other. | i32_u32_is_an_error, u64_i64_is_an_error |
| s5.126 | Mixed-width operands | Write `as` to choose. | i32_u32_is_an_error, u64_i64_is_an_error |
| s5.127 | Mixed-width operands | **Integer to float needs `as` in an operator**, even where the table calls it lossless. | integer_float_in_an_operator_is_an_error |
| s5.128 | Mixed-width operands | A mixed-kind operator would hide integer division done earlier in the expression: | integer_float_in_an_operator_is_an_error |
| s5.129 | Mixed-width operands | `a_i32 / b_i32 + c_f64` divides in `i32`, truncating, before anything widens, which is a well-known bug in C and Java. | integer_float_in_an_operator_is_an_error |
| s5.130 | Mixed-width operands | Writing `(a_i32 as f64) / (b_i32 as f64) + c_f64` or `(a_i32 / b_i32) as f64 + c_f64` makes the choice visible. | integer_float_in_an_operator_is_an_error |
| s5.131 | Mixed-width operands | **Which operators.** The rule covers the arithmetic operators `+ - * / %`, the bitwise operators `& / ^` and the comparisons. | NEW bitwise_and_comparison_operands_widen |
| s5.132 | Mixed-width operands | It does not cover the shifts ([Division, remainder and shifts](#division-remainder-and-shifts)). | shifts_arithmetic_on_signed_logical_via_cast_amount_of_any_int_type |
| s5.133 | Mixed-width operands | **How it dispatches.** Widening is a coercion applied to primitive operands before the operator is dispatched. | implementation note on dispatch; generic operator bounds are §9 Operator traits cells (tests/core_operator_traits.rs) |
| s5.134 | Mixed-width operands | The primitives' operator impls all take `Self`-typed operands, and generic code (`T: | implementation note on dispatch; generic operator bounds are §9 Operator traits cells (tests/core_operator_traits.rs) |
| s5.135 | Mixed-width operands | Add`) never widens ([§9 Operator traits](#operator-traits)). | implementation note on dispatch; generic operator bounds are §9 Operator traits cells (tests/core_operator_traits.rs) |
| s5.136 | Mixed-width operands | **Each operation runs in its own operands' type**, so arithmetic in a narrow type traps before anything widens. | each_operation_runs_in_its_operands_type_a_i32_b_i32_c_i64_traps_in_i3 |
| s5.137 | Mixed-width operands | In `a_i32 * b_i32 + c_i64`, the product is computed and checked in `i32`, then widened to `i64` for the addition. | each_operation_runs_in_its_operands_type_a_i32_b_i32_c_i64_traps_in_i3 |
| s5.138 | Mixed-width operands | `let c: i64 = a_i32 * b_i32;` behaves the same way. | each_operation_runs_in_its_operands_type_a_i32_b_i32_c_i64_traps_in_i3 |
| s5.139 | Mixed-width operands | ```kara let a: i32 = 7; let b: u8 = 200; let c: i64 = 1_000; let d: u32 = 5; let r: f64 = 0.5; let h: f32 = 0.25;  let s = a + c;        //  | mixed_width_the_narrower_operand_widens_the_result_is_the_wider_type |
| s5.140 | Mixed-width operands | **Compound assignment.** `p op= e` computes `p op e` by the same rule, and the result must then convert implicitly to the type of `p`. | compound_assignment_n_i32_total_i64_is_an_error, NEW compound_assignment_into_the_wider_side |
| s5.141 | Mixed-width operands | So `total_i64 += n_i32` is legal, and `n_i32 += total_i64` is an error. | compound_assignment_n_i32_total_i64_is_an_error, NEW compound_assignment_into_the_wider_side |
| s5.142 | `as` casts | **Every numeric `as` cast is fully defined.** No numeric cast has undefined or implementation-defined behaviour, and none panics. | integer_as_casts_sign_extend_truncate_low_bits_kept, float_to_int_saturates_nan_is_0_0_0_is_0, f64_as_f32_overflows_to_infinity |
| s5.143 | `as` casts | Each source and target pair has one deterministic meaning: | integer_as_casts_sign_extend_truncate_low_bits_kept, float_to_int_saturates_nan_is_0_0_0_is_0, f64_as_f32_overflows_to_infinity |
| s5.144 | `as` casts | / Source / Target / Meaning of `as` / /---/---/---/ | header |
| s5.145 | `as` casts | / `iN` / `uN` / `iM` / `uM`, lossless / Implicit, so `as` is not needed (see the lint below). / | tests/cast_lints.rs (lossless_cast on an integer widening) |
| s5.146 | `as` casts | / `i8` … `i32`, `u8` … `u32` / `f64` / Implicit at a binding or an argument. | NEW widening_at_bindings_arguments_and_compound_assignment (binding, argument), integer_float_in_an_operator_is_an_error (operator) |
| s5.147 | `as` casts | **`as` required** as an operand of an arithmetic or comparison operator ([Mixed-width operands](#mixed-width-operands)). / | NEW widening_at_bindings_arguments_and_compound_assignment (binding, argument), integer_float_in_an_operator_is_an_error (operator) |
| s5.148 | `as` casts | / `iN` / `uM` / **`as` required.** The source bits are read as two's complement, then sign-extended or truncated to width M. | integer_as_casts_sign_extend_truncate_low_bits_kept |
| s5.149 | `as` casts | `(-1i8) as u8 == 255`, `(-1i32) as u64 == 0xFFFFFFFF_FFFFFFFFu64`. / | integer_as_casts_sign_extend_truncate_low_bits_kept |
| s5.150 | `as` casts | / `iN` / `uN` / narrower `iM` / `uM` / **`as` required.** Keeps the low M bits; the sign of the result follows the target type. | integer_as_casts_sign_extend_truncate_low_bits_kept |
| s5.151 | `as` casts | `0x1FFi32 as u8 == 0xFFu8`, `(-1i32) as u8 == 0xFFu8`, `300i32 as i8 == 44i8`. / | integer_as_casts_sign_extend_truncate_low_bits_kept |
| s5.152 | `as` casts | / `iN` / `uN` / `f32` / `f64`, lossy / **`as` required.** IEEE 754 round-to-nearest-even. | integer_as_casts_sign_extend_truncate_low_bits_kept |
| s5.153 | `as` casts | `i64.MAX as f64` rounds to `9.223372036854776e+18`, just above `i64.MAX`, written as [float printing](library/core-types.md) writes it (revi | integer_as_casts_sign_extend_truncate_low_bits_kept |
| s5.154 | `as` casts | / `f64` / `f32` / **`as` required.** IEEE 754 round-to-nearest-even; overflow gives `±Infinity`. | f64_as_f32_overflows_to_infinity, a_narrower_float_suffix_widens_let_w_f64_0_1f32 |
| s5.155 | `as` casts | (`f32` to `f64` is implicit.) / | f64_as_f32_overflows_to_infinity, a_narrower_float_suffix_widens_let_w_f64_0_1f32 |
| s5.156 | `as` casts | / `f32` / `f64` / `iN` / `uN` / **`as` required. | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.157 | `as` casts | Saturates**; see [Float to integer conversion](#float-to-integer-conversion). / | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.158 | `as` casts | / `bool` / `iN` / `uN` / **`as` required.** `false` is `0`, `true` is `1`. | integer_as_casts_sign_extend_truncate_low_bits_kept (`true as i32`) |
| s5.159 | `as` casts | The reverse (`iN as bool`) is rejected; write `n != 0`. / | i64_as_bool_is_rejected |
| s5.160 | `as` casts | / `char` / `u32` / `i32` and wider (`i64`, `u64`, `i128`, `u128`, `usize`) / **`as` required.** The Unicode scalar value (at most `0x10FFFF` | integer_as_casts_sign_extend_truncate_low_bits_kept (`c as u32`, `c as i64`) |
| s5.161 | `as` casts | / `char` / `u8` / `u16` / `i8` / `i16` / **Rejected**: | char_as_u8_is_rejected_with_e_char_as_narrow_int |
| s5.162 | `as` casts | `error[E_CHAR_AS_NARROW_INT]: 'char as uN/iN' for N < 32 is rejected because it truncates the Unicode scalar value's low bits, which is not  | char_as_u8_is_rejected_with_e_char_as_narrow_int |
| s5.163 | `as` casts | The fix offers both. / | char_as_u8_is_rejected_with_e_char_as_narrow_int |
| s5.164 | `as` casts | / `u8` / `char` / **Allowed**, lossless: every byte value `0..=255` is a Unicode scalar, so `b as char` is the character with that code poin | integer_as_casts_sign_extend_truncate_low_bits_kept (`byte as char`) |
| s5.165 | `as` casts | / other `iN` / `uN` / `char` / **Rejected**: | i32_as_char_is_rejected_with_e_int_as_char |
| s5.166 | `as` casts | `error[E_INT_AS_CHAR]: cannot cast integer to char via 'as': not every integer is a Unicode scalar value (range '0..=0xD7FF \/ 0xE000..=0x10 | i32_as_char_is_rejected_with_e_int_as_char |
| s5.167 | `as` casts | / `f32` / `f64` / `bool` / **Rejected**: meaningless. / | f64_as_bool_is_rejected |
| s5.168 | `as` casts | / `bool` / `bool` / Identity; the `redundant_cast` lint flags it. / | tests/cast_lints.rs (redundant_cast) |
| s5.169 | `as` casts | **The `lossless_cast` lint flags an `as` the program does not need**: one whose conversion would happen implicitly at that position (`let b: | tests/cast_lints.rs, tests/cast_lints_operands.rs (lossless_cast and its fix) |
| s5.170 | `as` casts | Its fix removes the `as`. | tests/cast_lints.rs, tests/cast_lints_operands.rs (lossless_cast and its fix) |
| s5.171 | `as` casts | So an `as` in source marks a conversion that may change the value, or an integer-to-float conversion inside an operator, where the `as` is r | rationale or cross-reference, no cell |
| s5.172 | `as` casts | `as` has no other meanings. | statement of absence; the rejected casts above |
| s5.173 | `as` casts | Pointer casts are covered in [§15](#15-unsafe-ffi-and-layout-control); a [distinct type](#distinct-types) converts with its constructor and  | §15 cells; distinct-type cells below (`.raw()`) |
| s5.174 | `as` casts | There are no trait-object or subtype casts. | statement of absence, no cell |
| s5.175 | Float to integer conversion | **`f as iN` saturates.** For every `f32` or `f64` cast to any signed or unsigned integer type: | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.176 | Float to integer conversion | / Source value / Result / /---/---/ | header |
| s5.177 | Float to integer conversion | / Finite, within `[iN.MIN, iN.MAX]` (or `[0, uN.MAX]`) / Truncated toward zero. | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.178 | Float to integer conversion | `3.7f64 as i32 == 3`, `(-3.7f64) as i32 == -3`. / | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.179 | Float to integer conversion | / Finite, above `iN.MAX` (or `uN.MAX`) / `iN.MAX` (or `uN.MAX`). | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.180 | Float to integer conversion | `1e30f64 as i32 == i32.MAX`, `1e30f64 as u8 == 255`. / | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.181 | Float to integer conversion | / Finite, below `iN.MIN` (or below `0` for unsigned) / `iN.MIN` (or `0`). | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.182 | Float to integer conversion | `(-1e30f64) as i32 == i32.MIN`, `(-1.0f64) as u8 == 0`. / | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.183 | Float to integer conversion | / `+Infinity` / `iN.MAX` (or `uN.MAX`). / | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.184 | Float to integer conversion | / `-Infinity` / `iN.MIN` (or `0` for unsigned). / | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.185 | Float to integer conversion | / `NaN` (any payload) / `0`, for signed and unsigned targets alike. / | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.186 | Float to integer conversion | So: round toward zero; if the result is outside the target's range, saturate to the nearest end; NaN maps to zero. | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.187 | Float to integer conversion | This is bit-identical to LLVM's `fptosi.sat` and `fptoui.sat`, so it compiles to one instruction. | float_to_int_saturates_nan_is_0_0_0_is_0 |
| s5.188 | Float to integer conversion | **The checked conversion lives on the target type.** `iN.checked_from(f) -> Option[iN]` is `None` for NaN or an out-of-range value, and othe | checked_from_none_for_nan_or_out_of_range_else_truncated |
| s5.189 | Float to integer conversion | It exists for every integer target (`i32.checked_from(f)`, `u8.checked_from(f)`). | checked_from_none_for_nan_or_out_of_range_else_truncated |
| s5.190 | Float to integer conversion | These two are the only float-to-integer spellings: | checked_from_none_for_nan_or_out_of_range_else_truncated |
| s5.191 | Float to integer conversion | `as` for the saturating conversion, `checked_from` for the checked one. | checked_from_none_for_nan_or_out_of_range_else_truncated |
| s5.192 | Float to integer conversion | A conversion that panics when out of range is `iN.checked_from(f).unwrap()`. | checked_from_none_for_nan_or_out_of_range_else_truncated |
| s5.193 | Float to integer conversion | **Why `as` saturates instead of trapping.** `checked_from` already gives the checked behaviour, and `.unwrap()` on it the trapping one, so a | rationale or cross-reference, no cell |
| s5.194 | Float to integer conversion | Saturating is the most defensive default that is not a method: it never panics and never wraps silently. | rationale or cross-reference, no cell |
| s5.195 | Float to integer conversion | A trapping rule would also have to panic on NaN, which would put a `panics` effect into every function that does numeric work. | rationale or cross-reference, no cell |
| s5.196 | Float to integer conversion | Saturating with NaN mapped to `0` handles every edge case with one rule. | rationale or cross-reference, no cell |
| s5.197 | Floats | **`f32` and `f64` follow IEEE 754 exactly.** `NaN != NaN`. | floats_print_with_a_or_exponent_nan_nan (`n == n` false) |
| s5.198 | Floats | They implement `PartialEq` and `PartialOrd` but **not** `Eq`, `Ord` or `Hash`: | map_f64_string_is_refused_f64_does_not_implement_hash, sorting_a_vec_f64_is_refused, an_alias_bound_is_checked_at_the_use_site_e0262 |
| s5.199 | Floats | NaN breaks the reflexivity `Eq` requires (`a == a` for every `a`) and the totality `Ord` requires. | map_f64_string_is_refused_f64_does_not_implement_hash, sorting_a_vec_f64_is_refused, an_alias_bound_is_checked_at_the_use_site_e0262 |
| s5.200 | Floats | So `f64` cannot be a `Map` or `Set` key, or appear anywhere `Ord` is required. | map_f64_string_is_refused_f64_does_not_implement_hash, sorting_a_vec_f64_is_refused, an_alias_bound_is_checked_at_the_use_site_e0262 |
| s5.201 | Floats | This keeps FFI simple: | rationale or cross-reference, no cell |
| s5.202 | Floats | `f32` and `f64` cross `extern` boundaries and behave the same on both sides. | NEW f64_unwrapped_to_extern_runs |
| s5.203 | Floats | **Printing.** A float prints as the shortest decimal that reads back as the same value, always with a `.` or an exponent, so `1.0` never pri | floats_print_with_a_or_exponent_nan_nan (expectation set per library/core-types.md §Printing) |
| s5.204 | Floats | `to_string()`, `println` and f-string interpolation all use this rendering; the exponent and special-value rules are in [library/core-types. | floats_print_with_a_or_exponent_nan_nan (expectation set per library/core-types.md §Printing) |
| s5.205 | Floats | **Bit patterns.** `x.to_bits()` is the IEEE 754 bit pattern of `x` as an `f64`, a `u64`; `x.to_bits32()` is the pattern of `x` rounded to `f | to_bits_and_bits_as_f64_round_trip |
| s5.206 | Floats | The inverses are integer methods: | to_bits_and_bits_as_f64_round_trip |
| s5.207 | Floats | `bits.bits_as_f64()` reads a 64-bit integer's bits as an `f64`, and `bits.bits_as_f32()` reads a 32-bit integer's bits as an `f32`. | to_bits_and_bits_as_f64_round_trip |
| s5.208 | Floats | They are how a binary format's floats are encoded and decoded. | to_bits_and_bits_as_f64_round_trip |
| s5.209 | Floats | Rust's spelling, `f64.from_bits(bits)`, is an error whose fix writes `bits.bits_as_f64()`. | f64_from_bits_is_an_error_that_names_bits_as_f64 |
| s5.210 | Floats | **`F32` and `F64`: total-order floats.** The standard library provides `F32` and `F64` for contexts that need `Eq`, `Ord` or `Hash`. | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.211 | Floats | They store the same bits as `f32` and `f64` and order them by the IEEE 754 **totalOrder** predicate, with NaN normalized on the way in: | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.212 | Floats | - **Ordering:** `-Infinity < ... < -0.0 < 0.0 < ... < +Infinity < NaN`. | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.213 | Floats | `-0.0` and `0.0` are **distinct, adjacent** values here: the wrapper compares bit patterns, not IEEE values, so `-0.0 == 0.0` is `false` (on | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.214 | Floats | - **Equality is bit equality.** That is what makes the `Hash` impl sound: equal keys have identical bits, so they hash alike, with no normal | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.215 | Floats | - **NaN is one value.** Every NaN bit pattern is canonicalized to one quiet NaN when a wrapper is constructed (`F64.from(x)` and `F64 { valu | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.216 | Floats | So `NaN == NaN` is `true`, and NaN sorts after every other value whatever sign and payload it arrived with. | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.217 | Floats | - They implement `Eq`, `Ord` and `Hash`, so they work as `Map` and `Set` keys, in `SortedMap`, and wherever `T: | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.218 | Floats | Ord` is required. | f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.219 | Floats | Canonicalizing NaN is not cosmetic. | rationale or cross-reference, no cell |
| s5.220 | Floats | Raw totalOrder distinguishes NaNs by sign, which places `-NaN` before `-Infinity` and `+NaN` after `+Infinity`, and nothing in the source ch | rationale or cross-reference, no cell |
| s5.221 | Floats | Canonicalizing at construction makes the order a property of the program, not of how it was compiled. | rationale or cross-reference, no cell |
| s5.222 | Floats | The naming follows Kāra's convention: lowercase primitives (`f64`) have a minimal trait surface, and PascalCase library types (`F64`) carry  | rationale or cross-reference, no cell |
| s5.223 | Floats | ```kara let by_value: Map[f64, String] = Map.new();   // compile error: f64 does not implement Hash let by_total: Map[F64, String] = Map.new | map_f64_string_is_refused_f64_does_not_implement_hash, f64_total_order_nan_canonical_and_last_0_0_0_0 |
| s5.224 | Floats | **Diagnostic.** When `f64` appears where `Eq`, `Ord` or `Hash` is required, the compiler names the fix: | map_f64_string_is_refused_f64_does_not_implement_hash (the refusal names F64) |
| s5.225 | Floats | ``` error: f64 does not implement Hash f64 follows IEEE 754: NaN != NaN, so NaN keys could never be retrieved. Use F64 instead: it defines a | map_f64_string_is_refused_f64_does_not_implement_hash (the refusal names F64) |
| s5.226 | Floats | **FFI.** `f32` and `f64` cross FFI boundaries directly, IEEE on both sides. | NEW f64_unwrapped_to_extern_runs |
| s5.227 | Floats | `F32` and `F64` must be unwrapped to `f32` and `f64` before they are passed to an `extern` function; the compiler rejects an implicit conver | NEW f64_wrapper_to_extern_refused, control NEW f64_unwrapped_to_extern_runs |
| s5.228 | Floats | **Serialization.** An `f32` or `f64` field in a `#[derive(Serialize, Deserialize)]` type triggers the `float_in_serialized_type` lint: | tests/mir_serde.rs, tests/cli.rs (float_in_serialized_type, warn-only) |
| s5.229 | Floats | JSON has no NaN, and MessagePack and Protobuf carry IEEE bits that consumers may read differently. | tests/mir_serde.rs, tests/cli.rs (float_in_serialized_type, warn-only) |
| s5.230 | Floats | The lint is a warning; `#[allow(float_in_serialized_type)]` silences it per field. | tests/mir_serde.rs, tests/cli.rs (float_in_serialized_type, warn-only) |
| s5.231 | Floats | The reduced-precision floats `f16` and `bf16` belong to the data track ([deferred.md](deferred.md#m4c-data)). | deferred (deferred.md), no cell |
| s5.232 | Tuples | A tuple groups a fixed number of values whose types may differ. | tuples_position_reads_and_destructuring |
| s5.233 | Tuples | The type and the value are written the same way: | tuples_position_reads_and_destructuring |
| s5.234 | Tuples | ```kara let entry: (i64, f64, bool) = (3, 0.5, true); let count = entry.0;              // elements are read by position let (n, ratio, ok)  | tuples_position_reads_and_destructuring |
| s5.235 | Tuples | - `()` is the unit type ([Primitive types](#primitive-types)). | NEW unit_value_and_never_in_expression_positions |
| s5.236 | Tuples | - A tuple is `Copy` when every element is ([core-semantics.md §1.1](core-semantics.md#1-values)). | NEW tuples_copy_order_and_partial_move |
| s5.237 | Tuples | - Elements are evaluated left to right ([core-semantics.md §8](core-semantics.md#8-evaluation-order)) and dropped last to first ([core-seman | NEW tuples_copy_order_and_partial_move |
| s5.238 | Tuples | - An element of a tuple held in an owned local may be moved out on its own (a partial move, [core-semantics.md §3.6](core-semantics.md#3-mov | NEW tuples_copy_order_and_partial_move |
| s5.239 | Tuples | - A tuple of only shared handles and `Copy` values is a handle aggregate: using it counts its handles instead of moving it ([core-semantics. | corpus/core §6.1 handle-aggregate programs |
| s5.240 | Arrays | `Array[T, N]` is a fixed-size array of `N` elements of type `T`, stored inline. | arrays_literal_with_expected_type_v_n_copy_bounds_check |
| s5.241 | Arrays | `N` is a compile-time constant (const generics: [§8](#8-type-inference-and-generics)). | §8 const-generics cells (conformance/design/generics) |
| s5.242 | Arrays | An array is written as a list literal with an `Array` expected type, or with the repeat form `[v; n]` ([Collection literals](#collection-lit | arrays_literal_with_expected_type_v_n_copy_bounds_check, list_literals_follow_the_expected_type_set_sortedset_builds_the_other |
| s5.243 | Arrays | - Indexing is bounds-checked and uses `i64` ([Indexing](#indexing)). | arrays_literal_with_expected_type_v_n_copy_bounds_check (index 3 panics) |
| s5.244 | Arrays | - An array is `Copy` when `T` is ([core-semantics.md §1.1](core-semantics.md#1-values)). | arrays_literal_with_expected_type_v_n_copy_bounds_check (`let b = a;` then `a[0]`) |
| s5.245 | Arrays | Its elements drop first to last ([core-semantics.md §7.8](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0)). | NEW arrays_drop_first_to_last |
| s5.246 | Arrays | - Writing every slot of an uninitialized array one by one does not initialize it for [definite assignment](#variable-binding-rules); use a l | NEW array_slot_by_slot_initialization_refused |
| s5.247 | Arrays | - An `Array[T, N]` argument passes as a `Slice[T]` ([Slices](#slices)). | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.248 | Collection literals | **List literals.** `[a, b, c]` is a `Vec` by default and follows the expected type: with an `Array[T, N]`, `Set[T]` or `SortedSet[T]` annota | list_literals_follow_the_expected_type_set_sortedset_builds_the_other |
| s5.249 | Collection literals | The repeat form `[v; n]` always builds an `Array`, and `n` must be a constant. | arrays_literal_with_expected_type_v_n_copy_bounds_check, NEW repeat_form_count_must_be_constant_refused |
| s5.250 | Collection literals | A `Copy` value is copied into each slot; any other value must be `Clone`, and the slots after the first hold clones of `v`. | NEW repeat_form_clones_a_clone_value |
| s5.251 | Collection literals | A `Vec` of `n` copies is `Vec.filled(n, v)`. | NEW suffix_types_an_inline_argument (`Vec.filled`) |
| s5.252 | Collection literals | ```kara,uses=d-n let v = [1, 2, 3];                    // Vec[i64] let a: Array[i64, 3] = [1, 2, 3];     // Array[i64, 3]: the expected type | list_literals_follow_the_expected_type_set_sortedset_builds_the_other |
| s5.253 | Collection literals | - The other operand of `==` or `!=` is an expected type, as it is for a number literal (`x == 1` with `x: u8`): with `m: | list_literals_follow_the_expected_type_set_sortedset_builds_the_other (`m != [..]`, `s == [1, 3]`) |
| s5.254 | Collection literals | Array[u8, 4]`, `m != [75, 82, 69, 67]` builds an `Array[u8, 4]`, and with `s: | list_literals_follow_the_expected_type_set_sortedset_builds_the_other (`m != [..]`, `s == [1, 3]`) |
| s5.255 | Collection literals | Set[i64]`, `s == [1, 2]` builds a `Set`. | list_literals_follow_the_expected_type_set_sortedset_builds_the_other (`m != [..]`, `s == [1, 3]`) |
| s5.256 | Collection literals | - A length mismatch between a literal and its `Array[T, N]` annotation is a compile error. | an_array_literal_of_the_wrong_length_is_a_compile_error |
| s5.257 | Collection literals | - `let v: | let_v_vec_i64_0_8_is_an_error_with_the_vec_filled_fix |
| s5.258 | Collection literals | Vec[i64] = [0; 8];` is an error, because `[v; n]` is an `Array`. | let_v_vec_i64_0_8_is_an_error_with_the_vec_filled_fix |
| s5.259 | Collection literals | Its fix is `Vec.filled(8, 0)`. | let_v_vec_i64_0_8_is_an_error_with_the_vec_filled_fix |
| s5.260 | Collection literals | **Map literals.** `[k1: v1, k2: v2]` is a `Map` by default, and a `SortedMap` where one is expected. | map_literals_map_default_sortedmap_expected_empty |
| s5.261 | Collection literals | `[:]` is the empty map. | map_literals_map_default_sortedmap_expected_empty |
| s5.262 | Collection literals | ```kara let scores = ["alice": 10, "bob": 7];                  // Map[String, i64] let ranked: SortedMap[String, i64] = ["bob": 7];       // | map_literals_map_default_sortedmap_expected_empty |
| s5.263 | Collection literals | An empty literal, `[]` or `[:]`, takes its element types from the expected type. | NEW empty_literals_take_the_expected_types_and_from_constructors |
| s5.264 | Collection literals | There is one way to write each collection: | NEW empty_literals_take_the_expected_types_and_from_constructors |
| s5.265 | Collection literals | `[a, b]`, `[v; n]`, `[k: v]`, `[:]`, `Vec.new()` and `Vec.filled(n, v)`, plus the `from` constructors (`Set.from([1, 2, 3])`, `Map.from([(k, | NEW empty_literals_take_the_expected_types_and_from_constructors |
| s5.266 | Collection literals | There is no `Vec[...]` or `Set[...]` prefix literal. | statement of absence (deprecated forms below) |
| s5.267 | Collection literals | `vec![a, b]` is accepted as a deprecated spelling of the list literal: | tests/fmt_vec_spelling.rs, tests/fix_check_agree.rs (vec! and prefix-literal deprecation and fix; warn-only) |
| s5.268 | Collection literals | `karac check` warns on it and `karac fix` rewrites it to `[a, b]` (Gowtham, 2026-11-04). | tests/fmt_vec_spelling.rs, tests/fix_check_agree.rs (vec! and prefix-literal deprecation and fix; warn-only) |
| s5.269 | Collection literals | The prefix forms (`Vec[a, b]`, `Set[a, b]`, `Map[k: v]`, `Array[v; n]`, `VecDeque[a]`, `SortedMap[k: v]`) are deprecated the same way in 0.8 | tests/fmt_vec_spelling.rs, tests/fix_check_agree.rs (vec! and prefix-literal deprecation and fix; warn-only) |
| s5.270 | Collection literals | `karac fix` rewrites each to the literal, to `Vec.filled(n, v)`, to `Set.from([..])` or `SortedMap.from([..])` where no expected type picks  | tests/fmt_vec_spelling.rs, tests/fix_check_agree.rs (vec! and prefix-literal deprecation and fix; warn-only) |
| s5.271 | Collection literals | `Vec`, `Map`, `Set` and the other collections are library types ([library/collections.md](library/collections.md)). | rationale or cross-reference, no cell |
| s5.272 | Structs | A struct is a named record of fields: | struct_literal_shorthand_and_base_update_moves_the_rest |
| s5.273 | Structs | ```kara,uses=d-hash,name=d-user struct User { pub name: String,        // visible to users of the package pub email: String, password_hash:  | struct_literal_shorthand_and_base_update_moves_the_rest |
| s5.274 | Structs | - **Field visibility** takes the three levels of [§4 Visibility](#visibility): a field is visible in its package by default, to users of the | conformance/design/modules visibility cells (private_same_directory_default_package, private_from_other_directory_refused) |
| s5.275 | Structs | Code that cannot see a field cannot name it, in a field access or a struct literal; the diagnostic suggests a constructor or an accessor. | tests/cross_package_struct_literal.rs |
| s5.276 | Structs | - **Derived impls** (`#[derive(Eq)]`) see every field regardless of visibility, because the generated code lives in the struct's module ([§9 | §9 derive cells (tests/core_derive_lists.rs) |
| s5.277 | Structs | - **Fields are mutable only through the binding.** A field of a plain struct can be assigned when the value is held in a `let mut` binding o | a_field_of_a_non_mut_binding_cannot_be_assigned, NEW struct_fields_written_through_let_mut_and_mut_ref |
| s5.278 | Structs | `mut port: i64` on a plain struct field is a compile error. | tests/mut_field_of_plain_type.rs (deprecation warning and fix; warn-only) |
| s5.279 | Structs | (In 0.8.x it is a deprecation warning, and `karac fix` deletes the `mut`; it becomes the error at the next minor release, B-2026-11-04-79.)  | tests/mut_field_of_plain_type.rs (deprecation warning and fix; warn-only) |
| s5.280 | Structs | The fields of a `sync` type are never `mut`; they change through `Atomic` and `Mutex` methods ([§11](#sync-types)). | §11 sync-type cells |
| s5.281 | Structs | - **`shared struct` fields** combine visibility and `mut`: | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.282 | Structs | - `pub mut field: | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.283 | Structs | T`: visible to users. | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.284 | Structs | Inside the package it may be assigned directly. | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.285 | Structs | Users of the package change it only through `pub fn` methods. | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.286 | Structs | - `pub field: | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.287 | Structs | T`: visible to users, never assigned after construction. | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.288 | Structs | - `mut field: | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.289 | Structs | T`, `field: | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.290 | Structs | T`: package-internal, mutable or not. | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.291 | Structs | - `private mut field: | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.292 | Structs | T`, `private field: | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.293 | Structs | T`: directory-only, mutable or not. | §11 shared-type cells (conformance/design and tests/design_11_conformance_2026_11_22.rs) |
| s5.294 | Structs | - **Unit struct.** A struct with no fields is declared and built with empty braces: | a_unit_struct_is_built_with_empty_braces |
| s5.295 | Structs | `struct Marker {}`, `struct PhantomData[T] {}` and `let m = Marker {};`. | a_unit_struct_is_built_with_empty_braces |
| s5.296 | Structs | It has no runtime data, and suits type tags and stateless trait implementors. | a_unit_struct_is_built_with_empty_braces |
| s5.297 | Structs | The form `struct Marker;` is not valid. | struct_marker_is_not_valid |
| s5.298 | Structs | - **A struct with a `ref`, `Str` or `Slice` field is a view** and borrows like a reference ([core-semantics.md §5.2](core-semantics.md#5-ref | corpus/core §5.2 view-struct programs |
| s5.299 | Structs | - Fields drop in reverse declaration order, after the type's own `Drop` body ([core-semantics.md §7.8](core-semantics.md#7-destruction-c6-su | fields_drop_in_reverse_declaration_order_after_the_type_s_own_drop |
| s5.300 | Structs | - `shared struct` and `sync struct` have reference semantics, and `weak` fields break cycles between them ([§11](#11-ownership-and-sharing)) | §11 cells |
| s5.301 | Structs | **Struct literals.** Every field is given a value, in any order. | NEW struct_literal_missing_field_refused |
| s5.302 | Structs | The values are evaluated in written order ([core-semantics.md §8.1](core-semantics.md#8-evaluation-order)) and move into the struct. | NEW struct_literal_evaluates_in_written_order |
| s5.303 | Structs | ```kara,uses=d-pointf/d-user/d-user-loaders/d-pw let x = 1.0; let y = 2.0; let p = Point { x: x, y: y }; let q = Point { x, y };             | struct_literal_shorthand_and_base_update_moves_the_rest |
| s5.304 | Structs | The update form `..base` must come last. | NEW struct_update_base_not_last_refused |
| s5.305 | Structs | Every field not listed is taken from `base`, which must have the same struct type. | NEW struct_update_base_of_another_type_refused |
| s5.306 | Structs | Fields taken from `base` move out of it, or are copied when they are `Copy`, by the partial-move rules of [core-semantics.md §3.6](core-sema | struct_literal_shorthand_and_base_update_moves_the_rest, after_base_a_moved_field_of_base_is_unusable |
| s5.307 | Structs | Above, `alice.name` is still usable after the update. | struct_literal_shorthand_and_base_update_moves_the_rest (`alice.name`) |
| s5.308 | Structs | Derived behaviour (`#[derive(...)]`) is covered in [§9](#9-traits). | rationale or cross-reference, no cell |
| s5.309 | Enums | An enum is a closed set of variants. | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant |
| s5.310 | Enums | A variant has no payload, a tuple payload, or named fields: | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant |
| s5.311 | Enums | ```kara,uses=d-describe enum Token { Eof,                                    // unit variant Integer(i64),                           // tupl | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant |
| s5.312 | Enums | A variant is built with its qualified name. | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant |
| s5.313 | Enums | In a pattern, the enum name may be left out when the scrutinee's type is known: | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant (`Wait(ms)`, `Done`) |
| s5.314 | Enums | `Circle { radius }` and `Shape.Circle { radius }` match the same values. | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant (`Wait(ms)`, `Done`) |
| s5.315 | Enums | The prelude variants `Some`, `None`, `Ok` and `Err` are canonically written bare; the qualified form (`Option.Some(x)`) is accepted, and the | NEW qualified_prelude_variant_is_accepted; the lint and fix: tests/qualified_prelude_variant.rs |
| s5.316 | Enums | **A payload may be any type, another enum included.** | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant |
| s5.317 | Enums | ```kara,uses=d-commands enum Command { Start, Stop, Restart { delay_ms: i64 } } enum Step { Do(Command), Wait(i64), Done }  fn run(step: Ste | enums_unit_tuple_and_struct_variants_nested_payload_match_bare_variant |
| s5.318 | Enums | **An enum is `Copy` only with `#[derive(Copy)]`**, even when no variant has a payload ([core-semantics.md §1.1](core-semantics.md#1-values)) | a_payload_free_enum_is_not_copy_without_derive |
| s5.319 | Enums | So adding a payload to a variant later does not silently change how callers' values move. | a_payload_free_enum_is_not_copy_without_derive |
| s5.320 | Enums | **Recursive enums require `shared enum`.** A plain enum whose payload contains the same enum directly would have infinite size, and is a com | a_recursive_plain_enum_is_refused_and_suggests_shared_enum |
| s5.321 | Enums | ``` error: enum `Expr` is recursive without indirection --> src/query.kara:3:5 / 3  /     And(Expr, Expr), /         ^^^^ contains `Expr`, w | a_recursive_plain_enum_is_refused_and_suggests_shared_enum |
| s5.322 | Enums | The idiomatic fix for tree-shaped data (query trees, expression trees, JSON values) is `shared enum`: | a_recursive_shared_enum_works |
| s5.323 | Enums | ```kara,uses=d-value shared enum Expr { And(Expr, Expr),    // OK: each payload holds a counted handle to a child Or(Expr, Expr), Eq { colum | a_recursive_shared_enum_works |
| s5.324 | Enums | A `shared enum` gives each node reference semantics, so every payload is a fixed-size handle to its children ([§11](#11-ownership-and-sharin | a_recursive_shared_enum_works |
| s5.325 | Enums | A cycle of strong handles is never freed ([core-semantics.md §6.5](core-semantics.md#6-sharing-c7)). | corpus/core §6.5 cycle programs |
| s5.326 | Enums | - The active variant's payload drops last to first ([core-semantics.md §7.8](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment | NEW enum_payload_drops_last_to_first |
| s5.327 | Enums | - Explicit discriminants and `#[repr(intN)]` are covered in [§15](#15-unsafe-ffi-and-layout-control); `#[non_exhaustive]` in [§11](#11-owner | c_like_enum_discriminant_and_tryfrom_on_a_repr_enum; §11 and §15 cells |
| s5.328 | `Option` and `Result` | Absence and failure are ordinary enums from the prelude: | option_t_is_copy_when_t_is, NEW qualified_prelude_variant_is_accepted |
| s5.329 | `Option` and `Result` | ```kara enum Option[T] { Some(T), None } enum Result[T, E] { Ok(T), Err(E) } | option_t_is_copy_when_t_is, NEW qualified_prelude_variant_is_accepted |
| s5.330 | `Option` and `Result` | - Their variants are canonically written bare: | option_t_is_copy_when_t_is, NEW qualified_prelude_variant_is_accepted |
| s5.331 | `Option` and `Result` | `Some(x)`, `None`, `Ok(v)`, `Err(e)`. | option_t_is_copy_when_t_is, NEW qualified_prelude_variant_is_accepted |
| s5.332 | `Option` and `Result` | The qualified form (`Option.Some(x)`, `Result.Err(e)`) is accepted; `karac fix` rewrites it to the bare one. | NEW qualified_prelude_variant_is_accepted; fix under `-W qualified_prelude_variant`: tests/qualified_prelude_variant.rs (DOC FIX: the sentence now says the lint must be on, as §5 Enums already did) |
| s5.333 | `Option` and `Result` | - `Option[T]` is `Copy` when `T` is ([core-semantics.md §1.1](core-semantics.md#1-values)). | option_t_is_copy_when_t_is, option_string_is_not_copy |
| s5.334 | `Option` and `Result` | `Result[T, E]` is `Copy` when `T` and `E` both are, and a `Result` of shared handles and `Copy` values is a handle aggregate. | NEW result_is_copy_when_both_sides_are, control NEW result_of_string_is_not_copy_refused |
| s5.335 | `Option` and `Result` | An `Option` of shared handles is a handle aggregate ([core-semantics.md §6.1](core-semantics.md#6-sharing-c7)), and `Option[ref T]` is a vie | corpus/core §6.1 and §5.2 programs |
| s5.336 | `Option` and `Result` | - `?` propagates `None` and `Err` ([§10](#results-and-propagation)), and `??` supplies a default in their place ([`?` and `??`](#-and-)). | §10 cells; option_t_is_copy_when_t_is (`??`) |
| s5.337 | `Option` and `Result` | Error handling, the `Error` trait and `AnyError` are in [§10](#10-errors-and-panics). | rationale or cross-reference, no cell |
| s5.338 | `Option` and `Result` | The methods are in [library/core-types.md](library/core-types.md). | rationale or cross-reference, no cell |
| s5.339 | Strings | Two types cover text: | header/table; string cells below |
| s5.340 | Strings | / Type / What it is / When to use / /---/---/---/ | header/table; string cells below |
| s5.341 | Strings | / `String` / Owned UTF-8 text on the heap / The default: owning and building text / | header/table; string cells below |
| s5.342 | Strings | / `Str` / A borrowed view of UTF-8 text: a pointer and a length / Parsing, splitting, and passing text without copying / | header/table; string cells below |
| s5.343 | Strings | A string literal is a `String` by default and a `Str` where a `Str` is expected; a literal typed as `Str` views static data and borrows noth | NEW string_literal_is_string_or_str_by_expected_type |
| s5.344 | Strings | **`String`, the default.** Owned, growable, UTF-8 encoded. | NEW string_literal_is_string_or_str_by_expected_type |
| s5.345 | Strings | ```kara fn greeting(name: String) -> String { "Hello, " + name } | NEW string_literal_is_string_or_str_by_expected_type |
| s5.346 | Strings | **`Str`, a zero-copy view.** A `Str` views UTF-8 bytes that something else owns: a pointer and a length, with no allocation. | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view |
| s5.347 | Strings | It is `Copy`, and it borrows exactly like a `ref` ([core-semantics.md §5](core-semantics.md#5-references-and-views-c5)), so it cannot outliv | NEW string_literal_is_string_or_str_by_expected_type (`let t = s;` keeps `s`), NEW returning_a_view_of_an_owned_string_refused |
| s5.348 | Strings | A function returns it by value; there is no `ref Str`. | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view (returns `Str`) |
| s5.349 | Strings | **A `String` passes as `Str`.** A parameter of type `Str` accepts a `String` place, borrowed, the way a `Slice[T]` parameter accepts a `Vec[ | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view |
| s5.350 | Strings | The caller writes the string and the compiler builds the view. | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view |
| s5.351 | Strings | As with slices, this happens at call boundaries, a method receiver included: a `String` receiver also finds `Str`'s methods ([§9 Method reso | NEW string_receiver_finds_str_methods_and_str_agrees |
| s5.352 | Strings | Elsewhere write `s.as_str()`. | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view (`as_str`) |
| s5.353 | Strings | ```kara fn first_word(s: Str) -> Str { let end = s.find(" ").unwrap_or(s.len()); s[0..end]                         // no allocation: a view  | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view |
| s5.354 | Strings | `first_word` has one view parameter, so its result borrows from it ([core-semantics.md §5.4](core-semantics.md#5-references-and-views-c5)). | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view |
| s5.355 | Strings | Returning a view of a `String` the function owns is an error, because that `String` is dropped when the function returns. | NEW returning_a_view_of_an_owned_string_refused |
| s5.356 | Strings | To keep text beyond the borrow, call `.to_string()`. | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view (`to_string`) |
| s5.357 | Strings | Splitting methods (`split`, `lines` and the like) return lazy iterators of `Str` views into the original text ([library/strings.md](library/ | NEW chars_collected_for_indexed_access_and_split_views |
| s5.358 | Strings | There is no copy-on-write and no hidden retention: the programmer always knows whether they hold a view or a copy. | rationale or cross-reference, no cell |
| s5.359 | Strings | **A `Str` agrees with its `String`.** For any `String` `s`, `s.as_str()` and `s` give the same `hash`, the same `==` and the same `cmp` agai | NEW string_receiver_finds_str_methods_and_str_agrees |
| s5.360 | Strings | That is what lets a `String`-keyed `Map` or `Set` be looked up by a `Str` without allocating ([library/collections.md § Lookup by view](libr | NEW string_receiver_finds_str_methods_and_str_agrees |
| s5.361 | Strings | **`Str` is not `Slice[u8]`.** The UTF-8 guarantee is carried by the type, not by the element type, and a byte slice would lose it. | NEW str_is_not_a_byte_slice_refused |
| s5.362 | Strings | `Slice[T]` covers every other sequence; `Str` covers text. | NEW str_is_not_a_byte_slice_refused |
| s5.363 | Strings | **Characters.** `char` is one Unicode scalar value. | char_at_returns_option_bytes_i_is_a_u8_for_c_in_s_decodes |
| s5.364 | Strings | `for c in s` iterates over characters, decoding UTF-8. | char_at_returns_option_bytes_i_is_a_u8_for_c_in_s_decodes |
| s5.365 | Strings | Byte access is explicit: | char_at_returns_option_bytes_i_is_a_u8_for_c_in_s_decodes |
| s5.366 | Strings | `s.bytes()` gives a `Slice[u8]`. | char_at_returns_option_bytes_i_is_a_u8_for_c_in_s_decodes |
| s5.367 | Strings | **Indexing text.** | header |
| s5.368 | Strings | - **`s[a..b]` yields a `Str`** over the **byte** range `[a, b)`: the same offsets that `len()`, `find` and `bytes()` use, not character indi | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view, s_a_b_cutting_inside_a_multi_byte_char_panics |
| s5.369 | Strings | It panics if a bound is out of range, or if a cut lands **inside** a multi-byte character. | s_a_b_out_of_range_panics, s_a_b_cutting_inside_a_multi_byte_char_panics |
| s5.370 | Strings | The alternatives would be to hand back raw bytes, which would let text hold invalid UTF-8 and break the guarantee this section rests on, or  | rationale or cross-reference, no cell |
| s5.371 | Strings | Find a valid cut with `find`, or work in characters with `chars()`. | NEW chars_collected_for_indexed_access_and_split_views |
| s5.372 | Strings | - **`s[i]` is a compile error.** The syntax looks O(1), but finding the i-th character of UTF-8 text is O(n). | s_i_is_a_compile_error_naming_char_at_and_bytes |
| s5.373 | Strings | The diagnostic names both alternatives: | s_i_is_a_compile_error_naming_char_at_and_bytes |
| s5.374 | Strings | ``` error: String does not support indexing with [] s[i] would hide an O(n) scan: Strings are UTF-8 encoded and characters are variable-widt | s_i_is_a_compile_error_naming_char_at_and_bytes |
| s5.375 | Strings | - `s.char_at(i)` returns `Option[char]`: | char_at_returns_option_bytes_i_is_a_u8_for_c_in_s_decodes |
| s5.376 | Strings | `None` when out of range, never a panic. | char_at_returns_option_bytes_i_is_a_u8_for_c_in_s_decodes |
| s5.377 | Strings | For iteration, prefer `for c in s` (O(n) in total) over repeated `s.char_at(i)` (O(n²)). | rationale or cross-reference, no cell |
| s5.378 | Strings | When repeated indexed access is really needed, convert once: | NEW chars_collected_for_indexed_access_and_split_views |
| s5.379 | Strings | `let chars: | NEW chars_collected_for_indexed_access_and_split_views |
| s5.380 | Strings | Vec[char] = s.chars().collect();`, after which `chars[i]` is honestly O(1). | NEW chars_collected_for_indexed_access_and_split_views |
| s5.381 | Strings | - `s.bytes()[i]` returns a `u8` and panics when out of range, like any slice. | char_at_returns_option_bytes_i_is_a_u8_for_c_in_s_decodes |
| s5.382 | Strings | Use it for protocols and binary formats, not text. | rationale or cross-reference, no cell |
| s5.383 | Strings | **Equality is byte equality.** `==` on text compares raw UTF-8 bytes, and `Hash` hashes the same bytes, so two strings that look identical i | equality_is_byte_equality_nfc_and_nfd_forms_differ |
| s5.384 | Strings | Normalize both sides first when that matters ([library/strings.md](library/strings.md)). | library/strings.md cells |
| s5.385 | Slices | `Slice[T]` is a borrowed view of contiguous elements: a pointer and a length (`{ ptr: | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.386 | Slices | *const T, len: i64 }`). | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.387 | Slices | It is the general counterpart of `Str`, and it lets one function work over a `Vec[T]`, an `Array[T, N]`, or part of either. | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.388 | Slices | ```kara fn sum(xs: Slice[i64]) -> i64 { let mut acc = 0; for x in xs { acc += x; }     // `x` is a `ref i64`, read as an `i64` acc }  let v: | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.389 | Slices | **Coercion at call boundaries.** A `Slice[T]` parameter accepts a `Vec[T]` or `Array[T, N]` place, borrowed, or a `Slice[T]`. | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.390 | Slices | The caller writes the collection and the compiler builds the slice header. | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.391 | Slices | This keeps signatures to one type instead of three, without a dereference chain users cannot name. | slice_t_accepts_a_vec_an_array_and_a_sub_range |
| s5.392 | Slices | The conversion happens only at call boundaries, a method receiver included: a `Vec[T]` or `Array[T, N]` receiver also finds `Slice[T]`'s met | NEW slice_is_copy_and_receiver_finds_slice_methods |
| s5.393 | Slices | `let` and assignment do not convert. | NEW let_does_not_convert_a_vec_to_a_slice_refused |
| s5.394 | Slices | Where a slice must be named directly (inside a closure, in a `match` arm), call `.as_slice()`: | slice_t_accepts_a_vec_an_array_and_a_sub_range (`as_slice`) |
| s5.395 | Slices | ```kara,uses=d-v let window = v.as_slice();    // Slice[i64] as a first-class value | slice_t_accepts_a_vec_an_array_and_a_sub_range (`as_slice`) |
| s5.396 | Slices | **Comparison.** `==` and `!=` between a `Slice[T]`, a `Vec[T]` and an `Array[T, N]` compare elements, in either order, as Rust's do: | slice_comparisons_array_vs_slice_of_other_length_is_false_either_order |
| s5.397 | Slices | `a == s` and `s == a` are both accepted and give the same answer, and an `Array` compared with a `Slice` of another length is `false`, not a | slice_comparisons_array_vs_slice_of_other_length_is_false_either_order |
| s5.398 | Slices | No `.as_slice()` is needed on either side (review ruling 2026-11-13, B-2026-11-13-17). | slice_comparisons_array_vs_slice_of_other_length_is_false_either_order |
| s5.399 | Slices | **Mutable slices.** An in-place view is written with the `mut` modifier, as with `ref T` and `mut ref T`: | mut_slice_in_place_sort_through_a_mut_marker_a_sub_range_view |
| s5.400 | Slices | ```kara fn sort_in_place[T: Ord](xs: mut Slice[T]) { // … }  let mut v = [3, 1, 4, 1, 5]; sort_in_place(mut v);            // a `mut` Vec ar | mut_slice_in_place_sort_through_a_mut_marker_a_sub_range_view |
| s5.401 | Slices | A `mut ref Vec[T]` or `mut ref Array[T, N]` passes as `mut Slice[T]` at call boundaries, by the same rule as the read-only case. | mut_slice_in_place_sort_through_a_mut_marker_a_sub_range_view |
| s5.402 | Slices | Mutability is a modifier on one type, not a second type. | mut_slice_in_place_sort_through_a_mut_marker_a_sub_range_view |
| s5.403 | Slices | **Borrowing.** `Slice[T]` borrows like `ref T`: it cannot outlive the collection it views, and many read-only views or one mutable view may  | NEW slice_cannot_outlive_its_collection_refused, NEW collection_used_while_a_split_half_is_live_refused |
| s5.404 | Slices | `Slice[T]` is `Copy`, so passing it does not invalidate the caller's binding. | NEW slice_is_copy_and_receiver_finds_slice_methods |
| s5.405 | Slices | `mut Slice[T]` is not `Copy`: a mutable view is unique, like `mut ref T`. | NEW mut_slice_is_not_copy_refused |
| s5.406 | Slices | **`split_at_mut` splits one mutable borrow into two disjoint ones.** | split_at_mut_gives_two_disjoint_halves_mid_len_panics |
| s5.407 | Slices | ```kara,fragment fn split_at_mut(mut ref self, mid: i64) -> (mut Slice[T], mut Slice[T]) | split_at_mut_gives_two_disjoint_halves_mid_len_panics |
| s5.408 | Slices | The first half covers indices `[0, mid)` and the second `[mid, len)`. | split_at_mut_gives_two_disjoint_halves_mid_len_panics |
| s5.409 | Slices | It panics if `mid > self.len()`. | split_at_mut_gives_two_disjoint_halves_mid_len_panics |
| s5.410 | Slices | The halves cannot overlap, so both may be live at once without breaking the one-mutable-view rule. | split_at_mut_gives_two_disjoint_halves_mid_len_panics |
| s5.411 | Slices | The collection itself may not be used while either half is live. | NEW collection_used_while_a_split_half_is_live_refused |
| s5.412 | Slices | This is the basic tool for parallel writes into one buffer: give each task one half, and their write regions are disjoint by construction. | rationale or cross-reference, no cell |
| s5.413 | Indexing | **Indices are `i64`.** Sizes, lengths and indices all use `i64`, so no cast is needed between an index and other integers. | a_computed_negative_index_panics (i64 index), usize_to_i64_at_a_binding_needs_as |
| s5.414 | Indexing | `usize` exists only for FFI. | a_computed_negative_index_panics (i64 index), usize_to_i64_at_a_binding_needs_as |
| s5.415 | Indexing | - **`v[i]` is bounds-checked.** An index outside `0..v.len()` panics ([core-semantics.md §10.1](core-semantics.md#10-panics-and-errors-c8)). | arrays_literal_with_expected_type_v_n_copy_bounds_check |
| s5.416 | Indexing | A negative index is out of bounds like any other: there is no Python-style wrap-around, because `-1` as an index is always a bug. | a_computed_negative_index_panics |
| s5.417 | Indexing | A constant negative index (`v[-1]`) is therefore refused at compile time, with `v[v.len() - 1]` as the fix; a computed one panics when it ru | a_constant_negative_index_is_refused_with_the_len_1_fix, a_computed_negative_index_panics |
| s5.418 | Indexing | - **Accessors return `Option` instead of panicking.** `v.get(i)`, `v.first()` and `v.last()` answer `None` where `v[i]` would panic ([librar | get_first_last_answer_none_instead_of_panicking |
| s5.419 | Indexing | - **Indexing does not move.** `v[i]` is a place. | binding_a_non_copy_element_by_value_is_an_error |
| s5.420 | Indexing | Binding a non-`Copy` element by value is an error; borrow it or `.clone()` it ([core-semantics.md §3.7](core-semantics.md#3-moves), and the  | binding_a_non_copy_element_by_value_is_an_error |
| s5.421 | Indexing | - **Range indexing yields a view.** `v[a..b]` is a `Slice[T]`, or a `mut Slice[T]` where a mutable view is needed, and the half-open forms w | slice_t_accepts_a_vec_an_array_and_a_sub_range, mut_slice_in_place_sort_through_a_mut_marker_a_sub_range_view |
| s5.422 | Indexing | On text, `s[a..b]` is a `Str` and `s[i]` is an error ([Strings](#strings)). | string_str_a_string_passes_as_str_s_a_b_is_a_byte_range_view, s_i_is_a_compile_error_naming_char_at_and_bytes |
| s5.423 | Indexing | - What `[]` accepts on a type is set by its `Index` and `IndexMut` implementations ([§9](#9-traits)). | tests/mir_patterns.rs user_index_impls_are_called; §9 cells |
| s5.424 | Indexing | The multi-index form `e[i, j]` is described under [Operators and precedence](#operators-and-precedence). | §6 cells |
| s5.425 | Type aliases | A type alias names an existing type, and may add generic parameters with bounds. | type_aliases_are_transparent, a_type_alias_may_name_dyn_trait_in_its_arguments, an_alias_bound_is_checked_at_the_use_site_e0262 |
| s5.426 | Type aliases | An alias is transparent: every use behaves exactly as if the aliased type were written there, except that **bounds on the alias's generic pa | type_aliases_are_transparent, a_type_alias_may_name_dyn_trait_in_its_arguments, an_alias_bound_is_checked_at_the_use_site_e0262 |
| s5.427 | Type aliases | ```kara type Counts[T: Eq + Hash] = Map[T, i64]; type SortedVec[T: Ord] = Vec[T]; type Cache[K: Eq + Hash, V: Clone] = Map[K, V];  let by_na | type_aliases_are_transparent, a_type_alias_may_name_dyn_trait_in_its_arguments, an_alias_bound_is_checked_at_the_use_site_e0262 |
| s5.428 | Type aliases | **Bounds are enforced, not ignored.** Bounds on an alias's generic parameters are part of its interface. | an_alias_bound_is_checked_at_the_use_site_e0262, NEW alias_bound_adds_to_the_aliased_type_refused |
| s5.429 | Type aliases | A program that writes `type X[T: | an_alias_bound_is_checked_at_the_use_site_e0262, NEW alias_bound_adds_to_the_aliased_type_refused |
| s5.430 | Type aliases | Ord] = Vec[T]` gets the bound it wrote: every use of `X[U]` checks `U: | an_alias_bound_is_checked_at_the_use_site_e0262, NEW alias_bound_adds_to_the_aliased_type_refused |
| s5.431 | Type aliases | Ord`. | an_alias_bound_is_checked_at_the_use_site_e0262, NEW alias_bound_adds_to_the_aliased_type_refused |
| s5.432 | Type aliases | **Bounds add to the aliased type's own bounds.** `type SortedVec[T: | NEW alias_bound_adds_to_the_aliased_type_refused, type_aliases_are_transparent (SortedVec is Vec) |
| s5.433 | Type aliases | Ord] = Vec[T]` adds a requirement that `Vec` does not have. | NEW alias_bound_adds_to_the_aliased_type_refused, type_aliases_are_transparent (SortedVec is Vec) |
| s5.434 | Type aliases | `SortedVec[String]` checks `String: | NEW alias_bound_adds_to_the_aliased_type_refused, type_aliases_are_transparent (SortedVec is Vec) |
| s5.435 | Type aliases | Ord`, and for every other purpose the alias is `Vec[String]`. | NEW alias_bound_adds_to_the_aliased_type_refused, type_aliases_are_transparent (SortedVec is Vec) |
| s5.436 | Type aliases | When the aliased type already requires the bound (`type SortedKeyMap[T: | tests/spec_section5_checks.rs a_bound_the_aliased_type_already_requires_draws_redundant_alias_bound (warn-only) |
| s5.437 | Type aliases | Ord] = SortedMap[T, i32]`, where `SortedMap` already needs `K: | tests/spec_section5_checks.rs a_bound_the_aliased_type_already_requires_draws_redundant_alias_bound (warn-only) |
| s5.438 | Type aliases | Ord`), the bound is redundant but accepted, with `warning[redundant_alias_bound]: bound 'T: | tests/spec_section5_checks.rs a_bound_the_aliased_type_already_requires_draws_redundant_alias_bound (warn-only) |
| s5.439 | Type aliases | Ord' on alias 'SortedKeyMap' is implied by the underlying type 'SortedMap[T, i32]'; consider removing`. | tests/spec_section5_checks.rs a_bound_the_aliased_type_already_requires_draws_redundant_alias_bound (warn-only) |
| s5.440 | Type aliases | `#[allow(redundant_alias_bound)]` keeps it when the author wants to document the requirement. | tests/spec_section5_checks.rs a_bound_the_aliased_type_already_requires_draws_redundant_alias_bound (warn-only) |
| s5.441 | Type aliases | **Diagnostic.** When a use site does not satisfy an alias's bound, the error is `E0262 TypeAliasBoundNotSatisfied`: | an_alias_bound_is_checked_at_the_use_site_e0262; tests/spec_section5_checks.rs an_unsatisfied_alias_bound_is_one_e0262_naming_the_whole_bound |
| s5.442 | Type aliases | ``` error[E0262]: 'f64' does not satisfy 'T: Eq + Hash' required by alias 'Counts' --> src/lookup.kara:42:26 / 42 /     let by_score: Counts | an_alias_bound_is_checked_at_the_use_site_e0262; tests/spec_section5_checks.rs an_unsatisfied_alias_bound_is_one_e0262_naming_the_whole_bound |
| s5.443 | Type aliases | **Across packages.** The bounds of a `pub type` are part of its published interface. | versioning rationale, no cell |
| s5.444 | Type aliases | Removing or weakening a bound is not a breaking change, because callers passing tighter types still satisfy it. | versioning rationale, no cell |
| s5.445 | Type aliases | Adding or tightening a bound is breaking, because callers passing types that met the old bound may not meet the new one. | versioning rationale, no cell |
| s5.446 | Type aliases | This matches how bounds evolve on `pub fn` signatures and `pub struct` parameters. | versioning rationale, no cell |
| s5.447 | Type aliases | **Why enforce the bounds instead of rejecting them.** The alternative is to forbid bounds on aliases, so they have to live on the aliased ty | rationale or cross-reference, no cell |
| s5.448 | Type aliases | That was rejected for two reasons: | rationale or cross-reference, no cell |
| s5.449 | Type aliases | 1. | rationale or cross-reference, no cell |
| s5.450 | Type aliases | **The bound documents the alias.** `type Counts[T: | rationale or cross-reference, no cell |
| s5.451 | Type aliases | Eq + Hash] = Map[T, i64]` says "keys must be `Eq + Hash`" at the place a reader looks first. | rationale or cross-reference, no cell |
| s5.452 | Type aliases | 2. | rationale or cross-reference, no cell |
| s5.453 | Type aliases | **Aliases that add constraints are useful.** `type SortedVec[T: | rationale or cross-reference, no cell |
| s5.454 | Type aliases | Ord] = Vec[T]` says the alias is for sorted contents. | rationale or cross-reference, no cell |
| s5.455 | Type aliases | The requirement is real; rejecting it would scatter it across every use site. | rationale or cross-reference, no cell |
| s5.456 | Distinct types | A distinct type is a zero-cost wrapper that keeps structurally identical but semantically different values apart: | distinct_types_keep_identical_bases_apart |
| s5.457 | Distinct types | ```kara,uses=d-load-user distinct type UserId = i64; distinct type PostId = i64; distinct type Meters = f64; distinct type Seconds = f64;  f | distinct_types_keep_identical_bases_apart |
| s5.458 | Distinct types | **No operation carries over by default.** A distinct type is opaque: no arithmetic and no comparison unless it opts in with `#[derive]`: | a_distinct_type_has_no_arithmetic_without_derive, a_distinct_type_has_no_comparison_without_derive |
| s5.459 | Distinct types | ```kara #[derive(PartialEq, Eq, PartialOrd, Ord, Hash, Display)] distinct type UserId = i64;  #[derive(PartialEq, Eq, PartialOrd, Ord, Arith | derived_arithmetic_ord_and_display_on_a_distinct_type |
| s5.460 | Distinct types | A distinct type does not inherit `Copy` from its base either ([core-semantics.md §1.1](core-semantics.md#1-values)); derive it when wanted. | a_distinct_type_does_not_inherit_copy |
| s5.461 | Distinct types | A distinct type may be generic: over `distinct type Ids[T] = Vec[T]`, `Ids([1])` is an `Ids[i64]`, and its `.raw()` is a `Vec[i64]`. | a_generic_distinct_type_raw_gives_the_base |
| s5.462 | Distinct types | A derive on a distinct type requires its base type to have the trait, as a struct's derive does of each field: | derive_eq_on_a_distinct_f64_is_refused |
| s5.463 | Distinct types | `#[derive(Eq)] distinct type M = f64` is refused. | derive_eq_on_a_distinct_f64_is_refused |
| s5.464 | Distinct types | `Default` is derivable, and `Count.default()` is `Count(i64.default())`; a distinct type with a `where` refinement cannot derive it, since i | derive_default_on_a_distinct_type_is_the_base_s_default; the `where` refinement part is deferred (s5.475) |
| s5.465 | Distinct types | **Conversion:** | distinct_types_keep_identical_bases_apart, derived_arithmetic_ord_and_display_on_a_distinct_type (`.raw()`) |
| s5.466 | Distinct types | - Wrap with the constructor: | distinct_types_keep_identical_bases_apart, derived_arithmetic_ord_and_display_on_a_distinct_type (`.raw()`) |
| s5.467 | Distinct types | `UserId(42)`. | distinct_types_keep_identical_bases_apart, derived_arithmetic_ord_and_display_on_a_distinct_type (`.raw()`) |
| s5.468 | Distinct types | - Unwrap with `uid.raw()`, which returns the base type. | distinct_types_keep_identical_bases_apart, derived_arithmetic_ord_and_display_on_a_distinct_type (`.raw()`) |
| s5.469 | Distinct types | On an owned value it gives up the base; on a borrowed one it copies a `Copy` base and otherwise returns `ref Base`, so `fn f(self) -> Vec[i6 | raw_on_a_borrowed_non_copy_base_returns_ref_returning_it_owned_is_a_ty |
| s5.470 | Distinct types | **Generics.** `fn find[T: | a_distinct_type_with_derive_eq_works_with_a_generic_find |
| s5.471 | Distinct types | Eq](items: | a_distinct_type_with_derive_eq_works_with_a_generic_find |
| s5.472 | Distinct types | Slice[T], target: | a_distinct_type_with_derive_eq_works_with_a_generic_find |
| s5.473 | Distinct types | T) -> Option[i64]` works on a `Vec[UserId]` when `UserId` derives `Eq`. | a_distinct_type_with_derive_eq_works_with_a_generic_find |
| s5.474 | Distinct types | For matching, a distinct type has its base type's values ([Pattern exhaustiveness](#pattern-exhaustiveness)). | NEW distinct_type_matches_by_its_base_values, NEW distinct_type_match_without_wildcard_refused; tests/distinct_exhaustive.rs |
| s5.475 | Distinct types | Value predicates on a distinct type (`distinct type Port = u16 where …`) belong to the verification track ([deferred.md](deferred.md#verific | deferred (deferred.md#verification), no cell |
| s5.476 | Pattern matching | `match` tests a value against patterns in order and runs the first arm that matches: | NEW pattern_forms_each_match, NEW worked_example_event_match |
| s5.477 | Pattern matching | ```kara,uses=d-vec2 const PI: f64 = 3.14159;  enum Shape { Circle { radius: f64 }, Rectangle { width: f64, height: f64 }, Triangle { a: Vec2 | NEW pattern_forms_each_match, NEW worked_example_event_match |
| s5.478 | Pattern matching | An arm is `PATTERN [if GUARD] => BODY`. | NEW guard_false_falls_through_to_the_next_arm |
| s5.479 | Pattern matching | A body is an expression, a block, or an assignment or compound assignment (`x => total += x,`). | NEW arm_bodies_and_commas |
| s5.480 | Pattern matching | A comma is required after an arm whose body is not a block, and optional after a block. | NEW arm_bodies_and_commas |
| s5.481 | Pattern matching | A `match` must be exhaustive ([Pattern exhaustiveness](#pattern-exhaustiveness)). | NEW empty_match_on_an_integer_refused, NEW missing_enum_variant_refused |
| s5.482 | Pattern matching | **Pattern forms.** | header |
| s5.483 | Pattern matching | / Pattern / Matches / /---/---/ | header |
| s5.484 | Pattern matching | / `_` / Anything; binds nothing / | NEW pattern_forms_each_match |
| s5.485 | Pattern matching | / `name` / Anything; binds it to `name` / | NEW pattern_forms_each_match |
| s5.486 | Pattern matching | / `ref name`, `mut ref name` / Anything; binds a borrow of it ([binding modes](#binding-modes-in-patterns)) / | NEW rebinding_mut_in_the_arm_body_and_mut_ref_pattern, NEW at_binding_ref_both_and_let_forms |
| s5.487 | Pattern matching | / `42`, `'a'`, `true`, a `const` / That value / | NEW pattern_forms_each_match |
| s5.488 | Pattern matching | / `lo..=hi` and the other [range patterns](#range-patterns) / A value in the range / | NEW pattern_forms_each_match |
| s5.489 | Pattern matching | / `name @ P` / What `P` matches; also binds the whole to `name` ([`@` bindings](#-bindings)) / | NEW pattern_forms_each_match |
| s5.490 | Pattern matching | / `Point { x, y: 0, .. }` / A struct or struct variant, field by field / | NEW pattern_forms_each_match |
| s5.491 | Pattern matching | / `Some(p)`, `Token.Integer(n)` / A tuple variant / | NEW pattern_forms_each_match |
| s5.492 | Pattern matching | / `Token.Eof` / A unit variant / | NEW pattern_forms_each_match |
| s5.493 | Pattern matching | / `(p, q)` / A tuple / | NEW pattern_forms_each_match |
| s5.494 | Pattern matching | / `[p, .., q]` / An array or slice, by position ([Slice and array patterns](#slice-and-array-patterns)) / | NEW pattern_forms_each_match, NEW slice_pattern_forms |
| s5.495 | Pattern matching | / `P \/ Q` / Either alternative ([Or-patterns](#or-patterns)) / | NEW pattern_forms_each_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.496 | Pattern matching | In a field pattern, `field` alone is short for `field: field`: the field name is both the selector and the new binding. | NEW pattern_forms_each_match (`Outer { inner: Inner { b }, tag: _ }`) |
| s5.497 | Pattern matching | Field patterns nest (`Outer { inner: | NEW pattern_forms_each_match (`Outer { inner: Inner { b }, tag: _ }`) |
| s5.498 | Pattern matching | Inner { b } }`). | NEW pattern_forms_each_match (`Outer { inner: Inner { b }, tag: _ }`) |
| s5.499 | Pattern matching | `{ field, .. }` matches some fields and skips the rest; `..` must come last, and `{ .. }` alone skips everything. | NEW pattern_forms_each_match (`{ x, y: 0, .. }`, `{ .. }`), NEW rest_marker_not_last_in_braces_refused |
| s5.500 | Pattern matching | Inside a pattern's braces, `..` is always the rest marker, never a range. | NEW pattern_forms_each_match |
| s5.501 | Pattern matching | A bare `..` is not a pattern on its own; use `_`. | NEW bare_rest_marker_is_not_a_pattern_refused (FIXED in this audit: the refusal now says to write `_`) |
| s5.502 | Pattern matching | **Binding modes.** Whether a binding moves, copies or borrows is decided by the pattern and the scrutinee's type, never by how the arm uses  | §7 binding-mode cells (conformance/design/functions) |
| s5.503 | Pattern matching | The rules are in [§7 Binding modes in patterns](#binding-modes-in-patterns). | §7 binding-mode cells (conformance/design/functions) |
| s5.504 | Pattern matching | **There is no `mut name` pattern.** `mut` before a whole `let` pattern makes every binding mutable ([Variable binding rules](#variable-bindi | NEW mut_name_pattern_refused, NEW rebinding_mut_in_the_arm_body_and_mut_ref_pattern (`let mut (a, b)`) |
| s5.505 | Pattern matching | In a `match`, `if let` or `while let` arm, rebind with `let mut x = x;` inside the body. | NEW rebinding_mut_in_the_arm_body_and_mut_ref_pattern |
| s5.506 | Pattern matching | `mut ref name` is a mutable borrow, not a mutable binding. | NEW rebinding_mut_in_the_arm_body_and_mut_ref_pattern (`Some(mut ref n)`) |
| s5.507 | Pattern matching | **Pattern guards.** An arm may add `if EXPR` between the pattern and `=>`. | NEW guard_false_falls_through_to_the_next_arm |
| s5.508 | Pattern matching | The guard runs only when the pattern matches; if it is false, matching continues with the next arm. | NEW guard_false_falls_through_to_the_next_arm |
| s5.509 | Pattern matching | A guarded arm does not count toward exhaustiveness, so an unguarded arm or a wildcard is still required. | NEW guarded_arms_do_not_cover_refused |
| s5.510 | Pattern matching | A guard's effects count toward the enclosing function's effects like any other subexpression, and its temporaries drop at the end of the gua | corpus/core drop_temp_match_guard; §12 effects cells |
| s5.511 | Pattern matching | ```kara,uses=d-score match score { Ok(n) if n >= 90 => "A", Ok(n) if n >= 70 => "B", Ok(_)            => "C or below",   // unguarded: requi | NEW guard_false_falls_through_to_the_next_arm |
| s5.512 | Or-patterns | An arm may list several patterns separated by `/`. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW worked_example_event_match |
| s5.513 | Or-patterns | It fires if any of them matches. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW worked_example_event_match |
| s5.514 | Or-patterns | ```kara,uses=d-event match event { MouseDown { x, y } / TouchStart { x, y } => handle_click(x, y), _ => {} } | NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW worked_example_event_match |
| s5.515 | Or-patterns | - Every alternative must bind the **same names with the same types**. | NEW or_pattern_binding_types_differ_refused; tests/spec_section5_checks.rs an_or_pattern_whose_alternatives_bind_different_names_is_refused |
| s5.516 | Or-patterns | `A(x) / B(y)` is an error, and so is an `x` whose type differs between alternatives. | NEW or_pattern_binding_types_differ_refused; tests/spec_section5_checks.rs an_or_pattern_whose_alternatives_bind_different_names_is_refused |
| s5.517 | Or-patterns | - Or-patterns may be nested: | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.518 | Or-patterns | `Foo(A / B)` means `Foo(A) / Foo(B)`. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.519 | Or-patterns | - A guard applies after either alternative matches: | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.520 | Or-patterns | `P1 / P2 if guard => body`. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.521 | Or-patterns | It runs once per match, not once per alternative. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.522 | Or-patterns | - Or-patterns work in `if let` and `while let`: | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.523 | Or-patterns | `if let Left(x) / Right(x) = val { … }`. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.524 | Or-patterns | - For exhaustiveness, an or-pattern arm counts for all its alternatives at once. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.525 | Range patterns | A range pattern matches a contiguous range of integer or `char` values: | NEW range_pattern_forms |
| s5.526 | Range patterns | / Form / Matches / /---/---/ | NEW range_pattern_forms |
| s5.527 | Range patterns | / `lo..=hi` / `[lo, hi]` / | NEW range_pattern_forms |
| s5.528 | Range patterns | / `lo..hi` / `[lo, hi)` / | NEW range_pattern_forms |
| s5.529 | Range patterns | / `lo..` / `[lo, ∞)` / | NEW range_pattern_forms |
| s5.530 | Range patterns | / `..=hi` / `(-∞, hi]` / | NEW range_pattern_forms |
| s5.531 | Range patterns | / `..hi` / `(-∞, hi)` / | NEW range_pattern_forms |
| s5.532 | Range patterns | ``` RANGE_PATTERN  = CONST_PAT_BOUND ".."  CONST_PAT_BOUND   // exclusive,    [lo, hi) / CONST_PAT_BOUND "..=" CONST_PAT_BOUND   // inclusiv | NEW range_pattern_forms, NEW named_range_bounds (grammar) |
| s5.533 | Range patterns | ```kara,uses=d-c-code-n match c { 'a'..='z' => "lowercase", 'A'..='Z' => "uppercase", '0'..='9' => "digit", _ => "other", }  match code { 20 | NEW range_pattern_forms |
| s5.534 | Range patterns | - When both bounds are present they must have the same type, integer or `char`. | tests/types_exprs_conformance.rs types_exprs_char_range_on_an_int_is_refused / int_range_on_a_char; NEW range_bounds_of_different_types_refused |
| s5.535 | Range patterns | Float ranges are not supported, because of NaN ordering. | tests/types_exprs_conformance.rs types_exprs_float_range_pattern_is_refused_with_the_reason |
| s5.536 | Range patterns | - A bounded inclusive range needs start ≤ end; `9..=0` is a compile error. | NEW reversed_range_pattern_refused |
| s5.537 | Range patterns | The check runs after named bounds are resolved. | NEW reversed_named_range_bounds_refused |
| s5.538 | Range patterns | - Range patterns compose with or-patterns (`0..=9 / 100..=199 => …`) and `@` bindings (`n @ 0..=9 => …`). | NEW range_pattern_forms (`0..=9 | 100..=199`, `k @ 10..20`) |
| s5.539 | Range patterns | - For exhaustiveness, a range pattern covers exactly its values, and ranges that together cover the whole type make a match exhaustive witho | NEW ranges_covering_the_type_are_exhaustive |
| s5.540 | Range patterns | `match n { 0..=127 => .., 128..=255 => .. }` on a `u8`, or `..=-1`, `0` and `1..` on an `i64`. | NEW ranges_covering_the_type_are_exhaustive |
| s5.541 | Range patterns | A gap is an error that names the first value not covered (`pattern `10` not covered` for `0..=9` alone on a `u8`), as in Rust. | NEW range_gap_names_the_first_uncovered_value_refused |
| s5.542 | Range patterns | **Named bounds.** A bound may be a literal or a path to a module-level `const` of integer or `char` type. | NEW named_range_bounds |
| s5.543 | Range patterns | The path is resolved during type checking and the constant's value is used as the bound: | NEW named_range_bounds |
| s5.544 | Range patterns | ```kara,uses=d-age32 const MIN_AGE: i32 = 18; const MAX_AGE: i32 = 65;  match age { ..MIN_AGE         => "minor", MIN_AGE..=MAX_AGE => "work | NEW named_range_bounds |
| s5.545 | Range patterns | A named bound follows the same type, ordering, exhaustiveness and composition rules as a literal; it only names a value. | NEW named_range_bounds |
| s5.546 | Range patterns | A path that does not resolve to such a constant is E0263 RangePatternBoundNotConst, `error[E0263]: range pattern bound 'PATH' must resolve t | NEW range_bound_not_a_const_refused |
| s5.547 | Range patterns | Two bounds of different resolved types fall under the same-type rule, and the diagnostic names both types. | NEW range_bounds_of_different_types_refused (FIXED in this audit: two constants of different types were accepted) |
| s5.548 | Range patterns | Calls and other constant expressions are not accepted in a pattern, only literals and bare paths, so that patterns stay simple enough for th | NEW call_in_a_range_pattern_refused (FIXED in this audit: the refusal was a stray-`(` parse error) |
| s5.549 | `@` bindings | `name @ P` tests `P` and also binds the whole value at that position to `name`: | NEW at_bindings_test_and_bind |
| s5.550 | `@` bindings | ```kara,uses=d-at-values match val { x @ Some(_) => f"got: {x}", None => "nothing", }  match age { n @ 0..=12 => f"child, age {n}", n @ 13.. | NEW at_bindings_test_and_bind |
| s5.551 | `@` bindings | - The binding has the type of the scrutinee at that position. | NEW at_bindings_test_and_bind |
| s5.552 | `@` bindings | - `@` composes with or-patterns: | NEW at_bindings_test_and_bind |
| s5.553 | `@` bindings | `x @ (A / B)` is valid when `A` and `B` have the same type, and `x @ Some(_) / x @ None` is valid because both alternatives bind `x` with th | NEW at_bindings_test_and_bind (FIXED in this audit: `x @ (K.A | K.B)` over a non-Copy enum was refused at run as a double consume) |
| s5.554 | `@` bindings | - For exhaustiveness, `x @ P` is the same as `P`. | NEW at_bindings_test_and_bind |
| s5.555 | `@` bindings | **Ownership.** `name` is an ordinary binding, so its mode follows [§7 Binding modes in patterns](#binding-modes-in-patterns). | NEW at_binding_ref_both_and_let_forms, §7 binding-mode cells |
| s5.556 | `@` bindings | Under a borrowed scrutinee, `name` and every binding inside `P` borrow, except that a binding of a `Copy` part copies it. | NEW at_binding_ref_both_and_let_forms, §7 binding-mode cells |
| s5.557 | `@` bindings | Under an owned scrutinee, a plain `name` moves the whole value (or copies it, if it is `Copy`). | NEW at_binding_over_an_owned_part_refused |
| s5.558 | `@` bindings | `P` may then not bind any non-`Copy` part of that value, by value or by `ref`, because the whole has already moved. | NEW at_binding_over_an_owned_part_refused |
| s5.559 | `@` bindings | To bind both the whole and a part, borrow both: | NEW at_binding_ref_both_and_let_forms |
| s5.560 | `@` bindings | `ref x @ Some(ref y)`. | NEW at_binding_ref_both_and_let_forms |
| s5.561 | `@` bindings | The same rule applies to nested `@` bindings (`outer @ Foo { field: inner @ Bar(value) }`): under an owned scrutinee, an outer binding that  | the E_AT_BINDING_DOUBLE_CONSUME tests in tests/typechecker.rs (nested `@`) |
| s5.562 | `@` bindings | **Refutability.** `@` itself never fails, so `name @ P` is refutable exactly when `P` is. | NEW refutable_at_binding_in_let_refused |
| s5.563 | `@` bindings | `let p @ Point { x, y } = point;` is accepted (the fields are `Copy`, so binding them does not conflict with `p`); `let x @ Some(y) = opt;`  | NEW at_binding_ref_both_and_let_forms, NEW refutable_at_binding_in_let_refused |
| s5.564 | Slice and array patterns | A slice pattern matches a sequence by position and may name a remainder. | NEW slice_pattern_forms |
| s5.565 | Slice and array patterns | Each `pᵢ` is a pattern and `..` is the rest marker: | NEW slice_pattern_forms |
| s5.566 | Slice and array patterns | / Form / Matches / /---/---/ | header |
| s5.567 | Slice and array patterns | / `[p₁, …, pₙ]` / Exactly n elements / | NEW slice_pattern_forms |
| s5.568 | Slice and array patterns | / `[p₁, …, pₖ, ..]` / A k-element head, tail ignored / | NEW slice_pattern_forms |
| s5.569 | Slice and array patterns | / `[.., q₁, …, qⱼ]` / A j-element tail, head ignored / | NEW slice_pattern_forms |
| s5.570 | Slice and array patterns | / `[p₁, …, pₖ, .., q₁, …, qⱼ]` / Head and tail, middle ignored / | NEW slice_pattern_forms |
| s5.571 | Slice and array patterns | / `[p₁, …, pₖ, ..rest]` / Head, with the tail named / | NEW slice_pattern_forms |
| s5.572 | Slice and array patterns | / `[..rest, q₁, …, qⱼ]` / Tail, with the head named / | NEW slice_pattern_forms |
| s5.573 | Slice and array patterns | / `[p₁, …, pₖ, ..rest, q₁, …, qⱼ]` / Head and tail, with the middle named / | NEW slice_pattern_forms |
| s5.574 | Slice and array patterns | ```kara fn describe(items: Slice[i64]) -> String { match items { []                                  => "empty", [only]                      | NEW slice_pattern_forms |
| s5.575 | Slice and array patterns | **Types matched.** Slice patterns apply to `Array[T, N]`, `Vec[T]` and `Slice[T]`. | NEW slice_pattern_forms (Slice, Vec, Array) |
| s5.576 | Slice and array patterns | They do not apply to `String` or `Str`: | NEW slice_pattern_on_a_string_refused |
| s5.577 | Slice and array patterns | UTF-8 characters vary in width, so positional byte patterns would cut characters. | NEW slice_pattern_on_a_string_refused |
| s5.578 | Slice and array patterns | Match `s.bytes()` (a `Slice[u8]`) or a collected `Vec[char]` instead. | NEW slice_pattern_over_bytes_and_chars |
| s5.579 | Slice and array patterns | **Element bindings.** | header |
| s5.580 | Slice and array patterns | - **Over a `Vec[T]` or a `Slice[T]`, the pattern sees a view.** Every element binding is a `ref`, or a `mut ref` when written `mut ref name` | NEW slice_pattern_forms (ref element bindings), slice_pattern_mut_ref_element_writes, slice_pattern_mut_ref_element_through_mut_ref_vec, slice_pattern_mut_ref_element_over_ref_param_refused, slice_pattern_mut_ref_element_over_immutable_local_refused (fixed by B-2026-11-24-36). slice_pattern_several_mut_ref_elements_write (several `mut ref` elements, fixed by B-2026-11-25-1) |
| s5.581 | Slice and array patterns | A `Vec[T]` moves whole or not at all. | rationale or cross-reference, no cell |
| s5.582 | Slice and array patterns | - **Over an owned `Array[T, N]`, the pattern destructures like a tuple.** Element bindings follow [§7 Binding modes in patterns](#binding-mo | NEW slice_pattern_forms (`let [h, ..rest] = a` over an owned Array) |
| s5.583 | Slice and array patterns | **The rest binding borrows, never moves**, even under an owned scrutinee; it is valid for the arm's body. | NEW slice_pattern_forms, NEW mut_ref_rest_writes_through |
| s5.584 | Slice and array patterns | An `Array[T, N]` is stored inline, so moving out a contiguous range would leave a half-initialized array. | rationale or cross-reference, no cell |
| s5.585 | Slice and array patterns | - On `Vec[T]` and `Slice[T]`, `..rest` binds a `Slice[T]`: a view, with no copy and no allocation. | NEW slice_pattern_forms (`rest.len()`) |
| s5.586 | Slice and array patterns | Written `..mut ref rest` against a mutable place (a `let mut` owner, a `mut ref` or a `mut Slice[T]`), it binds a `mut Slice[T]`. | NEW mut_ref_rest_writes_through |
| s5.587 | Slice and array patterns | - On `Array[T, N]` with a k-element head and a j-element tail, `..rest` covers `K = N − k − j` elements, computed by the same constant arith | NEW slice_pattern_forms (`[h, ..rest]` over Array[i64, 4] is length 3) |
| s5.588 | Slice and array patterns | If `K < 0`, the pattern is rejected with a length-mismatch diagnostic. | NEW array_rest_longer_than_the_array_refused |
| s5.589 | Slice and array patterns | Over an owned array, `..rest` is an `Array[T, K]`; over a `ref Array[T, N]`, a `ref Array[T, K]`; and written `..mut ref rest` over a `mut r | NEW slice_pattern_forms (owned array rest is an Array), array_pattern_through_a_ref_is_irrefutable, mut_ref_rest_over_a_mut_ref_array_is_a_sub_array (B-2026-11-25-26) |
| s5.590 | Slice and array patterns | **At most one rest marker per slice pattern.** `[a, .., b, .., c]` is a parse error, because the element positions would be ambiguous. | NEW two_rest_markers_refused |
| s5.591 | Slice and array patterns | Nested slice patterns (`[[..a, b], [c, ..d]]` over `Array[Array[T, M], N]`) each have their own single rest marker. | tests/param_patterns_full_grammar.rs; nested-array rest, no cell |
| s5.592 | Slice and array patterns | **Exhaustiveness.** | NEW array_patterns_exhaustive_by_positions |
| s5.593 | Slice and array patterns | - An `Array[T, N]` match is exhaustive when its fixed-position patterns cover all `N` positions, or when a rest marker covers the trailing p | NEW array_patterns_exhaustive_by_positions |
| s5.594 | Slice and array patterns | The rest counts as a wildcard whether or not it is named. | NEW array_patterns_exhaustive_by_positions |
| s5.595 | Slice and array patterns | - A `Vec[T]` or `Slice[T]` match needs an arm that covers every length. | NEW vec_match_without_empty_arm_refused, tests/slice_arm_reachability.rs |
| s5.596 | Slice and array patterns | `[x, ..rest]` covers every non-empty sequence, so `[]` or `_` is still needed for the empty one. | NEW vec_match_without_empty_arm_refused, tests/slice_arm_reachability.rs |
| s5.597 | Slice and array patterns | **Refutability.** On `Vec[T]` and `Slice[T]` a slice pattern is refutable, since the length may not match: it is allowed in `match`, `if let | NEW let_slice_pattern_on_a_vec_refused, NEW let_else_with_a_slice_pattern_on_a_vec |
| s5.598 | Slice and array patterns | On `Array[T, N]` it is refutable only if an element pattern is, and otherwise a plain `let` accepts it. | NEW slice_pattern_forms (`let [x, y, ..] = a`) |
| s5.599 | Pattern exhaustiveness | Kāra uses **Maranget's usefulness algorithm** ("Warnings for pattern matching", JFP 2007), the decidable algorithm that Rust, OCaml and Scal | algorithm description; the exhaustiveness cells below |
| s5.600 | Pattern exhaustiveness | It is specified here so implementers need not chase the paper, and so later extensions have a fixed baseline. | algorithm description; the exhaustiveness cells below |
| s5.601 | Pattern exhaustiveness | **Two judgments.** The algorithm is built on one primitive, **usefulness**: | algorithm description; the exhaustiveness cells below |
| s5.602 | Pattern exhaustiveness | $$U(P, q) = \text{there exists a value } v \text{ matched by } q \text{ that is not matched by any row of } P$$ | algorithm description; the exhaustiveness cells below |
| s5.603 | Pattern exhaustiveness | where `P` is a *pattern matrix* (the rows of a match, each a tuple of patterns) and `q` is a candidate pattern. | algorithm description; the exhaustiveness cells below |
| s5.604 | Pattern exhaustiveness | Two checks follow: | algorithm description; the exhaustiveness cells below |
| s5.605 | Pattern exhaustiveness | - **A match is exhaustive** iff `U(P, _)` is **false**: the wildcard is not useful against `P`, so every value of the scrutinee type is matc | NEW missing_enum_variant_refused, NEW empty_match_on_an_integer_refused |
| s5.606 | Pattern exhaustiveness | A non-exhaustive match produces a diagnostic naming one uncovered pattern, found by the same recursion. | NEW non_exhaustive_tuple_names_the_witness_refused, NEW range_gap_names_the_first_uncovered_value_refused |
| s5.607 | Pattern exhaustiveness | - **Arm `i` is reachable** iff `U(P[0..i], P[i])` is **true**: the arm is useful against the earlier arms. | NEW distinct_type_matches_by_its_base_values (the duplicate arm draws unreachable_arm and runs); tests/slice_arm_reachability.rs |
| s5.608 | Pattern exhaustiveness | An unreachable arm is a warning. | NEW distinct_type_matches_by_its_base_values (the duplicate arm draws unreachable_arm and runs); tests/slice_arm_reachability.rs |
| s5.609 | Pattern exhaustiveness | **How `U` is computed**, by recursion on the head of `q`: | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.610 | Pattern exhaustiveness | 1. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.611 | Pattern exhaustiveness | **`q = _` and every column is empty**: | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.612 | Pattern exhaustiveness | `U` is true iff the matrix has no rows. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.613 | Pattern exhaustiveness | This is the base case. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.614 | Pattern exhaustiveness | 2. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.615 | Pattern exhaustiveness | **`q` starts with a constructor `c(q1, …, qa)`**: | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.616 | Pattern exhaustiveness | *specialize* `P` to `c`. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.617 | Pattern exhaustiveness | Drop the rows whose first column does not match `c`, replace the first column of the others with the constructor's fields, and recurse on th | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.618 | Pattern exhaustiveness | 3. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.619 | Pattern exhaustiveness | **`q` starts with `_`**: if every constructor of the column's type is covered by some row's head, recurse on the *default matrix* (the rows  | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.620 | Pattern exhaustiveness | Otherwise `q` is useful, because a missing constructor witnesses a value `P` does not match. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.621 | Pattern exhaustiveness | 4. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.622 | Pattern exhaustiveness | **`q` starts with an or-pattern `q1 / q2`**: | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.623 | Pattern exhaustiveness | `U(P, q1 / q2) = U(P, q1) ∨ U(P_after_q1, q2)`, where `P_after_q1` is `P` with a row for `q1` added. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.624 | Pattern exhaustiveness | Specialization recurses through or-patterns without expanding them. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.625 | Pattern exhaustiveness | This is the full algorithm for finite constructor spaces. | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.626 | Pattern exhaustiveness | The rules below cover types whose constructor space is not finite (integers, floats, strings, `Vec`) and types that wrap others (distinct ty | algorithm description; NEW worked_example_event_match, NEW or_patterns_nest_guard_once_and_in_if_let_while_let, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.627 | Type-specific rules | **`bool`** has two constructors, `true` and `false`. | NEW bool_enum_and_struct_matches_are_exhaustive |
| s5.628 | Type-specific rules | A match with both arms is exhaustive without a wildcard: | NEW bool_enum_and_struct_matches_are_exhaustive |
| s5.629 | Type-specific rules | ```kara,uses=d-flag match flag { true  => "yes", false => "no",   // exhaustive: no wildcard needed } | NEW bool_enum_and_struct_matches_are_exhaustive |
| s5.630 | Type-specific rules | **Enums.** The constructors are the declared variants. | NEW bool_enum_and_struct_matches_are_exhaustive |
| s5.631 | Type-specific rules | Adding a variant breaks every match over the enum that has no wildcard arm; `#[non_exhaustive]` declares that a public enum's variant set is | NEW missing_enum_variant_refused; tests/cross_package_non_exhaustive.rs |
| s5.632 | Type-specific rules | **Structs and tuples** have one constructor. | NEW bool_enum_and_struct_matches_are_exhaustive, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.633 | Type-specific rules | Specialization always applies, and exhaustiveness and reachability carry through to the fields. | NEW bool_enum_and_struct_matches_are_exhaustive, NEW non_exhaustive_tuple_names_the_witness_refused |
| s5.634 | Type-specific rules | **Integers, floats, strings and `char`.** The constructor space (2^64 values for `i64`, unbounded for `String`) is not enumerated. | NEW integer_literals_never_exhaust_refused, NEW string_match_needs_a_wildcard_refused, NEW ranges_covering_the_type_are_exhaustive |
| s5.635 | Type-specific rules | A match over one of these types is exhaustive **only** if some row matches anything: | NEW integer_literals_never_exhaust_refused, NEW string_match_needs_a_wildcard_refused, NEW ranges_covering_the_type_are_exhaustive |
| s5.636 | Type-specific rules | `_`, a plain binding, or a half-open range that covers the remaining values. | NEW integer_literals_never_exhaust_refused, NEW string_match_needs_a_wildcard_refused, NEW ranges_covering_the_type_are_exhaustive |
| s5.637 | Type-specific rules | Listing every `i64` literal is still non-exhaustive; the algorithm does not try. | NEW integer_literals_never_exhaust_refused, NEW string_match_needs_a_wildcard_refused, NEW ranges_covering_the_type_are_exhaustive |
| s5.638 | Type-specific rules | Range patterns cover contiguous integer or `char` values (not floats). | NEW range_pattern_forms; tests/types_exprs_conformance.rs (float range refused) |
| s5.639 | Type-specific rules | **`Array[T, N]`.** `N` is known, so an array pattern `[p1, p2, …, pN]` specializes like a tuple: | NEW array_patterns_exhaustive_by_positions |
| s5.640 | Type-specific rules | `[a, b, c]` is exhaustive on `Array[T, 3]` iff each of `a`, `b` and `c` is exhaustive on `T`. | NEW array_patterns_exhaustive_by_positions |
| s5.641 | Type-specific rules | A rest pattern counts as a wildcard for the positions it covers ([Slice and array patterns](#slice-and-array-patterns)). | NEW array_patterns_exhaustive_by_positions |
| s5.642 | Type-specific rules | **`Vec[T]`, `Map[K, V]`, `String`.** Variable length. | header |
| s5.643 | Type-specific rules | A match over a `Map[K, V]` or a `String` is exhaustive only with a wildcard arm. | NEW string_match_needs_a_wildcard_refused |
| s5.644 | Type-specific rules | A match over a `Vec[T]` is checked one length at a time: a slice pattern `[p1, …, pn]` has the length `n`, and `[p1, …, pi, .., q1, …, qj]`  | NEW vec_lengths_checked_one_at_a_time, tests/slice_arm_reachability.rs (`[_, _]` after `[x, ..]` unreachable) |
| s5.645 | Type-specific rules | So `[]` and `[x, ..rest]` together cover every length, `[_, _]` after `[x, ..]` is unreachable, and `[.., 0]` after `[0, ..]` is reachable ( | NEW vec_lengths_checked_one_at_a_time, tests/slice_arm_reachability.rs (`[_, _]` after `[x, ..]` unreachable) |
| s5.646 | Type-specific rules | Past the longest fixed pattern and the longest prefix plus the longest suffix, every length behaves alike, so the check is finite. | NEW vec_lengths_checked_one_at_a_time, tests/slice_arm_reachability.rs (`[_, _]` after `[x, ..]` unreachable) |
| s5.647 | Type-specific rules | `Map[K, V]` has no positional pattern; match it as a whole and use `.get()` or iteration in the arm. | statement of absence, no cell |
| s5.648 | Type-specific rules | `String` has no slice patterns. | NEW slice_pattern_on_a_string_refused |
| s5.649 | Type-specific rules | **`Never`.** `match x { }` on a scrutinee of type `Never` is exhaustive, because the empty matrix misses nothing. | NEW empty_match_on_never_is_exhaustive, NEW empty_match_on_an_integer_refused |
| s5.650 | Type-specific rules | This is the only type for which an empty match is well formed. | NEW empty_match_on_never_is_exhaustive, NEW empty_match_on_an_integer_refused |
| s5.651 | Type-specific rules | **Distinct types.** `distinct type UserId = i64` is an opaque nominal wrapper with exactly its base type's constructor space. | NEW distinct_type_matches_by_its_base_values, NEW distinct_type_match_without_wildcard_refused, tests/distinct_exhaustive.rs |
| s5.652 | Type-specific rules | Two `UserId(5)` patterns are the same literal, `UserId(0)` and `UserId(1)` are different literals, and a wildcard is needed to cover every ` | NEW distinct_type_matches_by_its_base_values, NEW distinct_type_match_without_wildcard_refused, tests/distinct_exhaustive.rs |
| s5.653 | Type-specific rules | Distinct types stop values crossing between types in the type checker; they are transparent to the pattern algorithm. | NEW distinct_type_matches_by_its_base_values, NEW distinct_type_match_without_wildcard_refused, tests/distinct_exhaustive.rs |
| s5.654 | Type-specific rules | **`shared struct` and `shared enum`** follow the same rules as their plain counterparts: the handle is transparent to matching. | NEW shared_scrutinee_binds_parts_by_ref_and_copies_the_handle |
| s5.655 | Type-specific rules | A shared scrutinee counts as a `ref` scrutinee for binding modes, so a binding of a part is a `ref` and a binding of the whole copies the ha | NEW shared_scrutinee_binds_parts_by_ref_and_copies_the_handle |
| s5.656 | Or-patterns and guards | **Or-patterns do not multiply rows.** An arm `P1 / P2 / P3 => body` is three matrix rows `[P1; i]`, `[P2; i]`, `[P3; i]` that share body `i` | NEW or_patterns_nest_guard_once_and_in_if_let_while_let |
| s5.657 | Or-patterns and guards | Specialization walks through or-patterns one alternative at a time (step 4 above) and caches intermediate results, so `Foo(A / B / C)` deep  | implementation property, no cell |
| s5.658 | Or-patterns and guards | **Guards do not count toward exhaustiveness.** The algorithm treats a guarded arm `P if G => body` as if `G` were never true, so it can neve | NEW guarded_arms_do_not_cover_refused |
| s5.659 | Or-patterns and guards | A match is exhaustive iff its unguarded arms are. | NEW guarded_arms_do_not_cover_refused |
| s5.660 | Or-patterns and guards | **Even a trivially true guard does not count.** `if true` and `if 1 == 1` do not satisfy exhaustiveness either: guards are not evaluated for | NEW trivially_true_guard_does_not_cover_refused |
| s5.661 | Or-patterns and guards | Accepting "obviously true" guards would create a cliff between the guards a prover handles and guards one character away from them, and the  | rationale or cross-reference, no cell |
| s5.662 | Or-patterns and guards | **A guard on an or-pattern runs once per match.** In `P1 / P2 if G => body`, the or-pattern decides which alternative matched and `G` runs o | NEW or_patterns_nest_guard_once_and_in_if_let_while_let (the guard ran once) |
| s5.663 | Or-patterns and guards | The alternatives bind the same names with the same types, so the guard always sees one well-typed set of bindings. | NEW or_patterns_nest_guard_once_and_in_if_let_while_let (the guard ran once) |
| s5.664 | Or-patterns and guards | Neither compile time nor run time grows with the number of alternatives. | implementation property, no cell |
| s5.665 | `let`, `if let`, `while let` | **`let PAT = expr` needs an irrefutable pattern**: the one-row matrix `[PAT]` must cover every value of the type, that is `U([PAT], _)` is f | NEW refutable_let_suggests_if_let_or_let_else_refused |
| s5.666 | `let`, `if let`, `while let` | A refutable pattern is rejected with a diagnostic suggesting `if let` or `let…else`. | NEW refutable_let_suggests_if_let_or_let_else_refused |
| s5.667 | `let`, `if let`, `while let` | **`if let` and `while let` are not checked for exhaustiveness.** They test one pattern, which is expected to be refutable. | tests/types_exprs_conformance.rs types_exprs_irrefutable_if_let_warns / while_let_warns / let_else_warns / refutable_if_let_is_silent |
| s5.668 | `let`, `if let`, `while let` | An irrefutable pattern there is a warning, `irrefutable_let_pattern` (the `else` branch is unreachable, or the loop never ends by a failed m | tests/types_exprs_conformance.rs types_exprs_irrefutable_if_let_warns / while_let_warns / let_else_warns / refutable_if_let_is_silent |
| s5.669 | `let`, `if let`, `while let` | The same warning covers `let … else` with an irrefutable pattern, whose `else` block is dead code (review-thread ruling 2026-11-16). | tests/types_exprs_conformance.rs types_exprs_irrefutable_if_let_warns / while_let_warns / let_else_warns / refutable_if_let_is_silent |
| s5.670 | `let`, `if let`, `while let` | **`let…else`** takes a refutable pattern. | NEW let_else_with_a_slice_pattern_on_a_vec, NEW let_else_block_must_diverge_refused |
| s5.671 | `let`, `if let`, `while let` | Its `else` block must diverge, which the type checker verifies by giving the block type `Never`. | NEW let_else_with_a_slice_pattern_on_a_vec, NEW let_else_block_must_diverge_refused |
| s5.672 | `let`, `if let`, `while let` | No exhaustiveness check is needed, because the `else` block handles every other value. | NEW let_else_with_a_slice_pattern_on_a_vec, NEW let_else_block_must_diverge_refused |
| s5.673 | Empty and unreachable arms | **An empty match on a type other than `Never` is non-exhaustive.** `match x { }` with `x: | NEW empty_match_on_an_integer_refused; tests/types_exprs_conformance.rs types_exprs_empty_match_names_every_variant |
| s5.674 | Empty and unreachable arms | T` and `T ≠ Never` is rejected with the full-type-uncovered diagnostic. | NEW empty_match_on_an_integer_refused; tests/types_exprs_conformance.rs types_exprs_empty_match_names_every_variant |
| s5.675 | Empty and unreachable arms | Add arms that cover `T`, or a wildcard arm that calls `unreachable()`. | NEW empty_match_on_an_integer_refused; tests/types_exprs_conformance.rs types_exprs_empty_match_names_every_variant |
| s5.676 | Empty and unreachable arms | **An unreachable arm is a warning, not an error.** The reachability judgment finds an arm whose pattern earlier arms already cover. | NEW distinct_type_matches_by_its_base_values (unreachable arm warns, program runs) |
| s5.677 | Empty and unreachable arms | The arm is kept in the generated code rather than silently dropped, so changing an earlier arm later cannot bring unrelated code back to lif | implementation property, no cell |
| s5.678 | Worked example | ```kara,uses=d-evt enum Event { MouseDown { x: i64, y: i64 }, MouseUp   { x: i64, y: i64 }, KeyDown   { code: i64 }, Tick, }  match evt { Mo | NEW worked_example_event_match |
| s5.679 | Worked example | The exhaustiveness check runs as follows: | NEW worked_example_event_match (walk-through) |
| s5.680 | Worked example | 1. | NEW worked_example_event_match (walk-through) |
| s5.681 | Worked example | Start with the three rows `R0`, `R1`, `R2` and the candidate `q = _`. | NEW worked_example_event_match (walk-through) |
| s5.682 | Worked example | 2. | NEW worked_example_event_match (walk-through) |
| s5.683 | Worked example | `q` is a wildcard, so check whether every constructor of `Event` heads some row. | NEW worked_example_event_match (walk-through) |
| s5.684 | Worked example | All four do: | NEW worked_example_event_match (walk-through) |
| s5.685 | Worked example | `R0` expands through its or-pattern to cover `MouseDown` and `MouseUp`, `R1` covers `KeyDown`, and `R2` covers `Tick`. | NEW worked_example_event_match (walk-through) |
| s5.686 | Worked example | 3. | NEW worked_example_event_match (walk-through) |
| s5.687 | Worked example | Recurse into each constructor's specialized matrix. | NEW worked_example_event_match (walk-through) |
| s5.688 | Worked example | For `MouseDown` it has one row `[_, _]` (the `{ x, y }` bindings) and the candidate is `[_, _]`. | NEW worked_example_event_match (walk-through) |
| s5.689 | Worked example | That row covers every pair of `i64`s, so `MouseDown` is exhausted, and likewise `MouseUp`, `KeyDown` and `Tick`. | NEW worked_example_event_match (walk-through) |
| s5.690 | Worked example | 4. | NEW worked_example_event_match (walk-through) |
| s5.691 | Worked example | No branch is useful, so `U(P, _)` is false: the match is exhaustive. | NEW worked_example_event_match (walk-through) |
| s5.692 | Worked example | Without the `Tick` arm, step 2 finds `Tick` uncovered and `U(P, _)` is true with the witness `Tick`; the compiler reports `non-exhaustive ma | NEW worked_example_missing_tick_refused (DOC FIX: the text quoted `missing Tick`; the diagnostic is `missing variants: Tick`) |
| s5.693 | Worked example | With a duplicate `MouseDown { x, y } => …` arm after `R0`, the reachability check computes `U([R0], duplicate)`, finds `MouseDown` already c | NEW distinct_type_matches_by_its_base_values (duplicate arm warns); tests/slice_arm_reachability.rs |
| s5.694 | Worked example | --- | rule |
