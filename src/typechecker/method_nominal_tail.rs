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
    pub(super) fn try_nominal_tail_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        receiver_for_lookup: &Type,
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
        if let Type::Named { name, .. } = receiver_for_lookup {
            if let Some(base) = self.env.distinct_bases.get(name).cloned() {
                if method == "raw" {
                    if !args.is_empty() {
                        self.type_error(
                            format!("'.raw()' takes no arguments, found {}", args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
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
        if method == "cmp" {
            if let Type::Named { name, .. } = receiver_for_lookup {
                let name = name.clone();
                if self.type_supports_ord(receiver_for_lookup) && !self.has_user_impl_ord(&name) {
                    if args.len() != 1 {
                        self.type_error(
                            format!("'cmp' expects 1 argument, found {}", args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                    } else {
                        let arg_ty = self.infer_expr(&args[0].value);
                        self.check_assignable(
                            receiver_for_lookup,
                            &arg_ty,
                            args[0].value.span.clone(),
                        );
                    }
                    return Some(Type::Named {
                        name: "Ordering".to_string(),
                        args: Vec::new(),
                    });
                }
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
                    span.clone(),
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
        // died in the backend. Their whole surface is `to_string`, typed by the
        // intercept directly above, so anything still here is a typo.
        //
        // This is placed AFTER every legitimate intercept rather than in the
        // arm below, because that arm is keyed on `method_callee_type_name`
        // and a tuple has no name to look up. Giving it one would also feed the
        // `method_callee_types` side-table that codegen's `dispatch_key` reads,
        // pointing dispatch at a name no impl can ever be registered under —
        // an early return keeps the fix inside the typechecker.
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
                span.clone(),
                TypeErrorKind::NoMethodFound,
            );
            return Some(Type::Error);
        }
        None
    }
}
