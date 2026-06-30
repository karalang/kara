//! `Column[T]` codegen lowering (phase-11 data-science stdlib, Arrow
//! commitment Q5). The interpreter MVP (`src/interpreter/method_call_column.rs`)
//! carries the logical semantics; this is the native LLVM lowering with
//! the **real bit-packed Apache Arrow layout**.
//!
//! **Value layout.** A `Column[T]` value is a single pointer to one
//! malloc'd control block, field order per design.md § Column:
//!
//! ```text
//! { ptr data, ptr null_bitmap, i64 len, i64 capacity }
//! ```
//!
//! - `data` — a contiguous buffer of `capacity` `T`-typed elements
//!   (a separate Arrow values buffer).
//! - `null_bitmap` — a separate bit-packed validity buffer of
//!   `ceil(capacity/8)` bytes; bit `i` is `1` for a valid slot and `0`
//!   for a SQL null (Arrow's validity convention).
//! - `len` / `capacity` — the logical length and the allocated slack.
//!
//! The single-pointer value gives a trivial ABI (params / returns / moves
//! are pointer copies); the indirection (vs. an inline `{ptr,len,cap}`
//! Vec struct) lets `push` grow the two buffers in place without writing
//! back to the variable slot — the control pointer is stable across a
//! `mut ref self` mutation.
//!
//! **Ownership.** A bound column gets a `CleanupAction::FreeColumn` that
//! frees `data`, `null_bitmap`, then the control block (three `free`s),
//! null-guarded so a moved-out column (its slot nulled by the
//! move-suppression sentinel) is skipped — the Column analog of
//! `FreeTensor` / `FreeVecBuffer`.
//!
//! **Scope.** Constructors `new` / `with_capacity` / `from_vec` /
//! `from_iter_nullable`; mutators `push` / `push_null`; accessors `len` /
//! `null_count` / `valid_count` / `is_null`; positional indexing
//! `c[i] -> Option[T]`; the Column-returning transforms `fillna` /
//! `dropna`; the Vec-returning iterators `iter` -> `Vec[Option[T]]` and
//! `iter_valid` -> `Vec[T]`; and the SQL three-valued-logic element-wise
//! operators (`+ - * /` -> `Column[T]`, comparisons -> `Column[bool]`,
//! unary `-`, with null propagation). Element types are the numeric
//! primitives and `bool` (POD, ≤ one word), matching the Tensor codegen
//! surface. Only `fillna`'s `treat_nan_as_null` float flag remains
//! interpreter-only (`karac run`).

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

use super::state::ColumnVarInfo;
use super::tensor::type_expr_is_unsigned_int;
use crate::ast::{BinOp, CallArg, Expr, ExprKind, GenericArg, TypeExpr, TypeKind};
use crate::token::Span;

impl<'ctx> super::Codegen<'ctx> {
    // ── Layout helpers ──────────────────────────────────────────

    /// The control-block LLVM type `{ ptr data, ptr null_bitmap, i64
    /// len, i64 capacity }` (field order per design.md § Column).
    pub(super) fn column_control_struct_type(&self) -> StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default()).into();
        let i64_ty = self.context.i64_type().into();
        self.context
            .struct_type(&[ptr_ty, ptr_ty, i64_ty, i64_ty], false)
    }

    /// Element size in bytes for the supported (POD) element types —
    /// mirrors `tensor_elem_size`. `pub(super)` so the DataFrame lowering
    /// can size a stored column's data buffer for copy-in / copy-out.
    pub(super) fn column_elem_size(&self, elem: BasicTypeEnum<'ctx>) -> Result<u64, String> {
        match elem {
            BasicTypeEnum::FloatType(ft) => {
                if ft == self.context.f64_type() {
                    Ok(8)
                } else {
                    Ok(4)
                }
            }
            BasicTypeEnum::IntType(it) => Ok((it.get_bit_width() as u64).div_ceil(8)),
            // `String` (the `{ ptr, i64, i64 }` value struct) — a heap
            // element; the data buffer stores the 24-byte structs inline,
            // and the per-element String heap is cloned/freed separately.
            BasicTypeEnum::StructType(_) if self.column_elem_is_string(elem) => Ok(24),
            other => Err(format!(
                "Column element type {:?} is not yet supported in codegen — \
                 numeric primitives, bool, and String only",
                other
            )),
        }
    }

    /// Whether a Column element is `String` (a heap element). The `String`
    /// value type is the `{ ptr, i64, i64 }` struct (shared with `Vec`);
    /// `Column` only admits numeric / bool / String elements, so a struct
    /// element is a String. Heap elements need per-slot clone (copy in/out)
    /// and per-slot free (drop), unlike POD numeric / bool elements.
    pub(super) fn column_elem_is_string(&self, elem: BasicTypeEnum<'ctx>) -> bool {
        matches!(elem, BasicTypeEnum::StructType(st) if st == self.vec_struct_type())
    }

    /// `ceil(n / 8)` — the byte count of an `n`-element validity bitmap.
    fn column_bitmap_bytes(&self, n: IntValue<'ctx>) -> IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        self.builder
            .build_right_shift(
                self.builder
                    .build_int_add(n, i64_t.const_int(7, false), "col.bm.add7")
                    .unwrap(),
                i64_t.const_int(3, false),
                false,
                "col.bm.bytes",
            )
            .unwrap()
    }

    /// Extract a `ColumnVarInfo` from an annotation TypeExpr
    /// (`Column[T]`). Returns `None` for non-Column types.
    pub(super) fn column_var_info_from_type_expr(
        &self,
        te: &TypeExpr,
    ) -> Option<ColumnVarInfo<'ctx>> {
        let TypeKind::Path(path) = &te.kind else {
            return None;
        };
        if path.segments.last().map(|s| s.as_str()) != Some("Column") {
            return None;
        }
        let gargs = path.generic_args.as_ref()?;
        let elem_te = gargs.iter().find_map(|ga| match ga {
            GenericArg::Type(t) => Some(t),
            _ => None,
        })?;
        Some(ColumnVarInfo {
            elem: self.llvm_type_for_type_expr(elem_te),
            elem_unsigned: type_expr_is_unsigned_int(elem_te),
        })
    }

    /// `ColumnVarInfo` from the lowering side-table's plain-data
    /// `ColumnTypeInfo` (unannotated bindings).
    pub(super) fn column_var_info_from_table(
        &self,
        ci: &crate::ast::ColumnTypeInfo,
    ) -> ColumnVarInfo<'ctx> {
        ColumnVarInfo {
            elem: self.llvm_type_for_type_expr(&ci.elem),
            elem_unsigned: type_expr_is_unsigned_int(&ci.elem),
        }
    }

    /// Deep-copy a column control block into a fresh, fully independent
    /// column (new control + data + bitmap, byte-wise `memcpy`). The
    /// erased control block doesn't carry its element size, so the caller
    /// — which knows it, from a static `Column[T]` type at copy-in or the
    /// `elem_size` it stored at copy-out — passes `elem_size` (bytes per
    /// element). The fresh column owns its allocations; the source is
    /// untouched. This is the primitive behind DataFrame value semantics
    /// (`insert` copies in, `column` copies out), so the frame owns its
    /// columns outright and `karac run` / `karac build` agree.
    pub(super) fn column_deep_copy(
        &self,
        src: PointerValue<'ctx>,
        elem_size: IntValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, String> {
        let src_data = self
            .column_load_field(src, 0, "col.cp.src.data")
            .into_pointer_value();
        let src_bm = self
            .column_load_field(src, 1, "col.cp.src.bm")
            .into_pointer_value();
        let len = self
            .column_load_field(src, 2, "col.cp.len")
            .into_int_value();
        let cap = self
            .column_load_field(src, 3, "col.cp.cap")
            .into_int_value();
        let data_bytes = self
            .builder
            .build_int_mul(cap, elem_size, "col.cp.dbytes")
            .unwrap();
        let bm_bytes = self.column_bitmap_bytes(cap);

        let ctrl_bytes = self.column_control_struct_type().size_of().unwrap();
        let control = self
            .builder
            .build_call(self.malloc_fn, &[ctrl_bytes.into()], "col.cp.ctrl")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let data = self
            .builder
            .build_call(self.malloc_fn, &[data_bytes.into()], "col.cp.data")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(data, 8, src_data, 8, data_bytes)
            .map_err(|e| format!("column_deep_copy data memcpy failed: {e:?}"))?;
        let bitmap = self
            .builder
            .build_call(self.malloc_fn, &[bm_bytes.into()], "col.cp.bm")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(bitmap, 1, src_bm, 1, bm_bytes)
            .map_err(|e| format!("column_deep_copy bitmap memcpy failed: {e:?}"))?;

        self.builder
            .build_store(self.column_field_slot(control, 0, "col.cp.f.data"), data)
            .unwrap();
        self.builder
            .build_store(self.column_field_slot(control, 1, "col.cp.f.bm"), bitmap)
            .unwrap();
        self.builder
            .build_store(self.column_field_slot(control, 2, "col.cp.f.len"), len)
            .unwrap();
        self.builder
            .build_store(self.column_field_slot(control, 3, "col.cp.f.cap"), cap)
            .unwrap();
        Ok(control)
    }

    /// Free a column control block's three allocations (data + bitmap +
    /// control). Unguarded — callers (the DataFrame drop loop) only pass
    /// live, frame-owned column pointers. `pub(super)` for reuse.
    pub(super) fn column_free_allocations(&self, ctrl: PointerValue<'ctx>) {
        let data = self
            .column_load_field(ctrl, 0, "col.free.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(ctrl, 1, "col.free.bm")
            .into_pointer_value();
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[bitmap.into()], "")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[ctrl.into()], "")
            .unwrap();
    }

    /// Load a column control block's `len` field (index 2). `pub(super)`
    /// for the DataFrame `height` accessor.
    pub(super) fn column_len_field(&self, ctrl: PointerValue<'ctx>) -> IntValue<'ctx> {
        self.column_load_field(ctrl, 2, "col.lenf").into_int_value()
    }

    /// Load the control pointer from a column binding's slot.
    pub(super) fn column_ptr_for_var(&self, name: &str) -> Result<PointerValue<'ctx>, String> {
        let slot = self
            .variables
            .get(name)
            .ok_or_else(|| format!("Undefined column variable '{}'", name))?;
        Ok(self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                slot.ptr,
                &format!("{}.col", name),
            )
            .unwrap()
            .into_pointer_value())
    }

    /// GEP + load a control-block field. Index 0 = data (ptr),
    /// 1 = null_bitmap (ptr), 2 = len (i64), 3 = capacity (i64).
    fn column_load_field(
        &self,
        control: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let st = self.column_control_struct_type();
        let p = self
            .builder
            .build_struct_gep(st, control, idx, &format!("{name}.p"))
            .unwrap();
        let ty: BasicTypeEnum<'ctx> = if idx < 2 {
            self.context.ptr_type(AddressSpace::default()).into()
        } else {
            self.context.i64_type().into()
        };
        self.builder.build_load(ty, p, name).unwrap()
    }

    /// GEP to a control-block field slot (for stores).
    fn column_field_slot(
        &self,
        control: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> PointerValue<'ctx> {
        let st = self.column_control_struct_type();
        self.builder
            .build_struct_gep(st, control, idx, name)
            .unwrap()
    }

    /// Write validity bit `idx` of `bitmap` to `valid` (compile-time
    /// constant: `true` for a valid slot, `false` for a SQL null).
    /// Each slot's bit is written precisely (OR-set / AND-NOT-clear), so
    /// adjacent bits in the same byte are never disturbed and a grown
    /// bitmap needs no zero-fill (every readable bit `< len` was written
    /// when its slot was pushed).
    fn column_write_bit(&self, bitmap: PointerValue<'ctx>, idx: IntValue<'ctx>, valid: bool) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let byte_idx = self
            .builder
            .build_right_shift(idx, i64_t.const_int(3, false), false, "col.byteidx")
            .unwrap();
        let bit_in = self
            .builder
            .build_and(idx, i64_t.const_int(7, false), "col.bitidx")
            .unwrap();
        let bit_in8 = self
            .builder
            .build_int_truncate(bit_in, i8_t, "col.bitidx8")
            .unwrap();
        let mask = self
            .builder
            .build_left_shift(i8_t.const_int(1, false), bit_in8, "col.mask")
            .unwrap();
        let byte_ptr = unsafe {
            self.builder
                .build_gep(i8_t, bitmap, &[byte_idx], "col.bytep")
                .unwrap()
        };
        let byte = self
            .builder
            .build_load(i8_t, byte_ptr, "col.byte")
            .unwrap()
            .into_int_value();
        let new_byte = if valid {
            self.builder.build_or(byte, mask, "col.byte.set").unwrap()
        } else {
            let notmask = self.builder.build_not(mask, "col.notmask").unwrap();
            self.builder
                .build_and(byte, notmask, "col.byte.clr")
                .unwrap()
        };
        self.builder.build_store(byte_ptr, new_byte).unwrap();
    }

    /// Write validity bit `idx` of `bitmap` to a *runtime* `i1` `valid`
    /// (the `from_iter_nullable` per-slot `Some`/`None` case, where
    /// validity isn't a compile-time constant). `byte = valid ? (byte |
    /// mask) : (byte & ~mask)`.
    fn column_write_bit_runtime(
        &self,
        bitmap: PointerValue<'ctx>,
        idx: IntValue<'ctx>,
        valid: IntValue<'ctx>,
    ) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let byte_idx = self
            .builder
            .build_right_shift(idx, i64_t.const_int(3, false), false, "col.rbyteidx")
            .unwrap();
        let bit_in = self
            .builder
            .build_and(idx, i64_t.const_int(7, false), "col.rbitidx")
            .unwrap();
        let bit_in8 = self
            .builder
            .build_int_truncate(bit_in, i8_t, "col.rbitidx8")
            .unwrap();
        let mask = self
            .builder
            .build_left_shift(i8_t.const_int(1, false), bit_in8, "col.rmask")
            .unwrap();
        let byte_ptr = unsafe {
            self.builder
                .build_gep(i8_t, bitmap, &[byte_idx], "col.rbytep")
                .unwrap()
        };
        let byte = self
            .builder
            .build_load(i8_t, byte_ptr, "col.rbyte")
            .unwrap()
            .into_int_value();
        let set = self.builder.build_or(byte, mask, "col.rset").unwrap();
        let notmask = self.builder.build_not(mask, "col.rnotmask").unwrap();
        let clr = self.builder.build_and(byte, notmask, "col.rclr").unwrap();
        let new_byte = self
            .builder
            .build_select(valid, set, clr, "col.rbyte.new")
            .unwrap()
            .into_int_value();
        self.builder.build_store(byte_ptr, new_byte).unwrap();
    }

    /// Load validity bit `idx` of `bitmap` as an `i1` (`true` = valid).
    pub(super) fn column_load_valid_bit(
        &self,
        bitmap: PointerValue<'ctx>,
        idx: IntValue<'ctx>,
    ) -> IntValue<'ctx> {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let byte_idx = self
            .builder
            .build_right_shift(idx, i64_t.const_int(3, false), false, "col.byteidx")
            .unwrap();
        let bit_in = self
            .builder
            .build_and(idx, i64_t.const_int(7, false), "col.bitidx")
            .unwrap();
        let bit_in8 = self
            .builder
            .build_int_truncate(bit_in, i8_t, "col.bitidx8")
            .unwrap();
        let byte_ptr = unsafe {
            self.builder
                .build_gep(i8_t, bitmap, &[byte_idx], "col.bytep")
                .unwrap()
        };
        let byte = self
            .builder
            .build_load(i8_t, byte_ptr, "col.byte")
            .unwrap()
            .into_int_value();
        let shifted = self
            .builder
            .build_right_shift(byte, bit_in8, false, "col.bit.sh")
            .unwrap();
        let bit = self
            .builder
            .build_and(shifted, i8_t.const_int(1, false), "col.bit")
            .unwrap();
        self.builder
            .build_int_compare(IntPredicate::EQ, bit, i8_t.const_int(1, false), "col.valid")
            .unwrap()
    }

    /// Encode a loaded element value as a single i64 enum-payload word
    /// (for the `Option[T]` index result). The inverse decode is the
    /// canonical Option-match path. Handles the POD element types;
    /// `f32` is bit-cast to `i32` then zero-extended (a direct
    /// `f32 -> i64` bit-cast is a size mismatch).
    fn column_value_to_word(&self, val: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        match val {
            BasicValueEnum::FloatValue(fv) => {
                if fv.get_type() == self.context.f64_type() {
                    self.builder
                        .build_bit_cast(fv, i64_t, "col.fbits")
                        .unwrap()
                        .into_int_value()
                } else {
                    let i32_t = self.context.i32_type();
                    let bits = self
                        .builder
                        .build_bit_cast(fv, i32_t, "col.f32bits")
                        .unwrap()
                        .into_int_value();
                    self.builder
                        .build_int_z_extend(bits, i64_t, "col.f32word")
                        .unwrap()
                }
            }
            // ints / bool / ptr — `coerce_to_i64` zext/truncs / casts.
            other => self
                .coerce_to_i64(other)
                .unwrap_or_else(|_| i64_t.const_zero()),
        }
    }

    /// Decode an enum-payload word `w` (the `Option[T].Some` payload) back
    /// to an `elem`-typed value — the inverse of `column_value_to_word`,
    /// used by `from_iter_nullable` reading a `Vec[Option[T]]`.
    fn column_word_to_value(
        &self,
        w: IntValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match elem {
            BasicTypeEnum::IntType(it) => {
                let width = it.get_bit_width();
                if width == 64 {
                    w.into()
                } else {
                    self.builder
                        .build_int_truncate(w, it, "col.w2v.trunc")
                        .unwrap()
                        .into()
                }
            }
            BasicTypeEnum::FloatType(ft) => {
                if ft == self.context.f64_type() {
                    self.builder.build_bit_cast(w, ft, "col.w2v.f64").unwrap()
                } else {
                    let i32_t = self.context.i32_type();
                    let lo = self
                        .builder
                        .build_int_truncate(w, i32_t, "col.w2v.lo")
                        .unwrap();
                    self.builder.build_bit_cast(lo, ft, "col.w2v.f32").unwrap()
                }
            }
            other => other.const_zero(),
        }
    }

    /// Allocate a fresh control block + `capacity`-sized data buffer +
    /// `ceil(capacity/8)`-byte validity bitmap. Stores all four fields;
    /// the caller fills the data + bitmap. Neither buffer is
    /// zero-initialised (the caller writes exactly the live slots).
    fn column_alloc(
        &self,
        elem: BasicTypeEnum<'ctx>,
        capacity: IntValue<'ctx>,
        len: IntValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let elem_size = i64_t.const_int(self.column_elem_size(elem)?, false);

        let ctrl_bytes = self.column_control_struct_type().size_of().unwrap();
        let control = self
            .builder
            .build_call(self.malloc_fn, &[ctrl_bytes.into()], "col.ctrl")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let data_bytes = self
            .builder
            .build_int_mul(capacity, elem_size, "col.data.bytes")
            .unwrap();
        let data = self
            .builder
            .build_call(self.malloc_fn, &[data_bytes.into()], "col.data")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // ceil(capacity / 8) == (capacity + 7) >> 3.
        let bm_bytes = self
            .builder
            .build_right_shift(
                self.builder
                    .build_int_add(capacity, i64_t.const_int(7, false), "col.bm.add7")
                    .unwrap(),
                i64_t.const_int(3, false),
                false,
                "col.bm.bytes",
            )
            .unwrap();
        let bitmap = self
            .builder
            .build_call(self.malloc_fn, &[bm_bytes.into()], "col.bm")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        self.builder
            .build_store(self.column_field_slot(control, 0, "col.f.data"), data)
            .unwrap();
        self.builder
            .build_store(self.column_field_slot(control, 1, "col.f.bm"), bitmap)
            .unwrap();
        self.builder
            .build_store(self.column_field_slot(control, 2, "col.f.len"), len)
            .unwrap();
        self.builder
            .build_store(self.column_field_slot(control, 3, "col.f.cap"), capacity)
            .unwrap();
        Ok(control)
    }

    // ── Constructors ────────────────────────────────────────────

    /// `Column.new()` / `Column.with_capacity(cap)` / `Column.from_vec(values)`.
    /// The element type comes from the destination binding's annotation
    /// via `pending_let_column_info` (the `Tensor.zeros` /
    /// `Vec.with_capacity` expected-type mechanism; the typechecker
    /// enforces the annotation upstream, so a missing pending here is a
    /// codegen-order bug, not a user error).
    pub(super) fn compile_column_new(
        &mut self,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let info = self.pending_let_column_info.ok_or_else(|| {
            format!(
                "Column.{}: element type unknown — requires a \
                 `let c: Column[T] = ...` annotation",
                method
            )
        })?;
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();

        match method {
            "new" => Ok(self.column_alloc(info.elem, zero, zero)?.into()),
            "with_capacity" => {
                let cap = match args.first() {
                    Some(a) => {
                        let v = self.compile_expr(&a.value)?;
                        self.coerce_to_i64(v)?
                    }
                    None => zero,
                };
                Ok(self.column_alloc(info.elem, cap, zero)?.into())
            }
            "from_vec" => self.compile_column_from_vec(info.elem, args),
            "from_iter_nullable" => self.compile_column_from_iter_nullable(info.elem, args),
            other => Err(format!("unknown Column constructor '{}'", other)),
        }
    }

    /// `Column.from_vec(values: Vec[T])` — every slot valid (no nulls).
    /// The column deep-copies the values into its own buffer (it owns an
    /// independent allocation; the source Vec is untouched, and a
    /// temporary source is freed here). The validity bitmap is all-ones.
    fn compile_column_from_vec(
        &mut self,
        elem: BasicTypeEnum<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let arg_expr = args
            .first()
            .map(|a| &a.value)
            .ok_or_else(|| "Column.from_vec: missing values argument".to_string())?;
        let arg_val = self.compile_expr(arg_expr)?;

        // Bare array-literal bindings (`let v = [1, 2, 3];`) compile to an
        // `[N x T]` aggregate even though the typechecker types them
        // `Vec[T]` — read the elements straight out (the Tensor `.zeros`
        // dims path handles the same shape).
        if let BasicValueEnum::ArrayValue(av) = arg_val {
            let n = av.get_type().len() as u64;
            let n_val = i64_t.const_int(n, false);
            let control = self.column_alloc(elem, n_val, n_val)?;
            let data = self
                .column_load_field(control, 0, "col.fv.data")
                .into_pointer_value();
            for i in 0..n {
                let e = self
                    .builder
                    .build_extract_value(av, i as u32, &format!("col.fv.e{i}"))
                    .unwrap();
                let e = self.coerce_scalar_to_type(e, elem);
                let slot = unsafe {
                    self.builder
                        .build_gep(
                            elem,
                            data,
                            &[i64_t.const_int(i, false)],
                            &format!("col.fv.p{i}"),
                        )
                        .unwrap()
                };
                self.builder.build_store(slot, e).unwrap();
            }
            // All-valid bitmap: memset ceil(n/8) bytes to 0xFF.
            let bitmap = self
                .column_load_field(control, 1, "col.fv.bm")
                .into_pointer_value();
            let bm_bytes = i64_t.const_int(n.div_ceil(8), false);
            self.builder
                .build_memset(bitmap, 1, i8_t.const_int(0xFF, false), bm_bytes)
                .map_err(|e| format!("Column.from_vec bitmap memset failed: {:?}", e))?;
            return Ok(control.into());
        }

        // General path: a `Vec[T]` runtime value `{ptr, len, cap}`.
        let vec_val = arg_val.into_struct_value();
        let src_data = self
            .builder
            .build_extract_value(vec_val, 0, "col.fv.src.data")
            .unwrap()
            .into_pointer_value();
        let vlen = self
            .builder
            .build_extract_value(vec_val, 1, "col.fv.src.len")
            .unwrap()
            .into_int_value();
        let control = self.column_alloc(elem, vlen, vlen)?;
        let data = self
            .column_load_field(control, 0, "col.fv.data")
            .into_pointer_value();

        // `Column[String]`: a `memcpy` would share each source String's heap
        // ptr (→ double-free), so deep-clone every element into the column
        // (the column owns independent String heaps). Only an identifier
        // source is supported here — its own scope drop frees the originals;
        // a fresh-temp `Vec[String]` source needs ownership-transfer plumbing
        // (a follow-on slice), so it errors loudly rather than leak.
        if self.column_elem_is_string(elem) {
            if !matches!(arg_expr.kind, ExprKind::Identifier(_)) {
                return Err(
                    "Column.from_vec(<temporary Vec[String]>) is not yet supported by the \
                     native backend; bind the Vec to a `let` first (a follow-on codegen slice \
                     adds the ownership transfer)"
                        .to_string(),
                );
            }
            let str_st = self.vec_struct_type();
            let clone_fn = self.emit_string_clone_fn();
            let fn_val = self
                .current_fn
                .ok_or_else(|| "Column.from_vec outside function".to_string())?;
            let i_slot = self.builder.build_alloca(i64_t, "col.fv.s.i").unwrap();
            self.builder
                .build_store(i_slot, i64_t.const_zero())
                .unwrap();
            let head = self.context.append_basic_block(fn_val, "col.fv.s.head");
            let body = self.context.append_basic_block(fn_val, "col.fv.s.body");
            let exit = self.context.append_basic_block(fn_val, "col.fv.s.exit");
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(head);
            let i = self
                .builder
                .build_load(i64_t, i_slot, "col.fv.s.iv")
                .unwrap()
                .into_int_value();
            let more = self
                .builder
                .build_int_compare(IntPredicate::ULT, i, vlen, "col.fv.s.more")
                .unwrap();
            self.builder
                .build_conditional_branch(more, body, exit)
                .unwrap();
            self.builder.position_at_end(body);
            let s_slot = unsafe {
                self.builder
                    .build_gep(str_st, src_data, &[i], "col.fv.s.src")
                    .unwrap()
            };
            let d_slot = unsafe {
                self.builder
                    .build_gep(str_st, data, &[i], "col.fv.s.dst")
                    .unwrap()
            };
            self.builder
                .build_call(clone_fn, &[s_slot.into(), d_slot.into()], "")
                .unwrap();
            self.builder
                .build_store(
                    i_slot,
                    self.builder
                        .build_int_add(i, i64_t.const_int(1, false), "col.fv.s.next")
                        .unwrap(),
                )
                .unwrap();
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(exit);
            let bitmap = self
                .column_load_field(control, 1, "col.fv.s.bm")
                .into_pointer_value();
            let bm_bytes = self.column_bitmap_bytes(vlen);
            self.builder
                .build_memset(bitmap, 1, i8_t.const_int(0xFF, false), bm_bytes)
                .map_err(|e| format!("Column.from_vec[String] bitmap memset failed: {:?}", e))?;
            // Identifier source is borrowed (its own cleanup frees it).
            return Ok(control.into());
        }

        let elem_size = i64_t.const_int(self.column_elem_size(elem)?, false);
        let copy_bytes = self
            .builder
            .build_int_mul(vlen, elem_size, "col.fv.bytes")
            .unwrap();
        self.builder
            .build_memcpy(data, 8, src_data, 8, copy_bytes)
            .map_err(|e| format!("Column.from_vec memcpy failed: {:?}", e))?;
        // All-valid bitmap: ceil(vlen/8) bytes to 0xFF.
        let bitmap = self
            .column_load_field(control, 1, "col.fv.bm")
            .into_pointer_value();
        let bm_bytes = self
            .builder
            .build_right_shift(
                self.builder
                    .build_int_add(vlen, i64_t.const_int(7, false), "col.fv.add7")
                    .unwrap(),
                i64_t.const_int(3, false),
                false,
                "col.fv.bmbytes",
            )
            .unwrap();
        self.builder
            .build_memset(bitmap, 1, i8_t.const_int(0xFF, false), bm_bytes)
            .map_err(|e| format!("Column.from_vec bitmap memset failed: {:?}", e))?;

        // Free a temporary source Vec's buffer (nothing else owns it).
        // An identifier source keeps its own scope cleanup.
        if !matches!(arg_expr.kind, ExprKind::Identifier(_)) {
            let cap = self
                .builder
                .build_extract_value(vec_val, 2, "col.fv.src.cap")
                .unwrap()
                .into_int_value();
            self.emit_free_if_cap_positive(src_data, cap);
        }
        Ok(control.into())
    }

    /// `Column.from_iter_nullable(values: Vec[Option[T]]) -> Column[T]` —
    /// `Some(v)` becomes a valid slot, `None` a SQL null. Reads each
    /// `Option[T]` out of the source Vec buffer (the canonical 4-word
    /// enum struct), decodes the `Some` payload word back to `T`, and
    /// scatters value + runtime validity bit into a fresh column.
    fn compile_column_from_iter_nullable(
        &mut self,
        elem: BasicTypeEnum<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if self.column_elem_is_string(elem) {
            return Err(
                "Column.from_iter_nullable for String is not yet supported by the native \
                 backend (`karac build`); it works under `karac run` and lands in a follow-on \
                 codegen slice. Use `Column.from_vec(<Vec[String] binding>)` for now."
                    .to_string(),
            );
        }
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.from_iter_nullable outside function".to_string())?;
        let arg_expr = args
            .first()
            .map(|a| &a.value)
            .ok_or_else(|| "Column.from_iter_nullable: missing values argument".to_string())?;
        let arg_val = self.compile_expr(arg_expr)?;
        let vec_val = arg_val.into_struct_value();
        let src_ptr = self
            .builder
            .build_extract_value(vec_val, 0, "col.fin.src")
            .unwrap()
            .into_pointer_value();
        let vlen = self
            .builder
            .build_extract_value(vec_val, 1, "col.fin.len")
            .unwrap()
            .into_int_value();

        let control = self.column_alloc(elem, vlen, vlen)?;
        let data = self
            .column_load_field(control, 0, "col.fin.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.fin.bm")
            .into_pointer_value();

        let option_ty = self
            .enum_layouts
            .get("Option")
            .map(|l| l.llvm_type)
            .ok_or_else(|| "Column.from_iter_nullable: Option enum layout missing".to_string())?;
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);

        // for i in 0..vlen { let o = src[i]; data[i] = decode(o.w0);
        //                    bit[i] = (o.tag == Some) }
        let idx_slot = self.builder.build_alloca(i64_t, "col.fin.i").unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "col.fin.head");
        let body = self.context.append_basic_block(fn_val, "col.fin.body");
        let exit = self.context.append_basic_block(fn_val, "col.fin.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "col.fin.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, vlen, "col.fin.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let opt_ptr = unsafe {
            self.builder
                .build_gep(option_ty, src_ptr, &[i], "col.fin.optp")
                .unwrap()
        };
        let tag_ptr = self
            .builder
            .build_struct_gep(option_ty, opt_ptr, 0, "col.fin.tagp")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "col.fin.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "col.fin.some",
            )
            .unwrap();
        let w0_ptr = self
            .builder
            .build_struct_gep(option_ty, opt_ptr, 1, "col.fin.w0p")
            .unwrap();
        let w0 = self
            .builder
            .build_load(i64_t, w0_ptr, "col.fin.w0")
            .unwrap()
            .into_int_value();
        let val = self.column_word_to_value(w0, elem);
        // A null slot stores the zero placeholder (the payload word is 0
        // for `None`, so the decode already yields zero — but select keeps
        // it explicit and matches `push_null`).
        let zero = self.column_zero_elem(elem);
        let stored = self
            .builder
            .build_select(is_some, val, zero, "col.fin.sel")
            .unwrap();
        let dslot = unsafe {
            self.builder
                .build_gep(elem, data, &[i], "col.fin.dslot")
                .unwrap()
        };
        self.builder.build_store(dslot, stored).unwrap();
        self.column_write_bit_runtime(bitmap, i, is_some);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.fin.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);

        // Free a temporary source Vec's buffer (POD Option elements — no
        // per-element heap to walk).
        if !matches!(arg_expr.kind, ExprKind::Identifier(_)) {
            let cap = self
                .builder
                .build_extract_value(vec_val, 2, "col.fin.cap")
                .unwrap()
                .into_int_value();
            self.emit_free_if_cap_positive(src_ptr, cap);
        }
        Ok(control.into())
    }

    // ── Instance methods ────────────────────────────────────────

    /// Column instance methods on an identifier receiver (`push` /
    /// `push_null` / `len` / `null_count` / `valid_count` / `is_null`).
    /// Returns `Ok(None)` for a non-column receiver / unhandled method
    /// (caller falls through to normal dispatch). Identifier-only:
    /// `push` / `push_null` need an lvalue (`mut ref self`), and the
    /// receiver's element type comes from `column_var_infos` (name-keyed,
    /// span-collision-immune — like the Tensor reduce intercepts).
    pub(super) fn try_compile_column_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        _span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let ExprKind::Identifier(name) = &object.kind else {
            return Ok(None);
        };
        let Some(info) = self.column_var_infos.get(name.as_str()).copied() else {
            return Ok(None);
        };
        if !matches!(
            method,
            "push"
                | "push_null"
                | "len"
                | "null_count"
                | "valid_count"
                | "is_null"
                | "fillna"
                | "dropna"
                | "iter"
                | "iter_valid"
                | "sum"
                | "mean"
                | "min"
                | "max"
                | "var"
                | "std"
                | "corr"
                | "median"
                | "quantile"
        ) {
            return Ok(None);
        }
        // `Column[String]` (heap element) supports the read / introspection
        // surface (`len` / `null_count` / `valid_count` / `is_null` /
        // `iter_valid`) in this slice; the heap-mutating / Option-payload
        // methods need per-slot clone/move plumbing that lands in a follow-on
        // — fail loudly rather than run the POD path on String structs.
        if self.column_elem_is_string(info.elem)
            && matches!(method, "push" | "push_null" | "iter" | "fillna" | "dropna")
        {
            return Err(format!(
                "Column[String].{method} is not yet supported by the native backend \
                 (`karac build`); it works under `karac run` and lands in a follow-on \
                 codegen slice. Supported on String in build today: from_vec, len, \
                 null_count, valid_count, is_null, iter_valid."
            ));
        }
        let control = self.column_ptr_for_var(name)?;
        let i64_t = self.context.i64_type();

        match method {
            "push" | "push_null" => {
                let value = if method == "push" {
                    let arg = args
                        .first()
                        .ok_or_else(|| "Column.push requires an argument".to_string())?;
                    Some(self.compile_expr(&arg.value)?)
                } else {
                    None
                };
                self.compile_column_push(control, info.elem, value)?;
                Ok(Some(i64_t.const_zero().into()))
            }
            "len" => Ok(Some(self.column_load_field(control, 2, "col.len"))),
            "null_count" => Ok(Some(self.compile_column_count(control, false)?)),
            "valid_count" => Ok(Some(self.compile_column_count(control, true)?)),
            "is_null" => {
                let arg = args
                    .first()
                    .ok_or_else(|| "Column.is_null requires an index argument".to_string())?;
                let i_raw = self.compile_expr(&arg.value)?;
                let i = self.coerce_to_i64(i_raw)?;
                let len = self
                    .column_load_field(control, 2, "col.isnull.len")
                    .into_int_value();
                let in_bounds = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, i, len, "col.isnull.ib")
                    .unwrap();
                self.emit_column_guard(in_bounds, "Column.is_null index out of bounds")?;
                let bitmap = self
                    .column_load_field(control, 1, "col.isnull.bm")
                    .into_pointer_value();
                let valid = self.column_load_valid_bit(bitmap, i);
                // is_null == !valid
                let is_null = self.builder.build_not(valid, "col.isnull").unwrap();
                Ok(Some(is_null.into()))
            }
            "fillna" => {
                // `value` is the leading positional arg; `treat_nan_as_null`
                // is the labeled / second positional arg (default `false`).
                let value_arg = args
                    .iter()
                    .find(|a| a.label.as_deref() == Some("value"))
                    .or_else(|| args.iter().find(|a| a.label.is_none()))
                    .ok_or_else(|| "Column.fillna requires a value argument".to_string())?;
                let fill = self.compile_expr(&value_arg.value)?;
                let fill = self.coerce_scalar_to_type(fill, info.elem);
                let treat_nan = match args
                    .iter()
                    .find(|a| a.label.as_deref() == Some("treat_nan_as_null"))
                    .or_else(|| args.iter().filter(|a| a.label.is_none()).nth(1))
                {
                    Some(flag) => {
                        let v = self.compile_expr(&flag.value)?;
                        v.into_int_value()
                    }
                    None => self.context.bool_type().const_zero(),
                };
                Ok(Some(self.compile_column_fillna(
                    control, info.elem, fill, treat_nan,
                )?))
            }
            "dropna" => Ok(Some(self.compile_column_dropna(control, info.elem)?)),
            "iter" => Ok(Some(self.compile_column_iter(control, info.elem)?)),
            "iter_valid" => Ok(Some(self.compile_column_iter_valid(control, info.elem)?)),
            // Statistical reductions over the valid slots (nulls skipped).
            // `sum`/`min`/`max` -> T; `mean`/`var`/`std`/`corr` -> f64.
            // (`median`/`quantile` are interpreter-only pending an in-IR
            // sort — a follow-on slice.)
            "sum" => Ok(Some(self.compile_column_sum(
                control,
                info.elem,
                info.elem_unsigned,
            )?)),
            "min" => Ok(Some(self.compile_column_minmax(
                control,
                info.elem,
                info.elem_unsigned,
                true,
            )?)),
            "max" => Ok(Some(self.compile_column_minmax(
                control,
                info.elem,
                info.elem_unsigned,
                false,
            )?)),
            "mean" => Ok(Some(self.compile_column_mean(
                control,
                info.elem,
                info.elem_unsigned,
            )?)),
            "var" => Ok(Some(self.compile_column_var(
                control,
                info.elem,
                info.elem_unsigned,
                false,
            )?)),
            "std" => Ok(Some(self.compile_column_var(
                control,
                info.elem,
                info.elem_unsigned,
                true,
            )?)),
            "corr" => {
                let arg = args
                    .first()
                    .ok_or_else(|| "Column.corr requires a Column argument".to_string())?;
                Ok(Some(self.compile_column_corr(control, &arg.value)?))
            }
            // `median()` ≡ `quantile(0.5)` under linear interpolation (the
            // mean of the two middle values for an even count, the middle
            // for odd) — both share one sorted-buffer path.
            "median" => {
                let half = self.context.f64_type().const_float(0.5);
                Ok(Some(self.compile_column_quantile_value(
                    control,
                    info.elem,
                    info.elem_unsigned,
                    half,
                    "median",
                )?))
            }
            "quantile" => {
                let arg = args
                    .first()
                    .ok_or_else(|| "Column.quantile requires a q argument".to_string())?;
                let q = self.compile_expr(&arg.value)?;
                let q = self
                    .coerce_scalar_to_type(q, self.context.f64_type().into())
                    .into_float_value();
                // q ∈ [0, 1] (runtime-checked, matching the interpreter).
                let ge0 = self
                    .builder
                    .build_float_compare(
                        FloatPredicate::OGE,
                        q,
                        self.context.f64_type().const_zero(),
                        "col.q.ge0",
                    )
                    .unwrap();
                let le1 = self
                    .builder
                    .build_float_compare(
                        FloatPredicate::OLE,
                        q,
                        self.context.f64_type().const_float(1.0),
                        "col.q.le1",
                    )
                    .unwrap();
                let ok = self.builder.build_and(ge0, le1, "col.q.ok").unwrap();
                self.emit_column_guard(ok, "Column.quantile q must be in [0, 1]")?;
                Ok(Some(self.compile_column_quantile_value(
                    control,
                    info.elem,
                    info.elem_unsigned,
                    q,
                    "quantile",
                )?))
            }
            _ => unreachable!(),
        }
    }

    /// `iter() -> Vec[Option[T]]` — every slot as an `Option[T]` in order
    /// (Some for a valid slot, None for a null). Builds a fresh `Vec`
    /// whose element is the canonical 4-word `Option` enum struct; each
    /// slot stores `{ tag = valid?Some:None, w0 = valid?word:0, 0, 0 }`.
    fn compile_column_iter(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.iter outside function".to_string())?;
        let len = self
            .column_load_field(control, 2, "col.iter.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.iter.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.iter.bm")
            .into_pointer_value();
        let option_ty = self
            .enum_layouts
            .get("Option")
            .map(|l| l.llvm_type)
            .ok_or_else(|| "Column.iter: Option enum layout missing".to_string())?;
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        let opt_size = option_ty.size_of().unwrap();
        let bytes = self
            .builder
            .build_int_mul(len, opt_size, "col.iter.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[bytes.into()], "col.iter.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let idx_slot = self.builder.build_alloca(i64_t, "col.iter.i").unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "col.iter.head");
        let body = self.context.append_basic_block(fn_val, "col.iter.body");
        let exit = self.context.append_basic_block(fn_val, "col.iter.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "col.iter.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.iter.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, i);
        let src_slot = unsafe {
            self.builder
                .build_gep(elem, data, &[i], "col.iter.sslot")
                .unwrap()
        };
        let loaded = self
            .builder
            .build_load(elem, src_slot, "col.iter.sval")
            .unwrap();
        let word = self.column_value_to_word(loaded);
        let tag = self
            .builder
            .build_select(
                valid,
                i64_t.const_int(some_tag, false),
                i64_t.const_zero(),
                "col.iter.tag",
            )
            .unwrap()
            .into_int_value();
        let w0 = self
            .builder
            .build_select(valid, word, i64_t.const_zero(), "col.iter.w0")
            .unwrap()
            .into_int_value();
        let opt_slot = unsafe {
            self.builder
                .build_gep(option_ty, buf, &[i], "col.iter.optp")
                .unwrap()
        };
        // Store tag + w0; zero the remaining payload words for a sound `==`.
        self.builder
            .build_store(
                self.builder
                    .build_struct_gep(option_ty, opt_slot, 0, "col.iter.tagp")
                    .unwrap(),
                tag,
            )
            .unwrap();
        let n_fields = option_ty.count_fields();
        for f in 1..n_fields {
            let v = if f == 1 { w0 } else { i64_t.const_zero() };
            self.builder
                .build_store(
                    self.builder
                        .build_struct_gep(option_ty, opt_slot, f, "col.iter.wp")
                        .unwrap(),
                    v,
                )
                .unwrap();
        }
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.iter.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        Ok(self.build_vec_value(buf, len, len))
    }

    /// `iter_valid() -> Vec[T]` — the valid slots only, unwrapped, in
    /// order (nulls skipped). Builds a fresh `Vec[T]` sized to the valid
    /// count.
    fn compile_column_iter_valid(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.iter_valid outside function".to_string())?;
        let len = self
            .column_load_field(control, 2, "col.iv.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.iv.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.iv.bm")
            .into_pointer_value();
        let vc = self.compile_column_count(control, true)?.into_int_value();
        let elem_size = i64_t.const_int(self.column_elem_size(elem)?, false);
        let bytes = self
            .builder
            .build_int_mul(vc, elem_size, "col.iv.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[bytes.into()], "col.iv.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let idx_slot = self.builder.build_alloca(i64_t, "col.iv.i").unwrap();
        let j_slot = self.builder.build_alloca(i64_t, "col.iv.j").unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(j_slot, i64_t.const_zero())
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "col.iv.head");
        let body = self.context.append_basic_block(fn_val, "col.iv.body");
        let keep = self.context.append_basic_block(fn_val, "col.iv.keep");
        let cont = self.context.append_basic_block(fn_val, "col.iv.cont");
        let exit = self.context.append_basic_block(fn_val, "col.iv.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "col.iv.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.iv.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, i);
        self.builder
            .build_conditional_branch(valid, keep, cont)
            .unwrap();
        self.builder.position_at_end(keep);
        let j = self
            .builder
            .build_load(i64_t, j_slot, "col.iv.jv")
            .unwrap()
            .into_int_value();
        let src_slot = unsafe {
            self.builder
                .build_gep(elem, data, &[i], "col.iv.sslot")
                .unwrap()
        };
        let dst_slot = unsafe {
            self.builder
                .build_gep(elem, buf, &[j], "col.iv.dslot")
                .unwrap()
        };
        if self.column_elem_is_string(elem) {
            // Deep-clone each valid String into the result Vec, which then
            // owns independent heaps (its own `Vec[String]` drop frees them).
            let clone_fn = self.emit_string_clone_fn();
            self.builder
                .build_call(clone_fn, &[src_slot.into(), dst_slot.into()], "")
                .unwrap();
        } else {
            let loaded = self
                .builder
                .build_load(elem, src_slot, "col.iv.sval")
                .unwrap();
            self.builder.build_store(dst_slot, loaded).unwrap();
        }
        let j2 = self
            .builder
            .build_int_add(j, i64_t.const_int(1, false), "col.iv.j2")
            .unwrap();
        self.builder.build_store(j_slot, j2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.iv.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        Ok(self.build_vec_value(buf, vc, vc))
    }

    /// `fillna(value, treat_nan_as_null = false) -> Column[T]` — a fresh
    /// all-valid column the same length as the receiver: valid slots copied
    /// as-is, null slots replaced with `value`. When `treat_nan` (an i1) is
    /// set and the element is a float, a bitmap-valid NaN slot is also
    /// replaced — the opt-in NaN→null normalization (design.md § Data
    /// types); the flag is inert for non-float elements. The receiver is
    /// borrowed (unchanged); the result owns independent allocations.
    fn compile_column_fillna(
        &mut self,
        src: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        fill: BasicValueEnum<'ctx>,
        treat_nan: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.fillna outside function".to_string())?;
        let len = self
            .column_load_field(src, 2, "col.fill.len")
            .into_int_value();
        let src_data = self
            .column_load_field(src, 0, "col.fill.sdata")
            .into_pointer_value();
        let src_bm = self
            .column_load_field(src, 1, "col.fill.sbm")
            .into_pointer_value();

        let dst = self.column_alloc(elem, len, len)?;
        let dst_data = self
            .column_load_field(dst, 0, "col.fill.ddata")
            .into_pointer_value();
        let dst_bm = self
            .column_load_field(dst, 1, "col.fill.dbm")
            .into_pointer_value();
        // All-valid result bitmap.
        let bm_bytes = self.column_bitmap_bytes(len);
        self.builder
            .build_memset(dst_bm, 1, i8_t.const_int(0xFF, false), bm_bytes)
            .map_err(|e| format!("Column.fillna bitmap memset failed: {:?}", e))?;

        // for i in 0..len { dst[i] = valid(i) ? src[i] : fill }
        let idx_slot = self.builder.build_alloca(i64_t, "col.fill.i").unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "col.fill.head");
        let body = self.context.append_basic_block(fn_val, "col.fill.body");
        let exit = self.context.append_basic_block(fn_val, "col.fill.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "col.fill.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.fill.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(src_bm, i);
        let src_slot = unsafe {
            self.builder
                .build_gep(elem, src_data, &[i], "col.fill.sslot")
                .unwrap()
        };
        let loaded = self
            .builder
            .build_load(elem, src_slot, "col.fill.sval")
            .unwrap();
        // keep = valid AND NOT(treat_nan AND isnan(loaded)). The NaN arm is
        // only meaningful for float elements; for any other element type the
        // flag is inert and `keep` collapses to the bitmap-valid bit.
        let keep = if let BasicTypeEnum::FloatType(_) = elem {
            let fv = loaded.into_float_value();
            // `fcmp uno x, x` is true iff x is NaN (unordered self-compare).
            let is_nan = self
                .builder
                .build_float_compare(FloatPredicate::UNO, fv, fv, "col.fill.isnan")
                .unwrap();
            let drop_nan = self
                .builder
                .build_and(treat_nan, is_nan, "col.fill.dropnan")
                .unwrap();
            let not_drop = self
                .builder
                .build_not(drop_nan, "col.fill.keepnan")
                .unwrap();
            self.builder
                .build_and(valid, not_drop, "col.fill.keep")
                .unwrap()
        } else {
            valid
        };
        let chosen = self
            .builder
            .build_select(keep, loaded, fill, "col.fill.sel")
            .unwrap();
        let dst_slot = unsafe {
            self.builder
                .build_gep(elem, dst_data, &[i], "col.fill.dslot")
                .unwrap()
        };
        self.builder.build_store(dst_slot, chosen).unwrap();
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.fill.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        Ok(dst.into())
    }

    /// `dropna() -> Column[T]` — a fresh all-valid column of the
    /// receiver's valid values in order (nulls dropped). The receiver is
    /// borrowed; the result owns independent allocations.
    fn compile_column_dropna(
        &mut self,
        src: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.dropna outside function".to_string())?;
        let len = self
            .column_load_field(src, 2, "col.drop.len")
            .into_int_value();
        let src_data = self
            .column_load_field(src, 0, "col.drop.sdata")
            .into_pointer_value();
        let src_bm = self
            .column_load_field(src, 1, "col.drop.sbm")
            .into_pointer_value();
        // valid_count → result capacity/len.
        let vc = self.compile_column_count(src, true)?.into_int_value();
        let dst = self.column_alloc(elem, vc, vc)?;
        let dst_data = self
            .column_load_field(dst, 0, "col.drop.ddata")
            .into_pointer_value();
        let dst_bm = self
            .column_load_field(dst, 1, "col.drop.dbm")
            .into_pointer_value();
        let bm_bytes = self.column_bitmap_bytes(vc);
        self.builder
            .build_memset(dst_bm, 1, i8_t.const_int(0xFF, false), bm_bytes)
            .map_err(|e| format!("Column.dropna bitmap memset failed: {:?}", e))?;

        // for i in 0..len { if valid(i) { dst[j++] = src[i] } }
        let idx_slot = self.builder.build_alloca(i64_t, "col.drop.i").unwrap();
        let j_slot = self.builder.build_alloca(i64_t, "col.drop.j").unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(j_slot, i64_t.const_zero())
            .unwrap();
        let head = self.context.append_basic_block(fn_val, "col.drop.head");
        let body = self.context.append_basic_block(fn_val, "col.drop.body");
        let keep = self.context.append_basic_block(fn_val, "col.drop.keep");
        let cont = self.context.append_basic_block(fn_val, "col.drop.cont");
        let exit = self.context.append_basic_block(fn_val, "col.drop.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "col.drop.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.drop.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(src_bm, i);
        self.builder
            .build_conditional_branch(valid, keep, cont)
            .unwrap();
        self.builder.position_at_end(keep);
        let j = self
            .builder
            .build_load(i64_t, j_slot, "col.drop.jv")
            .unwrap()
            .into_int_value();
        let src_slot = unsafe {
            self.builder
                .build_gep(elem, src_data, &[i], "col.drop.sslot")
                .unwrap()
        };
        let loaded = self
            .builder
            .build_load(elem, src_slot, "col.drop.sval")
            .unwrap();
        let dst_slot = unsafe {
            self.builder
                .build_gep(elem, dst_data, &[j], "col.drop.dslot")
                .unwrap()
        };
        self.builder.build_store(dst_slot, loaded).unwrap();
        let j2 = self
            .builder
            .build_int_add(j, i64_t.const_int(1, false), "col.drop.j2")
            .unwrap();
        self.builder.build_store(j_slot, j2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.drop.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        Ok(dst.into())
    }

    /// `push(value)` (valid slot) / `push_null()` (null slot). Grows the
    /// data + bitmap buffers in place (the control pointer is stable);
    /// stores the value, sets/clears the validity bit, bumps `len`.
    fn compile_column_push(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        value: Option<BasicValueEnum<'ctx>>,
    ) -> Result<(), String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.push outside function".to_string())?;

        let len = self
            .column_load_field(control, 2, "col.push.len")
            .into_int_value();
        let cap = self
            .column_load_field(control, 3, "col.push.cap")
            .into_int_value();

        let grow_bb = self.context.append_basic_block(fn_val, "col.push.grow");
        let store_bb = self.context.append_basic_block(fn_val, "col.push.store");
        let needs_grow = self
            .builder
            .build_int_compare(IntPredicate::EQ, len, cap, "col.push.grow?")
            .unwrap();
        self.builder
            .build_conditional_branch(needs_grow, grow_bb, store_bb)
            .unwrap();

        // Grow: new_cap = max(4, cap * 2); realloc data + bitmap.
        self.builder.position_at_end(grow_bb);
        let doubled = self
            .builder
            .build_int_mul(cap, i64_t.const_int(2, false), "col.push.dbl")
            .unwrap();
        let four = i64_t.const_int(4, false);
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGT, doubled, four, "col.push.cmp")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(cmp, doubled, four, "col.push.newcap")
            .unwrap()
            .into_int_value();
        let realloc_fn = self.realloc_or_panic_fn_decl();
        // Data buffer.
        let elem_size = i64_t.const_int(self.column_elem_size(elem)?, false);
        let data = self
            .column_load_field(control, 0, "col.push.data0")
            .into_pointer_value();
        let new_data_bytes = self
            .builder
            .build_int_mul(new_cap, elem_size, "col.push.dbytes")
            .unwrap();
        let new_data = self
            .builder
            .build_call(
                realloc_fn,
                &[data.into(), new_data_bytes.into()],
                "col.push.ndata",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_store(
                self.column_field_slot(control, 0, "col.push.data.s"),
                new_data,
            )
            .unwrap();
        // Bitmap buffer: ceil(new_cap/8).
        let bitmap = self
            .column_load_field(control, 1, "col.push.bm0")
            .into_pointer_value();
        let new_bm_bytes = self
            .builder
            .build_right_shift(
                self.builder
                    .build_int_add(new_cap, i64_t.const_int(7, false), "col.push.bm.add7")
                    .unwrap(),
                i64_t.const_int(3, false),
                false,
                "col.push.bmbytes",
            )
            .unwrap();
        let new_bitmap = self
            .builder
            .build_call(
                realloc_fn,
                &[bitmap.into(), new_bm_bytes.into()],
                "col.push.nbm",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_store(
                self.column_field_slot(control, 1, "col.push.bm.s"),
                new_bitmap,
            )
            .unwrap();
        self.builder
            .build_store(
                self.column_field_slot(control, 3, "col.push.cap.s"),
                new_cap,
            )
            .unwrap();
        self.builder.build_unconditional_branch(store_bb).unwrap();

        // Store + bit-set + len bump (re-load possibly-grown buffers).
        self.builder.position_at_end(store_bb);
        let data = self
            .builder
            .build_load(
                ptr_ty,
                self.column_field_slot(control, 0, "col.push.data.l"),
                "col.push.data",
            )
            .unwrap()
            .into_pointer_value();
        let bitmap = self
            .builder
            .build_load(
                ptr_ty,
                self.column_field_slot(control, 1, "col.push.bm.l"),
                "col.push.bm",
            )
            .unwrap()
            .into_pointer_value();
        match value {
            Some(v) => {
                let v = self.coerce_scalar_to_type(v, elem);
                let slot = unsafe {
                    self.builder
                        .build_gep(elem, data, &[len], "col.push.slot")
                        .unwrap()
                };
                self.builder.build_store(slot, v).unwrap();
                self.column_write_bit(bitmap, len, true);
            }
            None => {
                // push_null: zero the data slot (deterministic placeholder)
                // and clear the validity bit.
                let zero = self.column_zero_elem(elem);
                let slot = unsafe {
                    self.builder
                        .build_gep(elem, data, &[len], "col.pushn.slot")
                        .unwrap()
                };
                self.builder.build_store(slot, zero).unwrap();
                self.column_write_bit(bitmap, len, false);
            }
        }
        let new_len = self
            .builder
            .build_int_add(len, i64_t.const_int(1, false), "col.push.newlen")
            .unwrap();
        self.builder
            .build_store(
                self.column_field_slot(control, 2, "col.push.len.s"),
                new_len,
            )
            .unwrap();
        Ok(())
    }

    /// A typed zero value for the element (the `push_null` placeholder).
    fn column_zero_elem(&self, elem: BasicTypeEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match elem {
            BasicTypeEnum::FloatType(ft) => ft.const_zero().into(),
            BasicTypeEnum::IntType(it) => it.const_zero().into(),
            other => other.const_zero(),
        }
    }

    /// `null_count()` (`valid == false`) / `valid_count()` (`valid ==
    /// true`) — one pass over `[0, len)` counting matching validity bits.
    fn compile_column_count(
        &mut self,
        control: PointerValue<'ctx>,
        count_valid: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column count outside function".to_string())?;
        let len = self
            .column_load_field(control, 2, "col.cnt.len")
            .into_int_value();
        let bitmap = self
            .column_load_field(control, 1, "col.cnt.bm")
            .into_pointer_value();

        let idx_slot = self.builder.build_alloca(i64_t, "col.cnt.i").unwrap();
        let acc_slot = self.builder.build_alloca(i64_t, "col.cnt.acc").unwrap();
        self.builder
            .build_store(idx_slot, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(acc_slot, i64_t.const_zero())
            .unwrap();

        let head_bb = self.context.append_basic_block(fn_val, "col.cnt.head");
        let body_bb = self.context.append_basic_block(fn_val, "col.cnt.body");
        let exit_bb = self.context.append_basic_block(fn_val, "col.cnt.exit");
        self.builder.build_unconditional_branch(head_bb).unwrap();

        self.builder.position_at_end(head_bb);
        let i = self
            .builder
            .build_load(i64_t, idx_slot, "col.cnt.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.cnt.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let valid = self.column_load_valid_bit(bitmap, i);
        let hit = if count_valid {
            valid
        } else {
            self.builder.build_not(valid, "col.cnt.null").unwrap()
        };
        let acc = self
            .builder
            .build_load(i64_t, acc_slot, "col.cnt.accv")
            .unwrap()
            .into_int_value();
        let hit64 = self
            .builder
            .build_int_z_extend(hit, i64_t, "col.cnt.hit64")
            .unwrap();
        let acc2 = self
            .builder
            .build_int_add(acc, hit64, "col.cnt.acc2")
            .unwrap();
        self.builder.build_store(acc_slot, acc2).unwrap();
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.cnt.next")
            .unwrap();
        self.builder.build_store(idx_slot, next).unwrap();
        self.builder.build_unconditional_branch(head_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        Ok(self
            .builder
            .build_load(i64_t, acc_slot, "col.cnt.result")
            .unwrap())
    }

    // ── Statistical reductions ──────────────────────────────────

    /// A numeric element value widened to `f64` (for the float-result
    /// statistics). Signed/unsigned ints convert per the column's element
    /// signedness; `f32` extends to `f64`; `f64` is returned as-is. Bool /
    /// non-numeric never reach here (the typechecker requires a numeric
    /// element for every stat).
    fn column_elem_to_f64(&self, val: BasicValueEnum<'ctx>, unsigned: bool) -> FloatValue<'ctx> {
        let f64_t = self.context.f64_type();
        match val {
            BasicValueEnum::FloatValue(fv) => {
                if fv.get_type() == f64_t {
                    fv
                } else {
                    self.builder
                        .build_float_ext(fv, f64_t, "col.f2f64")
                        .unwrap()
                }
            }
            BasicValueEnum::IntValue(iv) => {
                if unsigned {
                    self.builder
                        .build_unsigned_int_to_float(iv, f64_t, "col.u2f")
                        .unwrap()
                } else {
                    self.builder
                        .build_signed_int_to_float(iv, f64_t, "col.i2f")
                        .unwrap()
                }
            }
            _ => f64_t.const_zero(),
        }
    }

    /// `sqrt` of an `f64` via the overloaded `llvm.sqrt` intrinsic (the
    /// `x.sqrt()` lowering, reused for `std` / `corr`).
    fn column_sqrt_f64(&self, fv: FloatValue<'ctx>) -> FloatValue<'ctx> {
        let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.sqrt")
            .expect("llvm.sqrt intrinsic must exist");
        let decl = intrinsic
            .get_declaration(&self.module, &[fv.get_type().into()])
            .expect("llvm.sqrt declaration for f64");
        self.builder
            .build_call(decl, &[fv.into()], "col.sqrt")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_float_value()
    }

    /// `sum() -> T` — fold `+` over the valid slots (inherits the scalar
    /// overflow trap via `compile_binop_typed`); an all-null / empty column
    /// traps (parity with the interpreter / Tensor empty-reduce).
    fn compile_column_sum(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.sum outside function".to_string())?;
        let len = self
            .column_load_field(control, 2, "col.sum.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.sum.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.sum.bm")
            .into_pointer_value();
        let idx = self.builder.build_alloca(i64_t, "col.sum.i").unwrap();
        let acc = self.builder.build_alloca(elem, "col.sum.acc").unwrap();
        let cnt = self.builder.build_alloca(i64_t, "col.sum.cnt").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder
            .build_store(acc, self.column_zero_elem(elem))
            .unwrap();
        self.builder.build_store(cnt, i64_t.const_zero()).unwrap();

        let head = self.context.append_basic_block(fn_val, "col.sum.head");
        let body = self.context.append_basic_block(fn_val, "col.sum.body");
        let add = self.context.append_basic_block(fn_val, "col.sum.add");
        let cont = self.context.append_basic_block(fn_val, "col.sum.cont");
        let exit = self.context.append_basic_block(fn_val, "col.sum.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx, "col.sum.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.sum.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, i);
        self.builder
            .build_conditional_branch(valid, add, cont)
            .unwrap();
        self.builder.position_at_end(add);
        let x = self.column_gep_load(data, elem, i, "col.sum.x");
        let a = self.builder.build_load(elem, acc, "col.sum.a").unwrap();
        let a2 = self.compile_binop_typed(&BinOp::Add, a, x, unsigned)?;
        self.builder.build_store(acc, a2).unwrap();
        let c = self
            .builder
            .build_load(i64_t, cnt, "col.sum.c")
            .unwrap()
            .into_int_value();
        let c2 = self
            .builder
            .build_int_add(c, i64_t.const_int(1, false), "col.sum.c2")
            .unwrap();
        self.builder.build_store(cnt, c2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.sum.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let cnt_v = self
            .builder
            .build_load(i64_t, cnt, "col.sum.cntv")
            .unwrap()
            .into_int_value();
        let ok = self
            .builder
            .build_int_compare(IntPredicate::UGT, cnt_v, i64_t.const_zero(), "col.sum.ok")
            .unwrap();
        self.emit_column_guard(ok, "cannot compute `sum` on a column with no valid values")?;
        Ok(self
            .builder
            .build_load(elem, acc, "col.sum.result")
            .unwrap())
    }

    /// `min() -> T` / `max() -> T` — the smallest / largest valid slot,
    /// seeded by the first valid element so NaN neither displaces nor is
    /// taken (the scalar `<` / `>` posture, matching the interpreter). An
    /// all-null / empty column traps.
    fn compile_column_minmax(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        is_min: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.minmax outside function".to_string())?;
        let len = self
            .column_load_field(control, 2, "col.mm.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.mm.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.mm.bm")
            .into_pointer_value();
        let idx = self.builder.build_alloca(i64_t, "col.mm.i").unwrap();
        let acc = self.builder.build_alloca(elem, "col.mm.acc").unwrap();
        let seeded = self.builder.build_alloca(bool_t, "col.mm.seeded").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder
            .build_store(acc, self.column_zero_elem(elem))
            .unwrap();
        self.builder
            .build_store(seeded, bool_t.const_zero())
            .unwrap();

        let head = self.context.append_basic_block(fn_val, "col.mm.head");
        let body = self.context.append_basic_block(fn_val, "col.mm.body");
        let upd = self.context.append_basic_block(fn_val, "col.mm.upd");
        let cont = self.context.append_basic_block(fn_val, "col.mm.cont");
        let exit = self.context.append_basic_block(fn_val, "col.mm.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx, "col.mm.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.mm.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, i);
        self.builder
            .build_conditional_branch(valid, upd, cont)
            .unwrap();
        self.builder.position_at_end(upd);
        let x = self.column_gep_load(data, elem, i, "col.mm.x");
        let cur = self.builder.build_load(elem, acc, "col.mm.cur").unwrap();
        let s = self
            .builder
            .build_load(bool_t, seeded, "col.mm.s")
            .unwrap()
            .into_int_value();
        // Strict compare `x ⋖ cur`; if not yet seeded, always take.
        let op = if is_min { BinOp::Lt } else { BinOp::Gt };
        let cmp = self
            .compile_binop_typed(&op, x, cur, unsigned)?
            .into_int_value();
        let not_seeded = self.builder.build_not(s, "col.mm.ns").unwrap();
        let take = self
            .builder
            .build_or(not_seeded, cmp, "col.mm.take")
            .unwrap();
        let newacc = self
            .builder
            .build_select(take, x, cur, "col.mm.new")
            .unwrap();
        self.builder.build_store(acc, newacc).unwrap();
        self.builder
            .build_store(seeded, bool_t.const_int(1, false))
            .unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.mm.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let s = self
            .builder
            .build_load(bool_t, seeded, "col.mm.sf")
            .unwrap()
            .into_int_value();
        let method = if is_min { "min" } else { "max" };
        self.emit_column_guard(
            s,
            &format!("cannot compute `{method}` on a column with no valid values"),
        )?;
        Ok(self.builder.build_load(elem, acc, "col.mm.result").unwrap())
    }

    /// `mean() -> f64` — `Σ valid / count` as `f64`; empty traps. The
    /// `(sum_f64, count)` pair is also the first pass of `var` / `std`, so
    /// this is factored into `column_sum_f64_and_count`.
    fn compile_column_mean(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let f64_t = self.context.f64_type();
        let (sum, cnt) = self.column_sum_f64_and_count(control, elem, unsigned)?;
        let ok = self
            .builder
            .build_int_compare(
                IntPredicate::UGT,
                cnt,
                self.context.i64_type().const_zero(),
                "col.mean.ok",
            )
            .unwrap();
        self.emit_column_guard(ok, "cannot compute `mean` on a column with no valid values")?;
        let cntf = self
            .builder
            .build_unsigned_int_to_float(cnt, f64_t, "col.mean.cntf")
            .unwrap();
        let mean = self.builder.build_float_div(sum, cntf, "col.mean").unwrap();
        Ok(mean.into())
    }

    /// One pass over the valid slots accumulating `(Σ x as f64, count)`.
    fn column_sum_f64_and_count(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> Result<(FloatValue<'ctx>, IntValue<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column f64-sum outside function".to_string())?;
        let len = self
            .column_load_field(control, 2, "col.fsum.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.fsum.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.fsum.bm")
            .into_pointer_value();
        let idx = self.builder.build_alloca(i64_t, "col.fsum.i").unwrap();
        let acc = self.builder.build_alloca(f64_t, "col.fsum.acc").unwrap();
        let cnt = self.builder.build_alloca(i64_t, "col.fsum.cnt").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder.build_store(acc, f64_t.const_zero()).unwrap();
        self.builder.build_store(cnt, i64_t.const_zero()).unwrap();

        let head = self.context.append_basic_block(fn_val, "col.fsum.head");
        let body = self.context.append_basic_block(fn_val, "col.fsum.body");
        let add = self.context.append_basic_block(fn_val, "col.fsum.add");
        let cont = self.context.append_basic_block(fn_val, "col.fsum.cont");
        let exit = self.context.append_basic_block(fn_val, "col.fsum.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx, "col.fsum.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.fsum.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, i);
        self.builder
            .build_conditional_branch(valid, add, cont)
            .unwrap();
        self.builder.position_at_end(add);
        let x = self.column_gep_load(data, elem, i, "col.fsum.x");
        let xf = self.column_elem_to_f64(x, unsigned);
        let a = self
            .builder
            .build_load(f64_t, acc, "col.fsum.a")
            .unwrap()
            .into_float_value();
        let a2 = self.builder.build_float_add(a, xf, "col.fsum.a2").unwrap();
        self.builder.build_store(acc, a2).unwrap();
        let c = self
            .builder
            .build_load(i64_t, cnt, "col.fsum.c")
            .unwrap()
            .into_int_value();
        let c2 = self
            .builder
            .build_int_add(c, i64_t.const_int(1, false), "col.fsum.c2")
            .unwrap();
        self.builder.build_store(cnt, c2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.fsum.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        self.builder.position_at_end(exit);
        let sum = self
            .builder
            .build_load(f64_t, acc, "col.fsum.sum")
            .unwrap()
            .into_float_value();
        let count = self
            .builder
            .build_load(i64_t, cnt, "col.fsum.count")
            .unwrap()
            .into_int_value();
        Ok((sum, count))
    }

    /// `var() -> f64` / `std() -> f64` — the **sample** (Bessel `n-1`)
    /// variance / standard deviation over the valid slots. Requires ≥ 2
    /// valid values (else traps; sample variance is undefined for fewer).
    fn compile_column_var(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        is_std: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.var outside function".to_string())?;
        // Pass 1 — mean.
        let (sum, cnt) = self.column_sum_f64_and_count(control, elem, unsigned)?;
        let two = i64_t.const_int(2, false);
        let ok = self
            .builder
            .build_int_compare(IntPredicate::UGE, cnt, two, "col.var.ok")
            .unwrap();
        let method = if is_std { "std" } else { "var" };
        self.emit_column_guard(
            ok,
            &format!("`{method}` requires at least 2 valid values (sample variance is undefined for fewer)"),
        )?;
        let cntf = self
            .builder
            .build_unsigned_int_to_float(cnt, f64_t, "col.var.cntf")
            .unwrap();
        let mean = self
            .builder
            .build_float_div(sum, cntf, "col.var.mean")
            .unwrap();

        // Pass 2 — Σ (x - mean)².
        let len = self
            .column_load_field(control, 2, "col.var.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.var.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.var.bm")
            .into_pointer_value();
        let idx = self.builder.build_alloca(i64_t, "col.var.i").unwrap();
        let ss = self.builder.build_alloca(f64_t, "col.var.ss").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        self.builder.build_store(ss, f64_t.const_zero()).unwrap();
        let head = self.context.append_basic_block(fn_val, "col.var.head");
        let body = self.context.append_basic_block(fn_val, "col.var.body");
        let add = self.context.append_basic_block(fn_val, "col.var.add");
        let cont = self.context.append_basic_block(fn_val, "col.var.cont");
        let exit = self.context.append_basic_block(fn_val, "col.var.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx, "col.var.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.var.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let valid = self.column_load_valid_bit(bitmap, i);
        self.builder
            .build_conditional_branch(valid, add, cont)
            .unwrap();
        self.builder.position_at_end(add);
        let x = self.column_gep_load(data, elem, i, "col.var.x");
        let xf = self.column_elem_to_f64(x, unsigned);
        let d = self.builder.build_float_sub(xf, mean, "col.var.d").unwrap();
        let d2 = self.builder.build_float_mul(d, d, "col.var.d2").unwrap();
        let s = self
            .builder
            .build_load(f64_t, ss, "col.var.s")
            .unwrap()
            .into_float_value();
        let s2 = self.builder.build_float_add(s, d2, "col.var.s2").unwrap();
        self.builder.build_store(ss, s2).unwrap();
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.var.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        let ss_v = self
            .builder
            .build_load(f64_t, ss, "col.var.ssv")
            .unwrap()
            .into_float_value();
        let one = f64_t.const_float(1.0);
        let denom = self
            .builder
            .build_float_sub(cntf, one, "col.var.denom")
            .unwrap();
        let var = self
            .builder
            .build_float_div(ss_v, denom, "col.var.var")
            .unwrap();
        if is_std {
            Ok(self.column_sqrt_f64(var).into())
        } else {
            Ok(var.into())
        }
    }

    /// `corr(other) -> f64` — Pearson correlation over the pairwise-valid
    /// (both columns valid) slots; requires equal length (else traps) and
    /// ≥ 2 valid pairs (else traps). A zero-variance operand yields `NaN`
    /// (the pandas posture). Both columns are `f64` (typechecker-enforced).
    fn compile_column_corr(
        &mut self,
        control: PointerValue<'ctx>,
        other: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.corr outside function".to_string())?;
        let (octrl, _oelem, _ounsigned) = self.column_operand(other)?;
        let elem: BasicTypeEnum = f64_t.into(); // typechecker guarantees Column[f64] both sides

        let len = self
            .column_load_field(control, 2, "col.corr.len")
            .into_int_value();
        let olen = self
            .column_load_field(octrl, 2, "col.corr.olen")
            .into_int_value();
        let len_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, len, olen, "col.corr.leneq")
            .unwrap();
        self.emit_column_guard(len_eq, "Column.corr length mismatch")?;

        let xdata = self
            .column_load_field(control, 0, "col.corr.xdata")
            .into_pointer_value();
        let xbm = self
            .column_load_field(control, 1, "col.corr.xbm")
            .into_pointer_value();
        let ydata = self
            .column_load_field(octrl, 0, "col.corr.ydata")
            .into_pointer_value();
        let ybm = self
            .column_load_field(octrl, 1, "col.corr.ybm")
            .into_pointer_value();

        // Both-valid bit for slot `i` (emitted in the current block).
        // Pass 1 — pairwise sums sx / sy and the paired count.
        let sx = self.builder.build_alloca(f64_t, "col.corr.sx").unwrap();
        let sy = self.builder.build_alloca(f64_t, "col.corr.sy").unwrap();
        let cnt = self.builder.build_alloca(i64_t, "col.corr.cnt").unwrap();
        let idx1 = self.builder.build_alloca(i64_t, "col.corr.i1").unwrap();
        for slot in [sx, sy] {
            self.builder.build_store(slot, f64_t.const_zero()).unwrap();
        }
        self.builder.build_store(cnt, i64_t.const_zero()).unwrap();
        self.builder.build_store(idx1, i64_t.const_zero()).unwrap();

        let h1 = self.context.append_basic_block(fn_val, "col.corr1.head");
        let b1 = self.context.append_basic_block(fn_val, "col.corr1.body");
        let a1 = self.context.append_basic_block(fn_val, "col.corr1.add");
        let c1 = self.context.append_basic_block(fn_val, "col.corr1.cont");
        let e1 = self.context.append_basic_block(fn_val, "col.corr1.exit");
        self.builder.build_unconditional_branch(h1).unwrap();
        self.builder.position_at_end(h1);
        let i = self
            .builder
            .build_load(i64_t, idx1, "col.corr1.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.corr1.more")
            .unwrap();
        self.builder.build_conditional_branch(more, b1, e1).unwrap();
        self.builder.position_at_end(b1);
        let vx = self.column_load_valid_bit(xbm, i);
        let vy = self.column_load_valid_bit(ybm, i);
        let both = self.builder.build_and(vx, vy, "col.corr1.both").unwrap();
        self.builder.build_conditional_branch(both, a1, c1).unwrap();
        self.builder.position_at_end(a1);
        let x = self
            .column_gep_load(xdata, elem, i, "col.corr1.x")
            .into_float_value();
        let y = self
            .column_gep_load(ydata, elem, i, "col.corr1.y")
            .into_float_value();
        let sxv = self
            .builder
            .build_load(f64_t, sx, "col.corr1.sxv")
            .unwrap()
            .into_float_value();
        self.builder
            .build_store(
                sx,
                self.builder
                    .build_float_add(sxv, x, "col.corr1.sx2")
                    .unwrap(),
            )
            .unwrap();
        let syv = self
            .builder
            .build_load(f64_t, sy, "col.corr1.syv")
            .unwrap()
            .into_float_value();
        self.builder
            .build_store(
                sy,
                self.builder
                    .build_float_add(syv, y, "col.corr1.sy2")
                    .unwrap(),
            )
            .unwrap();
        let cv = self
            .builder
            .build_load(i64_t, cnt, "col.corr1.cv")
            .unwrap()
            .into_int_value();
        self.builder
            .build_store(
                cnt,
                self.builder
                    .build_int_add(cv, i64_t.const_int(1, false), "col.corr1.cv2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(c1).unwrap();
        self.builder.position_at_end(c1);
        self.builder
            .build_store(
                idx1,
                self.builder
                    .build_int_add(i, i64_t.const_int(1, false), "col.corr1.next")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(h1).unwrap();
        self.builder.position_at_end(e1);

        let cnt_v = self
            .builder
            .build_load(i64_t, cnt, "col.corr.cntv")
            .unwrap()
            .into_int_value();
        let two = i64_t.const_int(2, false);
        let ok = self
            .builder
            .build_int_compare(IntPredicate::UGE, cnt_v, two, "col.corr.ok")
            .unwrap();
        self.emit_column_guard(ok, "Column.corr requires at least 2 valid paired values")?;
        let cntf = self
            .builder
            .build_unsigned_int_to_float(cnt_v, f64_t, "col.corr.cntf")
            .unwrap();
        let sxv = self
            .builder
            .build_load(f64_t, sx, "col.corr.sxf")
            .unwrap()
            .into_float_value();
        let syv = self
            .builder
            .build_load(f64_t, sy, "col.corr.syf")
            .unwrap()
            .into_float_value();
        let mx = self
            .builder
            .build_float_div(sxv, cntf, "col.corr.mx")
            .unwrap();
        let my = self
            .builder
            .build_float_div(syv, cntf, "col.corr.my")
            .unwrap();

        // Pass 2 — sxy / sxx / syy about the means.
        let sxy = self.builder.build_alloca(f64_t, "col.corr.sxy").unwrap();
        let sxx = self.builder.build_alloca(f64_t, "col.corr.sxx").unwrap();
        let syy = self.builder.build_alloca(f64_t, "col.corr.syy").unwrap();
        let idx2 = self.builder.build_alloca(i64_t, "col.corr.i2").unwrap();
        for slot in [sxy, sxx, syy] {
            self.builder.build_store(slot, f64_t.const_zero()).unwrap();
        }
        self.builder.build_store(idx2, i64_t.const_zero()).unwrap();
        let h2 = self.context.append_basic_block(fn_val, "col.corr2.head");
        let b2 = self.context.append_basic_block(fn_val, "col.corr2.body");
        let a2 = self.context.append_basic_block(fn_val, "col.corr2.add");
        let c2 = self.context.append_basic_block(fn_val, "col.corr2.cont");
        let e2 = self.context.append_basic_block(fn_val, "col.corr2.exit");
        self.builder.build_unconditional_branch(h2).unwrap();
        self.builder.position_at_end(h2);
        let i = self
            .builder
            .build_load(i64_t, idx2, "col.corr2.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.corr2.more")
            .unwrap();
        self.builder.build_conditional_branch(more, b2, e2).unwrap();
        self.builder.position_at_end(b2);
        let vx = self.column_load_valid_bit(xbm, i);
        let vy = self.column_load_valid_bit(ybm, i);
        let both = self.builder.build_and(vx, vy, "col.corr2.both").unwrap();
        self.builder.build_conditional_branch(both, a2, c2).unwrap();
        self.builder.position_at_end(a2);
        let x = self
            .column_gep_load(xdata, elem, i, "col.corr2.x")
            .into_float_value();
        let y = self
            .column_gep_load(ydata, elem, i, "col.corr2.y")
            .into_float_value();
        let dx = self.builder.build_float_sub(x, mx, "col.corr2.dx").unwrap();
        let dy = self.builder.build_float_sub(y, my, "col.corr2.dy").unwrap();
        let upd = |me: &Self, slot: PointerValue<'ctx>, inc: FloatValue<'ctx>, n: &str| {
            let cur = me
                .builder
                .build_load(f64_t, slot, n)
                .unwrap()
                .into_float_value();
            me.builder
                .build_store(slot, me.builder.build_float_add(cur, inc, n).unwrap())
                .unwrap();
        };
        upd(
            self,
            sxy,
            self.builder
                .build_float_mul(dx, dy, "col.corr2.dxy")
                .unwrap(),
            "col.corr2.sxy",
        );
        upd(
            self,
            sxx,
            self.builder
                .build_float_mul(dx, dx, "col.corr2.dxx")
                .unwrap(),
            "col.corr2.sxx",
        );
        upd(
            self,
            syy,
            self.builder
                .build_float_mul(dy, dy, "col.corr2.dyy")
                .unwrap(),
            "col.corr2.syy",
        );
        self.builder.build_unconditional_branch(c2).unwrap();
        self.builder.position_at_end(c2);
        self.builder
            .build_store(
                idx2,
                self.builder
                    .build_int_add(i, i64_t.const_int(1, false), "col.corr2.next")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(h2).unwrap();
        self.builder.position_at_end(e2);

        let sxx_v = self
            .builder
            .build_load(f64_t, sxx, "col.corr.sxxv")
            .unwrap()
            .into_float_value();
        let syy_v = self
            .builder
            .build_load(f64_t, syy, "col.corr.syyv")
            .unwrap()
            .into_float_value();
        let sxy_v = self
            .builder
            .build_load(f64_t, sxy, "col.corr.sxyv")
            .unwrap()
            .into_float_value();
        let prod = self
            .builder
            .build_float_mul(sxx_v, syy_v, "col.corr.prod")
            .unwrap();
        let denom = self.column_sqrt_f64(prod);
        let is_zero = self
            .builder
            .build_float_compare(
                FloatPredicate::OEQ,
                denom,
                f64_t.const_zero(),
                "col.corr.dz",
            )
            .unwrap();
        let r = self
            .builder
            .build_float_div(sxy_v, denom, "col.corr.r")
            .unwrap();
        let nan = f64_t.const_float(f64::NAN);
        let result = self
            .builder
            .build_select(is_zero, nan, r, "col.corr.result")
            .unwrap();
        // A fresh-temp `other` operand (e.g. `a.corr(b + c)`) is freed.
        self.column_free_if_fresh_temp(other, octrl);
        Ok(result)
    }

    /// Collect the valid slots into a fresh malloc'd `f64` scratch buffer
    /// and sort it ascending (insertion sort) — the shared backbone of
    /// `median` / `quantile`. Traps on an empty valid set (parity with the
    /// other reductions). Returns `(buffer, count)`; the caller frees the
    /// buffer after reading. NaN ordering follows `fcmp ogt` (a NaN key
    /// never shifts a smaller element, so NaNs settle at the front — an
    /// acceptable v1 posture, since the scalar comparison world treats NaN
    /// as unordered; quantiles over NaN-bearing data are undefined anyway).
    fn column_sorted_valid_f64(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        method: &str,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column sorted-buffer outside function".to_string())?;
        let n = self.compile_column_count(control, true)?.into_int_value();
        let ok = self
            .builder
            .build_int_compare(IntPredicate::UGT, n, i64_t.const_zero(), "col.srt.ok")
            .unwrap();
        self.emit_column_guard(
            ok,
            &format!("cannot compute `{method}` on a column with no valid values"),
        )?;

        // Fresh f64 scratch buffer of `n` elements.
        let nbytes = self
            .builder
            .build_int_mul(n, i64_t.const_int(8, false), "col.srt.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "col.srt.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Fill: walk [0, len), copy each valid element (as f64) into buf[j++].
        let len = self
            .column_load_field(control, 2, "col.srt.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.srt.data")
            .into_pointer_value();
        let bm = self
            .column_load_field(control, 1, "col.srt.bm")
            .into_pointer_value();
        let i = self.builder.build_alloca(i64_t, "col.srt.i").unwrap();
        let j = self.builder.build_alloca(i64_t, "col.srt.j").unwrap();
        self.builder.build_store(i, i64_t.const_zero()).unwrap();
        self.builder.build_store(j, i64_t.const_zero()).unwrap();
        let fh = self.context.append_basic_block(fn_val, "col.fill.head");
        let fb = self.context.append_basic_block(fn_val, "col.fill.body");
        let fa = self.context.append_basic_block(fn_val, "col.fill.add");
        let fc = self.context.append_basic_block(fn_val, "col.fill.cont");
        let fe = self.context.append_basic_block(fn_val, "col.fill.exit");
        self.builder.build_unconditional_branch(fh).unwrap();
        self.builder.position_at_end(fh);
        let iv = self
            .builder
            .build_load(i64_t, i, "col.fill.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, len, "col.fill.more")
            .unwrap();
        self.builder.build_conditional_branch(more, fb, fe).unwrap();
        self.builder.position_at_end(fb);
        let valid = self.column_load_valid_bit(bm, iv);
        self.builder
            .build_conditional_branch(valid, fa, fc)
            .unwrap();
        self.builder.position_at_end(fa);
        let x = self.column_gep_load(data, elem, iv, "col.fill.x");
        let xf = self.column_elem_to_f64(x, unsigned);
        let jv = self
            .builder
            .build_load(i64_t, j, "col.fill.jv")
            .unwrap()
            .into_int_value();
        let slot = unsafe {
            self.builder
                .build_gep(f64_t, buf, &[jv], "col.fill.slot")
                .unwrap()
        };
        self.builder.build_store(slot, xf).unwrap();
        self.builder
            .build_store(
                j,
                self.builder
                    .build_int_add(jv, i64_t.const_int(1, false), "col.fill.j2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(fc).unwrap();
        self.builder.position_at_end(fc);
        self.builder
            .build_store(
                i,
                self.builder
                    .build_int_add(iv, i64_t.const_int(1, false), "col.fill.i2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(fh).unwrap();
        self.builder.position_at_end(fe);

        // Insertion sort buf[0..n] ascending.
        // for si in 1..n { key = buf[si]; sj = si-1;
        //   while sj >= 0 && buf[sj] > key { buf[sj+1] = buf[sj]; sj-- }
        //   buf[sj+1] = key }
        let si = self.builder.build_alloca(i64_t, "col.is.si").unwrap();
        let sj = self.builder.build_alloca(i64_t, "col.is.sj").unwrap();
        let key = self.builder.build_alloca(f64_t, "col.is.key").unwrap();
        self.builder
            .build_store(si, i64_t.const_int(1, false))
            .unwrap();
        let oh = self.context.append_basic_block(fn_val, "col.is.ohead");
        let ob = self.context.append_basic_block(fn_val, "col.is.obody");
        let ih = self.context.append_basic_block(fn_val, "col.is.ihead");
        let ick = self.context.append_basic_block(fn_val, "col.is.icheck");
        let ish = self.context.append_basic_block(fn_val, "col.is.ishift");
        let ipl = self.context.append_basic_block(fn_val, "col.is.iplace");
        let oc = self.context.append_basic_block(fn_val, "col.is.ocont");
        let oe = self.context.append_basic_block(fn_val, "col.is.oexit");
        self.builder.build_unconditional_branch(oh).unwrap();
        self.builder.position_at_end(oh);
        let siv = self
            .builder
            .build_load(i64_t, si, "col.is.siv")
            .unwrap()
            .into_int_value();
        let omore = self
            .builder
            .build_int_compare(IntPredicate::ULT, siv, n, "col.is.omore")
            .unwrap();
        self.builder
            .build_conditional_branch(omore, ob, oe)
            .unwrap();
        self.builder.position_at_end(ob);
        let key_slot = unsafe {
            self.builder
                .build_gep(f64_t, buf, &[siv], "col.is.keyslot")
                .unwrap()
        };
        let key_v = self
            .builder
            .build_load(f64_t, key_slot, "col.is.keyv")
            .unwrap();
        self.builder.build_store(key, key_v).unwrap();
        self.builder
            .build_store(
                sj,
                self.builder
                    .build_int_sub(siv, i64_t.const_int(1, false), "col.is.sj0")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(ih).unwrap();
        self.builder.position_at_end(ih);
        let sjv = self
            .builder
            .build_load(i64_t, sj, "col.is.sjv")
            .unwrap()
            .into_int_value();
        // Signed `sj >= 0` (short-circuits before the buf[sj] read).
        let ge0 = self
            .builder
            .build_int_compare(IntPredicate::SGE, sjv, i64_t.const_zero(), "col.is.ge0")
            .unwrap();
        self.builder
            .build_conditional_branch(ge0, ick, ipl)
            .unwrap();
        self.builder.position_at_end(ick);
        let bj_slot = unsafe {
            self.builder
                .build_gep(f64_t, buf, &[sjv], "col.is.bjslot")
                .unwrap()
        };
        let bj = self
            .builder
            .build_load(f64_t, bj_slot, "col.is.bj")
            .unwrap()
            .into_float_value();
        let key_cur = self
            .builder
            .build_load(f64_t, key, "col.is.keycur")
            .unwrap()
            .into_float_value();
        let gt = self
            .builder
            .build_float_compare(FloatPredicate::OGT, bj, key_cur, "col.is.gt")
            .unwrap();
        self.builder.build_conditional_branch(gt, ish, ipl).unwrap();
        self.builder.position_at_end(ish);
        // buf[sj+1] = buf[sj]
        let sjp1 = self
            .builder
            .build_int_add(sjv, i64_t.const_int(1, false), "col.is.sjp1")
            .unwrap();
        let dst = unsafe {
            self.builder
                .build_gep(f64_t, buf, &[sjp1], "col.is.dst")
                .unwrap()
        };
        self.builder.build_store(dst, bj).unwrap();
        self.builder
            .build_store(
                sj,
                self.builder
                    .build_int_sub(sjv, i64_t.const_int(1, false), "col.is.sjdec")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(ih).unwrap();
        self.builder.position_at_end(ipl);
        // buf[sj+1] = key (sj is the current value in the slot)
        let sjv2 = self
            .builder
            .build_load(i64_t, sj, "col.is.sjv2")
            .unwrap()
            .into_int_value();
        let placep1 = self
            .builder
            .build_int_add(sjv2, i64_t.const_int(1, false), "col.is.placep1")
            .unwrap();
        let pslot = unsafe {
            self.builder
                .build_gep(f64_t, buf, &[placep1], "col.is.pslot")
                .unwrap()
        };
        let key_final = self.builder.build_load(f64_t, key, "col.is.keyf").unwrap();
        self.builder.build_store(pslot, key_final).unwrap();
        self.builder.build_unconditional_branch(oc).unwrap();
        self.builder.position_at_end(oc);
        self.builder
            .build_store(
                si,
                self.builder
                    .build_int_add(siv, i64_t.const_int(1, false), "col.is.sinext")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(oh).unwrap();
        self.builder.position_at_end(oe);
        Ok((buf, n))
    }

    /// `median` / `quantile(q)` — the `q`-quantile of the valid slots via
    /// linear interpolation (NumPy / pandas default), freeing the sorted
    /// scratch buffer after the read. `median` passes `q = 0.5`.
    fn compile_column_quantile_value(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        q: FloatValue<'ctx>,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let (buf, n) = self.column_sorted_valid_f64(control, elem, unsigned, method)?;

        // pos = q * (n - 1); lo = floor(pos) (= trunc, pos ≥ 0); frac = pos - lo.
        let nf = self
            .builder
            .build_unsigned_int_to_float(n, f64_t, "col.q.nf")
            .unwrap();
        let nm1 = self
            .builder
            .build_float_sub(nf, f64_t.const_float(1.0), "col.q.nm1")
            .unwrap();
        let pos = self.builder.build_float_mul(q, nm1, "col.q.pos").unwrap();
        let lo = self
            .builder
            .build_float_to_unsigned_int(pos, i64_t, "col.q.lo")
            .unwrap();
        let lof = self
            .builder
            .build_unsigned_int_to_float(lo, f64_t, "col.q.lof")
            .unwrap();
        let frac = self
            .builder
            .build_float_sub(pos, lof, "col.q.frac")
            .unwrap();
        // hi = (lo + 1 < n) ? lo + 1 : lo  (no OOB at q == 1 / n == 1).
        let lop1 = self
            .builder
            .build_int_add(lo, i64_t.const_int(1, false), "col.q.lop1")
            .unwrap();
        let hi_ok = self
            .builder
            .build_int_compare(IntPredicate::ULT, lop1, n, "col.q.hiok")
            .unwrap();
        let hi = self
            .builder
            .build_select(hi_ok, lop1, lo, "col.q.hi")
            .unwrap()
            .into_int_value();
        let blo = self
            .builder
            .build_load(
                f64_t,
                unsafe {
                    self.builder
                        .build_gep(f64_t, buf, &[lo], "col.q.bloslot")
                        .unwrap()
                },
                "col.q.blo",
            )
            .unwrap()
            .into_float_value();
        let bhi = self
            .builder
            .build_load(
                f64_t,
                unsafe {
                    self.builder
                        .build_gep(f64_t, buf, &[hi], "col.q.bhislot")
                        .unwrap()
                },
                "col.q.bhi",
            )
            .unwrap()
            .into_float_value();
        let diff = self
            .builder
            .build_float_sub(bhi, blo, "col.q.diff")
            .unwrap();
        let scaled = self
            .builder
            .build_float_mul(frac, diff, "col.q.scaled")
            .unwrap();
        let result = self
            .builder
            .build_float_add(blo, scaled, "col.q.result")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[buf.into()], "")
            .unwrap();
        Ok(result.into())
    }

    // ── Indexing ────────────────────────────────────────────────

    /// `c[i] -> Option[T]` — runtime-bounds-checked (OOB traps, matching
    /// the interpreter); a valid slot yields `Some(c.data[i])`, a SQL
    /// null yields `None`.
    pub(super) fn compile_column_index(
        &mut self,
        control: PointerValue<'ctx>,
        info: &ColumnVarInfo<'ctx>,
        index: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if self.column_elem_is_string(info.elem) {
            return Err(
                "Column[String] indexing `c[i] -> Option[String]` is not yet supported by the \
                 native backend (`karac build`); it works under `karac run` and lands in a \
                 follow-on codegen slice. Use `iter_valid()` to read a String column for now."
                    .to_string(),
            );
        }
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column index outside function".to_string())?;
        let i_raw = self.compile_expr(index)?;
        let i = self.coerce_to_i64(i_raw)?;
        let len = self
            .column_load_field(control, 2, "col.idx.len")
            .into_int_value();
        // Unsigned compare catches both negative and `>= len`.
        let in_bounds = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.idx.ib")
            .unwrap();
        self.emit_column_guard(in_bounds, "Column index out of bounds")?;

        let bitmap = self
            .column_load_field(control, 1, "col.idx.bm")
            .into_pointer_value();
        let data = self
            .column_load_field(control, 0, "col.idx.data")
            .into_pointer_value();
        let valid = self.column_load_valid_bit(bitmap, i);

        let some_bb = self.context.append_basic_block(fn_val, "col.idx.some");
        let none_bb = self.context.append_basic_block(fn_val, "col.idx.none");
        let merge_bb = self.context.append_basic_block(fn_val, "col.idx.merge");
        self.builder
            .build_conditional_branch(valid, some_bb, none_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let slot = unsafe {
            self.builder
                .build_gep(info.elem, data, &[i], "col.idx.slot")
                .unwrap()
        };
        let loaded = self
            .builder
            .build_load(info.elem, slot, "col.idx.elem")
            .unwrap();
        let word = self.column_value_to_word(loaded);
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[word], some_end_bb, none_bb, "col.idx"))
    }

    // ── SQL three-valued-logic arithmetic / comparison ──────────

    /// True iff `expr` is a column-typed operand (an identifier bound as a
    /// column, or any column-typed expression in the side-table).
    fn expr_is_column(&self, expr: &Expr) -> bool {
        if let ExprKind::Identifier(name) = &expr.kind {
            if self.column_var_infos.contains_key(name.as_str()) {
                return true;
            }
        }
        self.column_typed_exprs
            .contains_key(&(expr.span.offset, expr.span.length))
    }

    /// Resolve a column operand to `(control_ptr, elem_llvm, elem_unsigned)`.
    /// An identifier reads its slot; any other column-typed expression is
    /// compiled to its control pointer (its element type from the
    /// side-table).
    fn column_operand(
        &mut self,
        expr: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>, bool), String> {
        if let ExprKind::Identifier(name) = &expr.kind {
            if let Some(info) = self.column_var_infos.get(name.as_str()).copied() {
                let ptr = self.column_ptr_for_var(name)?;
                return Ok((ptr, info.elem, info.elem_unsigned));
            }
        }
        let ci = self
            .column_typed_exprs
            .get(&(expr.span.offset, expr.span.length))
            .cloned()
            .ok_or_else(|| "column operand: missing element-type side-table entry".to_string())?;
        let elem = self.llvm_type_for_type_expr(&ci.elem);
        let unsigned = type_expr_is_unsigned_int(&ci.elem);
        let ptr = self.compile_expr(expr)?.into_pointer_value();
        Ok((ptr, elem, unsigned))
    }

    /// Free a column operand's three allocations when it is a fresh
    /// temporary (a column-producing expression that is not a live
    /// binding / place) — so chained `a + b + c` / `-a + b` don't leak the
    /// intermediate. An identifier / field / index operand is a live owner
    /// (its own scope cleanup frees it) and is left alone.
    fn column_free_if_fresh_temp(&self, expr: &Expr, control: PointerValue<'ctx>) {
        if matches!(
            expr.kind,
            ExprKind::Identifier(_) | ExprKind::FieldAccess { .. } | ExprKind::Index { .. }
        ) {
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let data = self
            .builder
            .build_load(
                ptr_ty,
                self.column_field_slot(control, 0, "col.ft.data.p"),
                "col.ft.data",
            )
            .unwrap()
            .into_pointer_value();
        let bm = self
            .builder
            .build_load(
                ptr_ty,
                self.column_field_slot(control, 1, "col.ft.bm.p"),
                "col.ft.bm",
            )
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[bm.into()], "")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[control.into()], "")
            .unwrap();
    }

    /// Element-wise `Column ⊕ Column` / `Column ⊕ scalar` (and the scalar-
    /// on-left form) with SQL null propagation: a result slot is valid iff
    /// **both** inputs are valid (a null on either side → null result, never
    /// `false`). Arithmetic (`+ - * /`) yields `Column[T]`; comparison
    /// (`== != < <= > >=`) yields `Column[bool]`. The per-element op runs
    /// only in the valid branch (so a null slot's placeholder never trips a
    /// div-by-zero / overflow trap — matching the interpreter, which evals
    /// valid slots only). Operands are read; the result is a fresh owned
    /// column; a fresh-temp operand is freed after the copy. Result element
    /// type comes from the side-table at the op span (T / bool).
    pub(super) fn compile_column_binop(
        &mut self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
        span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "column binop outside function".to_string())?;
        let result_ci = self
            .column_typed_exprs
            .get(&(span.offset, span.length))
            .cloned()
            .ok_or_else(|| "column binop: missing result element-type side-table".to_string())?;
        let result_elem = self.llvm_type_for_type_expr(&result_ci.elem);

        let l_is_col = self.expr_is_column(left);
        let r_is_col = self.expr_is_column(right);

        // ── col-col ──────────────────────────────────────────────
        if l_is_col && r_is_col {
            let (lp, lelem, lunsigned) = self.column_operand(left)?;
            let (rp, relem, _) = self.column_operand(right)?;
            let len = self
                .column_load_field(lp, 2, "col.bin.llen")
                .into_int_value();
            let rlen = self
                .column_load_field(rp, 2, "col.bin.rlen")
                .into_int_value();
            let eq = self
                .builder
                .build_int_compare(IntPredicate::EQ, len, rlen, "col.bin.leneq")
                .unwrap();
            self.emit_column_guard(eq, "column length mismatch in element-wise operator")?;
            let ldata = self
                .column_load_field(lp, 0, "col.bin.ldata")
                .into_pointer_value();
            let lbm = self
                .column_load_field(lp, 1, "col.bin.lbm")
                .into_pointer_value();
            let rdata = self
                .column_load_field(rp, 0, "col.bin.rdata")
                .into_pointer_value();
            let rbm = self
                .column_load_field(rp, 1, "col.bin.rbm")
                .into_pointer_value();
            let dst = self.column_alloc(result_elem, len, len)?;
            let dst_data = self
                .column_load_field(dst, 0, "col.bin.ddata")
                .into_pointer_value();
            let dst_bm = self
                .column_load_field(dst, 1, "col.bin.dbm")
                .into_pointer_value();

            let idx = self.builder.build_alloca(i64_t, "col.bin.i").unwrap();
            self.builder.build_store(idx, i64_t.const_zero()).unwrap();
            let head = self.context.append_basic_block(fn_val, "col.bin.head");
            let body = self.context.append_basic_block(fn_val, "col.bin.body");
            let comp = self.context.append_basic_block(fn_val, "col.bin.comp");
            let skip = self.context.append_basic_block(fn_val, "col.bin.skip");
            let cont = self.context.append_basic_block(fn_val, "col.bin.cont");
            let exit = self.context.append_basic_block(fn_val, "col.bin.exit");
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(head);
            let i = self
                .builder
                .build_load(i64_t, idx, "col.bin.iv")
                .unwrap()
                .into_int_value();
            let more = self
                .builder
                .build_int_compare(IntPredicate::ULT, i, len, "col.bin.more")
                .unwrap();
            self.builder
                .build_conditional_branch(more, body, exit)
                .unwrap();
            self.builder.position_at_end(body);
            let lv = self.column_load_valid_bit(lbm, i);
            let rv = self.column_load_valid_bit(rbm, i);
            let both = self.builder.build_and(lv, rv, "col.bin.both").unwrap();
            self.column_write_bit_runtime(dst_bm, i, both);
            self.builder
                .build_conditional_branch(both, comp, skip)
                .unwrap();
            self.builder.position_at_end(comp);
            let a = self.column_gep_load(ldata, lelem, i, "col.bin.a");
            let b = self.column_gep_load(rdata, relem, i, "col.bin.b");
            let r = self.compile_binop_typed(op, a, b, lunsigned)?;
            let r = self.coerce_scalar_to_type(r, result_elem);
            self.column_gep_store(dst_data, result_elem, i, r);
            self.builder.build_unconditional_branch(cont).unwrap();
            self.builder.position_at_end(skip);
            self.column_gep_store(dst_data, result_elem, i, self.column_zero_elem(result_elem));
            self.builder.build_unconditional_branch(cont).unwrap();
            self.builder.position_at_end(cont);
            let next = self
                .builder
                .build_int_add(i, i64_t.const_int(1, false), "col.bin.next")
                .unwrap();
            self.builder.build_store(idx, next).unwrap();
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(exit);
            self.column_free_if_fresh_temp(left, lp);
            self.column_free_if_fresh_temp(right, rp);
            return Ok(dst.into());
        }

        // ── col-scalar / scalar-col ──────────────────────────────
        let (col_expr, scalar_expr, scalar_on_left) = if l_is_col {
            (left, right, false)
        } else {
            (right, left, true)
        };
        let (cp, celem, cunsigned) = self.column_operand(col_expr)?;
        let scalar = self.compile_expr(scalar_expr)?;
        let scalar = self.coerce_scalar_to_type(scalar, celem);
        let len = self.column_load_field(cp, 2, "col.bs.len").into_int_value();
        let cdata = self
            .column_load_field(cp, 0, "col.bs.data")
            .into_pointer_value();
        let cbm = self
            .column_load_field(cp, 1, "col.bs.bm")
            .into_pointer_value();
        let dst = self.column_alloc(result_elem, len, len)?;
        let dst_data = self
            .column_load_field(dst, 0, "col.bs.ddata")
            .into_pointer_value();
        let dst_bm = self
            .column_load_field(dst, 1, "col.bs.dbm")
            .into_pointer_value();

        let idx = self.builder.build_alloca(i64_t, "col.bs.i").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        let head = self.context.append_basic_block(fn_val, "col.bs.head");
        let body = self.context.append_basic_block(fn_val, "col.bs.body");
        let comp = self.context.append_basic_block(fn_val, "col.bs.comp");
        let skip = self.context.append_basic_block(fn_val, "col.bs.skip");
        let cont = self.context.append_basic_block(fn_val, "col.bs.cont");
        let exit = self.context.append_basic_block(fn_val, "col.bs.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx, "col.bs.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.bs.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let v = self.column_load_valid_bit(cbm, i);
        self.column_write_bit_runtime(dst_bm, i, v);
        self.builder
            .build_conditional_branch(v, comp, skip)
            .unwrap();
        self.builder.position_at_end(comp);
        let x = self.column_gep_load(cdata, celem, i, "col.bs.x");
        let r = if scalar_on_left {
            self.compile_binop_typed(op, scalar, x, cunsigned)?
        } else {
            self.compile_binop_typed(op, x, scalar, cunsigned)?
        };
        let r = self.coerce_scalar_to_type(r, result_elem);
        self.column_gep_store(dst_data, result_elem, i, r);
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(skip);
        self.column_gep_store(dst_data, result_elem, i, self.column_zero_elem(result_elem));
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.bs.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        self.column_free_if_fresh_temp(col_expr, cp);
        Ok(dst.into())
    }

    /// Unary `-Column[T]` — negate every valid slot, nulls stay null.
    pub(super) fn compile_column_neg(
        &mut self,
        operand: &Expr,
        span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "column neg outside function".to_string())?;
        let result_ci = self
            .column_typed_exprs
            .get(&(span.offset, span.length))
            .cloned()
            .ok_or_else(|| "column neg: missing result element-type side-table".to_string())?;
        let result_elem = self.llvm_type_for_type_expr(&result_ci.elem);
        let (cp, celem, _) = self.column_operand(operand)?;
        let len = self
            .column_load_field(cp, 2, "col.neg.len")
            .into_int_value();
        let cdata = self
            .column_load_field(cp, 0, "col.neg.data")
            .into_pointer_value();
        let cbm = self
            .column_load_field(cp, 1, "col.neg.bm")
            .into_pointer_value();
        let dst = self.column_alloc(result_elem, len, len)?;
        let dst_data = self
            .column_load_field(dst, 0, "col.neg.ddata")
            .into_pointer_value();
        let dst_bm = self
            .column_load_field(dst, 1, "col.neg.dbm")
            .into_pointer_value();

        let idx = self.builder.build_alloca(i64_t, "col.neg.i").unwrap();
        self.builder.build_store(idx, i64_t.const_zero()).unwrap();
        let head = self.context.append_basic_block(fn_val, "col.neg.head");
        let body = self.context.append_basic_block(fn_val, "col.neg.body");
        let comp = self.context.append_basic_block(fn_val, "col.neg.comp");
        let skip = self.context.append_basic_block(fn_val, "col.neg.skip");
        let cont = self.context.append_basic_block(fn_val, "col.neg.cont");
        let exit = self.context.append_basic_block(fn_val, "col.neg.exit");
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, idx, "col.neg.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.neg.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body, exit)
            .unwrap();
        self.builder.position_at_end(body);
        let v = self.column_load_valid_bit(cbm, i);
        self.column_write_bit_runtime(dst_bm, i, v);
        self.builder
            .build_conditional_branch(v, comp, skip)
            .unwrap();
        self.builder.position_at_end(comp);
        let x = self.column_gep_load(cdata, celem, i, "col.neg.x");
        let r = match x {
            BasicValueEnum::FloatValue(fv) => self
                .builder
                .build_float_neg(fv, "col.neg.f")
                .unwrap()
                .into(),
            BasicValueEnum::IntValue(iv) => {
                self.builder.build_int_neg(iv, "col.neg.i2").unwrap().into()
            }
            other => other,
        };
        let r = self.coerce_scalar_to_type(r, result_elem);
        self.column_gep_store(dst_data, result_elem, i, r);
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(skip);
        self.column_gep_store(dst_data, result_elem, i, self.column_zero_elem(result_elem));
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "col.neg.next")
            .unwrap();
        self.builder.build_store(idx, next).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();
        self.builder.position_at_end(exit);
        self.column_free_if_fresh_temp(operand, cp);
        Ok(dst.into())
    }

    /// `data[i]` load with the element type.
    fn column_gep_load(
        &self,
        data: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        i: IntValue<'ctx>,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let slot = unsafe { self.builder.build_gep(elem, data, &[i], name).unwrap() };
        self.builder.build_load(elem, slot, name).unwrap()
    }

    /// `data[i] = v` store with the element type.
    fn column_gep_store(
        &self,
        data: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        i: IntValue<'ctx>,
        v: BasicValueEnum<'ctx>,
    ) {
        let slot = unsafe {
            self.builder
                .build_gep(elem, data, &[i], "col.store")
                .unwrap()
        };
        self.builder.build_store(slot, v).unwrap();
    }

    // ── Cleanup ─────────────────────────────────────────────────

    /// Register a column binding's cleanup (scope-exit free of the data
    /// buffer, validity bitmap, and control block). Mirrors
    /// `track_tensor_var`.
    pub(super) fn track_column_var(
        &mut self,
        column_alloca: PointerValue<'ctx>,
        string_elem: bool,
    ) {
        // Pre-emit the canonical String drop fn so the `&self` cleanup
        // drain can fetch it immutably from the module.
        if string_elem {
            self.emit_string_drop_fn();
        }
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(super::state::CleanupAction::FreeColumn {
                column_alloca,
                string_elem,
            });
        }
    }

    /// Branch-to-panic guard for column runtime checks (the Column twin
    /// of `emit_tensor_guard`). `pub(super)` so the DataFrame lowering can
    /// reuse it for the missing-column trap.
    pub(super) fn emit_column_guard(
        &mut self,
        ok: IntValue<'ctx>,
        message: &str,
    ) -> Result<(), String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "column guard outside function".to_string())?;
        let fail_bb = self.context.append_basic_block(fn_val, "col.guard.fail");
        let ok_bb = self.context.append_basic_block(fn_val, "col.guard.ok");
        self.builder
            .build_conditional_branch(ok, ok_bb, fail_bb)
            .unwrap();
        self.builder.position_at_end(fail_bb);
        self.emit_panic(message);
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        Ok(())
    }
}
