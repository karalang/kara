//! Arrow IPC interchange for `Column[T]`, `DataFrame`, and `Tensor` — the
//! interpreter's reference implementation (phase-11 Arrow IPC).
//!
//! `to_arrow_ipc()` serializes to the Apache Arrow **IPC stream** format (a
//! single `RecordBatch`); the matching `from_arrow_ipc` parses it back. Each
//! value type gets its canonical Arrow mapping, interoperable with any Arrow
//! reader (pyarrow `ipc.open_stream`, DuckDB, polars):
//!
//! - **`Column`** → a one-field batch (field `col`).
//! - **`DataFrame`** → an N-field batch, one field per named column, in schema
//!   order — the canonical tabular mapping.
//! - **`Tensor`** → a single-row `FixedSizeList[numel]` typed as the canonical
//!   `arrow.fixed_shape_tensor` extension, with the shape in the field's
//!   extension metadata (what pyarrow exposes as `pa.fixed_shape_tensor`).
//!
//! Backed by the `arrow-array` / `arrow-schema` / `arrow-ipc` crates so the
//! wire format is spec-compliant rather than hand-rolled (Arrow IPC metadata is
//! flatbuffers-encoded).
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
//! The element type is inferred per column from its first valid slot; an empty
//! or all-null column defaults to `Int64` (its length and null pattern still
//! round-trip exactly).
//!
//! This module is the **reference implementation**: the AOT twin
//! (`runtime/src/arrow_ipc.rs`, behind `libkarac_runtime_arrow.a`) covers the
//! write direction for all three types and is required to emit BYTE-IDENTICAL
//! streams — a change to the mapping rules here is a change to that contract,
//! and `tests/codegen.rs` will catch a one-sided edit. The parse direction is
//! still interpreter-only (codegen rejects it loudly). `karac run` routes an
//! arrow program here regardless, because the JIT runner links the runtime
//! without the opt-in `arrow` feature (mirroring the `gpu` / `regex`
//! fallback).

use std::io::Cursor;
use std::sync::Arc;

use arrow_array::{
    Array, BooleanArray, FixedSizeListArray, Float32Array, Float64Array, Int32Array, Int64Array,
    LargeStringArray, RecordBatch, RecordBatchOptions, StringArray,
};
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};

use super::value::Value;

/// One parsed column from an Arrow batch: `(name, data cells, validity mask)` —
/// the per-field shape `dataframe_from_ipc` returns, which the interpreter
/// turns into a `Value::Column`.
type NamedColumn = (String, Vec<Value>, Vec<bool>);

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

/// Build the Arrow `(DataType, array)` for one column's `(data, valid)`. A slot
/// that is null (`valid[i] == false`) — or, defensively, whose cell does not
/// match the inferred kind, which the typechecker's homogeneity rule forbids —
/// becomes an Arrow null.
fn col_to_arrow(data: &[Value], valid: &[bool]) -> (DataType, Arc<dyn Array>) {
    match infer_kind(data, valid) {
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
    }
}

/// Assemble a one-batch IPC stream from parallel `fields` / `arrays`. The row
/// count is taken from the first array (0 for a column-less frame), so a
/// zero-column `DataFrame` still produces a valid stream. Returns an error
/// string on any Arrow-side failure (kept as a plain `String` so the
/// interpreter can surface it as an ordinary runtime error).
fn write_ipc(fields: Vec<Field>, arrays: Vec<Arc<dyn Array>>) -> Result<Vec<u8>, String> {
    let schema = Arc::new(Schema::new(fields));
    let row_count = arrays.first().map_or(0, |a| a.len());
    let batch = RecordBatch::try_new_with_options(
        schema.clone(),
        arrays,
        &RecordBatchOptions::new().with_row_count(Some(row_count)),
    )
    .map_err(|e| format!("arrow: {e}"))?;

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer =
            StreamWriter::try_new(&mut buf, &schema).map_err(|e| format!("arrow: {e}"))?;
        writer.write(&batch).map_err(|e| format!("arrow: {e}"))?;
        writer.finish().map_err(|e| format!("arrow: {e}"))?;
    }
    Ok(buf)
}

/// Serialize a single column `(data, valid)` to a one-field (`col`) Arrow IPC
/// stream.
pub(super) fn column_to_ipc(data: &[Value], valid: &[bool]) -> Result<Vec<u8>, String> {
    let (dt, arr) = col_to_arrow(data, valid);
    write_ipc(vec![Field::new("col", dt, true)], vec![arr])
}

/// Arrow's canonical extension-type metadata keys (Arrow columnar spec §
/// "Extension types"). A reader that doesn't know the extension still sees the
/// underlying `FixedSizeList` storage, so the stream stays universally
/// readable.
const EXT_NAME_KEY: &str = "ARROW:extension:name";
const EXT_META_KEY: &str = "ARROW:extension:metadata";
/// The canonical N-d tensor extension — what pyarrow exposes as
/// `pa.fixed_shape_tensor(value_type, shape)`.
const FIXED_SHAPE_TENSOR: &str = "arrow.fixed_shape_tensor";

/// Serialize a `Tensor`'s `(dims, row-major data)` to a one-field Arrow IPC
/// stream typed as the canonical `arrow.fixed_shape_tensor` extension: a
/// single-row `FixedSizeList[numel]` holding the flattened values, with the
/// shape carried in the field's extension metadata (`{"shape":[d0,d1,…]}`).
/// pyarrow reads this back as a real tensor (`pa.fixed_shape_tensor`); a reader
/// that ignores extension metadata still sees the flat FixedSizeList storage.
///
/// A tensor has no null slots (no validity bitmap in `Value::Tensor`), so every
/// element is valid — the element-type inference is otherwise shared with
/// `Column` via `col_to_arrow`.
pub(super) fn tensor_to_ipc(dims: &[i64], data: &[Value]) -> Result<Vec<u8>, String> {
    let numel: i64 = dims.iter().product::<i64>().max(0);
    if numel != data.len() as i64 {
        return Err(format!(
            "arrow: Tensor shape {dims:?} implies {numel} elements but the buffer holds {}",
            data.len()
        ));
    }
    let list_size = i32::try_from(numel)
        .map_err(|_| format!("arrow: Tensor with {numel} elements exceeds Arrow's list size"))?;

    let all_valid = vec![true; data.len()];
    let (item_dt, values) = col_to_arrow(data, &all_valid);
    // Items are non-nullable — a tensor slot always holds a value.
    let item_field = Arc::new(Field::new("item", item_dt, false));
    let list = FixedSizeListArray::try_new(Arc::clone(&item_field), list_size, values, None)
        .map_err(|e| format!("arrow: {e}"))?;

    let shape = serde_json::json!({ "shape": dims });
    let metadata = std::collections::HashMap::from([
        (EXT_NAME_KEY.to_string(), FIXED_SHAPE_TENSOR.to_string()),
        (EXT_META_KEY.to_string(), shape.to_string()),
    ]);
    let field = Field::new(
        "tensor",
        DataType::FixedSizeList(item_field, list_size),
        false,
    )
    .with_metadata(metadata);

    write_ipc(vec![field], vec![Arc::new(list)])
}

/// Parse an `arrow.fixed_shape_tensor` IPC stream back into `(dims, data)`.
/// The shape comes from the field's extension metadata; a stream whose field
/// carries no shape (a plain `FixedSizeList`, or a foreign producer that
/// dropped the metadata) is read as a 1-D tensor over the flattened values, so
/// the data still round-trips even when the shape doesn't.
pub(super) fn tensor_from_ipc(bytes: &[u8]) -> Result<(Vec<i64>, Vec<Value>), String> {
    let batch = match read_first_batch(bytes)? {
        Some(b) if b.num_columns() > 0 => b,
        _ => return Ok((vec![0], Vec::new())),
    };
    let field = batch.schema().field(0).clone();
    let col = batch.column(0);

    // Unwrap the FixedSizeList storage to the flat value array. A stream that
    // is a plain (non-list) array is read as 1-D over its own values.
    let values: Arc<dyn Array> = match col.as_any().downcast_ref::<FixedSizeListArray>() {
        Some(list) => {
            if list.is_empty() {
                return Ok((vec![0], Vec::new()));
            }
            // One tensor per row; this surface serializes exactly one.
            list.value(0)
        }
        None => Arc::clone(col),
    };
    let (data, _valid) = arrow_to_col(values.as_ref())?;

    // Shape from `{"shape":[…]}` in the extension metadata; fall back to 1-D.
    let dims = field
        .metadata()
        .get(EXT_META_KEY)
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        .and_then(|v| {
            v.get("shape")?
                .as_array()?
                .iter()
                .map(|d| d.as_i64())
                .collect::<Option<Vec<i64>>>()
        })
        .filter(|d| !d.is_empty() && d.iter().product::<i64>() == data.len() as i64)
        .unwrap_or_else(|| vec![data.len() as i64]);
    Ok((dims, data))
}

/// Serialize a `DataFrame`'s `(name, Column)` list to an N-field Arrow IPC
/// stream — one field per column, field name = column name, in schema order.
pub(super) fn dataframe_to_ipc(columns: &[(String, Value)]) -> Result<Vec<u8>, String> {
    let mut fields = Vec::with_capacity(columns.len());
    let mut arrays: Vec<Arc<dyn Array>> = Vec::with_capacity(columns.len());
    for (name, col_val) in columns {
        let Value::Column { data, valid } = col_val else {
            return Err(format!(
                "arrow: DataFrame column '{name}' is not a Column value"
            ));
        };
        let data = data.read().unwrap();
        let valid = valid.read().unwrap();
        let (dt, arr) = col_to_arrow(&data, &valid);
        fields.push(Field::new(name, dt, true));
        arrays.push(arr);
    }
    write_ipc(fields, arrays)
}

/// Read the first `RecordBatch` from an IPC stream (`None` for an empty
/// stream).
fn read_first_batch(bytes: &[u8]) -> Result<Option<RecordBatch>, String> {
    let mut reader =
        StreamReader::try_new(Cursor::new(bytes), None).map_err(|e| format!("arrow: {e}"))?;
    match reader.next() {
        Some(b) => Ok(Some(b.map_err(|e| format!("arrow: {e}"))?)),
        None => Ok(None),
    }
}

/// Convert one Arrow array into a column's `(data, valid)`. A null slot becomes
/// `Value::Unit` in `data` with `false` in `valid` (the column's never-read
/// placeholder convention). `Int32` / `Float32` widen to `i64` / `f64` and
/// `LargeUtf8` reads as `String`, so a column a foreign producer wrote in a
/// narrow form still loads.
fn arrow_to_col(col: &dyn Array) -> Result<(Vec<Value>, Vec<bool>), String> {
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
            "arrow: from_arrow_ipc supports Int64/Int32, Float64/Float32, \
             Utf8/LargeUtf8, and Boolean columns; got {}",
            col.data_type()
        ));
    }
    Ok((data, valid))
}

/// Parse a one-field (or first-field-of-many) Arrow IPC stream into a column's
/// `(data, valid)`.
pub(super) fn column_from_ipc(bytes: &[u8]) -> Result<(Vec<Value>, Vec<bool>), String> {
    match read_first_batch(bytes)? {
        Some(batch) if batch.num_columns() > 0 => arrow_to_col(batch.column(0)),
        _ => Ok((Vec::new(), Vec::new())),
    }
}

/// Parse an Arrow IPC stream into a `DataFrame`'s `(name, data, valid)` triples
/// — one per field, in schema order.
pub(super) fn dataframe_from_ipc(bytes: &[u8]) -> Result<Vec<NamedColumn>, String> {
    let batch = match read_first_batch(bytes)? {
        Some(b) => b,
        None => return Ok(Vec::new()),
    };
    let schema = batch.schema();
    let mut out = Vec::with_capacity(batch.num_columns());
    for i in 0..batch.num_columns() {
        let name = schema.field(i).name().clone();
        let (data, valid) = arrow_to_col(batch.column(i))?;
        out.push((name, data, valid));
    }
    Ok(out)
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

    // Build a `Value::Column` from parallel data/valid for DataFrame tests.
    fn col(data: Vec<Value>, valid: Vec<bool>) -> Value {
        use std::sync::RwLock;
        Value::Column {
            data: Arc::new(RwLock::new(data)),
            valid: Arc::new(RwLock::new(valid)),
        }
    }

    #[test]
    fn dataframe_round_trips_names_types_nulls() {
        // Two columns of different element types, with a null, round-trip
        // through the multi-field batch: names, per-column types, and the null
        // pattern all survive.
        let columns = vec![
            (
                "id".to_string(),
                col(
                    vec![Value::Int(1), Value::Int(2), Value::Unit],
                    vec![true, true, false],
                ),
            ),
            (
                "name".to_string(),
                col(
                    vec![
                        Value::String("a".to_string()),
                        Value::String("b".to_string()),
                        Value::String("c".to_string()),
                    ],
                    vec![true, true, true],
                ),
            ),
        ];
        let bytes = dataframe_to_ipc(&columns).unwrap();
        let out = dataframe_from_ipc(&bytes).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "id");
        assert_eq!(out[1].0, "name");
        // id column: [1, 2, null]
        assert!(matches!(out[0].1[0], Value::Int(1)));
        assert!(matches!(out[0].1[2], Value::Unit));
        assert_eq!(out[0].2, vec![true, true, false]);
        // name column: ["a", "b", "c"]
        match &out[1].1[2] {
            Value::String(s) => assert_eq!(s.as_str(), "c"),
            _ => panic!("expected String"),
        }
        assert_eq!(out[1].2, vec![true, true, true]);
    }

    #[test]
    fn empty_dataframe_round_trips() {
        // A zero-column frame still produces a valid stream and reads back empty.
        let bytes = dataframe_to_ipc(&[]).unwrap();
        assert!(dataframe_from_ipc(&bytes).unwrap().is_empty());
    }

    #[test]
    fn tensor_writes_the_fixed_shape_tensor_extension() {
        // The stream must be typed as the canonical extension (so pyarrow reads
        // it as a real tensor), with the shape in the extension metadata and
        // FixedSizeList[numel] storage underneath.
        let dims = vec![2i64, 3];
        let data: Vec<Value> = (1..=6).map(Value::Int).collect();
        let bytes = tensor_to_ipc(&dims, &data).unwrap();

        let batch = read_first_batch(&bytes).unwrap().unwrap();
        let field = batch.schema().field(0).clone();
        assert_eq!(
            field.metadata().get(EXT_NAME_KEY).map(String::as_str),
            Some(FIXED_SHAPE_TENSOR)
        );
        assert_eq!(
            field.metadata().get(EXT_META_KEY).map(String::as_str),
            Some(r#"{"shape":[2,3]}"#)
        );
        assert!(matches!(
            field.data_type(),
            DataType::FixedSizeList(_, 6) // 2 * 3 flattened values
        ));
        // One row: the whole tensor.
        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn tensor_round_trips_shape_and_values() {
        let dims = vec![2i64, 3];
        let data: Vec<Value> = (1..=6).map(Value::Int).collect();
        let bytes = tensor_to_ipc(&dims, &data).unwrap();
        let (out_dims, out_data) = tensor_from_ipc(&bytes).unwrap();
        assert_eq!(out_dims, dims);
        assert_eq!(out_data.len(), 6);
        assert!(matches!(out_data[0], Value::Int(1)));
        assert!(matches!(out_data[5], Value::Int(6)));
    }

    #[test]
    fn tensor_without_shape_metadata_reads_as_1d() {
        // A foreign stream that is a plain (non-extension, non-list) array has
        // no shape to recover — it must still load, as a 1-D tensor over the
        // flattened values, rather than erroring.
        let arr = Arc::new(Int64Array::from(vec![7i64, 8, 9])) as Arc<dyn Array>;
        let (dims, data) = tensor_from_ipc(&ipc_stream(DataType::Int64, arr)).unwrap();
        assert_eq!(dims, vec![3]);
        assert_eq!(data.len(), 3);
        assert!(matches!(data[0], Value::Int(7)));
    }

    #[test]
    fn tensor_shape_buffer_mismatch_errors() {
        // A shape whose product disagrees with the buffer length is a clean
        // Err, never a silently truncated/padded tensor.
        let err = tensor_to_ipc(&[2, 3], &[Value::Int(1), Value::Int(2)]).unwrap_err();
        assert!(err.contains("implies 6 elements"), "got: {err}");
    }
}
