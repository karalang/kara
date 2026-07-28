//! Arrow IPC serialization for compiled (AOT) `Column`, `DataFrame`, and
//! `Tensor` values — the codegen twin of the interpreter's
//! `src/interpreter/arrow_ipc.rs` (phase-11 Arrow IPC codegen twin).
//!
//! Gated behind the opt-in `arrow` feature, which produces the separate
//! `libkarac_runtime_arrow.a` archive. `karac` auto-selects that archive only
//! when the emitted object references a `karac_arrow_*` symbol, so binaries
//! that never touch Arrow don't carry the arrow-rs dep (same posture as `gpu`
//! and `regex`).
//!
//! **Byte-identity with the interpreter is the contract**, and the E2E test
//! asserts it. Two rules make the two backends agree:
//!
//! 1. **Element-type mapping.** The interpreter's `Value` erases integer and
//!    float width (every int is `i64`, every float `f64`), so a compiled
//!    `Column[i32]` must serialize as `Int64` — the *interpreter's* view — not
//!    as the physically narrower Arrow type. `kind` selects the logical Arrow
//!    type; `elem_size` only says how to *read* each slot.
//! 2. **The all-null default.** The interpreter picks its element type from
//!    the first VALID slot and falls back to `Int64` when a column is empty or
//!    entirely null (it has no value to key on). The runtime knows the static
//!    type but deliberately applies the same fallback, so a compiled
//!    `Column[String]` with no valid slot emits `Int64` exactly like the
//!    interpreter does. Without this the two backends would diverge on
//!    degenerate columns only — the worst kind of divergence to discover late.
//!
//! The three entrypoints mirror the interpreter's three mappings exactly:
//! `Column` → a one-field (`col`) batch, `DataFrame` → an N-field batch in
//! schema order, `Tensor` → a single-row `FixedSizeList[numel]` tagged as the
//! canonical `arrow.fixed_shape_tensor` extension.

use std::sync::Arc;

use arrow_array::{
    Array, BooleanArray, FixedSizeListArray, Float64Array, Int64Array, RecordBatch,
    RecordBatchOptions, StringArray,
};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, Field, Schema};

/// Column element classes as codegen tags them in the DataFrame entry / passes
/// alongside a bare column control block. Mirrors the table in
/// `karac_runtime_df_write_csv`.
mod kind {
    pub const OTHER: i64 = 0; // bool at elem_size 1
    pub const SIGNED: i64 = 1;
    pub const UNSIGNED: i64 = 2;
    pub const FLOAT: i64 = 3;
    pub const STRING: i64 = 4;
}

/// One decoded slot from a compiled column — already widened to the
/// interpreter's value model (i64 / f64 / String / bool).
enum Slot {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

/// Read slot `row` out of a compiled column's data buffer.
///
/// # Safety
///
/// `data` must point to at least `(row + 1) * elem_size` readable bytes laid
/// out as codegen emits for `(kind, elem_size)`.
unsafe fn read_slot(data: *const u8, row: usize, elem_size: i64, kind: i64) -> Option<Slot> {
    let p = data.add(row * elem_size as usize);
    Some(match (kind, elem_size) {
        (kind::SIGNED, 1) => Slot::Int(i64::from(*(p as *const i8))),
        (kind::SIGNED, 2) => Slot::Int(i64::from(*(p as *const i16))),
        (kind::SIGNED, 4) => Slot::Int(i64::from(*(p as *const i32))),
        (kind::SIGNED, 8) => Slot::Int(*(p as *const i64)),
        (kind::UNSIGNED, 1) => Slot::Int(i64::from(*p)),
        (kind::UNSIGNED, 2) => Slot::Int(i64::from(*(p as *const u16))),
        (kind::UNSIGNED, 4) => Slot::Int(i64::from(*(p as *const u32))),
        // A u64 above i64::MAX wraps, matching the interpreter's `Value::Int`
        // (also i64) — the two stay identical, both lossy at the same point.
        (kind::UNSIGNED, 8) => Slot::Int(*(p as *const u64) as i64),
        (kind::FLOAT, 4) => Slot::Float(f64::from(*(p as *const f32))),
        (kind::FLOAT, 8) => Slot::Float(*(p as *const f64)),
        (kind::OTHER, 1) => Slot::Bool(*p != 0),
        (kind::STRING, _) => {
            // String element: the 24-byte `{ ptr, i64 len, i64 cap }` struct
            // inline in the data buffer.
            let sptr = *(p as *const *const u8);
            let slen = *(p.add(8) as *const i64);
            let s = if sptr.is_null() || slen <= 0 {
                String::new()
            } else {
                String::from_utf8_lossy(std::slice::from_raw_parts(sptr, slen as usize))
                    .into_owned()
            };
            Slot::Str(s)
        }
        // Unknown (kind, size) — treat as a null slot rather than risk UB.
        // New element classes must extend this table.
        _ => return None,
    })
}

/// Validity bit `row` of a compiled column's null bitmap (1 = valid). A null
/// bitmap pointer means "no validity info", which codegen only emits for an
/// all-valid column.
///
/// # Safety
///
/// `bitmap`, when non-null, must point to at least `row / 8 + 1` readable
/// bytes.
unsafe fn is_valid(bitmap: *const u8, row: usize) -> bool {
    if bitmap.is_null() {
        return true;
    }
    (*bitmap.add(row / 8) >> (row % 8)) & 1 == 1
}

/// Build the Arrow `(DataType, array)` for a decoded column. Applies the two
/// interpreter-parity rules documented at the module header: widths widen to
/// i64/f64, and a column with no valid slot falls back to `Int64`.
fn slots_to_arrow(slots: &[Option<Slot>], kind: i64, elem_size: i64) -> (DataType, Arc<dyn Array>) {
    let has_valid = slots.iter().any(|s| s.is_some());
    let logical = if has_valid { kind } else { kind::SIGNED };
    match (logical, elem_size) {
        (kind::STRING, _) => {
            let vals = slots.iter().map(|s| match s {
                Some(Slot::Str(v)) => Some(v.clone()),
                _ => None,
            });
            (DataType::Utf8, Arc::new(StringArray::from_iter(vals)))
        }
        (kind::FLOAT, _) => {
            let vals = slots.iter().map(|s| match s {
                Some(Slot::Float(v)) => Some(*v),
                Some(Slot::Int(v)) => Some(*v as f64),
                _ => None,
            });
            (DataType::Float64, Arc::new(Float64Array::from_iter(vals)))
        }
        (kind::OTHER, 1) => {
            let vals = slots.iter().map(|s| match s {
                Some(Slot::Bool(v)) => Some(*v),
                _ => None,
            });
            (DataType::Boolean, Arc::new(BooleanArray::from_iter(vals)))
        }
        // SIGNED / UNSIGNED, and the all-null fallback for every other kind.
        _ => {
            let vals = slots.iter().map(|s| match s {
                Some(Slot::Int(v)) => Some(*v),
                Some(Slot::Float(v)) => Some(*v as i64),
                _ => None,
            });
            (DataType::Int64, Arc::new(Int64Array::from_iter(vals)))
        }
    }
}

/// Write a set of fields + arrays as a single-batch Arrow IPC stream. Mirrors
/// the interpreter's `write_ipc` down to the explicit row count, which is what
/// lets a zero-column `DataFrame` still produce a valid batch (arrow can't
/// infer the row count with no arrays to ask).
fn write_ipc(fields: Vec<Field>, arrays: Vec<Arc<dyn Array>>) -> Option<Vec<u8>> {
    let schema = Arc::new(Schema::new(fields));
    let rows = arrays.first().map_or(0, |a| a.len());
    let batch = RecordBatch::try_new_with_options(
        schema.clone(),
        arrays,
        &RecordBatchOptions::new().with_row_count(Some(rows)),
    )
    .ok()?;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut w = StreamWriter::try_new(&mut buf, &schema).ok()?;
        w.write(&batch).ok()?;
        w.finish().ok()?;
    }
    Some(buf)
}

/// Copy `bytes` into a freshly allocated buffer codegen owns (and frees as an
/// ordinary `Vec[u8]` data pointer), reporting the length through `out_len`.
/// `max(len, 1)` so an empty stream is still a unique non-null freeable
/// pointer — codegen sets `cap = max(len, 1)` to match (the `karac_regex_*`
/// convention).
///
/// # Safety
///
/// `out_len`, when non-null, must point to a writable `i64`.
unsafe fn emit_buffer(bytes: &[u8], out_len: *mut i64) -> *mut u8 {
    let len = bytes.len();
    if !out_len.is_null() {
        *out_len = len as i64;
    }
    let buf = crate::alloc::karac_alloc_or_panic(if len == 0 { 1 } else { len });
    core::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, len);
    buf
}

/// Decode every slot of a compiled Column control block — `{ ptr data, ptr
/// null_bitmap, i64 len, i64 cap }` — into the interpreter's value model.
/// `elem_size` / `kind` travel alongside because a bare control block carries
/// no element tag (a DataFrame entry does, in its own trailing fields).
///
/// # Safety
///
/// `col_ctrl`, when non-null, must be a live Column control block laid out as
/// above, with a data buffer holding `len` slots of `elem_size` bytes.
unsafe fn read_column_slots(col_ctrl: *const u8, elem_size: i64, kind: i64) -> Vec<Option<Slot>> {
    if col_ctrl.is_null() {
        return Vec::new();
    }
    let data = *(col_ctrl as *const *const u8);
    let bitmap = *(col_ctrl.add(8) as *const *const u8);
    let len = (*(col_ctrl.add(16) as *const i64)).max(0) as usize;

    let mut slots: Vec<Option<Slot>> = Vec::with_capacity(len);
    for row in 0..len {
        slots.push(if is_valid(bitmap, row) && !data.is_null() {
            read_slot(data, row, elem_size, kind)
        } else {
            None
        });
    }
    slots
}

/// `col.to_arrow_ipc() -> Vec[u8]` — serialize a compiled `Column` to a
/// one-field (`col`) Arrow IPC stream. The AOT twin of the interpreter's
/// `column_to_ipc`; the two emit byte-identical streams (asserted E2E).
///
/// Returns the malloc'd stream buffer; `out_len` receives its length.
///
/// # Safety
///
/// `col_ctrl` must satisfy `read_column_slots`' contract; `out_len` must point
/// to a writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_arrow_column_to_ipc(
    col_ctrl: *const u8,
    elem_size: i64,
    kind: i64,
    out_len: *mut i64,
) -> *mut u8 {
    let slots = read_column_slots(col_ctrl, elem_size, kind);
    let (dt, arr) = slots_to_arrow(&slots, kind, elem_size);
    match write_ipc(vec![Field::new("col", dt, true)], vec![arr]) {
        Some(bytes) => emit_buffer(&bytes, out_len),
        // An arrow-side failure yields an empty stream rather than aborting the
        // program — the same "surface it as data" posture as the other
        // buffer-returning runtime entrypoints.
        None => emit_buffer(&[], out_len),
    }
}

/// `df.to_arrow_ipc() -> Vec[u8]` — serialize a compiled `DataFrame` to an
/// N-field Arrow IPC batch, one field per column in schema order. The AOT twin
/// of the interpreter's `dataframe_to_ipc`.
///
/// Walks the DataFrame control block `{ ptr entries, i64 len, i64 cap }` and
/// its stride-40 entries `{ ptr name_data, i64 name_len, ptr col_ctrl, i64
/// elem_size, i64 kind }` — the same walk as `karac_runtime_df_write_csv`, so
/// the two share one view of the layout. Each entry carries its own
/// `elem_size` / `kind`, so unlike the Column entrypoint nothing extra needs
/// passing from codegen.
///
/// # Safety
///
/// `df_ctrl` must be a live DataFrame control block laid out as above, with
/// every entry's `col_ctrl` satisfying `read_column_slots`' contract;
/// `out_len` must point to a writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_arrow_dataframe_to_ipc(
    df_ctrl: *const u8,
    out_len: *mut i64,
) -> *mut u8 {
    if df_ctrl.is_null() {
        return emit_buffer(&[], out_len);
    }
    let entries = *(df_ctrl as *const *const u8);
    let n_cols = (*(df_ctrl.add(8) as *const i64)).max(0) as usize;

    let mut fields: Vec<Field> = Vec::with_capacity(n_cols);
    let mut arrays: Vec<Arc<dyn Array>> = Vec::with_capacity(n_cols);
    for i in 0..n_cols {
        let e = entries.add(i * 40);
        let name_data = *(e as *const *const u8);
        let name_len = *(e.add(8) as *const i64);
        let col_ctrl = *(e.add(16) as *const *const u8);
        let elem_size = *(e.add(24) as *const i64);
        let kind = *(e.add(32) as *const i64);

        let name = if name_data.is_null() || name_len <= 0 {
            String::new()
        } else {
            String::from_utf8_lossy(std::slice::from_raw_parts(name_data, name_len as usize))
                .into_owned()
        };
        let slots = read_column_slots(col_ctrl, elem_size, kind);
        let (dt, arr) = slots_to_arrow(&slots, kind, elem_size);
        fields.push(Field::new(name, dt, true));
        arrays.push(arr);
    }

    match write_ipc(fields, arrays) {
        Some(bytes) => emit_buffer(&bytes, out_len),
        None => emit_buffer(&[], out_len),
    }
}

/// Arrow's canonical extension-type metadata keys (Arrow columnar spec §
/// "Extension types") and the fixed-shape-tensor extension name. Must match
/// the interpreter's constants verbatim — they land in the schema, which is
/// part of the byte stream.
const EXT_NAME_KEY: &str = "ARROW:extension:name";
const EXT_META_KEY: &str = "ARROW:extension:metadata";
const FIXED_SHAPE_TENSOR: &str = "arrow.fixed_shape_tensor";

/// The extension metadata payload: `{"shape":[d0,d1,…]}`.
///
/// The interpreter builds this with `serde_json`; here it is formatted by
/// hand, which is byte-identical because serde_json's compact `to_string`
/// emits no whitespace and renders `i64` exactly as `Display` does. Hand
/// formatting keeps `serde_json` out of the runtime's dependency tree for one
/// object with one integer-array field — and the E2E byte-identity test is
/// what actually holds the two in agreement.
fn shape_metadata(dims: &[i64]) -> String {
    let mut s = String::from("{\"shape\":[");
    for (i, d) in dims.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&d.to_string());
    }
    s.push_str("]}");
    s
}

/// `t.to_arrow_ipc() -> Vec[u8]` — serialize a compiled `Tensor` as the
/// canonical `arrow.fixed_shape_tensor` extension: a single-row
/// `FixedSizeList[numel]` over the row-major values, with the shape in the
/// field's extension metadata. The AOT twin of the interpreter's
/// `tensor_to_ipc`.
///
/// Rank and dims come from the tensor block's own header — `[i64 rank][rank ×
/// i64 dims][C-order data]` (`src/codegen/tensor.rs`) — which is authoritative
/// at runtime, so codegen passes only the element description. Tensor slots
/// are never null (a tensor has no validity concept), so every slot is read as
/// valid.
///
/// # Safety
///
/// `t_ptr` must be a live tensor block laid out as above, whose data region
/// holds `product(dims)` slots of `elem_size` bytes; `out_len` must point to a
/// writable `i64`.
#[no_mangle]
pub unsafe extern "C" fn karac_arrow_tensor_to_ipc(
    t_ptr: *const u8,
    elem_size: i64,
    kind: i64,
    out_len: *mut i64,
) -> *mut u8 {
    if t_ptr.is_null() {
        return emit_buffer(&[], out_len);
    }
    let rank = (*(t_ptr as *const i64)).max(0) as usize;
    let mut dims: Vec<i64> = Vec::with_capacity(rank);
    for i in 0..rank {
        dims.push(*(t_ptr.add(8 * (1 + i)) as *const i64));
    }
    let numel: i64 = dims.iter().product::<i64>().max(0);
    let data = t_ptr.add(8 * (1 + rank));

    let mut slots: Vec<Option<Slot>> = Vec::with_capacity(numel as usize);
    for row in 0..numel as usize {
        slots.push(read_slot(data, row, elem_size, kind));
    }
    let (item_dt, values) = slots_to_arrow(&slots, kind, elem_size);

    let Ok(list_size) = i32::try_from(numel) else {
        return emit_buffer(&[], out_len);
    };
    // Items are non-nullable — a tensor slot always holds a value.
    let item_field = Arc::new(Field::new("item", item_dt, false));
    let Ok(list) = FixedSizeListArray::try_new(Arc::clone(&item_field), list_size, values, None)
    else {
        return emit_buffer(&[], out_len);
    };

    let metadata = std::collections::HashMap::from([
        (EXT_NAME_KEY.to_string(), FIXED_SHAPE_TENSOR.to_string()),
        (EXT_META_KEY.to_string(), shape_metadata(&dims)),
    ]);
    let field = Field::new(
        "tensor",
        DataType::FixedSizeList(item_field, list_size),
        false,
    )
    .with_metadata(metadata);

    match write_ipc(vec![field], vec![Arc::new(list)]) {
        Some(bytes) => emit_buffer(&bytes, out_len),
        None => emit_buffer(&[], out_len),
    }
}
