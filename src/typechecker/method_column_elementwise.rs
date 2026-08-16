//! `Column` element-wise and closure-taking method typechecking.
//!
//! Fifth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! phase-11 `Column` methods whose result type mentions the impl
//! type-param `T`, which baked-signature dispatch does not bind from the
//! receiver — so each is computed here from the receiver's element type:
//!
//! - `iter` -> `Vec[Option[T]]`, `iter_valid` -> `Vec[T]`,
//!   `fillna(value)` / `dropna` -> `Column[T]`;
//! - `fold(init, f)` — the seeded reduction;
//! - `map(f)` — the element-wise map, including its `Option` / `Result`
//!   and `Tensor` broadcast forms.
//!
//! `iter` in particular must intercept *before* the generic iterator
//! surface, which is why this sits where it does in the chain.
//!
//! The block order is load-bearing — `infer_method_call` is a
//! first-match-wins chain, so these guards keep the exact relative order
//! they had inline, and the function is called from the same position in
//! that chain (immediately before the `Option` / `Result` combinators).
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use crate::resolver::SpanKey;

use super::inference::{resolve_type_var_top, unify_types};
use super::types::{type_display, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type a `Column` element-wise / closure-taking method.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_column_elementwise_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // Column[T] result-typed methods (phase-11 Arrow): `iter` ->
        // `Vec[Option[T]]`, `iter_valid` -> `Vec[T]`, `fillna(value)` /
        // `dropna` -> `Column[T]`. Their result type mentions the impl
        // type-param `T`, which baked-signature dispatch doesn't bind from
        // the receiver, so it's computed here (binding `T` from the
        // receiver's element type) — and `iter` must intercept *before* the
        // generic `iter()` iterator-source handler just below would claim
        // it. `len`/`null_count`/`valid_count`/`is_null`/`push`/`push_null`
        // keep flowing through normal baked dispatch (their result types are
        // concrete).
        if matches!(method, "iter" | "iter_valid" | "fillna" | "dropna") {
            let column_elem = match obj_ty {
                Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                    Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                        Some(args[0].clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(elem) = column_elem {
                // `fillna` takes the fill `value` plus an optional
                // `treat_nan_as_null: bool` (1 or 2 args, the flag default
                // `false`); the rest take none.
                if method == "fillna" {
                    if args.is_empty() || args.len() > 2 {
                        self.type_error(
                            format!("fillna expects 1 or 2 argument(s), got {}", args.len()),
                            span.clone(),
                            TypeErrorKind::WrongNumberOfArgs,
                        );
                        return Some(Type::Error);
                    }
                } else if !args.is_empty() {
                    self.type_error(
                        format!("{method} expects 0 argument(s), got {}", args.len()),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    return Some(Type::Error);
                }
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                // The `treat_nan_as_null` flag (labeled, or the 2nd
                // positional arg) must be a bool — the only statically
                // checkable arg, since the fill `value`'s type `T` isn't
                // bound from the receiver for baked generic methods.
                if method == "fillna" {
                    if let Some(flag) = args
                        .iter()
                        .find(|a| a.label.as_deref() == Some("treat_nan_as_null"))
                        .or_else(|| args.iter().filter(|a| a.label.is_none()).nth(1))
                    {
                        let flag_ty = self.infer_expr(&flag.value);
                        self.check_assignable(&Type::Bool, &flag_ty, flag.value.span.clone());
                    }
                }
                let vec_of = |inner: Type| Type::Named {
                    name: "Vec".to_string(),
                    args: vec![inner],
                };
                return Some(match method {
                    "iter" => vec_of(Type::Named {
                        name: "Option".to_string(),
                        args: vec![elem],
                    }),
                    "iter_valid" => vec_of(elem),
                    // fillna / dropna
                    _ => Type::Named {
                        name: "Column".to_string(),
                        args: vec![elem],
                    },
                });
            }
        }

        // `Column[T]` / `Tensor[T, ...S]` `.fold[A](init: A, f: Fn(A, T) -> A)
        // -> A` — the general left-fold primitive. `A` is inferred from `init`
        // (concrete after `infer_expr`), so the closure params `(A, T)` and its
        // return `A` are all concrete and `check_expr` drives closure-param
        // pushdown (same shape as `Iterator.fold`). `T` is the receiver's
        // element (`Column[T]` → the sole arg; `Tensor[T, ...S]` → the leading
        // arg). Typed here because baked generic dispatch can't bind `A` from
        // an argument nor thread the receiver's `T` into the closure signature.
        if method == "fold" {
            // The element `T` and the container's display name, for either
            // handle-backed reducer.
            let fold_receiver = |ty: &Type| -> Option<(Type, &'static str)> {
                match ty {
                    Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                        Some((args[0].clone(), "Column"))
                    }
                    Type::Named { name, args } if name == "Tensor" && !args.is_empty() => {
                        Some((args[0].clone(), "Tensor"))
                    }
                    _ => None,
                }
            };
            let mut elem_and_kind = match obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => fold_receiver(inner),
                other => fold_receiver(other),
            };
            // Bound-generic receiver (`c: ref C` where `C: Reduce[T]`): the
            // element is the trait bound's argument. Falls into the SAME
            // A-from-init + closure-pushdown below, so `fold` on a `Reduce`-
            // bounded generic type-checks (S6c); the mono'd receiver routes to
            // the inline-closure kernel exactly as `sum`/`max` do. Interp
            // dispatches on the concrete `Column`/`Tensor` Value at runtime.
            if elem_and_kind.is_none() {
                if let Some(pname) = Self::receiver_type_param_name(obj_ty) {
                    if let Some(elem) = self.bound_element_for_trait(&pname, "Reduce") {
                        elem_and_kind = Some((elem, "Reduce"));
                    }
                }
            }
            if let Some((elem, container)) = elem_and_kind {
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "{container}.fold expects 2 arguments (init, closure), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Some(Type::Error);
                }
                let acc_ty = self.infer_expr(&args[0].value);
                let f_ty = Type::Function {
                    params: vec![acc_ty.clone(), elem],
                    return_type: Box::new(acc_ty.clone()),
                };
                self.check_expr(&args[1].value, &f_ty);
                return Some(acc_ty);
            }
        }

        // `Column[T]` / `Tensor[T, ...S]` `.map(|x| ...) -> Self` — the
        // element-wise map surface (S6c-2, the `ElementwiseMap` trait's `map`).
        // Same element type first cut (`Fn(T) -> T`), so the result is the
        // receiver's own container type. Typed here (like `fold`) because the
        // closure's parameter `T` is the receiver's element, which baked
        // generic dispatch can't thread into the closure signature.
        if method == "map" {
            // `Option[T].map(f)` / `Result[T, E].map(f)`: apply `f: Fn(T) -> R`
            // to the present payload, yielding `Option[R]` / `Result[R, E]`
            // (an absent receiver passes through). Push `T` into the closure
            // param so an un-annotated `|x| ..` infers, read `R` back from the
            // solved return type, and record the SOURCE inner `T` in
            // `method_unwrap_inner_types` for codegen payload reconstruction.
            // design.md documents `.map` on Result as intended; previously this
            // fell through to a permissive fallback that typechecked but had no
            // runtime dispatch in either backend (B-2026-07-12-11).
            let optres_map = |ty: &Type| -> Option<(&'static str, Type, Option<Type>)> {
                match ty {
                    Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                        Some(("Option", args[0].clone(), None))
                    }
                    Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                        Some(("Result", args[0].clone(), Some(args[1].clone())))
                    }
                    _ => None,
                }
            };
            let optres_recv = match obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => optres_map(inner),
                other => optres_map(other),
            };
            if let Some((enum_name, t_ty, e_ty)) = optres_recv {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "{enum_name}.map expects 1 argument (closure), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Some(Type::Error);
                }
                // Infer the mapper's type. For a fn-reference or an ANNOTATED
                // closure this yields a concrete `Fn(T') -> R`; seed the param
                // from `T` (so an un-annotated typevar param picks it up) and
                // read `R` as the result payload type. `check_expr` with a
                // fresh return typevar can't be used here — `check_assignable`
                // is subtyping, not unification, so it never solves the return
                // var. (Fully inferring an un-annotated `|x| ..` param from `T`
                // is the separate closure-param-inference gap B-2026-07-12-10.)
                //
                // B-2026-08-08-21 — publish the param SEED first
                // (B-2026-07-15-16's mechanism), so an un-annotated `|s| ..`
                // sees the payload type `T` while its body is inferred rather
                // than relying on post-hoc unification to solve it. Every
                // sibling that takes a payload closure already does this
                // through `infer_closure_ret` (`map_or`, `map_or_else`,
                // `map_err`, `and_then`); `map` was the one that did not, which
                // is why `out.first().map(|s| s.to_uppercase())` passed
                // `karac check` and was then REFUSED by codegen — the param
                // stayed a metavar, so the closure's surface types were not
                // recoverable and the heap-payload path bailed loudly rather
                // than miscompile. An explicit annotation still wins, in the
                // synth-mode closure arm.
                if matches!(&args[0].value.kind, ExprKind::Closure { .. }) {
                    self.closure_param_seeds
                        .insert(SpanKey::from_span(&args[0].value.span), vec![t_ty.clone()]);
                }
                let f_actual = self.infer_expr(&args[0].value);
                let f_resolved = resolve_type_var_top(&f_actual, &self.env.substitutions);
                let r_resolved = match &f_resolved {
                    Type::Function {
                        params,
                        return_type,
                    }
                    | Type::OnceFunction {
                        params,
                        return_type,
                    } => {
                        if let Some(p0) = params.first() {
                            unify_types(
                                p0,
                                &t_ty,
                                &mut self.env.substitutions,
                                &mut self.env.const_substitutions,
                            );
                        }
                        resolve_type_var_top(return_type, &self.env.substitutions)
                    }
                    _ => {
                        self.type_error(
                            format!(
                                "{enum_name}.map expects a function argument, got '{}'",
                                type_display(&f_resolved)
                            ),
                            args[0].value.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        return Some(Type::Error);
                    }
                };
                let t_resolved = resolve_type_var_top(&t_ty, &self.env.substitutions);
                // Codegen reconstructs the receiver's payload from these words
                // to feed the mapper; the RESULT `R` is read off the mapper's
                // compiled SSA value, so only the SOURCE `T` needs recording.
                self.method_unwrap_inner_types.insert(
                    SpanKey::for_method_call(span, args_close_span),
                    Self::type_to_type_expr(&t_resolved),
                );
                // B-2026-08-09-6 — `Result[T, E].map(f)` also needs `E`. The
                // heap-`T` lowering is `compile_map_via_match_synthesis`, which
                // synthesizes `Err(e) => Err(e)` and seeds that binding's type
                // from `method_unwrap_err_types`. Nothing ever wrote the entry
                // for `map` — the two producers are `unwrap_or` (B-2026-08-05-9)
                // and the absent-closure combinators (B-2026-07-14-6) — so the
                // synthesis has read `None` there since it landed in 4b941dc,
                // and a heap `Err` payload was lowered as a bare scalar word:
                // `Result[String, String].map(|x| x)` on the Err branch printed
                // the EMPTY STRING against `--interp`'s payload, and leaked the
                // buffer. Recorded for `Result` only; `Option`'s absent branch
                // carries no payload to type.
                if enum_name == "Result" {
                    if let Some(e) = &e_ty {
                        let e_resolved = resolve_type_var_top(e, &self.env.substitutions);
                        self.method_unwrap_err_types.insert(
                            SpanKey::for_method_call(span, args_close_span),
                            Self::type_to_type_expr(&e_resolved),
                        );
                    }
                }
                // Record the SOLVED mapper `Fn(T) -> R` at the closure's own
                // span. The lowering pass folds Function-typed `expr_types`
                // entries into `Program.fn_value_typed_exprs`, which codegen's
                // closure compilation reads to type an UN-ANNOTATED closure
                // param/return. Without it the mapper falls back to `i64`
                // params — and since `String`/`Vec` share one LLVM type,
                // codegen can't otherwise tell `|s| s.to_uppercase()` returns a
                // String — so a heap-payload `.map()` mapper mis-typed. Recorded
                // only for a closure LITERAL (a fn-reference arg keeps its own
                // recorded type). Heap-payload map codegen (B-2026-07-12-11).
                if matches!(&args[0].value.kind, ExprKind::Closure { .. }) {
                    let mapper_fn_ty = Type::Function {
                        params: vec![t_resolved.clone()],
                        return_type: Box::new(r_resolved.clone()),
                    };
                    self.record_expr_type(&args[0].value.span, &mapper_fn_ty);
                }
                let result = if enum_name == "Option" {
                    Type::Named {
                        name: "Option".to_string(),
                        args: vec![r_resolved],
                    }
                } else {
                    Type::Named {
                        name: "Result".to_string(),
                        args: vec![r_resolved, e_ty.unwrap_or(Type::Error)],
                    }
                };
                self.record_expr_type(span, &result);
                return Some(result);
            }
            // (element `T`, the owned `Self` container type, display name).
            let map_receiver = |ty: &Type| -> Option<(Type, Type, &'static str)> {
                match ty {
                    Type::Named { name, args } if name == "Column" && args.len() == 1 => {
                        Some((args[0].clone(), ty.clone(), "Column"))
                    }
                    Type::Named { name, args } if name == "Tensor" && !args.is_empty() => {
                        Some((args[0].clone(), ty.clone(), "Tensor"))
                    }
                    _ => None,
                }
            };
            let mut recv = match obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => map_receiver(inner),
                other => map_receiver(other),
            };
            // Bound-generic receiver (`c: ref C` where `C: ElementwiseMap[T]`):
            // `map` returns `Self = C`, and the closure param `T` is the bound's
            // element. Mono routes the receiver to the inline-closure kernel
            // (which allocates a fresh `Self`) exactly as the concrete surface
            // does; interp dispatches on the concrete Column/Tensor Value.
            if recv.is_none() {
                if let Some(pname) = Self::receiver_type_param_name(obj_ty) {
                    if let Some(elem) = self.bound_element_for_trait(&pname, "ElementwiseMap") {
                        recv = Some((elem, Type::TypeParam(pname), "ElementwiseMap"));
                    }
                }
            }
            if let Some((elem, self_ty, container)) = recv {
                if args.len() != 1 {
                    self.type_error(
                        format!(
                            "{container}.map expects 1 argument (closure), got {}",
                            args.len()
                        ),
                        span.clone(),
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Some(Type::Error);
                }
                let f_ty = Type::Function {
                    params: vec![elem.clone()],
                    return_type: Box::new(elem),
                };
                self.check_expr(&args[0].value, &f_ty);
                self.record_expr_type(span, &self_ty);
                return Some(self_ty);
            }
        }
        None
    }
}
