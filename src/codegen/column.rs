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
//! **Scope (this slice).** Constructors `new` / `with_capacity` /
//! `from_vec`; mutators `push` / `push_null`; accessors `len` /
//! `null_count` / `valid_count` / `is_null`; positional indexing
//! `c[i] -> Option[T]`. Element types are the numeric primitives + `bool`
//! (POD, ≤ one word), matching the Tensor codegen surface. The
//! Vec-returning methods (`iter` / `iter_valid` / `fillna` / `dropna`),
//! `from_iter_nullable`, and the SQL three-valued-logic arithmetic are
//! follow-on slices (they stay on `karac run` until then).

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use super::state::ColumnVarInfo;
use super::tensor::type_expr_is_unsigned_int;
use crate::ast::{CallArg, Expr, ExprKind, GenericArg, TypeExpr, TypeKind};

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
            "push" | "push_null" | "len" | "null_count" | "valid_count" | "is_null"
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
            _ => unreachable!(),
        }
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
