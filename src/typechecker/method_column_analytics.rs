//! `Column` / `DataFrame` / `Tensor` analytics-method typechecking.
//!
//! Fourth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Holds the
//! phase-11 analytics surface that sits after the `Option` / `Result`
//! combinators in the chain:
//!
//! - `zip_with(other, f)` — `ElementwiseMap`'s binary form;
//! - `argmin` / `argmax` / `sorted` / `argsort` — index and ordering
//!   reductions;
//! - the `Column` scalar reductions — `sum`, `mean`, `min`, `max`, `range`,
//!   `var`, `std`, `median`, `quantile`, `corr`;
//! - the `DataFrame` surface — `column`, `has_column`, `column_names`,
//!   `width`, `height`, `select`, `describe`, `write_csv`, `insert`.
//!
//! These need a dedicated surface because their result types are computed
//! from the receiver's element type rather than read off a baked signature.
//!
//! The block order is load-bearing — `infer_method_call` is a
//! first-match-wins chain, so these guards keep the exact relative order
//! they had inline, and the function is called from the same position in
//! that chain.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::types::{is_numeric, type_display, FloatSize, IntSize, Type};
use super::TypeErrorKind;

impl<'a> super::TypeChecker<'a> {
    /// Type a `Column` / `DataFrame` / `Tensor` analytics method.
    ///
    /// Returns `Some(ty)` when this surface claims `method` (including
    /// `Some(Type::Error)` when it claims the name but the call is
    /// ill-formed and a diagnostic has been emitted), and `None` when the
    /// name belongs to some later link in the `infer_method_call` chain.
    pub(super) fn try_column_analytics_method(
        &mut self,
        method: &str,
        args: &[CallArg],
        span: &Span,
        args_close_span: &Span,
        obj_ty: &Type,
    ) -> Option<Type> {
        // `ElementwiseMap`'s binary form `zip_with(other: Self, f: Fn(T, T) ->
        // T) -> Self` on the handle-backed containers (S6c) — element-wise
        // combine of two same-shape containers through the closure, yielding a
        // fresh `Self`. `other` must be the SAME container type; the closure is
        // typed `Fn(T, T) -> T` (both params + result the receiver's element).
        // Result is `Self`, which baked dispatch can't bind, so it's typed here
        // like `map`.
        if method == "zip_with" {
            // (element `T`, the owned `Self` container type, display name).
            let zip_receiver = |ty: &Type| -> Option<(Type, Type, &'static str)> {
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
                Type::Ref(inner) | Type::MutRef(inner) => zip_receiver(inner),
                other => zip_receiver(other),
            };
            // Bound-generic receiver (`a: ref C` where `C: ElementwiseMap[T]`):
            // `zip_with` returns `Self = C`; `other` must also be `C`, and the
            // closure is `Fn(T, T) -> T` over the bound's element. Same mono
            // routing as `map` (fresh `Self` allocation); interp dispatches on
            // the concrete Column/Tensor Value.
            if recv.is_none() {
                if let Some(pname) = Self::receiver_type_param_name(obj_ty) {
                    if let Some(elem) = self.bound_element_for_trait(&pname, "ElementwiseMap") {
                        recv = Some((elem, Type::TypeParam(pname), "ElementwiseMap"));
                    }
                }
            }
            if let Some((elem, self_ty, container)) = recv {
                if args.len() != 2 {
                    self.type_error(
                        format!(
                            "{container}.zip_with expects 2 arguments (other, closure), got {}",
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
                // `other` must be the same container type. The baked
                // signature declares it `other: ref Self` (a read borrow), so
                // a `ref Tensor` / `ref Column` argument — e.g. forwarding a
                // `ref Tensor[T, S]` parameter, the shape `dot`/`cosine`
                // helpers need — is correct; unwrap the borrow before the
                // same-container check, symmetric to the receiver unwrap
                // above. An owned argument (which auto-refs at the call)
                // passes through unchanged. (B-2026-07-13-5 gap C.)
                let other_ty_raw = self.infer_expr(&args[0].value);
                let other_ty = match &other_ty_raw {
                    Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
                    _ => other_ty_raw,
                };
                self.check_assignable(&self_ty, &other_ty, args[0].value.span.clone());
                let f_ty = Type::Function {
                    params: vec![elem.clone(), elem.clone()],
                    return_type: Box::new(elem),
                };
                self.check_expr(&args[1].value, &f_ty);
                self.record_expr_type(span, &self_ty);
                return Some(self_ty);
            }
        }

        // `ElementwiseOrd`'s ordering reductions on the handle-backed
        // containers (S6c) — `argmin` / `argmax` → `Option[i64]` (the FIRST
        // min/max index, `None` on empty/all-null), `sorted` → `Vec[T]`
        // (ascending values), `argsort` → `Vec[i64]` (the indices that sort
        // ascending, stable). The result mentions `T` (or is independent of
        // it), so it can't be expressed in a baked signature that binds `T`
        // from the receiver — typed here. For `Column` these operate on the
        // valid slots (nulls skipped; `argmin`/`argsort` report ORIGINAL slot
        // positions — `Series.idxmin` semantics); for `Tensor` over all
        // elements in flat C-order.
        if matches!(method, "argmin" | "argmax" | "sorted" | "argsort") {
            // (element `T`, display name) for a Column[T] / Tensor[T, ...S].
            let ord_receiver = |ty: &Type| -> Option<(Type, &'static str)> {
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
            let recv = match obj_ty {
                Type::Ref(inner) | Type::MutRef(inner) => ord_receiver(inner),
                other => ord_receiver(other),
            };
            if let Some((elem, container)) = recv {
                if !args.is_empty() {
                    self.type_error(
                        format!(
                            "{container}.{method} expects 0 arguments, got {}",
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
                if !is_numeric(&elem) && !self.type_param_has_numeric_bound(&elem) {
                    self.type_error(
                        format!(
                            "{container}.{method} requires a numeric element type, found '{}'",
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Some(Type::Error);
                }
                let ret = match method {
                    // The index of the first min/max, or None on empty/all-null.
                    "argmin" | "argmax" => Type::Named {
                        name: "Option".to_string(),
                        args: vec![Type::Int(IntSize::I64)],
                    },
                    // Ascending-sorted values (nulls dropped for a Column).
                    "sorted" => Type::Named {
                        name: "Vec".to_string(),
                        args: vec![elem.clone()],
                    },
                    // The indices that sort ascending (stable, original slots).
                    _ => Type::Named {
                        name: "Vec".to_string(),
                        args: vec![Type::Int(IntSize::I64)],
                    },
                };
                self.record_expr_type(span, &ret);
                // Stash the ELEMENT type at the non-aliased close-paren leaf so
                // the interpreter can recover element signedness for the
                // unsigned-64 sort order (B-2026-07-04-8). The result type
                // (`Vec[i64]` / `Option[i64]` for argsort/argmin/argmax, or the
                // `Vec[T]` that `record_expr_type(span, …)` just wrote) clobbers
                // `expr_types[receiver.span]`, so a `u64` element is otherwise
                // unrecoverable — the same receiver-span aliasing the `pow` /
                // bit-intrinsic paths work around via `args_close_span`.
                self.record_expr_type(args_close_span, &elem);
                return Some(ret);
            }
        }

        // Column[T] statistical reductions (phase-11 stats). All operate on
        // the valid (non-null) slots — SQL/pandas aggregate semantics.
        // `sum`/`min`/`max` -> T; `mean`/`var`/`std`/`median`/`quantile` ->
        // f64 (the numerical world promotes integer stats to float, and
        // `Value` can't distinguish f32/f64 — the `Tensor.mean` rule).
        // `corr` is Pearson over two `Column[f64]` -> f64. Baked generic
        // dispatch can't bind `T` (nor the result type) from the receiver,
        // so the whole surface is typed here from the receiver's element.
        if matches!(
            method,
            // B-2026-08-12-8 — `range` alongside the other Column reductions;
            // see the Tensor site above for why it was missing.
            "sum"
                | "mean"
                | "min"
                | "max"
                | "range"
                | "var"
                | "std"
                | "median"
                | "quantile"
                | "corr"
        ) {
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
                let nargs = usize::from(matches!(method, "quantile" | "corr"));
                if args.len() != nargs {
                    self.type_error(
                        format!(
                            "Column.{method} expects {nargs} argument(s), got {}",
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
                // `corr` is f64-only and binary (the other column); the rest
                // accept any numeric element.
                if method == "corr" {
                    if !matches!(elem, Type::Float(FloatSize::F64)) {
                        self.type_error(
                            format!(
                                "Column.corr requires an f64 column, found '{}'",
                                type_display(&elem)
                            ),
                            span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                        self.infer_expr(&args[0].value);
                        return Some(Type::Error);
                    }
                    let arg_ty = self.infer_expr(&args[0].value);
                    let expected = Type::Named {
                        name: "Column".to_string(),
                        args: vec![Type::Float(FloatSize::F64)],
                    };
                    self.check_assignable(&expected, &arg_ty, args[0].value.span.clone());
                    return Some(Type::Float(FloatSize::F64));
                }
                if !is_numeric(&elem) && !self.type_param_has_numeric_bound(&elem) {
                    self.type_error(
                        format!(
                            "Column.{method} requires a numeric element type, found '{}'",
                            type_display(&elem)
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Some(Type::Error);
                }
                // `quantile(q)` — `q` is an f64 in [0, 1] (range checked at
                // runtime, since it isn't a compile-time constant in general).
                if method == "quantile" {
                    let q_ty = self.infer_expr(&args[0].value);
                    self.check_assignable(
                        &Type::Float(FloatSize::F64),
                        &q_ty,
                        args[0].value.span.clone(),
                    );
                }
                return Some(match method {
                    "sum" | "min" | "max" | "range" => elem,
                    // mean / var / std / median / quantile
                    _ => Type::Float(FloatSize::F64),
                });
            }
        }

        // DataFrame methods (phase-11 Arrow, interpreter MVP). `DataFrame`
        // is non-generic, so a result that mentions an element type can't
        // bind it from the receiver: `column(name)` types as `Column[?]`
        // — a fresh var pinned by the binding annotation / downstream use
        // (the `Column.new()` posture; a wrong annotation isn't caught
        // statically, the codegen slice tightens it). The concrete-typed
        // methods are handled here too so the whole surface is predictable
        // for a brand-new builtin rather than leaning on baked dispatch.
        let is_dataframe = match obj_ty {
            Type::Named { name, .. } => name == "DataFrame",
            Type::Ref(inner) | Type::MutRef(inner) => {
                matches!(inner.as_ref(), Type::Named { name, .. } if name == "DataFrame")
            }
            _ => false,
        };
        if is_dataframe
            && matches!(
                method,
                "column"
                    | "insert"
                    | "has_column"
                    | "column_names"
                    | "width"
                    | "height"
                    | "select"
                    | "describe"
                    | "write_csv"
            )
        {
            let arity = |m: &str| match m {
                "insert" => 2usize,
                "column" | "has_column" | "select" | "write_csv" => 1,
                _ => 0,
            };
            let want = arity(method);
            if args.len() != want {
                self.type_error(
                    format!("{method} expects {want} argument(s), got {}", args.len()),
                    span.clone(),
                    TypeErrorKind::WrongNumberOfArgs,
                );
                return Some(Type::Error);
            }
            // Infer every arg (side effects / diagnostics); the leading
            // `name` of `column` / `has_column` / `insert` must be a
            // String, and `select`'s arg a `Vec[String]`. `insert`'s `col`
            // arg type isn't bound from the receiver (the baked-generic
            // limitation) — accepted as-is.
            let arg_tys: Vec<Type> = args.iter().map(|a| self.infer_expr(&a.value)).collect();
            if matches!(method, "column" | "has_column" | "insert" | "write_csv") {
                self.check_assignable(&Type::Str, &arg_tys[0], args[0].value.span.clone());
            } else if method == "select" {
                self.check_assignable(
                    &Type::Named {
                        name: "Vec".to_string(),
                        args: vec![Type::Str],
                    },
                    &arg_tys[0],
                    args[0].value.span.clone(),
                );
            }
            return Some(match method {
                "column" => Type::Named {
                    name: "Column".to_string(),
                    args: vec![self.env.fresh_type_var()],
                },
                "has_column" => Type::Bool,
                "column_names" => Type::Named {
                    name: "Vec".to_string(),
                    args: vec![Type::Str],
                },
                "width" | "height" => Type::Int(IntSize::I64),
                "select" | "describe" => Type::Named {
                    name: "DataFrame".to_string(),
                    args: vec![],
                },
                // CSV serialization (phase-11 CSV leg slice 1) — the same
                // `Result[Unit, IoError]` + `writes(FileSystem)` shape as
                // `fs.write`; the effect rides the stdlib stub's declared
                // signature like every other `#[compiler_builtin]` I/O fn.
                "write_csv" => Type::Named {
                    name: "Result".to_string(),
                    args: vec![
                        Type::Unit,
                        Type::Named {
                            name: "IoError".to_string(),
                            args: vec![],
                        },
                    ],
                },
                // insert
                _ => Type::Unit,
            });
        }
        None
    }
}
