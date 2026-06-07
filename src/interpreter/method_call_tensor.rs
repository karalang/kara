//! `Tensor[T, Shape]` interpreter intrinsics (phase-11 numerical
//! stdlib, MVP slice). Constructors dispatch through `eval_call.rs`'s
//! `"Tensor.zeros"` / `"Tensor.ones"` / `"Tensor.full"` path-string
//! arms; instance methods (`shape` / `rank` / `iter_axis`) through
//! `try_eval_tensor_method` in the method-dispatch chain. Element
//! storage is C-order (row-major) in the same `Arc<RwLock<Vec<Value>>>`
//! shared-cell shape as `Value::Array` (par-block capture shares Values
//! across real OS threads). Indexing get/set live at the existing
//! Index-expression sites (`eval_expr.rs` / `set_index`), routed by the
//! `Value::Tensor` match arms there via [`tensor_offset`].
//!
//! See `runtime/stdlib/tensor.kara` for the surface declaration and the
//! interpreter fill-type note (`zeros`/`ones` fill `f64`; `full` is the
//! typed fill).

use std::sync::{Arc, RwLock};

use crate::ast::{CallArg, Expr, ExprKind};
use crate::interpreter::value::Value;
use crate::token::Span;

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

impl<'a> super::Interpreter<'a> {
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
            "Tensor.zeros" => Value::Float(0.0),
            "Tensor.ones" => Value::Float(1.0),
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
    /// the interpreter's Array value), `rank()` -> i64, `iter_axis(n)`
    /// -> Vec of sub-tensors. Returns `None` for non-Tensor receivers /
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
            _ => None,
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
}
