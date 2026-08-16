//! Raw-pointer instance-method typechecking.
//!
//! Tenth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! instance methods on a `*const T` / `*mut T` receiver (design.md § raw
//! pointers) — the additive surface that complements the free-function
//! `ptr` module handled in `method_identifier_receiver.rs`.
//!
//! The block order is load-bearing — `infer_method_call` is a
//! first-match-wins chain, so these guards keep the exact relative order
//! they had inline, and the function is called from the same position in
//! that chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::types::{IntSize, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type an instance method on a raw-pointer receiver.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_pointer_receiver_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // Raw-pointer instance methods (design.md § raw pointers; additive-
        // interop Slice 4 Path A). Record the receiver's pointee keyed by
        // the call span BEFORE the method's result type overwrites the
        // receiver's `*T` entry at the same (collided) span key — codegen
        // needs the receiver pointee for the GEP/load/store, and `.read` /
        // `.write` results (`T` / unit) are not pointers. Mirrors the
        // `vector_method_receivers` fix for the same span collision.
        // Raw-pointer inherent methods (`*const T` / `*mut T`) — design.md
        // § "Method dispatch on raw pointers requires a known pointee". These are
        // inherent methods on the pointer type itself (no auto-deref to `T`), so
        // they must be resolved here BEFORE the generic dispatch below (which
        // would return `Type::Error` for a pointer receiver, silently degrading a
        // `let p1 = p.offset(1)` intermediate to `Error` and breaking every
        // downstream `p1.read()` — B-fixed here). `unsafe { }` is enforced
        // separately by `unsafe_lint`; the pointee side-table feeds codegen.
        if let Type::Pointer { inner, is_mut } = obj_ty {
            let is_mut = *is_mut;
            let inner_ty = (**inner).clone();
            // Record the pointee for every raw-pointer method so codegen can
            // (a) identify the receiver as a raw pointer — the method-call span
            // equals the receiver span, and the call's result type overwrites the
            // receiver's `*T` entry in `expr_types`, so this side-table is how
            // `compile_pointer_instance_method` recovers the pointer-ness — and
            // (b) size its typed load/store/GEP. `is_null` ignores the pointee in
            // its lowering (a null-bits check) and stays SAFE (no `unsafe { }`,
            // matching `ptr.is_null(p)`), but still records it for (a).
            let is_ptr_method = matches!(
                method,
                "offset"
                    | "add"
                    | "read"
                    | "read_unaligned"
                    | "read_volatile"
                    | "write"
                    | "write_unaligned"
                    | "write_volatile"
                    | "is_null"
            );
            if is_ptr_method {
                let pointee = Self::type_to_type_expr(&inner_ty);
                self.pointer_method_receiver_pointees
                    .insert(SpanKey::from_span(span), pointee);
            }
            // `E_RAW_POINTER_UNRESOLVED_POINTEE` (design.md § "Method dispatch on
            // raw pointers requires a known pointee"). A size/stride-dependent
            // method (`read`/`write`/`offset`/`add` + unaligned/volatile) cannot
            // be lowered when `T` is unresolved — the load/store width and GEP
            // stride depend on it. `is_null` is EXEMPT (a null-bits check reads no
            // pointee, so it accepts an unresolved `T`). A generic parameter `T`
            // that is IN SCOPE (`fn f[T](p: *const T) { p.read() }`) is resolved
            // per-instantiation at monomorphization and does NOT fire — that is
            // exactly what `find_unbound_type_param` (which consults the enclosing
            // generic bounds) distinguishes from an un-pinned metavariable.
            // Emitted at the RECEIVER span (the user's question is "what type is
            // `p`?"), and returns `Type::Error` to suppress the cascade (the
            // binding-level infer error is already suppressed for the pointer
            // construction — see `check_unsolved_type_param`).
            let sized_ptr_op = matches!(
                method,
                "offset"
                    | "add"
                    | "read"
                    | "read_unaligned"
                    | "read_volatile"
                    | "write"
                    | "write_unaligned"
                    | "write_volatile"
            );
            if sized_ptr_op {
                let unresolved: Option<String> = {
                    let in_scope: std::collections::HashSet<&str> =
                        self.enclosing_bounds.keys().map(|s| s.as_str()).collect();
                    super::inference::find_unbound_type_param(&inner_ty, &in_scope)
                        .map(|s| s.to_string())
                };
                if let Some(pointee_name) = unresolved {
                    self.type_error(
                        format!(
                            "error[E_RAW_POINTER_UNRESOLVED_POINTEE]: method '{method}' on a \
                             raw pointer requires a known pointee type; the pointee type \
                             '{pointee_name}' is unresolved at this call site. Annotate the \
                             pointer's declared type (e.g. `let p: *const u8 = ...`), or pin it \
                             with a turbofish on the originating constructor (e.g. \
                             `ptr.null[u8]()`)."
                        ),
                        object.span,
                        TypeErrorKind::CannotInferTypeParam,
                    );
                    return Some(Type::Error);
                }
            }
            let arg_count_ok = |s: &mut Self, want: usize| -> bool {
                if args.len() != want {
                    s.type_error(
                        format!(
                            "'{method}' on a raw pointer takes {want} argument{}",
                            if want == 1 { "" } else { "s" }
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    return false;
                }
                true
            };
            match method {
                // `p.offset(n) / p.add(n) -> *_ T` — same pointee + mutability.
                "offset" | "add" => {
                    if arg_count_ok(self, 1) {
                        self.check_expr(&args[0].value, &Type::Int(IntSize::I64));
                    }
                    return Some(Type::Pointer {
                        is_mut,
                        inner: Box::new(inner_ty),
                    });
                }
                // `p.read() / read_unaligned() / read_volatile() -> T`.
                "read" | "read_unaligned" | "read_volatile" => {
                    arg_count_ok(self, 0);
                    return Some(inner_ty);
                }
                // `p.write(v) / write_unaligned(v) / write_volatile(v) -> Unit`,
                // with `v: T`.
                "write" | "write_unaligned" | "write_volatile" => {
                    if arg_count_ok(self, 1) {
                        self.check_expr(&args[0].value, &inner_ty);
                    }
                    return Some(Type::Unit);
                }
                // `p.is_null() -> bool` — the method-form of `ptr.is_null(p)`
                // (design.md § raw pointers). SAFE and pointee-agnostic.
                "is_null" => {
                    arg_count_ok(self, 0);
                    return Some(Type::Bool);
                }
                _ => {}
            }
        }
        None
    }
}
