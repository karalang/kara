//! `Tensor[T, Shape]` interpreter intrinsics (phase-11 numerical
//! stdlib, MVP slice). Constructors dispatch through `eval_call.rs`'s
//! `"Tensor.zeros"` / `"Tensor.ones"` / `"Tensor.full"` path-string
//! arms; instance methods (`shape` / `rank`) through
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

use crate::ast::CallArg;
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

    /// Instance methods on `Value::Tensor`: `shape()` -> Vec[i64] (as
    /// the interpreter's Array value), `rank()` -> i64. Returns `None`
    /// for non-Tensor receivers / unknown methods (caller falls
    /// through).
    pub(super) fn try_eval_tensor_method(
        &mut self,
        method: &str,
        obj: &Value,
        _span: &Span,
    ) -> Option<Value> {
        let Value::Tensor { dims, .. } = obj else {
            return None;
        };
        match method {
            "shape" => Some(Value::Array(Arc::new(RwLock::new(
                dims.iter().map(|&d| Value::Int(d)).collect(),
            )))),
            "rank" => Some(Value::Int(dims.len() as i64)),
            _ => None,
        }
    }
}
