# design.md § 3 Lexical structure: sentence-to-cell map

Status: covered, partial, FAILS (a cell fails; the row named), not testable, not drafted. Cell names without a prefix are in `conformance/design/lexical/`; `tests/` names a Rust test as `file::test`. Line numbers are docs/design.md lines (§ 3 is lines 168 to 561).

**Totals (2026-11-25, after the gap audit and its fixes):** 173 normative sentences, 156 covered, 6 partial, 1 FAILS (filed as B-2026-11-25-9 and -10; -3 to -7 found and fixed by the audit; -10 fixed, its field half dropped by the review ruling that a field or method may use a primitive name), 6 not testable, 4 not drafted (`karac doc` rendering and `karac fix --edition`: a cell cannot hold a project, and the flag does not exist yet). Before the audit (estimated from which rows cite a pre-existing cell or test): about 76 covered, 25 partial, 61 gaps. `(new)` marks a cell the audit added; every new refusal has a control beside it where one exists. Lane run of this directory (interp,build): 140 passed, 0 failed before; 342 passed, 10 failed after the new cells (139 new); 348 passed, 5 failed after the fixes (the 5 cells of -7 and -8).

## Identifiers and naming

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 170 | Where `syntax.md` §1 and § 3 differ, § 3 governs | not testable | (a rule about the documents) |
| 174 | The case class is set by the pattern of ASCII letters in the identifier | covered | cn1_struct_const_class (new: `HTTP` is Const class), cn7_one_letter_const_and_type, raw_ident_case_class_after_prefix (new) |
| 176 | The case class is the same in every compiler and version | not testable | |
| 182 | Type class: first letter uppercase and a later lowercase letter, or exactly one letter | covered | cn1_every_type_declaration_ok (new), cn7_one_letter_type_is_type_class (new), cn4_acronym_type_name_warns (new: `HTTPClient` is Type class) |
| 183 | Const class: every letter uppercase (digits and `_` allowed) | covered | cn7_one_letter_const_and_type, cn3_const_type_class (new), cn1_struct_const_class (new) |
| 184 | Value class: first letter lowercase, or begins with `_` | covered | cn7_one_letter_const_and_type (`_unused`), cn5_underscore_suppresses_unused (new) |
| 204 | CN-1: struct, enum, trait, type, distinct type, generic type parameter, effect resource names are Type class | covered | cn1_struct_value_class, cn1_enum_value_class, cn1_trait_value_class, cn1_type_alias_value_class, cn1_distinct_type_value_class, cn1_generic_param_value_class, cn1_effect_resource_value_class, cn1_struct_const_class (all new but the first); control cn1_every_type_declaration_ok (new) |
| 204 | `struct user_account { ... }` is a compile error | covered | cn1_struct_value_class |
| 205 | CN-2: fn, let, parameter, field names are Value class; `fn ReadFile()` is an error | covered | cn2_fn_type_class, cn2_let_type_class, cn2_param_type_class, cn2_field_type_class (new) |
| 205 | A module file name is Value class: `src/DbClient.kara` is an error, rename it `src/db_client.kara` | covered | tests/walker_module_name_class.rs a_type_class_module_file_is_refused_with_its_snake_case_name |
| 206 | CN-3: const items and const generic parameters are Const class; `const max_retries` is an error | covered | cn3_const_value_class, cn3_const_type_class (new), cn3_const_generic_value_class (new) |
| 206 | `let` and `let mut` introduce Value-class identifiers | covered | cn3_let_const_class, cn3_let_mut_const_class, cn2_let_type_class (new) |
| 207 | CN-4: acronyms as words (`HttpClient`, not `HTTPClient`); `HTTPClient` is formally Type class | covered | cn4_acronym_type_name_warns (new: accepted, warns naming `HttpClient`), control cn4_acronym_as_word_quiet (new) |
| 208 | CN-5: a Value-class identifier may begin with `_`; the class is unchanged | covered | cn7_one_letter_const_and_type, cn5_underscore_suppresses_unused (new) |
| 208 | The underscore suppresses unused-binding diagnostics | covered | cn5_underscore_suppresses_unused (new), control cn5_unused_binding_warns (new; the lint is `#[warn(unused_variable)]`) |
| 208 | Type-class and Const-class identifiers may not begin with `_` | covered | cn5_type_leading_underscore, cn5_const_leading_underscore (new) |
| 209 | CN-6: enum variants are Type class | covered | cn6_variant_value_class |
| 210 | CN-7: a single uppercase letter is Type class | covered | cn7_one_letter_const_and_type (`W[T]`), cn7_one_letter_type_is_type_class (new) |
| 210 | A one-letter name after `const` (item, block `const N`, const generic `const N: i64`) is Const class | covered | cn7_one_letter_const_and_type, cn7_block_const_and_const_generic (new) |
| 210 | A lowercase one-letter name is still Value class there (`const n` refused) | covered (fixed, B-2026-11-25-4) | cn7_one_letter_lowercase_const (new): refused, but its help says "a name with a single letter is always Type-class, so no capitalization change can make it Const-class", which contradicts this rule (`N` would do) |
| 211 | CN-8: a foreign name may be any ASCII at the boundary, but the Kāra-visible name must follow the rules | covered | cn8_ffi_names (new: `abs`), cn8_ffi_fn_without_rename_refused, cn8_ffi_type_without_rename_refused (new) |
| 211 | `#[kara_name("...")]` rebinds a non-conforming foreign name | covered | cn8_ffi_names (new: `type xcb_window_t` used as `XcbWindow`) |
| 224 | Primitive type names cannot be bound: `let i64 = 3;` is an error | covered | primitive_name_as_binding, primitive_name_as_param_refused (new) |
| 224 | Primitive type names cannot be declared as items | covered | primitive_name_as_fn_refused (new; B-2026-11-25-10); control reserved_type_names_control (new) |
| 224 | A field or method may use a primitive type name, since it is reached after `.` (review ruling 2026-11-25) | covered | primitive_name_as_field_or_method_allowed (new) |
| 224 | As a path's first segment a primitive name always names the type (`i64.MAX`, `u32.size_of()`, `char.try_from(n)`) | covered | primitive_type_paths (new); primitive_name_as_fn_refused (an item can no longer take the name, B-2026-11-25-10) |
| 224 | `Fn`, `MutFn` and `OnceFn` are reserved type names too | covered | fn_type_name_reserved_struct, mutfn_type_name_reserved_enum, oncefn_type_name_reserved_trait (new; B-2026-11-25-10); control reserved_type_names_control (new) |
| 228 | Rationale: how a path's first segment is read | not testable | rationale |
| 238 | Identifiers are ASCII; a non-ASCII identifier is a parse error | covered | non_ascii_identifier; tests/lexer.rs test_non_ascii_identifier_mid_word_emits_one_error |

## Keywords

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 244 | A hard keyword is never an identifier except through `r#` (outside member names) | covered | hard_keyword_*_as_binding (11 old, 22 new: with the next two, every hard keyword of lines 246-249), hard_keyword_{mut,ref,false}_as_fn_name (new), hard_keyword_as_field_refused (new) |
| 244 | `r#` makes every escapable hard keyword usable | covered | hard_keywords_escape_with_raw (new, passes), raw_identifiers; raw_ident_true_false_literals_still_literals (new) fixed, B-2026-11-25-3 |
| 251 | Contextual keywords are ordinary identifiers outside their position | covered | contextual_keywords_are_names, contextual_keyword_doc_examples (new); tests/contextual_keywords.rs every_soft_word_is_a_binding_a_parameter_and_a_function_name |
| 253 | Each contextual keyword is a keyword in its one position | covered | contextual_keywords_still_keywords (new: `distinct type`, `marker trait`, `shared struct`, `own` beside the same words as names); tests/contextual_keywords.rs soft_words_still_start_their_constructs |
| 273 | `struct IoStats { reads, writes, blocks }`, `fn union(mut ref self, ..)`, `fn create_server(host: String, ..)` are legal | covered | contextual_keyword_doc_examples (new) |
| 279 | `use` and `mod` are not keywords | covered | use_and_mod_bare_are_names, deferred_words_and_use_mod_are_names |
| 279 | Deferred-feature words are not keywords in v1 | covered | deferred_words_and_use_mod_are_names |
| 281 | The logical operators are `and`, `or`, `not` | covered | keywords_as_member_names, symbols fixes below |
| 281 | `&&`, `\|\|`, prefix `!` are rejected with a fix that rewrites them | covered | symbol_and_refused, symbol_or_refused, symbol_bang_refused (.fixed) |
| 283 | Any keyword is a member name after `.` | covered | keywords_as_member_names, keyword_member_raw_and_bare_same (new: `c.move`, `c.await`), keyword_method_names_reserved_and_trait (new) |
| 283 | Any keyword names a method or associated function declared in an `impl` or `trait` | FAILS, B-2026-11-25-9 | keyword_method_names_reserved_and_trait (new, passes: `await`, `const`, trait `yield`); keyword_assoc_fn_called_bare (new): `Handle.type()` / `Handle.match(2)` fail with E0516; control keyword_assoc_fn_called_raw (new) |
| 283 | A free function, a local, a field declaration or a type still needs `r#` | covered | hard_keyword_as_free_fn_refused, hard_keyword_as_field_refused (new), hard_keyword_*_as_binding |
| 283 | Declaring or calling such a member with `r#` is legal and means the same | covered | keyword_member_raw_and_bare_same (new), keyword_assoc_fn_called_raw (new) |
| 287 | A reserved word as an identifier is E_RESERVED_KEYWORD with the fix `r#name` | covered | reserved_* (16 cells), reserved_async (.fixed), reserved_pure (new) |
| 287 | `priv` cannot be escaped | covered | reserved_priv_cannot_escape, reserved_priv (new: no `r#priv` advice) |
| 287 | Reserved words escape with `r#` | covered | reserved_word_escaped_ok (new, all 17) |
| 299 | `move` reports E_MOVE_NOT_USED and suggests `.clone()` | covered | move_closure_refused |
| 301 | `pure` before `fn` in an `extern` block declares no effects | covered | pure_extern_fn (new, .effects `magnitude =`) |
| 301 | Only `extern` functions take `pure` | covered | pure_outside_extern_refused (new) |
| 305 | The list is closed for v1; adding a keyword needs an edition | not testable | |

## Raw identifiers

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 309 | `r#` followed by an identifier-start makes a keyword an ordinary identifier | partial | raw_identifiers, hard_keywords_escape_with_raw (new); `r#true` / `r#false` FAIL (row below) |
| 312 | The examples: `let r#async`, `fn r#try`, field `r#move`, `handle.r#await` == `handle.await`, a raw module-path segment | covered | raw_identifiers, keyword_member_raw_and_bare_same (new), raw_ident_positions (new: `import std.r#encoding`) |
| 319 | The escape is purely lexical; symbol tables, diagnostics see the bare name | covered (fixed, B-2026-11-25-3) | raw_ident_diagnostics_bare_name (new, passes); raw_ident_true_false_literals_still_literals (new): any `r#true` / `r#false` anywhere makes every bare `true` / `false` before `,` `;` `)` an E_RESERVED_KEYWORD, including the prelude's, so the escape is not lexical. Mangled names and debug info not drafted |
| 319 | `r#foo` on a non-keyword is legal and means `foo` | covered | raw_identifiers (`r#foo` / `foo`), raw_ident_positions (new) |
| 321 | Case class is decided after the prefix: `r#async` Value, `r#Async` Type | covered | raw_ident_case_class_after_prefix (new), raw_ident_type_class_as_binding_refused, raw_ident_value_class_as_struct_refused (new) |
| 323 | Raw in every binding position (let, fn, struct, enum, variants, generic params, params, fields, aliases, traits, methods) | covered | raw_ident_positions (new), raw_identifiers |
| 323 | Raw in every reference position (`r#name`, `obj.r#name`, `module.r#name`, `r#Name[T]`) | covered | raw_ident_positions (new) |
| 323 | Raw in an attribute name `#[r#name]` | covered | raw_ident_positions (new: `#[r#non_exhaustive]`) |
| 323 | Attribute paths have one segment (except `karac::` / `diagnostic::`); `#[tool::name]` is an error | covered | attribute_tool_path_refused (new) |
| 326 | `r#` inside comments and string literals is text | covered | raw_ident_in_text_is_text (new) |
| 327 | `r#` cannot appear in a label | covered | raw_ident_in_label_refused (new), control label_and_char_literal (new) |
| 328 | `r#` cannot apply to operators or punctuation | covered | raw_ident_on_operator_refused (new) |
| 330 | Structural markers (`self`, `Self`, `_`, `super`, `crate`, `pub`, `priv`, `private`, `mut`, `ref`) cannot be escaped: E_RAW_IDENT_NOT_ALLOWED | covered | raw_ident_structural_* (9), reserved_priv_cannot_escape |
| 341 | `r#` + identifier-start is a raw identifier; `r"`, `r#"`, `r##"` start a raw string | covered | raw_ident_in_text_is_text (new: `let r#r = r#"..."#`), raw_strings; tests/lexer.rs test_raw_identifier_does_not_intercept_r_string_prefix |
| 343 | `karac fix --edition` will insert the prefix | not drafted | the flag arrives with the first edition boundary |

## Symbols

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 347 | The delimiter, punctuation, arithmetic, comparison, bitwise and assignment symbols | covered | symbols_operators (new: every operator and compound assignment, `..`, `..=`, `??`) |
| 367 | `#[` starts an attribute | covered | raw_ident_positions (new), cn8_ffi_names (new) |
| 368 | `'` + identifier and no closing `'` is a label; with a closing quote a character literal | covered | label_and_char_literal (new) |
| 369 | There is no `?.`: `f()?.x` is `(f()?).x` | covered | question_then_dot_is_postfix (new) |
| 369 | There is no `\|>` | covered | pipe_operator_refused (new) |

## Numeric, character and boolean literals

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 373 | Integers are decimal, hex, binary or octal | covered | integer_bases_separators_suffixes; conformance/literals.py |
| 373 | `_` stands only between digits; `1__0`, `1_`, `0x_FF`, `1_000_.5` are errors, each with a fix | covered | separator_* (4, .fixed) |
| 373 | A suffix fixes the type (all twelve suffixes) | covered | integer_bases_separators_suffixes, integer_all_suffixes (new), suffix_fixes_type (new: `255u8.wrapping_add(1)` is 0) |
| 374 | Floats: a point, an exponent, or both | covered | float_forms, int_with_exponent_no_point_is_float (new) |
| 374 | `1f32` is not a float; write `1.0f32` | covered | int_with_f32_suffix (.fixed) |
| 374 | The exponent marker is `e` or `E`, optionally followed by `+` or `-` | covered | float_forms, float_suffix_and_exponent_signs (new), float_exponent_without_digits_refused (new) |
| 374 | `_` groups digits in the mantissa and the exponent | covered | float_forms |
| 374 | A point is followed by a digit: `1.` and `1.e5` are errors with fixes `1.0`, `1.0e5` | covered | float_point_without_digit, float_point_before_exponent (.fixed) |
| 374 | A float may carry `f32` or `f64` | covered | float_forms, float_suffix_and_exponent_signs (new: `2.5f64`) |
| 374 | The data track adds `f16` and `bf16` | not testable | data track; tests/lexer.rs test_f16_bf16_float_literal_suffixes for the lexing |
| 375 | Without a suffix a literal defaults to `i64` / `f64` | covered | unsuffixed_literal_defaults (new) |
| 375 | The value is range-checked against its type at compile time | covered | i8_literal_128_out_of_range, u8_literal_256_out_of_range, suffix_u8_256_out_of_range, i64_literal_past_max, negative_unsigned_literal, suffix_i16_out_of_range_refused (new), literal_range_from_parameter_refused + control (new) |
| 375 | A float is out of range when it rounds to infinity in its type | covered | f32_literal_rounds_to_infinity, f64_literal_rounds_to_infinity, f32_expected_type_literal_past_max_refused (new) |
| 375 | One that rounds to zero or to a subnormal is in range | covered | float_forms (`1e-400`), float_suffix_and_exponent_signs (new: `5e-324`, `1e-45f32`, `1e-50` as f32) |
| 376 | Characters `'a'`, `'\n'`, `'\u{1F600}'` are `char` | covered | char_and_text_escapes, char_escape_past_max, char_surrogate_escape |
| 376 | `b'A'` is a byte literal | covered | byte_literals |
| 377 | Booleans are `true` and `false` | covered | keywords_as_member_names, symbols_operators (new) |

## String literals

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 383 | `"..."` may span lines; the newline is verbatim, no dedent | covered | basic_string_spans_lines_verbatim |
| 384 | `"""..."""` is multi-line with dedent | covered | triple_quoted_dedent |
| 385 | `r"..."`, `r#"..."#`: a backslash is ordinary | covered | raw_strings |
| 386 | `f"..."`, `f"""..."""` are interpolated | covered | fstring_braces_and_holes, fstring_triple_quoted (new) |
| 387 | `b"..."` is `Array[u8, N]` | covered | byte_literals |
| 388 | `c"..."` is NUL-terminated with byte escapes plus `\u{...}` | covered | c_string_literal_len, c_string_is_copy_and_source_utf8 (new) |
| 390 | Escapes `\n \t \r \\ \' \" \0 \u{...} \xHH` in text and character literals | covered | char_and_text_escapes, text_escapes_all (new) |
| 390 | A text literal must be valid UTF-8 | covered | text_hex_escape_not_utf8 |
| 390 | There is no `\{` escape; in an f-string a literal brace is `{{` / `}}` | covered | no_brace_escape, fstring_braces_and_holes |
| 392 | A prefix is recognized only when it touches the quote; `f`, `r`, `b`, `c` are names otherwise | partial | fstring_prefix_letters_are_names (new); tests/lexer.rs test_bare_identifier_b_still_legal. `f "x"` lexing as two tokens has no cell (its result is a parse error with no promised text) |
| 392 | `rf`, `br`, `rb`, `cr` are focused lexer errors | covered | prefix_combination_* (4); tests/lexical_conformance_2026_11_16.rs lex_combined_string_prefixes_are_one_focused_error |
| 392 | No other prefix is reserved | partial | no cell: a would-be prefix is an identifier followed by a string, a parse error with no promised text |
| 394 | A string literal is `String` by default and `Str` where `Str` is expected, in every scope | covered | string_literal_type_default_and_str (new: `push_str` on the default, `Str` argument and annotation, `const NAME: Str`) |
| 396 | A list literal iterated and collected where written holds its items' slot types (`Map[char, Str]`, `Vec[(char, Str)]`) | covered | string_literal_type_default_and_str (new) |
| 397 | A `Str` literal is static data; it may go in a `const` or a global | covered | string_literal_type_default_and_str (new: `const NAME: Str`) |
| 398 | A literal in a `ref String` / `ref T` slot borrows nothing and may be bound and kept (`unwrap_or("")`, `unwrap_or(0)`) | covered | string_literal_ref_slot_kept (new) |
| 399 | A literal receiving a `Str` method is a `Str` view and may be kept (`"0123…".bytes()`) | covered | string_literal_receiver_str_view (new) |
| 399 | A literal receiving a `String`-only method is a `String` temporary | partial | string_literal_type_default_and_str covers `push_str` on a binding only; the temporary receiver has no cell |
| 405 | The line breaks after the opening and before the closing `"""` are not part of the value | covered | triple_quoted_dedent |
| 406 | The closing `"""`'s indentation is removed from every line | covered | triple_quoted_dedent |
| 407 | A non-blank line indented less than the closing `"""` is an error | covered | triple_quoted_line_under_indented |
| 418 | Raw: no escapes; `#` contains `"`; `r##` holds `"#` | covered | raw_strings |
| 418 | A raw string never dedents and has no triple-quoted form; no `rf` | covered | raw_string_spans_lines_no_dedent (new), prefix_combination_rf; tests/lexical_conformance_2026_11_16.rs lex_c_char_and_raw_triple_quote_are_focused_errors |

## Interpolated strings and format specifiers

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 436 | Each hole holds any expression, formatted with its `Display` | covered | fstring_braces_and_holes, fstring_user_display (new) |
| 436 | Concatenated in one allocation; no user-visible concatenation function | not testable | |
| 437 | Holes are evaluated left to right | covered | fstring_holes_left_to_right (new) |
| 438 | The parser balances quotes, brackets and braces in a hole (`{map["key"]}`, `{arr[idx]}`, `{(a, b)}`) | covered | fstring_hole_map_string_key (new), fstring_braces_and_holes |
| 439 | No nesting: an f-string in a hole is an error (0.8.x: a deprecation warning, and `karac fix` writes the inner text in the hole) | covered | fstring_nested_still_runs_in_0_8, fstring_nested_deprecation_fix (new, .notes + .fixed); tests/core_naming_lexical.rs a_nested_f_string_is_deprecated |
| 440 | `{{` / `}}` are literal braces; in a plain string a brace is ordinary | covered | fstring_braces_and_holes, fstring_triple_quoted (new) |
| 441 | `f"""..."""` follows the `"""` rules | covered | fstring_triple_quoted (new) |
| 453 | `align` `<` `>` `^` within `width`; `fill` needs an explicit align; a bare `0` is the zero-pad flag | covered | fstring_spec_align_fill_defaults (new), fstring_spec_fill_without_align_refused (new) |
| 453 | Width without align: numbers right-aligned, every other value left-aligned | covered | fstring_spec_align_fill_defaults (new); tests/format_spec_radix_align.rs interp_default_alignment_by_kind, compiled_default_alignment_by_kind |
| 454 | `+` signs non-negative numbers; `0` pads after the sign | covered | fstring_spec_doc_examples, fstring_spec_align_fill_defaults (new) |
| 455 | `.precision` sets the digits after the point | covered | fstring_spec_doc_examples, fstring_spec_align_fill_defaults (new) |
| 456 | `width` and `precision` are at most 65535; larger is an error at the hole | covered | fstring_spec_width_max_ok, fstring_spec_width_past_max_refused (new); tests/format_spec_bounds.rs format_width_and_precision_over_65535_are_refused |
| 457 | `#` is refused, naming the prefix to write | covered | fstring_spec_alternate_form_refused (new); tests/format_spec_bounds.rs format_alternate_form_is_named_with_the_prefix_to_write |
| 458 | `x X o b d`; a negative prints its two's complement at its own width (`-1i8` is `ff`, `-2i32` is `fffffffe`) | covered | fstring_spec_radix_types (new); tests/format_spec_radix_align.rs |
| 458 | `+` with `x X o b` is an error; `+` with `d` is accepted | covered | fstring_spec_plus_with_hex_refused, fstring_spec_radix_types (new) |
| 459 | An unrecognized specifier is an error at the hole | covered | fstring_spec_unrecognized_refused (new) |
| 460 | `fill`, `align`, `width` apply to every value (`bool`, `char`, `f64`) | covered | fstring_spec_align_fill_defaults (new); tests/format_spec_parts_by_type.rs |
| 460 | `+` / `0` apply to numbers, `type` to integers, `.precision` to floats; a part that does not apply is an error | covered | fstring_spec_zero_on_string_refused, fstring_spec_type_on_float_refused, fstring_spec_precision_on_int_refused (new); tests/format_spec_parts_by_type.rs a_part_that_does_not_apply_is_an_error_at_the_hole |
| 460 | `.precision` on a string is refused | covered | fstring_spec_string_precision_refused (new) |
| 460 | A float without precision prints its `Display` (`{x:+}` is `+1.5`, `{nan:>5}` is `  NaN`) | covered | fstring_spec_align_fill_defaults, fstring_spec_nonfinite (new) |
| 460 | NaN never takes a sign, infinity does; `0` pads both; `.precision` changes neither | covered | fstring_spec_nonfinite (new) |
| 462 | The six examples (`{pi:.2}`, `{n:>5}`, `{n:05}`, `{n:x}`, `{n:+}`, `{name:*^9}`) | covered | fstring_spec_doc_examples (new) |
| 474 | `{x:?}` and `{x:#?}` are reserved for `Debug`; the compiler says they are not implemented | covered | fstring_debug_spec_not_implemented, fstring_debug_pretty_spec_not_implemented (new) |

## Byte and byte-string literals

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 481 | The examples (`b'U'`, `b"\x08\x00"` as `Array[u8, 2]`, a 13-byte banner) | covered | byte_literals |
| 487 | `b'A'` has type `u8` | covered | byte_literals |
| 488 | `b"..."` is `Array[u8, N]`, N the byte count after escapes | covered | byte_literals |
| 488 | It passes as `Slice[u8]` wherever an `Array[u8, N]` does | covered | byte_string_as_slice (new) |
| 491 | Permitted escapes `\n \t \r \0 \\ \' \" \xHH`; `b'\xFF'`, `b"\xDE\xAD\xBE\xEF"` | covered | byte_literals, byte_string_as_slice (new) |
| 492 | `\u{...}` is forbidden | covered | byte_char_unicode_escape, byte_string_unicode_escape |
| 492 | The error says `Unicode escapes are not permitted in byte literals; use \xFF for byte 0xFF.` | covered (fixed, B-2026-11-25-5) | byte_unicode_escape_full_message (new): karac writes `… byte literals — use \xFF …` (an em dash for the `;`) |
| 494 | A byte string where `String` or `Str` is expected is a type error | covered | byte_string_is_not_text, byte_string_not_str_refused (new) |
| 494 | `String.from_utf8(bytes)` converts, returning `Result[String, Utf8Error]` | covered (fixed, B-2026-11-25-6) | byte_string_from_utf8 (new): build prints `hi` / `bad`; interp stops with `expected a sequence, found Agg([Int(104), Int(105)])` |

## C-string literals

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 498 | `c"..."` passes a constant string to C without a copy | covered | c_string_puts_example (new) |
| 500 | Type `ref CStr`, points into read-only data, no allocation | covered | c_string_literal_len, c_string_puts_example (new) |
| 500 | It is `Copy` | covered | c_string_is_copy_and_source_utf8 (new) |
| 501 | N source bytes occupy N + 1; the compiler adds the NUL | covered (fixed, B-2026-11-25-7) | c_string_nul_terminated (new): build reads the NUL at `p.add(2)` of `c"hi"` (prints 104, 0); interp stops with `index 2 out of bounds for length 2` |
| 501 | `c"hello".len()` is 5 | covered | c_string_literal_len |
| 502 | Byte-string escapes plus `\u{...}` as UTF-8; `c"café"` is five bytes and a NUL | covered | c_string_literal_len (`\u{e9}`), c_string_is_copy_and_source_utf8 (new: literal `café`) |
| 503 | An interior NUL (`\0`, `\x00`, `\u{0}`) is E_INTERIOR_NUL_IN_C_STRING | covered | c_string_interior_nul, c_string_interior_hex_nul, c_string_interior_unicode_nul |
| 503 | Reported at the escape | partial | tests/lexer.rs test_c_string_literal_interior_nul_*_rejected (span not pinned by a cell) |
| 504 | No `c'...'`; write `b'A'` | covered | c_char_literal_is_not_a_form |
| 505 | No implicit conversion to `String` or `Str` | covered | c_string_not_string_refused, c_string_not_str_refused (new), control c_string_is_copy_and_source_utf8 (`to_str()`) |
| 518 | The pointer type is `*const u8` | covered | c_string_puts_example, c_string_nul_terminated (new; the build lane) |

## Comments and doc comments

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 522 | `//` runs to the end of the line | covered | comments_nest_and_doc |
| 523 | `/* */` block comments nest | covered | comments_nest_and_doc; tests/lexical_conformance_2026_11_16.rs lex_unterminated_block_comment_is_one_error_at_its_opening |
| 524 | `///` and `//!` are doc comments | covered | comments_nest_and_doc, doc_comments_on_every_item_and_member (new) |
| 528 | The compiler keeps doc comments on the tree; `karac doc` renders them | covered | tests/doc.rs doc_emits_html_for_documented_function |
| 535 | Both are line comments; there is no block form | covered | doc_comment_no_block_form (new: `/** */` is a plain comment, no doc warning) |
| 535 | Consecutive lines join with newlines, one leading space stripped | covered | tests/doc.rs doc_renders_markdown_in_doc_comment; tests/lexer.rs test_doc_comments |
| 535 | A plain `//` comment or a blank line ends the run | covered | doc_comment_ended_by_plain_comment_warns, doc_comment_detached_by_blank_line_warns (new) |
| 544 | A `///` run attaches to the item directly after it; otherwise it is discarded with a warning | covered | doc_comment_detached_by_blank_line_warns (new); tests/core_naming_lexical.rs unused_doc_comments_warn_and_the_program_runs |
| 545 | Items: fn, struct, enum, trait, const, type alias, distinct type, foreign function | covered | doc_comments_on_every_item_and_member (new, no warning) |
| 545 | An `impl` block takes no doc comment | covered | doc_comment_on_impl_block_warns (new) |
| 546 | Members: fields, variants, struct-like variant fields, parameters, trait methods and their parameters | covered | doc_comments_on_every_item_and_member (new); tests/doc.rs doc_renders_per_field_doc_comments, doc_renders_per_param_doc_comments |
| 547 | `//!` at the top of a file; anywhere else a parse error | covered | inner_doc_comment_after_item, doc_comment_inner_inside_body_refused (new), control comments_nest_and_doc |
| 549 | A `///` before `self` is ignored | covered | tests/doc_comment_before_self.rs doc_comment_before_self_is_ignored_and_the_next_param_keeps_its_own |
| 551 | CommonMark with tables, footnotes, task lists, strikethrough, fenced code | not drafted | `karac doc` needs a project; no test pins the extensions (tests/doc.rs doc_renders_markdown_in_doc_comment covers base Markdown) |
| 552 | A reference-style link to a documented item links to its page; unresolved renders as text | covered | tests/doc.rs doc_resolves_cross_reference_to_known_item, doc_unresolved_cross_reference_renders_as_plain_text |
| 552 | An ambiguous name resolves to the first match in module order | not drafted | no test; needs a project |
| 553 | Item pages: signature as written, doc text, parameter section, field / variant sections, methods and trait impls; inferred effects of a `pub fn` | covered | tests/doc_conformance_2026_11_22.rs doc_conf_callable_types_render_as_written, doc_conf_ffi_signatures_render_as_written, doc_conf_inferred_effects_are_written_side_by_side, doc_conf_doc_text_precedes_sections; tests/doc_signatures.rs |
| 554 | A library shows only `pub` items and fields; an executable shows every documented item | covered | tests/doc_conformance_2026_11_22.rs doc_conf_library_shows_only_its_public_surface, doc_conf_executable_keeps_every_documented_item |
| 555 | A `pub import` lists the item under the re-exporting module, by its alias | partial | tests/doc_conformance_2026_11_22.rs doc_conf_reexport_shows_at_the_shorter_path (the alias form not pinned) |
| 556 | The root's `//!` renders above the index; other modules' before their items | covered | tests/doc.rs doc_renders_module_level_doc_comments, doc_per_module_index_includes_module_doc |
| 558 | No path-qualified cross-references; private inferred effects not shown; doc code blocks not run as tests | not drafted | needs a project and `karac test`; doc_conf_inferred_effects_are_written_side_by_side covers the `pub fn` side only |

## Findings

| Cell | Line | Phase | Where | Row |
|---|---|---|---|---|
| raw_ident_true_false_literals_still_literals | 309, 319 | parser | `recover_keyword_binding_use`, src/parser.rs:3311-3316: any `r#true` token anywhere makes a bare `true` before `,` `;` `)` an error, in the prelude too (reported under the user's file at prelude lines) | B-2026-11-25-3, fixed |
| keyword_assoc_fn_called_bare | 283 | typechecker (surfaced by MIR lowering as E0516) | `Type.type()` / `Type.match(x)`: a keyword-spelled associated-function call on a type checks to an error type with no typechecker diagnostic; `Type.r#type()` works | B-2026-11-25-9, open |
| primitive_name_as_fn_refused, primitive_name_as_field_refused | 224 | resolver | `reject_primitive_bindings`, src/resolver.rs:~1781 checks `SymbolKind::Variable` only | B-2026-11-25-10, open |
| mutfn_type_name_reserved_enum, oncefn_type_name_reserved_trait | 224 | resolver | `RESERVED_IDENTIFIERS`, src/resolver.rs:398 lists `Fn` only | B-2026-11-25-10, open |
| cn7_one_letter_lowercase_const | 210 | parser (diagnostic) | src/parser.rs:4718-4722 (and src/resolver/collect.rs:1107) predate the CN-7 ruling of 2026-11-19 | B-2026-11-25-4, fixed |
| byte_unicode_escape_full_message | 492 | lexer (diagnostic) | src/lexer.rs:1417, 1526 write `—` where the page quotes `;` | B-2026-11-25-5, fixed |
| byte_string_from_utf8 | 494 | interp (run-vs-build) | `view_of`, src/mir/interp.rs:10445-10446 accepts an `Agg` only behind a reference | B-2026-11-25-6, fixed |
| c_string_nul_terminated | 501 | interp (run-vs-build) | the interpreter's `c"..."` buffer has no trailing NUL | B-2026-11-25-7, fixed |
