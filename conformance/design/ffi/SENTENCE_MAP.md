# design.md § 15 Unsafe, FFI and layout control: sentence-to-cell map

Status: covered, partial, FAILS (a cell fails; the row named), not testable, not drafted. Cell names without a prefix are in `conformance/design/ffi/`; `effects/`, `types/`, `traits/` and `lexical/` name other directories under `conformance/design/`. `tests/x.rs::name` is a Rust test, `corpus/...` a corpus entry, `layout.py` is `conformance/layout.py` (random `#[repr(C)]` structs against GCC through `[link]`). Line numbers are docs/design.md lines (§ 15 is 6209 to 6923).

**Totals (2026-11-25, after the gap audit and its fixes):** 219 normative sentences, 179 covered, 9 partial, 7 FAILS (filed as B-2026-11-25-11 to -17; -8 found and fixed by the audit; -13 fixed; -14 closed by correcting the page; -12 closed by the review ruling that no enum is a transparent carrier, which removed the two enum-carrier rows at 6628), 20 not testable (deferred, unimplemented by the page's own word, stdlib-internal, undefined behaviour or policy), 4 not drafted (need a package manifest, a second module or a link-time coherence check, which `cases.py` cannot express). Before the audit no cell lived here; the section was pinned by Rust tests (tests/unsafe_lint.rs, tests/typechecker.rs, tests/repr_and_extern_declarations.rs, tests/s15_conformance_pass.rs, tests/probe_s15_*.rs, tests/ffi_c_abi_layout_2026_11_22.rs), `layout.py`, eleven `corpus/design` entries and seven cells in other directories. `(new)` marks a cell the audit added; every new refusal has an accepted control beside it where one exists. Run with `CONF_LANES=interp,build,asan`: the raw-memory cells are ASan-clean.

## Who may use it

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6214 | `std.intrinsics` and `#[compiler_builtin]` are restricted to the standard library | covered | std_intrinsics_import_refused, compiler_builtin_outside_std_refused (new) |
| 6215 | `unsafe` blocks, `unsafe fn`, raw pointers and the `ptr` module are available to every package | covered | every run cell here is a user program (unsafe_fn_examples_run, ptr_module_constructors, ...) (new) |
| 6215 | `MaybeUninit[T]` is available to every package | not testable | 6439: not yet implemented |
| 6218 | Layout blocks, `repr(packed)`, `repr(align(N))`, `offset_of`, FFI unions, C export, volatile, asm, interrupt handlers, linker attributes, other conventions, codegen hints are deferred | not testable: deferred | (tests/probe_s15_unsafe_ffi_layout.rs R2 refuses packed/align; tests/ffi_c_abi_layout_2026_11_22.rs::ffi_abi_union_use_names_the_deferral) |

## `unsafe`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6224 | `unsafe { ... }` is an expression, for raw pointer work | covered | unsafe_block_offset_and_deref (new), corpus/design/ok_unsafe_block_takes_the_expected_type |
| 6226 | `unsafe extern` is the trust boundary for foreign imports | covered | extern_call_needs_no_unsafe (new) |
| 6228 | `unsafe fn` needs a `# Safety` section; an `unsafe extern` block needs one at block level | covered | unsafe_fn_without_safety_section_warns, unsafe_extern_block_without_safety_warns, controls unsafe_fn_with_safety_section_quiet, unsafe_extern_block_with_safety_quiet (new) |
| 6230 | The example: safe `ptr.const`, then `p.offset(4)` and `*raw` in one block | covered | unsafe_block_offset_and_deref (new) |
| 6240 | Pointer construction (`ptr.const` / `ptr.mut`) is safe | covered | ptr_mut_is_the_place_itself, unsafe_block_offset_and_deref (new) |
| 6240 | Dereference requires `unsafe { }` | covered | deref_outside_unsafe_refused (new); tests/unsafe_lint.rs::test_raw_pointer_deref_outside_unsafe_errors |
| 6240 | Arithmetic requires `unsafe { }` | covered | offset_outside_unsafe_refused (new) |
| 6240 | Unaligned access requires `unsafe { }` | covered | read_unaligned_outside_unsafe_refused, control read_unaligned_in_unsafe (new) |
| 6240 | Pointee-changing casts require `unsafe { }` | covered | pointee_change_cast_outside_unsafe_refused (new) |
| 6240 | `*const T as *mut T` requires `unsafe { }` | covered | const_to_mut_cast_outside_unsafe_refused (new) |
| 6240 | `*mut T as *const T` stays safe | covered | ptr_module_constructors, provenance_tag_and_round_trip (new) |
| 6242 | `unsafe` does not switch off the effect system | covered | unsafe_does_not_switch_off_effects_refused (new) |

## `undocumented_unsafe` lint

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6246 | The compiler warns, by default, on an undocumented `unsafe` block | covered | undocumented_unsafe_block_warns (new) |
| 6248 | A preceding line comment whose content begins `Safety:`, case-insensitive, documents it | covered | safety_comment_case_insensitive_quiet (new); tests/unsafe_lint.rs::test_safety_comment_case_insensitive |
| 6248 | ... or a doc comment | covered | safety_doc_comment_documents_block (new) |
| 6248 | ... followed by at least one word | covered | safety_comment_without_a_word_warns (new); tests/s15_conformance_pass.rs::s15_a_safety_comment_needs_a_word_after_the_colon |
| 6255 | Each `unsafe` block needs its own comment | covered | each_unsafe_block_needs_its_own_comment (new) |
| 6257 | An `unsafe fn` requires a `# Safety` section | covered | unsafe_fn_without_safety_section_warns, unsafe_fn_with_safety_section_quiet (new); tests/unsafe_lint.rs::test_unsafe_fn_without_doc_warns |
| 6268 | An `unsafe extern` block requires `# Safety` attached to the block | covered | unsafe_extern_block_without_safety_warns, unsafe_extern_block_with_safety_quiet (new); tests/unsafe_lint.rs::test_unsafe_extern_block_without_doc_warns |
| 6284 | `#[allow(undocumented_unsafe)]` at the function level silences it | covered | allow_undocumented_unsafe_on_fn (new); tests/unsafe_lint.rs::test_allow_attribute_suppresses |
| 6284 | `#[allow(undocumented_unsafe)]` at the block level silences it | FAILS, B-2026-11-25-15 | allow_undocumented_unsafe_on_block (new): the attribute on the block statement parses and is ignored; src/unsafe_lint.rs:104 reads only the enclosing fn's attributes |
| 6284 | `#[allow(undocumented_unsafe)]` at the module level silences it | FAILS, B-2026-11-25-15 | allow_undocumented_unsafe_on_module (new): `#![allow(undocumented_unsafe)]` parses and is ignored (same place) |
| 6284 | `#[deny(undocumented_unsafe)]` makes it an error | covered | deny_undocumented_unsafe_is_an_error_refused (new) |
| 6284 | `-D warnings` makes it an error | covered | tests/unsafe_lint.rs::test_cli_deny_promotes_undocumented_unsafe |
| 6284 | The stdlib ships with the lint at warn and a `Safety:` comment on every block | not testable | stdlib-internal |

## `unsafe_op_in_unsafe_fn` rule

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6288 | An `unsafe fn` body is not an implicit `unsafe { }`; its unsafe operations need their own block | covered | unsafe_fn_body_bare_deref_refused (new); tests/unsafe_lint.rs::test_unsafe_fn_body_still_requires_inner_unsafe_block |
| 6290 | Callers acknowledge an `unsafe fn` at each call: `unsafe { foo() }` | covered | unsafe_fn_call_outside_unsafe_refused (new) |
| 6296 | `read_unchecked`: a safe `as_ptr`, one documented inner block | covered | unsafe_fn_examples_run (new) |
| 6308 | A bare unsafe operation in an `unsafe fn` body is a compile error, not a warning | covered | unsafe_fn_body_bare_deref_refused (new); tests/probe_s15_unsafe_ffi_layout.rs::an_unsafe_operation_outside_unsafe_fails_check |
| 6308 | There is no `#[allow(unsafe_op_in_unsafe_fn)]` | covered | unsafe_op_in_unsafe_fn_cannot_be_allowed_refused (new); corpus/diagnostics/e_lint_level_on_hard_rule |
| 6310 | Calling another `unsafe fn` is an unsafe operation, inside an `unsafe fn` too | covered | unsafe_fn_calling_unsafe_fn_needs_block_refused (new) |
| 6313 | `copy_unchecked`: one inner block around a loop of `offset` / `read` / `write` | covered | unsafe_fn_examples_run (new) |
| 6330 | Calling a function from an `unsafe extern` block is not an unsafe operation | covered | extern_call_needs_no_unsafe (new); tests/unsafe_lint.rs::test_extern_fn_call_does_not_trigger |

## Raw pointers

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6334 | Casting a reference to a raw pointer is forbidden | covered | ref_to_const_ptr_cast_refused, mut_ref_to_mut_ptr_cast_refused (new) |
| 6337 | `ptr.const`, `mut`, `null`, `null_mut`, `dangling`, `dangling_mut` live in `ptr` | covered | ptr_module_constructors, ptr_mut_is_the_place_itself (new) |
| 6339 | `log_byte`: the address of a local handed to C's `write`, no reference created | covered | log_byte_writes_through_extern (new; stderr `A`) |
| 6349 | `ptr.const(h.field)` on a borrowed parameter is the caller's field address | covered | ptr_const_of_a_borrowed_field (new) |
| 6356 | `ptr.const` / `ptr.mut` take a place (binding, field, index, dereference) and give its address; no temporary | covered | ptr_mut_is_the_place_itself, ptr_const_of_a_dereference_place (new) |
| 6357 | A value argument is `E_PTR_CONST_REQUIRES_PLACE` with the quoted message | partial | ptr_const_of_a_value_refused (new): the code matches; the text is "'ptr.const' requires a place expression — a binding, field access, index, or dereference; got a value-producing expression that has no stable address", not the page's "(a binding, field, or index); the argument here is a value expression" (the cell pins the shared phrase) |
| 6358 | `ptr.mut` needs a mutably-reachable place: `E_PTR_MUT_REQUIRES_MUTABLE_PLACE` | covered | ptr_mut_of_an_immutable_binding_refused, ptr_mut_through_a_shared_ref_refused (new); corpus/design/err_ptr_mut_of_an_immutable_binding |
| 6360 | `r as *const T` is `E_REF_TO_RAW_PTR_CAST_FORBIDDEN`, with the quoted message | covered | ref_to_const_ptr_cast_refused (new) |
| 6360 | The diagnostic carries a fix replacing the cast with `ptr.const(...)` | FAILS, B-2026-11-25-16 | ref_to_const_ptr_cast_refused.fixed (new): no replacement in `check --output=json`; `karac fix` reports no machine-applicable fix |
| 6360 | Only `ref T -> *const T` and `mut ref T -> *mut T` are forbidden outright | covered | mut_ref_to_mut_ptr_cast_refused (new) |
| 6364 | Strengthening and pointee-changing casts are `E_PTR_CAST_REQUIRES_UNSAFE` outside `unsafe` | covered | const_to_mut_cast_outside_unsafe_refused, pointee_change_cast_outside_unsafe_refused (new); corpus/diagnostics/e_ptr_cast_requires_unsafe |
| 6364 | Inside `unsafe { }` those casts are accepted (and the pointer reads as its new pointee) | FAILS, B-2026-11-25-11 | raw_pointer_casts_safe_and_unsafe (new): `*(c as *const u8)` over a `u32` reads the whole u32 (168496141, typed u8) on the interpreter, the low byte (13) when built and under ASan |
| 6365 | No implicit `*mut T -> *const T` coercion at a call or an assignment | covered | mut_ptr_to_const_param_no_coercion_refused, mut_ptr_to_const_let_no_coercion_refused (new) |
| 6365 | The `expected '*const T', found '*mut T'` mismatch has a machine-applicable ` as *const T` fix | covered | mut_ptr_to_const_param_no_coercion_refused (.fixed) (new) |
| 6368 | `*p`, `p.read()`, `p.write(v)` need `unsafe { }` | covered | deref_outside_unsafe_refused, read_method_outside_unsafe_refused, write_method_outside_unsafe_refused (new) |
| 6370 | `p.offset(n)` needs `unsafe`; `n` is an `i64` count of elements | covered | offset_outside_unsafe_refused, unsafe_block_offset_and_deref (offset 4 over `i64` is element 4), unsafe_fn_examples_run (new) |
| 6371 | `p.read_unaligned()` needs `unsafe` | covered | read_unaligned_outside_unsafe_refused, read_unaligned_in_unsafe (new) |
| 6374 | Construction carries no per-iteration `unsafe` ceremony | covered | unsafe_fn_examples_run (`ptr.const(src[0])` outside any block) (new) |
| 6382 | `ptr.null()` / `ptr.null_mut()` are the null pointers | covered | ptr_module_constructors (new) |
| 6384 | `ptr.dangling()` / `dangling_mut()` are non-null and aligned to `T.align_of()` | covered | ptr_module_constructors (new) |
| 6386 | `is_null`, `addr`, `with_addr`, `with_addr_mut`, `expose`, `expose_mut` are safe | covered | ptr_module_constructors, provenance_tag_and_round_trip (new); tests/unsafe_lint.rs::test_ptr_addr_safe_silent, test_ptr_with_addr_safe_silent, test_ptr_expose_safe_silent |
| 6392 | `from_exposed` / `from_exposed_mut` are unsafe | covered | from_exposed_outside_unsafe_refused, from_exposed_mut_outside_unsafe_refused (new) |
| 6395 | The generic constructors take `T` from the expected type | covered | ptr_module_constructors (new) |
| 6395 | No expected type: no `T` (no call-site type arguments) | partial | ptr_null_needs_an_expected_type_refused (new): refused, but as E0516 "an expression whose type does not lower (Type(UnknownParam("T"))) is not lowered yet", an internal lowering message rather than an inference error naming the annotation |
| 6397 | `ptr.mut(xs[i])` moves nothing; `read` copies the bits out; `write` stores without dropping, non-`Copy` too | covered | ptr_read_write_non_copy_without_drop (new, ASan clean); corpus/design/ok_raw_pointer_moves_a_non_copy_element_without_a_drop |

## Pointer provenance

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6410 | Tagging: `with_addr_mut(p, addr(p) \| 1)` and `& ~1` keep `p`'s provenance and place | covered | provenance_tag_and_round_trip (new); corpus/design/ok_ptr_addr_round_trips_a_tagged_pointer |
| 6416 | Round trip: `expose`, then `from_exposed` inside `unsafe` | covered | provenance_tag_and_round_trip (new) |
| 6423 | `addr` and `expose` return the same bits | covered | provenance_tag_and_round_trip (new) |
| 6423 | `addr` then `from_exposed` is undefined behaviour | not testable | UB, no observable promise |
| 6425 | `with_addr` is safe; `from_exposed` is unsafe | covered | provenance_tag_and_round_trip, from_exposed_outside_unsafe_refused (new) |
| 6428 | Pointer to integer (both pointer kinds) is `E_PTR_TO_INT_CAST_FORBIDDEN`, quoted message | covered | const_ptr_to_usize_cast_refused, mut_ptr_to_usize_cast_refused (new) |
| 6428 | The fix offers `ptr.addr(p)` | FAILS, B-2026-11-25-16 | const_ptr_to_usize_cast_refused.fixed (new): the message names `ptr.addr(p)`; no machine-applicable fix |
| 6429 | Integer to pointer (both kinds) is `E_INT_TO_PTR_CAST_FORBIDDEN`, quoted message | covered | usize_to_const_ptr_cast_refused, usize_to_mut_ptr_cast_refused (new) |
| 6429 | There is no automatic fix | covered | usize_to_const_ptr_cast_refused.fixed (`karac fix` leaves it unchanged) (new) |
| 6431 | Integer-integer and raw-raw casts are unaffected | covered | int_and_raw_casts_unaffected (new) |
| 6433 | The `from_exposed` precondition | not testable | a caller obligation |

## Uninitialised memory: `MaybeUninit[T]`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6437 | `MaybeUninit[T]` and its `assume_init*` surface | not testable | 6439: not yet implemented |
| 6459 | No `mem.uninitialized()` under any name; `import std.mem.uninitialized` is an unresolved-name error | partial | mem_uninitialized_import_unresolved_refused, mem_uninitialized_call_unresolved_refused (new): both refused, but the import reads "unknown module `std.mem`, did you mean `std.web`?" and the call "undefined name 'mem'", though `import std.mem;` resolves (mem_zeroed_pod_types) |
| 6461 | `mem.zeroed()` is bounded by `T: Pod` | covered | mem_zeroed_string_refused (new); traits/std_pod_marker_nameable |
| 6461 | `Pod` covers integer and float primitives, `bool` (zero is false), tuples and arrays of `Pod` | covered | mem_zeroed_pod_types (new); tests/probe_s15_ffi_glibc.rs::ff4_mem_zeroed_makes_the_all_zeros_pod_value |
| 6461 | `char` is not in the list | covered | mem_zeroed_char_refused (new) |
| 6461 | A struct is `Pod` only by `#[derive(Pod)]` or an empty impl, never automatically | covered | mem_zeroed_pod_types, mem_zeroed_struct_without_opt_in_refused (new) |
| 6461 | The compiler checks each field | covered | pod_impl_checks_each_field_refused, pod_derive_checks_each_field_refused (new) |
| 6461 | Not `Pod`: references, fn pointers, `NonZero`, enums with no zero variant, `Option[ref T]` | partial | tests/probe_s15_ffi_glibc.rs::ff4_a_type_whose_zero_bits_are_not_a_value_is_not_pod; each kind not pinned by a cell |
| 6461 | `mem.zeroed()` returns `T` in safe code, `T` from the expected type | covered | mem_zeroed_pod_types (new) |
| 6465 | `let s: String = mem.zeroed();` is an error | covered | mem_zeroed_string_refused (new) |
| 6470 | Stdlib hygiene (`Vec.with_capacity`, `uninit_array`, `mem.replace`) | not testable | stdlib-internal; MaybeUninit not implemented |

## Intrinsics

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6474 | The intrinsic set | not testable | stdlib-internal |
| 6483 | Only standard-library packages may import `std.intrinsics` | covered | std_intrinsics_import_refused (new; refused as "unknown module") |
| 6484 | Raw-memory intrinsics are `unsafe fn` | not testable | unreachable from user code (6483) |
| 6485 | `#[compiler_builtin]` is rejected outside the standard library | covered | compiler_builtin_outside_std_refused (new) |
| 6487 | `T.size_of()` / `T.align_of()` are safe, callable by any package, return `i64` | covered | size_of_align_of_are_safe_i64, size_of_and_align_of_have_no_effects (new) |

## `#[repr]`: ABI layout

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6495 | `#[repr(C)]`: field order preserved, C padding | covered | repr_c_layout (new); layout.py; tests/s15_conformance_pass.rs::s15_repr_c_structs_are_laid_out_as_the_c_compiler_lays_them_out |
| 6496 | `#[repr(u8)]`, `(i32)` and the other integers fix the discriminant representation | covered | repr_int_fixes_enum_size (new); tests/s15_conformance_pass.rs::s15_repr_enums_cross_to_c_by_value_and_inside_structs |
| 6499 | `#[repr(packed)]` and `#[repr(align(N))]` are deferred | not testable: deferred | (tests/probe_s15_unsafe_ffi_layout.rs R2) |
| 6517 | `#[repr(transparent)] distinct type Fd = i32` has `i32`'s layout | covered | types/size_of_primitives_a_repr_transparent_distinct_type_option_dyn_trait_i, transparent_carriers_have_the_field_layout (Fd row) |
| 6521 | `#[repr(intN)]` is required on an enum passed across FFI | covered | tests/repr_and_extern_declarations.rs::extern_declaration_refuses_a_type_that_does_not_cross ("has no `#[repr]`"); extern_repr_enum_crosses_as_its_integer (new) |

## Enum discriminant runtime surface

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6527 | Every C-like enum has `discriminant()` returning the repr type: `u8`, `i32` for `(i32)` and `(C)`, `u32` without `#[repr]` | covered | discriminant_type_follows_repr (new); types/c_like_enum_discriminant_and_tryfrom_on_a_repr_enum; corpus/design/ok_repr_c_discriminant_reads_as_c_int |
| 6527 | `#[repr(C)] enum E { A = -1 }` gives -1 | covered | types/c_like_enum_discriminant_and_tryfrom_on_a_repr_enum, discriminant_type_follows_repr (new) |
| 6527 | The method is a read with no effect | covered | discriminant_is_a_read_with_no_effect (new) |
| 6529 | The `UsbClass` example (`c.discriminant()` is 3) | covered | usb_class_discriminant_and_values (new) |
| 6538 | Payload enums have no `discriminant()` | covered | payload_enum_has_no_discriminant_refused (new) |
| 6540 | `#[repr(intN)]` C-like enums get `TryFrom[intN]`, `Err(DiscriminantError.OutOfRange { value })` | covered | try_from_out_of_range_names_the_value (new); types/c_like_enum_discriminant_and_tryfrom_on_a_repr_enum |
| 6540 | No `TryFrom` without `#[repr]` | covered | no_repr_enum_has_no_try_from_refused (new) |
| 6540 | No `From[intN]` | covered | repr_enum_has_no_from_int_refused (new) |
| 6550 | `DiscriminantError.OutOfRange { value: D }` | covered | try_from_out_of_range_names_the_value (new) |
| 6552 | `values()` returns every variant in declaration order | covered | usb_class_discriminant_and_values (new); tests/probe_s15_unsafe_ffi_layout.rs R1 |
| 6552 | The slice is static data, borrows nothing, may go anywhere | covered | usb_class_discriminant_and_values (returned from `fn all()`) (new) |
| 6552 | Payload enums have no `values()` | covered | payload_enum_has_no_values_refused (new) |
| 6555 | The `for c in UsbClass.values()` listing prints `1: Audio` ... | covered | usb_class_discriminant_and_values (new) |
| 6560 | Semver lock rules | not testable | API policy |
| 6563 | `karac explain` on `.discriminant()` of a no-`#[repr]` enum says it is a local property, not a semver contract | FAILS, B-2026-11-25-17 | explain_discriminant_without_repr_says_local (new, `.cmd`): `karac explain main.kara.code` prints only "Effects: none / Declared effects: no `with` clause" |

## Explicit discriminants on payload variants

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6572 | Under `#[repr(intN)]` or `#[repr(C)]` any variant may carry `= INT_CONST` | covered | payload_discriminants_op, payload_discriminants_constant_expressions (new) |
| 6584 | The value is a constant expression: literals, arithmetic, integer `const`s | covered | payload_discriminants_constant_expressions (new); tests/typechecker.rs::discriminant_const_reference_participates_in_checks |
| 6585 | Negative values are legal under a signed repr | covered | payload_discriminants_constant_expressions, control payload_discriminant_negative_under_unsigned_refused (new) |
| 6586 | Out of range: `E_DISCRIMINANT_OUT_OF_RANGE`, naming the repr type and the value | covered | payload_discriminant_out_of_range_refused, payload_discriminant_negative_under_unsigned_refused (new) |
| 6587 | Duplicate: `E_DUPLICATE_DISCRIMINANT`, naming both variants | covered | payload_discriminant_duplicate_refused (new) |
| 6589 | All or nothing: `E_PARTIAL_EXPLICIT_DISCRIMINANTS` with the quoted message | covered | partial_explicit_discriminants_refused, partial_explicit_discriminants_fieldless_refused (new) |
| 6591 | Payload discriminants without `#[repr]` or under `transparent`: `E_PAYLOAD_DISCRIMINANT_REQUIRES_REPR`, quoted message | covered | payload_discriminant_without_repr_refused, payload_discriminant_under_transparent_refused (new) |
| 6591 | Field-less enums without `#[repr]` may write `Audio = 0x01` | covered | fieldless_discriminants_without_repr_ok (new) |
| 6593 | No runtime surface on payload enums | covered | payload_enum_has_no_discriminant_refused, payload_enum_has_no_try_from_refused, payload_enum_has_no_values_refused (new) |
| 6593 | The value declares the wire value only; no reader or writer is emitted | not testable | an absence |
| 6596 | The `tag` method example | covered | payload_discriminants_op (new) |
| 6608 | The compiler does not check `tag`'s literals against the declarations | covered | tag_literals_are_not_checked (new) |
| 6610 | Semver lock | not testable | API policy |

## `#[repr(transparent)]`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6616 | A transparent wrapper has exactly its field's ABI shape: arguments, results, fields, behind pointers, slots | partial | transparent_carriers_have_the_field_layout, transparent_wrapper_crosses_raw_or_as_itself (new); tests/probe_s15_ffi_glibc.rs::ff3_a_transparent_wrapper_crosses_as_its_field; results and struct-field positions not pinned |
| 6626 | Carrier: `distinct type Foo = Inner` | covered | transparent_carriers_have_the_field_layout (new) |
| 6627 | Carrier: a single-field struct, with zero-sized companions | covered | transparent_carriers_have_the_field_layout (new) |
| 6631 | Two non-zero-sized fields: `E_REPR_TRANSPARENT_REQUIRES_SINGLE_FIELD` | covered | transparent_two_data_fields_refused (new); tests/typechecker.rs::repr_transparent_multi_field_struct_rejected |
| 6632 | No fields: `E_REPR_TRANSPARENT_REQUIRES_FIELD` | covered | transparent_no_fields_refused (new) |
| 6633 | Any enum, a single-variant one-field enum included: `E_REPR_TRANSPARENT_ON_ENUM` | covered | transparent_single_variant_enum_refused, transparent_two_variants_refused, transparent_payloadless_variant_refused; tests/typechecker.rs::repr_transparent_single_variant_enum_rejected (B-2026-11-25-12) |
| 6634 | `transparent` with any other repr: `E_REPR_TRANSPARENT_EXCLUSIVE` | covered | transparent_with_another_repr_refused, transparent_with_int_repr_refused (new) |
| 6636 | Neighbours of size zero and alignment one: `()`, a unit struct, `Array[u8, 0]`, a `PhantomData[T]`-style unit struct | covered | transparent_carriers_have_the_field_layout, transparent_generic_unit_neighbour (new); tests/repr_and_extern_declarations.rs::transparent_keeps_alignment_one_neighbours (a library `PhantomData` does not exist: E0104; design.md §5 shows it user-declared) |
| 6636 | `Array[u64, 0]` is 8-aligned and refused | covered | transparent_over_aligned_zero_sized_neighbour_refused (new); tests/repr_and_extern_declarations.rs::transparent_refuses_an_over_aligned_zero_sized_neighbour |
| 6636 | Per field: two non-zero-sized fields are refused even if their sizes add up to one | covered | transparent_two_data_fields_refused (new) |
| 6639 | The `Reading { raw: i32, _unit: Celsius }` example is 4 bytes | covered | transparent_carriers_have_the_field_layout (new) |
| 6652 | `close(fd.raw())`: `.raw()` is the explicit unwrap at the boundary | covered | transparent_wrapper_crosses_raw_or_as_itself (new) |
| 6664 | A wrapper passed to an inner-typed foreign parameter needs `.raw()` | covered | transparent_wrapper_needs_raw_at_the_boundary_refused (new) |
| 6664 | Or declare the foreign signature in terms of the wrapper | covered | transparent_wrapper_crosses_raw_or_as_itself (new) |
| 6666 | The inner repr passes through; a transparent `Array[u8, 16]` is `uint8_t[16]` | covered | transparent_inner_repr_passes_through (new) |
| 6668 | The orphan-rule escape needs only a local type, not `transparent` | covered | traits/distinct_type_newtype |
| 6677 | Field-walking derives work on a transparent carrier; `Display` prints `TypeName { field: value }` | covered | transparent_derives_follow_the_carrier (new) |

## `extern "C"`

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6681 | Foreign imports live in `unsafe extern "C" { }`, each declaring its effects | covered | extern_scalars_cross, single_import_block_form (new) |
| 6690 | A Kāra panic in an exported body aborts at the boundary | not testable: deferred | C export is deferred (6690) |
| 6690 | Each `"C-unwind"` import or export is refused, E0415 | covered | c_unwind_import_refused (new); tests/effectchecker.rs::extern_c_unwind_import_rejected_under_abort_default, extern_c_unwind_export_rejected_under_abort_default |
| 6690 | ... with `"C"` as the fix | FAILS, B-2026-11-25-16 | c_unwind_import_refused.fixed (new): the message says "Use `extern "C"` instead"; no machine-applicable fix |
| 6690 | `"stdcall"`, `"fastcall"`, `"win64"`, `"sysv64"` refused where written, as reserved | covered | abi_stdcall_reserved_refused, abi_fastcall_reserved_refused, abi_win64_reserved_refused, abi_sysv64_reserved_refused (new) |
| 6690 | Any other ABI string refused as unsupported, never read as `"C"` | covered | abi_unknown_unsupported_refused (new); tests/repr_and_extern_declarations.rs::extern_block_refuses_an_abi_other_than_c |
| 6692 | `usize` / `isize` exist for FFI signatures | covered | extern_usize_signature_and_c_literal (new) |
| 6694 | Integers, `bool`, `char`, `f32`, `f64`, raw pointers cross | covered | extern_scalars_cross, extern_usize_signature_and_c_literal (new); layout.py (14 scalar identity round trips) |
| 6694 | A C-like repr enum crosses as its C integer holding the declared discriminant | covered | extern_repr_enum_crosses_as_its_integer (new); tests/s15_conformance_pass.rs::s15_repr_enums_cross_to_c_by_value_and_inside_structs |
| 6694 | A `#[repr(C)]` struct of these and fixed arrays crosses | covered | layout.py; tests/s15_conformance_pass.rs; tests/probe_s15_ffi_glibc.rs::ff3_structs_cross_by_the_platform_abi |
| 6694 | A transparent struct or a distinct type crosses as its field | covered | transparent_wrapper_crosses_raw_or_as_itself (new); tests/probe_s15_ffi_glibc.rs::ff3_a_transparent_wrapper_crosses_as_its_field |
| 6694 | Nothing else crosses (String, Vec, tuple, payload or plain enum, struct without `#[repr(C)]`) | covered | tests/repr_and_extern_declarations.rs::extern_declaration_refuses_a_type_that_does_not_cross, extern_declaration_refuses_a_struct_without_repr_c |
| 6694 | A struct crosses by the platform ABI (16-aligned included); `i128` as `__int128` | covered | layout.py; tests/ffi_c_abi_layout_2026_11_22.rs::ffi_abi_i128_and_sixteen_byte_aligned_structs |
| 6694 | `char` crosses as `uint32_t` | covered | tests/s15_conformance_pass.rs::s15_char_crosses_as_char32_and_an_opaque_ref_gives_its_address |
| 6694 | A C `char` outside the scalar range: the interpreter reports it | covered | manual probe (`abs` declared `-> char`, 0xD800): interp "C handed back 0xd800 as a `char`, which is no Unicode scalar value"; no cell, since the page makes the compiled lane a broken contract and the two lanes differ by design |

## `unsafe extern` blocks and effects of `extern` functions

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6698 | Only foreign fn declarations and opaque types are legal inside | covered | extern_block_struct_item_refused, extern_fn_with_body_refused (new) |
| 6698 | `pure fn name(...) -> T;` is the other fn form | covered | effects/extern_pure_fn_runs_on_every_lane, extern_pure_has_no_effects_static (new) |
| 6704 | Every foreign import is inside an `unsafe extern` block | covered | extern_block_without_unsafe_refused (new) |
| 6706 | Calls need no `unsafe { }`; declared effects flow into the caller | covered | extern_call_needs_no_unsafe, extern_declared_effects_are_trusted (new) |
| 6708 | `extern "C" fn name(...);` on its own is a parse error | covered | standalone_extern_fn_parse_error_refused (new) |
| 6710 | One import uses the block form with one item | covered | single_import_block_form (new) |
| 6721 | An `extern` fn's effects are exactly its `with` clause | covered | effects/extern_fn_with_a_clause_has_exactly_it, extern_declared_effects_are_trusted (new); tests/probe_s15_unsafe_ffi_layout.rs::an_extern_fn_has_what_it_declares |
| 6722 | `pure` has no effects | covered | effects/extern_pure_fn_has_no_effects, extern_pure_has_no_effects_static (new); tests/pure_extern.rs::pure_extern_has_no_effects |
| 6723 | Neither: every effect | covered | effects/extern_fn_with_neither_has_every_effect_blocks; tests/probe_s15_unsafe_ffi_layout.rs::an_extern_fn_with_no_clause_has_every_effect |
| 6731 | Only `extern` declarations take `pure` | covered | effects/pure_is_refused_outside_extern; tests/pure_extern.rs::pure_is_rejected_off_extern_and_with_a_with_clause |
| 6733 | A declared `reads(Network)` joins every caller's set, unverified | covered | extern_declared_effects_are_trusted (new) |
| 6744 | Declare `blocks`, `panics`, `allocates(Heap)` where they apply | not testable | author guidance |
| 6748 | The compiler cannot reject an under-declared `extern` | covered | extern_declared_effects_are_trusted (new) |
| 6748 | The linter flags known blocking / allocating libc names | covered | extern_blocking_name_lint_hint, extern_allocating_name_lint_hint (new) |
| 6750 | FFI calls run on real OS threads; the stdlib wraps them | not testable | runtime model |

## Linking foreign libraries

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6754 | A `kara.toml` `[link]` table names the libraries | covered | tests/cli.rs::test_link_directive_resolves_real_library_e2e; tests/s15_conformance_pass.rs; layout.py |
| 6762 | `libs` are bare stems appended as `-l<name>` | covered | tests/cli.rs::test_link_directive_injects_lib_flag_e2e |
| 6763 | `search-paths` are `-L<path>`, before the `-l` flags | partial | tests/cli.rs::test_link_directive_resolves_real_library_e2e resolves through a search path; the order is not asserted |
| 6764 | Both follow the object and the runtime archive | partial | implied by every `[link]` test resolving its symbols; the order is not asserted |
| 6765 | An absent or empty `[link]` leaves the link line unchanged | covered | every compiled cell here (no manifest) |
| 6766 | `karac run` / `karac test` read the table: interpreter `dlopen`, JIT runner opens them | covered | tests/ffi_c_abi_layout_2026_11_22.rs::ffi_abi_run_reads_the_link_table |
| 6766 | A built binary finds the library through the loader; no `rpath` | not drafted | needs a manifest project and `readelf` |
| 6767 | Native targets only; wasm ignores the table | not drafted | needs a manifest project |
| 6768 | No existence check at manifest read; a missing library is a link error | covered | tests/cli.rs::test_link_directive_injects_lib_flag_e2e |

## Opaque foreign types

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6790 | `type FILE;` with `#[kara_name("CFile")]` is `CFile` in Kāra, an unsized handle | covered | opaque_type_fopen_fread_fclose (new); tests/probe_s15_ffi_glibc.rs::ff2_an_opaque_type_is_its_kara_name |
| 6798 | `*const Foo` / `*mut Foo` are permitted wherever a pointer is | covered | opaque_type_fopen_fread_fclose, opaque_pointer_field_ok (new) |
| 6799 | `ref Foo`, `mut ref Foo`, a borrowed `x: Foo` are permitted | covered | opaque_pointer_field_ok (new) |
| 6800 | FFI signatures using them | covered | opaque_type_fopen_fread_fclose (new) |
| 6801 | `ptr.const(r)` on such a reference is the C object's address | covered | tests/s15_conformance_pass.rs::s15_char_crosses_as_char32_and_an_opaque_ref_gives_its_address |
| 6804 | A by-value binding: `E_OPAQUE_TYPE_REQUIRES_INDIRECTION`, quoted message | covered | opaque_by_value_let_refused (new) |
| 6805 | By-value parameters (`own`) and returns: the same | covered | opaque_own_param_refused, opaque_by_value_return_refused (new); tests/typechecker.rs::test_opaque_type_by_value_fn_param_rejected |
| 6806 | A by-value field: the same; behind a pointer is fine | covered | opaque_by_value_field_refused, control opaque_pointer_field_ok (new) |
| 6807 | Field access through `*mut CFile`: `E_OPAQUE_TYPE_NO_FIELDS` | covered | opaque_field_access_through_pointer_refused (new); corpus/diagnostics/e_opaque_type_no_fields |
| 6808 | A method call on an opaque type: `E_OPAQUE_TYPE_NO_METHODS`; methods come through a wrapper type | covered | opaque_method_call_refused (new); corpus/diagnostics/e_opaque_type_no_methods. The page said `E_OPAQUE_TYPE_NO_FIELDS` until B-2026-11-25-14 (review ruling 2026-11-25: fix the doc, not the code) |
| 6808 | Methods cannot be defined on an opaque type | covered | opaque_inherent_impl_refused (new); tests/typechecker.rs::test_inherent_impl_on_opaque_type_rejected |
| 6809 | `size_of` / `align_of`: `E_OPAQUE_TYPE_NO_KNOWN_SIZE` | covered | opaque_size_of_refused, opaque_align_of_refused (new) |
| 6811 | Pattern matching on an opaque place is refused as field access | covered | opaque_pattern_match_refused (new): `E_OPAQUE_TYPE_NO_FIELDS` (B-2026-11-25-13) |
| 6812 | `#[derive(...)]` on an opaque type: `E_OPAQUE_TYPE_NO_INHERENT_OR_TRAIT_IMPLS` | covered | opaque_derive_refused (new) (B-2026-11-25-13) |
| 6812 | `impl Trait for Foo`: the same code | covered | opaque_trait_impl_refused (new); tests/typechecker.rs::test_trait_impl_on_opaque_type_rejected |
| 6813 | `type Foo[T];` is a parse error, `E_OPAQUE_TYPE_GENERIC_FORBIDDEN` | covered | opaque_generic_refused (new); tests/spec_symbol_codes.rs::spec_symbol_opaque_type_generic_forbidden |
| 6815 | An opaque type is an item in the type namespace; `import lib.CFile` works | not drafted | needs a second module |
| 6815 | A duplicate declaration is `W_OPAQUE_TYPE_REDECLARED`, not an error | covered | opaque_redeclared_warns (new); tests/s15_conformance_pass.rs::s15_a_redeclared_opaque_type_warns_and_compiles |
| 6817 | No destructor; a wrapper's `Drop` calls the C cleanup (`SafeFile`) | covered | opaque_safe_file_drop_wrapper (new) |
| 6837 | An opaque declaration has no effects | covered | opaque_declaration_has_no_effects (new) |
| 6839 | Declaring a C type both opaque and `#[repr(C)]` is a coherence error at link time | not drafted | needs two compilation units |

## C strings

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6843 | `String` / `Str` `.as_ptr()` must not go to a `const char *` | not testable | a programmer obligation (tests/probe_s15_ffi_glibc.rs::ff2_a_string_lends_c_its_bytes) |
| 6844 | A `c"..."` literal is a `ref CStr` NUL-terminated for C | covered | c_literal_is_ref_cstr, extern_usize_signature_and_c_literal (new); lexical/c_string_literal_len |
| 6845 | A `CString` from `to_cstring()` for run-time text (`Result[_, NulError]`) | covered | c_strings_greet, to_cstring_interior_nul_is_an_error (new); corpus/design/ok_cstring_reading_surface |
| 6847 | The `greet` example through `puts` | covered | c_strings_greet (new) |

## Host functions

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 6869 | Native: a `host fn` lowers to an `extern "C"` call with the same signature | covered (fixed, B-2026-11-25-8) | host_fn_native_lowers_to_c (new): the build prints `5 6 7`; the interpreter refuses: "`labs` is a `host fn`: it runs only in a wasm module whose host provides it" (src/mir/interp.rs:2305). Run-vs-build divergence |
| 6870 | `wasm_wasi`: an import entry in `kara_host` | covered | tests/cli.rs::wasm_wasi_host_fn_e2e |
| 6872 | Browser and Component Model lowerings | not testable: deferred | |
| 6887 | No body; module scope; attributes and visibility as any item | covered | host_fn_native_lowers_to_c (`pub`, `#[allow]`), host_fn_with_body_refused (new) |
| 6887 | A string crosses as a `(pointer, length)` pair | not testable | convention |
| 6891 | A `host fn` without a `with` clause is an error | covered | host_fn_without_effects_refused (new) |
| 6891 | The declared set is trusted; callers inherit it | covered | host_fn_effects_flow_to_callers (new) |
| 6896 | Primitives, `Copy` types and single-field handle structs are permitted | covered | host_fn_native_lowers_to_c (a `Copy` handle struct, compiled lane) (new) |
| 6901 | An owned non-`Copy` parameter or return is refused | covered | host_fn_owned_string_param_refused, host_fn_string_return_refused (new) |
| 6902 | A borrowed non-`Copy` parameter and `mut ref T` are refused | covered | host_fn_borrowed_param_refused, host_fn_mut_ref_param_refused (new) |
| 6903 | A generic `host fn` is refused | covered | host_fn_generic_refused (new) |
| 6909 | Handles are plain scalars; no count, no destructor | not testable | an absence |
| 6913 | On native, `host fn` and `extern "C"` make the same call | partial | as 6869: same on the compiled lane, refused on the interpreter |
| 6917 | `--target=wasm_wasi` lowers each `host fn` to a `kara_host` import, no glue file | covered | tests/cli.rs::wasm_wasi_host_fn_e2e |
| 6919 | Boundary types are core wasm types; `isize` / `usize` as `i32` | partial | tests/mir_wasm_pointer_width.rs (pointer width); the import signatures are not asserted per type |
| 6920 | `extern "C"` gets no import entry; an unresolved one stays a link error | covered | tests/cli.rs::wasm_wasi_extern_c_stays_loud_undefined |

## Read but not drift

- The §15 example at 6417 passes a `*mut u32` to `ptr.expose`, whose table signature is `*const T`, against the 6365 rule of no implicit `*mut -> *const` coercion. karac accepts it: the `ptr` functions take either pointer kind (tests/probe_s15_ffi_glibc.rs::ff2_is_null_takes_either_raw_pointer). The cells use `expose_mut` for a `*mut`.
- Unsafe operations outside any `unsafe fn` are reported under the code `unsafe_op_in_unsafe_fn` (`error[unsafe_op_in_unsafe_fn]: raw-pointer dereference must be wrapped in an unsafe { ... } block`). The page names no code for them; the refusal cells pin `unsafe`.
- The §15 examples that bind `write`, `fopen`, `fclose` and `fread` without `blocks` draw the 6748 lint note (`L0001`, "commonly blocking"), as the page says they should.
