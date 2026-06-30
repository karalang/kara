//! `DataFrame` codegen lowering (phase-11 data-science stdlib, Arrow
//! commitment Q6). The interpreter MVP
//! (`src/interpreter/method_call_dataframe.rs`) carries the logical
//! semantics; this is the native LLVM lowering.
//!
//! **Value layout.** A `DataFrame` is a single pointer to one malloc'd
//! control block:
//!
//! ```text
//! { ptr entries, i64 len, i64 capacity }
//! ```
//!
//! `entries` is a contiguous buffer of `capacity` entry structs, in
//! insertion (Arrow schema) order:
//!
//! ```text
//! { ptr name_data, i64 name_len, ptr col_ctrl, i64 elem_size }
//! ```
//!
//! Each `col_ctrl` is a **type-erased** column control pointer — the
//! `Column[T]` control block is identical for every `T` (the element
//! type only matters at the data-buffer GEP), so a heterogeneous table
//! just stores plain column pointers. `elem_size` is the only per-column
//! type residue the erased pointer can't recover; it lets the value-copy
//! paths size the data buffer. `column(name)` reinterprets a stored
//! pointer as `Column[T]`, recovering `T` from the binding annotation —
//! exactly the `AnyColumn` erasure, with no wrapper.
//!
//! **Ownership — value semantics.** The frame owns its columns outright:
//! `insert` deep-copies the argument column *in*, `column(name)` deep-
//! copies *out*. No `Arc`-style sharing (compiled code has no refcount),
//! so a program that mutates a looked-up / inserted column never sees it
//! reflected in the frame — byte-identical to `karac run`. A bound
//! DataFrame gets a `CleanupAction::FreeDataFrame` that loops the entries
//! freeing each column (data + bitmap + control) and each name buffer,
//! then the entries buffer and the control block.
//!
//! **Scope (full interpreter-MVP parity).** `new`; `insert` (copy-in,
//! replace-or-append, equal-length runtime guard); `column(name) ->
//! Column[T]` (copy-out); `has_column`; `width`; `height`;
//! `column_names() -> Vec[String]` (fresh name copies); `select(cols) ->
//! DataFrame` (fresh frame of column copies). Column element types are
//! the numeric primitives and `bool`, matching the Column codegen
//! surface.

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use crate::ast::{CallArg, Expr, ExprKind};
use crate::token::Span;

impl<'ctx> super::Codegen<'ctx> {
    // ── Layout helpers ──────────────────────────────────────────

    /// The control-block type `{ ptr entries, i64 len, i64 capacity }`.
    pub(super) fn dataframe_control_struct_type(&self) -> StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default()).into();
        let i64_ty = self.context.i64_type().into();
        self.context.struct_type(&[ptr_ty, i64_ty, i64_ty], false)
    }

    /// One entry: `{ ptr name_data, i64 name_len, ptr col_ctrl, i64
    /// elem_size }`.
    pub(super) fn dataframe_entry_struct_type(&self) -> StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default()).into();
        let i64_ty = self.context.i64_type().into();
        self.context
            .struct_type(&[ptr_ty, i64_ty, ptr_ty, i64_ty], false)
    }

    /// GEP + load a control-block field. 0 = entries (ptr), 1 = len (i64),
    /// 2 = capacity (i64).
    fn df_load_field(
        &self,
        control: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let st = self.dataframe_control_struct_type();
        let p = self
            .builder
            .build_struct_gep(st, control, idx, &format!("{name}.p"))
            .unwrap();
        let ty: BasicTypeEnum<'ctx> = if idx == 0 {
            self.context.ptr_type(AddressSpace::default()).into()
        } else {
            self.context.i64_type().into()
        };
        self.builder.build_load(ty, p, name).unwrap()
    }

    /// GEP to a control-block field slot (for stores).
    fn df_field_slot(
        &self,
        control: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> PointerValue<'ctx> {
        let st = self.dataframe_control_struct_type();
        self.builder
            .build_struct_gep(st, control, idx, name)
            .unwrap()
    }

    /// Pointer to entry `i` within the `entries` buffer.
    fn df_entry_ptr(&self, entries: PointerValue<'ctx>, i: IntValue<'ctx>) -> PointerValue<'ctx> {
        let est = self.dataframe_entry_struct_type();
        unsafe {
            self.builder
                .build_gep(est, entries, &[i], "df.entry")
                .unwrap()
        }
    }

    /// GEP to a field slot of entry pointer `entry`. 0 = name_data,
    /// 1 = name_len, 2 = col_ctrl, 3 = elem_size.
    fn df_entry_field_slot(
        &self,
        entry: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> PointerValue<'ctx> {
        let est = self.dataframe_entry_struct_type();
        self.builder
            .build_struct_gep(est, entry, idx, name)
            .unwrap()
    }

    fn df_entry_load(
        &self,
        entry: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let slot = self.df_entry_field_slot(entry, idx, name);
        let ty: BasicTypeEnum<'ctx> = if idx == 0 || idx == 2 {
            self.context.ptr_type(AddressSpace::default()).into()
        } else {
            self.context.i64_type().into()
        };
        self.builder.build_load(ty, slot, name).unwrap()
    }

    /// Load the control pointer from a DataFrame binding's slot.
    pub(super) fn dataframe_ptr_for_var(&self, name: &str) -> Result<PointerValue<'ctx>, String> {
        let slot = self
            .variables
            .get(name)
            .ok_or_else(|| format!("Undefined DataFrame variable '{name}'"))?;
        Ok(self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                slot.ptr,
                &format!("{name}.df"),
            )
            .unwrap()
            .into_pointer_value())
    }

    // ── Constructor ─────────────────────────────────────────────

    /// `DataFrame.new()` — a fresh empty table (no columns, zero rows).
    pub(super) fn compile_dataframe_new(&mut self) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let ctrl_bytes = self.dataframe_control_struct_type().size_of().unwrap();
        let control = self
            .builder
            .build_call(self.malloc_fn, &[ctrl_bytes.into()], "df.ctrl")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_store(
                self.df_field_slot(control, 0, "df.f.entries"),
                ptr_t.const_null(),
            )
            .unwrap();
        self.builder
            .build_store(
                self.df_field_slot(control, 1, "df.f.len"),
                i64_t.const_zero(),
            )
            .unwrap();
        self.builder
            .build_store(
                self.df_field_slot(control, 2, "df.f.cap"),
                i64_t.const_zero(),
            )
            .unwrap();
        Ok(control.into())
    }

    // ── Name lookup (runtime scan) ──────────────────────────────

    /// Scan the entries for one whose name equals `(name_data, name_len)`.
    /// Returns `(found: i1, index: i64)` — index is the matching slot, or
    /// `len` when not found. Uses alloca-based loop induction.
    fn dataframe_find_index(
        &mut self,
        control: PointerValue<'ctx>,
        name_data: PointerValue<'ctx>,
        name_len: IntValue<'ctx>,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "DataFrame lookup outside function".to_string())?;
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();

        let entries = self
            .df_load_field(control, 0, "df.find.entries")
            .into_pointer_value();
        let len = self
            .df_load_field(control, 1, "df.find.len")
            .into_int_value();

        let i_slot = self.create_entry_alloca(fn_val, "df.find.i", i64_t.into());
        let found_slot = self.create_entry_alloca(fn_val, "df.find.found", i64_t.into());
        let idx_slot = self.create_entry_alloca(fn_val, "df.find.idx", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(found_slot, i64_t.const_zero())
            .unwrap();
        self.builder.build_store(idx_slot, len).unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "df.find.cond");
        let body_bb = self.context.append_basic_block(fn_val, "df.find.body");
        let cmp_bb = self.context.append_basic_block(fn_val, "df.find.cmp");
        let hit_bb = self.context.append_basic_block(fn_val, "df.find.hit");
        let next_bb = self.context.append_basic_block(fn_val, "df.find.next");
        let exit_bb = self.context.append_basic_block(fn_val, "df.find.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // cond: i < len ?
        self.builder.position_at_end(cond_bb);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "df.find.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "df.find.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, exit_bb)
            .unwrap();

        // body: name_len == entry.name_len ?
        self.builder.position_at_end(body_bb);
        let entry = self.df_entry_ptr(entries, i);
        let e_name_len = self
            .df_entry_load(entry, 1, "df.find.elen")
            .into_int_value();
        let len_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, name_len, e_name_len, "df.find.leneq")
            .unwrap();
        self.builder
            .build_conditional_branch(len_eq, cmp_bb, next_bb)
            .unwrap();

        // cmp: memcmp(name_data, entry.name_data, name_len) == 0 ?
        self.builder.position_at_end(cmp_bb);
        let e_name_data = self
            .df_entry_load(entry, 0, "df.find.edata")
            .into_pointer_value();
        let cmp = self
            .builder
            .build_call(
                self.memcmp_fn,
                &[name_data.into(), e_name_data.into(), name_len.into()],
                "df.find.memcmp",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let data_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, cmp, i32_t.const_zero(), "df.find.dataeq")
            .unwrap();
        self.builder
            .build_conditional_branch(data_eq, hit_bb, next_bb)
            .unwrap();

        // hit: record found + index, exit.
        self.builder.position_at_end(hit_bb);
        self.builder
            .build_store(found_slot, i64_t.const_int(1, false))
            .unwrap();
        self.builder.build_store(idx_slot, i).unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        // next: i++, loop.
        self.builder.position_at_end(next_bb);
        let i_next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "df.find.inc")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // exit: load results.
        self.builder.position_at_end(exit_bb);
        let found_i64 = self
            .builder
            .build_load(i64_t, found_slot, "df.find.foundv")
            .unwrap()
            .into_int_value();
        let found = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                found_i64,
                i64_t.const_zero(),
                "df.find.foundb",
            )
            .unwrap();
        let index = self
            .builder
            .build_load(i64_t, idx_slot, "df.find.idxv")
            .unwrap()
            .into_int_value();
        Ok((found, index))
    }

    /// Scan for the first entry whose name *differs* from `(name_data,
    /// name_len)`, returning `(has_other: i1, height: i64)` — `height` is
    /// that column's length (the table's row count for the equal-length
    /// guard). `has_other` is false (height 0) when every column shares the
    /// name (or the frame is empty), in which case the guard is skipped.
    fn dataframe_other_height(
        &mut self,
        control: PointerValue<'ctx>,
        name_data: PointerValue<'ctx>,
        name_len: IntValue<'ctx>,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "DataFrame guard outside function".to_string())?;
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();

        let entries = self
            .df_load_field(control, 0, "df.oh.entries")
            .into_pointer_value();
        let len = self.df_load_field(control, 1, "df.oh.len").into_int_value();

        let i_slot = self.create_entry_alloca(fn_val, "df.oh.i", i64_t.into());
        let has_slot = self.create_entry_alloca(fn_val, "df.oh.has", i64_t.into());
        let h_slot = self.create_entry_alloca(fn_val, "df.oh.h", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(has_slot, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(h_slot, i64_t.const_zero())
            .unwrap();

        let cond_bb = self.context.append_basic_block(fn_val, "df.oh.cond");
        let body_bb = self.context.append_basic_block(fn_val, "df.oh.body");
        let cmp_bb = self.context.append_basic_block(fn_val, "df.oh.cmp");
        let other_bb = self.context.append_basic_block(fn_val, "df.oh.other");
        let next_bb = self.context.append_basic_block(fn_val, "df.oh.next");
        let exit_bb = self.context.append_basic_block(fn_val, "df.oh.exit");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "df.oh.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "df.oh.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, exit_bb)
            .unwrap();

        // body: a different name_len means a different name → other.
        self.builder.position_at_end(body_bb);
        let entry = self.df_entry_ptr(entries, i);
        let e_name_len = self.df_entry_load(entry, 1, "df.oh.elen").into_int_value();
        let len_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, name_len, e_name_len, "df.oh.leneq")
            .unwrap();
        // len differs → definitely other; len equal → compare bytes.
        self.builder
            .build_conditional_branch(len_eq, cmp_bb, other_bb)
            .unwrap();

        self.builder.position_at_end(cmp_bb);
        let e_name_data = self
            .df_entry_load(entry, 0, "df.oh.edata")
            .into_pointer_value();
        let cmp = self
            .builder
            .build_call(
                self.memcmp_fn,
                &[name_data.into(), e_name_data.into(), name_len.into()],
                "df.oh.memcmp",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let same = self
            .builder
            .build_int_compare(IntPredicate::EQ, cmp, i32_t.const_zero(), "df.oh.same")
            .unwrap();
        // same name → keep scanning; different → other.
        self.builder
            .build_conditional_branch(same, next_bb, other_bb)
            .unwrap();

        // other: record this column's length, exit.
        self.builder.position_at_end(other_bb);
        let col = self
            .df_entry_load(entry, 2, "df.oh.col")
            .into_pointer_value();
        let h = self.column_len_field(col);
        self.builder
            .build_store(has_slot, i64_t.const_int(1, false))
            .unwrap();
        self.builder.build_store(h_slot, h).unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(next_bb);
        let i_next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "df.oh.inc")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        let has_i64 = self
            .builder
            .build_load(i64_t, has_slot, "df.oh.hasv")
            .unwrap()
            .into_int_value();
        let has = self
            .builder
            .build_int_compare(IntPredicate::NE, has_i64, i64_t.const_zero(), "df.oh.hasb")
            .unwrap();
        let height = self
            .builder
            .build_load(i64_t, h_slot, "df.oh.hv")
            .unwrap()
            .into_int_value();
        Ok((has, height))
    }

    /// Extract `(data_ptr, len)` from a compiled Kāra `String` value (a
    /// `{ptr, len, cap}` struct).
    fn df_string_parts(
        &mut self,
        expr: &Expr,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), String> {
        let v = self.compile_expr(expr)?;
        let s = v.into_struct_value();
        let data = self
            .builder
            .build_extract_value(s, 0, "df.name.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(s, 1, "df.name.len")
            .unwrap()
            .into_int_value();
        Ok((data, len))
    }

    /// A fresh frame-owned copy of `(name_data, name_len)` — malloc +
    /// memcpy, so the entry owns its name independent of the source
    /// String (which is freed by the caller's normal cleanup).
    fn df_copy_name(
        &self,
        name_data: PointerValue<'ctx>,
        name_len: IntValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, String> {
        let dst = self
            .builder
            .build_call(self.malloc_fn, &[name_len.into()], "df.name.copy")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(dst, 1, name_data, 1, name_len)
            .map_err(|e| format!("DataFrame name memcpy failed: {e:?}"))?;
        Ok(dst)
    }

    /// The `ColumnVarInfo` (element LLVM type + signedness) of a
    /// `Column[T]`-typed `insert` argument, from the lowering side-table
    /// keyed by the argument's span — the only per-column type residue the
    /// value-copy paths need (to size the data buffer, and to thread the
    /// element type into a `Column.from_vec` / `new` argument constructor
    /// via `pending_let_column_info`). The non-generic `DataFrame` can't
    /// carry it.
    fn dataframe_insert_col_info(
        &self,
        arg: &Expr,
    ) -> Result<super::state::ColumnVarInfo<'ctx>, String> {
        let key = (arg.span.offset, arg.span.length);
        let ci = self.column_typed_exprs.get(&key).ok_or_else(|| {
            "DataFrame.insert: column argument's element type is unknown \
             (no Column[T] side-table entry for the argument)"
                .to_string()
        })?;
        Ok(self.column_var_info_from_table(ci))
    }

    /// Register a `FreeDataFrame` cleanup for a DataFrame binding's slot.
    pub(super) fn track_dataframe_var(&mut self, df_alloca: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(super::state::CleanupAction::FreeDataFrame { df_alloca });
        }
    }

    // ── Method dispatch ─────────────────────────────────────────

    /// DataFrame instance methods on an identifier receiver. Returns
    /// `Ok(None)` for a non-DataFrame receiver / unhandled method (caller
    /// falls through).
    pub(super) fn try_compile_dataframe_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        _span: &Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let ExprKind::Identifier(var) = &object.kind else {
            return Ok(None);
        };
        if !self.dataframe_var_infos.contains(var.as_str()) {
            return Ok(None);
        }
        // `describe()` typechecks and runs under `karac run`, but the native
        // backend defers it: a `DataFrame` entry is type-erased (it carries
        // only `elem_size`, which is 8 for i64 / f64 / String alike), so the
        // build backend can't yet tell which columns are numeric — fail
        // loudly rather than miscompile. A follow-on slice adds a per-column
        // element-kind tag to the entry.
        if method == "describe" {
            return Err(
                "DataFrame.describe is not yet supported by the native backend \
                 (`karac build`); it works under `karac run` and lands in a \
                 follow-on codegen slice (needs a per-column element-kind tag)"
                    .to_string(),
            );
        }
        if !matches!(
            method,
            "insert" | "column" | "has_column" | "width" | "height" | "column_names" | "select"
        ) {
            return Ok(None);
        }
        let i64_t = self.context.i64_type();
        let control = self.dataframe_ptr_for_var(var)?;

        match method {
            "width" => {
                let len = self.df_load_field(control, 1, "df.width");
                Ok(Some(len))
            }
            "height" => Ok(Some(self.compile_dataframe_height(control)?)),
            "has_column" => {
                let (name_data, name_len) = self.df_string_parts(&args[0].value)?;
                let (found, _) = self.dataframe_find_index(control, name_data, name_len)?;
                // Widen i1 -> the bool repr the rest of codegen uses (i1).
                Ok(Some(found.into()))
            }
            "insert" => {
                // Element type from the col arg's static `Column[T]` type
                // (the lowering side-table, keyed by the arg span). Sizes
                // the data buffer AND threads into a `Column.from_vec` /
                // `new` argument constructor via `pending_let_column_info`
                // (which has no enclosing `let` to set it here).
                let info = self.dataframe_insert_col_info(&args[1].value)?;
                let elem_size_u = self.column_elem_size(info.elem)?;
                let elem_size = i64_t.const_int(elem_size_u, false);

                let (name_data, name_len) = self.df_string_parts(&args[0].value)?;
                let saved = self.pending_let_column_info.take();
                self.pending_let_column_info = Some(info);
                let col_val = self.compile_expr(&args[1].value)?;
                self.pending_let_column_info = saved;
                let src_col = col_val.into_pointer_value();
                let owned_col = self.column_deep_copy(src_col, elem_size)?;
                // The argument column was copied in (frame owns the copy).
                // Free a fresh-temp original (`Column.from_vec(..)`) — an
                // identifier source keeps its own scope cleanup, which
                // would double-free if we freed it here.
                if !matches!(args[1].value.kind, ExprKind::Identifier(_)) {
                    self.column_free_allocations(src_col);
                }

                self.dataframe_store_entry(control, name_data, name_len, owned_col, elem_size)?;
                Ok(Some(self.context.i8_type().const_zero().into()))
            }
            "column" => {
                let (name_data, name_len) = self.df_string_parts(&args[0].value)?;
                Ok(Some(
                    self.compile_dataframe_column(control, name_data, name_len)?,
                ))
            }
            "column_names" => Ok(Some(self.compile_dataframe_column_names(control)?)),
            "select" => Ok(Some(
                self.compile_dataframe_select(control, &args[0].value)?,
            )),
            _ => unreachable!(),
        }
    }

    /// Row count: 0 for an empty frame, else the first column's length.
    fn compile_dataframe_height(
        &mut self,
        control: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "DataFrame.height outside function".to_string())?;
        let i64_t = self.context.i64_type();
        let len = self.df_load_field(control, 1, "df.h.len").into_int_value();
        let entries = self
            .df_load_field(control, 0, "df.h.entries")
            .into_pointer_value();

        let nonempty = self
            .builder
            .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "df.h.ne")
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "df.h.some");
        let zero_bb = self.context.append_basic_block(fn_val, "df.h.zero");
        let merge_bb = self.context.append_basic_block(fn_val, "df.h.merge");
        self.builder
            .build_conditional_branch(nonempty, some_bb, zero_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let entry0 = self.df_entry_ptr(entries, i64_t.const_zero());
        let col0 = self
            .df_entry_load(entry0, 2, "df.h.col0")
            .into_pointer_value();
        let h = self.column_len_field(col0);
        let some_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(zero_bb);
        let zero_end = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        let phi = self.builder.build_phi(i64_t, "df.h").unwrap();
        phi.add_incoming(&[(&h, some_end), (&i64_t.const_zero(), zero_end)]);
        Ok(phi.as_basic_value())
    }

    /// `column_names() -> Vec[String]` — a fresh Vec of the entry names,
    /// in schema order. Each String element owns a fresh copy of its name
    /// (malloc + memcpy, `cap == len`), independent of the frame's name
    /// buffers — so the Vec's own drop frees the copies and never touches
    /// the frame.
    fn compile_dataframe_column_names(
        &mut self,
        control: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "DataFrame.column_names outside function".to_string())?;
        let i64_t = self.context.i64_type();
        let vec_st = self.vec_struct_type();
        let entries = self
            .df_load_field(control, 0, "df.cn.entries")
            .into_pointer_value();
        let len = self.df_load_field(control, 1, "df.cn.len").into_int_value();

        // Buffer of `len` String structs.
        let elem_size = vec_st.size_of().unwrap();
        let buf_bytes = self
            .builder
            .build_int_mul(len, elem_size, "df.cn.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[buf_bytes.into()], "df.cn.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let i_slot = self.create_entry_alloca(fn_val, "df.cn.i", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_zero())
            .unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "df.cn.cond");
        let body_bb = self.context.append_basic_block(fn_val, "df.cn.body");
        let after_bb = self.context.append_basic_block(fn_val, "df.cn.after");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "df.cn.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "df.cn.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, after_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let entry = self.df_entry_ptr(entries, i);
        let e_name = self
            .df_entry_load(entry, 0, "df.cn.ename")
            .into_pointer_value();
        let e_len = self.df_entry_load(entry, 1, "df.cn.elen").into_int_value();
        let name_copy = self.df_copy_name(e_name, e_len)?;
        let slot = unsafe {
            self.builder
                .build_gep(vec_st, buf, &[i], "df.cn.slot")
                .unwrap()
        };
        self.builder
            .build_store(
                self.builder
                    .build_struct_gep(vec_st, slot, 0, "df.cn.s.data")
                    .unwrap(),
                name_copy,
            )
            .unwrap();
        self.builder
            .build_store(
                self.builder
                    .build_struct_gep(vec_st, slot, 1, "df.cn.s.len")
                    .unwrap(),
                e_len,
            )
            .unwrap();
        self.builder
            .build_store(
                self.builder
                    .build_struct_gep(vec_st, slot, 2, "df.cn.s.cap")
                    .unwrap(),
                e_len,
            )
            .unwrap();
        let i_next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "df.cn.inc")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(after_bb);
        Ok(self.build_vec_value(buf, len, len))
    }

    /// `select(cols) -> DataFrame` — a fresh frame holding copies of the
    /// named columns, in the given order (subset / reorder). Iterates the
    /// `Vec[String]` argument; each name is looked up in the source (a
    /// missing name traps), its column deep-copied into the new frame
    /// (always appended — the interpreter allows a name more than once).
    /// The source frame is borrowed (unchanged).
    fn compile_dataframe_select(
        &mut self,
        src_control: PointerValue<'ctx>,
        cols_arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "DataFrame.select outside function".to_string())?;
        let i64_t = self.context.i64_type();
        let vec_st = self.vec_struct_type();

        let new_ctrl = self.compile_dataframe_new()?.into_pointer_value();

        // The `Vec[String]` of requested names.
        let cols_val = self.compile_expr(cols_arg)?;
        // The `select` dispatch returns early from `compile_method_call`
        // (before the generic owned-temp arg loop), so a *fresh-owned*
        // `Vec[String]` arg has no consuming binding and would leak its
        // buffer + element strings. The names are only *read* below (copied
        // in via `df_copy_name`), so route the arg through the same
        // `materialize_owned_temp` chokepoint the generic path uses: a
        // `FreeVecBuffer` (element-type-aware via `owned_temp_drops`,
        // `cap > 0`-guarded) drains it at scope exit. Two fresh forms:
        //   * a Vec literal — `df.select(["b", "a"])`; lowering canonicalizes
        //     a bare `[…]` / `Vec[…]` to `PrefixCollectionLiteral` (Vec),
        //     which `compile_vec_prefix_literal` mallocs a real heap buffer
        //     for (the 48-byte leak this closes);
        //   * a fresh call result — `df.select(other.column_names())`.
        // An identifier / field / index arg is an existing-binding alias
        // (its own cleanup frees it), so it is NOT matched here — freeing it
        // would double-free. Mirrors `insert`'s fresh-temp column free.
        let cols_is_fresh_owned = self.expr_yields_fresh_owned_temp(cols_arg)
            || matches!(&cols_arg.kind, ExprKind::PrefixCollectionLiteral { .. });
        if cols_is_fresh_owned && self.llvm_ty_is_vec_struct(cols_val.get_type()) {
            self.materialize_owned_temp(cols_val, (cols_arg.span.offset, cols_arg.span.length));
        }
        let cols_struct = cols_val.into_struct_value();
        let cols_data = self
            .builder
            .build_extract_value(cols_struct, 0, "df.sel.cdata")
            .unwrap()
            .into_pointer_value();
        let cols_len = self
            .builder
            .build_extract_value(cols_struct, 1, "df.sel.clen")
            .unwrap()
            .into_int_value();

        let j_slot = self.create_entry_alloca(fn_val, "df.sel.j", i64_t.into());
        self.builder
            .build_store(j_slot, i64_t.const_zero())
            .unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "df.sel.cond");
        let body_bb = self.context.append_basic_block(fn_val, "df.sel.body");
        let after_bb = self.context.append_basic_block(fn_val, "df.sel.after");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let j = self
            .builder
            .build_load(i64_t, j_slot, "df.sel.jv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, j, cols_len, "df.sel.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, after_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let name_slot = unsafe {
            self.builder
                .build_gep(vec_st, cols_data, &[j], "df.sel.nslot")
                .unwrap()
        };
        let name_data = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                self.builder
                    .build_struct_gep(vec_st, name_slot, 0, "df.sel.nd.p")
                    .unwrap(),
                "df.sel.ndata",
            )
            .unwrap()
            .into_pointer_value();
        let name_len = self
            .builder
            .build_load(
                i64_t,
                self.builder
                    .build_struct_gep(vec_st, name_slot, 1, "df.sel.nl.p")
                    .unwrap(),
                "df.sel.nlen",
            )
            .unwrap()
            .into_int_value();
        let (found, index) = self.dataframe_find_index(src_control, name_data, name_len)?;
        self.emit_column_guard(found, "DataFrame.select: no column with that name")?;
        let entries = self
            .df_load_field(src_control, 0, "df.sel.entries")
            .into_pointer_value();
        let entry = self.df_entry_ptr(entries, index);
        let col = self
            .df_entry_load(entry, 2, "df.sel.col")
            .into_pointer_value();
        let elem_size = self
            .df_entry_load(entry, 3, "df.sel.esize")
            .into_int_value();
        let owned = self.column_deep_copy(col, elem_size)?;
        self.dataframe_append_entry(new_ctrl, name_data, name_len, owned, elem_size)?;
        let j_next = self
            .builder
            .build_int_add(j, i64_t.const_int(1, false), "df.sel.inc")
            .unwrap();
        self.builder.build_store(j_slot, j_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(after_bb);
        // The `cols` Vec[String] is only read (names are copied in via
        // `df_copy_name`); it is not consumed. An identifier arg keeps its
        // own binding cleanup; a fresh-owned temp got a `FreeVecBuffer`
        // registered above (`materialize_owned_temp`). Either way it is
        // freed exactly once — never here (that would double-free).
        let _ = cols_struct;
        Ok(new_ctrl.into())
    }

    /// `column(name) -> Column[T]` — find the named column, deep-copy it
    /// out (value semantics), trap on a missing name.
    fn compile_dataframe_column(
        &mut self,
        control: PointerValue<'ctx>,
        name_data: PointerValue<'ctx>,
        name_len: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let (found, index) = self.dataframe_find_index(control, name_data, name_len)?;
        self.emit_column_guard(found, "DataFrame.column: no column with that name")?;
        let entries = self
            .df_load_field(control, 0, "df.col.entries")
            .into_pointer_value();
        let entry = self.df_entry_ptr(entries, index);
        let col = self
            .df_entry_load(entry, 2, "df.col.ctrl")
            .into_pointer_value();
        let elem_size = self
            .df_entry_load(entry, 3, "df.col.esize")
            .into_int_value();
        let copy = self.column_deep_copy(col, elem_size)?;
        Ok(copy.into())
    }

    /// Store a column into the frame under `name`: replace the existing
    /// same-named entry (freeing its old column) or append (growing the
    /// entries buffer, copying the name in). `owned_col` is already a
    /// fresh frame-owned column copy.
    fn dataframe_store_entry(
        &mut self,
        control: PointerValue<'ctx>,
        name_data: PointerValue<'ctx>,
        name_len: IntValue<'ctx>,
        owned_col: PointerValue<'ctx>,
        elem_size: IntValue<'ctx>,
    ) -> Result<(), String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "DataFrame.insert outside function".to_string())?;
        // Equal-length (Arrow) invariant: a new / replacement column must
        // match the table's row count, measured against any *other*
        // (different-named) existing column — replacing the sole column may
        // change the height (nothing else to disagree). A mismatch traps,
        // matching the interpreter.
        let new_len = self.column_len_field(owned_col);
        let (has_other, other_h) = self.dataframe_other_height(control, name_data, name_len)?;
        let len_eq = self
            .builder
            .build_int_compare(IntPredicate::EQ, new_len, other_h, "df.ins.leneq")
            .unwrap();
        let not_other = self.builder.build_not(has_other, "df.ins.noother").unwrap();
        let len_ok = self
            .builder
            .build_or(not_other, len_eq, "df.ins.lenok")
            .unwrap();
        self.emit_column_guard(
            len_ok,
            "DataFrame.insert: column length does not match the table's row count",
        )?;

        // Replace an existing same-named column, else append.
        let (found, index) = self.dataframe_find_index(control, name_data, name_len)?;
        let replace_bb = self.context.append_basic_block(fn_val, "df.ins.replace");
        let append_bb = self.context.append_basic_block(fn_val, "df.ins.append");
        let done_bb = self.context.append_basic_block(fn_val, "df.ins.done");
        self.builder
            .build_conditional_branch(found, replace_bb, append_bb)
            .unwrap();

        // Replace: free the old column, keep the name, store new col + size.
        self.builder.position_at_end(replace_bb);
        let entries = self
            .df_load_field(control, 0, "df.ins.r.entries")
            .into_pointer_value();
        let entry = self.df_entry_ptr(entries, index);
        let old_col = self
            .df_entry_load(entry, 2, "df.ins.r.oldcol")
            .into_pointer_value();
        self.column_free_allocations(old_col);
        self.builder
            .build_store(
                self.df_entry_field_slot(entry, 2, "df.ins.r.col"),
                owned_col,
            )
            .unwrap();
        self.builder
            .build_store(
                self.df_entry_field_slot(entry, 3, "df.ins.r.size"),
                elem_size,
            )
            .unwrap();
        self.builder.build_unconditional_branch(done_bb).unwrap();

        // Append a new column at the end.
        self.builder.position_at_end(append_bb);
        self.dataframe_append_entry(control, name_data, name_len, owned_col, elem_size)?;
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
        Ok(())
    }

    /// Append one column at the end (grow if needed, copy the name in,
    /// store the entry, len++). Used by `insert`'s append branch and by
    /// `select` (which always appends — the interpreter allows a name to
    /// appear more than once in the selection). Caller is positioned at a
    /// straight-line block (no branching here).
    fn dataframe_append_entry(
        &mut self,
        control: PointerValue<'ctx>,
        name_data: PointerValue<'ctx>,
        name_len: IntValue<'ctx>,
        owned_col: PointerValue<'ctx>,
        elem_size: IntValue<'ctx>,
    ) -> Result<(), String> {
        self.dataframe_ensure_capacity(control)?;
        let entries = self
            .df_load_field(control, 0, "df.ap.entries")
            .into_pointer_value();
        let len = self.df_load_field(control, 1, "df.ap.len").into_int_value();
        let entry = self.df_entry_ptr(entries, len);
        let name_copy = self.df_copy_name(name_data, name_len)?;
        self.builder
            .build_store(self.df_entry_field_slot(entry, 0, "df.ap.name"), name_copy)
            .unwrap();
        self.builder
            .build_store(self.df_entry_field_slot(entry, 1, "df.ap.nlen"), name_len)
            .unwrap();
        self.builder
            .build_store(self.df_entry_field_slot(entry, 2, "df.ap.col"), owned_col)
            .unwrap();
        self.builder
            .build_store(self.df_entry_field_slot(entry, 3, "df.ap.size"), elem_size)
            .unwrap();
        let i64_t = self.context.i64_type();
        let new_len = self
            .builder
            .build_int_add(len, i64_t.const_int(1, false), "df.ap.newlen")
            .unwrap();
        self.builder
            .build_store(self.df_field_slot(control, 1, "df.ap.lens"), new_len)
            .unwrap();
        Ok(())
    }

    /// Ensure `entries` has room for one more (len < cap). On growth,
    /// realloc to `max(4, cap*2)` entries (realloc preserves contents).
    fn dataframe_ensure_capacity(&mut self, control: PointerValue<'ctx>) -> Result<(), String> {
        let fn_val = self
            .current_fn
            .ok_or_else(|| "DataFrame grow outside function".to_string())?;
        let i64_t = self.context.i64_type();
        let len = self
            .df_load_field(control, 1, "df.cap.len")
            .into_int_value();
        let cap = self
            .df_load_field(control, 2, "df.cap.cap")
            .into_int_value();
        let full = self
            .builder
            .build_int_compare(IntPredicate::UGE, len, cap, "df.cap.full")
            .unwrap();
        let grow_bb = self.context.append_basic_block(fn_val, "df.cap.grow");
        let done_bb = self.context.append_basic_block(fn_val, "df.cap.done");
        self.builder
            .build_conditional_branch(full, grow_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(grow_bb);
        let doubled = self
            .builder
            .build_int_mul(cap, i64_t.const_int(2, false), "df.cap.dbl")
            .unwrap();
        let four = i64_t.const_int(4, false);
        let use_four = self
            .builder
            .build_int_compare(IntPredicate::ULT, doubled, four, "df.cap.lt4")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(use_four, four, doubled, "df.cap.new")
            .unwrap()
            .into_int_value();
        let entry_bytes = self.dataframe_entry_struct_type().size_of().unwrap();
        let new_bytes = self
            .builder
            .build_int_mul(new_cap, entry_bytes, "df.cap.bytes")
            .unwrap();
        let entries = self
            .df_load_field(control, 0, "df.cap.entries")
            .into_pointer_value();
        let realloc_fn = self.realloc_or_panic_fn_decl();
        let new_entries = self
            .builder
            .build_call(
                realloc_fn,
                &[entries.into(), new_bytes.into()],
                "df.cap.realloc",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_store(self.df_field_slot(control, 0, "df.cap.es"), new_entries)
            .unwrap();
        self.builder
            .build_store(self.df_field_slot(control, 2, "df.cap.cs"), new_cap)
            .unwrap();
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
        Ok(())
    }

    // ── Drop ────────────────────────────────────────────────────

    /// Free a DataFrame at scope exit: loop the entries freeing each
    /// column (data + bitmap + control) and name buffer, then the entries
    /// buffer and the control block. Null control = moved-out (skip).
    pub(super) fn emit_dataframe_free(
        &self,
        fn_val: FunctionValue<'ctx>,
        df_alloca: PointerValue<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());

        let control = self
            .builder
            .build_load(ptr_t, df_alloca, "df.drop.ctrl")
            .unwrap()
            .into_pointer_value();
        let live = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                control,
                ptr_t.const_null(),
                "df.drop.live",
            )
            .unwrap();
        let free_bb = self.context.append_basic_block(fn_val, "df.drop.free");
        let skip_bb = self.context.append_basic_block(fn_val, "df.drop.skip");
        self.builder
            .build_conditional_branch(live, free_bb, skip_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        let entries = self
            .df_load_field(control, 0, "df.drop.entries")
            .into_pointer_value();
        let len = self
            .df_load_field(control, 1, "df.drop.len")
            .into_int_value();

        let i_slot = self.create_entry_alloca(fn_val, "df.drop.i", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_zero())
            .unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "df.drop.cond");
        let body_bb = self.context.append_basic_block(fn_val, "df.drop.body");
        let after_bb = self.context.append_basic_block(fn_val, "df.drop.after");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "df.drop.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "df.drop.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, after_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let entry = self.df_entry_ptr(entries, i);
        let name = self
            .df_entry_load(entry, 0, "df.drop.name")
            .into_pointer_value();
        let col = self
            .df_entry_load(entry, 2, "df.drop.col")
            .into_pointer_value();
        self.column_free_allocations(col);
        self.builder
            .build_call(self.free_fn, &[name.into()], "")
            .unwrap();
        let i_next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "df.drop.inc")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(after_bb);
        // entries may be null (empty frame); free() of null is a no-op.
        self.builder
            .build_call(self.free_fn, &[entries.into()], "")
            .unwrap();
        self.builder
            .build_call(self.free_fn, &[control.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(skip_bb).unwrap();

        self.builder.position_at_end(skip_bb);
    }
}
