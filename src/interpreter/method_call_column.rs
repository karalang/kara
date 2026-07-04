//! `Column[T]` interpreter MVP (phase-11 data-science stdlib, Arrow
//! commitment Q5). Constructors (`new` / `with_capacity` / `from_vec` /
//! `from_iter_nullable`) dispatched from `eval_call.rs`, and the instance
//! methods (`push` / `push_null` / `len` / `null_count` / `valid_count` /
//! `is_null` / `iter` / `iter_valid` / `fillna` / `dropna`) intercepted
//! before the surface-only `#[compiler_builtin]` bodies in
//! `runtime/stdlib/column.kara`. `iter` / `iter_valid` / `fillna` /
//! `dropna` are also typed by an intercept in
//! `src/typechecker/expr_method_call.rs` (binding `T` from the receiver),
//! and `iter` must dispatch here *before* the iterator machinery (which
//! `unreachable!`s on a `Value::Column`). Positional indexing `c[i] ->
//! Option[T]` is intercepted at the index sites (`eval_expr.rs` get,
//! typechecker `exprs.rs` typing) — the `[]` operator has no method to
//! dispatch through.
//!
//! A `Value::Column` carries a `data` element buffer plus a parallel
//! `valid` validity bitmap (one `bool` per slot; `false` = SQL null),
//! both `Arc<RwLock<…>>` shared cells so mutation through a cloned
//! receiver (the `mut ref self` push path) reaches the original binding
//! and par-block capture stays sound. The two Vecs are kept the same
//! length — the Arrow invariant — so `push_null` appends a never-read
//! `Value::Unit` placeholder to `data`.

use std::sync::{Arc, RwLock};

use crate::ast::{BinOp, CallArg};
use crate::interpreter::value::{EnumData, Value};
use crate::reduce_kernel::{quantile_linear_sorted, reduce_f64, ReduceOp, ReduceOutcome};
use crate::token::Span;

// The float-result statistics (`mean`/`var`/`std`/`median`/`quantile`/`corr`)
// read numeric slots as `f64`, and `min`/`max` keep the bare element type —
// both are shared with `Tensor` in `super::helpers`.
use super::helpers::minmax_value_reduce;
use super::helpers::value_as_f64 as val_f64;
use super::helpers::value_compare;

/// Unwrap a scalar [`ReduceOutcome`] (the float-result reductions) into a
/// `Value::Float`.
fn float_outcome(o: ReduceOutcome) -> Value {
    match o {
        ReduceOutcome::Scalar(f) => Value::Float(f),
        _ => unreachable!("column float reduction returned a non-scalar outcome"),
    }
}

/// Collect the valid (non-null) slot values of a column, in order — the
/// SQL/pandas posture for every aggregate (nulls are skipped).
fn collect_valid(data: &Arc<RwLock<Vec<Value>>>, valid: &Arc<RwLock<Vec<bool>>>) -> Vec<Value> {
    let d = data.read().unwrap();
    let v = valid.read().unwrap();
    v.iter()
        .zip(d.iter())
        .filter(|(&ok, _)| ok)
        .map(|(_, x)| x.clone())
        .collect()
}

/// Build an `Option[T]` value — `Some(v)` / `None`, the interpreter's
/// `Value::EnumVariant` Option representation.
fn some_value(v: Value) -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "Some".to_string(),
        data: EnumData::Tuple(vec![v]),
    }
}

fn none_value() -> Value {
    Value::EnumVariant {
        enum_name: "Option".to_string(),
        variant: "None".to_string(),
        data: EnumData::Unit,
    }
}

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
            // `from_iter_nullable(values)` — the argument is a
            // `Vec[Option[T]]` (a `Value::Array` of Option enum values):
            // `Some(v)` becomes a valid slot, `None` a null slot.
            "Column.from_iter_nullable" => {
                let arg = args.first()?;
                let Value::Array(rc) = self.eval_expr_inner(&arg.value) else {
                    return None;
                };
                let src = rc.read().unwrap();
                let mut data = Vec::with_capacity(src.len());
                let mut valid = Vec::with_capacity(src.len());
                for opt in src.iter() {
                    match opt {
                        Value::EnumVariant {
                            variant, data: ed, ..
                        } if variant == "Some" => {
                            let inner = match ed {
                                EnumData::Tuple(vs) if vs.len() == 1 => vs[0].clone(),
                                _ => Value::Unit,
                            };
                            data.push(inner);
                            valid.push(true);
                        }
                        // `None` (or any non-Some value) → a null slot.
                        _ => {
                            data.push(Value::Unit);
                            valid.push(false);
                        }
                    }
                }
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
            // Every slot as an Option[T], in order — a fresh Vec (copies).
            "iter" => {
                let data_guard = data.read().unwrap();
                let valid_guard = valid.read().unwrap();
                let out: Vec<Value> = valid_guard
                    .iter()
                    .zip(data_guard.iter())
                    .map(|(&ok, v)| {
                        if ok {
                            some_value(v.clone())
                        } else {
                            none_value()
                        }
                    })
                    .collect();
                Some(Value::Array(Arc::new(RwLock::new(out))))
            }
            // The valid slots only, unwrapped, in order — a fresh Vec.
            "iter_valid" => {
                let data_guard = data.read().unwrap();
                let valid_guard = valid.read().unwrap();
                let out: Vec<Value> = valid_guard
                    .iter()
                    .zip(data_guard.iter())
                    .filter(|(&ok, _)| ok)
                    .map(|(_, v)| v.clone())
                    .collect();
                Some(Value::Array(Arc::new(RwLock::new(out))))
            }
            // Replace every null slot with `value` → a fresh all-valid
            // column (the receiver is unchanged). `treat_nan_as_null = true`
            // additionally normalizes a float column's NaN slots (which are
            // bitmap-valid, not null) into fills — the opt-in NaN→null
            // surface (design.md § Data types); a no-op for non-float
            // elements. `value` is the leading positional arg; the flag is
            // the labeled / second positional arg (default `false`).
            "fillna" => {
                let value_arg = args
                    .iter()
                    .find(|a| a.label.as_deref() == Some("value"))
                    .or_else(|| args.iter().find(|a| a.label.is_none()))?;
                let fill = self.eval_expr_inner(&value_arg.value);
                let treat_nan = args
                    .iter()
                    .find(|a| a.label.as_deref() == Some("treat_nan_as_null"))
                    .or_else(|| args.iter().filter(|a| a.label.is_none()).nth(1))
                    .map(|a| matches!(self.eval_expr_inner(&a.value), Value::Bool(true)))
                    .unwrap_or(false);
                let data_guard = data.read().unwrap();
                let valid_guard = valid.read().unwrap();
                let out: Vec<Value> = valid_guard
                    .iter()
                    .zip(data_guard.iter())
                    .map(|(&ok, v)| {
                        let nullish =
                            !ok || (treat_nan && matches!(v, Value::Float(f) if f.is_nan()));
                        if nullish {
                            fill.clone()
                        } else {
                            v.clone()
                        }
                    })
                    .collect();
                let n = out.len();
                Some(Value::Column {
                    data: Arc::new(RwLock::new(out)),
                    valid: Arc::new(RwLock::new(vec![true; n])),
                })
            }
            // Drop null slots → a fresh all-valid column of the valid
            // values in order (the receiver is unchanged).
            "dropna" => {
                let data_guard = data.read().unwrap();
                let valid_guard = valid.read().unwrap();
                let out: Vec<Value> = valid_guard
                    .iter()
                    .zip(data_guard.iter())
                    .filter(|(&ok, _)| ok)
                    .map(|(_, v)| v.clone())
                    .collect();
                let n = out.len();
                Some(Value::Column {
                    data: Arc::new(RwLock::new(out)),
                    valid: Arc::new(RwLock::new(vec![true; n])),
                })
            }
            // Statistical reductions over the valid slots (nulls skipped —
            // SQL/pandas aggregate semantics). `sum`/`min`/`max` preserve the
            // element type `T`; `mean`/`var`/`std`/`median`/`quantile` always
            // yield `f64` (the numerical world promotes; `Value` can't
            // distinguish f32/f64 — the `Tensor.mean` rule). `corr` is the
            // Pearson correlation of two `Column[f64]`.
            "sum" | "prod" | "mean" | "min" | "max" | "var" | "std" | "median" | "quantile" => {
                Some(self.eval_column_reduce(method, data, valid, args, span))
            }
            // `Reduce[T]::range` default (`max - min`) — the trait method the
            // builtin `Column[T]` implementor doesn't get via the impl-splice, so
            // route it through the same `eval_column_reduce` min/max path (which
            // traps on an empty/all-null column just like `min`/`max`).
            "range" => {
                let mx = self.eval_column_reduce("max", data, valid, args, span);
                if self.pending_cf.is_some() {
                    return Some(Value::Unit);
                }
                let mn = self.eval_column_reduce("min", data, valid, args, span);
                if self.pending_cf.is_some() {
                    return Some(Value::Unit);
                }
                Some(self.eval_binary(&BinOp::Sub, mx, mn, span))
            }
            // General left-fold over the valid slots (nulls skipped, in
            // order): thread `init` through `f(acc, elem)`. An empty /
            // all-null column returns `init` unchanged — the fold identity,
            // with no empty trap (unlike `sum`/`min`/`max`). Typed by the
            // `fold` intercept in `src/typechecker/expr_method_call.rs`.
            "fold" => {
                if args.len() != 2 {
                    return Some(self.record_runtime_error(
                        format!(
                            "Column.fold expects 2 arguments (init, closure), got {}",
                            args.len()
                        ),
                        span,
                    ));
                }
                let mut acc = self.eval_expr_inner(&args[0].value);
                let f = self.eval_expr_inner(&args[1].value);
                if !matches!(f, Value::Function { .. }) {
                    return Some(self.record_runtime_error(
                        format!("Column.fold expects a closure as its second argument; got {f}"),
                        span,
                    ));
                }
                for x in collect_valid(data, valid) {
                    acc = self.invoke_function_value(f.clone(), vec![acc, x]);
                    if self.pending_cf.is_some() {
                        return Some(Value::Unit);
                    }
                }
                Some(acc)
            }
            // Element-wise map over the valid slots, producing a fresh column
            // of the same length; null slots pass through unchanged (their
            // value is a placeholder — the parallel `valid` bit keeps them
            // null). Typed by the `map` intercept (returns `Self`).
            "map" => {
                if args.len() != 1 {
                    return Some(self.record_runtime_error(
                        format!(
                            "Column.map expects 1 argument (closure), got {}",
                            args.len()
                        ),
                        span,
                    ));
                }
                let f = self.eval_expr_inner(&args[0].value);
                if !matches!(f, Value::Function { .. }) {
                    return Some(self.record_runtime_error(
                        format!("Column.map expects a closure as its argument; got {f}"),
                        span,
                    ));
                }
                // Clone the cells up front so no lock is held across the
                // closure call (which re-enters the interpreter).
                let cells: Vec<Value> = data.read().unwrap().clone();
                let bits: Vec<bool> = valid.read().unwrap().clone();
                let mut out = Vec::with_capacity(cells.len());
                for (i, x) in cells.into_iter().enumerate() {
                    if bits[i] {
                        let mapped = self.invoke_function_value(f.clone(), vec![x]);
                        if self.pending_cf.is_some() {
                            return Some(Value::Unit);
                        }
                        out.push(mapped);
                    } else {
                        out.push(x);
                    }
                }
                Some(Value::Column {
                    data: Arc::new(RwLock::new(out)),
                    valid: Arc::new(RwLock::new(bits)),
                })
            }
            "corr" => Some(self.eval_column_corr(data, valid, args, span)),
            // `argmin` / `argmax` -> `Option[i64]` (ElementwiseOrd, S6c): the
            // ORIGINAL slot index (Arrow position) of the first minimum /
            // maximum over the valid slots; null slots are skipped in the
            // comparison but never reported. Empty / all-null -> `None`
            // (mirroring `Stats.argmin`, unlike the `min`/`max` empty trap).
            // Typed by the `argmin`/`argmax` intercept (returns `Option[i64]`).
            "argmin" | "argmax" => {
                let cells = data.read().unwrap();
                let bits = valid.read().unwrap();
                let want_max = method == "argmax";
                let mut best: Option<(usize, Value)> = None;
                for (i, cell) in cells.iter().enumerate() {
                    if !bits[i] {
                        continue;
                    }
                    let take = match &best {
                        None => true,
                        Some((_, bv)) => {
                            let ord = value_compare(cell, bv);
                            // Strict compare keeps the FIRST occurrence: a later
                            // equal value never displaces the incumbent.
                            if want_max {
                                ord == std::cmp::Ordering::Greater
                            } else {
                                ord == std::cmp::Ordering::Less
                            }
                        }
                    };
                    if take {
                        best = Some((i, cell.clone()));
                    }
                }
                Some(match best {
                    Some((i, _)) => Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "Some".to_string(),
                        data: EnumData::Tuple(vec![Value::Int(i as i64)]),
                    },
                    None => Value::EnumVariant {
                        enum_name: "Option".to_string(),
                        variant: "None".to_string(),
                        data: EnumData::Unit,
                    },
                })
            }
            _ => None,
        }
    }

    /// The unary statistical reductions on `Column[T: Numeric]`. Operates on
    /// the valid slots only; an empty valid set traps (mirroring the Tensor
    /// empty-reduce trap, since `min`/`max` have no identity and the
    /// float-result forms would divide by zero). `var`/`std` are the
    /// **sample** (Bessel-corrected, `n-1`) forms — the pandas-Series /
    /// SQL-`stddev` default — so they additionally require ≥ 2 valid values.
    fn eval_column_reduce(
        &mut self,
        method: &str,
        data: &Arc<RwLock<Vec<Value>>>,
        valid: &Arc<RwLock<Vec<bool>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        let vals = collect_valid(data, valid);
        let n = vals.len();
        if n == 0 {
            return self.record_runtime_error(
                format!("cannot compute `{method}` on a column with no valid values"),
                span,
            );
        }
        match method {
            "sum" => {
                let mut acc = vals[0].clone();
                for x in vals.into_iter().skip(1) {
                    acc = self.eval_binary(&BinOp::Add, acc, x, span);
                    if self.pending_cf.is_some() {
                        return Value::Unit;
                    }
                }
                acc
            }
            "prod" => {
                let mut acc = vals[0].clone();
                for x in vals.into_iter().skip(1) {
                    acc = self.eval_binary(&BinOp::Mul, acc, x, span);
                    if self.pending_cf.is_some() {
                        return Value::Unit;
                    }
                }
                acc
            }
            "min" => minmax_value_reduce(true, vals),
            "max" => minmax_value_reduce(false, vals),
            // The f64-result reductions funnel through `crate::reduce_kernel`
            // (shared with `Stats.*`). `var`/`std` are the **sample** (Bessel,
            // ÷ n−1) forms, so `bessel: true` — distinct from `Stats`'
            // population form; the ≥ 2-valid-value guard the sample form needs
            // stays here at the call site.
            "mean" => {
                let xs: Vec<f64> = vals.iter().map(val_f64).collect();
                float_outcome(reduce_f64(&xs, ReduceOp::Mean))
            }
            "var" | "std" => {
                if n < 2 {
                    return self.record_runtime_error(
                        format!(
                            "`{method}` requires at least 2 valid values \
                             (sample variance is undefined for fewer)"
                        ),
                        span,
                    );
                }
                let xs: Vec<f64> = vals.iter().map(val_f64).collect();
                let op = if method == "std" {
                    ReduceOp::Std { bessel: true }
                } else {
                    ReduceOp::Var { bessel: true }
                };
                float_outcome(reduce_f64(&xs, op))
            }
            "median" => {
                let xs: Vec<f64> = vals.iter().map(val_f64).collect();
                float_outcome(reduce_f64(&xs, ReduceOp::Median))
            }
            "quantile" => {
                let q = match args.first().map(|a| self.eval_expr_inner(&a.value)) {
                    Some(Value::Float(q)) => q,
                    Some(Value::Int(i)) => i as f64,
                    _ => {
                        return self.record_runtime_error(
                            "Column.quantile expects a float argument in [0, 1]".to_string(),
                            span,
                        )
                    }
                };
                if !(0.0..=1.0).contains(&q) {
                    return self.record_runtime_error(
                        format!("Column.quantile q must be in [0, 1], got {q}"),
                        span,
                    );
                }
                let mut xs: Vec<f64> = vals.iter().map(val_f64).collect();
                xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                // `q ∈ [0, 1] → pos ∈ [0, n−1]` (vs `Stats.percentile`'s
                // `[0, 100]`); same linear-interpolation kernel.
                let pos = q * (n as f64 - 1.0);
                Value::Float(quantile_linear_sorted(&xs, pos))
            }
            _ => unreachable!("eval_column_reduce: unrouted method '{method}'"),
        }
    }

    /// `c.corr(other)` — Pearson correlation between two equal-length
    /// `Column[f64]`. Uses the slots where **both** columns are valid
    /// (pairwise-complete observations, the pandas posture); requires ≥ 2
    /// such pairs. A zero-variance operand yields `NaN` (pandas returns NaN
    /// rather than trapping). Length mismatch traps.
    fn eval_column_corr(
        &mut self,
        data: &Arc<RwLock<Vec<Value>>>,
        valid: &Arc<RwLock<Vec<bool>>>,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        let Some(arg) = args.first() else {
            return self
                .record_runtime_error("Column.corr expects one Column argument".to_string(), span);
        };
        let Value::Column {
            data: odata,
            valid: ovalid,
        } = self.eval_expr_inner(&arg.value)
        else {
            return self
                .record_runtime_error("Column.corr argument must be a Column".to_string(), span);
        };
        let d = data.read().unwrap();
        let v = valid.read().unwrap();
        let od = odata.read().unwrap();
        let ov = ovalid.read().unwrap();
        if d.len() != od.len() {
            return self.record_runtime_error(
                format!("Column.corr length mismatch: {} vs {}", d.len(), od.len()),
                span,
            );
        }
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for i in 0..d.len() {
            if v[i] && ov[i] {
                xs.push(val_f64(&d[i]));
                ys.push(val_f64(&od[i]));
            }
        }
        let n = xs.len();
        if n < 2 {
            return self.record_runtime_error(
                "Column.corr requires at least 2 valid paired values".to_string(),
                span,
            );
        }
        let mx = xs.iter().sum::<f64>() / n as f64;
        let my = ys.iter().sum::<f64>() / n as f64;
        let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
        for i in 0..n {
            let dx = xs[i] - mx;
            let dy = ys[i] - my;
            sxy += dx * dy;
            sxx += dx * dx;
            syy += dy * dy;
        }
        let denom = (sxx * syy).sqrt();
        let r = if denom == 0.0 { f64::NAN } else { sxy / denom };
        Value::Float(r)
    }
}
