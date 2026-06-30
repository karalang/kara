//! `DataFrame` interpreter MVP (phase-11 data-science stdlib, Arrow
//! commitment Q6). The `new` constructor is dispatched from
//! `eval_call.rs`; the instance methods (`insert` / `column` /
//! `has_column` / `column_names` / `width` / `height`) are intercepted
//! before the surface-only `#[compiler_builtin]` bodies in
//! `runtime/stdlib/dataframe.kara`. The typechecker types the same six in
//! `src/typechecker/expr_method_call.rs` (mirroring the Column intercept)
//! so `column` reads as `Column[T]` without leaning on baked dispatch.
//!
//! A `Value::DataFrame` carries an insertion-ordered `(name, Column)`
//! list — the order IS the Arrow schema order. The list rides an
//! `Arc<RwLock<…>>` shared cell, so a `mut ref self` mutation (`insert`)
//! through a cloned receiver reaches the original binding and par-block
//! capture stays sound; each stored `Value::Column` likewise shares its
//! own cells, so `column(name)` hands back a *view* (a clone of the
//! handle, not the data). Every column is kept the same length (the row
//! count / `height`) — the Arrow equal-length invariant, enforced at
//! `insert`.

use std::sync::{Arc, RwLock};

use crate::ast::CallArg;
use crate::interpreter::value::Value;
use crate::token::Span;

/// A deep, independent copy of a `Value::Column` — fresh `Arc` cells with
/// cloned contents, sharing nothing with the source. This is the
/// **value-semantics** the codegen lowering also uses (the frame owns its
/// columns outright; `insert` copies in, `column` copies out), so a
/// program that mutates a column after inserting / looking it up behaves
/// identically under `karac run` and `karac build` — no frame↔column
/// `Arc` aliasing to diverge on. (CoW buffer-sharing is a later
/// optimization, the documented Tensor/Column posture.) Non-Column values
/// fall back to a plain clone (defensive; callers only pass columns).
fn deep_copy_column(v: &Value) -> Value {
    match v {
        Value::Column { data, valid } => Value::Column {
            data: Arc::new(RwLock::new(data.read().unwrap().clone())),
            valid: Arc::new(RwLock::new(valid.read().unwrap().clone())),
        },
        other => other.clone(),
    }
}

/// The valid (non-null) values of a column as `f64`, **iff** the column is
/// numeric — every valid slot is an `Int` / `Float` and there is at least
/// one. Returns `None` for a non-numeric column or one with no valid value
/// (which `describe()` then skips, like pandas). The tree-walk interpreter
/// has no static element type, so numeric-ness is decided by inspecting the
/// values; the codegen slice will use the column's static element type
/// (the documented run/build reconciliation point for the all-null edge).
fn numeric_valid_f64(
    data: &Arc<RwLock<Vec<Value>>>,
    valid: &Arc<RwLock<Vec<bool>>>,
) -> Option<Vec<f64>> {
    let d = data.read().unwrap();
    let v = valid.read().unwrap();
    let mut out = Vec::new();
    for (&ok, x) in v.iter().zip(d.iter()) {
        if !ok {
            continue;
        }
        match x {
            Value::Int(i) => out.push(*i as f64),
            Value::Float(f) => out.push(*f),
            // A valid non-numeric slot disqualifies the whole column.
            _ => return None,
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The eight `describe()` statistics over a non-empty value list, in the
/// canonical order `[count, mean, std, min, 25%, 50%, 75%, max]`. `std` is
/// the **sample** (`n-1`) form — `NaN` for a single value (describe never
/// traps on size). Quartiles use NumPy/pandas linear interpolation,
/// matching the `Column.quantile` lowering.
fn describe_stats(vals: &[f64]) -> [f64; 8] {
    let n = vals.len();
    let count = n as f64;
    let sum: f64 = vals.iter().sum();
    let mean = sum / count;
    let std = if n >= 2 {
        let ss: f64 = vals
            .iter()
            .map(|x| {
                let d = x - mean;
                d * d
            })
            .sum();
        (ss / (count - 1.0)).sqrt()
    } else {
        f64::NAN
    };
    let mut sorted = vals.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let quantile = |p: f64| -> f64 {
        if n == 1 {
            return sorted[0];
        }
        let pos = p * (count - 1.0);
        let lo = pos.floor() as usize;
        let hi = pos.ceil() as usize;
        if lo == hi {
            sorted[lo]
        } else {
            let frac = pos - lo as f64;
            sorted[lo] + frac * (sorted[hi] - sorted[lo])
        }
    };
    [
        count,
        mean,
        std,
        sorted[0],
        quantile(0.25),
        quantile(0.5),
        quantile(0.75),
        sorted[n - 1],
    ]
}

/// Build an all-valid `Value::Column` from a value list.
fn all_valid_column(data: Vec<Value>) -> Value {
    let n = data.len();
    Value::Column {
        data: Arc::new(RwLock::new(data)),
        valid: Arc::new(RwLock::new(vec![true; n])),
    }
}

impl<'a> super::Interpreter<'a> {
    /// `DataFrame.new` constructor dispatched from `eval_call.rs`. Returns
    /// `None` for an unrecognized path (caller falls through).
    pub(super) fn eval_dataframe_new(&mut self, path_str: &str) -> Option<Value> {
        match path_str {
            "DataFrame.new" => Some(Value::DataFrame {
                columns: Arc::new(RwLock::new(Vec::new())),
            }),
            _ => None,
        }
    }

    /// Instance methods on `Value::DataFrame`. Returns `None` for a
    /// non-DataFrame receiver / unknown method (caller falls through).
    pub(super) fn try_eval_dataframe_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        let Value::DataFrame { columns } = obj else {
            return None;
        };
        match method {
            // Add or replace a named column. Args evaluated *before* the
            // `columns` write lock is taken (the column's own cells share
            // an Arc with the source binding — a view, mirroring Column).
            "insert" => {
                let name = match self.eval_expr_inner(&args.first()?.value) {
                    Value::String(s) => s,
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "DataFrame.insert name must be a String, got {}",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let col = self.eval_expr_inner(&args.get(1)?.value);
                let Value::Column { valid, .. } = &col else {
                    return Some(self.record_runtime_error(
                        format!(
                            "DataFrame.insert expects a Column, got {}",
                            col.variant_name()
                        ),
                        span,
                    ));
                };
                let col_len = valid.read().unwrap().len();
                let mut cols = columns.write().unwrap();
                // Equal-length (Arrow) invariant: a new column must match
                // the table's row count. Replacing a same-named column
                // doesn't change height, so measure against any *other*
                // existing column.
                if let Some(height) = cols.iter().find(|(n, _)| n != &name).map(|(_, c)| match c {
                    Value::Column { valid, .. } => valid.read().unwrap().len(),
                    _ => 0,
                }) {
                    if height != col_len {
                        return Some(self.record_runtime_error(
                            format!(
                                "DataFrame.insert column '{name}' has length {col_len} but the table has {height} row(s)"
                            ),
                            span,
                        ));
                    }
                }
                // Copy in — the frame owns an independent column (value
                // semantics; matches codegen).
                let owned = deep_copy_column(&col);
                if let Some(slot) = cols.iter_mut().find(|(n, _)| n == &name) {
                    slot.1 = owned;
                } else {
                    cols.push((name, owned));
                }
                Some(Value::Unit)
            }
            // Look up a column by name → a view (clone of the handle, so
            // the buffer is shared). Missing name is a runtime error.
            "column" => {
                let name = match self.eval_expr_inner(&args.first()?.value) {
                    Value::String(s) => s,
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "DataFrame.column name must be a String, got {}",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let cols = columns.read().unwrap();
                match cols.iter().find(|(n, _)| n == &name) {
                    // Copy out — the looked-up column is independent of the
                    // frame (value semantics; matches codegen).
                    Some((_, col)) => Some(deep_copy_column(col)),
                    None => Some(self.record_runtime_error(
                        format!("DataFrame.column: no column named '{name}'"),
                        span,
                    )),
                }
            }
            "has_column" => {
                let name = match self.eval_expr_inner(&args.first()?.value) {
                    Value::String(s) => s,
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "DataFrame.has_column name must be a String, got {}",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let cols = columns.read().unwrap();
                Some(Value::Bool(cols.iter().any(|(n, _)| n == &name)))
            }
            // Names in insertion (Arrow schema) order → `Vec[String]`.
            "column_names" => {
                let cols = columns.read().unwrap();
                let names: Vec<Value> =
                    cols.iter().map(|(n, _)| Value::String(n.clone())).collect();
                Some(Value::Array(Arc::new(RwLock::new(names))))
            }
            "width" => {
                let cols = columns.read().unwrap();
                Some(Value::Int(cols.len() as i64))
            }
            // Row count — every column's length (kept uniform by the
            // equal-length invariant); 0 for an empty table.
            "height" => {
                let cols = columns.read().unwrap();
                let h = cols.first().map_or(0, |(_, c)| match c {
                    Value::Column { valid, .. } => valid.read().unwrap().len(),
                    _ => 0,
                });
                Some(Value::Int(h as i64))
            }
            // A fresh table with only the named columns, in the given
            // order (subset / reorder; views share the source buffers).
            // A name absent from this table is a runtime error.
            "select" => {
                let Value::Array(rc) = self.eval_expr_inner(&args.first()?.value) else {
                    return Some(self.record_runtime_error(
                        "DataFrame.select expects a Vec[String] of column names",
                        span,
                    ));
                };
                let wanted = rc.read().unwrap();
                let cols = columns.read().unwrap();
                let mut picked: Vec<(String, Value)> = Vec::with_capacity(wanted.len());
                for nv in wanted.iter() {
                    let Value::String(name) = nv else {
                        return Some(self.record_runtime_error(
                            format!(
                                "DataFrame.select column name must be a String, got {}",
                                nv.variant_name()
                            ),
                            span,
                        ));
                    };
                    match cols.iter().find(|(n, _)| n == name) {
                        Some((_, col)) => picked.push((name.clone(), deep_copy_column(col))),
                        None => {
                            return Some(self.record_runtime_error(
                                format!("DataFrame.select: no column named '{name}'"),
                                span,
                            ));
                        }
                    }
                }
                Some(Value::DataFrame {
                    columns: Arc::new(RwLock::new(picked)),
                })
            }
            // Summary statistics over the numeric columns → a fresh table:
            // a leading `statistic` label column + one `Column[f64]` per
            // numeric source column (same name, source order). Non-numeric
            // / all-null columns are skipped (pandas posture). Always 8 rows.
            "describe" => {
                let cols = columns.read().unwrap();
                let labels = ["count", "mean", "std", "min", "25%", "50%", "75%", "max"];
                let stat_data: Vec<Value> = labels
                    .iter()
                    .map(|s| Value::String(s.to_string()))
                    .collect();
                let mut out: Vec<(String, Value)> =
                    vec![("statistic".to_string(), all_valid_column(stat_data))];
                for (name, col) in cols.iter() {
                    let Value::Column { data, valid } = col else {
                        continue;
                    };
                    let Some(vals) = numeric_valid_f64(data, valid) else {
                        continue;
                    };
                    let stats = describe_stats(&vals);
                    let cdata: Vec<Value> = stats.iter().map(|&x| Value::Float(x)).collect();
                    out.push((name.clone(), all_valid_column(cdata)));
                }
                Some(Value::DataFrame {
                    columns: Arc::new(RwLock::new(out)),
                })
            }
            _ => None,
        }
    }
}
