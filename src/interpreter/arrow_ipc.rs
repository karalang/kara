//! Arrow IPC interchange for `Column[T]` — the interpreter's reference
//! implementation (phase-11 Arrow IPC slice 1 + slice 1.5 element-type
//! coverage).
//!
//! `Column.to_arrow_ipc()` serializes a column to the Apache Arrow **IPC
//! stream** format (a single `RecordBatch` with one field named `col`); it
//! interoperates with any Arrow reader (pyarrow `ipc.open_stream`, DuckDB,
//! polars). `Column.from_arrow_ipc(bytes)` parses such a stream back into a
//! column. Backed by the `arrow-array` / `arrow-schema` / `arrow-ipc` crates
//! so the wire format is spec-compliant rather than hand-rolled (Arrow IPC
//! metadata is flatbuffers-encoded).
//!
//! Element-type coverage — the four kinds the interpreter's `Value` can
//! distinguish, all nullable (the column's validity bitmap maps to Arrow's
//! null buffer):
//!
//! | Kara cell    | Arrow write type | Arrow read types accepted            |
//! |--------------|------------------|--------------------------------------|
//! | `Value::Int` | `Int64`          | `Int64`, `Int32` (widened)           |
//! | `Value::Float`| `Float64`       | `Float64`, `Float32` (widened)       |
//! | `Value::String`| `Utf8`         | `Utf8`, `LargeUtf8`                   |
//! | `Value::Bool`| `Boolean`        | `Boolean`                            |
//!
//! The interpreter erases integer/float *width* (all ints are `i64`, all
//! floats are `f64`), so the writer emits the widest form; the reader also
//! accepts the narrow forms and widens them losslessly so a column written by
//! a foreign producer (pyarrow `int32`, `float32`, `large_string`) still loads.
//! The element type is inferred from the first valid slot; an empty or all-null
//! column defaults to `Int64` (its length and null pattern still round-trip
//! exactly — only the logical element type of a value-less column is
//! unspecified). Codegen + the runtime `libkarac_runtime_arrow.a` archive are a
//! later slice; `karac run` routes an arrow program to this interpreter path in
//! the meantime (mirroring the `gpu` / `regex` fallback).

use std::io::Cursor;
use std::sync::Arc;

use arrow_array::{
    Array, BooleanArray, Float32Array, Float64Array, Int32Array, Int64Array, LargeStringArray,
    RecordBatch, StringArray,
};
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};

use super::value::Value;

/// The Arrow element type the writer serializes a column as, chosen from the
/// column's runtime values.
enum ColKind {
    Int64,
    Float64,
    Utf8,
    Boolean,
}

/// Pick the Arrow element type from the first VALID slot: `Int` → `Int64`,
/// `Float` → `Float64`, `String` → `Utf8`, `Bool` → `Boolean`. An empty or
/// all-null column has no value to key on, so it defaults to `Int64` (length +
/// null pattern still round-trip exactly).
fn infer_kind(data: &[Value], valid: &[bool]) -> ColKind {
    for (v, &ok) in data.iter().zip(valid.iter()) {
        if ok {
            match v {
                Value::Float(_) => return ColKind::Float64,
                Value::Int(_) => return ColKind::Int64,
                Value::String(_) => return ColKind::Utf8,
                Value::Bool(_) => return ColKind::Boolean,
                _ => {}
            }
        }
    }
    ColKind::Int64
}

/// Serialize `(data, valid)` to an Arrow IPC stream (`Vec<u8>`). Returns an
/// error string on any Arrow-side failure (kept as a plain `String` so the
/// interpreter can surface it as an ordinary runtime error). A slot that is
/// null (`valid[i] == false`) — or, defensively, whose cell does not match the
/// inferred kind, which the typechecker's homogeneity rule forbids — becomes an
/// Arrow null.
pub(super) fn column_to_ipc(data: &[Value], valid: &[bool]) -> Result<Vec<u8>, String> {
    let (field_type, array): (DataType, Arc<dyn Array>) = match infer_kind(data, valid) {
        ColKind::Int64 => {
            let vals = data.iter().zip(valid.iter()).map(|(v, &ok)| match (ok, v) {
                (true, Value::Int(n)) => Some(*n),
                // A float slot in an inferred-Int64 column would only arise from
                // a genuinely mixed column, which the typechecker forbids;
                // coerce defensively rather than drop the value silently.
                (true, Value::Float(f)) => Some(*f as i64),
                _ => None,
            });
            (DataType::Int64, Arc::new(Int64Array::from_iter(vals)))
        }
        ColKind::Float64 => {
            let vals = data.iter().zip(valid.iter()).map(|(v, &ok)| match (ok, v) {
                (true, Value::Float(f)) => Some(*f),
                (true, Value::Int(n)) => Some(*n as f64),
                _ => None,
            });
            (DataType::Float64, Arc::new(Float64Array::from_iter(vals)))
        }
        ColKind::Utf8 => {
            let vals = data.iter().zip(valid.iter()).map(|(v, &ok)| match (ok, v) {
                (true, Value::String(s)) => Some(s.clone()),
                _ => None,
            });
            (DataType::Utf8, Arc::new(StringArray::from_iter(vals)))
        }
        ColKind::Boolean => {
            let vals = data.iter().zip(valid.iter()).map(|(v, &ok)| match (ok, v) {
                (true, Value::Bool(b)) => Some(*b),
                _ => None,
            });
            (DataType::Boolean, Arc::new(BooleanArray::from_iter(vals)))
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
/// first `RecordBatch`'s first column. A null slot becomes `Value::Unit` in
/// `data` with `false` in `valid` (the column's never-read placeholder
/// convention). `Int32` / `Float32` widen to `i64` / `f64` and `LargeUtf8`
/// reads as `String`, so a column a foreign producer wrote in a narrow form
/// still loads.
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

    // Downcast to a concrete array type, then map each slot through `$ctor`
    // (null slots become `Value::Unit` / `false`). `$ctor` receives the array's
    // native element and returns the `Value` cell.
    macro_rules! collect_col {
        ($arr:expr, $ctor:expr) => {{
            let arr = $arr;
            for i in 0..arr.len() {
                if arr.is_null(i) {
                    data.push(Value::Unit);
                    valid.push(false);
                } else {
                    data.push($ctor(arr.value(i)));
                    valid.push(true);
                }
            }
        }};
    }

    let any = col.as_any();
    if let Some(a) = any.downcast_ref::<Int64Array>() {
        collect_col!(a, Value::Int);
    } else if let Some(a) = any.downcast_ref::<Int32Array>() {
        collect_col!(a, |x| Value::Int(i64::from(x)));
    } else if let Some(a) = any.downcast_ref::<Float64Array>() {
        collect_col!(a, Value::Float);
    } else if let Some(a) = any.downcast_ref::<Float32Array>() {
        collect_col!(a, |x| Value::Float(f64::from(x)));
    } else if let Some(a) = any.downcast_ref::<StringArray>() {
        collect_col!(a, |x: &str| Value::String(x.to_string()));
    } else if let Some(a) = any.downcast_ref::<LargeStringArray>() {
        collect_col!(a, |x: &str| Value::String(x.to_string()));
    } else if let Some(a) = any.downcast_ref::<BooleanArray>() {
        collect_col!(a, Value::Bool);
    } else {
        return Err(format!(
            "arrow: Column.from_arrow_ipc supports Int64/Int32, Float64/Float32, \
             Utf8/LargeUtf8, and Boolean columns; got {}",
            col.data_type()
        ));
    }
    Ok((data, valid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{Float32Array, Int32Array, LargeStringArray};

    /// Build a one-field (`col`) Arrow IPC stream from a single array — the
    /// foreign-producer streams the reader's widening paths must accept.
    fn ipc_stream(field_type: DataType, array: Arc<dyn Array>) -> Vec<u8> {
        let schema = Arc::new(Schema::new(vec![Field::new("col", field_type, true)]));
        let batch = RecordBatch::try_new(schema.clone(), vec![array]).unwrap();
        let mut buf = Vec::new();
        {
            let mut w = StreamWriter::try_new(&mut buf, &schema).unwrap();
            w.write(&batch).unwrap();
            w.finish().unwrap();
        }
        buf
    }

    #[test]
    fn int32_reads_widen_to_i64() {
        let arr = Arc::new(Int32Array::from(vec![Some(1), None, Some(3)])) as Arc<dyn Array>;
        let (data, valid) = column_from_ipc(&ipc_stream(DataType::Int32, arr)).unwrap();
        assert_eq!(valid, vec![true, false, true]);
        assert!(matches!(data[0], Value::Int(1)));
        assert!(matches!(data[1], Value::Unit));
        assert!(matches!(data[2], Value::Int(3)));
    }

    #[test]
    fn float32_reads_widen_to_f64() {
        let arr = Arc::new(Float32Array::from(vec![Some(1.5f32), None])) as Arc<dyn Array>;
        let (data, valid) = column_from_ipc(&ipc_stream(DataType::Float32, arr)).unwrap();
        assert_eq!(valid, vec![true, false]);
        // 1.5 is exactly representable in both f32 and f64, so widening is exact.
        match data[0] {
            Value::Float(f) => assert!((f - 1.5).abs() < 1e-12),
            _ => panic!("expected Float"),
        }
        assert!(matches!(data[1], Value::Unit));
    }

    #[test]
    fn large_utf8_reads_as_string() {
        let arr =
            Arc::new(LargeStringArray::from(vec![Some("x"), None, Some("yz")])) as Arc<dyn Array>;
        let (data, valid) = column_from_ipc(&ipc_stream(DataType::LargeUtf8, arr)).unwrap();
        assert_eq!(valid, vec![true, false, true]);
        match &data[0] {
            Value::String(s) => assert_eq!(s.as_str(), "x"),
            _ => panic!("expected String"),
        }
        assert!(matches!(data[1], Value::Unit));
        match &data[2] {
            Value::String(s) => assert_eq!(s.as_str(), "yz"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn boolean_reads_round_trip() {
        let arr =
            Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])) as Arc<dyn Array>;
        let (data, valid) = column_from_ipc(&ipc_stream(DataType::Boolean, arr)).unwrap();
        assert_eq!(valid, vec![true, true, false]);
        assert!(matches!(data[0], Value::Bool(true)));
        assert!(matches!(data[1], Value::Bool(false)));
        assert!(matches!(data[2], Value::Unit));
    }

    #[test]
    fn unsupported_type_errors_cleanly() {
        // A type outside the accepted set (e.g. Date32) is a clean Err, not a
        // panic — the interpreter surfaces it as an ordinary runtime error.
        use arrow_array::Date32Array;
        let arr = Arc::new(Date32Array::from(vec![Some(0), Some(1)])) as Arc<dyn Array>;
        let err = column_from_ipc(&ipc_stream(DataType::Date32, arr)).unwrap_err();
        assert!(err.contains("from_arrow_ipc supports"), "got: {err}");
    }
}
