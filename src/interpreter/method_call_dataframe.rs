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
            // Serialize to a CSV file (phase-11 CSV leg, slice 1). Header row
            // = column names in schema order; one line per table row; cells
            // format like `println` (`Display` on the cell `Value`); a NULL
            // slot is an empty cell. RFC-4180-lite quoting: any cell or
            // header containing a comma, double-quote, CR, or LF is wrapped
            // in double quotes with embedded quotes doubled — numeric cells
            // never need it, so output stays minimal. Returns
            // `Result[Unit, IoError]`; write errors map through
            // `io_error_from_std` like the `File.*` arms.
            "write_csv" => {
                let path = match self.eval_expr_inner(&args.first()?.value) {
                    Value::String(s) => s,
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "DataFrame.write_csv path must be a String, got {}",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                self.track_effect("writes(FileSystem)");
                let cols = columns.read().unwrap();
                let quote = |cell: &str| -> String {
                    if cell.contains(',')
                        || cell.contains('"')
                        || cell.contains('\n')
                        || cell.contains('\r')
                    {
                        format!("\"{}\"", cell.replace('"', "\"\""))
                    } else {
                        cell.to_string()
                    }
                };
                let mut out = String::new();
                let header: Vec<String> = cols.iter().map(|(n, _)| quote(n)).collect();
                out.push_str(&header.join(","));
                out.push('\n');
                let height = cols.first().map_or(0, |(_, c)| match c {
                    Value::Column { valid, .. } => valid.read().unwrap().len(),
                    _ => 0,
                });
                for row in 0..height {
                    let mut cells: Vec<String> = Vec::with_capacity(cols.len());
                    for (_, col) in cols.iter() {
                        let Value::Column { data, valid } = col else {
                            cells.push(String::new());
                            continue;
                        };
                        let is_valid = valid.read().unwrap().get(row).copied().unwrap_or(false);
                        if !is_valid {
                            cells.push(String::new());
                            continue;
                        }
                        let cell = data
                            .read()
                            .unwrap()
                            .get(row)
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        cells.push(quote(&cell));
                    }
                    out.push_str(&cells.join(","));
                    out.push('\n');
                }
                use super::helpers::{io_err_value, io_error_from_std, io_ok};
                Some(match std::fs::write(&path, out) {
                    Ok(()) => io_ok(Value::Unit),
                    Err(e) => io_err_value(io_error_from_std(&e)),
                })
            }
            // Start a lazy query: a LazyFrame holding a live VIEW of this
            // frame's column list (the same Arc — eager mutations before
            // collect are visible, the Column view semantics) and an empty
            // plan. Phase-11 LazyDataFrame slice 1.
            "lazy" => Some(Value::LazyFrame {
                source: Arc::clone(columns),
                ops: Arc::new(Vec::new()),
            }),
            _ => None,
        }
    }

    /// `LazyFrame` methods (phase-11 LazyDataFrame, slices 1-2): the plan
    /// builders `select` / `limit` / `filter` (owned-self fluent chain —
    /// each clones the op list and pushes one step), `collect` (validate +
    /// run the plan, materializing an eager DataFrame), and `explain`
    /// (render the logical plan and its optimized pipeline form).
    /// Returns `None` for a non-LazyFrame receiver / unknown method.
    pub(super) fn try_eval_lazyframe_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        use crate::interpreter::value::LazyOp;
        let Value::LazyFrame { source, ops } = obj else {
            return None;
        };
        match method {
            "select" => {
                let Value::Array(rc) = self.eval_expr_inner(&args.first()?.value) else {
                    return Some(self.record_runtime_error(
                        "LazyFrame.select expects a Vec[String] of column names",
                        span,
                    ));
                };
                let mut cols: Vec<String> = Vec::new();
                for v in rc.read().unwrap().iter() {
                    match v {
                        Value::String(s) => cols.push(s.clone()),
                        other => {
                            return Some(self.record_runtime_error(
                                format!(
                                    "LazyFrame.select column names must be Strings, got {}",
                                    other.variant_name()
                                ),
                                span,
                            ));
                        }
                    }
                }
                let mut new_ops = ops.as_ref().clone();
                new_ops.push(LazyOp::Select(cols));
                Some(Value::LazyFrame {
                    source: Arc::clone(source),
                    ops: Arc::new(new_ops),
                })
            }
            "limit" => {
                let n = match self.eval_expr_inner(&args.first()?.value) {
                    Value::Int(n) => n.max(0),
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "LazyFrame.limit expects an i64, got {}",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                let mut new_ops = ops.as_ref().clone();
                new_ops.push(LazyOp::Limit(n));
                Some(Value::LazyFrame {
                    source: Arc::clone(source),
                    ops: Arc::new(new_ops),
                })
            }
            "filter" => {
                let Value::LazyExpr(ir) = self.eval_expr_inner(&args.first()?.value) else {
                    return Some(self.record_runtime_error(
                        "LazyFrame.filter expects a LazyExpr predicate (build one with \
                         LazyExpr.col(..) / std.lazy's col(..))",
                        span,
                    ));
                };
                let mut new_ops = ops.as_ref().clone();
                new_ops.push(LazyOp::Filter(ir));
                Some(Value::LazyFrame {
                    source: Arc::clone(source),
                    ops: Arc::new(new_ops),
                })
            }
            "with_columns" => {
                let Value::Array(rc) = self.eval_expr_inner(&args.first()?.value) else {
                    return Some(self.record_runtime_error(
                        "LazyFrame.with_columns expects a Vec[LazyExpr]",
                        span,
                    ));
                };
                let mut exprs = Vec::new();
                for v in rc.read().unwrap().iter() {
                    match v {
                        Value::LazyExpr(ir) => exprs.push(Arc::clone(ir)),
                        other => {
                            return Some(self.record_runtime_error(
                                format!(
                                    "LazyFrame.with_columns entries must be LazyExprs, got {}",
                                    other.variant_name()
                                ),
                                span,
                            ));
                        }
                    }
                }
                if exprs.is_empty() {
                    return Some(self.record_runtime_error(
                        "LazyFrame.with_columns needs at least one entry",
                        span,
                    ));
                }
                let mut new_ops = ops.as_ref().clone();
                new_ops.push(LazyOp::WithColumns(exprs));
                Some(Value::LazyFrame {
                    source: Arc::clone(source),
                    ops: Arc::new(new_ops),
                })
            }
            "sort" => {
                let Value::Array(rc) = self.eval_expr_inner(&args.first()?.value) else {
                    return Some(self.record_runtime_error(
                        "LazyFrame.sort expects a Vec[LazyExpr] of sort keys",
                        span,
                    ));
                };
                let mut keys = Vec::new();
                for v in rc.read().unwrap().iter() {
                    match v {
                        Value::LazyExpr(ir) => keys.push(Arc::clone(ir)),
                        other => {
                            return Some(self.record_runtime_error(
                                format!(
                                    "LazyFrame.sort keys must be LazyExprs, got {}",
                                    other.variant_name()
                                ),
                                span,
                            ));
                        }
                    }
                }
                if keys.is_empty() {
                    return Some(
                        self.record_runtime_error("LazyFrame.sort needs at least one key", span),
                    );
                }
                let mut new_ops = ops.as_ref().clone();
                new_ops.push(LazyOp::Sort(keys));
                Some(Value::LazyFrame {
                    source: Arc::clone(source),
                    ops: Arc::new(new_ops),
                })
            }
            "group_by" => {
                let Value::Array(rc) = self.eval_expr_inner(&args.first()?.value) else {
                    return Some(self.record_runtime_error(
                        "LazyFrame.group_by expects a Vec[LazyExpr] of key columns",
                        span,
                    ));
                };
                let mut keys = Vec::new();
                for v in rc.read().unwrap().iter() {
                    match v {
                        Value::LazyExpr(ir) => keys.push(Arc::clone(ir)),
                        other => {
                            return Some(self.record_runtime_error(
                                format!(
                                    "LazyFrame.group_by keys must be LazyExprs, got {}",
                                    other.variant_name()
                                ),
                                span,
                            ));
                        }
                    }
                }
                if keys.is_empty() {
                    return Some(
                        self.record_runtime_error(
                            "LazyFrame.group_by needs at least one key",
                            span,
                        ),
                    );
                }
                Some(Value::LazyGroupBy {
                    source: Arc::clone(source),
                    ops: Arc::clone(ops),
                    keys,
                })
            }
            "join" => {
                let other = self.eval_expr_inner(&args.first()?.value);
                let Value::LazyFrame {
                    source: rsource,
                    ops: rops,
                } = other
                else {
                    return Some(self.record_runtime_error(
                        "LazyFrame.join expects a LazyFrame as its first argument \
                         (build one with other_df.lazy())",
                        span,
                    ));
                };
                let Value::Array(rc) = self.eval_expr_inner(&args.get(1)?.value) else {
                    return Some(self.record_runtime_error(
                        "LazyFrame.join expects a Vec[String] of key column names",
                        span,
                    ));
                };
                let mut on = Vec::new();
                for v in rc.read().unwrap().iter() {
                    match v {
                        Value::String(s) => on.push(s.clone()),
                        other => {
                            return Some(self.record_runtime_error(
                                format!(
                                    "LazyFrame.join key names must be Strings, got {}",
                                    other.variant_name()
                                ),
                                span,
                            ));
                        }
                    }
                }
                if on.is_empty() {
                    return Some(
                        self.record_runtime_error("LazyFrame.join needs at least one key", span),
                    );
                }
                let mut new_ops = ops.as_ref().clone();
                new_ops.push(LazyOp::Join {
                    right_source: rsource,
                    right_ops: rops,
                    on,
                });
                Some(Value::LazyFrame {
                    source: Arc::clone(source),
                    ops: Arc::new(new_ops),
                })
            }
            "collect" => {
                // Validate the whole plan first (the fold is the single
                // validation authority), then run it — `eval_lazy_plan` is
                // a free fn so the Join arm can recurse into its right
                // sub-plan.
                if let Err(msg) = fold_lazy_plan(source, ops) {
                    return Some(self.record_runtime_error(msg, span));
                }
                match eval_lazy_plan(source, ops) {
                    Ok(cols) => Some(Value::DataFrame {
                        columns: Arc::new(RwLock::new(cols)),
                    }),
                    Err(msg) => Some(self.record_runtime_error(msg, span)),
                }
            }
            "explain" => {
                // Logical plan: innermost SCAN, one indented line per
                // recorded step; then the optimized pipeline form.
                // Deterministic — optimizer tests pin this byte-for-byte.
                let src = source.read().unwrap();
                let src_names: Vec<&str> = src.iter().map(|(n, _)| n.as_str()).collect();
                let mut lines: Vec<String> = vec![format!("SCAN [{}]", src_names.join(", "))];
                for op in ops.iter() {
                    let step = match op {
                        LazyOp::Select(cols) => format!("SELECT [{}]", cols.join(", ")),
                        LazyOp::Limit(n) => format!("LIMIT {n}"),
                        LazyOp::Filter(ir) => format!("FILTER {ir}"),
                        LazyOp::Sort(keys) => format!(
                            "SORT [{}]",
                            keys.iter()
                                .map(|k| k.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        LazyOp::GroupBy { keys, aggs } => format!(
                            "GROUP BY [{}] AGG [{}]",
                            keys.iter()
                                .map(|k| k.to_string())
                                .collect::<Vec<_>>()
                                .join(", "),
                            aggs.iter()
                                .map(|a| a.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        LazyOp::Join {
                            right_source,
                            right_ops,
                            on,
                        } => format!(
                            "JOIN on=[{}] right=({})",
                            on.join(", "),
                            lazy_logical_compact(right_source, right_ops)
                        ),
                        LazyOp::WithColumns(exprs) => format!(
                            "WITH [{}]",
                            exprs
                                .iter()
                                .map(|e| e.to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    lines.push(step);
                }
                let mut logical = String::new();
                for (i, line) in lines.iter().rev().enumerate() {
                    logical.push_str(&"  ".repeat(i));
                    logical.push_str(line);
                    logical.push('\n');
                }
                drop(src);
                let optimized = match fold_lazy_plan(source, ops) {
                    Ok(plan) => render_optimized_plan(&plan),
                    Err(msg) => format!("INVALID PLAN: {msg}"),
                };
                Some(Value::String(format!(
                    "== logical plan ==\n{logical}== optimized ==\n{optimized}"
                )))
            }
            _ => None,
        }
    }

    /// `LazyExpr` builder methods (phase-11 LazyDataFrame slice 2):
    /// the comparisons (`gt`/`ge`/`lt`/`le`/`eq`/`ne` — RHS a literal
    /// scalar or another expression) and the boolean combinators
    /// (`and_`/`or_`/`not_` — underscore-suffixed because `and`/`or`/`not`
    /// are Kāra keywords, the Polars-Python convention for the same
    /// collision). Returns `None` for a non-LazyExpr receiver.
    pub(super) fn try_eval_lazyexpr_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        use crate::interpreter::value::{LazyCmpOp, LazyExprIR};
        let Value::LazyExpr(ir) = obj else {
            return None;
        };
        let cmp = |op: LazyCmpOp| Some(op);
        let cmp_op = match method {
            "gt" => cmp(LazyCmpOp::Gt),
            "ge" => cmp(LazyCmpOp::Ge),
            "lt" => cmp(LazyCmpOp::Lt),
            "le" => cmp(LazyCmpOp::Le),
            "eq" => cmp(LazyCmpOp::Eq),
            "ne" => cmp(LazyCmpOp::Ne),
            _ => None,
        };
        if let Some(op) = cmp_op {
            let rhs_val = self.eval_expr_inner(&args.first()?.value);
            let rhs = match lazy_expr_ir_from_value(&rhs_val) {
                Ok(ir) => ir,
                Err(msg) => return Some(self.record_runtime_error(msg, span)),
            };
            return Some(Value::LazyExpr(Arc::new(LazyExprIR::Cmp {
                op,
                lhs: Box::new(ir.as_ref().clone()),
                rhs: Box::new(rhs),
            })));
        }
        let arith_op = {
            use crate::interpreter::value::LazyArithOp;
            match method {
                "add" => Some(LazyArithOp::Add),
                "sub" => Some(LazyArithOp::Sub),
                "mul" => Some(LazyArithOp::Mul),
                "div" => Some(LazyArithOp::Div),
                _ => None,
            }
        };
        if let Some(op) = arith_op {
            let rhs_val = self.eval_expr_inner(&args.first()?.value);
            let rhs = match lazy_expr_ir_from_value(&rhs_val) {
                Ok(ir) => ir,
                Err(msg) => return Some(self.record_runtime_error(msg, span)),
            };
            return Some(Value::LazyExpr(Arc::new(LazyExprIR::Arith {
                op,
                lhs: Box::new(ir.as_ref().clone()),
                rhs: Box::new(rhs),
            })));
        }
        match method {
            "and_" | "or_" => {
                let rhs_val = self.eval_expr_inner(&args.first()?.value);
                let Value::LazyExpr(rhs) = rhs_val else {
                    return Some(self.record_runtime_error(
                        format!("LazyExpr.{method} expects a LazyExpr argument"),
                        span,
                    ));
                };
                let a = Box::new(ir.as_ref().clone());
                let b = Box::new(rhs.as_ref().clone());
                let node = if method == "and_" {
                    LazyExprIR::And(a, b)
                } else {
                    LazyExprIR::Or(a, b)
                };
                Some(Value::LazyExpr(Arc::new(node)))
            }
            "not_" => Some(Value::LazyExpr(Arc::new(LazyExprIR::Not(Box::new(
                ir.as_ref().clone(),
            ))))),
            "desc" => Some(Value::LazyExpr(Arc::new(LazyExprIR::Desc(Box::new(
                ir.as_ref().clone(),
            ))))),
            "count" | "sum" | "mean" | "min" | "max" => {
                use crate::interpreter::value::LazyAggOp;
                let op = match method {
                    "count" => LazyAggOp::Count,
                    "sum" => LazyAggOp::Sum,
                    "mean" => LazyAggOp::Mean,
                    "min" => LazyAggOp::Min,
                    _ => LazyAggOp::Max,
                };
                Some(Value::LazyExpr(Arc::new(LazyExprIR::Agg {
                    op,
                    arg: Box::new(ir.as_ref().clone()),
                })))
            }
            "alias_" => {
                let name = match self.eval_expr_inner(&args.first()?.value) {
                    Value::String(s) => s,
                    other => {
                        return Some(self.record_runtime_error(
                            format!(
                                "LazyExpr.alias_ expects a String name, got {}",
                                other.variant_name()
                            ),
                            span,
                        ));
                    }
                };
                Some(Value::LazyExpr(Arc::new(LazyExprIR::Alias {
                    name,
                    expr: Box::new(ir.as_ref().clone()),
                })))
            }
            _ => None,
        }
    }

    /// `LazyGroupBy.agg(aggs)` (slice 4) — completes a pending grouping
    /// into a `LazyOp::GroupBy` plan step and returns the LazyFrame.
    /// Returns `None` for a non-LazyGroupBy receiver / unknown method.
    pub(super) fn try_eval_lazygroupby_method(
        &mut self,
        method: &str,
        obj: &Value,
        args: &[CallArg],
        span: &Span,
    ) -> Option<Value> {
        use crate::interpreter::value::LazyOp;
        let Value::LazyGroupBy { source, ops, keys } = obj else {
            return None;
        };
        if method != "agg" {
            return None;
        }
        let Value::Array(rc) = self.eval_expr_inner(&args.first()?.value) else {
            return Some(self.record_runtime_error(
                "LazyGroupBy.agg expects a Vec[LazyExpr] of aggregates",
                span,
            ));
        };
        let mut aggs = Vec::new();
        for v in rc.read().unwrap().iter() {
            match v {
                Value::LazyExpr(ir) => aggs.push(Arc::clone(ir)),
                other => {
                    return Some(self.record_runtime_error(
                        format!(
                            "LazyGroupBy.agg entries must be LazyExprs, got {}",
                            other.variant_name()
                        ),
                        span,
                    ));
                }
            }
        }
        if aggs.is_empty() {
            return Some(
                self.record_runtime_error("LazyGroupBy.agg needs at least one aggregate", span),
            );
        }
        let mut new_ops = ops.as_ref().clone();
        new_ops.push(LazyOp::GroupBy {
            keys: keys.clone(),
            aggs,
        });
        Some(Value::LazyFrame {
            source: Arc::clone(source),
            ops: Arc::new(new_ops),
        })
    }
}

/// Convert a comparison RHS `Value` into expression IR: a literal scalar
/// (i64 / f64 / String / bool) or another `LazyExpr` (column-vs-column).
fn lazy_expr_ir_from_value(v: &Value) -> Result<crate::interpreter::value::LazyExprIR, String> {
    use crate::interpreter::value::LazyExprIR;
    Ok(match v {
        Value::Int(n) => LazyExprIR::LitInt(*n),
        Value::Float(f) => LazyExprIR::LitFloat(*f),
        Value::String(s) => LazyExprIR::LitStr(s.clone()),
        Value::Bool(b) => LazyExprIR::LitBool(*b),
        Value::LazyExpr(ir) => ir.as_ref().clone(),
        other => {
            return Err(format!(
                "LazyExpr comparison expects a scalar literal (i64 / f64 / String / bool) \
                 or another LazyExpr, got {}",
                other.variant_name()
            ))
        }
    })
}

/// A scalar produced while evaluating a lazy expression over one row.
/// `None` (at the caller) marks a NULL slot.
enum LazyScalar {
    I(i64),
    F(f64),
    S(String),
    B(bool),
}

/// Evaluate an expression to a scalar over `columns` at `row`.
/// `Ok(None)` = a NULL column slot was read (comparisons against it are
/// FALSE — the documented simple semantics, not full three-valued logic).
fn eval_lazy_scalar(
    ir: &crate::interpreter::value::LazyExprIR,
    columns: &[(String, Value)],
    row: usize,
) -> Result<Option<LazyScalar>, String> {
    use crate::interpreter::value::LazyExprIR;
    Ok(match ir {
        LazyExprIR::Col(name) => {
            let Some((_, col)) = columns.iter().find(|(n, _)| n == name) else {
                return Err(format!("LazyFrame.filter: no column named '{name}'"));
            };
            let Value::Column { data, valid } = col else {
                return Err(format!("LazyFrame.filter: '{name}' is not a Column"));
            };
            if !valid.read().unwrap().get(row).copied().unwrap_or(false) {
                return Ok(None);
            }
            match &data.read().unwrap()[row] {
                Value::Int(n) => Some(LazyScalar::I(*n)),
                Value::Float(f) => Some(LazyScalar::F(*f)),
                Value::String(s) => Some(LazyScalar::S(s.clone())),
                Value::Bool(b) => Some(LazyScalar::B(*b)),
                other => {
                    return Err(format!(
                        "LazyFrame.filter: unsupported cell type {} in column '{name}'",
                        other.variant_name()
                    ))
                }
            }
        }
        LazyExprIR::LitInt(n) => Some(LazyScalar::I(*n)),
        LazyExprIR::LitFloat(f) => Some(LazyScalar::F(*f)),
        LazyExprIR::LitStr(s) => Some(LazyScalar::S(s.clone())),
        LazyExprIR::LitBool(b) => Some(LazyScalar::B(*b)),
        LazyExprIR::Desc(_) => {
            return Err("LazyExpr.desc() is only meaningful as a LazyFrame.sort key".to_string())
        }
        LazyExprIR::Agg { op, .. } => {
            return Err(format!(
                "LazyExpr.{}() is only meaningful inside LazyGroupBy.agg(..)",
                op.name()
            ))
        }
        LazyExprIR::Alias { .. } => {
            return Err(
                "LazyExpr.alias() is only meaningful inside LazyGroupBy.agg(..)".to_string(),
            )
        }
        LazyExprIR::Arith { op, lhs, rhs } => {
            use crate::interpreter::value::LazyArithOp;
            let (Some(a), Some(b)) = (
                eval_lazy_scalar(lhs, columns, row)?,
                eval_lazy_scalar(rhs, columns, row)?,
            ) else {
                return Ok(None); // NULL on either side → NULL result
            };
            match (&a, &b) {
                (LazyScalar::I(x), LazyScalar::I(y)) => {
                    // i64 pairs stay i64; division by zero and overflow
                    // are loud (matching the language's scalar posture).
                    let v = match op {
                        LazyArithOp::Add => x.checked_add(*y),
                        LazyArithOp::Sub => x.checked_sub(*y),
                        LazyArithOp::Mul => x.checked_mul(*y),
                        LazyArithOp::Div => {
                            if *y == 0 {
                                return Err(
                                    "LazyExpr: integer division by zero in expression".to_string()
                                );
                            }
                            x.checked_div(*y)
                        }
                    };
                    match v {
                        Some(v) => Some(LazyScalar::I(v)),
                        None => return Err("LazyExpr: integer overflow in expression".to_string()),
                    }
                }
                (LazyScalar::I(_) | LazyScalar::F(_), LazyScalar::I(_) | LazyScalar::F(_)) => {
                    let x = match &a {
                        LazyScalar::I(v) => *v as f64,
                        LazyScalar::F(v) => *v,
                        _ => unreachable!(),
                    };
                    let y = match &b {
                        LazyScalar::I(v) => *v as f64,
                        LazyScalar::F(v) => *v,
                        _ => unreachable!(),
                    };
                    Some(LazyScalar::F(match op {
                        LazyArithOp::Add => x + y,
                        LazyArithOp::Sub => x - y,
                        LazyArithOp::Mul => x * y,
                        LazyArithOp::Div => x / y, // IEEE: /0 → inf/NaN
                    }))
                }
                _ => {
                    return Err(
                        "LazyExpr: arithmetic on non-numeric values (String / bool)".to_string()
                    )
                }
            }
        }
        // Bool-valued sub-expressions evaluate through the predicate path.
        LazyExprIR::Cmp { .. } | LazyExprIR::And(..) | LazyExprIR::Or(..) | LazyExprIR::Not(..) => {
            Some(LazyScalar::B(eval_lazy_pred(ir, columns, row)?))
        }
    })
}

/// Evaluate a predicate (bool-valued) expression over one row. A NULL
/// slot makes the enclosing COMPARISON false; `and`/`or` short-circuit;
/// a non-boolean tree at predicate position is an error.
fn eval_lazy_pred(
    ir: &crate::interpreter::value::LazyExprIR,
    columns: &[(String, Value)],
    row: usize,
) -> Result<bool, String> {
    use crate::interpreter::value::{LazyCmpOp, LazyExprIR};
    match ir {
        LazyExprIR::Cmp { op, lhs, rhs } => {
            let (Some(a), Some(b)) = (
                eval_lazy_scalar(lhs, columns, row)?,
                eval_lazy_scalar(rhs, columns, row)?,
            ) else {
                return Ok(false); // NULL involved → comparison is false
            };
            let ord = match (&a, &b) {
                (LazyScalar::I(x), LazyScalar::I(y)) => x.partial_cmp(y),
                (LazyScalar::F(x), LazyScalar::F(y)) => x.partial_cmp(y),
                (LazyScalar::I(x), LazyScalar::F(y)) => (*x as f64).partial_cmp(y),
                (LazyScalar::F(x), LazyScalar::I(y)) => x.partial_cmp(&(*y as f64)),
                (LazyScalar::S(x), LazyScalar::S(y)) => Some(x.cmp(y)),
                (LazyScalar::B(x), LazyScalar::B(y)) => {
                    if matches!(op, LazyCmpOp::Eq | LazyCmpOp::Ne) {
                        Some(x.cmp(y))
                    } else {
                        return Err(
                            "LazyFrame.filter: ordered comparison on bool values".to_string()
                        );
                    }
                }
                _ => {
                    return Err(
                        "LazyFrame.filter: cannot compare values of different types \
                         (String vs number / bool vs non-bool)"
                            .to_string(),
                    )
                }
            };
            let Some(ord) = ord else {
                return Ok(false); // NaN comparisons are false (IEEE posture)
            };
            Ok(match op {
                LazyCmpOp::Gt => ord.is_gt(),
                LazyCmpOp::Ge => ord.is_ge(),
                LazyCmpOp::Lt => ord.is_lt(),
                LazyCmpOp::Le => ord.is_le(),
                LazyCmpOp::Eq => ord.is_eq(),
                LazyCmpOp::Ne => ord.is_ne(),
            })
        }
        LazyExprIR::And(a, b) => {
            Ok(eval_lazy_pred(a, columns, row)? && eval_lazy_pred(b, columns, row)?)
        }
        LazyExprIR::Or(a, b) => {
            Ok(eval_lazy_pred(a, columns, row)? || eval_lazy_pred(b, columns, row)?)
        }
        LazyExprIR::Not(x) => Ok(!eval_lazy_pred(x, columns, row)?),
        LazyExprIR::LitBool(b) => Ok(*b),
        LazyExprIR::Col(_) => match eval_lazy_scalar(ir, columns, row)? {
            Some(LazyScalar::B(b)) => Ok(b),
            Some(_) => Err(
                "LazyFrame.filter: predicate must be boolean (a bare column reference \
                 must name a bool column)"
                    .to_string(),
            ),
            None => Ok(false),
        },
        _ => Err("LazyFrame.filter: predicate must be boolean".to_string()),
    }
}

/// One evaluated sort-key scalar, ordered by `cmp_lazy_sort_keys`.
enum LazySortKey {
    Null,
    Nan,
    I(i64),
    F(f64),
    S(String),
    B(bool),
}

/// Evaluate one sort key for one row: strip a `Desc` wrapper (the
/// comparator re-checks the direction), evaluate the inner expression,
/// and normalize NULL / NaN into ranks (0 = value, 1 = NaN, 2 = NULL —
/// NULLs last, never reversed, so they stay last under `desc` too).
fn eval_lazy_sort_key(
    key: &crate::interpreter::value::LazyExprIR,
    columns: &[(String, Value)],
    row: usize,
) -> Result<(u8, LazySortKey), String> {
    use crate::interpreter::value::LazyExprIR;
    let inner = match key {
        LazyExprIR::Desc(x) => x.as_ref(),
        other => other,
    };
    Ok(match eval_lazy_scalar(inner, columns, row)? {
        None => (2, LazySortKey::Null),
        Some(LazyScalar::F(f)) if f.is_nan() => (1, LazySortKey::Nan),
        Some(LazyScalar::I(v)) => (0, LazySortKey::I(v)),
        Some(LazyScalar::F(v)) => (0, LazySortKey::F(v)),
        Some(LazyScalar::S(v)) => (0, LazySortKey::S(v)),
        Some(LazyScalar::B(v)) => (0, LazySortKey::B(v)),
    })
}

/// Multi-key comparator: keys in order; per key, rank first (values <
/// NaN < NULL — never reversed), then the value comparison, reversed for
/// a `Desc`-wrapped key. Mixed value types within one key can only arise
/// from computed expressions over mixed columns — ordered by type tag
/// (deterministic; homogeneous columns never hit it).
fn cmp_lazy_sort_keys(
    keys: &[Arc<crate::interpreter::value::LazyExprIR>],
    a: &[(u8, LazySortKey)],
    b: &[(u8, LazySortKey)],
) -> std::cmp::Ordering {
    use crate::interpreter::value::LazyExprIR;
    use std::cmp::Ordering;
    for (i, key) in keys.iter().enumerate() {
        let desc = matches!(key.as_ref(), LazyExprIR::Desc(_));
        let (ra, ka) = &a[i];
        let (rb, kb) = &b[i];
        let ord = match ra.cmp(rb) {
            Ordering::Equal if *ra == 0 => {
                let v = match (ka, kb) {
                    (LazySortKey::I(x), LazySortKey::I(y)) => x.cmp(y),
                    (LazySortKey::F(x), LazySortKey::F(y)) => {
                        x.partial_cmp(y).unwrap_or(Ordering::Equal)
                    }
                    (LazySortKey::I(x), LazySortKey::F(y)) => {
                        (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)
                    }
                    (LazySortKey::F(x), LazySortKey::I(y)) => {
                        x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)
                    }
                    (LazySortKey::S(x), LazySortKey::S(y)) => x.cmp(y),
                    (LazySortKey::B(x), LazySortKey::B(y)) => x.cmp(y),
                    _ => sort_key_type_tag(ka).cmp(&sort_key_type_tag(kb)),
                };
                if desc {
                    v.reverse()
                } else {
                    v
                }
            }
            other => other, // rank order never reverses (NULLs stay last)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn sort_key_type_tag(k: &LazySortKey) -> u8 {
    match k {
        LazySortKey::B(_) => 0,
        LazySortKey::I(_) => 1,
        LazySortKey::F(_) => 2,
        LazySortKey::S(_) => 3,
        LazySortKey::Nan => 4,
        LazySortKey::Null => 5,
    }
}

/// Run a lazy plan to materialized output columns — the recursive
/// evaluation core shared by `collect` and a JOIN parent evaluating its
/// right sub-plan. Row-index pipeline over the ops IN ORDER (the
/// optimizer only feeds `explain`); `GroupBy` and `Join` REPLACE the
/// working column set mid-pipeline.
fn eval_lazy_plan(
    source: &Arc<RwLock<Vec<(String, Value)>>>,
    ops: &Arc<Vec<crate::interpreter::value::LazyOp>>,
) -> Result<Vec<(String, Value)>, String> {
    use crate::interpreter::value::LazyOp;
    let mut cur: Vec<(String, Value)> = source.read().unwrap().clone();
    let mut height = lazy_cols_height(&cur);
    let mut indices: Vec<usize> = (0..height).collect();
    let mut projection: Option<Vec<String>> = None;
    let mut row_ops = false;
    for op in ops.iter() {
        match op {
            LazyOp::Select(cols) => projection = Some(cols.clone()),
            LazyOp::Limit(n) => {
                row_ops = true;
                indices.truncate((*n).max(0) as usize);
            }
            LazyOp::Filter(ir) => {
                row_ops = true;
                let mut kept = Vec::with_capacity(indices.len());
                for &row in &indices {
                    if eval_lazy_pred(ir, &cur, row)? {
                        kept.push(row);
                    }
                }
                indices = kept;
            }
            LazyOp::Sort(keys) => {
                row_ops = true;
                let mut keyed: Vec<(usize, Vec<(u8, LazySortKey)>)> =
                    Vec::with_capacity(indices.len());
                for &row in &indices {
                    let mut kvs = Vec::with_capacity(keys.len());
                    for k in keys {
                        kvs.push(eval_lazy_sort_key(k, &cur, row)?);
                    }
                    keyed.push((row, kvs));
                }
                keyed.sort_by(|(_, a), (_, b)| cmp_lazy_sort_keys(keys, a, b));
                indices = keyed.into_iter().map(|(row, _)| row).collect();
            }
            LazyOp::GroupBy { keys, aggs } => {
                cur = eval_lazy_group_by(keys, aggs, &cur, &indices)?;
                height = lazy_cols_height(&cur);
                indices = (0..height).collect();
                row_ops = false;
                projection = None;
            }
            LazyOp::Join {
                right_source,
                right_ops,
                on,
            } => {
                // Materialize the LEFT state (applying any pending
                // projection — fold narrows the schema at the join the
                // same way), evaluate the RIGHT sub-plan recursively,
                // then inner-join.
                let left = materialize_lazy_cols(&cur, &indices, &projection, true)?;
                let right = eval_lazy_plan(right_source, right_ops)?;
                cur = eval_lazy_join(&left, &right, on)?;
                height = lazy_cols_height(&cur);
                indices = (0..height).collect();
                row_ops = false;
                projection = None;
            }
            LazyOp::WithColumns(exprs) => {
                // Materialize the current state (pending projection
                // applied — fold flushes it the same way), compute every
                // entry against that INPUT frame (the Polars parallel
                // semantics — entries never see each other), then
                // replace-or-append by output name.
                cur = materialize_lazy_cols(&cur, &indices, &projection, true)?;
                height = lazy_cols_height(&cur);
                indices = (0..height).collect();
                row_ops = false;
                projection = None;
                let mut computed: Vec<(String, Value)> = Vec::with_capacity(exprs.len());
                for e in exprs.iter() {
                    let (name, inner) = with_columns_output(e)?;
                    let mut data = Vec::with_capacity(height);
                    let mut validv = Vec::with_capacity(height);
                    for row in 0..height {
                        match eval_lazy_scalar(inner, &cur, row)? {
                            Some(LazyScalar::I(v)) => {
                                data.push(Value::Int(v));
                                validv.push(true);
                            }
                            Some(LazyScalar::F(v)) => {
                                data.push(Value::Float(v));
                                validv.push(true);
                            }
                            Some(LazyScalar::S(v)) => {
                                data.push(Value::String(v));
                                validv.push(true);
                            }
                            Some(LazyScalar::B(v)) => {
                                data.push(Value::Bool(v));
                                validv.push(true);
                            }
                            None => {
                                data.push(Value::Unit);
                                validv.push(false);
                            }
                        }
                    }
                    computed.push((
                        name,
                        Value::Column {
                            data: Arc::new(RwLock::new(data)),
                            valid: Arc::new(RwLock::new(validv)),
                        },
                    ));
                }
                for (name, col) in computed {
                    match cur.iter_mut().find(|(n, _)| n == &name) {
                        Some(slot) => slot.1 = col,
                        None => cur.push((name, col)),
                    }
                }
            }
        }
    }
    materialize_lazy_cols(&cur, &indices, &projection, row_ops)
}

/// The output name of one `with_columns` entry and the expression that
/// computes it: a bare `col(..)` keeps its own name (a same-named
/// replace — useful after `alias_`-free type coercions land; today an
/// identity copy), anything else must be `.alias_(..)`ed.
fn with_columns_output(
    e: &crate::interpreter::value::LazyExprIR,
) -> Result<(String, &crate::interpreter::value::LazyExprIR), String> {
    use crate::interpreter::value::LazyExprIR;
    match e {
        LazyExprIR::Alias { name, expr } => Ok((name.clone(), expr.as_ref())),
        LazyExprIR::Col(n) => Ok((n.clone(), e)),
        _ => Err(
            "LazyFrame.with_columns: each entry needs an output name — a bare col(..) keeps \
             its own, anything computed must be .alias_(..)ed"
                .to_string(),
        ),
    }
}

/// The row count of a column list (0 when empty).
fn lazy_cols_height(cols: &[(String, Value)]) -> usize {
    cols.first().map_or(0, |(_, c)| match c {
        Value::Column { valid, .. } => valid.read().unwrap().len(),
        _ => 0,
    })
}

/// Gather the surviving rows / projection into output columns. With no
/// row ops the projected columns are handed back as VIEWS (the eager-
/// `select` sharing semantics); otherwise fresh gathered cells.
fn materialize_lazy_cols(
    cur: &[(String, Value)],
    indices: &[usize],
    projection: &Option<Vec<String>>,
    row_ops: bool,
) -> Result<Vec<(String, Value)>, String> {
    let names: Vec<String> = match projection {
        Some(cols) => cols.clone(),
        None => cur.iter().map(|(n, _)| n.clone()).collect(),
    };
    let mut out: Vec<(String, Value)> = Vec::with_capacity(names.len());
    for name in names {
        let Some((_, col)) = cur.iter().find(|(n, _)| n == &name) else {
            return Err(format!("LazyFrame.collect: no column named '{name}'"));
        };
        let out_col = if row_ops {
            if let Value::Column { data, valid } = col {
                let d = data.read().unwrap();
                let v = valid.read().unwrap();
                let nd: Vec<Value> = indices.iter().map(|&i| d[i].clone()).collect();
                let nv: Vec<bool> = indices.iter().map(|&i| v[i]).collect();
                Value::Column {
                    data: Arc::new(RwLock::new(nd)),
                    valid: Arc::new(RwLock::new(nv)),
                }
            } else {
                col.clone()
            }
        } else {
            col.clone()
        };
        out.push((name, out_col));
    }
    Ok(out)
}

/// Inner join two materialized column sets on equal-named keys. Left
/// row order, then right match order (nested loop — MVP scale,
/// deterministic). NULL keys join nothing. Output: left columns, then
/// right non-key columns (`_right` suffix on collisions).
fn eval_lazy_join(
    left: &[(String, Value)],
    right: &[(String, Value)],
    on: &[String],
) -> Result<Vec<(String, Value)>, String> {
    let lh = lazy_cols_height(left);
    let rh = lazy_cols_height(right);
    // Loud on incompatible key types across the two sides — a
    // String-vs-number key pair would otherwise silently join nothing
    // (the "loud, not empty" rule the filter path already follows). An
    // i64/f64 mix is fine: keys compare numerically, like filter/sort.
    let key_family = |cols: &[(String, Value)], h: usize, k: &str| -> Result<Option<u8>, String> {
        use crate::interpreter::value::LazyExprIR;
        for row in 0..h {
            let (rank, sk) = eval_lazy_sort_key(&LazyExprIR::Col(k.to_string()), cols, row)?;
            if rank == 2 {
                continue; // NULL — keep scanning for a typed value
            }
            return Ok(Some(match sk {
                LazySortKey::S(_) => 1,
                LazySortKey::B(_) => 2,
                _ => 0, // numeric family: I / F / NaN
            }));
        }
        Ok(None)
    };
    for k in on {
        if let (Some(lf), Some(rf)) = (key_family(left, lh, k)?, key_family(right, rh, k)?) {
            if lf != rf {
                return Err(format!(
                    "LazyFrame.join: key '{k}' has incompatible types on the two sides"
                ));
            }
        }
    }
    // Key tuple per row, `None` when any key slot is NULL.
    let key_tuple =
        |cols: &[(String, Value)], row: usize| -> Result<Option<Vec<(u8, LazySortKey)>>, String> {
            let mut kt = Vec::with_capacity(on.len());
            for k in on {
                use crate::interpreter::value::LazyExprIR;
                let (rank, sk) = eval_lazy_sort_key(&LazyExprIR::Col(k.clone()), cols, row)?;
                if rank == 2 {
                    return Ok(None); // NULL key joins nothing
                }
                kt.push((rank, sk));
            }
            Ok(Some(kt))
        };
    let mut lrows: Vec<usize> = Vec::new();
    let mut rrows: Vec<usize> = Vec::new();
    for lrow in 0..lh {
        let Some(lk) = key_tuple(left, lrow)? else {
            continue;
        };
        for rrow in 0..rh {
            let Some(rk) = key_tuple(right, rrow)? else {
                continue;
            };
            if sort_keys_equal(&lk, &rk) {
                lrows.push(lrow);
                rrows.push(rrow);
            }
        }
    }
    let gather = |col: &Value, rows: &[usize]| -> Value {
        if let Value::Column { data, valid } = col {
            let d = data.read().unwrap();
            let v = valid.read().unwrap();
            Value::Column {
                data: Arc::new(RwLock::new(rows.iter().map(|&i| d[i].clone()).collect())),
                valid: Arc::new(RwLock::new(rows.iter().map(|&i| v[i]).collect())),
            }
        } else {
            col.clone()
        }
    };
    let mut out: Vec<(String, Value)> = Vec::new();
    for (n, c) in left {
        out.push((n.clone(), gather(c, &lrows)));
    }
    let left_names: Vec<&String> = left.iter().map(|(n, _)| n).collect();
    for (n, c) in right {
        if on.contains(n) {
            continue;
        }
        let name = if left_names.contains(&n) {
            format!("{n}_right")
        } else {
            n.clone()
        };
        out.push((name, gather(c, &rrows)));
    }
    Ok(out)
}

/// The right sub-plan's LOGICAL rendering flattened to one line
/// (outermost step first, " <- " separated) — the v1 JOIN rendering
/// (a real two-child tree layout is the P2 explain expansion).
fn lazy_logical_compact(
    source: &Arc<RwLock<Vec<(String, Value)>>>,
    ops: &Arc<Vec<crate::interpreter::value::LazyOp>>,
) -> String {
    use crate::interpreter::value::LazyOp;
    let src = source.read().unwrap();
    let src_names: Vec<&str> = src.iter().map(|(n, _)| n.as_str()).collect();
    let mut lines: Vec<String> = vec![format!("SCAN [{}]", src_names.join(", "))];
    drop(src);
    for op in ops.iter() {
        let step = match op {
            LazyOp::Select(cols) => format!("SELECT [{}]", cols.join(", ")),
            LazyOp::Limit(n) => format!("LIMIT {n}"),
            LazyOp::Filter(ir) => format!("FILTER {ir}"),
            LazyOp::Sort(keys) => format!(
                "SORT [{}]",
                keys.iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            LazyOp::GroupBy { keys, aggs } => format!(
                "GROUP BY [{}] AGG [{}]",
                keys.iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                aggs.iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            LazyOp::Join {
                right_source,
                right_ops,
                on,
            } => format!(
                "JOIN on=[{}] right=({})",
                on.join(", "),
                lazy_logical_compact(right_source, right_ops)
            ),
            LazyOp::WithColumns(exprs) => format!(
                "WITH [{}]",
                exprs
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        lines.push(step);
    }
    lines.reverse();
    lines.join(" <- ")
}

/// The right sub-plan's OPTIMIZED rendering flattened to one line —
/// compact twin of `render_optimized_plan` for the JOIN label. An
/// invalid sub-plan renders its message (the parent fold surfaces the
/// error before collect runs).
fn lazy_optimized_compact(
    source: &Arc<RwLock<Vec<(String, Value)>>>,
    ops: &Arc<Vec<crate::interpreter::value::LazyOp>>,
) -> String {
    match fold_lazy_plan(source, ops) {
        Ok(plan) => {
            let rendered = render_optimized_plan(&plan);
            rendered
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" <- ")
        }
        Err(msg) => format!("INVALID: {msg}"),
    }
}

/// The output-column name of a group KEY expression — v1: a bare
/// `col(..)`, optionally `alias`ed.
fn lazy_group_key_name(k: &crate::interpreter::value::LazyExprIR) -> Result<String, String> {
    use crate::interpreter::value::LazyExprIR;
    match k {
        LazyExprIR::Col(n) => Ok(n.clone()),
        LazyExprIR::Alias { name, expr } => match expr.as_ref() {
            LazyExprIR::Col(_) => Ok(name.clone()),
            _ => Err("LazyFrame.group_by: each key must be a bare col(..) in v1".to_string()),
        },
        _ => Err("LazyFrame.group_by: each key must be a bare col(..) in v1".to_string()),
    }
}

/// The output-column name of an aggregate expression: `alias` wins, else
/// `<col>_<op>` (`score_sum`). Non-aggregate entries are an error.
fn lazy_agg_output_name(a: &crate::interpreter::value::LazyExprIR) -> Result<String, String> {
    use crate::interpreter::value::LazyExprIR;
    match a {
        LazyExprIR::Alias { name, expr } => match expr.as_ref() {
            LazyExprIR::Agg { .. } => Ok(name.clone()),
            _ => Err(
                "LazyGroupBy.agg: each entry must be an aggregate (col(..).count() / \
                 sum / mean / min / max), optionally aliased"
                    .to_string(),
            ),
        },
        LazyExprIR::Agg { op, arg } => match arg.as_ref() {
            LazyExprIR::Col(c) => Ok(format!("{c}_{}", op.name())),
            _ => Err(
                "LazyGroupBy.agg: the aggregate argument must be a bare col(..) in v1".to_string(),
            ),
        },
        _ => Err(
            "LazyGroupBy.agg: each entry must be an aggregate (col(..).count() / sum / \
             mean / min / max), optionally aliased"
                .to_string(),
        ),
    }
}

/// Evaluate a GroupBy step: group the surviving rows by the evaluated
/// key scalars (first-occurrence order; NULL keys group together), then
/// compute each aggregate per group. Returns the materialized output
/// columns: keys first, then one column per aggregate.
fn eval_lazy_group_by(
    keys: &[Arc<crate::interpreter::value::LazyExprIR>],
    aggs: &[Arc<crate::interpreter::value::LazyExprIR>],
    cur: &[(String, Value)],
    indices: &[usize],
) -> Result<Vec<(String, Value)>, String> {
    use crate::interpreter::value::LazyExprIR;
    // 1. Group rows by key tuples (rank+scalar reuses the sort-key
    //    normalization so NULL/NaN keys group deterministically).
    type GroupEntry = (Vec<(u8, LazySortKey)>, Vec<usize>);
    let mut groups: Vec<GroupEntry> = Vec::new();
    for &row in indices {
        let mut kt = Vec::with_capacity(keys.len());
        for k in keys {
            kt.push(eval_lazy_sort_key(k, cur, row)?);
        }
        match groups.iter_mut().find(|(g, _)| sort_keys_equal(g, &kt)) {
            Some((_, rows)) => rows.push(row),
            None => groups.push((kt, vec![row])),
        }
    }
    let n_groups = groups.len();
    let mut out: Vec<(String, Value)> = Vec::new();
    // 2. Key columns — the representative (first) row's stored cell.
    for (ki, k) in keys.iter().enumerate() {
        let name = lazy_group_key_name(k)?;
        let mut data = Vec::with_capacity(n_groups);
        let mut valid = Vec::with_capacity(n_groups);
        for (kt, _) in &groups {
            match &kt[ki] {
                (2, _) => {
                    data.push(Value::Unit);
                    valid.push(false);
                }
                (_, LazySortKey::I(v)) => {
                    data.push(Value::Int(*v));
                    valid.push(true);
                }
                (_, LazySortKey::F(v)) => {
                    data.push(Value::Float(*v));
                    valid.push(true);
                }
                (_, LazySortKey::S(v)) => {
                    data.push(Value::String(v.clone()));
                    valid.push(true);
                }
                (_, LazySortKey::B(v)) => {
                    data.push(Value::Bool(*v));
                    valid.push(true);
                }
                (_, LazySortKey::Nan) => {
                    data.push(Value::Float(f64::NAN));
                    valid.push(true);
                }
                (_, LazySortKey::Null) => {
                    data.push(Value::Unit);
                    valid.push(false);
                }
            }
        }
        out.push((
            name,
            Value::Column {
                data: Arc::new(RwLock::new(data)),
                valid: Arc::new(RwLock::new(valid)),
            },
        ));
    }
    // 3. Aggregate columns.
    for a in aggs {
        let name = lazy_agg_output_name(a)?;
        let (op, arg) = match a.as_ref() {
            LazyExprIR::Alias { expr, .. } => match expr.as_ref() {
                LazyExprIR::Agg { op, arg } => (*op, arg.as_ref()),
                _ => unreachable!("validated by lazy_agg_output_name"),
            },
            LazyExprIR::Agg { op, arg } => (*op, arg.as_ref()),
            _ => unreachable!("validated by lazy_agg_output_name"),
        };
        let mut data = Vec::with_capacity(n_groups);
        let mut valid = Vec::with_capacity(n_groups);
        for (_, rows) in &groups {
            let (v, ok) = eval_lazy_agg(op, arg, cur, rows)?;
            data.push(v);
            valid.push(ok);
        }
        out.push((
            name,
            Value::Column {
                data: Arc::new(RwLock::new(data)),
                valid: Arc::new(RwLock::new(valid)),
            },
        ));
    }
    Ok(out)
}

/// Key-tuple equality on the (rank, scalar) normalization: NULLs equal
/// NULLs, NaNs equal NaNs (grouping semantics, unlike IEEE comparison).
fn sort_keys_equal(a: &[(u8, LazySortKey)], b: &[(u8, LazySortKey)]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|((ra, ka), (rb, kb))| {
            ra == rb
                && match (ka, kb) {
                    (LazySortKey::I(x), LazySortKey::I(y)) => x == y,
                    (LazySortKey::F(x), LazySortKey::F(y)) => x == y,
                    (LazySortKey::I(x), LazySortKey::F(y)) => (*x as f64) == *y,
                    (LazySortKey::F(x), LazySortKey::I(y)) => *x == (*y as f64),
                    (LazySortKey::S(x), LazySortKey::S(y)) => x == y,
                    (LazySortKey::B(x), LazySortKey::B(y)) => x == y,
                    (LazySortKey::Nan, LazySortKey::Nan) => true,
                    (LazySortKey::Null, LazySortKey::Null) => true,
                    _ => false,
                }
        })
}

/// One aggregate over one group's rows. Returns `(value, valid)` —
/// an all-null group yields `(Unit, false)` for sum/mean/min/max and
/// `(Int(0), true)` for count.
fn eval_lazy_agg(
    op: crate::interpreter::value::LazyAggOp,
    arg: &crate::interpreter::value::LazyExprIR,
    cur: &[(String, Value)],
    rows: &[usize],
) -> Result<(Value, bool), String> {
    use crate::interpreter::value::LazyAggOp;
    let mut ints: Vec<i64> = Vec::new();
    let mut floats: Vec<f64> = Vec::new();
    let mut strs: Vec<String> = Vec::new();
    let mut count: i64 = 0;
    for &row in rows {
        match eval_lazy_scalar(arg, cur, row)? {
            None => {}
            Some(LazyScalar::I(v)) => {
                count += 1;
                ints.push(v);
            }
            Some(LazyScalar::F(v)) => {
                count += 1;
                floats.push(v);
            }
            Some(LazyScalar::S(v)) => {
                count += 1;
                strs.push(v);
            }
            Some(LazyScalar::B(_)) => {
                count += 1;
            }
        }
    }
    if matches!(op, LazyAggOp::Count) {
        return Ok((Value::Int(count), true));
    }
    if !strs.is_empty() {
        if !ints.is_empty() || !floats.is_empty() {
            return Err("LazyGroupBy.agg: mixed String and numeric values".to_string());
        }
        return match op {
            LazyAggOp::Min => Ok(strs
                .into_iter()
                .min()
                .map_or((Value::Unit, false), |s| (Value::String(s), true))),
            LazyAggOp::Max => Ok(strs
                .into_iter()
                .max()
                .map_or((Value::Unit, false), |s| (Value::String(s), true))),
            _ => Err(format!(
                "LazyGroupBy.agg: {}() needs numeric values",
                op.name()
            )),
        };
    }
    let all_int = floats.is_empty();
    let mut vals: Vec<f64> = floats;
    vals.extend(ints.iter().map(|&v| v as f64));
    if vals.is_empty() {
        return Ok((Value::Unit, false));
    }
    Ok(match op {
        LazyAggOp::Sum => {
            if all_int {
                (Value::Int(ints.iter().sum()), true)
            } else {
                (Value::Float(vals.iter().sum()), true)
            }
        }
        LazyAggOp::Mean => (
            Value::Float(vals.iter().sum::<f64>() / vals.len() as f64),
            true,
        ),
        LazyAggOp::Min => {
            if all_int {
                (Value::Int(*ints.iter().min().unwrap()), true)
            } else {
                (
                    Value::Float(vals.iter().cloned().fold(f64::INFINITY, f64::min)),
                    true,
                )
            }
        }
        LazyAggOp::Max => {
            if all_int {
                (Value::Int(*ints.iter().max().unwrap()), true)
            } else {
                (
                    Value::Float(vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                    true,
                )
            }
        }
        LazyAggOp::Count => unreachable!(),
    })
}

/// The columns an expression references (for validation + scan-projection
/// union in the optimizer).
fn lazy_expr_cols(ir: &crate::interpreter::value::LazyExprIR, out: &mut Vec<String>) {
    use crate::interpreter::value::LazyExprIR;
    match ir {
        LazyExprIR::Col(n) => {
            if !out.contains(n) {
                out.push(n.clone());
            }
        }
        LazyExprIR::Cmp { lhs, rhs, .. } | LazyExprIR::Arith { lhs, rhs, .. } => {
            lazy_expr_cols(lhs, out);
            lazy_expr_cols(rhs, out);
        }
        LazyExprIR::And(a, b) | LazyExprIR::Or(a, b) => {
            lazy_expr_cols(a, out);
            lazy_expr_cols(b, out);
        }
        LazyExprIR::Not(x) | LazyExprIR::Desc(x) => lazy_expr_cols(x, out),
        LazyExprIR::Agg { arg, .. } => lazy_expr_cols(arg, out),
        LazyExprIR::Alias { expr, .. } => lazy_expr_cols(expr, out),
        _ => {}
    }
}

/// Flatten a same-op `and`/`or` chain into its leaves, folding each leaf
/// on the way in (so a leaf that simplifies into another same-op chain —
/// e.g. via double-negation removal — flattens too).
fn flatten_bool_chain(
    ir: crate::interpreter::value::LazyExprIR,
    is_and: bool,
    out: &mut Vec<crate::interpreter::value::LazyExprIR>,
) {
    use crate::interpreter::value::LazyExprIR;
    match ir {
        LazyExprIR::And(a, b) if is_and => {
            flatten_bool_chain(fold_lazy_expr(&a), is_and, out);
            flatten_bool_chain(fold_lazy_expr(&b), is_and, out);
        }
        LazyExprIR::Or(a, b) if !is_and => {
            flatten_bool_chain(fold_lazy_expr(&a), is_and, out);
            flatten_bool_chain(fold_lazy_expr(&b), is_and, out);
        }
        other => out.push(other),
    }
}

/// Bottom-up plan-time expression simplification — the CONSTANT FOLDING
/// and CSE passes of the pinned Option A optimizer (deferred.md § Lazy
/// DataFrame Query Planner). Three rewrites:
///
///   1. Literal-only comparisons fold to `LitBool`, mirroring
///      `eval_lazy_pred` EXACTLY (i64↔f64 widen; NaN comparisons are
///      false for every op — the engine's documented posture). A
///      type-mismatched or bool-ordered literal comparison stays
///      UNFOLDED so collect still errors loudly.
///   2. Boolean algebra over `lit(..)` arms: the neutral literal drops
///      out of its chain (`x and true` → `x`, `x or false` → `x`), the
///      dominating literal collapses it (`_ and false` → `false`,
///      `_ or true` → `true`); `not` on a literal flips it, and double
///      negation cancels.
///   3. CSE: structurally-identical conjuncts/disjuncts within one
///      `and`/`or` chain deduplicate to the first occurrence — adjacent
///      -filter fusion feeds this `X and X` when the same predicate is
///      applied twice.
///
/// `collect` always evaluates the ORIGINAL ops, so folding can never
/// change results — it feeds `explain` and the scan projection.
fn fold_lazy_expr(
    ir: &crate::interpreter::value::LazyExprIR,
) -> crate::interpreter::value::LazyExprIR {
    use crate::interpreter::value::{LazyCmpOp, LazyExprIR};
    match ir {
        LazyExprIR::Cmp { op, lhs, rhs } => {
            let l = fold_lazy_expr(lhs);
            let r = fold_lazy_expr(rhs);
            // `Some(ord)` = a foldable literal pair; the inner Option is
            // `partial_cmp`'s (None = NaN involved → false, like eval).
            let ord = match (&l, &r) {
                (LazyExprIR::LitInt(x), LazyExprIR::LitInt(y)) => Some(x.partial_cmp(y)),
                (LazyExprIR::LitFloat(x), LazyExprIR::LitFloat(y)) => Some(x.partial_cmp(y)),
                (LazyExprIR::LitInt(x), LazyExprIR::LitFloat(y)) => {
                    Some((*x as f64).partial_cmp(y))
                }
                (LazyExprIR::LitFloat(x), LazyExprIR::LitInt(y)) => {
                    Some(x.partial_cmp(&(*y as f64)))
                }
                (LazyExprIR::LitStr(x), LazyExprIR::LitStr(y)) => Some(Some(x.cmp(y))),
                (LazyExprIR::LitBool(x), LazyExprIR::LitBool(y))
                    if matches!(op, LazyCmpOp::Eq | LazyCmpOp::Ne) =>
                {
                    Some(Some(x.cmp(y)))
                }
                _ => None,
            };
            match ord {
                Some(ord) => LazyExprIR::LitBool(ord.is_some_and(|o| match op {
                    LazyCmpOp::Gt => o.is_gt(),
                    LazyCmpOp::Ge => o.is_ge(),
                    LazyCmpOp::Lt => o.is_lt(),
                    LazyCmpOp::Le => o.is_le(),
                    LazyCmpOp::Eq => o.is_eq(),
                    LazyCmpOp::Ne => o.is_ne(),
                })),
                None => LazyExprIR::Cmp {
                    op: *op,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                },
            }
        }
        LazyExprIR::And(..) | LazyExprIR::Or(..) => {
            let is_and = matches!(ir, LazyExprIR::And(..));
            let mut leaves: Vec<LazyExprIR> = Vec::new();
            flatten_bool_chain(ir.clone(), is_and, &mut leaves);
            let mut out: Vec<LazyExprIR> = Vec::new();
            for leaf in leaves {
                match leaf {
                    LazyExprIR::LitBool(b) => {
                        if b == is_and {
                            continue; // neutral: true in and / false in or
                        }
                        return LazyExprIR::LitBool(b); // dominator
                    }
                    other => {
                        if !out.contains(&other) {
                            out.push(other); // CSE: first occurrence wins
                        }
                    }
                }
            }
            let mut it = out.into_iter();
            match it.next() {
                None => LazyExprIR::LitBool(is_and), // all leaves were neutral
                Some(first) => it.fold(first, |acc, x| {
                    if is_and {
                        LazyExprIR::And(Box::new(acc), Box::new(x))
                    } else {
                        LazyExprIR::Or(Box::new(acc), Box::new(x))
                    }
                }),
            }
        }
        LazyExprIR::Arith { op, lhs, rhs } => {
            use crate::interpreter::value::LazyArithOp;
            let l = fold_lazy_expr(lhs);
            let r = fold_lazy_expr(rhs);
            let folded = match (&l, &r) {
                (LazyExprIR::LitInt(x), LazyExprIR::LitInt(y)) => match op {
                    // Checked — a would-be overflow / division by zero
                    // stays UNFOLDED so collect errors loudly.
                    LazyArithOp::Add => x.checked_add(*y).map(LazyExprIR::LitInt),
                    LazyArithOp::Sub => x.checked_sub(*y).map(LazyExprIR::LitInt),
                    LazyArithOp::Mul => x.checked_mul(*y).map(LazyExprIR::LitInt),
                    LazyArithOp::Div => {
                        if *y == 0 {
                            None
                        } else {
                            x.checked_div(*y).map(LazyExprIR::LitInt)
                        }
                    }
                },
                (
                    LazyExprIR::LitInt(_) | LazyExprIR::LitFloat(_),
                    LazyExprIR::LitInt(_) | LazyExprIR::LitFloat(_),
                ) => {
                    let as_f = |e: &LazyExprIR| match e {
                        LazyExprIR::LitInt(v) => *v as f64,
                        LazyExprIR::LitFloat(v) => *v,
                        _ => unreachable!(),
                    };
                    let (x, y) = (as_f(&l), as_f(&r));
                    Some(LazyExprIR::LitFloat(match op {
                        LazyArithOp::Add => x + y,
                        LazyArithOp::Sub => x - y,
                        LazyArithOp::Mul => x * y,
                        LazyArithOp::Div => x / y, // IEEE, like eval
                    }))
                }
                _ => None,
            };
            folded.unwrap_or(LazyExprIR::Arith {
                op: *op,
                lhs: Box::new(l),
                rhs: Box::new(r),
            })
        }
        LazyExprIR::Not(x) => match fold_lazy_expr(x) {
            LazyExprIR::LitBool(b) => LazyExprIR::LitBool(!b),
            LazyExprIR::Not(inner) => *inner, // double negation cancels
            other => LazyExprIR::Not(Box::new(other)),
        },
        LazyExprIR::Desc(x) => LazyExprIR::Desc(Box::new(fold_lazy_expr(x))),
        LazyExprIR::Agg { op, arg } => LazyExprIR::Agg {
            op: *op,
            arg: Box::new(fold_lazy_expr(arg)),
        },
        LazyExprIR::Alias { name, expr } => LazyExprIR::Alias {
            name: name.clone(),
            expr: Box::new(fold_lazy_expr(expr)),
        },
        leaf => leaf.clone(),
    }
}

/// The optimized pipeline form of a slice-1/2 lazy plan.
pub(super) struct OptimizedLazyPlan {
    /// Scan-level projection: the union of columns any predicate or the
    /// final projection needs, in SOURCE order. `None` = every column.
    scan_cols: Option<Vec<String>>,
    /// The row pipeline nearest-scan-first: filters (adjacent ones fused
    /// with `and`) and limits (adjacent ones fused to the min), in their
    /// original relative order (filters do NOT commute with limits).
    steps: Vec<crate::interpreter::value::LazyOp>,
    /// The final output projection (selects collapse — the last wins),
    /// or `None` for every column.
    projection: Option<Vec<String>>,
    /// The plan's final schema (what a downstream consumer — e.g. a
    /// JOIN parent — sees). Tracked through selects / group-bys / joins.
    final_schema: Vec<String>,
}

/// Validate + optimize a lazy plan: stepwise visible-column tracking
/// (selects narrow it; predicate refs must be visible AT THEIR STEP),
/// select collapse, adjacent filter/limit fusion, and the scan-projection
/// union. The single validation authority for `collect` and `explain`.
fn fold_lazy_plan(
    source: &Arc<RwLock<Vec<(String, Value)>>>,
    ops: &Arc<Vec<crate::interpreter::value::LazyOp>>,
) -> Result<OptimizedLazyPlan, String> {
    use crate::interpreter::value::{LazyExprIR, LazyOp};
    let src = source.read().unwrap();
    let source_order: Vec<String> = src.iter().map(|(n, _)| n.clone()).collect();
    drop(src);
    let mut visible: Vec<String> = source_order.clone();
    let mut projection: Option<Vec<String>> = None;
    let mut needed: Vec<String> = Vec::new();
    let mut steps: Vec<LazyOp> = Vec::new();
    // After a GroupBy the schema is DERIVED — downstream column refs no
    // longer touch the scan, so they stop feeding the scan projection.
    let mut past_group_by = false;
    // Pushdown does not yet cross a JOIN (the P2 optimizer-expansion
    // entry): once one appears the scan reads every column.
    let mut past_join = false;
    for op in ops.iter() {
        match op {
            LazyOp::Select(cols) => {
                for c in cols {
                    if !visible.contains(c) {
                        return Err(format!(
                            "LazyFrame.select: no column named '{c}' at this plan step"
                        ));
                    }
                }
                visible = cols.clone();
                projection = Some(cols.clone());
            }
            LazyOp::Limit(n) => match steps.last_mut() {
                Some(LazyOp::Limit(m)) => *m = (*m).min(*n),
                _ => steps.push(LazyOp::Limit(*n)),
            },
            LazyOp::Filter(ir) => {
                // Validate against the ORIGINAL expression — folding may
                // elide a branch, but a bad column name in it must stay
                // a loud error.
                let mut cols = Vec::new();
                lazy_expr_cols(ir, &mut cols);
                for c in &cols {
                    if !visible.contains(c) {
                        return Err(format!(
                            "LazyFrame.filter: no column named '{c}' at this plan step"
                        ));
                    }
                }
                // Constant folding + CSE (slice 6) — plan-time only
                // (collect evaluates the original ops). The scan
                // projection counts only the columns the FOLDED
                // predicate still reads.
                let folded = fold_lazy_expr(ir);
                let mut fcols = Vec::new();
                lazy_expr_cols(&folded, &mut fcols);
                for c in &fcols {
                    if !past_group_by && !needed.contains(c) {
                        needed.push(c.clone());
                    }
                }
                if matches!(folded, LazyExprIR::LitBool(true)) {
                    continue; // a constant-true filter drops out
                }
                match steps.last_mut() {
                    Some(LazyOp::Filter(prev)) => {
                        // Re-fold after fusing so duplicate adjacent
                        // filters (`X and X`) collapse — the CSE trigger.
                        *prev = Arc::new(fold_lazy_expr(&LazyExprIR::And(
                            Box::new(prev.as_ref().clone()),
                            Box::new(folded),
                        )));
                    }
                    _ => steps.push(LazyOp::Filter(Arc::new(folded))),
                }
            }
            LazyOp::Sort(keys) => {
                // No fusion: a later sort dominates but the EARLIER sort is
                // the stable tie-break within equal keys, so both must run.
                for k in keys {
                    let mut cols = Vec::new();
                    lazy_expr_cols(k, &mut cols);
                    for c in &cols {
                        if !visible.contains(c) {
                            return Err(format!(
                                "LazyFrame.sort: no column named '{c}' at this plan step"
                            ));
                        }
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                }
                steps.push(LazyOp::Sort(keys.clone()));
            }
            LazyOp::GroupBy { keys, aggs } => {
                let mut out_schema: Vec<String> = Vec::new();
                for k in keys {
                    let name = lazy_group_key_name(k)?;
                    if !visible.contains(&name) {
                        return Err(format!(
                            "LazyFrame.group_by: no column named '{name}' at this plan step"
                        ));
                    }
                    if !past_group_by && !needed.contains(&name) {
                        needed.push(name.clone());
                    }
                    out_schema.push(name);
                }
                for a in aggs {
                    let mut cols = Vec::new();
                    lazy_expr_cols(a, &mut cols);
                    for c in &cols {
                        if !visible.contains(c) {
                            return Err(format!(
                                "LazyGroupBy.agg: no column named '{c}' at this plan step"
                            ));
                        }
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                    out_schema.push(lazy_agg_output_name(a)?);
                }
                visible = out_schema;
                projection = None;
                past_group_by = true;
                steps.push(LazyOp::GroupBy {
                    keys: keys.clone(),
                    aggs: aggs.clone(),
                });
            }
            LazyOp::Join {
                right_source,
                right_ops,
                on,
            } => {
                let right_plan = fold_lazy_plan(right_source, right_ops)?;
                for k in on {
                    if !visible.contains(k) {
                        return Err(format!(
                            "LazyFrame.join: no column named '{k}' on the LEFT side at \
                             this plan step"
                        ));
                    }
                    if !right_plan.final_schema.contains(k) {
                        return Err(format!(
                            "LazyFrame.join: no column named '{k}' on the RIGHT side"
                        ));
                    }
                }
                // Output schema: left, then right minus keys (collisions
                // take a `_right` suffix). A pending left projection is
                // APPLIED at the join (collect materializes it), so fold
                // narrows to it first — and pushes it as an explicit
                // SELECT step so the rendered pipeline stays honest.
                if let Some(p) = &projection {
                    visible = p.clone();
                    steps.push(LazyOp::Select(p.clone()));
                }
                let mut out_schema = visible.clone();
                for rc in &right_plan.final_schema {
                    if on.contains(rc) {
                        continue;
                    }
                    if out_schema.contains(rc) {
                        out_schema.push(format!("{rc}_right"));
                    } else {
                        out_schema.push(rc.clone());
                    }
                }
                visible = out_schema;
                projection = None;
                past_join = true;
                steps.push(LazyOp::Join {
                    right_source: Arc::clone(right_source),
                    right_ops: Arc::clone(right_ops),
                    on: on.clone(),
                });
            }
            LazyOp::WithColumns(exprs) => {
                // Every entry validates against this step's INPUT schema
                // (the Polars parallel semantics — entries never see
                // each other); duplicate output names in one call are a
                // loud error.
                let mut outs: Vec<String> = Vec::new();
                for e in exprs {
                    let (name, _) = with_columns_output(e)?;
                    let mut cols = Vec::new();
                    lazy_expr_cols(e, &mut cols);
                    for c in &cols {
                        if !visible.contains(c) {
                            return Err(format!(
                                "LazyFrame.with_columns: no column named '{c}' at this plan step"
                            ));
                        }
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                    if outs.contains(&name) {
                        return Err(format!(
                            "LazyFrame.with_columns: duplicate output name '{name}'"
                        ));
                    }
                    outs.push(name);
                }
                // Flush a pending projection as an explicit SELECT step
                // (same boundary rule as JOIN — lifting a select past
                // this step would reorder it against the computed
                // columns). Its columns MATERIALIZE here, so they all
                // join the scan set (they flow through to the output —
                // no top projection narrows them away anymore).
                if let Some(p) = projection.take() {
                    visible = p.clone();
                    for c in &p {
                        if !past_group_by && !needed.contains(c) {
                            needed.push(c.clone());
                        }
                    }
                    steps.push(LazyOp::Select(p));
                }
                for name in outs {
                    if !visible.contains(&name) {
                        visible.push(name);
                    }
                }
                // Constant folding applies inside each entry.
                steps.push(LazyOp::WithColumns(
                    exprs.iter().map(|e| Arc::new(fold_lazy_expr(e))).collect(),
                ));
            }
        }
    }
    // Scan projection: union of predicate columns + the final projection,
    // in SOURCE order. `None` when nothing narrows it. Past a GroupBy the
    // projection names DERIVED columns, so only pre-groupby refs count.
    let scan_cols = if past_join {
        None
    } else if past_group_by {
        if needed.is_empty() {
            None
        } else {
            Some(
                source_order
                    .iter()
                    .filter(|n| needed.contains(n))
                    .cloned()
                    .collect(),
            )
        }
    } else if projection.is_none() && needed.is_empty() {
        None
    } else {
        let mut wanted: Vec<String> = Vec::new();
        if let Some(p) = &projection {
            wanted.extend(p.iter().cloned());
        }
        wanted.extend(needed.iter().cloned());
        let has_filters = steps.iter().any(|s| {
            matches!(
                s,
                LazyOp::Filter(_)
                    | LazyOp::Sort(_)
                    | LazyOp::GroupBy { .. }
                    | LazyOp::Join { .. }
                    | LazyOp::WithColumns(_)
            )
        });
        if has_filters {
            // Union in source order (deterministic).
            Some(
                source_order
                    .iter()
                    .filter(|n| wanted.contains(n))
                    .cloned()
                    .collect(),
            )
        } else {
            // No filters: the scan can project straight to the output
            // order — the top SELECT is then elided by the renderer.
            projection.clone()
        }
    };
    let final_schema = match &projection {
        Some(p) => p.clone(),
        None => visible.clone(),
    };
    Ok(OptimizedLazyPlan {
        scan_cols,
        steps,
        projection,
        final_schema,
    })
}

/// Render the optimized pipeline, innermost SCAN last. A limit adjacent
/// to the scan fuses into the scan line; with no filters the projection
/// lives on the scan itself and the top SELECT is elided (recovering the
/// slice-1 single-scan rendering for select/limit-only plans).
fn render_optimized_plan(plan: &OptimizedLazyPlan) -> String {
    use crate::interpreter::value::LazyOp;
    let has_filters = plan.steps.iter().any(|s| {
        matches!(
            s,
            LazyOp::Filter(_) | LazyOp::Sort(_) | LazyOp::GroupBy { .. } | LazyOp::Join { .. }
        )
    });
    let mut scan = match &plan.scan_cols {
        Some(cols) => format!("SCAN cols=[{}]", cols.join(", ")),
        None => "SCAN cols=[*]".to_string(),
    };
    let mut steps = plan.steps.clone();
    // Fuse a scan-adjacent limit into the scan line.
    if let Some(LazyOp::Limit(n)) = steps.first() {
        scan = format!("{scan} limit={n}");
        steps.remove(0);
    }
    let mut lines: Vec<String> = vec![scan];
    for step in &steps {
        let rendered = match step {
            LazyOp::Limit(n) => format!("LIMIT {n}"),
            LazyOp::Filter(ir) => format!("FILTER {ir}"),
            LazyOp::Sort(keys) => format!(
                "SORT [{}]",
                keys.iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            LazyOp::GroupBy { keys, aggs } => format!(
                "GROUP BY [{}] AGG [{}]",
                keys.iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                aggs.iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            LazyOp::Join {
                right_source,
                right_ops,
                on,
            } => format!(
                "JOIN on=[{}] right=({})",
                on.join(", "),
                lazy_optimized_compact(right_source, right_ops)
            ),
            LazyOp::WithColumns(exprs) => format!(
                "WITH [{}]",
                exprs
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            // Only pushed by the fold at a JOIN/WITH boundary (a pending
            // left projection the step consumes); ordinary selects live
            // in `projection`, not in `steps`.
            LazyOp::Select(cols) => format!("SELECT [{}]", cols.join(", ")),
        };
        lines.push(rendered);
    }
    if has_filters {
        if let Some(p) = &plan.projection {
            lines.push(format!("SELECT [{}]", p.join(", ")));
        }
    }
    let mut out = String::new();
    for (i, line) in lines.iter().rev().enumerate() {
        out.push_str(&"  ".repeat(i));
        out.push_str(line);
        out.push('\n');
    }
    // Trim the trailing newline — explain() adds its own framing.
    out.pop();
    out
}

/// Parse RFC-4180-lite CSV text into a `Value::DataFrame` (phase-11 CSV
/// leg, slice 2 — the inverse of `write_csv`). First record = column
/// names. A double-quoted cell may contain commas, CR/LF, and doubled
/// quotes (`""` → `"`); an UNQUOTED empty cell is a NULL slot (write_csv's
/// NULL encoding), while a quoted cell — even `""` — is always a value.
/// Per-column type inference over the value cells: all parse as i64 →
/// `Column[i64]`; else all parse as f64 → `Column[f64]`; else
/// `Column[String]`. Null slots store the `Value::Unit` placeholder +
/// `valid=false`, matching `Column.from_iter_nullable`. Ragged rows (cell
/// count ≠ header count) and an empty file are `Err(<message>)` — the
/// caller wraps the message in `IoError.Other`.
pub(super) fn parse_csv_to_dataframe(text: &str) -> Result<Value, String> {
    // Record/field splitter honoring quotes. `Some(s)` = value cell,
    // `None` = null (unquoted empty).
    let mut records: Vec<Vec<Option<String>>> = Vec::new();
    let mut field = String::new();
    let mut quoted = false; // current field was ever quoted
    let mut fields: Vec<Option<String>> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    let flush_field = |field: &mut String, quoted: &mut bool, fields: &mut Vec<Option<String>>| {
        let cell = std::mem::take(field);
        fields.push(if cell.is_empty() && !*quoted {
            None
        } else {
            Some(cell)
        });
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => field.push(other),
            }
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                quoted = true;
            }
            ',' => flush_field(&mut field, &mut quoted, &mut fields),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                flush_field(&mut field, &mut quoted, &mut fields);
                records.push(std::mem::take(&mut fields));
            }
            '\n' => {
                flush_field(&mut field, &mut quoted, &mut fields);
                records.push(std::mem::take(&mut fields));
            }
            other => field.push(other),
        }
    }
    if in_quotes {
        return Err("CSV parse error: unterminated quoted cell".to_string());
    }
    // A final record without a trailing newline.
    if !field.is_empty() || quoted || !fields.is_empty() {
        flush_field(&mut field, &mut quoted, &mut fields);
        records.push(fields);
    }
    let Some(header) = records.first() else {
        return Err("CSV parse error: empty file (no header row)".to_string());
    };
    let names: Vec<String> = header
        .iter()
        .enumerate()
        .map(|(i, c)| c.clone().unwrap_or_else(|| format!("column_{i}")))
        .collect();
    let width = names.len();
    for (i, rec) in records.iter().enumerate().skip(1) {
        if rec.len() != width {
            return Err(format!(
                "CSV parse error: row {} has {} cell(s) but the header has {}",
                i,
                rec.len(),
                width
            ));
        }
    }
    // Per-column inference + build.
    let mut columns: Vec<(String, Value)> = Vec::with_capacity(width);
    for (ci, name) in names.into_iter().enumerate() {
        let cells: Vec<&Option<String>> = records.iter().skip(1).map(|r| &r[ci]).collect();
        let all_i64 = cells
            .iter()
            .all(|c| c.as_ref().is_none_or(|s| s.parse::<i64>().is_ok()));
        let all_f64 = all_i64
            || cells
                .iter()
                .all(|c| c.as_ref().is_none_or(|s| s.parse::<f64>().is_ok()));
        let mut data: Vec<Value> = Vec::with_capacity(cells.len());
        let mut valid: Vec<bool> = Vec::with_capacity(cells.len());
        for c in cells {
            match c {
                None => {
                    data.push(Value::Unit);
                    valid.push(false);
                }
                Some(s) => {
                    data.push(if all_i64 {
                        Value::Int(s.parse::<i64>().unwrap())
                    } else if all_f64 {
                        Value::Float(s.parse::<f64>().unwrap())
                    } else {
                        Value::String(s.clone())
                    });
                    valid.push(true);
                }
            }
        }
        columns.push((
            name,
            Value::Column {
                data: Arc::new(RwLock::new(data)),
                valid: Arc::new(RwLock::new(valid)),
            },
        ));
    }
    Ok(Value::DataFrame {
        columns: Arc::new(RwLock::new(columns)),
    })
}
