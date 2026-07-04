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
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue, StructValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

use super::kernel::{ContainerAccess, MapDest, MapKernelOp, MapOther, SortKey};
use super::state::{ColumnVarInfo, VarSlot};
use super::tensor::type_expr_is_unsigned_int;
use crate::ast::{BinOp, CallArg, Expr, ExprKind, GenericArg, PatternKind, TypeExpr, TypeKind};
use crate::reduce_kernel::ReduceOp;
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

        // `Column[String]` (a heap element, uniquely `elem_size == 24` among
        // the admitted Column element types): the `memcpy` above shared each
        // source String's heap ptr — re-clone every *valid* slot in place so
        // the copy owns independent heaps (`karac_string_clone(dst, dst)`
        // reads the shared ptr and overwrites it with a fresh clone). A
        // runtime branch so one `deep_copy` serves POD and String columns
        // (insert copy-in / `column` copy-out / `select`).
        if let Some(fn_val) = self.current_fn {
            let i64_t = self.context.i64_type();
            let is_string = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    elem_size,
                    i64_t.const_int(24, false),
                    "col.cp.isstr",
                )
                .unwrap();
            let str_bb = self.context.append_basic_block(fn_val, "col.cp.str");
            let done_bb = self.context.append_basic_block(fn_val, "col.cp.done");
            self.builder
                .build_conditional_branch(is_string, str_bb, done_bb)
                .unwrap();
            self.builder.position_at_end(str_bb);
            let str_st = self.vec_struct_type();
            let i_slot = self.builder.build_alloca(i64_t, "col.cp.s.i").unwrap();
            self.builder
                .build_store(i_slot, i64_t.const_zero())
                .unwrap();
            let head = self.context.append_basic_block(fn_val, "col.cp.s.head");
            let body = self.context.append_basic_block(fn_val, "col.cp.s.body");
            let cl = self.context.append_basic_block(fn_val, "col.cp.s.clone");
            let cont = self.context.append_basic_block(fn_val, "col.cp.s.cont");
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(head);
            let i = self
                .builder
                .build_load(i64_t, i_slot, "col.cp.s.iv")
                .unwrap()
                .into_int_value();
            let more = self
                .builder
                .build_int_compare(IntPredicate::ULT, i, len, "col.cp.s.more")
                .unwrap();
            self.builder
                .build_conditional_branch(more, body, done_bb)
                .unwrap();
            self.builder.position_at_end(body);
            let valid = self.column_load_valid_bit(bitmap, i);
            self.builder
                .build_conditional_branch(valid, cl, cont)
                .unwrap();
            self.builder.position_at_end(cl);
            let slot = unsafe {
                self.builder
                    .build_gep(str_st, data, &[i], "col.cp.s.slot")
                    .unwrap()
            };
            self.builder
                .build_call(self.karac_string_clone_fn, &[slot.into(), slot.into()], "")
                .unwrap();
            self.builder.build_unconditional_branch(cont).unwrap();
            self.builder.position_at_end(cont);
            self.builder
                .build_store(
                    i_slot,
                    self.builder
                        .build_int_add(i, i64_t.const_int(1, false), "col.cp.s.next")
                        .unwrap(),
                )
                .unwrap();
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(done_bb);
        }
        Ok(control)
    }

    /// Free a column control block's three allocations (data + bitmap +
    /// control). Unguarded — callers (the DataFrame drop loop) only pass
    /// live, frame-owned column pointers. `pub(super)` for reuse. For a
    /// `Column[String]` (`elem_size == 24`) each valid slot's String heap is
    /// freed first (cap-guarded, inline — matching `karac_drop_String`).
    pub(super) fn column_free_allocations(
        &self,
        ctrl: PointerValue<'ctx>,
        elem_size: IntValue<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let data = self
            .column_load_field(ctrl, 0, "col.free.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(ctrl, 1, "col.free.bm")
            .into_pointer_value();

        // `Column[String]` (`elem_size == 24`): free each valid slot's String
        // heap before the buffers (cap-guarded inline, like `karac_drop_String`
        // — a `cap == 0` empty/static String owns no heap). Runtime branch so
        // the same free serves POD and String columns.
        if let Some(fn_val) = self.current_fn {
            let is_string = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    elem_size,
                    i64_t.const_int(24, false),
                    "col.free.isstr",
                )
                .unwrap();
            let str_bb = self.context.append_basic_block(fn_val, "col.free.str");
            let buf_bb = self.context.append_basic_block(fn_val, "col.free.bufs");
            self.builder
                .build_conditional_branch(is_string, str_bb, buf_bb)
                .unwrap();
            self.builder.position_at_end(str_bb);
            let len = self
                .column_load_field(ctrl, 2, "col.free.len")
                .into_int_value();
            let str_st = self.vec_struct_type();
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i_slot = self.builder.build_alloca(i64_t, "col.free.s.i").unwrap();
            self.builder
                .build_store(i_slot, i64_t.const_zero())
                .unwrap();
            let head = self.context.append_basic_block(fn_val, "col.free.s.head");
            let body = self.context.append_basic_block(fn_val, "col.free.s.body");
            let chk = self.context.append_basic_block(fn_val, "col.free.s.chk");
            let frees = self.context.append_basic_block(fn_val, "col.free.s.free");
            let cont = self.context.append_basic_block(fn_val, "col.free.s.cont");
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(head);
            let i = self
                .builder
                .build_load(i64_t, i_slot, "col.free.s.iv")
                .unwrap()
                .into_int_value();
            let more = self
                .builder
                .build_int_compare(IntPredicate::ULT, i, len, "col.free.s.more")
                .unwrap();
            self.builder
                .build_conditional_branch(more, body, buf_bb)
                .unwrap();
            self.builder.position_at_end(body);
            let valid = self.column_load_valid_bit(bitmap, i);
            self.builder
                .build_conditional_branch(valid, chk, cont)
                .unwrap();
            self.builder.position_at_end(chk);
            let slot = unsafe {
                self.builder
                    .build_gep(str_st, data, &[i], "col.free.s.slot")
                    .unwrap()
            };
            // String struct { ptr(0), len(1), cap(2) } — free ptr iff cap > 0.
            let cap = self
                .builder
                .build_load(
                    i64_t,
                    self.builder
                        .build_struct_gep(str_st, slot, 2, "col.free.s.capp")
                        .unwrap(),
                    "col.free.s.cap",
                )
                .unwrap()
                .into_int_value();
            let has_heap = self
                .builder
                .build_int_compare(
                    IntPredicate::UGT,
                    cap,
                    i64_t.const_zero(),
                    "col.free.s.heap",
                )
                .unwrap();
            self.builder
                .build_conditional_branch(has_heap, frees, cont)
                .unwrap();
            self.builder.position_at_end(frees);
            let sptr = self
                .builder
                .build_load(
                    ptr_ty,
                    self.builder
                        .build_struct_gep(str_st, slot, 0, "col.free.s.ptrp")
                        .unwrap(),
                    "col.free.s.ptr",
                )
                .unwrap()
                .into_pointer_value();
            self.builder
                .build_call(self.free_fn, &[sptr.into()], "")
                .unwrap();
            self.builder.build_unconditional_branch(cont).unwrap();
            self.builder.position_at_end(cont);
            self.builder
                .build_store(
                    i_slot,
                    self.builder
                        .build_int_add(i, i64_t.const_int(1, false), "col.free.s.next")
                        .unwrap(),
                )
                .unwrap();
            self.builder.build_unconditional_branch(head).unwrap();
            self.builder.position_at_end(buf_bb);
        }

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
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // An owned Column binding's alloca holds a single `ptr` = the
        // control-block pointer; one load reaches the control block.
        //
        // A `ref Column[T]` / `mut ref Column[T]` parameter's alloca holds the
        // BORROW pointer the call site passed — which is `get_data_ptr(caller)`
        // = the address of the CALLER's Column alloca (i.e. a pointer TO the
        // control-block pointer, not the control block itself). Reaching the
        // control block therefore needs a SECOND load. Without it every field
        // read (len/data/bitmap) dereferences the caller's stack slot as if it
        // were the control struct — a `len` of garbage (often 0) makes the
        // callee silently emit no output (B-2026-07-02-27). The ref-param
        // double-indirection mirrors `get_data_ptr`'s ref arm, adjusted for the
        // extra pointer layer a Column control block carries versus an inline
        // Vec/Slice header.
        let first = self
            .builder
            .build_load(ptr_ty, slot.ptr, &format!("{}.col", name))
            .unwrap()
            .into_pointer_value();
        if self.ref_params.contains_key(name) {
            return Ok(self
                .builder
                .build_load(ptr_ty, first, &format!("{}.col.deref", name))
                .unwrap()
                .into_pointer_value());
        }
        Ok(first)
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
    pub(super) fn column_write_bit_runtime(
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
        // B-2026-07-02-6: an INLINE literal arg (`Column.from_vec([10, 20,
        // 30])` on a `Column[i32]`) has no let annotation to thread the
        // narrow element width — set the pending hint from the column's
        // element so the literal packs at the width the memcpy below
        // assumes. Scalar elements only (String columns deep-clone).
        let saved_hint = self.pending_let_elem_type;
        if elem.is_int_type() || elem.is_float_type() {
            self.pending_let_elem_type = Some(elem);
        }
        let arg_val = self.compile_expr(arg_expr);
        self.pending_let_elem_type = saved_hint;
        let arg_val = arg_val?;

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
                | "prod"
                | "mean"
                | "min"
                | "max"
                | "range"
                | "fold"
                | "map"
                | "zip_with"
                | "var"
                | "std"
                | "corr"
                | "median"
                | "quantile"
                | "argmin"
                | "argmax"
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
            "prod" => Ok(Some(self.compile_column_prod(
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
            // `Reduce[T]::range` default (`max - min`) — the builtin `Column`
            // implementor doesn't inherit it via the impl-splice, so emit the
            // same min/max reductions (each traps on an empty/all-null column,
            // matching `min`/`max`) and subtract on the element type.
            "range" => {
                let mx =
                    self.compile_column_minmax(control, info.elem, info.elem_unsigned, false)?;
                let mn =
                    self.compile_column_minmax(control, info.elem, info.elem_unsigned, true)?;
                Ok(Some(self.compile_binop_typed(
                    &BinOp::Sub,
                    mx,
                    mn,
                    info.elem_unsigned,
                )?))
            }
            // `fold[A](init, |acc, x| ...)` — the general left-fold primitive.
            // Inlines the closure body over the valid slots (see
            // `compile_column_fold`).
            "fold" => Ok(Some(self.compile_column_fold(
                control,
                info.elem,
                info.elem_unsigned,
                args,
            )?)),
            // `map(|x| ...) -> Column[T]` — element-wise map over valid slots,
            // producing a fresh column (see `compile_column_map`).
            "map" => Ok(Some(self.compile_column_map(
                control,
                info.elem,
                info.elem_unsigned,
                args,
            )?)),
            "zip_with" => Ok(Some(self.compile_column_zip_with(
                control,
                info.elem,
                info.elem_unsigned,
                args,
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
            // `argmin`/`argmax` -> `Option[i64]` (ElementwiseOrd, S6c): the
            // original-slot index of the first min/max over the valid slots,
            // `None` on an empty/all-null column (see `compile_column_argminmax`).
            "argmin" => Ok(Some(self.compile_column_argminmax(
                control,
                info.elem,
                info.elem_unsigned,
                false,
            )?)),
            "argmax" => Ok(Some(self.compile_column_argminmax(
                control,
                info.elem,
                info.elem_unsigned,
                true,
            )?)),
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

    /// The multiplicative identity (`1`) of `elem` — the fold seed for
    /// `prod` (seed-neutral: `1 * x0 * x1 * … = product`).
    fn column_one_elem(&self, elem: BasicTypeEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match elem {
            BasicTypeEnum::FloatType(ft) => ft.const_float(1.0).into(),
            BasicTypeEnum::IntType(it) => it.const_int(1, false).into(),
            other => other.const_zero(),
        }
    }

    /// `null_count()` (`valid == false`) / `valid_count()` (`valid ==
    /// true`) — one pass over `[0, len)` counting matching validity bits.
    pub(super) fn compile_column_count(
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
    pub(super) fn column_elem_to_f64(
        &self,
        val: BasicValueEnum<'ctx>,
        unsigned: bool,
    ) -> FloatValue<'ctx> {
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
    pub(super) fn column_sqrt_f64(&self, fv: FloatValue<'ctx>) -> FloatValue<'ctx> {
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
        let len = self
            .column_load_field(control, 2, "col.sum.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.sum.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.sum.bm")
            .into_pointer_value();
        // The shared validity-gated fold (`emit_reduce_fold` dispatches on the
        // `bitmap`): fold `+` over valid slots, guard the all-null case.
        let access = ContainerAccess {
            data,
            len,
            elem,
            unsigned,
            bitmap: Some(bitmap),
        };
        let seed = self.column_zero_elem(elem);
        self.emit_reduce_fold(&access, ReduceOp::Sum, seed)
    }

    /// `prod() -> T` — fold `*` over the valid slots (inherits the scalar
    /// overflow trap via `compile_binop_typed`); an all-null / empty column
    /// traps, exactly as `sum` does. Mirrors `compile_column_sum` with the
    /// multiplicative op + identity seed.
    fn compile_column_prod(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let len = self
            .column_load_field(control, 2, "col.prod.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.prod.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.prod.bm")
            .into_pointer_value();
        let access = ContainerAccess {
            data,
            len,
            elem,
            unsigned,
            bitmap: Some(bitmap),
        };
        let seed = self.column_one_elem(elem);
        self.emit_reduce_fold(&access, ReduceOp::Prod, seed)
    }

    /// `fold[A](init, |acc, elem| body) -> A` — the general left-fold
    /// primitive. Emits an in-place reduction loop over the valid slots (nulls
    /// skipped, in order), threading an `A`-typed accumulator through the
    /// closure body, which is **inlined** (compiled in the current function
    /// with `acc` / `elem` bound as locals) — sidestepping the closure-value
    /// ABI and letting any captures resolve through the enclosing scope. An
    /// empty / all-null column returns `init` unchanged (no empty trap — the
    /// fold identity, unlike `sum` / `min` / `max`).
    ///
    /// First cut: the closure must be an inline literal, and both the element
    /// `T` and the accumulator `A` must be POD (scalar). A closure-valued local
    /// / named fn, a heap element (`Column[String]`), or a heap / aggregate
    /// accumulator is rejected **loudly** here — `karac run` handles those
    /// shapes; their native (`karac build`) paths land in a follow-on slice.
    fn compile_column_fold(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        _unsigned: bool,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 2 {
            return Err(format!(
                "Column.fold expects 2 arguments (init, closure), got {}",
                args.len()
            ));
        }
        // The inline-body strategy needs the closure literal at the call site.
        let ExprKind::Closure { params, body, .. } = &args[1].value.kind else {
            return Err(
                "Column.fold expects an inline closure literal as its second \
                        argument under `karac build`; a closure-valued local / named \
                        fn is not yet supported by the native backend (it works under \
                        `karac run`)."
                    .to_string(),
            );
        };
        if params.len() != 2 {
            return Err(format!(
                "Column.fold closure must take exactly 2 parameters (acc, elem), got {}",
                params.len()
            ));
        }
        // Heap element (`Column[String]`) — the per-slot load would need
        // clone plumbing; POD-only for this cut.
        if self.column_elem_is_string(elem) {
            return Err("Column[String].fold is not yet supported by the native \
                        backend (`karac build`); it works under `karac run`."
                .to_string());
        }

        // 1. Seed the accumulator; its LLVM type IS `A`.
        let init_val = self.compile_expr(&args[0].value)?;
        let acc_ty = init_val.get_type();
        // A heap / aggregate accumulator (String / Vec `{ptr,len,cap}` struct,
        // or a pointer) would leak / double-free on each per-iteration
        // replacement without drop plumbing — reject loudly (POD `A` only).
        if acc_ty.is_struct_type() || acc_ty.is_pointer_type() || acc_ty.is_array_type() {
            return Err(
                "Column.fold with a heap / aggregate accumulator is not yet \
                        supported by the native backend (`karac build`); use a scalar \
                        accumulator, or run it under `karac run`."
                    .to_string(),
            );
        }

        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();

        let acc_slot = self.create_entry_alloca(fn_val, "col.fold.acc", acc_ty);
        self.builder.build_store(acc_slot, init_val).unwrap();

        // 2. Column control fields.
        let len = self
            .column_load_field(control, 2, "col.fold.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.fold.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.fold.bm")
            .into_pointer_value();

        // 3. Loop scaffold:
        //     head  → i < len ? body : exit
        //     body  → valid[i] ? apply : next   (nulls skipped)
        //     apply → elem = data[i]; bind (acc, elem); acc = <closure>; → next
        //     next  → i++ ; → head
        //     exit  → ret acc
        let head = self.context.append_basic_block(fn_val, "col.fold.head");
        let body_bb = self.context.append_basic_block(fn_val, "col.fold.body");
        let apply_bb = self.context.append_basic_block(fn_val, "col.fold.apply");
        let next_bb = self.context.append_basic_block(fn_val, "col.fold.next");
        let exit_bb = self.context.append_basic_block(fn_val, "col.fold.exit");

        let i_slot = self.create_entry_alloca(fn_val, "col.fold.i", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_zero())
            .unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        // head: i < len ?
        self.builder.position_at_end(head);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "col.fold.i.load")
            .unwrap()
            .into_int_value();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "col.fold.ir")
            .unwrap();
        self.builder
            .build_conditional_branch(in_range, body_bb, exit_bb)
            .unwrap();

        // body: skip null slots.
        self.builder.position_at_end(body_bb);
        let valid = self.column_load_valid_bit(bitmap, i);
        self.builder
            .build_conditional_branch(valid, apply_bb, next_bb)
            .unwrap();

        // apply: elem = data[i]; bind closure params; compile body; store acc.
        self.builder.position_at_end(apply_bb);
        let elem_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem, data, &[i], "col.fold.ep")
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem, elem_addr, "col.fold.ev")
            .unwrap();
        let acc_cur = self
            .builder
            .build_load(acc_ty, acc_slot, "col.fold.acc.cur")
            .unwrap();

        // Bind the two closure params (`acc`, then `elem`) as locals, saving
        // any shadowed outer binding so captures still resolve — and restoring
        // it after the body so the loop's own scope stays contained.
        let pname = |i: usize| match &params[i].pattern.kind {
            PatternKind::Binding(n) => n.clone(),
            _ => format!("_col_fold_p{i}"),
        };
        let acc_name = pname(0);
        let elem_name = pname(1);
        let saved_acc = self.variables.get(&acc_name).copied();
        let saved_elem = self.variables.get(&elem_name).copied();

        let acc_param = self.create_entry_alloca(fn_val, &acc_name, acc_ty);
        self.builder.build_store(acc_param, acc_cur).unwrap();
        self.variables.insert(
            acc_name.clone(),
            VarSlot {
                ptr: acc_param,
                ty: acc_ty,
            },
        );
        let elem_param = self.create_entry_alloca(fn_val, &elem_name, elem);
        self.builder.build_store(elem_param, elem_val).unwrap();
        self.variables.insert(
            elem_name.clone(),
            VarSlot {
                ptr: elem_param,
                ty: elem,
            },
        );

        let new_acc = self.compile_expr(body)?;
        // Defensive width/int-float reconciliation — the typechecker already
        // pinned the closure result to `A`, but a mixed-width body could leave
        // a narrower/other-kind SSA value; coerce to the accumulator's type.
        let new_acc = self.coerce_scalar_to_type(new_acc, acc_ty);

        // Restore the outer bindings the params shadowed.
        match saved_elem {
            Some(s) => {
                self.variables.insert(elem_name.clone(), s);
            }
            None => {
                self.variables.remove(&elem_name);
            }
        }
        match saved_acc {
            Some(s) => {
                self.variables.insert(acc_name.clone(), s);
            }
            None => {
                self.variables.remove(&acc_name);
            }
        }

        self.builder.build_store(acc_slot, new_acc).unwrap();
        self.builder.build_unconditional_branch(next_bb).unwrap();

        // next: i++ ; loop.
        self.builder.position_at_end(next_bb);
        let i2 = self
            .builder
            .build_load(i64_t, i_slot, "col.fold.i.load2")
            .unwrap()
            .into_int_value();
        let inc = self
            .builder
            .build_int_add(i2, i64_t.const_int(1, false), "col.fold.i.inc")
            .unwrap();
        self.builder.build_store(i_slot, inc).unwrap();
        self.builder.build_unconditional_branch(head).unwrap();

        // exit: the threaded accumulator.
        self.builder.position_at_end(exit_bb);
        Ok(self
            .builder
            .build_load(acc_ty, acc_slot, "col.fold.result")
            .unwrap())
    }

    /// `map(|x| ...) -> Column[T]` — the element-wise map surface (S6c-2).
    /// Applies the inline closure to every valid slot, producing a fresh
    /// `Column[T]` of the same length; null slots are preserved (the shared
    /// gated map copies the source validity bitmap and skips computing them).
    /// Same-element-type only (`Fn(T) -> T`); the first native cut is POD-only
    /// and inline-literal-only, matching `Column.fold`. A heap element
    /// (`Column[String]`) or a closure-valued local is rejected loudly (each
    /// works under `karac run`), never a silent miscompile.
    fn compile_column_map(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 1 {
            return Err(format!(
                "Column.map expects 1 argument (closure), got {}",
                args.len()
            ));
        }
        let ExprKind::Closure { params, body, .. } = &args[0].value.kind else {
            return Err(
                "Column.map expects an inline closure literal under `karac build`; a \
                 closure-valued local / named fn is not yet supported by the native \
                 backend (it works under `karac run`)."
                    .to_string(),
            );
        };
        if params.len() != 1 {
            return Err(format!(
                "Column.map closure must take exactly 1 parameter (elem), got {}",
                params.len()
            ));
        }
        if self.column_elem_is_string(elem) {
            return Err("Column[String].map is not yet supported by the native \
                        backend (`karac build`); it works under `karac run`."
                .to_string());
        }

        let len = self
            .column_load_field(control, 2, "col.map.len")
            .into_int_value();
        let src_data = self
            .column_load_field(control, 0, "col.map.data")
            .into_pointer_value();
        let src_bm = self
            .column_load_field(control, 1, "col.map.bm")
            .into_pointer_value();

        // Same element type (`Fn(T) -> T`), same length. The gated map copies
        // the single operand's validity into the result bitmap, so nulls carry
        // through without a bespoke bitmap memcpy.
        let dst = self.column_alloc(elem, len, len)?;
        let dst_data = self
            .column_load_field(dst, 0, "col.map.ddata")
            .into_pointer_value();
        let dst_bm = self
            .column_load_field(dst, 1, "col.map.dbm")
            .into_pointer_value();

        let lhs = ContainerAccess {
            data: src_data,
            len,
            elem,
            unsigned,
            bitmap: Some(src_bm),
        };
        let dest = MapDest {
            data: dst_data,
            elem,
            bitmap: Some(dst_bm),
        };
        self.emit_elementwise_map(
            &lhs,
            &MapOther::Unary,
            &MapKernelOp::Closure {
                params,
                body: body.as_ref(),
            },
            &dest,
        )?;
        Ok(dst.into())
    }

    /// `zip_with(other, |a, b| body) -> Column` — element-wise combine of two
    /// same-length columns through the inline closure. Result validity is the
    /// AND of the two operands' bitmaps (null propagation, like the element-wise
    /// binops); a null on either side yields a null result and the closure is
    /// not called there. The closure body is INLINED (same strategy as `map` /
    /// `fold`); only the inline-literal form reaches here.
    fn compile_column_zip_with(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if args.len() != 2 {
            return Err(format!(
                "Column.zip_with expects 2 arguments (other, closure), got {}",
                args.len()
            ));
        }
        let ExprKind::Closure { params, body, .. } = &args[1].value.kind else {
            return Err(
                "Column.zip_with expects an inline closure literal under `karac build`; a \
                 closure-valued local / named fn is not yet supported by the native \
                 backend (it works under `karac run`)."
                    .to_string(),
            );
        };
        if params.len() != 2 {
            return Err(format!(
                "Column.zip_with closure must take exactly 2 parameters (a, b), got {}",
                params.len()
            ));
        }
        if self.column_elem_is_string(elem) {
            return Err(
                "Column[String].zip_with is not yet supported by the native \
                        backend (`karac build`); it works under `karac run`."
                    .to_string(),
            );
        }

        // The second operand — another column (identifier or fresh temp).
        let (other_ctrl, other_elem, _other_unsigned) = self.column_operand(&args[0].value)?;

        let len = self
            .column_load_field(control, 2, "col.zip.llen")
            .into_int_value();
        let rlen = self
            .column_load_field(other_ctrl, 2, "col.zip.rlen")
            .into_int_value();
        let eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, len, rlen, "col.zip.leneq")
            .unwrap();
        self.emit_column_guard(eq, "Column.zip_with length mismatch")?;

        let ldata = self
            .column_load_field(control, 0, "col.zip.ldata")
            .into_pointer_value();
        let lbm = self
            .column_load_field(control, 1, "col.zip.lbm")
            .into_pointer_value();
        let rdata = self
            .column_load_field(other_ctrl, 0, "col.zip.rdata")
            .into_pointer_value();
        let rbm = self
            .column_load_field(other_ctrl, 1, "col.zip.rbm")
            .into_pointer_value();

        let dst = self.column_alloc(elem, len, len)?;
        let dst_data = self
            .column_load_field(dst, 0, "col.zip.ddata")
            .into_pointer_value();
        let dst_bm = self
            .column_load_field(dst, 1, "col.zip.dbm")
            .into_pointer_value();

        // Gated map: result bit = lv AND rv, per-element via the inlined closure
        // in the valid branch only.
        let lhs = ContainerAccess {
            data: ldata,
            len,
            elem,
            unsigned,
            bitmap: Some(lbm),
        };
        let other = MapOther::Access(ContainerAccess {
            data: rdata,
            len,
            elem: other_elem,
            unsigned,
            bitmap: Some(rbm),
        });
        let dest = MapDest {
            data: dst_data,
            elem,
            bitmap: Some(dst_bm),
        };
        self.emit_elementwise_map(
            &lhs,
            &other,
            &MapKernelOp::Closure {
                params,
                body: body.as_ref(),
            },
            &dest,
        )?;
        // Free the other operand if it was a fresh temporary (an identifier is a
        // live owner — left alone).
        self.column_free_if_fresh_temp(&args[0].value, other_ctrl);
        Ok(dst.into())
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
        let len = self
            .column_load_field(control, 2, "col.mm.len")
            .into_int_value();
        let data = self
            .column_load_field(control, 0, "col.mm.data")
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, "col.mm.bm")
            .into_pointer_value();
        // The shared validity-gated compare-select (`emit_reduce_minmax`
        // dispatches on the `bitmap`): seed on the first valid slot, guard the
        // all-null case.
        let access = ContainerAccess {
            data,
            len,
            elem,
            unsigned,
            bitmap: Some(bitmap),
        };
        self.emit_reduce_minmax(&access, !is_min)
    }

    /// `argmin() -> Option[i64]` / `argmax() -> Option[i64]` (ElementwiseOrd,
    /// S6c): the ORIGINAL slot index (Arrow position) of the first minimum /
    /// maximum over the valid slots, or `None` on an empty / all-null column
    /// (unlike `min`/`max`, which trap). The gated scan
    /// ([`emit_reduce_argminmax_gated`](super::Codegen::emit_reduce_argminmax_gated))
    /// yields `(seeded, best)`; `seeded` selects the `Some`/`None` arm here.
    fn compile_column_argminmax(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        is_max: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Column.argmin/argmax outside function".to_string())?;
        let access = self.column_access(control, elem, unsigned, "col.am");
        let bitmap = access
            .bitmap
            .ok_or_else(|| "Column.argmin/argmax expects a validity bitmap".to_string())?;
        let (seeded, best) = self.emit_reduce_argminmax_gated(&access, bitmap, is_max)?;
        // `seeded ? Some(best) : None` via the shared Option-phi builder.
        let some_bb = self.context.append_basic_block(fn_val, "col.am.some");
        let none_bb = self.context.append_basic_block(fn_val, "col.am.none");
        let merge_bb = self.context.append_basic_block(fn_val, "col.am.merge");
        self.builder
            .build_conditional_branch(seeded, some_bb, none_bb)
            .unwrap();
        self.builder.position_at_end(some_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();
        self.builder.position_at_end(merge_bb);
        Ok(self.build_option_some_via_phis(&[best], some_bb, none_bb, "col.am"))
    }

    /// Build the shared [`ContainerAccess`] for this column's data block +
    /// Arrow validity bitmap — the `bitmap: Some` receiver the kernel's
    /// `*_gated` / f64-accumulator emitters fold over.
    fn column_access(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        tag: &str,
    ) -> ContainerAccess<'ctx> {
        let len = self
            .column_load_field(control, 2, &format!("{tag}.len"))
            .into_int_value();
        let data = self
            .column_load_field(control, 0, &format!("{tag}.data"))
            .into_pointer_value();
        let bitmap = self
            .column_load_field(control, 1, &format!("{tag}.bm"))
            .into_pointer_value();
        ContainerAccess {
            data,
            len,
            elem,
            unsigned,
            bitmap: Some(bitmap),
        }
    }

    /// `mean() -> f64` — `Σ valid / count` as `f64`; an all-null / empty column
    /// traps. Shares the overflow-safe f64 first pass
    /// ([`emit_sum_f64_and_count`](super::Codegen::emit_sum_f64_and_count))
    /// with `var` / `std`.
    fn compile_column_mean(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let f64_t = self.context.f64_type();
        let access = self.column_access(control, elem, unsigned, "col.mean");
        let (sum, cnt) = self.emit_sum_f64_and_count(&access)?;
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

    /// `var() -> f64` / `std() -> f64` — the **sample** (Bessel `n-1`)
    /// variance / standard deviation over the valid slots. Requires ≥ 2
    /// valid values (else traps; sample variance is undefined for fewer).
    /// Both passes live in the shared kernel
    /// ([`emit_sum_f64_and_count`](super::Codegen::emit_sum_f64_and_count) +
    /// [`emit_variance_from`](super::Codegen::emit_variance_from) with
    /// `bessel: true`); `std` sqrts the variance.
    fn compile_column_var(
        &mut self,
        control: PointerValue<'ctx>,
        elem: BasicTypeEnum<'ctx>,
        unsigned: bool,
        is_std: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let access = self.column_access(control, elem, unsigned, "col.var");
        let (sum, cnt) = self.emit_sum_f64_and_count(&access)?;
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
        let var = self.emit_variance_from(&access, sum, cnt, true)?;
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

        // Insertion-sort buf[0..n] ascending via the shared kernel value sort.
        self.emit_sort_scratch(buf, n, &SortKey::Value);
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

    // ── DataFrame.describe() support (phase-11) ─────────────────

    /// A static `String` value `{ ptr → rodata, len, cap = 0 }`. `cap == 0`
    /// marks it static (the String drop / free paths skip it); a `column`
    /// copy-out re-clones it to a freeable heap String. Used to synthesize
    /// the `describe()` `statistic` label column.
    pub(super) fn build_static_string_value(&mut self, s: &str) -> StructValue<'ctx> {
        let data_ptr = self.build_str_bytes_global(s.as_bytes(), "df.stat");
        let str_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let mut agg = str_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, data_ptr, 0, "stat.data")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, i64_t.const_int(s.len() as u64, false), 1, "stat.len")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, i64_t.const_zero(), 2, "stat.cap")
            .unwrap()
            .into_struct_value();
        agg
    }

    /// Build a fresh all-valid `Column[String]` of the given static labels
    /// (the `describe()` `statistic` column). `pub(super)` for the DataFrame
    /// lowering.
    pub(super) fn build_label_column(
        &mut self,
        labels: &[&str],
    ) -> Result<PointerValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let i8_t = self.context.i8_type();
        let str_st: BasicTypeEnum = self.vec_struct_type().into();
        let n = i64_t.const_int(labels.len() as u64, false);
        let control = self.column_alloc(str_st, n, n)?;
        let data = self
            .column_load_field(control, 0, "df.lbl.data")
            .into_pointer_value();
        for (j, label) in labels.iter().enumerate() {
            let sv = self.build_static_string_value(label);
            let slot = unsafe {
                self.builder
                    .build_gep(
                        str_st,
                        data,
                        &[i64_t.const_int(j as u64, false)],
                        "df.lbl.slot",
                    )
                    .unwrap()
            };
            self.builder.build_store(slot, sv).unwrap();
        }
        let bm = self
            .column_load_field(control, 1, "df.lbl.bm")
            .into_pointer_value();
        let bm_bytes = self.column_bitmap_bytes(n);
        self.builder
            .build_memset(bm, 1, i8_t.const_int(0xFF, false), bm_bytes)
            .map_err(|e| format!("build_label_column bitmap memset failed: {e:?}"))?;
        Ok(control)
    }

    /// Read `data[i]` of a type-erased numeric column as `f64`, dispatching
    /// at runtime on the column's `kind` (1 = signed int, 2 = unsigned int,
    /// 3 = float) and `elem_size` (1/2/4/8). The data buffer's element stride
    /// is `elem_size`, so a `gep` with the matching concrete LLVM type and
    /// index `i` lands on the right element. For `describe()`, where the
    /// element type isn't statically known per column.
    fn column_load_elem_as_f64(
        &self,
        data: PointerValue<'ctx>,
        i: IntValue<'ctx>,
        kind: IntValue<'ctx>,
        elem_size: IntValue<'ctx>,
    ) -> FloatValue<'ctx> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let fn_val = self.current_fn.expect("load-elem-as-f64 in function");
        let result = self.builder.build_alloca(f64_t, "df.lf.res").unwrap();

        let is_float = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                kind,
                i64_t.const_int(3, false),
                "df.lf.isf",
            )
            .unwrap();
        let fbb = self.context.append_basic_block(fn_val, "df.lf.float");
        let ibb = self.context.append_basic_block(fn_val, "df.lf.int");
        let done = self.context.append_basic_block(fn_val, "df.lf.done");
        self.builder
            .build_conditional_branch(is_float, fbb, ibb)
            .unwrap();

        // Float: f64 (size 8) or f32 (size 4 → fpext).
        self.builder.position_at_end(fbb);
        let is8 = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                elem_size,
                i64_t.const_int(8, false),
                "df.lf.f8",
            )
            .unwrap();
        let f8 = self.context.append_basic_block(fn_val, "df.lf.f8b");
        let f4 = self.context.append_basic_block(fn_val, "df.lf.f4b");
        self.builder.build_conditional_branch(is8, f8, f4).unwrap();
        self.builder.position_at_end(f8);
        let v8 = self.column_gep_load(data, f64_t.into(), i, "df.lf.v8");
        self.builder.build_store(result, v8).unwrap();
        self.builder.build_unconditional_branch(done).unwrap();
        self.builder.position_at_end(f4);
        let v4 = self
            .column_gep_load(data, self.context.f32_type().into(), i, "df.lf.v4")
            .into_float_value();
        let v4e = self
            .builder
            .build_float_ext(v4, f64_t, "df.lf.f4ext")
            .unwrap();
        self.builder.build_store(result, v4e).unwrap();
        self.builder.build_unconditional_branch(done).unwrap();

        // Int: load the matching width, then s/u-to-float by signedness.
        self.builder.position_at_end(ibb);
        let is_unsigned = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                kind,
                i64_t.const_int(2, false),
                "df.lf.isu",
            )
            .unwrap();
        let widths = [
            (8u64, self.context.i64_type()),
            (4, self.context.i32_type()),
            (2, self.context.i16_type()),
            (1, self.context.i8_type()),
        ];
        // Chain of size checks; the last (size 1) is the fallthrough.
        let mut next = self.context.append_basic_block(fn_val, "df.lf.iw0");
        self.builder.build_unconditional_branch(next).unwrap();
        for (idx, (w, ity)) in widths.iter().enumerate() {
            self.builder.position_at_end(next);
            let cur = next;
            let is_last = idx == widths.len() - 1;
            let body = self.context.append_basic_block(fn_val, "df.lf.iwb");
            let cont = if is_last {
                body // unused
            } else {
                self.context.append_basic_block(fn_val, "df.lf.iwn")
            };
            if is_last {
                self.builder.position_at_end(cur);
                self.builder.build_unconditional_branch(body).unwrap();
            } else {
                let match_w = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        elem_size,
                        i64_t.const_int(*w, false),
                        "df.lf.iwc",
                    )
                    .unwrap();
                self.builder.position_at_end(cur);
                self.builder
                    .build_conditional_branch(match_w, body, cont)
                    .unwrap();
            }
            self.builder.position_at_end(body);
            let iv = self
                .column_gep_load(data, (*ity).into(), i, "df.lf.iv")
                .into_int_value();
            let sval = self
                .builder
                .build_signed_int_to_float(iv, f64_t, "df.lf.s")
                .unwrap();
            let uval = self
                .builder
                .build_unsigned_int_to_float(iv, f64_t, "df.lf.u")
                .unwrap();
            let sel = self
                .builder
                .build_select(is_unsigned, uval, sval, "df.lf.sel")
                .unwrap();
            self.builder.build_store(result, sel).unwrap();
            self.builder.build_unconditional_branch(done).unwrap();
            if !is_last {
                next = cont;
            }
        }

        self.builder.position_at_end(done);
        self.builder
            .build_load(f64_t, result, "df.lf.out")
            .unwrap()
            .into_float_value()
    }

    /// In-place ascending insertion sort of an `f64` buffer of `n` elements
    /// (NaN settles to the front under `fcmp ogt`, matching the quantile
    /// posture). Thin adapter over the shared kernel value sort
    /// ([`emit_sort_scratch`](super::Codegen::emit_sort_scratch)).
    pub(super) fn column_sort_f64_inplace(&self, buf: PointerValue<'ctx>, n: IntValue<'ctx>) {
        self.emit_sort_scratch(buf, n, &SortKey::Value);
    }

    /// Build a fresh all-valid `Column[f64]` holding the 8 `describe()`
    /// statistics `[count, mean, std, min, 25%, 50%, 75%, max]` over the
    /// valid slots of `src_col` (a numeric column of class `kind` /
    /// `elem_size`; `n` = its valid count, guaranteed > 0 by the caller).
    /// `std` is the sample (n-1) form — `NaN` for a single value (describe
    /// never traps). Quartiles use the same linear interpolation as
    /// `Column.quantile`.
    pub(super) fn compile_describe_stats_column(
        &mut self,
        src_col: PointerValue<'ctx>,
        kind: IntValue<'ctx>,
        elem_size: IntValue<'ctx>,
        n: IntValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let f64_t = self.context.f64_type();
        let i8_t = self.context.i8_type();
        let fn_val = self
            .current_fn
            .ok_or_else(|| "describe stats outside function".to_string())?;

        // Scratch f64 buffer of the n valid values (read via runtime dispatch).
        let nbytes = self
            .builder
            .build_int_mul(n, i64_t.const_int(8, false), "df.st.nb")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[nbytes.into()], "df.st.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let len = self
            .column_load_field(src_col, 2, "df.st.len")
            .into_int_value();
        let data = self
            .column_load_field(src_col, 0, "df.st.data")
            .into_pointer_value();
        let bm = self
            .column_load_field(src_col, 1, "df.st.bm")
            .into_pointer_value();
        let sum = self.builder.build_alloca(f64_t, "df.st.sum").unwrap();
        self.builder.build_store(sum, f64_t.const_zero()).unwrap();
        let i = self.builder.build_alloca(i64_t, "df.st.i").unwrap();
        let j = self.builder.build_alloca(i64_t, "df.st.j").unwrap();
        self.builder.build_store(i, i64_t.const_zero()).unwrap();
        self.builder.build_store(j, i64_t.const_zero()).unwrap();
        let h = self.context.append_basic_block(fn_val, "df.st.h");
        let b = self.context.append_basic_block(fn_val, "df.st.b");
        let a = self.context.append_basic_block(fn_val, "df.st.a");
        let c = self.context.append_basic_block(fn_val, "df.st.c");
        let e = self.context.append_basic_block(fn_val, "df.st.e");
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(h);
        let iv = self
            .builder
            .build_load(i64_t, i, "df.st.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, iv, len, "df.st.more")
            .unwrap();
        self.builder.build_conditional_branch(more, b, e).unwrap();
        self.builder.position_at_end(b);
        let valid = self.column_load_valid_bit(bm, iv);
        self.builder.build_conditional_branch(valid, a, c).unwrap();
        self.builder.position_at_end(a);
        let xf = self.column_load_elem_as_f64(data, iv, kind, elem_size);
        let jv = self
            .builder
            .build_load(i64_t, j, "df.st.jv")
            .unwrap()
            .into_int_value();
        let slot = unsafe {
            self.builder
                .build_gep(f64_t, buf, &[jv], "df.st.slot")
                .unwrap()
        };
        self.builder.build_store(slot, xf).unwrap();
        let s0 = self
            .builder
            .build_load(f64_t, sum, "df.st.s0")
            .unwrap()
            .into_float_value();
        self.builder
            .build_store(
                sum,
                self.builder.build_float_add(s0, xf, "df.st.s1").unwrap(),
            )
            .unwrap();
        self.builder
            .build_store(
                j,
                self.builder
                    .build_int_add(jv, i64_t.const_int(1, false), "df.st.j2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(c).unwrap();
        self.builder.position_at_end(c);
        self.builder
            .build_store(
                i,
                self.builder
                    .build_int_add(iv, i64_t.const_int(1, false), "df.st.i2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(h).unwrap();
        self.builder.position_at_end(e);

        let nf = self
            .builder
            .build_unsigned_int_to_float(n, f64_t, "df.st.nf")
            .unwrap();
        let sumv = self
            .builder
            .build_load(f64_t, sum, "df.st.sumv")
            .unwrap()
            .into_float_value();
        let mean = self
            .builder
            .build_float_div(sumv, nf, "df.st.mean")
            .unwrap();

        // std (sample): n >= 2 ? sqrt(Σ(x-mean)² / (n-1)) : NaN.
        let two = i64_t.const_int(2, false);
        let has2 = self
            .builder
            .build_int_compare(IntPredicate::UGE, n, two, "df.st.has2")
            .unwrap();
        let ss = self.builder.build_alloca(f64_t, "df.st.ss").unwrap();
        self.builder.build_store(ss, f64_t.const_zero()).unwrap();
        let k = self.builder.build_alloca(i64_t, "df.st.k").unwrap();
        self.builder.build_store(k, i64_t.const_zero()).unwrap();
        let sh = self.context.append_basic_block(fn_val, "df.st.sh");
        let sb = self.context.append_basic_block(fn_val, "df.st.sb");
        let se = self.context.append_basic_block(fn_val, "df.st.se");
        self.builder.build_unconditional_branch(sh).unwrap();
        self.builder.position_at_end(sh);
        let kv = self
            .builder
            .build_load(i64_t, k, "df.st.kv")
            .unwrap()
            .into_int_value();
        let kmore = self
            .builder
            .build_int_compare(IntPredicate::ULT, kv, n, "df.st.kmore")
            .unwrap();
        self.builder
            .build_conditional_branch(kmore, sb, se)
            .unwrap();
        self.builder.position_at_end(sb);
        let xs = unsafe {
            self.builder
                .build_gep(f64_t, buf, &[kv], "df.st.xs")
                .unwrap()
        };
        let xv = self
            .builder
            .build_load(f64_t, xs, "df.st.xv")
            .unwrap()
            .into_float_value();
        let d = self.builder.build_float_sub(xv, mean, "df.st.d").unwrap();
        let d2 = self.builder.build_float_mul(d, d, "df.st.d2").unwrap();
        let ssv = self
            .builder
            .build_load(f64_t, ss, "df.st.ssv")
            .unwrap()
            .into_float_value();
        self.builder
            .build_store(
                ss,
                self.builder.build_float_add(ssv, d2, "df.st.ss2").unwrap(),
            )
            .unwrap();
        self.builder
            .build_store(
                k,
                self.builder
                    .build_int_add(kv, i64_t.const_int(1, false), "df.st.k2")
                    .unwrap(),
            )
            .unwrap();
        self.builder.build_unconditional_branch(sh).unwrap();
        self.builder.position_at_end(se);
        let ssf = self
            .builder
            .build_load(f64_t, ss, "df.st.ssf")
            .unwrap()
            .into_float_value();
        let nm1 = self
            .builder
            .build_float_sub(nf, f64_t.const_float(1.0), "df.st.nm1")
            .unwrap();
        let var = self.builder.build_float_div(ssf, nm1, "df.st.var").unwrap();
        let std_ok = self.column_sqrt_f64(var);
        let nan = f64_t.const_float(f64::NAN);
        let std = self
            .builder
            .build_select(has2, std_ok, nan, "df.st.std")
            .unwrap()
            .into_float_value();

        // Sort the scratch for min / max / quantiles.
        self.column_sort_f64_inplace(buf, n);
        let load_at = |me: &Self, idx: IntValue<'ctx>, nm: &str| -> FloatValue<'ctx> {
            let p = unsafe { me.builder.build_gep(f64_t, buf, &[idx], nm).unwrap() };
            me.builder
                .build_load(f64_t, p, nm)
                .unwrap()
                .into_float_value()
        };
        let min = load_at(self, i64_t.const_zero(), "df.st.min");
        let nm1i = self
            .builder
            .build_int_sub(n, i64_t.const_int(1, false), "df.st.nm1i")
            .unwrap();
        let max = load_at(self, nm1i, "df.st.max");
        let q = |me: &Self, p: f64, nm: &str| -> FloatValue<'ctx> {
            // pos = p*(n-1); lo=floor; hi=min(lo+1,n-1); buf[lo]+frac*(buf[hi]-buf[lo]).
            let pos = me
                .builder
                .build_float_mul(f64_t.const_float(p), nm1, &format!("{nm}.pos"))
                .unwrap();
            let lo = me
                .builder
                .build_float_to_unsigned_int(pos, i64_t, &format!("{nm}.lo"))
                .unwrap();
            let lof = me
                .builder
                .build_unsigned_int_to_float(lo, f64_t, &format!("{nm}.lof"))
                .unwrap();
            let frac = me
                .builder
                .build_float_sub(pos, lof, &format!("{nm}.frac"))
                .unwrap();
            let lop1 = me
                .builder
                .build_int_add(lo, i64_t.const_int(1, false), &format!("{nm}.lop1"))
                .unwrap();
            let hiok = me
                .builder
                .build_int_compare(IntPredicate::ULT, lop1, n, &format!("{nm}.hiok"))
                .unwrap();
            let hi = me
                .builder
                .build_select(hiok, lop1, lo, &format!("{nm}.hi"))
                .unwrap()
                .into_int_value();
            let blo = load_at(me, lo, &format!("{nm}.blo"));
            let bhi = load_at(me, hi, &format!("{nm}.bhi"));
            let diff = me
                .builder
                .build_float_sub(bhi, blo, &format!("{nm}.diff"))
                .unwrap();
            let sc = me
                .builder
                .build_float_mul(frac, diff, &format!("{nm}.sc"))
                .unwrap();
            me.builder
                .build_float_add(blo, sc, &format!("{nm}.r"))
                .unwrap()
        };
        let q25 = q(self, 0.25, "df.st.q25");
        let q50 = q(self, 0.50, "df.st.q50");
        let q75 = q(self, 0.75, "df.st.q75");
        self.builder
            .build_call(self.free_fn, &[buf.into()], "")
            .unwrap();

        // Build the result Column[f64] of the 8 stats (all valid).
        let eight = i64_t.const_int(8, false);
        let out = self.column_alloc(f64_t.into(), eight, eight)?;
        let out_data = self
            .column_load_field(out, 0, "df.st.odata")
            .into_pointer_value();
        let stats = [nf, mean, std, min, q25, q50, q75, max];
        for (idx, v) in stats.iter().enumerate() {
            let slot = unsafe {
                self.builder
                    .build_gep(
                        f64_t,
                        out_data,
                        &[i64_t.const_int(idx as u64, false)],
                        "df.st.oslot",
                    )
                    .unwrap()
            };
            self.builder.build_store(slot, *v).unwrap();
        }
        let out_bm = self
            .column_load_field(out, 1, "df.st.obm")
            .into_pointer_value();
        let bm8 = self.column_bitmap_bytes(eight);
        self.builder
            .build_memset(out_bm, 1, i8_t.const_int(0xFF, false), bm8)
            .map_err(|e| format!("describe stats bitmap memset failed: {e:?}"))?;
        Ok(out)
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

            // The shared gated map: result bit = lv AND rv (null propagation),
            // per-element op via `compile_binop_typed` in the valid branch only.
            let lhs = ContainerAccess {
                data: ldata,
                len,
                elem: lelem,
                unsigned: lunsigned,
                bitmap: Some(lbm),
            };
            let other = MapOther::Access(ContainerAccess {
                data: rdata,
                len,
                elem: relem,
                unsigned: lunsigned,
                bitmap: Some(rbm),
            });
            let dest = MapDest {
                data: dst_data,
                elem: result_elem,
                bitmap: Some(dst_bm),
            };
            self.emit_elementwise_map(&lhs, &other, &MapKernelOp::Binop(op), &dest)?;
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

        // The shared gated map with a broadcast scalar (`on_left` for `2 - c`).
        let lhs = ContainerAccess {
            data: cdata,
            len,
            elem: celem,
            unsigned: cunsigned,
            bitmap: Some(cbm),
        };
        let other = MapOther::Scalar {
            value: scalar,
            on_left: scalar_on_left,
        };
        let dest = MapDest {
            data: dst_data,
            elem: result_elem,
            bitmap: Some(dst_bm),
        };
        self.emit_elementwise_map(&lhs, &other, &MapKernelOp::Binop(op), &dest)?;
        self.column_free_if_fresh_temp(col_expr, cp);
        Ok(dst.into())
    }

    /// Unary `-Column[T]` — negate every valid slot, nulls stay null.
    pub(super) fn compile_column_neg(
        &mut self,
        operand: &Expr,
        span: &Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
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

        // The shared gated map, unary: scalar `-x` semantics per valid slot —
        // IEEE `fneg` for floats, checked `0 - x` for ints (traps on
        // `i64::MIN` like the interpreter's `checked_neg`; B-2026-07-01-2).
        let lhs = ContainerAccess {
            data: cdata,
            len,
            elem: celem,
            unsigned: false,
            bitmap: Some(cbm),
        };
        let dest = MapDest {
            data: dst_data,
            elem: result_elem,
            bitmap: Some(dst_bm),
        };
        self.emit_elementwise_map(&lhs, &MapOther::Unary, &MapKernelOp::Neg, &dest)?;
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
