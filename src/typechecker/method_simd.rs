//! `Vector[T, N]` SIMD instance-method typechecking.
//!
//! Twelfth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! portable-SIMD instance surface (design.md § Portable SIMD): the
//! lane-wise operations, reductions and lane accessors on a
//! `Vector[T, N]` receiver, whose result types are computed from the
//! receiver's element type and lane count.
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

use super::types::Type;

impl<'a> super::TypeChecker<'a> {
    /// Type a `Vector[T, N]` SIMD instance method.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_simd_vector_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // `Vector[T, N]` instance-method dispatch (design.md § Portable SIMD).
        // Not a `Type::Named`, so handle before the named-type extraction.
        if let Type::Vector { element, lanes } = &obj_ty.clone() {
            // Record the receiver vector type for the SIMD scalarization
            // analysis before delegating — the method-call node is about to
            // overwrite this span in `expr_types` with the method's *result*
            // type (scalar for reductions), erasing the receiver's `(T, N)`.
            // See `TypeCheckResult::vector_method_receivers`.
            if let Some(n) = lanes.as_usize() {
                self.vector_method_receivers
                    .insert(SpanKey::from_span(span), ((**element).clone(), n));
            }
            return Some(self.infer_vector_method(element, lanes, method, args, span));
        }

        // Tensor shape-transform family — `iter_axis` / `reshape` /
        // `permute` / `slice` / `squeeze` (phase-11). Their result types
        // depend on the receiver's shape and the arguments' syntactic
        // form, so they aren't expressible in the baked stdlib signatures
        // and are computed before the impl-table search; `shape()` /
        // `rank()` keep flowing through normal impl dispatch. Typing
        // rules in `src/typechecker/expr_method_tensor.rs`.
        if matches!(
            method,
            "iter_axis" | "reshape" | "permute" | "slice" | "squeeze" | "transpose" | "matmul"
        ) {
            let tensor_args = match obj_ty {
                Type::Named { name, args } if name == "Tensor" => Some(args),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Tensor" => Some(args),
                    _ => None,
                },
                _ => None,
            };
            if let Some(tensor_args) = tensor_args.cloned() {
                return Some(self.infer_tensor_shape_method(method, &tensor_args, args, span));
            }
        }

        // Tensor reductions: `sum` / `mean` / `prod` / `min` / `max` collapse
        // the whole tensor to a scalar; `sum_axis(n)` / `mean_axis(n)` collapse
        // one axis, yielding a tensor of rank-1 lower. `mean`/`mean_axis`
        // always yield `f64`; the rest preserve the element type. Like the
        // shape family these can't be expressed in the baked signatures, so
        // they intercept before impl dispatch. Typing in
        // `src/typechecker/expr_method_tensor.rs`.
        if matches!(
            method,
            // B-2026-08-12-8: `range` (the baked `Reduce[T]::range` default,
            // `max - min`) is implemented at BOTH reduce dispatch sites —
            // codegen `column.rs`/tensor and the interpreter — but was never
            // listed here, so `karac check` rejected every program using it
            // and its E2E test only passed because the harness discarded
            // typecheck errors (B-2026-08-11-34). Same element-typed result as
            // `min`/`max`, which is what the two emitters subtract.
            "sum" | "mean" | "prod" | "min" | "max" | "range" | "sum_axis" | "mean_axis"
        ) {
            let tensor_args = match obj_ty {
                Type::Named { name, args } if name == "Tensor" => Some(args),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Tensor" => Some(args),
                    _ => None,
                },
                _ => None,
            };
            if let Some(tensor_args) = tensor_args.cloned() {
                // Record the receiver's ELEMENT type keyed by the reduction call
                // span so codegen can reduce over a NON-IDENTIFIER receiver — a
                // tensor-producing chain like `a.zip_with(b, f).sum()`
                // (B-2026-07-13-5 legs A/C). `MethodCall.span == receiver.span`
                // collapses the sum/zip_with/`a` spans into one, so
                // `expr_types` (hence `tensor_typed_exprs`) at that span holds
                // the OUTERMOST scalar reduce result, not the intermediate
                // Tensor — the element type is unrecoverable from the span
                // otherwise. Reuses `temp_recv_elem_types` (the fresh-temp
                // non-identifier collection-receiver element-type table); the
                // by-name codegen path ignores it (it uses `tensor_var_infos`),
                // so recording unconditionally is harmless. Only the scalar
                // full reductions codegen wires (`sum`/`mean`/`prod`/`min`/`max`)
                // are recorded; `sum_axis`/`mean_axis` (tensor result) stay on
                // the by-name path.
                if matches!(method, "sum" | "mean" | "prod" | "min" | "max" | "range")
                    && !tensor_args.is_empty()
                {
                    let elem_te = Self::type_to_type_expr(&tensor_args[0]);
                    self.temp_recv_elem_types
                        .insert(SpanKey::from_span(span), elem_te);
                }
                return Some(self.infer_tensor_reduce(method, &tensor_args, args, span));
            }
        }

        // Tensor broadcasting — `broadcast_add` / `broadcast_sub` /
        // `broadcast_mul` / `broadcast_div` apply a binary op with NumPy-style
        // shape broadcasting (size-1 dims expand; shapes align from the
        // right). The result shape depends on *both* operand shapes, so it's
        // computed here before impl dispatch, like the shape/reduce families.
        // Typing in `src/typechecker/expr_method_tensor.rs`.
        if matches!(
            method,
            "broadcast_add" | "broadcast_sub" | "broadcast_mul" | "broadcast_div"
        ) {
            let tensor_args = match obj_ty {
                Type::Named { name, args } if name == "Tensor" => Some(args),
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Tensor" => Some(args),
                    _ => None,
                },
                _ => None,
            };
            if let Some(tensor_args) = tensor_args.cloned() {
                return Some(self.infer_tensor_broadcast(method, &tensor_args, args, span));
            }
        }
        None
    }
}
