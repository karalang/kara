//! `Column[T]` interpreter MVP (phase-11 data-science stdlib, Arrow
//! commitment Q5). Constructors (`new` / `with_capacity` / `from_vec`)
//! dispatched from `eval_call.rs`, and the instance methods (`push` /
//! `push_null` / `len` / `null_count` / `valid_count` / `is_null`)
//! intercepted before the surface-only `#[compiler_builtin]` bodies in
//! `runtime/stdlib/column.kara`. Positional indexing `c[i] -> Option[T]`
//! is intercepted at the index sites (`eval_expr.rs` get, typechecker
//! `exprs.rs` typing) — the `[]` operator has no method to dispatch
//! through.
//!
//! A `Value::Column` carries a `data` element buffer plus a parallel
//! `valid` validity bitmap (one `bool` per slot; `false` = SQL null),
//! both `Arc<RwLock<…>>` shared cells so mutation through a cloned
//! receiver (the `mut ref self` push path) reaches the original binding
//! and par-block capture stays sound. The two Vecs are kept the same
//! length — the Arrow invariant — so `push_null` appends a never-read
//! `Value::Unit` placeholder to `data`.

use std::sync::{Arc, RwLock};

use crate::ast::CallArg;
use crate::interpreter::value::Value;
use crate::token::Span;

impl<'a> super::Interpreter<'a> {
    /// Column constructors dispatched from `eval_call.rs`. Returns `None`
    /// for an unrecognized path / malformed args (caller falls through).
    pub(super) fn eval_column_new(
        &mut self,
        path_str: &str,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match path_str {
            // `new()` / `with_capacity(cap)` — start empty (the capacity
            // hint is advisory; the codegen Arrow buffer will honor it).
            "Column.new" | "Column.with_capacity" => Some(Value::Column {
                data: Arc::new(RwLock::new(Vec::new())),
                valid: Arc::new(RwLock::new(Vec::new())),
            }),
            // `from_vec(values)` — every slot valid (no nulls). The
            // argument is a `Vec[T]`, i.e. a `Value::Array`.
            "Column.from_vec" => {
                let arg = args.first()?;
                let Value::Array(rc) = self.eval_expr_inner(&arg.value) else {
                    return None;
                };
                let data: Vec<Value> = rc.read().unwrap().clone();
                let valid = vec![true; data.len()];
                Some(Value::Column {
                    data: Arc::new(RwLock::new(data)),
                    valid: Arc::new(RwLock::new(valid)),
                })
            }
            _ => None,
        }
    }

    /// Instance methods on `Value::Column`. Returns `None` for a
    /// non-Column receiver / unknown method (caller falls through).
    pub(super) fn try_eval_column_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let Value::Column { data, valid } = obj else {
            return None;
        };
        match method {
            "push" => {
                let arg = args.first()?;
                let value = self.eval_expr_inner(&arg.value);
                data.write().unwrap().push(value);
                valid.write().unwrap().push(true);
                Some(Value::Unit)
            }
            "push_null" => {
                // Arrow invariant: a data slot per position — a never-read
                // placeholder stands in for the null value.
                data.write().unwrap().push(Value::Unit);
                valid.write().unwrap().push(false);
                Some(Value::Unit)
            }
            "len" => Some(Value::Int(valid.read().unwrap().len() as i64)),
            "null_count" => {
                let n = valid.read().unwrap().iter().filter(|&&v| !v).count();
                Some(Value::Int(n as i64))
            }
            "valid_count" => {
                let n = valid.read().unwrap().iter().filter(|&&v| v).count();
                Some(Value::Int(n as i64))
            }
            "is_null" => {
                let arg = args.first()?;
                let Value::Int(i) = self.eval_expr_inner(&arg.value) else {
                    return Some(self.record_runtime_error(
                        "Column.is_null index must be an integer".to_string(),
                        span,
                    ));
                };
                let guard = valid.read().unwrap();
                if i < 0 || (i as usize) >= guard.len() {
                    return Some(self.record_runtime_error(
                        format!(
                            "Column.is_null index {} out of bounds (len {})",
                            i,
                            guard.len()
                        ),
                        span,
                    ));
                }
                Some(Value::Bool(!guard[i as usize]))
            }
            _ => None,
        }
    }
}
