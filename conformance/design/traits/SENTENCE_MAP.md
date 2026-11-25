# design.md § 9 Traits: sentence-to-cell map

Status: covered, partial, FAILS (filed, the row named), not testable, not drafted. Cell names without a prefix are in `conformance/design/traits/`; `impls/`, `generics/` and `concurrency/` name other directories under `conformance/` (`concurrency/` is outside the three audited directories). Line numbers are docs/design.md lines.

**Totals (2026-11-24, after the gap audit):** 362 normative sentences, 317 covered, 38 partial, 3 FAILS (a cell would fail; filed as B-2026-11-24-18..20; -21 and -22 since fixed), 2 not testable or not a gap, 2 not drafted (CLI output, a two-process hash check). Before the audit: 262 covered, 40 partial, 60 gaps. `(new)` marks a cell the audit added; every new refusal has a control beside it where one exists.

## Declarations and impls

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3166 | A required method (ending in `;`) must be provided by every impl | covered | required_method_missing_refused |
| 3167 | A default method may be overridden or left out | covered | default_method_and_override |
| 3184 | An inherent impl (no `for`) adds methods to the type | covered | receivers |
| 3195 | `user.validate()` and `User.validate(user)` are the same call; `Shape.area(c)` names the trait method | covered | ufcs_same_call |
| 3196 | Receivers: `self` (borrowed), `own self`, `mut ref self` | covered | receivers |
| 3196 | A `shared` type's methods take `self` only | covered | traits/shared_method_mut_ref_receiver_refused + control shared_method_self_receiver (new) |
| 3197 | A method without a receiver is an associated function, called on the type | covered | receivers, assoc_fn_called_on_type |
| 3198 | `fn visit(self, i32);` is E_TRAIT_METHOD_ANONYMOUS_PARAM | covered | anonymous_trait_param_refused |
| 3198 | `_: i32` is accepted; an impl may use different parameter names | covered | underscore_trait_param_and_renamed |
| 3199 | An impl method has exactly the trait method's parameter and return types | partial | conformance_type_mismatch_refused (parameter only; return-type mismatch untested) |
| 3199 | Modes may be the trait's or weaker (own, mut ref, borrowed); stronger refused | covered | conformance_weaker_mode, conformance_stronger_mode_refused |
| 3199 | A call on a known type uses the impl's modes; through a bound, the trait's | covered | conformance_known_type_uses_impl_modes, conformance_generic_call_uses_trait_modes_refused |
| 3200 | An impl holds only the trait's items; help names closest method or says move to inherent impl | covered | impl_extra_method_refused, impl_extra_unrelated_method_refused, impl_extra_assoc_type_refused |
| 3201 | A trait method's `with` clause is a ceiling each impl must stay within | covered | effect_ceiling_exceeded_refused |
| 3201 | A call through a bound has the clause's effects | covered | traits/bound_call_has_clause_effects(_refused) (new) |
| 3201 | No clause: a call through a bound has the instance's effects, charged to whoever instantiates | covered | traits/unclaused_trait_method_charges_instantiator(_refused) (new) |
| 3202 | An impl header effect clause is E0005; karac fix deletes it | covered | impl_header_effect_clause_refused (.fixed) |
| 3203 | Generic impls list parameters after `impl` (inherent and trait) | covered | impls/cond_inherent_impl_brackets, impls/cond_impl_applies_where_bounds_hold |
| 3204 | Associated types are bound in the impl with `type Name = Type;` | covered | assoc_type_impl_and_projection |
| 3205 | `pub trait` exports a trait from its package | covered | tests/cross_package_coherence.rs a_pub_fn_bounded_on_a_private_trait_leaks_it; modules/ private-import cells |

## Associated functions

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3222 | An associated function is called on a type: `T.default()`, `Config.default()` | covered | assoc_fn_called_on_type |
| 3222 | Looked up inherent first, then traits; the ambiguity rules are the same | partial | assoc_fn_inherent_hides_trait (inherent first); two traits' same-named associated fn untested |
| 3222 | There is no bare `default()` form | covered | assoc_fn_bare_default_refused |
| 3224 | No call-site type arguments: never `default[T]()` | covered | assoc_fn_call_site_type_arg_refused |
| 3226 | `Self` as first path segment in an impl: assoc fn, generic type, variant in expression and pattern | covered | self_paths |
| 3226 | In a trait default body `Self` is the implementing type (`Self.one()`) | covered | default_assoc_fn_self_on_primitive, self_paths |
| 3226 | Outside an impl or trait, `Self` is an error | covered | self_outside_impl_refused |
| 3228 | An inherent assoc fn hides a trait one at `T.name()`; `Trait.name(args)` reaches the trait's | covered | assoc_fn_inherent_hides_trait, assoc_fn_inherent_hides_trait_order |
| 3228 | `Trait.name()` takes `Self` from the expected type, through the return type (incl. `Option[Self]`) | covered | assoc_fn_inherent_hides_trait, assoc_fn_self_from_option, generics/trait_assoc_fn_decided_by_expected |
| 3228 | No expected type, or a return type not naming `Self`: error giving `P.name()` and the annotation | covered | assoc_fn_trait_no_expected_refused, assoc_fn_trait_return_not_self_refused, generics/trait_assoc_fn_undecided_refused |
| 3228 | An expected type that does not implement the trait is an error naming it | covered | assoc_fn_trait_expected_not_impl_refused |

## Coherence and the orphan rule

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3232 | At most one impl applies; checked where each impl is declared, order-independent | covered | duplicate_impl_refused, overlap_bounds_ignored_refused, overlap_concrete_and_generic_refused |
| 3234 | Two impls overlap when headers unify, ignoring bounds (both examples) | covered | overlap_bounds_ignored_refused, overlap_concrete_and_generic_refused, impls/cond_impl_overlap_bounds_refused |
| 3234 | The overlap error is reported at the second impl | partial | overlap_* cells (.refused does not pin the location) |
| 3235 | A duplicate impl is an overlap | covered | duplicate_impl_refused |
| 3236 | Orphan rule: a (library) package may write `impl Trait for Type` only if it defines Trait or Type or a type in Trait's args | covered | tests/cross_package_coherence.rs (orphan, from the app and from the library) |
| 3237 | A local type anywhere in the trait's arguments counts (`impl Mul[Column] for f64`) | covered | tests/cross_package_coherence.rs what_the_orphan_rule_allows_still_checks (Mul2[Col] for i64) |
| 3237 | A bare type parameter does not count: `impl[T] Mul[Column] for T` is an error outside Mul's package | covered | refused as a blanket impl; probed in a 3-package tree (E0246) |
| 3237 | An upstream blanket impl covering a downstream header makes it overlap, reported at the downstream impl | covered | traits/impl_trait_args_unify_overlap_refused, control traits/impl_trait_args_disjoint_ok, modules/blanket_trait_args_overlap_across_packages_refused (B-2026-11-24-18, fixed) |
| 3238 | The application package is exempt: it may implement a foreign trait for a foreign type | covered | app_exempt_foreign_trait_foreign_type, app_foreign_trait_foreign_type, impls/app_foreign_trait_foreign_type_ok |
| 3238 | The overlap rule still applies in the application | covered | app_impl_overlapping_library_refused, impls/overlap_with_std_* |
| 3239 | A blanket impl `impl[T: B] Trait for T` is allowed only if the package defines Trait (or is the application) | partial | blanket_impl_own_trait (accepted form only; library refusal untested) |
| 3240 | Inherent impls only on your own types, application included | covered | inherent_impl_on_foreign_type_refused, inherent_impl_on_primitive_refused |
| 3242 | A program implements its own trait for a concrete tuple | covered | tuple_user_trait_impl |
| 3242 | The std tuple impls (Display, Clone, PartialEq...) are the only ones of those traits | covered | impl_display_for_tuple_overlaps_std_refused, tuple_to_string_is_the_std_display |
| 3242 | A tuple impl over type parameters is refused | covered | tuple_generic_trait_impl_refused |
| 3242 | A tuple has no inherent impl | covered | traits/tuple_inherent_impl_refused (new) |
| 3243 | Every impl form (unconditional, conditional, blanket, with assoc types) follows these rules | partial | impls/cond_impl_overlap_*, assoc_type_second_impl_refused (marker-impl overlap and library orphan forms untested) |
| 3272 | The newtype escape hatch: a foreign trait on a local `distinct type` | covered | distinct_type_newtype |

## Method resolution

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3289 | An inherent method named m is chosen; trait methods named m are not considered | covered | resolution_inherent_hides_trait_method |
| 3289 | Generic inherent impls whose bounds hold count as inherent | covered | resolution_generic_inherent_with_bounds |
| 3289 | An inherent assoc fn without self does not hide a trait's m(self); it is reached as `T.m()` | covered | resolution_assoc_fn_does_not_hide_method, resolution_assoc_fn_does_not_hide_method_order |
| 3289 | Which impl comes first in the source never matters | covered | resolution_*_order, assoc_fn_inherent_hides_trait_order |
| 3290 | Trait candidates include conditional impls whose bounds hold; two or more is ambiguity | covered | resolution_two_traits_ambiguous_refused, impls/cond_impl_applies_where_bounds_hold |
| 3291 | `own self` moves e, or copies it when T is Copy | covered | resolution_own_self_moves, resolution_own_self_copy_copies |
| 3291 | `mut ref self` needs a mutable place; mismatch is an error at the call | covered | resolution_receiver_mut_needs_mutable_place_refused, impl_trait_mut_ref_receiver_needs_mut_refused |
| 3291 | The receiver check never changes which method was chosen | covered | resolution_inherent_self_beats_trait_own_self |
| 3294 | A `ref T` / `mut ref T` receiver finds T's methods | covered | resolution_ref_finds_methods |
| 3294 | Calling an `own self` method through a reference to a non-Copy T is an error | covered | resolution_own_self_through_ref_refused, dyn_own_self_needs_owned_refused |
| 3295 | A String receiver finds Str's methods; Vec/Array find Slice's, after their own | covered | resolution_string_finds_str_vec_finds_slice |
| 3298 | No autoref ladder: inherent `m(self)` always wins over a trait's `m(own self)` | covered | resolution_inherent_self_beats_trait_own_self |
| 3300 | Traits in scope: declared, imported by name, prelude; methods are candidates exactly when the name resolves | covered | resolution_trait_imported_by_name, trait_method_in_scope_named, trait_method_out_of_scope_refused |
| 3307 | Ambiguity diagnostic lists each candidate with trait, signature, effects; suggests UFCS | covered | ambiguity_diagnostic_lists_effects_refused, resolution_two_traits_ambiguous_refused |
| 3309 | UFCS calls the named trait's impl, checked against its signature and effects; reaches a hidden trait method | covered | resolution_two_traits_ufcs, resolution_inherent_hides_trait_method, resolution_ufcs_writer_effect_charged_refused |
| 3311 | A generic trait named with its arguments selects one impl (`Conv[String].conv(n)`) | covered | resolution_generic_trait_named_with_args |
| 3311 | Bare `Conv.conv(n)` is ambiguous whatever the expected type; diagnostic lists qualified forms | covered | resolution_generic_trait_bare_ambiguous_refused |
| 3313 | No method found: "no method 'm' on type 'T'" with a did-you-mean | covered | resolution_no_method_suggests_refused |
| 3315 | Distinct types do not reach their base type's methods | covered | resolution_distinct_type_no_base_methods_refused |
| 3317 | No user Deref: no trait can reroute method lookup | covered | traits/no_user_deref_refused (new) |

## Effects of an ambiguous call

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3321 | Two traits with same-named methods and different effects coexist on one type | covered | resolution_ufcs_effects_per_trait |
| 3343 | The bare call is ambiguous; each UFCS call has its own trait's effects | covered | ambiguity_diagnostic_lists_effects_refused, resolution_ufcs_writer_effect_charged_refused, resolution_ufcs_writer_in_par_conflicts_refused |
| 3349 | No union of effects across candidates | covered | resolution_ufcs_effects_per_trait |

## Generic receivers

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3353 | In a generic body lookup uses the bound: one candidate whatever else T implements | covered | resolution_generic_receiver_uses_bound |
| 3353 | With `T: Reader + Writer`, `x.access()` is ambiguous; UFCS resolves it | partial | resolution_generic_two_bounds_ambiguous_refused (refusal only; the UFCS control is untested) |
| 3353 | A generic body may use only what its bounds provide, operators included | covered | resolution_generic_body_only_bounds_refused, generics/bounds_body_uses_only_bounds, impls/generic_never_widens_refused |
| 3355 | A conditional impl whose bounds fail is dropped from the candidates, not an error by itself | covered | resolution_conditional_impl_dropped_refused |
| 3355 | If nothing remains, the error names the impl and the unmet bound | partial | impls/cond_impl_bound_not_met_refused, resolution_conditional_impl_dropped_refused (neither pins "via `impl Show for W`") |

## Raw pointers need a known pointee

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3359 | A method call on a raw pointer with an unresolved pointee is E_RAW_POINTER_UNRESOLVED_POINTEE | covered | traits/raw_pointer_unresolved_pointee_refused (new) |
| 3382 | It does not fire for a generic pointee in scope or an annotated binding | covered | traits/raw_pointer_known_pointee (new) |
| 3384 | The check runs before the unsafe requirement, so it is reported inside `unsafe` too | covered | traits/raw_pointer_unresolved_pointee_refused (new; the call is inside `unsafe`) |
| 3385 | No lookup through the pointer: a method of T needs `(*p).method()` | covered | traits/raw_pointer_no_method_lookup_through_refused + raw_pointer_method_through_deref (new) |
| 3386 | A program trait may be implemented for a pointer to a concrete type; answers calls, UFCS, bounds, dyn | covered | pointer_user_trait_impl |
| 3386 | `*mut T` and `*const T` are distinct types, each needing its own impl | partial | pointer_user_trait_impl (both impls written; missing one not refused) |
| 3386 | The pointer's own methods come first | covered | raw_pointer_own_methods_first (new; fixed by B-2026-11-24-21) |
| 3386 | A pointer impl with a type-parameter pointee, and an inherent impl on a pointer type, are refused | covered | traits/raw_pointer_generic_pointee_impl_refused, raw_pointer_inherent_impl_refused (new) |

## No specialization

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3390 | No `default fn`, no priority between impls, no most-specific-wins | partial | overlap_concrete_and_generic_refused (overlap refused; `default fn` syntax untested) |

## Supertraits

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3407 | `impl Sub for T` is rejected when a supertrait impl is missing | covered | supertrait_missing_refused, supertrait_several_one_missing_refused |
| 3411 | No reuse of method bodies: each impl implements its own trait's methods | partial | impl_extra_method_refused (generic; a supertrait method written in the subtrait impl untested) |
| 3412 | No subtyping: Sub is not a subtype of Super | partial | dyn_no_upcast_refused |
| 3413 | No dispatch up a chain: a call resolves through the declaring trait | covered | supertrait_no_dispatch_up_chain |
| 3416 | Default bodies may call supertrait methods | covered | supertrait_default_uses_super, supertrait_several |
| 3425 | Shorter bounds: `T: Sub` gives Super's methods | covered | supertrait_default_uses_super, supertrait_no_dispatch_up_chain |
| 3428 | Several supertraits, each implemented separately | covered | supertrait_several, supertrait_several_one_missing_refused |
| 3430 | A default method's `where Self: Bound`: body checked under it; a type without it lacks only that method, refused at the call | covered | supertrait_method_bound_where_self, supertrait_method_bound_unmet_refused, supertrait_cycle_not_clone_refused |
| 3432 | A failing impl reports "requires" | covered | supertrait_missing_refused |
| 3434 | Effects are not inherited from a supertrait | covered | supertrait_effects_not_inherited_refused |

## Marker traits

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3446 | `marker trait Foo;` and `marker trait Foo { }` are the same | covered | marker_trait_forms |
| 3447 | A method in a marker trait body is E_MARKER_TRAIT_HAS_METHOD | covered | marker_trait_method_refused |
| 3448 | `type Item;` or `const SIZE` in the body is E_MARKER_TRAIT_HAS_ITEM | covered | marker_trait_assoc_type_refused, marker_trait_const_refused |
| 3449 | Methods or items in a marker impl body are E_MARKER_IMPL_HAS_METHOD | partial | marker_impl_method_refused (method only; an item such as `type X = i64;` untested) |
| 3449 | `impl Foo for T;` is not accepted | covered | marker_impl_semicolon_refused |
| 3450 | Supertraits are checked at every marker impl | covered | marker_supertrait_checked_refused |
| 3451 | Generics and conditional marker impls work | covered | marker_trait_forms |
| 3452 | Never implemented implicitly | covered | marker_missing_impl_refused |
| 3453 | Coherence and the orphan rule apply unchanged to markers | covered | traits/marker_impl_twice_refused (new) |
| 3454 | Use as a bound; a missing impl is the ordinary error | covered | marker_missing_impl_refused, std_pod_marker_nameable |
| 3460 | The std markers Eq and Pod | covered | marker_eq_empty_impl, marker_eq_has_no_methods_refused, std_pod_marker_nameable |

## Associated types

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3473 | The impl binds the associated type once | covered | assoc_type_impl_and_projection |
| 3488 | Projection `T.Name`, and `Self.Name` inside the trait | covered | assoc_type_impl_and_projection, assoc_type_bound_usable_generic |
| 3498 | A projection two bounds declare is E_AMBIGUOUS_ASSOC_TYPE, `where T.Name = X` too | covered | assoc_type_ambiguous_projection_refused, assoc_type_ambiguous_where_eq_refused |
| 3498 | Each bound fixes its own; a type parameter names it; an operator's result carries its trait | covered | assoc_type_each_bound_fixes_own |
| 3498 | There is no qualified spelling such as `T.Add.Output` | covered | assoc_type_qualified_spelling_refused (new; fixed by B-2026-11-24-22) |
| 3508 | Equality constraints in where; bound form `I: Iterator[Item = i64]` | covered | assoc_type_equality_constraints, assoc_type_equality_mismatch_refused |
| 3508 | One equality constraint per associated type | covered | where_two_equalities_one_assoc_type_refused |
| 3510 | One impl per trait, so one binding of each assoc type | covered | assoc_type_second_impl_refused |
| 3520 | A generic trait parameter allows several impls for one type | covered | resolution_generic_trait_named_with_args |
| 3531 | A bound on an associated type is checked at every impl and usable without repeating | covered | assoc_type_bound_checked_at_impl_refused, assoc_type_bound_usable_generic |
| 3535 | The standard traits' associated types (Item, Output, Error) | covered | impls/index_user_traits, impls/iter_user_types, impls/conv_try_from_gives_try_into, impls/op_user_impls |
| 3546 | Generic associated types (`type Mapped[U]`) are deferred | not a gap | GATs are experimental (deferred.md); no cell |

## impl Trait

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3550 | The bare `Trait` form is a type error in every position | partial | impl_trait_bare_trait_type_refused (argument position only) |
| 3556 | impl Trait is legal in argument, return, and trait-method return positions | covered | impl_trait_argument_sugar, impl_trait_return, impl_trait_in_trait_method_return |
| 3560 | Type-alias `impl Trait` is deferred | covered | traits/impl_trait_type_alias_refused (new) |
| 3562 | Argument position is generic sugar; each `impl Trait` independent; `ref`/`mut ref impl Trait` | covered | impl_trait_argument_sugar |
| 3562 | Argument-position impl Trait is not allowed in trait methods | covered | impl_trait_argument_in_trait_method_refused |
| 3566 | The caller cannot match on, downcast or name the concrete type | covered | impl_trait_return_opaque_refused, impl_trait_return_field_hidden_refused |
| 3579 | An inline binding makes the projection visible to the caller | covered | impl_trait_return |
| 3579 | Without the binding, `it.next()` has no type the caller can use | covered | traits/impl_trait_without_binding_item_untyped_refused (new) |
| 3580 | The concrete Item must equal the binding (E_IMPL_TRAIT_ASSOC_MISMATCH) | covered | impl_trait_assoc_mismatch_refused |
| 3582 | The binding is legal in a bound, argument position and return position | partial | assoc_type_equality_constraints, impl_trait_return (argument position untested) |
| 3582 | Naming an undeclared associated type is E_UNKNOWN_ASSOC_TYPE_BINDING | covered | impl_trait_unknown_assoc_binding_refused |
| 3582 | An inline binding names a type, not a type constructor | covered | traits/impl_trait_binding_names_constructor_refused (new) |
| 3584 | A witness that does not implement the trait is refused | covered | impl_trait_witness_not_impl_refused |
| 3584 | Two distinct witnesses is E_IMPL_TRAIT_MULTIPLE_WITNESSES | covered | impl_trait_two_witnesses_refused |
| 3586 | Calling the fn has its own effects; methods on the value have the hidden type's, charged to the caller | covered | impl_trait_effects_deferred_to_methods, impl_trait_method_effects_charged_refused |
| 3601 | A parenthesized `with` ceiling on the return type bounds the hidden type's methods | covered | impl_trait_return_ceiling, impl_trait_return_ceiling_exceeded_refused |
| 3607 | A method call on the value then has the ceiling's effects | partial | impl_trait_return_ceiling (caller's charge not asserted) |
| 3609 | The result borrows every bare non-Copy parameter | covered | GAP_impl_trait_result_borrows_bare_param_refused (GAP_ prefix, undocumented) |
| 3609 | For a method with `self` / `mut ref self`, the result borrows `self` | covered | traits/impl_trait_result_borrows_self(_refused) (new) |
| 3609 | The result borrows every `mut ref` parameter and every view parameter | covered | traits/impl_trait_result_borrows_view_param_refused (new) |
| 3609 | With no such parameter the result borrows nothing | covered | impl_trait_borrow_rule |
| 3609 | The value is live until scope end, so the borrow persists, a Copy hidden type included | covered | traits/impl_trait_borrow_persists_copy_hidden(_refused) (new) |
| 3609 | Only a bound that is itself Copy makes the value Copy | covered | impl_trait_copy_supertrait_copies (B-2026-11-24-19, fixed); refusal half: traits/impl_trait_not_copy_refused |

## dyn Trait

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3622 | A call through dyn dispatches to the concrete type's method | covered | dyn_page_example |
| 3654 | An owned dyn owns its concrete value and drops it | covered | dyn_option_and_drop_in_vec, dyn_calls_and_moves |
| 3654 | An owned dyn moves, and is Copy never | covered | dyn_owned_moves_refused (B-2026-11-24-20, fixed) |
| 3655 | `ref dyn` / `mut ref dyn` view a concrete value or an owned dyn | covered | dyn_coercion_sites |
| 3656 | Every associated type is bound in the dyn type; an unbound one is an error naming it | covered | dyn_iterator_bound_assoc, dyn_assoc_type_unbound_refused |
| 3660 | The `dyn` keyword is required in a type position | partial | impl_trait_bare_trait_type_refused (same refusal; no dyn-context cell) |
| 3660 | A second trait after `+` is an error, except CrossTask | covered | dyn_second_trait_refused, concurrency/crosstask_accepted |
| 3662 | Coercion at a typed let, argument, field, return, collection element | covered | dyn_coercion_sites, dyn_page_example |
| 3664 | `ref S` to `ref dyn`, and a view of an owned dyn, allocate nothing | covered | dyn_coercion_sites |
| 3665 | S must implement Trait with the bound associated types | covered | traits/dyn_coercion_needs_impl_refused, dyn_coercion_assoc_binding_mismatch_refused (new) |
| 3666 | A coercion never reaches inside a type | covered | dyn_no_coercion_inside_type_refused |
| 3668 | Every dyn is two words, and `Option[dyn Trait]` is two words | covered | traits/dyn_two_words (new; needed the parser fix B-2026-11-24-9) |
| 3668 | dyn values have no `==` of their own | covered | dyn_no_eq_refused |
| 3671 | Dispatchable: receiver, no own type params, Self only as receiver | covered | dyn_not_dispatchable_assoc_fn_refused, dyn_not_dispatchable_generic_method_refused, dyn_not_dispatchable_refused |
| 3676 | Supertraits' required methods must be dispatchable too | covered | traits/dyn_supertrait_not_dispatchable_refused (new) |
| 3676 | A method returning impl Trait is E_RPITIT_INCOMPATIBLE_WITH_DYN | covered | dyn_impl_trait_return_code_refused |
| 3676 | A non-dispatchable default method still runs on dyn; a dyn fills `[T: Trait]` | covered | dyn_default_generic_method_runs |
| 3676 | User code cannot write an impl whose Self is a dyn type | covered | dyn_user_impl_for_dyn_refused |
| 3678 | `Trait: Clone` makes dyn Trait Clone, cloning the concrete value | covered | dyn_clone_supertrait |
| 3681 | `self` runs on a view; `mut ref self` changes the value in place | covered | dyn_calls_and_moves |
| 3682 | `own self` needs an owned dyn and consumes it | covered | dyn_calls_and_moves, dyn_own_self_needs_owned_refused |
| 3683 | `*r = other` through `mut ref dyn` is an error; assigning the owned dyn drops the old value first | covered | dyn_mut_ref_replace_refused, dyn_calls_and_moves |
| 3685 | No upcast; a dyn Sub can call every Super method | covered | dyn_no_upcast_refused, dyn_sub_calls_super_methods |
| 3687 | An owned dyn's drop runs the concrete drop | covered | dyn_option_and_drop_in_vec |
| 3690 | Non-escaping dyn param: effects of the value passed at each call site | covered | dyn_param_effects_from_value |
| 3691 | Elsewhere: the type's clause; coercing a value exceeding it is an error naming method and effect | covered | dyn_effects_clause_bounds_calls_refused |
| 3691 | The clause is narrowed to the method's own ceiling when the trait declares one | covered | traits/dyn_field_clause_narrowed_to_ceiling (new) |
| 3691 | A dyn type with neither clause nor ceiling has every effect | covered | traits/dyn_field_no_clause_no_ceiling_refused (new; one E0400, naming the call, the same on every run since B-2026-11-24-23) |
| 3692 | `-> (dyn Shape with reads(Clock))` gives the returned value the clause | covered | traits/dyn_return_clause_parenthesized(_refused) (new) |
| 3692 | The return_clause_on_fn warning; `-> (dyn Shape) with ..` silences it | partial | dyn_return_clause_warning (.out only; warning not asserted; silencing form untested) |
| 3693 | An effect variable in the clause is solved from coerced values | covered | dyn_effect_variable_router |
| 3710 | A dyn uses the providers in scope at coercion; E_DYN_PROVIDER otherwise | covered | dyn_static_default_ok, dyn_provider_refused |
| 3712 | dyn Trait is not CrossTask; dyn Trait + CrossTask is | covered | concurrency/crosstask_dyn_refused, concurrency/crosstask_accepted |
| 3712 | Each coercion into `dyn Trait + CrossTask` checks the concrete type is CrossTask | covered | traits/dyn_crosstask_coercion_checks_concrete_refused (new) |
| 3712 | A trait with CrossTask as a supertrait makes every dyn of it CrossTask | covered | traits/dyn_crosstask_supertrait (new) |
| 3712 | No view carve-out: an unmarked `ref dyn Trait` may not reach a task or par branch | covered | traits/dyn_view_not_crosstask_refused (new) |
| 3712 | `Fn(A) -> R` is not CrossTask; `+ CrossTask` is | covered | concurrency/crosstask_unmarked_fn_refused, concurrency/crosstask_unmarked_fn_par_refused, concurrency/crosstask_accepted |

## CrossTask

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3718 | T: CrossTask holds unless T contains a shared handle or task-local type | covered | concurrency/crosstask_shared_* |
| 3719 | `impl CrossTask for T` is an error | covered | concurrency/crosstask_impl_refused |
| 3719 | CrossTask cannot be derived | covered | impls/crosstask_derive_refused (new) |
| 3720 | CrossTask is usable as a bound | covered | concurrency/crosstask_accepted |
| 3721 | Views enter par branches and TaskGroup tasks, never a channel | covered | concurrency/crosstask_accepted, concurrency/channel_view_refused |
| 3722 | A generic fn that spawns a T must require T: CrossTask; reported at the signature | covered | concurrency/crosstask_generic_bound_refused |
| 3723 | A `par for` item must be CrossTask; `T: Copy` alone is refused | covered | impls/par_for_item_copy_not_crosstask_refused + par_for_item_copy_crosstask (new) |

## Lang-item traits

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3736 | The lang items drive their syntax (Copy, Drop, operators, Index, for, ?, Display) | covered | lang_items |
| 3746 | A derive produces an ordinary impl | covered | impls/derive_and_manual_impl_refused |

## Conditional impls

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3750 | An impl with bounds applies only where they hold | covered | impls/cond_impl_applies_where_bounds_hold, impls/cond_impl_bound_not_met_refused |
| 3765 | Several bounds with `+`; long bounds in `where` | covered | impls/cond_impl_applies_where_bounds_hold |
| 3767 | Inherent impls use the same brackets | covered | impls/cond_inherent_impl_brackets |
| 3778 | Derive adds the bounds the field types need | covered | impls/derive_adds_bounds, impls/derive_bound_not_met_refused |
| 3789 | Overlapping conditional impls are an error | covered | impls/cond_impl_overlap_refused, impls/cond_impl_overlap_bounds_refused |

## Operator traits

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3793 | Every operator is a trait call: `a + b` is `Add.add(a, b)`, primitives too | partial | impls/missing_impl_in_trait_terms_refused (UFCS `Add.add(2, 3)` / `PartialEq.eq` untested) |
| 3800 | `a != b` is `not PartialEq.eq(a, b)` | covered | impls/op_partialeq_rhs |
| 3801 | `a < b` is `partial_cmp(a, b).is_lt()` (and <=, >, >=) | covered | impls/op_partialord_user |
| 3802 | Bitwise and unary operators desugar to their traits | covered | impls/op_user_impls |
| 3846 | Rhs defaults to Self | covered | impls/op_generic_add_output, generics/default_generic_args |
| 3847 | PartialEq and PartialOrd take Rhs | partial | impls/op_partialeq_rhs (PartialEq only; a user `PartialOrd[Rhs]` untested) |
| 3847 | Eq and Ord stay homogeneous | covered | impls/ord_heterogeneous_refused (new) |
| 3848 | Output is the result type | covered | impls/op_user_impls, impls/op_generic_add_output |
| 3849 | Comparison traits borrow both operands | covered | impls/op_partialeq_rhs |
| 3850 | An impl may borrow where the trait owns; String `a + b` moves neither | covered | impls/string_add_moves_neither |
| 3861 | Ordering and Option[Ordering] helpers; None makes every predicate false | covered | impls/ordering_helpers, impls/float_nan_comparisons |
| 3881 | reverse, then, then_with | covered | impls/ordering_helpers |
| 3883 | Comparisons go through PartialOrd (a PartialOrd-only type supports <) | covered | impls/op_partialord_user |
| 3883 | Eq/Ord appear only as bounds | covered | impls/float_not_eq_refused, impls/float_not_ord_refused |
| 3887 | Eq adds no method; Ord adds cmp | covered | marker_eq_empty_impl, marker_eq_has_no_methods_refused, impls/derive_basic_point |
| 3889 | `a += b` is `a = a + b`, place once; Output must be a's type | covered | impls/compound_assign_widening, impls/compound_assign_narrow_refused, impls/op_user_impls |
| 3889 | Through a mut ref binding, `x += 1` writes through | covered | impls/compound_assign_widening |
| 3891 | Shl/Shr take any integer on the right, left's type as Output | covered | impls/shifts_any_integer_rhs |
| 3891 | Widening within a kind before dispatch; across kinds needs `as` | covered | impls/mixed_width_primitives, impls/mixed_kind_refused |
| 3891 | Generic code never widens | covered | impls/generic_never_widens_refused |
| 3895 | Add on String | covered | impls/string_add_moves_neither |
| 3896 | PartialEq through Vec, Option, Result and tuples | partial | Vec/Option compared in impls/index_place_ops, impls/float_nan_comparisons; Result and tuple `==` untested |
| 3897 | Eq on every primitive except floats, and through containers | covered | impls/float_not_eq_refused, impls/derive_adds_bounds |
| 3898 | PartialOrd on strings, and through Vec, Option, Result and tuples | partial | impls/index_place_ops (String only); container `<` untested |
| 3899 | Ord minus floats; F32/F64 implement Eq, Ord, Hash | covered | impls/float_not_ord_refused, impls/total_order_floats (F64 only) |
| 3900 | Bitwise traits on integers only; `a & b` on bools is refused | covered | impls/bool_bitwise_refused |
| 3901 | `~` takes integers only; `not` takes bool only | covered | impls/not_and_tilde_ok, impls/not_on_integer_refused, impls/tilde_on_bool_refused |
| 3903 | No `impl Add for Vec[T]`, with a diagnostic naming extend | covered | impls/vec_add_refused |
| 3903 | concat() flattens nested sequences and joins strings | covered | impls/concat_flattens |
| 3903 | concat() on other elements: deprecated, fix writes join("") | covered | impls/concat_display_deprecated (.fixed) |
| 3905 | User impls allowed for every operator trait, Index included | covered | impls/op_user_impls, impls/index_user_traits |
| 3919 | Distinct types derive the comparison traits | covered | impls/distinct_derive_compare |
| 3919 | `#[derive(Arithmetic)]`: same distinct type only; other types stay an error | covered | impls/distinct_derive_compare, impls/distinct_no_arith_without_derive_refused, impls/distinct_arith_other_type_refused, impls/distinct_arith_two_distinct_refused |
| 3933 | A missing impl is reported in trait terms | covered | impls/missing_impl_in_trait_terms_refused |
| 3933 | `karac explain --concept=operators` shows the desugaring table | not drafted | CLI output; left |

## Index and IndexMut

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 3958 | `c[key]` is the place `*Index.index(c, key)`; `c[i, j]` is tuple-index sugar | covered | impls/index_user_traits, lang_items |
| 3960 | `c[key] = v` uses IndexSet when implemented, else IndexMut | covered | impls/index_user_traits |
| 3965 | Map and SortedMap: `m[k] = v` inserts or replaces | covered | impls/index_map_set_inserts |
| 3965 | The replaced old value is dropped | covered | impls/index_set_drops_old_value (new) |
| 3965 | Sequences have no IndexSet: `v[i] = x` out of bounds panics | covered | impls/index_vec_set_out_of_bounds_panics |
| 3965 | Compound and method-through on a missing Map key panic | covered | impls/index_map_compound_missing_panics, impls/index_map_method_through_missing_panics |
| 3965 | A write to a part, `m[k].f = v`, on a missing key panics | covered | impls/index_part_write_missing_key_panics (new) |
| 3968 | `let t = v[i]` on a non-Copy element is E_INDEX_MOVE_NON_COPY | covered | impls/index_move_non_copy_refused |
| 3969 | `v[i] = v[j]` on a non-Copy element is refused | covered | impls/index_assign_from_index_non_copy_refused |
| 3970 | Borrowing operators and methods work on the element in place | covered | impls/index_place_ops |
| 3971 | `v[i] = value` drops the old element then stores | covered | impls/index_place_ops |
| 3998 | mem.take, mem.replace and mem.swap work on an index place | partial | impls/index_place_ops (mem.swap untested) |
| 4000 | Out of bounds panics; `.get(i)` returns Option | covered | impls/index_out_of_bounds_and_get |
| 4010 | Range indexing on Vec/Array/Slice gives a Slice; on strings a Str | covered | impls/index_ranges |
| 4010 | `c[a..b] = other` copies; a length mismatch panics | covered | impls/index_ranges, impls/index_range_assign_length_mismatch_panics |
| 4010 | Range assignment requires T: Copy | covered | impls/range_assign_requires_copy_refused (new) |

## Conversion traits

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4028 | The blanket impl gives Into from every From | covered | impls/conv_from_gives_into |
| 4036 | A direct `impl Into` is an error | covered | impls/conv_direct_into_refused |
| 4038 | `T.from(x)` picks by x's type; `x.into()` by the expected type; neither is an error | covered | impls/conv_choose_among_froms, impls/conv_into_expected_type_from_argument, impls/conv_into_undecided_refused |
| 4040 | `x.into()` has exactly the selected From impl's effects | covered | impls/conv_effects_through_into_ok, impls/conv_effects_through_into_refused |
| 4071 | TryFrom gives TryInto | covered | impls/conv_try_from_gives_try_into |
| 4071 | Effects pass through TryInto the same way | covered | impls/try_into_effects_pass_through_refused (new) |
| 4073 | `?` converts errors with From | covered | impls/conv_question_uses_from |
| 4073 | Every Error converts into AnyError; AnyError does not implement Error | covered | impls/error_converts_to_any_error, any_error_not_error_refused + any_error_display (new) |
| 4077 | The standard conversion impls | covered | impls/conv_standard_impls |

## Copy

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4093 | A Copy value is copied where a move-only one would move; the source stays valid | covered | impls/copy_derive_and_use |
| 4103 | Copy types: Array/tuples/Option/Result of Copy parts; ref T but not mut ref T; Fn closures with Copy captures | partial | impls/copy_derive_and_use (primitives and derived types only) |
| 4105 | A manual `impl Copy` is not allowed | covered | impls/copy_manual_impl_refused |
| 4105 | The Copy derive is rejected if a field is not Copy | covered | impls/copy_derive_non_copy_field_refused |
| 4113 | A fieldless enum is not Copy unless it derives it | covered | impls/copy_fieldless_enum_not_copy_refused |
| 4114 | A distinct type does not inherit Copy; it may derive it | covered | impls/copy_distinct_not_inherited_refused, impls/copy_derive_and_use |
| 4121 | T: Copy satisfies a T: Clone bound | covered | impls/copy_derive_and_use |
| 4121 | `#[derive(Copy)]` without Clone: error whose fix adds Clone (0.8.x: fill-in plus warning) | partial | impls/copy_without_clone (.out only; warning and fix not asserted) |

## Clone

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4134 | The signature is fixed: borrowed receiver, owned Self | partial | conformance_stronger_mode_refused (generic rule; no Clone-specific cell) |
| 4139 | Owned value types clone deep | covered | impls/clone_semantics |
| 4140 | A shared handle inside a clone is copied, both reach one object | covered | impls/clone_semantics |
| 4141 | `r.clone()` on a ref clones the referent; through a Clone bound it copies the reference | partial | impls/clone_semantics (method form only) |
| 4146 | Derived Clone clones each field; the bound propagates | covered | impls/clone_semantics, impls/clone_pair_bound_refused |
| 4146 | For an enum every variant's fields must be Clone | covered | impls/derive_clone_enum_variant_field_not_clone (new) |
| 4146 | Manual Clone impls are allowed | covered | impls/clone_manual_impl (new) |

## Drop

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4150 | drop runs when the value is destroyed | covered | lang_items, impls/drop_order_body_then_fields |
| 4168 | The signature is fixed: `fn drop(mut ref self)` | covered | impls/drop_signature_own_refused, drop_signature_self_refused (new) |
| 4170 | A drop body may suspend; a cancelled task's teardown runs it to completion | covered | corpus/core/drop_body_suspends_in_a_cancelled_teardown (+7 sibling pins) |
| 4172 | Drop and Copy are exclusive, rejected naming both | covered | impls/copy_and_drop_exclusive_refused |
| 4174 | A Drop body may panic; the panic ends the process | covered | impls/drop_body_panics_ends_process |
| 4176 | A drop's effects are charged to the scope where it happens | covered | impls/drop_effects_charged_to_scope(_refused) (new) |
| 4178 | Drop is not derivable | covered | impls/drop_not_derivable_refused |
| 4180 | Body first, then fields in reverse declaration order; enum payload after the body | covered | impls/drop_order_body_then_fields |
| 4199 | Drop and defer share one LIFO order | covered | impls/drop_order_body_then_fields |
| 4201 | No part of a Drop value can be moved out; a full destructure is refused | covered | impls/drop_move_field_out_refused, impls/drop_destructure_refused |
| 4203 | Reading a Copy field and a pattern over a borrowed Drop value are allowed | covered | impls/drop_allowed_reads |
| 4201 | The fixes (.clone(), mem.take, mem.replace, mem.swap) are accepted on a Drop value's field | partial | impls/drop_allowed_reads (.clone() only) |
| 4206 | A type without Drop may have fields moved out; the rest drop at scope end | partial | impls/drop_allowed_reads (move only; the rest's drop not observed) |

## Destructors are not soundness mechanisms

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4222 | Skipping a destructor needs unsafe | not testable | no operation that skips a destructor exists, safe or unsafe |
| 4224 | Drops and defers do not run on a panic | covered | impls/drop_not_run_on_panic (new) |

## Eq, Ord and Hash

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4232 | Map and Set keys need Hash + Eq | covered | impls/map_key_missing_hash_refused |
| 4240 | Hasher's default write_* bodies feed through write | covered | impls/hash_derive_feeds_fields |
| 4254 | Hash values differ between runs (per-process seed) | not drafted | needs two processes; left |
| 4258 | Derived Hash: struct fields in order; enum discriminant then fields | covered | impls/hash_derive_feeds_fields |
| 4259 | derive(Hash) implies PartialEq and Eq: warns, 0.8.x fills in, karac fix adds them | covered | impls/derive_implied_traits_fixed (new, karac fix); hash_without_eq_allowed comment corrected |
| 4260 | A key type lacking Hash or Eq: the diagnostic names the missing trait | partial | impls/map_key_missing_hash_refused (missing Hash only) |
| 4261 | A manual impl Hash is allowed | covered | impls/hash_manual_impl |

## Display and Debug

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4277 | `{expr}` calls to_string; println renders through Display; one value in several slots | covered | impls/display_same_at_every_depth |
| 4277 | Primitives, strings, collections, Option, Result, tuples implement Display | covered | impls/display_same_at_every_depth, tuple_to_string_is_the_std_display |
| 4280 | Debug: strings quoted, None, named fields, brackets | covered | impls/debug_derived_forms |
| 4285 | Missing Debug for dbg/assert_eq: error names it and suggests derive; no Display fallback | covered | impls/debug_missing_for_dbg_refused, impls/debug_missing_for_assert_eq_refused |
| 4287 | A hand-written Display renders the same at every depth | covered | impls/display_same_at_every_depth |
| 4287 | A container is Display exactly when its elements are | covered | impls/display_vec_without_display_refused |
| 4287 | Debug keeps the field-name form at every depth (`dbg(v)` gives `[A { n: 7 }, B]`) | partial | impls/debug_derived_forms (Vec[String] only) |
| 4290 | Derived Display of a struct | covered | impls/display_derived_forms |
| 4291 | Derived Display of enum unit and data variants | covered | impls/display_derived_forms |
| 4308 | `Display(snake_case)` casing | covered | impls/display_derived_forms |
| 4309 | A hand-written Display replaces the derived form | covered | impls/display_same_at_every_depth |
| 4311 | Derived Debug forms; bound propagates; never derived implicitly | covered | impls/debug_derived_forms, impls/debug_missing_for_dbg_refused |

## Iterator and Iterable

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4334 | Every iterator is IntoIterator of itself | covered | impls/iter_blanket_into_iterator |
| 4342 | Extend impls on Vec, VecDeque, Set, SortedSet, Map, SortedMap, String | covered | impls/iter_extend (Set and Map not exercised) |
| 4345 | Iterable borrows; a user type can implement it | covered | impls/iter_user_types |
| 4346 | IntoIterator consumes; `for` never calls it | covered | impls/iter_for_receiver_needs_into_iter_refused |
| 4349 | `for` over an Iterator moves and consumes it | covered | impls/iter_for_consumes_iterator_refused, impls/iter_user_types |
| 4350 | Anything else is borrowed through Iterable; the collection is not consumed | covered | impls/iter_for_borrows_collection |
| 4359 | The item is a ref: it cannot be moved out or written through | covered | impls/for_item_move_out_refused, for_item_write_refused (new) |
| 4361 | An Iterator is not Iterable; `by_ref()` keeps it usable | covered | impls/iter_iterator_not_iterable_refused, impls/iter_for_borrows_collection |
| 4363 | `.into_iter()` moves the collection | covered | impls/iter_map_into_iter_moves_refused |
| 4363 | A read-once source is IntoIterator but not Iterable | covered | impls/iter_for_receiver_needs_into_iter_refused |
| 4370 | Neither: error naming the missing impl; suggests `.into_iter()` when IntoIterator | covered | impls/iter_for_neither_refused, impls/iter_for_receiver_needs_into_iter_refused |
| 4372 | `for (k, v) in map` borrows: k and v are refs | partial | dyn_effect_variable_router (tuple destructuring over a Vec iter); Map case untested |

## Derive

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4393 | Deriving is explicit; each derive is an ordinary impl | covered | impls/derive_and_manual_impl_refused |
| 4404 | PartialEq, Eq, Hash, Display derives | covered | impls/derive_basic_point |
| 4408 | Derived PartialOrd/Ord compare lexicographically in field order | covered | impls/derive_basic_point |
| 4415 | Arithmetic derives on distinct types only | covered | impls/derive_arithmetic_struct_refused (new) |
| 4418 | A missing supertrait derive is an error with a machine-applicable fix (0.8.x fill-in plus warning) | partial | impls/derive_eq_without_partialeq, impls/derive_ord_missing_supertraits, impls/copy_without_clone (.out only; no .fixed) |
| 4418 | The order of the derive list does not matter | covered | impls/derive_order_does_not_matter |
| 4420 | PartialOrd needs PartialEq | covered | impls/derive_partialord_without_partialeq (new) + derive_implied_traits_fixed |
| 4424 | Default needs no other derive | covered | impls/default_derive_struct_and_enum |
| 4426 | A field lacking the trait: 0.8.x deprecated warning; Clone/Debug/Display still work by shape | partial | impls/derive_field_lacks_clone_deprecated (Clone only; warning not asserted) |
| 4426 | A Secret field meets Debug and Display, rendering `<redacted>` | partial | conformance/secret/redacted_and_access (outside the three dirs) |
| 4426 | Serialize/Deserialize on a field lacking the trait is refused, naming field and type | covered | impls/derive_serialize_field_lacks_trait_refused (Deserialize side untested) |
| 4428 | Bounds propagate on generic types | covered | impls/derive_adds_bounds |
| 4430 | A derive and a manual impl of one trait is the duplicate-impl error | covered | impls/derive_and_manual_impl_refused |

## Default and #[default]

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4436 | Derived Default on a struct builds from each field's default | covered | impls/default_derive_struct_and_enum |
| 4445 | Field defaults: 0, false, "", empty collection, None | covered | impls/default_derive_struct_and_enum |
| 4445 | A tuple field gets its parts' defaults; `Array[T, N]` gets N defaults | covered | impls/default_tuple_and_array_fields (new) |
| 4445 | E_DERIVE_DEFAULT_MISSING_FIELD_DEFAULT, field labelled | covered | impls/default_missing_field_default_refused |
| 4445 | On Pair[T] the derive is bounded by T: Default | covered | impls/default_generic_bound_refused, impls/default_derive_struct_and_enum |
| 4447 | On an enum, the one marked unit variant is the default | covered | impls/default_derive_struct_and_enum |
| 4463 | No variant marked: E_DEFAULT_NO_VARIANT_MARKED | covered | impls/default_no_variant_marked_refused |
| 4464 | Several marked: E_DEFAULT_MULTIPLE_VARIANTS | covered | impls/default_multiple_variants_refused |
| 4465 | Marked variant has fields: E_DEFAULT_VARIANT_HAS_PAYLOAD | covered | impls/default_variant_has_payload_refused |
| 4468 | `#[default]` anywhere else is E_DEFAULT_ATTRIBUTE_INVALID_POSITION | partial | impls/default_attr_on_field_refused, _on_fn_, _on_struct_ (alias, trait, impl, constant, extern untested) |
| 4469 | `#[default(...)]` is malformed | covered | impls/default_attr_with_args_refused |
| 4470 | On an enum without the derive: E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE, fix adds the derive | covered | impls/default_attr_without_derive_refused (.fixed) |
| 4472 | Independent of #[non_exhaustive] and explicit discriminants | covered | impls/default_derive_struct_and_enum |

## Serialize and Deserialize

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 4476 | The derive walks fields in declaration order | covered | impls/serde_field_attributes |
| 4476 | A generic type's derived impl bounds T by the trait | partial | impls/serde_generic_and_str_input (accepted only; Wrap[NoSer] refusal untested) |
| 4485 | Json.serialize / Json.deserialize, the annotation selects the type | covered | impls/serde_* |
| 4488 | A format implements Serializer/Deserializer; derived code works with any format | covered | impls/serde_user_serializer (new) |
| 4548 | A format implements every method without a default | partial | required_method_missing_refused (generic rule) |
| 4552 | Effects come from the format | covered | impls/serde_effects_from_format(_refused) (new) |
| 4568 | rename, skip, skip_serializing, skip_deserializing, default | covered | impls/serde_field_attributes |
| 4568 | `default` alone, or a skipped field, uses the type's Default | covered | impls/serde_field_attributes |
| 4568 | An unknown key is an error with a did-you-mean | partial | impls/serde_unknown_key_refused (field position only) |
| 4568 | A field skipped in one direction needs no trait for that direction | covered | impls/serde_skipped_field_needs_no_trait |
| 4568 | A skipped field read back takes its default or its type's Default, which it must have | covered | impls/serde_skipped_field_needs_default_refused |
| 4569 | Enums are externally tagged by default | covered | impls/serde_enum_tagging (new) |
| 4569 | `tag:` makes enums internally tagged and `untagged` untagged | covered | impls/serde_enum_tagging (new) |
| 4569 | An internally tagged enum holds unit and struct variants only | covered | impls/serde_internal_tag_tuple_variant_refused |
| 4569 | `rename` applies to a variant | covered | impls/serde_generic_and_str_input |
| 4570 | `default: expr` takes a constant expression; a run-time one is refused | covered | impls/serde_default_constant_forms, impls/serde_default_not_constant_refused |
| 4571 | A tuple field is a sequence of exactly its length | covered | impls/serde_tuple_field |
| 4571 | A tuple inside another type is refused by field name | covered | impls/serde_tuple_inside_vec_refused; a library type with no impl, impls/serde_field_without_library_impl_refused |
| 4572 | A duplicate field is refused | covered | impls/serde_missing_field_by_renamed_key |
| 4573 | A missing field is reported by its renamed key | covered | impls/serde_missing_field_by_renamed_key |
| 4574 | `Json.deserialize(text: Str)` reads a view | covered | impls/serde_generic_and_str_input |
| 4575 | A distinct type is its base on the wire | covered | impls/serde_distinct_base_on_wire |
