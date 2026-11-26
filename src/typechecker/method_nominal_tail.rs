//! Nominal-receiver method typechecking — the last guards before user-`impl` dispatch.
//!
//! Eleventh slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! final run of built-in guards, all keyed on the *normalized* receiver
//! (`receiver_for_lookup`) rather than the raw one:
//!
//! - distinct-type `.raw()` unwrap and the no-deref rule (design.md
//!   § Distinct types);
//! - `cmp` — the `Ordering`-returning comparison;
//! - `to_string()` — on `String` (identity copy), on any
//!   `#[derive(Display)]` / `impl Display` struct, and on an all-unit
//!   `#[derive(Display)]` enum;
//! - the tuple-receiver surface.
//!
//! These sit immediately before `dispatch_user_impl_method`, so they are
//! the last chance for a built-in to claim the call. Their order relative
//! to each other and to that terminal arm is load-bearing.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::types::{type_display, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type a nominal-receiver built-in method — the last guards before
    /// user-`impl` dispatch.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` to fall
    /// through to `dispatch_user_impl_method`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_nominal_tail_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        receiver_for_lookup: &Type,
        borrowed: bool,
    ) -> Option<Type> {
        // Distinct-type `.raw()` unwrap + no-deref rule (design.md § Distinct
        // Types). A distinct type flows as a nominal `Type::Named { name }`;
        // its built-in `.raw()` returns the underlying base value (recovered
        // from `env.distinct_bases`). Every *other* method resolves only
        // through inherent impls on the distinct type itself — distinct types
        // do not deref to their base (method-resolution rule 5), so a base
        // method like `i64.abs()` is not callable on a `UserId`. Non-`raw`
        // methods fall through to the generic impl search below; if none
        // matches, the bottom-of-function `NoMethodFound` fires (distinct
        // names are folded into `is_user_defined` there).
        if let Type::Named { .. } = receiver_for_lookup {
            if let Some(base) = self.env.distinct_base_of(receiver_for_lookup) {
                if method == "raw" {
                    if !args.is_empty() {
                        self.type_error(
                            format!("'.raw()' takes no arguments, found {}", args.len()),
                            *span,
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                    }
                    // Of a borrowed value, a borrow of its base, unless the
                    // base is `Copy`: `self.raw()[0]` under a bare `self`
                    // reads through it (B-2026-11-04-48).
                    if !self.is_copy_type_during_check(&base) {
                        if borrowed {
                            return Some(Type::Ref(Box::new(base)));
                        }
                        self.distinct_raw_receivers
                            .insert(crate::resolver::SpanKey::from_span(&object.span));
                    }
                    return Some(base);
                }
            }
        }
        // `.cmp(other)` on a `#[derive(Ord)]` struct/enum returns `Ordering` —
        // the method form of the `<`/`>` operators, which already work for such
        // types via the lexicographic `karac_cmp_<T>` comparator (codegen) /
        // `value_compare` (interpreter). The derive registers NO `cmp` entry in
        // `env.impls`, so without this intercept a Named receiver falls through
        // to the `NoMethodFound` arm ("no method 'cmp' on type 'P'"), breaking
        // `p.cmp(q)`, `min`/`max`/`clamp` on struct/enum types (their bodies
        // call `a.cmp(b)`), sorting, and any `fn f[T: Ord]` body. Gated to the
        // DERIVED case (`!has_user_impl_ord`) so a hand-written `impl Ord` still
        // resolves through the normal impl-table path. Mirrors the String `cmp`
        // handler (`stdlib_seq.rs`) and the primitive Ord builtin-impl
        // (`env_build.rs`). roadmap Phase 8 § Eq/Ord.
        //
        // B-2026-08-27-41 — a TUPLE receiver claims `cmp` on the same terms.
        // `type_supports_ord` already recurses through `Type::Tuple`, and the
        // `<`/`>` operators on a tuple already lower through the very same
        // comparator (B-2026-08-27-33), so the only thing that kept
        // `(1, 2).cmp((1, 3))` out was this arm's `Type::Named` gate — which
        // rejected it with "no method 'cmp' on type '(i64, i64)'" while the
        // operator spelling of the identical comparison compiled and ran.
        //
        // The derive gate does NOT carry over, and its absence is not an
        // oversight: `!has_user_impl_ord` exists so a HAND-WRITTEN `impl Ord`
        // is not silently overruled by the declaration-order comparator, and a
        // tuple is structural — it has no name to hang an impl on, so there is
        // no user ordering to overrule. Left-to-right IS the language's tuple
        // order. Same reasoning the operator arm in `exprs.rs` records for
        // skipping its own derive check on tuples.
        if method == "cmp" {
            let claims_cmp = match receiver_for_lookup {
                // A hand-written `impl Ord` owns `.cmp` on its own type.
                Type::Named { name, .. } => {
                    self.type_supports_ord(receiver_for_lookup) && !self.has_user_impl_ord(name)
                }
                Type::Tuple(elems) => {
                    !elems.is_empty() && self.type_supports_ord(receiver_for_lookup)
                }
                _ => false,
            };
            if claims_cmp {
                if args.len() != 1 {
                    self.type_error(
                        format!("'cmp' expects 1 argument, found {}", args.len()),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                } else {
                    // `other` is read (`fn cmp(ref self, other: ref Self)`,
                    // ord.kara): a bare parameter is compared as a local is
                    // (B-2026-11-26-70).
                    let arg_ty = self.infer_expr(&args[0].value);
                    let arg_ty = crate::typechecker::peel_probe_ref(&arg_ty);
                    self.check_assignable(receiver_for_lookup, &arg_ty, args[0].value.span);
                }
                return Some(Type::Named {
                    name: "Ordering".to_string(),
                    args: Vec::new(),
                });
            }
        }
        // `a.eq(b)` and `a.partial_cmp(b)` on a type whose `PartialEq` /
        // `PartialOrd` is derived: the methods design.md § Operator traits
        // declares (`fn eq(self, other: Rhs) -> bool`, `fn partial_cmp(self,
        // other: Rhs) -> Option[Ordering]`), which `==` and `<` already are
        // (B-2026-10-31-24). A hand-written impl owns its method through the
        // impl table, so only a type with no such method of its own is
        // claimed here, on the terms `cmp` is above.
        if let ("eq" | "partial_cmp", [other]) = (method, args) {
            let claims = match receiver_for_lookup {
                Type::Named { name, .. } => {
                    self.env
                        .find_methods_with_args(name, &[], method)
                        .is_empty()
                        && match method {
                            "eq" => self.type_supports_partial_eq(receiver_for_lookup),
                            _ => self.type_supports_partial_ord(receiver_for_lookup),
                        }
                }
                _ => false,
            };
            if claims {
                let arg_ty = self.infer_expr(&other.value);
                let arg_ty = crate::typechecker::peel_probe_ref(&arg_ty);
                self.check_assignable(receiver_for_lookup, &arg_ty, other.value.span);
                return Some(match method {
                    "eq" => Type::Bool,
                    _ => Type::Named {
                        name: "Option".to_string(),
                        args: vec![Type::Named {
                            name: "Ordering".to_string(),
                            args: Vec::new(),
                        }],
                    },
                });
            }
        }
        // Opaque foreign types have no methods by definition — impl blocks
        // on them are rejected at `E_OPAQUE_TYPE_NO_INHERENT_OR_TRAIT_IMPLS`,
        // so the generic "method not found" diagnostic that would otherwise
        // fire from the fallthrough at the bottom of this function is
        // technically true but misleading. Emit the focused
        // `E_OPAQUE_TYPE_NO_METHODS` instead so the programmer is steered
        // toward the wrapper-type / free-function pattern.
        if let Type::Named { name, .. } = receiver_for_lookup {
            if self.env.opaque_foreign_types.contains(name) {
                self.type_error(
                    format!(
                        "error[E_OPAQUE_TYPE_NO_METHODS]: opaque foreign type \
                         '{name}' has no methods — impl blocks on opaque types \
                         are rejected, so no '.{method}(…)' or any other method \
                         call resolves through '{name}'. Use the wrapper-type \
                         pattern (`distinct type Wrapper = *mut {name}; impl Wrapper {{ … }}`) \
                         or call a free function from the `unsafe extern \"C\" {{ … }}` \
                         block that takes `ref {name}` / `mut ref {name}`."
                    ),
                    *span,
                    TypeErrorKind::NoMethodFound,
                );
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                return Some(Type::Error);
            }
        }
        // Scalar-primitive receivers (`Int` / `UInt` / `Float` / `Char`) —
        // `abs`, the `float_math` table, the bit-width converters and bit
        // intrinsics, the wrapping/saturating/checked/overflowing families,
        // `pow`, `min`/`max`/`clamp`, `abs_diff`, the rotates, and the `char`
        // surface. Extracted to `method_numeric.rs`; it keeps this position in
        // the first-match-wins chain and the block order within it.
        if let Some(t) = self.try_scalar_primitive_method(
            object,
            method,
            args,
            span,
            args_close_span,
            receiver_for_lookup,
        ) {
            return Some(t);
        }
        // `to_string()` on `String` (identity copy), on any `#[derive(Display)]`
        // / `impl Display` **struct**, and on an all-unit `#[derive(Display)]`
        // **enum** → `String`. The `Display` trait provides
        // `to_string(ref self) -> String` (design.md § Display); this types the
        // explicit call so it stops poisoning to `Type::Error`. Codegen renders
        // structs in declaration order and all-unit enums as the bare variant
        // name (`synth_display`). Payload-bearing enums and `#[display_snake_case]`
        // enums are excluded — their codegen renderer is a follow-on, so leaving
        // `to_string` untyped keeps a clean typecheck rejection rather than a
        // codegen failure (interp still renders them under `karac run`).
        if method == "to_string" && args.is_empty() {
            let is_display_named = match receiver_for_lookup {
                Type::Str => true,
                Type::Named { name, .. } if name == "String" => true,
                // Collections render in codegen (`try_compile_collection_display`)
                // when their element/key/value types are `Display`.
                Type::Named { name, .. }
                    if matches!(name.as_str(), "Vec" | "VecDeque" | "Map" | "Set") =>
                {
                    self.type_supports_display(receiver_for_lookup)
                }
                Type::Named { name, .. } => {
                    let struct_display = self
                        .env
                        .structs
                        .get(name)
                        .map(|s| s.derived_traits.contains("Display"))
                        .unwrap_or(false)
                        || (self.env.structs.contains_key(name)
                            && self.env.impls.iter().any(|i| {
                                i.target_type == *name && i.trait_name.as_deref() == Some("Display")
                            }));
                    // Payload-bearing `#[derive(Display)]` enums now render
                    // under codegen exactly as f-string interpolation does
                    // (`Other(disk full)` etc.) — the payload-enum Display
                    // renderer that the old all-unit restriction waited on has
                    // landed, so explicit `.to_string()` types for them too
                    // (verified interp == JIT == AOT). `#[display_snake_case]`
                    // enums stay excluded pending their own renderer follow-on.
                    let enum_display = self
                        .env
                        .enums
                        .get(name)
                        .map(|e| {
                            e.derived_traits.contains("Display")
                                && !self.display_snake_case_enums.contains(name)
                        })
                        .unwrap_or(false);
                    struct_display || enum_display
                }
                // A tuple renders when every element does (B-2026-08-11-11).
                // `to_string` was the ONE method that worked on a tuple, but it
                // was not typed here — it fell through to the poison branch and
                // merely happened to have a runtime dispatch arm, so
                // `let n: i64 = t.to_string()` type-checked just as readily as
                // the `String` binding. Typing it is what lets the arm below
                // reject every other name without taking the one that works
                // down with it.
                Type::Tuple(elems) => elems.iter().all(|e| self.type_supports_display(e)),
                _ => false,
            };
            if is_display_named {
                return Some(Type::Str);
            }
        }
        // A TUPLE receiver reaching this point has no method (B-2026-08-11-11).
        // Tuples were the last receiver kind still on the unconditional
        // silent-poison fall-through that `char`/`bool` left in B-2026-08-11-2:
        // `method_callee_type_name` returns `None` for `Type::Tuple`, so the
        // scalar-primitive arm below skips them entirely and EVERY call
        // poisoned to `Type::Error` — universally assignable, so
        // `let s: String = t.bogus()` type-checked clean and the program then
        // died in the backend. Their whole surface is `to_string` (the intercept
        // directly above) and `cmp` (B-2026-08-27-41, further up), so anything
        // still here is a typo.
        //
        // This is placed AFTER every legitimate intercept rather than in the
        // arm below, because that arm is keyed on `method_callee_type_name`
        // and a tuple has no name to look up. Giving it one would also feed the
        // `method_callee_types` side-table that codegen's `dispatch_key` reads,
        // pointing dispatch at a name no impl can ever be registered under —
        // an early return keeps the fix inside the typechecker.
        // `.clone()` on a tuple whose elements all clone, element by element:
        // `type_supports_clone` already says so, and the `karac fix` edits
        // that write `.clone()` rely on it.
        if method == "clone"
            && args.is_empty()
            && matches!(receiver_for_lookup, Type::Tuple(elems) if !elems.is_empty())
            && self.type_supports_clone(receiver_for_lookup)
        {
            return Some(receiver_for_lookup.clone());
        }
        // A program's impl of a trait for this tuple type (`impl Show for
        // (i64, i64)`) answers its methods (B-2026-11-21-30).
        if let Type::Tuple(elems) = receiver_for_lookup {
            let impls: Vec<(String, Option<Span>)> = self
                .env
                .impls
                .iter()
                .filter(|i| {
                    i.target_type == "Tuple"
                        && (i.target_args == *elems
                            || (i.target_args.is_empty()
                                && super::env::impl_shape_fits(i, elems)
                                && self.env.impl_bounds_discharge(i, elems)))
                })
                .filter_map(|i| Some((i.trait_name.clone()?, i.target_span)))
                .collect();
            if !impls.is_empty() {
                // The impl whose method this is, keyed `(i64, i64).show`
                // as the effect checker and the ownership pass key a tuple
                // impl's methods (a tuple has no type name of its own).
                // MIR finds the method by the receiver's type instead.
                let owners: Vec<&(String, Option<Span>)> = impls
                    .iter()
                    .filter(|(t, _)| self.find_trait_method(t, method).is_some())
                    .collect();
                if let [(_, Some(at))] = owners.as_slice() {
                    let shown = self.program.items.iter().find_map(|it| match it {
                        Item::ImplBlock(b)
                            if matches!(b.target_type.kind, TypeKind::Tuple(_))
                                && b.target_type.span == *at =>
                        {
                            Some(crate::formatter::render_type_expr(&b.target_type))
                        }
                        _ => None,
                    });
                    if let Some(shown) = shown {
                        self.method_callee_types.insert(
                            crate::resolver::SpanKey::from_span(span),
                            format!("{shown}.{method}"),
                        );
                    }
                }
                return None;
            }
        }
        if matches!(receiver_for_lookup, Type::Tuple(_)) {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            self.type_error(
                format!(
                    "no method '{}' on type '{}'",
                    method,
                    type_display(receiver_for_lookup)
                ),
                *span,
                TypeErrorKind::NoMethodFound,
            );
            return Some(Type::Error);
        }
        // A RAW-POINTER receiver reaching this point has no method
        // (B-2026-11-21-80): the same silent-poison fall-through tuples left in
        // B-2026-08-11-11. `try_pointer_receiver_method` claims the pointer's
        // inherent surface early in the chain and returns `None` for every
        // other name, `method_callee_type_name` has no arm for `Type::Pointer`,
        // and the call poisoned to `Type::Error` with no diagnostic, so
        // `karac check` printed only MIR's root-less E0516 "a type that failed
        // to check is not lowered yet". Name the surface instead, and the
        // `ptr` functions an address-shaped name was probably reaching for.
        //
        // A program's impl of a trait for this pointer type (`impl Show for
        // *mut i64`) answers its methods first, as a tuple's does above
        // (B-2026-11-21-81), keyed `*mut i64.show` for the effect checker
        // and the ownership pass; MIR finds the method by the receiver's type.
        if let Type::Pointer { .. } = receiver_for_lookup {
            if let Some((key, key_args)) = super::types::impl_table_key(receiver_for_lookup) {
                let owners: Vec<Option<Span>> = self
                    .env
                    .impls
                    .iter()
                    .filter(|i| i.target_type == key && i.target_args == key_args)
                    .filter(|i| {
                        i.trait_name
                            .as_ref()
                            .is_some_and(|t| self.find_trait_method(t, method).is_some())
                    })
                    .map(|i| i.target_span)
                    .collect();
                if let [at] = owners.as_slice() {
                    let shown = at.and_then(|at| {
                        self.program.items.iter().find_map(|it| match it {
                            Item::ImplBlock(b)
                                if matches!(b.target_type.kind, TypeKind::Pointer { .. })
                                    && b.target_type.span == at =>
                            {
                                Some(crate::formatter::render_type_expr(&b.target_type))
                            }
                            _ => None,
                        })
                    });
                    if let Some(shown) = shown {
                        self.method_callee_types.insert(
                            crate::resolver::SpanKey::from_span(span),
                            format!("{shown}.{method}"),
                        );
                    }
                    return None;
                }
            }
        }
        if let Type::Pointer { .. } = receiver_for_lookup {
            for arg in args {
                self.infer_expr(&arg.value);
            }
            let mut msg = format!(
                "no method '{}' on type '{}'; a raw pointer's methods are \
                 `offset`, `add`, `read`, `read_unaligned`, `read_volatile`, \
                 `write`, `write_unaligned`, `write_volatile` and `is_null`",
                method,
                type_display(receiver_for_lookup)
            );
            if method.contains("addr") || method.contains("expose") {
                msg.push_str(
                    "; the address is a `ptr` function, not a method: `ptr.addr(p)` \
                     for the address bits, `ptr.expose(p)` / `ptr.expose_mut(p)` for an \
                     address that will be turned back into a pointer (design.md § Pointer provenance)",
                );
            } else if method == "clone" {
                msg.push_str("; a raw pointer is `Copy`, so use it directly");
            }
            self.type_error(msg, *span, TypeErrorKind::NoMethodFound);
            return Some(Type::Error);
        }
        None
    }
}

impl<'a> super::TypeChecker<'a> {
    /// A tuple receiver with the program's trait impls for tuples
    /// (B-2026-11-21-30): its elements resolved through the substitutions
    /// so far, and an element that is still an unsuffixed literal fixed
    /// to the matching impl's element type, as a literal argument is fixed
    /// by its parameter. `(1, 2).show()` beside `impl Show for (i64, i64)`
    /// is that impl's; beside impls for `(i32, i32)` and `(i64, i64)` the
    /// literals keep their default. Any other receiver is returned as is.
    pub(super) fn tuple_receiver_for_impls(&mut self, ty: Type) -> Type {
        let Type::Tuple(elems) = &ty else {
            return ty;
        };
        if !self.env.impls.iter().any(|i| i.target_type == "Tuple") {
            return ty;
        }
        let resolved: Vec<Type> = elems
            .iter()
            .map(|e| {
                super::inference::resolve_type_vars(
                    e,
                    &self.env.substitutions,
                    &std::collections::HashMap::new(),
                    &self.env.const_substitutions,
                    &std::collections::HashMap::new(),
                )
            })
            .collect();
        // The literal kind of each still-open element.
        let kinds: Vec<Option<super::inference::LiteralKind>> = resolved
            .iter()
            .map(|e| match e {
                Type::TypeVar(v) => self.body_literal_vars.iter().find_map(|s| {
                    let root = super::inference::resolve_type_var_top(
                        &Type::TypeVar(s.id),
                        &self.env.substitutions,
                    );
                    (root == Type::TypeVar(*v)).then_some(s.kind)
                }),
                _ => None,
            })
            .collect();
        let fits =
            |impl_elem: &Type, have: &Type, kind: Option<super::inference::LiteralKind>| match kind
            {
                Some(super::inference::LiteralKind::Int) => {
                    matches!(impl_elem, Type::Int(_) | Type::UInt(_))
                }
                Some(super::inference::LiteralKind::Float) => matches!(impl_elem, Type::Float(_)),
                None => impl_elem == have,
            };
        let candidates: Vec<Vec<Type>> = self
            .env
            .impls
            .iter()
            .filter(|i| {
                i.target_type == "Tuple"
                    && i.target_args.len() == resolved.len()
                    && i.target_args
                        .iter()
                        .zip(resolved.iter().zip(&kinds))
                        .all(|(ie, (h, k))| fits(ie, h, *k))
            })
            .map(|i| i.target_args.clone())
            .collect();
        let mut distinct: Vec<Vec<Type>> = Vec::new();
        for c in candidates {
            if !distinct.contains(&c) {
                distinct.push(c);
            }
        }
        let [chosen] = distinct.as_slice() else {
            return Type::Tuple(resolved);
        };
        for (e, c) in resolved.iter().zip(chosen) {
            if let Type::TypeVar(v) = e {
                self.env.substitutions.insert(*v, c.clone());
            }
        }
        Type::Tuple(chosen.clone())
    }
}

impl<'a> super::TypeChecker<'a> {
    /// A tuple whose elements include an open unsuffixed literal, with each
    /// such element at its default (`i64`, `f64`) and the rest resolved; or
    /// `None` when `ty` is not a tuple or the program has no tuple impl.
    pub(super) fn tuple_with_literal_defaults(&self, ty: &Type) -> Option<Type> {
        let Type::Tuple(elems) = ty else {
            return None;
        };
        if !self.env.impls.iter().any(|i| i.target_type == "Tuple") {
            return None;
        }
        let out = elems
            .iter()
            .map(|e| {
                let r = super::inference::resolve_type_vars(
                    e,
                    &self.env.substitutions,
                    &std::collections::HashMap::new(),
                    &self.env.const_substitutions,
                    &std::collections::HashMap::new(),
                );
                let Type::TypeVar(v) = r else { return r };
                let kind = self.body_literal_vars.iter().find_map(|s| {
                    let root = super::inference::resolve_type_var_top(
                        &Type::TypeVar(s.id),
                        &self.env.substitutions,
                    );
                    (root == Type::TypeVar(v)).then_some(s.kind)
                });
                match kind {
                    Some(super::inference::LiteralKind::Int) => {
                        Type::Int(super::types::IntSize::I64)
                    }
                    Some(super::inference::LiteralKind::Float) => {
                        Type::Float(super::types::FloatSize::F64)
                    }
                    None => r,
                }
            })
            .collect();
        Some(Type::Tuple(out))
    }
}
