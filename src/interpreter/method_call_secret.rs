//! Native intercepts for `std.secret.Secret[T]`'s `#[compiler_builtin]` access
//! methods. `Secret` is a plain `Value::Struct { name: "Secret", fields }`; its
//! `expose` / `expose_mut` methods have no interpreter-registered body (baked
//! stdlib `#[compiler_builtin]` methods are skipped by `register_impl_methods`),
//! so they are dispatched here, ahead of the generic user-impl dispatch.

use crate::ast::CallArg;
use crate::interpreter::value::Value;
use crate::token::Span;

impl<'a> super::Interpreter<'a> {
    /// Intercept `Secret.expose` / `Secret.expose_mut`. Returns `None` for any
    /// non-`Secret` receiver (falls through to the next dispatch guard).
    pub(super) fn try_eval_secret_method(
        &mut self,
        method: &str,
        obj: &Value,
        _args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        let Value::Struct { name, fields } = obj else {
            return None;
        };
        if name != "Secret" {
            return None;
        }
        match method {
            // `.expose() -> ref T` hands back the inner value. The tree-walk
            // interpreter has no struct-field place-ref, so this clones — which
            // is observationally correct for a *read* borrow (a `ref T` is never
            // used to mutate). `.expose_mut()` (which must alias so a write flows
            // back) needs a field place-ref and lands in a follow-on slice;
            // until then it falls through to a clean "no such method" error in
            // both backends, matching codegen.
            "expose" => fields.get("inner").cloned(),
            _ => None,
        }
    }
}
