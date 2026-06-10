//! `Tensor[T, Shape]` interpreter intrinsics (phase-11 numerical
//! stdlib, MVP slice). Constructors dispatch through `eval_call.rs`'s
//! `"Tensor.zeros"` / `"Tensor.ones"` / `"Tensor.full"` path-string
//! arms; instance methods (`shape` / `rank` and the shape-transform
//! family `iter_axis` / `reshape` / `permute` / `slice` / `squeeze`)
//! through `try_eval_tensor_method` in the method-dispatch chain. Element
//! storage is C-order (row-major) in the same `Arc<RwLock<Vec<Value>>>`
//! shared-cell shape as `Value::Array` (par-block capture shares Values
//! across real OS threads). Indexing get/set live at the existing
//! Index-expression sites (`eval_expr.rs` / `set_index`), routed by the
//! `Value::Tensor` match arms there via [`tensor_offset`].
//!
//! See `runtime/stdlib/tensor.kara` for the surface declaration. The
//! `zeros`/`ones` fill type is read off the enclosing `let`'s
//! `Tensor[Elem, …]` annotation (threaded via `pending_tensor_fill` —
//! the interpreter is dynamically typed and the typechecker only records
//! the unresolved `Tensor[T, S]` at the call span): an integer / bool
//! element fills `Value::Int` / `Value::Bool`, anything else (or no
//! annotation) falls back to the `f64` default. `full` takes an explicit
//! typed value and so needs no hint.

use std::sync::{Arc, RwLock};

use crate::ast::{BinOp, CallArg, Expr, ExprKind, TypeExpr, TypeKind};
use crate::interpreter::value::Value;
use crate::token::Span;

/// Element-fill class for `Tensor.zeros` / `Tensor.ones` — the only
/// distinction the dynamically-typed interpreter's `Value` makes among
/// numeric element types (integer widths collapse to `Value::Int`, both
/// float widths to `Value::Float`). Derived from a `let`'s `Tensor[Elem,
/// …]` annotation by [`tensor_elem_fill`] and stashed on
/// `Interpreter::pending_tensor_fill`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TensorElemFill {
    Int,
    Float,
    Bool,
}

/// Classify a tensor *element* type annotation into a [`TensorElemFill`].
/// The element is the first generic argument of a `Tensor[Elem, Shape]`
/// path type. `None` for non-primitive / unrecognized elements (the fill
/// then degrades to the `f64` default). Integer widths (signed + unsigned)
/// and `f16`/`bf16`-vs-`f32`/`f64` are not distinguished — the
/// interpreter's `Value` can't represent the distinction.
fn classify_elem_name(name: &str) -> Option<TensorElemFill> {
    match name {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => Some(TensorElemFill::Int),
        "f16" | "bf16" | "f32" | "f64" => Some(TensorElemFill::Float),
        "bool" => Some(TensorElemFill::Bool),
        _ => None,
    }
}

/// Pull the fill class out of a `let` annotation when it is a
/// `Tensor[Elem, …]` whose `Elem` is a recognized primitive. Returns
/// `None` for any other annotation shape (no hint — `zeros`/`ones` keep
/// the float default).
pub(super) fn tensor_elem_fill(ty: &TypeExpr) -> Option<TensorElemFill> {
    let TypeKind::Path(path) = &ty.kind else {
        return None;
    };
    if path.segments.last().map(String::as_str) != Some("Tensor") {
        return None;
    }
    let elem = match path.generic_args.as_ref()?.first()? {
        crate::ast::GenericArg::Type(t) => t,
        _ => return None,
    };
    let TypeKind::Path(elem_path) = &elem.kind else {
        return None;
    };
    classify_elem_name(elem_path.segments.last()?.as_str())
}

/// Row-major (C-order) flat offset for `idx` into `dims`. Errors carry
/// the user-facing message for `record_runtime_error`.
pub(super) fn tensor_offset(dims: &[i64], idx: &[i64]) -> Result<usize, String> {
    if idx.len() != dims.len() {
        return Err(format!(
            "rank-{} tensor requires {} index component(s), found {}",
            dims.len(),
            dims.len(),
            idx.len()
        ));
    }
    let mut offset: usize = 0;
    for (pos, (&i, &d)) in idx.iter().zip(dims.iter()).enumerate() {
        if i < 0 || i >= d {
            return Err(format!(
                "index {} out of bounds for dim {} (size {})",
                i, pos, d
            ));
        }
        offset = offset * (d as usize) + (i as usize);
    }
    Ok(offset)
}

/// Syntax walk for `Tensor.from`'s literal argument — interpreter twin
/// of the typechecker's `collect_tensor_literal`
/// (`src/typechecker/expr_call.rs`): the leftmost spine establishes
/// `dims`, every other visit checks against the established entry, leaf
/// expressions collect in C-order for evaluation. Errors carry the
/// user-facing message for `record_runtime_error`.
fn collect_tensor_literal_dims<'e>(
    expr: &'e Expr,
    depth: usize,
    dims: &mut Vec<i64>,
    leaves: &mut Vec<&'e Expr>,
) -> Result<(), String> {
    let ExprKind::ArrayLiteral(elements) = &expr.kind else {
        return Err(format!(
            "ragged tensor literal: expected a nested level at depth {} \
             (rank established as {} by the first element), found a scalar",
            depth,
            dims.len()
        ));
    };
    if elements.is_empty() {
        return Err("cannot infer tensor dims from an empty literal level — \
             zero-size tensors go through `Tensor.zeros(dims)`"
            .to_string());
    }
    let len = elements.len() as i64;
    let first_visit = dims.len() == depth;
    if first_visit {
        dims.push(len);
    } else if dims[depth] != len {
        return Err(format!(
            "ragged tensor literal: level at depth {} has {} element(s), expected {}",
            depth, len, dims[depth]
        ));
    }
    let nested = if first_visit {
        let any_array = elements
            .iter()
            .any(|e| matches!(e.kind, ExprKind::ArrayLiteral(_)));
        let all_array = elements
            .iter()
            .all(|e| matches!(e.kind, ExprKind::ArrayLiteral(_)));
        if any_array && !all_array {
            return Err(
                "ragged tensor literal: level mixes scalar and nested elements".to_string(),
            );
        }
        any_array
    } else {
        let expect_nested = dims.len() > depth + 1;
        if !expect_nested
            && elements
                .iter()
                .any(|e| matches!(e.kind, ExprKind::ArrayLiteral(_)))
        {
            return Err(format!(
                "ragged tensor literal: expected a scalar leaf at depth {} \
                 (rank established as {} by the first element), found a nested level",
                depth + 1,
                dims.len()
            ));
        }
        expect_nested
    };
    if nested {
        for elem in elements {
            collect_tensor_literal_dims(elem, depth + 1, dims, leaves)?;
        }
    } else {
        leaves.extend(elements.iter());
    }
    Ok(())
}

/// Extract the dim components of an evaluated index value — `Int` for
/// rank-1, `Tuple` of Ints for rank-N (the parser desugars `t[i, j, k]`
/// to a tuple index). `None` when the value isn't an integer family.
pub(super) fn index_components(idx: &Value) -> Option<Vec<i64>> {
    match idx {
        Value::Int(i) => Some(vec![*i]),
        Value::Tuple(parts) => {
            let mut out = Vec::with_capacity(parts.len());
            for p in parts {
                match p {
                    Value::Int(i) => out.push(*i),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Numeric value as `f64` — for `mean` / `mean_axis`, which always yield a
/// float regardless of the element type. Non-numeric values (never reached
/// for a typechecked reduce) fall back to `0.0`.
fn value_to_f64(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    }
}

impl<'a> super::Interpreter<'a> {
    /// Scalar fill `Value` for `Tensor.zeros` / `Tensor.ones`, picked
    /// from the element-type hint threaded off the enclosing `let`'s
    /// `Tensor[Elem, …]` annotation (`pending_tensor_fill`). The
    /// tree-walk interpreter is dynamically typed and the typechecker
    /// records only the *declared* return type `Tensor[T, S]` (with `T`
    /// unresolved) at the call span, so the concrete element type comes
    /// from the annotation — the same source codegen reads via
    /// `pending_let_tensor_info`. An integer / bool element tensor fills
    /// `Value::Int` / `Value::Bool`; with no hint (unannotated, or the
    /// element wasn't a recognized primitive) it falls back to the
    /// historical `Value::Float` — the numerical stack's primary element
    /// type. `is_one` selects the 1-fill (`Tensor.ones`) vs the 0-fill
    /// (`Tensor.zeros`).
    fn tensor_scalar_fill(&self, is_one: bool) -> Value {
        match self.pending_tensor_fill {
            Some(TensorElemFill::Int) => Value::Int(if is_one { 1 } else { 0 }),
            Some(TensorElemFill::Bool) => Value::Bool(is_one),
            Some(TensorElemFill::Float) | None => Value::Float(if is_one { 1.0 } else { 0.0 }),
        }
    }

    /// `Tensor.zeros(dims)` / `Tensor.ones(dims)` / `Tensor.full(dims,
    /// value)`. Returns `None` when the args don't fit (caller falls
    /// through to normal call dispatch, which lands on the baked stub
    /// body).
    pub(super) fn eval_tensor_new(
        &mut self,
        path_str: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let dims_arg = args.first()?;
        let dims_val = self.eval_expr_inner(&dims_arg.value);
        let dims: Vec<i64> = match &dims_val {
            Value::Array(rc) => {
                let guard = rc.read().unwrap();
                let mut out = Vec::with_capacity(guard.len());
                for v in guard.iter() {
                    match v {
                        Value::Int(i) if *i >= 0 => out.push(*i),
                        Value::Int(i) => {
                            return Some(self.record_runtime_error(
                                format!("tensor dim must be non-negative, got {}", i),
                                span,
                            ));
                        }
                        _ => return None,
                    }
                }
                out
            }
            _ => return None,
        };
        let fill = match path_str {
            "Tensor.zeros" => self.tensor_scalar_fill(false),
            "Tensor.ones" => self.tensor_scalar_fill(true),
            "Tensor.full" => {
                let val_arg = args.get(1)?;
                self.eval_expr_inner(&val_arg.value)
            }
            _ => return None,
        };
        let count: usize = dims.iter().map(|&d| d as usize).product();
        let data = vec![fill; count];
        Some(Value::Tensor {
            dims: Arc::new(dims),
            data: Arc::new(RwLock::new(data)),
        })
    }

    /// `Tensor.from(nested array literal)` — literal constructor. Walks
    /// the argument's *syntax* (not its evaluated value: a runtime
    /// `Value::Array` can't distinguish a nested row from a leaf `Vec`
    /// element), mirroring the typechecker's `infer_tensor_from` rule:
    /// the leftmost spine establishes dims, sibling levels are checked
    /// against it, leaves evaluate in C-order. Errors are emitted here
    /// too (not just at typecheck) because `karac run`'s `run_program`
    /// path doesn't gate on typecheck errors.
    pub(super) fn eval_tensor_from(&mut self, args: &[CallArg], span: &Span) -> Value {
        let Some(data) = args.first().map(|a| &a.value) else {
            return self.record_runtime_error(
                "Tensor.from takes exactly 1 argument (a nested array literal)".to_string(),
                span,
            );
        };
        if !matches!(data.kind, ExprKind::ArrayLiteral(_)) {
            return self.record_runtime_error(
                "Tensor.from requires an array-literal argument — dims are inferred \
                 from the literal's nesting; for runtime-shaped data use \
                 `Tensor.zeros(dims)` / `Tensor.full(dims, value)` plus indexed writes"
                    .to_string(),
                span,
            );
        }
        let mut dims: Vec<i64> = Vec::new();
        let mut leaves: Vec<&Expr> = Vec::new();
        if let Err(msg) = collect_tensor_literal_dims(data, 0, &mut dims, &mut leaves) {
            return self.record_runtime_error(msg, span);
        }
        let mut elements = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            elements.push(self.eval_expr_inner(leaf));
        }
        Value::Tensor {
            dims: Arc::new(dims),
            data: Arc::new(RwLock::new(elements)),
        }
    }

    /// Instance methods on `Value::Tensor`: `shape()` -> Vec[i64] (as
    /// the interpreter's Array value), `rank()` -> i64, and the
    /// shape-transform family (`iter_axis` / `reshape` / `permute` /
    /// `slice` / `squeeze`). Returns `None` for non-Tensor receivers /
    /// unknown methods (caller falls through).
    pub(super) fn try_eval_tensor_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let Value::Tensor { dims, data } = obj else {
            return None;
        };
        match method {
            "shape" => Some(Value::Array(Arc::new(RwLock::new(
                dims.iter().map(|&d| Value::Int(d)).collect(),
            )))),
            "rank" => Some(Value::Int(dims.len() as i64)),
            "iter_axis" => Some(self.eval_tensor_iter_axis(dims, data, args, span)),
            "reshape" => Some(self.eval_tensor_reshape(dims, data, args, span)),
            "permute" => Some(self.eval_tensor_permute(dims, data, args, span)),
            "slice" => Some(self.eval_tensor_slice(dims, data, args, span)),
            "squeeze" => Some(self.eval_tensor_squeeze(dims, data, args, span)),
            "sum" | "mean" | "prod" | "min" | "max" => {
                Some(self.eval_tensor_reduce(method, data, span))
            }
            "sum_axis" | "mean_axis" => {
                Some(self.eval_tensor_axis_reduce(method, dims, data, args, span))
            }
            _ => None,
        }
    }

    /// Full reduction `sum` / `mean` / `prod` / `min` / `max` → a scalar.
    /// `sum`/`prod` fold via the scalar binop (inheriting overflow / the
    /// element's Int-vs-Float arithmetic); `min`/`max` keep the extreme
    /// element; `mean` is `sum / count` as `f64` (always — the decided rule).
    /// An empty tensor is a runtime error for every reduce: `min`/`max` have
    /// no identity, and the dynamically-typed tree-walk can't type the
    /// identity of an empty `sum`/`prod` — a uniform trap keeps it in step
    /// with codegen. (`run_program` bypasses typecheck, so this is the only
    /// guard.)
    fn eval_tensor_reduce(
        &mut self,
        method: &str,
        data: &Arc<RwLock<Vec<Value>>>,
        span: &Span,
    ) -> Value {
        let elems = data.read().unwrap().clone();
        if elems.is_empty() {
            return self.record_runtime_error(
                format!(
                    "cannot reduce an empty tensor (`{method}` has no value for zero elements)"
                ),
                span,
            );
        }
        let count = elems.len();
        match method {
            "sum" => self.tensor_fold_reduce(&BinOp::Add, elems, span),
            "prod" => self.tensor_fold_reduce(&BinOp::Mul, elems, span),
            "min" => Self::tensor_minmax_reduce(true, elems),
            "max" => Self::tensor_minmax_reduce(false, elems),
            "mean" => {
                let s = self.tensor_fold_reduce(&BinOp::Add, elems, span);
                if self.pending_cf.is_some() {
                    return Value::Unit;
                }
                Value::Float(value_to_f64(&s) / count as f64)
            }
            _ => unreachable!("eval_tensor_reduce: unrouted method '{method}'"),
        }
    }

    /// Fold `elems` (non-empty) left-to-right through the scalar binop.
    fn tensor_fold_reduce(&mut self, op: &BinOp, elems: Vec<Value>, span: &Span) -> Value {
        let mut it = elems.into_iter();
        let mut acc = it.next().expect("non-empty");
        for x in it {
            acc = self.eval_binary(op, acc, x, span);
            if self.pending_cf.is_some() {
                return Value::Unit;
            }
        }
        acc
    }

    /// Keep the min (or max) element of a non-empty `elems`. NaN compares
    /// false against everything, so it neither displaces nor is taken — the
    /// scalar `<` posture; NaN propagation is a v1.5 refinement.
    fn tensor_minmax_reduce(is_min: bool, elems: Vec<Value>) -> Value {
        let mut it = elems.into_iter();
        let mut acc = it.next().expect("non-empty");
        for x in it {
            let take = match (&acc, &x) {
                (Value::Int(a), Value::Int(b)) => {
                    if is_min {
                        b < a
                    } else {
                        b > a
                    }
                }
                (Value::Float(a), Value::Float(b)) => {
                    if is_min {
                        b < a
                    } else {
                        b > a
                    }
                }
                _ => false,
            };
            if take {
                acc = x;
            }
        }
        acc
    }

    /// `t.sum_axis(n)` / `t.mean_axis(n)` — reduce along axis `n`, dropping
    /// that slot (NumPy `sum(axis=n)` semantics). A rank-1 receiver reduces
    /// to a scalar. `mean_axis` divides each summed cell by `dims[n]` as
    /// `f64`. Empty tensor → runtime error (parity with `eval_tensor_reduce`).
    /// Axis bounds re-checked here (the `run_program` bypass).
    fn eval_tensor_axis_reduce(
        &mut self,
        method: &str,
        dims: &[i64],
        data: &Arc<RwLock<Vec<Value>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        let Some(axis_arg) = args.first() else {
            return self.record_runtime_error(
                format!("{method} takes exactly 1 argument (the axis)"),
                span,
            );
        };
        let axis_val = self.eval_expr_inner(&axis_arg.value);
        let Value::Int(axis) = axis_val else {
            return self.record_runtime_error(format!("{method} axis must be an integer"), span);
        };
        let rank = dims.len();
        if axis < 0 || axis as usize >= rank {
            return self.record_runtime_error(
                format!("axis {} out of bounds for rank-{} tensor", axis, rank),
                span,
            );
        }
        let axis = axis as usize;
        let elems = data.read().unwrap().clone();
        if elems.is_empty() {
            return self.record_runtime_error(
                format!(
                    "cannot reduce an empty tensor (`{method}` has no value for zero elements)"
                ),
                span,
            );
        }
        let is_mean = method == "mean_axis";

        if rank == 1 {
            let s = self.tensor_fold_reduce(&BinOp::Add, elems.clone(), span);
            if self.pending_cf.is_some() {
                return Value::Unit;
            }
            return if is_mean {
                Value::Float(value_to_f64(&s) / elems.len() as f64)
            } else {
                s
            };
        }

        // result[outer*inner + inner_idx] = sum over the axis of input[f],
        // where inner = product(dims right of axis), n_axis = dims[axis].
        let inner: usize = dims[axis + 1..].iter().map(|&d| d as usize).product();
        let n_axis = dims[axis] as usize;
        let result_size = elems.len() / n_axis;
        let zero = match elems.first() {
            Some(Value::Int(_)) => Value::Int(0),
            _ => Value::Float(0.0),
        };
        let mut acc: Vec<Value> = vec![zero; result_size];
        for (f, v) in elems.iter().enumerate() {
            let inner_idx = f % inner;
            let outer_idx = f / (inner * n_axis);
            let r = outer_idx * inner + inner_idx;
            acc[r] = self.eval_binary(&BinOp::Add, acc[r].clone(), v.clone(), span);
            if self.pending_cf.is_some() {
                return Value::Unit;
            }
        }
        if is_mean {
            acc = acc
                .iter()
                .map(|s| Value::Float(value_to_f64(s) / n_axis as f64))
                .collect();
        }
        let sub_dims: Vec<i64> = dims
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != axis)
            .map(|(_, &d)| d)
            .collect();
        Value::Tensor {
            dims: Arc::new(sub_dims),
            data: Arc::new(RwLock::new(acc)),
        }
    }

    /// `t.iter_axis(n)` — axis iteration. Yields the `dims[n]`
    /// sub-tensors obtained by fixing the index along axis `n` (the axis
    /// is dropped — NumPy `take(i, axis=n)` semantics), as a Vec of
    /// *copies*. A rank-1 receiver yields the raw scalar elements
    /// (`Vec[T]`) — rank-0 tensors aren't expressible. One flat C-order
    /// pass buckets each element by its axis-`n` coordinate, which
    /// preserves C-order of the remaining dims within every bucket. The
    /// axis is bounds-checked here too (not just at typecheck) because
    /// `karac run`'s `run_program` path doesn't gate on typecheck
    /// errors, and a runtime-valued axis is never statically checkable.
    fn eval_tensor_iter_axis(
        &mut self,
        dims: &[i64],
        data: &Arc<RwLock<Vec<Value>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        let Some(axis_arg) = args.first() else {
            return self.record_runtime_error(
                "iter_axis takes exactly 1 argument (the axis)".to_string(),
                span,
            );
        };
        let axis_val = self.eval_expr_inner(&axis_arg.value);
        let Value::Int(axis) = axis_val else {
            return self
                .record_runtime_error("iter_axis axis must be an integer".to_string(), span);
        };
        let rank = dims.len();
        if axis < 0 || axis as usize >= rank {
            return self.record_runtime_error(
                format!("axis {} out of bounds for rank-{} tensor", axis, rank),
                span,
            );
        }
        let axis = axis as usize;
        let guard = data.read().unwrap();
        if rank == 1 {
            // Rank-1: yield the scalar elements directly.
            return Value::Array(Arc::new(RwLock::new(guard.clone())));
        }
        // Sub-tensor shape: dims with the axis slot dropped. The
        // axis-coordinate of flat index f is (f / stride) % dims[axis],
        // where stride is the product of the dims to the right of the
        // axis.
        let sub_dims: Vec<i64> = dims
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != axis)
            .map(|(_, &d)| d)
            .collect();
        let stride: usize = dims[axis + 1..].iter().map(|&d| d as usize).product();
        let bucket_len: usize = sub_dims.iter().map(|&d| d as usize).product();
        let n_buckets = dims[axis] as usize;
        let mut buckets: Vec<Vec<Value>> = vec![Vec::with_capacity(bucket_len); n_buckets];
        for (f, v) in guard.iter().enumerate() {
            buckets[(f / stride) % n_buckets].push(v.clone());
        }
        let out: Vec<Value> = buckets
            .into_iter()
            .map(|b| Value::Tensor {
                dims: Arc::new(sub_dims.clone()),
                data: Arc::new(RwLock::new(b)),
            })
            .collect();
        Value::Array(Arc::new(RwLock::new(out)))
    }

    /// `t.reshape([d0, d1, ...])` — same elements, new dims, C-order
    /// preserved. The dims argument must be an array literal (the
    /// typechecker's rule, re-emitted here since `run_program` doesn't
    /// gate on typecheck); entries evaluate to non-negative ints whose
    /// product must equal the element count. The data is *copied* —
    /// tensors are value types, so the result must not alias the
    /// receiver (codegen may share buffers copy-on-write later).
    fn eval_tensor_reshape(
        &mut self,
        dims: &[i64],
        data: &Arc<RwLock<Vec<Value>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        let Some(dims_arg) = args.first().map(|a| &a.value) else {
            return self.record_runtime_error(
                "reshape takes exactly 1 argument (the new dims)".to_string(),
                span,
            );
        };
        let ExprKind::ArrayLiteral(entries) = &dims_arg.kind else {
            return self.record_runtime_error(
                "reshape requires an array-literal dims argument — the result's \
                 static rank comes from the literal's length (`t.reshape([3, 4])`); \
                 runtime-shaped reshape is v1.5 shape arithmetic"
                    .to_string(),
                span,
            );
        };
        if entries.is_empty() {
            return self.record_runtime_error(
                "reshape to rank 0 — `[]` is not a valid dims list (rank-0 \
                 tensors aren't expressible)"
                    .to_string(),
                span,
            );
        }
        let mut new_dims: Vec<i64> = Vec::with_capacity(entries.len());
        for entry in entries {
            match self.eval_expr_inner(entry) {
                Value::Int(v) if v >= 0 => new_dims.push(v),
                Value::Int(v) => {
                    return self.record_runtime_error(
                        format!("reshape dim must be non-negative, got {}", v),
                        span,
                    );
                }
                _ => {
                    return self
                        .record_runtime_error("reshape dims must be integers".to_string(), span);
                }
            }
        }
        let old_count: i64 = dims.iter().product();
        let new_count: i64 = new_dims.iter().product();
        if old_count != new_count {
            return self.record_runtime_error(
                format!(
                    "reshape from {:?} ({} element(s)) to {:?} ({} element(s)) — \
                     element counts must match",
                    dims, old_count, new_dims, new_count
                ),
                span,
            );
        }
        let elements = data.read().unwrap().clone();
        Value::Tensor {
            dims: Arc::new(new_dims),
            data: Arc::new(RwLock::new(elements)),
        }
    }

    /// `t.permute([1, 0, 2])` — reorder the axes; result dim `i` is the
    /// receiver's dim `perm[i]`. The axis list must be an array literal
    /// forming an exact permutation of `0..rank` (typechecker rule,
    /// re-emitted at runtime). Data is reordered into a fresh C-order
    /// buffer: each output flat index decomposes into output coords by
    /// div/mod, and the source flat index is the dot product of those
    /// coords with the *source* strides of the permuted-from axes.
    fn eval_tensor_permute(
        &mut self,
        dims: &[i64],
        data: &Arc<RwLock<Vec<Value>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        let rank = dims.len();
        let Some(perm_arg) = args.first().map(|a| &a.value) else {
            return self.record_runtime_error(
                "permute takes exactly 1 argument (the axis list)".to_string(),
                span,
            );
        };
        let ExprKind::ArrayLiteral(entries) = &perm_arg.kind else {
            return self.record_runtime_error(
                "permute requires a literal axis-list argument \
                 (`t.permute([1, 0])`) — runtime-valued permutations are v1.5"
                    .to_string(),
                span,
            );
        };
        if entries.len() != rank {
            return self.record_runtime_error(
                format!(
                    "permute axis list has {} entr{}, expected {} (the receiver's rank)",
                    entries.len(),
                    if entries.len() == 1 { "y" } else { "ies" },
                    rank
                ),
                span,
            );
        }
        let mut perm: Vec<usize> = Vec::with_capacity(rank);
        let mut seen = vec![false; rank];
        for entry in entries {
            let Value::Int(i) = self.eval_expr_inner(entry) else {
                return self
                    .record_runtime_error("permute axes must be integers".to_string(), span);
            };
            if i < 0 || i as usize >= rank {
                return self.record_runtime_error(
                    format!("axis {} out of bounds for rank-{} tensor", i, rank),
                    span,
                );
            }
            if seen[i as usize] {
                return self
                    .record_runtime_error(format!("permute axis list repeats axis {}", i), span);
            }
            seen[i as usize] = true;
            perm.push(i as usize);
        }
        // Source strides (C-order): stride[k] = product of dims[k+1..].
        let mut src_strides = vec![1usize; rank];
        for k in (0..rank - 1).rev() {
            src_strides[k] = src_strides[k + 1] * (dims[k + 1] as usize);
        }
        let new_dims: Vec<i64> = perm.iter().map(|&p| dims[p]).collect();
        let guard = data.read().unwrap();
        let total = guard.len();
        let mut out: Vec<Value> = Vec::with_capacity(total);
        for f in 0..total {
            // Decompose f into output coords (C-order over new_dims),
            // accumulating the source flat index as we go: output coord
            // i indexes source axis perm[i].
            let mut rem = f;
            let mut src = 0usize;
            for (i, &nd) in new_dims.iter().enumerate().rev() {
                let coord = rem % (nd as usize);
                rem /= nd as usize;
                src += coord * src_strides[perm[i]];
            }
            out.push(guard[src].clone());
        }
        Value::Tensor {
            dims: Arc::new(new_dims),
            data: Arc::new(RwLock::new(out)),
        }
    }

    /// `t.slice(axis, start, end)` — contiguous `[start, end)` range
    /// along one axis, other axes untouched, as a copy. Runtime checks:
    /// axis in range, `0 <= start <= end <= dims[axis]`. The copy walks
    /// the receiver as outer × dims[axis] × inner (outer = product of
    /// dims left of the axis, inner = product right of it) and keeps
    /// the `[start, end)` middle band of every outer block.
    fn eval_tensor_slice(
        &mut self,
        dims: &[i64],
        data: &Arc<RwLock<Vec<Value>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        if args.len() != 3 {
            return self.record_runtime_error(
                format!(
                    "slice takes exactly 3 arguments (axis, start, end), found {}",
                    args.len()
                ),
                span,
            );
        }
        let mut vals = [0i64; 3];
        for (slot, arg) in vals.iter_mut().zip(args.iter()) {
            let Value::Int(v) = self.eval_expr_inner(&arg.value) else {
                return self
                    .record_runtime_error("slice arguments must be integers".to_string(), span);
            };
            *slot = v;
        }
        let [axis, start, end] = vals;
        let rank = dims.len();
        if axis < 0 || axis as usize >= rank {
            return self.record_runtime_error(
                format!("axis {} out of bounds for rank-{} tensor", axis, rank),
                span,
            );
        }
        let axis = axis as usize;
        if start < 0 {
            return self.record_runtime_error(
                format!("slice start must be non-negative, got {}", start),
                span,
            );
        }
        if end < start {
            return self.record_runtime_error(
                format!("slice end {} is before start {}", end, start),
                span,
            );
        }
        if end > dims[axis] {
            return self.record_runtime_error(
                format!(
                    "slice end {} out of bounds for dim {} (size {})",
                    end, axis, dims[axis]
                ),
                span,
            );
        }
        let (start, end) = (start as usize, end as usize);
        let inner: usize = dims[axis + 1..].iter().map(|&d| d as usize).product();
        let outer: usize = dims[..axis].iter().map(|&d| d as usize).product();
        let axis_len = dims[axis] as usize;
        let guard = data.read().unwrap();
        let mut out: Vec<Value> = Vec::with_capacity(outer * (end - start) * inner);
        for o in 0..outer {
            let block = o * axis_len * inner;
            out.extend_from_slice(&guard[block + start * inner..block + end * inner]);
        }
        let new_dims: Vec<i64> = dims
            .iter()
            .enumerate()
            .map(|(i, &d)| if i == axis { (end - start) as i64 } else { d })
            .collect();
        Value::Tensor {
            dims: Arc::new(new_dims),
            data: Arc::new(RwLock::new(out)),
        }
    }

    /// `t.squeeze()` / `t.squeeze(n)` — drop size-1 axes. The no-arg
    /// form drops every size-1 dim (error if that would leave rank 0);
    /// the one-arg form drops exactly slot `n`, which must be in range,
    /// of size 1, and on a rank ≥ 2 receiver. Data is unchanged (the
    /// element count and C-order are identical), only the dims shrink —
    /// still copied, since tensors are value types.
    fn eval_tensor_squeeze(
        &mut self,
        dims: &[i64],
        data: &Arc<RwLock<Vec<Value>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        let rank = dims.len();
        let new_dims: Vec<i64> = match args {
            [] => {
                let kept: Vec<i64> = dims.iter().copied().filter(|&d| d != 1).collect();
                if kept.is_empty() {
                    return self.record_runtime_error(
                        format!(
                            "squeezing every dim of {:?} produces a rank-0 tensor, \
                             which isn't expressible — keep at least one dim \
                             (use `squeeze(n)`)",
                            dims
                        ),
                        span,
                    );
                }
                kept
            }
            [axis_arg] => {
                let Value::Int(n) = self.eval_expr_inner(&axis_arg.value) else {
                    return self
                        .record_runtime_error("squeeze axis must be an integer".to_string(), span);
                };
                if rank < 2 {
                    return self.record_runtime_error(
                        "cannot squeeze a rank-1 tensor — the result would be \
                         rank-0, which isn't expressible"
                            .to_string(),
                        span,
                    );
                }
                if n < 0 || n as usize >= rank {
                    return self.record_runtime_error(
                        format!("axis {} out of bounds for rank-{} tensor", n, rank),
                        span,
                    );
                }
                let n = n as usize;
                if dims[n] != 1 {
                    return self.record_runtime_error(
                        format!("cannot squeeze axis {}: its size is {}, not 1", n, dims[n]),
                        span,
                    );
                }
                dims.iter()
                    .enumerate()
                    .filter(|(j, _)| *j != n)
                    .map(|(_, &d)| d)
                    .collect()
            }
            _ => {
                return self.record_runtime_error(
                    format!(
                        "squeeze takes 0 or 1 argument(s) (an optional axis), found {}",
                        args.len()
                    ),
                    span,
                );
            }
        };
        let elements = data.read().unwrap().clone();
        Value::Tensor {
            dims: Arc::new(new_dims),
            data: Arc::new(RwLock::new(elements)),
        }
    }
}
