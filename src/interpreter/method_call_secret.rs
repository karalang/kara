//! Native intercepts for `std.secret.Secret[T]`'s `#[compiler_builtin]` access
//! methods. `Secret` is a plain `Value::Struct { name: "Secret", fields }`; its
//! `expose` / `expose_mut` methods have no interpreter-registered body (baked
//! stdlib `#[compiler_builtin]` methods are skipped by `register_impl_methods`),
//! so they are dispatched here, ahead of the generic user-impl dispatch.

use crate::ast::CallArg;
use crate::interpreter::value::Value;
use crate::token::Span;

impl<'a> super::Interpreter<'a> {
    /// Intercept `Secret.expose` / `Secret.expose_mut` / `Secret.ct_eq`.
    /// Returns `None` for any non-`Secret` receiver (falls through to the next
    /// dispatch guard).
    pub(super) fn try_eval_secret_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
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
            // `.ct_eq(other) -> bool` — constant-time equality. Timing is
            // irrelevant in a tree-walk interpreter (there is no observable
            // wall-clock side-channel to protect); the contract this backend
            // upholds is the *boolean result*, which for equal byte contents is
            // ordinary value equality. Restricted to `Secret[String]` to match
            // codegen's v1 support, so both backends accept the same programs
            // (`karac_secret_ct_eq` in the runtime is the constant-time path).
            "ct_eq" => {
                let other = self.eval_expr_inner(&args.first()?.value);
                let self_inner = fields.get("inner")?;
                let other_inner = match &other {
                    Value::Struct {
                        name: n,
                        fields: of,
                    } if n == "Secret" => of.get("inner")?,
                    _ => return None,
                };
                match (self_inner, other_inner) {
                    (Value::String(a), Value::String(b)) => Some(Value::Bool(a == b)),
                    // Fail closed, matching codegen's compile-time rejection of a
                    // non-`String` inner — both backends reject the same programs.
                    _ => Some(self.record_runtime_error(
                        "Secret.ct_eq: only `Secret[String]` is supported in v1 \
                         (Vec[u8] / [u8; N] are planned)",
                        span,
                    )),
                }
            }
            _ => None,
        }
    }
}
