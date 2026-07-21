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
            _ => None,
        }
    }
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
