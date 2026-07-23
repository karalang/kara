//! Arrow IPC interchange for `Column[T]` — the interpreter's reference
//! implementation (phase-11 Arrow IPC slice 1).
//!
//! `Column.to_arrow_ipc()` serializes a column to the Apache Arrow **IPC
//! stream** format (a single `RecordBatch` with one field named `col`); it
//! interoperates with any Arrow reader (pyarrow `ipc.open_stream`, DuckDB,
//! polars). `Column.from_arrow_ipc(bytes)` parses such a stream back into a
//! column. Backed by the `arrow-array` / `arrow-schema` / `arrow-ipc` crates
//! so the wire format is spec-compliant rather than hand-rolled (Arrow IPC
//! metadata is flatbuffers-encoded).
//!
//! Slice 1 covers `i64` (`Int64`) and `f64` (`Float64`) element types, both
//! nullable (the column's validity bitmap maps to Arrow's null buffer). The
//! element type is inferred from the first valid slot; an empty or all-null
//! column defaults to `Int64` (its length and null pattern still round-trip
//! exactly — only the logical element type of a value-less column is
//! unspecified). Codegen + the runtime `libkarac_runtime_arrow.a` archive are
//! slice 2; `karac run` routes an arrow program to this interpreter path in
//! the meantime (mirroring the `gpu` / `regex` fallback).

use std::io::Cursor;
use std::sync::Arc;

use arrow_array::{Array, Float64Array, Int64Array, RecordBatch};
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};

use super::value::Value;

/// The Arrow element type this slice serializes a column as, chosen from the
/// column's runtime values.
enum ColKind {
    Int64,
    Float64,
}

/// Pick the Arrow element type from the first VALID slot: an `Int` → `Int64`,
/// a `Float` → `Float64`. An empty or all-null column has no value to key on,
/// so it defaults to `Int64` (length + null pattern still round-trip exactly).
fn infer_kind(data: &[Value], valid: &[bool]) -> ColKind {
    for (v, &ok) in data.iter().zip(valid.iter()) {
        if ok {
            match v {
                Value::Float(_) => return ColKind::Float64,
                Value::Int(_) => return ColKind::Int64,
                _ => {}
            }
        }
    }
    ColKind::Int64
}

/// Serialize `(data, valid)` to an Arrow IPC stream (`Vec<u8>`). Returns an
/// error string on any Arrow-side failure (kept as a plain `String` so the
/// interpreter can surface it as an ordinary runtime error).
pub(super) fn column_to_ipc(data: &[Value], valid: &[bool]) -> Result<Vec<u8>, String> {
    let (field_type, array): (DataType, Arc<dyn Array>) = match infer_kind(data, valid) {
        ColKind::Int64 => {
            let vals: Vec<Option<i64>> = data
                .iter()
                .zip(valid.iter())
                .map(|(v, &ok)| {
                    if !ok {
                        None
                    } else {
                        match v {
                            Value::Int(n) => Some(*n),
                            // A float slot in an inferred-Int64 column would
                            // only arise from a genuinely mixed column, which
                            // the typechecker forbids; coerce defensively.
                            Value::Float(f) => Some(*f as i64),
                            _ => None,
                        }
                    }
                })
                .collect();
            (DataType::Int64, Arc::new(Int64Array::from(vals)))
        }
        ColKind::Float64 => {
            let vals: Vec<Option<f64>> = data
                .iter()
                .zip(valid.iter())
                .map(|(v, &ok)| {
                    if !ok {
                        None
                    } else {
                        match v {
                            Value::Float(f) => Some(*f),
                            Value::Int(n) => Some(*n as f64),
                            _ => None,
                        }
                    }
                })
                .collect();
            (DataType::Float64, Arc::new(Float64Array::from(vals)))
        }
    };

    let schema = Arc::new(Schema::new(vec![Field::new("col", field_type, true)]));
    let batch =
        RecordBatch::try_new(schema.clone(), vec![array]).map_err(|e| format!("arrow: {e}"))?;

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer =
            StreamWriter::try_new(&mut buf, &schema).map_err(|e| format!("arrow: {e}"))?;
        writer.write(&batch).map_err(|e| format!("arrow: {e}"))?;
        writer.finish().map_err(|e| format!("arrow: {e}"))?;
    }
    Ok(buf)
}

/// Parse an Arrow IPC stream into `(data, valid)` for a `Column`. Reads the
/// first `RecordBatch`'s first column; supports `Int64` / `Float64`. A null
/// slot becomes `Value::Unit` in `data` with `false` in `valid` (the column's
/// never-read placeholder convention).
pub(super) fn column_from_ipc(bytes: &[u8]) -> Result<(Vec<Value>, Vec<bool>), String> {
    let mut reader =
        StreamReader::try_new(Cursor::new(bytes), None).map_err(|e| format!("arrow: {e}"))?;
    let batch = match reader.next() {
        Some(b) => b.map_err(|e| format!("arrow: {e}"))?,
        None => return Ok((Vec::new(), Vec::new())),
    };
    if batch.num_columns() == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let col = batch.column(0);
    let len = col.len();
    let mut data: Vec<Value> = Vec::with_capacity(len);
    let mut valid: Vec<bool> = Vec::with_capacity(len);

    if let Some(arr) = col.as_any().downcast_ref::<Int64Array>() {
        for i in 0..len {
            if arr.is_null(i) {
                data.push(Value::Unit);
                valid.push(false);
            } else {
                data.push(Value::Int(arr.value(i)));
                valid.push(true);
            }
        }
    } else if let Some(arr) = col.as_any().downcast_ref::<Float64Array>() {
        for i in 0..len {
            if arr.is_null(i) {
                data.push(Value::Unit);
                valid.push(false);
            } else {
                data.push(Value::Float(arr.value(i)));
                valid.push(true);
            }
        }
    } else {
        return Err(format!(
            "arrow: Column.from_arrow_ipc slice 1 supports Int64 / Float64 columns; \
             got {}",
            col.data_type()
        ));
    }
    Ok((data, valid))
}
