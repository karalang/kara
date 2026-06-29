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
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

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
    /// mirrors `tensor_elem_size`.
    fn column_elem_size(&self, elem: BasicTypeEnum<'ctx>) -> Result<u64, String> {
        match elem {
            BasicTypeEnum::FloatType(ft) => {
                if ft == self.context.f64_type() {
                    Ok(8)
                } else {
                    Ok(4)
                }
            }
            BasicTypeEnum::IntType(it) => Ok((it.get_bit_width() as u64).div_ceil(8)),
            other => Err(format!(
                "Column element type {:?} is not yet supported in codegen — \
                 numeric primitives and bool only",
                other
            )),
        }
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
    fn column_load_valid_bit(
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
        ) {
            return Ok(None);
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
                let arg = args
                    .first()
                    .ok_or_else(|| "Column.fillna requires a value argument".to_string())?;
                let fill = self.compile_expr(&arg.value)?;
                let fill = self.coerce_scalar_to_type(fill, info.elem);
                Ok(Some(self.compile_column_fillna(control, info.elem, fill)?))
            }
            "dropna" => Ok(Some(self.compile_column_dropna(control, info.elem)?)),
            "iter" => Ok(Some(self.compile_column_iter(control, info.elem)?)),
            "iter_valid" => Ok(Some(self.compile_column_iter_valid(control, info.elem)?)),
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
        let loaded = self
            .builder
            .build_load(elem, src_slot, "col.iv.sval")
            .unwrap();
        let dst_slot = unsafe {
            self.builder
                .build_gep(elem, buf, &[j], "col.iv.dslot")
                .unwrap()
        };
        self.builder.build_store(dst_slot, loaded).unwrap();
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

    /// `fillna(value) -> Column[T]` — a fresh all-valid column the same
    /// length as the receiver: valid slots copied as-is, null slots
    /// replaced with `value`. The receiver is borrowed (unchanged); the
    /// result owns independent allocations. (`treat_nan_as_null` is a
    /// follow-on slice.)
    fn compile_column_fillna(
        &mut self,
        src: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        fill: BasicValueEnum<'ctx>,
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
        let chosen = self
            .builder
            .build_select(valid, loaded, fill, "col.fill.sel")
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
    pub(super) fn track_column_var(&mut self, column_alloca: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(super::state::CleanupAction::FreeColumn { column_alloca });
        }
    }

    /// Branch-to-panic guard for column runtime checks (the Column twin
    /// of `emit_tensor_guard`).
    fn emit_column_guard(&mut self, ok: IntValue<'ctx>, message: &str) -> Result<(), String> {
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
