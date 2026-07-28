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

use std::io::Cursor;
use std::sync::Arc;

use arrow_array::{
    Array, BooleanArray, FixedSizeListArray, Float32Array, Float64Array, Int32Array, Int64Array,
    LargeStringArray, RecordBatch, RecordBatchOptions, StringArray,
};
use arrow_ipc::{reader::StreamReader, writer::StreamWriter};
use arrow_schema::{DataType, Field, Schema};

use crate::file::{control_alloc_bytes, control_alloc_zeroed};

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

// ── Read direction (`from_arrow_ipc`) ───────────────────────────────
//
// The inverse of everything above, and the harder half: instead of *walking*
// a control block the runtime must *build* one, laid out exactly as codegen
// builds frames itself, so the caller's ordinary cleanup frees the whole
// graph. `karac_runtime_df_read_csv` established that shape (and the
// allocation pairing, which is why the two share `control_alloc_*`); these
// entrypoints reuse it verbatim.
//
// Failure is signalled by a NULL return — never a partially-built graph.
// Both builders allocate a control block unconditionally on success (even for
// a zero-row column), so null is unambiguous. Codegen turns it into a panic
// with a static message, the same posture as `Regex.compile`'s Err under AOT.

/// Read the first `RecordBatch` from an IPC stream. `Ok(None)` is an EMPTY
/// stream — a valid, empty result, not a failure; `Err(())` is a malformed
/// one. The interpreter's reader draws the line in exactly this place.
fn read_first_batch(bytes: &[u8]) -> Result<Option<RecordBatch>, ()> {
    let mut reader = StreamReader::try_new(Cursor::new(bytes), None).map_err(|_| ())?;
    match reader.next() {
        Some(b) => Ok(Some(b.map_err(|_| ())?)),
        None => Ok(None),
    }
}

/// Decode one Arrow array into the shared `Slot` model. Applies the same
/// widening as the interpreter's `arrow_to_col` — Int32 → i64, Float32 → f64,
/// LargeUtf8 → String — so a column a foreign producer wrote in a narrow form
/// still loads. `None` for an element type outside the supported set.
fn arrow_to_slots(col: &dyn Array) -> Option<Vec<Option<Slot>>> {
    let mut slots: Vec<Option<Slot>> = Vec::with_capacity(col.len());

    // Downcast to a concrete array type, then map each slot through `$ctor`
    // (a null slot becomes `None`, which every conversion below accepts).
    macro_rules! collect {
        ($arr:expr, $ctor:expr) => {{
            let arr = $arr;
            for i in 0..arr.len() {
                slots.push(if arr.is_null(i) {
                    None
                } else {
                    Some($ctor(arr.value(i)))
                });
            }
        }};
    }

    let any = col.as_any();
    if let Some(a) = any.downcast_ref::<Int64Array>() {
        collect!(a, Slot::Int);
    } else if let Some(a) = any.downcast_ref::<Int32Array>() {
        collect!(a, |x| Slot::Int(i64::from(x)));
    } else if let Some(a) = any.downcast_ref::<Float64Array>() {
        collect!(a, Slot::Float);
    } else if let Some(a) = any.downcast_ref::<Float32Array>() {
        collect!(a, |x| Slot::Float(f64::from(x)));
    } else if let Some(a) = any.downcast_ref::<StringArray>() {
        collect!(a, |x: &str| Slot::Str(x.to_string()));
    } else if let Some(a) = any.downcast_ref::<LargeStringArray>() {
        collect!(a, |x: &str| Slot::Str(x.to_string()));
    } else if let Some(a) = any.downcast_ref::<BooleanArray>() {
        collect!(a, Slot::Bool);
    } else {
        return None;
    }
    Some(slots)
}

/// Store one decoded slot into a compiled column's data buffer at `row`,
/// converting to the column's declared `(kind, elem_size)`. `false` means the
/// stream's element type does not convert to the declared one.
///
/// Numeric classes convert freely in both directions (int ↔ float, and any
/// width — a narrower declared type truncates, the same lossy step `as`
/// performs in Kāra source). `String` and `bool` convert to nothing but
/// themselves: silently rendering a number as text, or a string as 0, would
/// turn a type error into corrupt data.
///
/// # Safety
///
/// `data` must point to at least `(row + 1) * elem_size` writable bytes.
unsafe fn write_slot(data: *mut u8, row: usize, elem_size: i64, kind: i64, slot: &Slot) -> bool {
    let p = data.add(row * elem_size as usize);
    // Numeric source value, in both models — `None` for a non-numeric slot.
    let as_int = match slot {
        Slot::Int(v) => Some(*v),
        Slot::Float(v) => Some(*v as i64),
        _ => None,
    };
    let as_float = match slot {
        Slot::Int(v) => Some(*v as f64),
        Slot::Float(v) => Some(*v),
        _ => None,
    };
    match (kind, elem_size) {
        (kind::SIGNED | kind::UNSIGNED, 1) => match as_int {
            Some(v) => *p = v as u8,
            None => return false,
        },
        (kind::SIGNED | kind::UNSIGNED, 2) => match as_int {
            Some(v) => *(p as *mut u16) = v as u16,
            None => return false,
        },
        (kind::SIGNED | kind::UNSIGNED, 4) => match as_int {
            Some(v) => *(p as *mut u32) = v as u32,
            None => return false,
        },
        (kind::SIGNED | kind::UNSIGNED, 8) => match as_int {
            Some(v) => *(p as *mut i64) = v,
            None => return false,
        },
        (kind::FLOAT, 4) => match as_float {
            Some(v) => *(p as *mut f32) = v as f32,
            None => return false,
        },
        (kind::FLOAT, 8) => match as_float {
            Some(v) => *(p as *mut f64) = v,
            None => return false,
        },
        (kind::OTHER, 1) => match slot {
            Slot::Bool(v) => *p = u8::from(*v),
            _ => return false,
        },
        (kind::STRING, _) => match slot {
            Slot::Str(s) => {
                // The inline 24-byte `{ ptr, i64 len, i64 cap }`. `cap == len`
                // marks the heap as owned, which is what makes codegen's
                // cap-guarded per-cell free reclaim it.
                let bytes = s.as_bytes();
                *(p as *mut *mut u8) = control_alloc_bytes(bytes);
                *(p.add(8) as *mut i64) = bytes.len() as i64;
                *(p.add(16) as *mut i64) = bytes.len() as i64;
            }
            _ => return false,
        },
        // Unknown (kind, size) — the write-side `read_slot` table's peer, and
        // it must be extended in lockstep with it.
        _ => return false,
    }
    true
}

/// Build a compiled Column control block — `{ ptr data, ptr null_bitmap, i64
/// len, i64 cap }` — holding `slots` at the declared `(elem_size, kind)`.
/// `None` if any VALID slot fails to convert.
///
/// A null slot never fails, and that is load-bearing rather than incidental:
/// the write side deliberately falls back to `Int64` for a column with no
/// valid slot (matching the interpreter, which has no value to key its
/// element type on). So an empty or all-null `Column[String]` serializes as
/// Int64, and this is what lets it read back into a `Column[String]` instead
/// of tripping the String-vs-numeric rejection.
///
/// # Safety
///
/// The returned pointer, when non-null, owns a graph laid out exactly as
/// codegen builds it; the caller's ordinary Column cleanup frees it.
unsafe fn build_column_control(
    slots: &[Option<Slot>],
    elem_size: i64,
    kind: i64,
) -> Option<*mut u8> {
    if elem_size <= 0 {
        return None;
    }
    let rows = slots.len();
    let data = control_alloc_zeroed(rows * elem_size as usize);
    let bitmap = control_alloc_zeroed(rows.div_ceil(8));
    for (row, slot) in slots.iter().enumerate() {
        let Some(slot) = slot else { continue };
        if !write_slot(data, row, elem_size, kind, slot) {
            // Free what was built before giving up — the caller gets null and
            // has nothing to clean up, so this is the only chance.
            free_partial_column(data, bitmap, row, elem_size, kind);
            return None;
        }
        *bitmap.add(row / 8) |= 1 << (row % 8);
    }
    let ctrl = control_alloc_zeroed(32);
    *(ctrl as *mut *mut u8) = data;
    *(ctrl.add(8) as *mut *mut u8) = bitmap;
    *(ctrl.add(16) as *mut i64) = rows as i64;
    *(ctrl.add(24) as *mut i64) = rows as i64;
    Some(ctrl)
}

/// Release a column body abandoned mid-build: the first `written` slots'
/// String heaps (nothing else in a slot is separately allocated), then the
/// data and bitmap buffers.
///
/// # Safety
///
/// `data` / `bitmap` must be `control_alloc_zeroed` allocations, with the
/// first `written` slots of `data` initialised at `(elem_size, kind)`.
unsafe fn free_partial_column(
    data: *mut u8,
    bitmap: *mut u8,
    written: usize,
    elem_size: i64,
    kind: i64,
) {
    if kind == kind::STRING && !data.is_null() {
        for row in 0..written {
            let p = data.add(row * elem_size as usize);
            let sptr = *(p as *mut *mut u8);
            if !sptr.is_null() {
                crate::alloc::karac_free_buf(sptr, *(p.add(16) as *const i64) as usize);
            }
        }
    }
    if !data.is_null() {
        crate::alloc::karac_free_buf(data, written * elem_size as usize);
    }
    if !bitmap.is_null() {
        crate::alloc::karac_free_buf(bitmap, written.div_ceil(8));
    }
}

/// `Column.from_arrow_ipc(bytes) -> Column[T]` — parse a one-field (or
/// first-field-of-many) Arrow IPC stream into a freshly built `Column`
/// control block at the call site's declared element type.
///
/// Returns null when the stream is malformed, carries an unsupported element
/// type, or holds values that do not convert to `(elem_size, kind)` — codegen
/// panics on null.
///
/// # Safety
///
/// `bytes` must describe `len` readable bytes (or be null with `len <= 0`).
#[no_mangle]
pub unsafe extern "C" fn karac_arrow_column_from_ipc(
    bytes: *const u8,
    len: i64,
    elem_size: i64,
    kind: i64,
) -> *mut u8 {
    let buf: &[u8] = if bytes.is_null() || len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(bytes, len as usize)
    };
    // An empty input is an empty column, not a failure — the stream a
    // zero-length `Vec[u8]` describes has no schema to disagree with.
    if buf.is_empty() {
        return build_column_control(&[], elem_size, kind).unwrap_or(core::ptr::null_mut());
    }
    let slots = match read_first_batch(buf) {
        Ok(Some(batch)) if batch.num_columns() > 0 => {
            match arrow_to_slots(batch.column(0).as_ref()) {
                Some(s) => s,
                None => return core::ptr::null_mut(),
            }
        }
        Ok(_) => Vec::new(),
        Err(()) => return core::ptr::null_mut(),
    };
    build_column_control(&slots, elem_size, kind).unwrap_or(core::ptr::null_mut())
}

/// The compiled `(elem_size, kind)` a `DataFrame` column takes for an Arrow
/// element type. Every integer width lands on `i64` and every float on `f64`
/// — which is both `karac_runtime_df_read_csv`'s inference table and the
/// interpreter's value model, so a frame parsed under either backend has the
/// same column types. `None` for an unsupported type.
fn df_column_repr(dt: &DataType) -> Option<(i64, i64)> {
    Some(match dt {
        DataType::Int64 | DataType::Int32 => (8, kind::SIGNED),
        DataType::Float64 | DataType::Float32 => (8, kind::FLOAT),
        DataType::Utf8 | DataType::LargeUtf8 => (24, kind::STRING),
        DataType::Boolean => (1, kind::OTHER),
        _ => return None,
    })
}

/// `DataFrame.from_arrow_ipc(bytes) -> DataFrame` — parse an Arrow IPC stream
/// into a freshly built frame: one column per field, names and types from the
/// batch schema. Unlike the Column entrypoint nothing is declared at the call
/// site (a `DataFrame` is not generic), so each column's representation comes
/// from its Arrow type via `df_column_repr` — no conversion can fail, and the
/// only rejections are a malformed stream or an unsupported field type.
///
/// Returns null on failure; codegen panics on null.
///
/// # Safety
///
/// `bytes` must describe `len` readable bytes (or be null with `len <= 0`).
#[no_mangle]
pub unsafe extern "C" fn karac_arrow_dataframe_from_ipc(bytes: *const u8, len: i64) -> *mut u8 {
    let buf: &[u8] = if bytes.is_null() || len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(bytes, len as usize)
    };
    // Column name + built control block, held until every column has been
    // built — a failure part-way through must free the ones already made
    // rather than leak them behind a null return.
    let mut built: Vec<(String, *mut u8, i64, i64)> = Vec::new();

    if !buf.is_empty() {
        let batch = match read_first_batch(buf) {
            Ok(Some(b)) => Some(b),
            Ok(None) => None,
            Err(()) => return core::ptr::null_mut(),
        };
        if let Some(batch) = batch {
            let schema = batch.schema();
            for i in 0..batch.num_columns() {
                let arr = batch.column(i);
                let built_col = df_column_repr(arr.data_type())
                    .zip(arrow_to_slots(arr.as_ref()))
                    .and_then(|((elem_size, kind), slots)| {
                        build_column_control(&slots, elem_size, kind).map(|c| (c, elem_size, kind))
                    });
                match built_col {
                    Some((ctrl, elem_size, kind)) => {
                        built.push((schema.field(i).name().clone(), ctrl, elem_size, kind))
                    }
                    None => {
                        free_built_columns(&built);
                        return core::ptr::null_mut();
                    }
                }
            }
        }
    }

    // Entries (stride 40: name*, name_len, col_ctrl*, elem_size, kind) and the
    // frame control block `{ entries, len, capacity }` — `df_read_csv`'s
    // layout exactly, so `FreeDataFrame` cleanup frees the whole graph.
    let width = built.len();
    let entries = control_alloc_zeroed(width * 40);
    for (ci, (name, ctrl, elem_size, kind)) in built.iter().enumerate() {
        let e = entries.add(ci * 40);
        let nbytes = name.as_bytes();
        *(e as *mut *mut u8) = control_alloc_bytes(nbytes);
        *(e.add(8) as *mut i64) = nbytes.len() as i64;
        *(e.add(16) as *mut *mut u8) = *ctrl;
        *(e.add(24) as *mut i64) = *elem_size;
        *(e.add(32) as *mut i64) = *kind;
    }
    let control = control_alloc_zeroed(24);
    *(control as *mut *mut u8) = entries;
    *(control.add(8) as *mut i64) = width as i64;
    *(control.add(16) as *mut i64) = width as i64;
    control
}

/// Release columns built before a later one failed — nothing outside this
/// function knows they exist, so this is their only cleanup path.
///
/// # Safety
///
/// Each entry must be a `build_column_control` result at the stated
/// `(elem_size, kind)`.
unsafe fn free_built_columns(built: &[(String, *mut u8, i64, i64)]) {
    for (_, ctrl, elem_size, kind) in built {
        let data = *(*ctrl as *mut *mut u8);
        let bitmap = *((*ctrl).add(8) as *mut *mut u8);
        let rows = *((*ctrl).add(16) as *const i64);
        free_partial_column(data, bitmap, rows.max(0) as usize, *elem_size, *kind);
        crate::alloc::karac_free_buf(*ctrl, 32);
    }
}

/// Parse the shape of a tensor stream and its flat values.
///
/// Mirrors the interpreter's `tensor_from_ipc` decision-for-decision, because
/// the two must agree on which streams are readable and what shape they carry:
/// a `FixedSizeList` column is unwrapped to its FIRST row's values (this
/// surface serializes exactly one tensor per stream), a plain non-list column
/// is read as 1-D over its own values, and the dims come from the
/// `arrow.fixed_shape_tensor` extension metadata — accepted only when it is
/// non-empty and its product matches the value count, so a producer that
/// dropped or corrupted the metadata still yields a valid 1-D tensor rather
/// than a mis-shaped one.
fn tensor_shape_and_slots(bytes: &[u8]) -> Option<(Vec<i64>, Vec<Option<Slot>>)> {
    let batch = match read_first_batch(bytes) {
        Ok(Some(b)) if b.num_columns() > 0 => b,
        // An empty stream (or a batch with no columns) is the empty tensor,
        // matching the interpreter's `(vec![0], vec![])`.
        Ok(_) => return Some((vec![0], Vec::new())),
        Err(()) => return None,
    };
    let field = batch.schema().field(0).clone();
    let col = batch.column(0);

    let values: Arc<dyn Array> = match col.as_any().downcast_ref::<FixedSizeListArray>() {
        Some(list) => {
            if list.is_empty() {
                return Some((vec![0], Vec::new()));
            }
            list.value(0)
        }
        None => Arc::clone(col),
    };
    let slots = arrow_to_slots(values.as_ref())?;

    let dims = parse_shape_metadata(field.metadata().get(EXT_META_KEY).map(String::as_str))
        .filter(|d| !d.is_empty() && d.iter().product::<i64>() == slots.len() as i64)
        .unwrap_or_else(|| vec![slots.len() as i64]);
    Some((dims, slots))
}

/// Extract `shape` from the extension metadata `{"shape":[d0,d1,…]}`.
///
/// The write side hand-formats this object (see `shape_metadata`), so the read
/// side hand-parses it: it is one key holding one integer array, and pulling
/// `serde_json` into the runtime to read it back would add a dependency for a
/// grammar this small. Anything that isn't that exact shape yields `None`,
/// which the caller turns into the 1-D fallback — the same tolerance the
/// interpreter's `serde_json` path gets from its `.ok()` / `.filter(…)` chain.
fn parse_shape_metadata(meta: Option<&str>) -> Option<Vec<i64>> {
    let meta = meta?;
    let start = meta.find("\"shape\"")?;
    let rest = &meta[start + "\"shape\"".len()..];
    let open = rest.find('[')?;
    // Only whitespace and the `:` separator may sit between key and array.
    if !rest[..open]
        .trim_matches(|c: char| c.is_whitespace() || c == ':')
        .is_empty()
    {
        return None;
    }
    let close = rest.find(']')?;
    if close < open {
        return None;
    }
    let body = rest[open + 1..close].trim();
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(',')
        .map(|t| t.trim().parse::<i64>().ok())
        .collect()
}

/// `Tensor.from_arrow_ipc(bytes) -> Tensor[T, S]` — parse a stream into a
/// freshly built tensor block `[i64 rank][rank × i64 dims][C-order data]`, the
/// single allocation `src/codegen/tensor.rs` makes for a tensor, so the
/// caller's ordinary free reclaims it.
///
/// Unlike the tabular pair this leg must also RECONCILE shapes. The stream
/// carries its own dims; the receiver declares a rank and, per axis, either a
/// concrete extent or `?`. `want_dims[i] < 0` encodes `?` ("accept whatever
/// the stream says"); any other value must match exactly, and the ranks must
/// always match. This is the construction-boundary check of design.md
/// § Runtime equality check, moved runtime-side because the stream's shape
/// isn't known until it is parsed.
///
/// Tensors have no null concept, so a null slot (which a canonical
/// `arrow.fixed_shape_tensor` cannot contain — its items are non-nullable —
/// but a foreign producer could send) reads as the zero value rather than
/// failing: the allocation is already zeroed, and there is nothing in a Kāra
/// tensor that could preserve the distinction.
///
/// Returns null on a malformed stream, an unsupported or non-converting
/// element type, or a shape the receiver's annotation rejects.
///
/// # Safety
///
/// `bytes` must describe `len` readable bytes (or be null with `len <= 0`);
/// `want_dims` must point to `want_rank` readable `i64`s.
#[no_mangle]
pub unsafe extern "C" fn karac_arrow_tensor_from_ipc(
    bytes: *const u8,
    len: i64,
    elem_size: i64,
    kind: i64,
    want_rank: i64,
    want_dims: *const i64,
) -> *mut u8 {
    // A tensor element is always a scalar — no String tensors exist at this
    // surface, which is what lets the failure path below free the block as the
    // whole graph rather than walking per-cell heaps.
    if elem_size <= 0 || want_rank < 0 || kind == kind::STRING {
        return core::ptr::null_mut();
    }
    let buf: &[u8] = if bytes.is_null() || len <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(bytes, len as usize)
    };
    let Some((dims, slots)) = tensor_shape_and_slots(buf) else {
        return core::ptr::null_mut();
    };

    // Shape reconciliation against the receiver's annotation.
    let want_rank = want_rank as usize;
    if dims.len() != want_rank {
        return core::ptr::null_mut();
    }
    if !want_dims.is_null() {
        for (i, d) in dims.iter().enumerate() {
            let want = *want_dims.add(i);
            if want >= 0 && want != *d {
                return core::ptr::null_mut();
            }
        }
    }

    let numel: i64 = dims.iter().product::<i64>().max(0);
    if numel != slots.len() as i64 {
        return core::ptr::null_mut();
    }

    let header_bytes = 8 * (1 + want_rank);
    let block = control_alloc_zeroed(header_bytes + numel as usize * elem_size as usize);
    if block.is_null() {
        return core::ptr::null_mut();
    }
    *(block as *mut i64) = want_rank as i64;
    for (i, d) in dims.iter().enumerate() {
        *(block.add(8 * (1 + i)) as *mut i64) = *d;
    }
    let data = block.add(header_bytes);
    for (row, slot) in slots.iter().enumerate() {
        // A null slot keeps the zeroed value — see the doc comment.
        let Some(slot) = slot else { continue };
        if !write_slot(data, row, elem_size, kind, slot) {
            // A tensor element is never separately allocated (String tensors
            // don't exist at this surface), so the block IS the whole graph.
            crate::alloc::karac_free_buf(block, header_bytes + numel as usize * elem_size as usize);
            return core::ptr::null_mut();
        }
    }
    block
}
