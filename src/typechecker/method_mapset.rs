//! `Map` / `Set` and named-container method typechecking.
//!
//! Thirteenth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! surfaces dispatched on `obj_ty_for_named` — the named-type view of the
//! receiver computed by the caller's normalization preamble:
//!
//! - `Map[K, V]` and its `Entry` API, `SortedMap`, `SortedSet[T]` and
//!   `Set[T]`, whose type parameters thread through the *return* types, so
//!   each result is computed from the receiver rather than read off a
//!   baked signature;
//! - the `Regex`, `CStr` / `CString` and HTTP (`Client`, `Response`,
//!   `HttpError`, `RequestBuilder`) named-type surfaces, each delegating
//!   to its own `infer_*_method` helper.
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

use super::expr_method_call::{MAP_BUILTIN_METHODS, SET_BUILTIN_METHODS};
use super::types::Type;

impl<'a> super::TypeChecker<'a> {
    /// Type a call on a named container / stdlib type.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_mapset_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        obj_ty_for_named: &Type,
    ) -> Option<Type> {
        // `Map[K, V]` method dispatch. K and V thread through return types.
        if let Type::Named {
            name,
            args: type_args,
        } = obj_ty_for_named
        {
            if name == "Map" {
                // B-2026-08-12-34 — a USER trait impl on `Map` gets the names
                // the builtin surface does not have, exactly as the `String`
                // gate above does, and with the same precedence: builtin first,
                // so `m.get(k)` cannot be hijacked. Without this the early
                // return below is unconditional and the call reports
                // `no method '<m>' on type 'Map'` even though the impl
                // registered (a `Map` is `Type::Named`, so unlike `String` it
                // was never the REGISTRATION that failed — only dispatch).
                if !MAP_BUILTIN_METHODS.contains(&method)
                    && !self
                        .env
                        .find_methods_with_args(name, type_args, method)
                        .is_empty()
                {
                    // fall through to the impl-table dispatch below
                } else {
                    let key = type_args.first().cloned().unwrap_or(Type::Error);
                    let val = type_args.get(1).cloned().unwrap_or(Type::Error);
                    return Some(self.infer_map_method(&key, &val, method, args, span));
                }
            }
        }

        // `Entry[K, V]` method dispatch — `or_insert`, `or_insert_with`,
        // `and_modify`. Produced by `Map.entry(k)`.
        if let Type::Named {
            name,
            args: type_args,
        } = obj_ty_for_named
        {
            if name == "Entry" {
                let key = type_args.first().cloned().unwrap_or(Type::Error);
                let val = type_args.get(1).cloned().unwrap_or(Type::Error);
                return Some(self.infer_entry_method(&key, &val, method, args, span));
            }
        }

        // `SortedSet[T]` method dispatch. Named type but with dedicated
        // per-method typing (generic T threads through return types).
        if let Type::Named {
            name,
            args: type_args,
        } = obj_ty_for_named
        {
            if name == "SortedSet" {
                let element = type_args.first().cloned().unwrap_or(Type::Error);
                return Some(self.infer_sorted_set_method(&element, method, args, span));
            }
            if name == "SortedMap" {
                let key = type_args.first().cloned().unwrap_or(Type::Error);
                let value = type_args.get(1).cloned().unwrap_or(Type::Error);
                return Some(self.infer_sorted_map_method(&key, &value, method, args, span));
            }
            if name == "Set" {
                // B-2026-08-12-34 — the `Set` peer of the `Map` gate above,
                // same precedence rule: builtin names first, impl table only
                // for what the builtin surface does not answer.
                if !SET_BUILTIN_METHODS.contains(&method)
                    && !self
                        .env
                        .find_methods_with_args(name, type_args, method)
                        .is_empty()
                {
                    // fall through to the impl-table dispatch below
                } else {
                    let element = type_args.first().cloned().unwrap_or(Type::Error);
                    return Some(self.infer_set_method(&element, method, args, span));
                }
            }
        }

        // `Regex` method dispatch.
        if let Type::Named { name, .. } = obj_ty_for_named {
            if name == "Regex" {
                return Some(self.infer_regex_method(method, args, span));
            }
        }

        // `CStr` method dispatch — the `c"..."` literal types as `ref CStr`
        // (see `infer_expr_inner`'s CStringLit arm), so the deref'd
        // named-receiver shape lands here. `as_ptr` / `len` / `is_empty` /
        // `as_bytes` per design.md § C-String Literals. The
        // `method_callee_types` insert mirrors the HTTP arm below: CStr
        // dispatches through a hardcoded arm (no impl block), and codegen's
        // `compile_method_call` keys its CStr routing off the recorded
        // `CStr.<method>` — without it, dispatch falls through to the
        // user-impl-block lookup, which errors.
        if let Type::Named { name, .. } = obj_ty_for_named {
            if name == "CStr" {
                self.method_callee_types
                    .insert(SpanKey::from_span(span), format!("CStr.{}", method));
                return Some(self.infer_cstr_method(method, args, span));
            }
        }

        // `CString` method dispatch — the owning C-string produced by
        // `String.to_cstring()` (design.md § C-String Literals). Same hardcoded-
        // arm pattern as `CStr`: record the `CString.<method>` callee so codegen
        // routes it (no impl block backs the type), then infer via
        // `infer_cstring_method` (`as_ptr` / `len` / `is_empty` / `as_bytes`).
        if let Type::Named { name, .. } = obj_ty_for_named {
            if name == "CString" {
                self.method_callee_types
                    .insert(SpanKey::from_span(span), format!("CString.{}", method));
                return Some(self.infer_cstring_method(method, args, span));
            }
        }

        // `Client` / `Response` / `HttpError` / `RequestBuilder` method dispatch.
        if let Type::Named { name, .. } = obj_ty_for_named {
            if matches!(
                name.as_str(),
                "Client" | "Response" | "HttpError" | "RequestBuilder"
            ) {
                // Record the precise `Type.method` callee for this call site.
                // These HTTP types dispatch through a hardcoded arm (not the
                // resolved-method path), so without this insert the effect
                // checker can't reach the `sends(Network)`/`receives(Network)`
                // seeds for `Client.get` / `Client.post` / `RequestBuilder.send`
                // — the call site would resolve to no precise key and the
                // name-only heuristics can't distinguish `client.get()` from
                // `map.get()`. Mirrors the resolved-method insert above.
                self.method_callee_types
                    .insert(SpanKey::from_span(span), format!("{}.{}", name, method));
            }
            match name.as_str() {
                "Client" => return Some(self.infer_http_client_method(method, args, span)),
                "Response" => return Some(self.infer_http_response_method(method, args, span)),
                "HttpError" => return Some(self.infer_http_error_method(method, args, span)),
                "RequestBuilder" => {
                    return Some(self.infer_http_request_builder_method(method, args, span))
                }
                _ => {}
            }
        }
        None
    }
}
