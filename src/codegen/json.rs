//! Codegen support for `std.json` compiled-binary dispatch.
//!
//! Phase-8 line 435 — the `Json.stringify()` codegen entry. Houses two
//! pieces of machinery:
//!
//! - `emit_json_kara_to_ffi_helper` — synthesises the private
//!   `__karac_json_kara_to_ffi(tag, w0, w1, w2) -> *mut KaracJsonValue`
//!   LLVM function. It dispatches on the variant tag of the Kāra-side
//!   `Json` enum (laid out as `{ i64 tag, i64 w0, i64 w1, i64 w2 }` per
//!   `declare_enums`) and emits per-variant calls into the runtime
//!   constructors declared in `Codegen::new`
//!   (`karac_runtime_json_make_*`). Array and Object arms walk the
//!   underlying `Vec[Json]` / `Vec[(String, Json)]` payload by element
//!   stride (32 and 56 bytes respectively) and recurse on each child via
//!   a direct self-call, so the whole walker fits in one LLVM function
//!   with no codegen-driven recursion.
//! - `compile_json_stringify` — invoked from `compile_method_call`'s
//!   Json dispatch arm. Loads the receiver's 4 enum words, hands them to
//!   `__karac_json_kara_to_ffi`, calls `karac_runtime_json_stringify` on
//!   the returned FFI tree, copies the resulting C-string into a fresh
//!   Kāra `String { ptr, len, cap }`, then frees both runtime
//!   allocations (the C-string and the FFI tree) before returning the
//!   Kāra String value.
//!
//! Variant tag mapping mirrors `runtime/stdlib/json.kara`:
//!   `Null=0, Bool=1, Number=2, String=3, Array=4, Object=5`.

use inkwell::module::Linkage;
use inkwell::values::{BasicValue, BasicValueEnum, FunctionValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use crate::ast::{Expr, ExprKind};

const JSON_TAG_NULL: u64 = 0;
const JSON_TAG_BOOL: u64 = 1;
const JSON_TAG_NUMBER: u64 = 2;
const JSON_TAG_STRING: u64 = 3;
const JSON_TAG_ARRAY: u64 = 4;
const JSON_TAG_OBJECT: u64 = 5;

/// Stride in bytes of the elements stored in `Vec[Json]`. A Json enum
/// occupies four i64 words (tag + 3 payload words — see
/// `declare_enums` for the dynamic max-word sizing that produces this).
const JSON_ELEM_STRIDE_BYTES: u64 = 32;

/// Stride in bytes of the elements stored in `Vec[(String, Json)]`. A
/// `(String, Json)` tuple is laid out as a struct of two i64-aligned
/// sub-aggregates: `String = {ptr, i64, i64}` (24 bytes) followed by
/// `Json = {i64, i64, i64, i64}` (32 bytes), with no padding because
/// both sides are 8-byte aligned. Pinned by
/// `tests/codegen.rs::test_e2e_json_stringify_object_two_keys`.
const JSON_OBJECT_PAIR_STRIDE_BYTES: u64 = 56;

const JSON_LOWER_HELPER: &str = "__karac_json_kara_to_ffi";
const JSON_LIFT_HELPER: &str = "__karac_json_ffi_to_kara";

/// Byte offsets within `KaracJsonValue` (`#[repr(C)]` layout pinned by
/// `runtime/src/lib.rs::tests::test_karac_json_value_layout_pinned`).
/// Hand-coded here because `inkwell` doesn't import the Rust struct
/// directly — codegen uses byte-stride GEPs against the raw pointer.
const KARAC_JSON_VALUE_TAG_OFFSET: u64 = 0;
const KARAC_JSON_VALUE_BOOL_OFFSET: u64 = 1;
const KARAC_JSON_VALUE_NUM_OFFSET: u64 = 8;
const KARAC_JSON_VALUE_STR_PTR_OFFSET: u64 = 16;
const KARAC_JSON_VALUE_STR_LEN_OFFSET: u64 = 24;
const KARAC_JSON_VALUE_ARR_ITEMS_OFFSET: u64 = 32;
const KARAC_JSON_VALUE_ARR_LEN_OFFSET: u64 = 40;
const KARAC_JSON_VALUE_OBJ_KEYS_OFFSET: u64 = 48;
const KARAC_JSON_VALUE_OBJ_VALS_OFFSET: u64 = 56;
const KARAC_JSON_VALUE_OBJ_LEN_OFFSET: u64 = 64;

/// Byte offsets within `KaracJsonError` (`#[repr(C)]` layout pinned by
/// `runtime/src/lib.rs::tests::test_karac_json_error_layout_pinned`).
const KARAC_JSON_ERROR_LINE_OFFSET: u64 = 0;
const KARAC_JSON_ERROR_COLUMN_OFFSET: u64 = 4;
const KARAC_JSON_ERROR_MESSAGE_OFFSET: u64 = 8;
const KARAC_JSON_ERROR_SIZE: u64 = 16;

impl<'ctx> super::Codegen<'ctx> {
    /// Recognise `Json.Variant(...)` expression shapes — the non-identifier
    /// receiver case for `Json.X(...).stringify()`. Matches a call whose
    /// callee is a `Json.<Variant>` path. Used by `compile_method_call`'s
    /// stringify dispatch to route directly to `compile_json_stringify`
    /// without requiring `var_type_names`.
    pub(super) fn expr_is_json_value(&self, expr: &Expr) -> bool {
        // 2-segment Path or Call(Path) — covers `Json.X(args)` and
        // `Json.X` shapes that the parser routes through Path.
        let path_segments = match &expr.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path { segments, .. } => Some(segments.as_slice()),
                _ => None,
            },
            ExprKind::Path { segments, .. } => Some(segments.as_slice()),
            _ => None,
        };
        if let Some(segments) = path_segments {
            if segments.len() == 2 && segments[0] == "Json" {
                return matches!(
                    segments[1].as_str(),
                    "Null" | "Bool" | "Number" | "String" | "Array" | "Object"
                );
            }
        }
        // Bare-variant FieldAccess shape: `Json.Null` parses as
        // `FieldAccess { object: Identifier("Json"), field: "Null" }`.
        // Matches all six variants, even though only the unit `Null`
        // form reaches this branch in practice (the others wear a Call
        // wrapper above).
        if let ExprKind::FieldAccess { object, field } = &expr.kind {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == "Json" {
                    return matches!(
                        field.as_str(),
                        "Null" | "Bool" | "Number" | "String" | "Array" | "Object"
                    );
                }
            }
        }
        false
    }

    /// Synthesize (or reuse) the module-private `__karac_json_kara_to_ffi`
    /// walker. Returns the FunctionValue ready for direct call sites.
    pub(super) fn emit_json_kara_to_ffi_helper(&mut self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(JSON_LOWER_HELPER) {
            return f;
        }

        let ctx = self.context;
        let i8_ty = ctx.i8_type();
        let i64_ty = ctx.i64_type();
        let f64_ty = ctx.f64_type();
        let ptr_ty = ctx.ptr_type(AddressSpace::default());

        let saved_bb = self.builder.get_insert_block();

        // fn(i64 tag, i64 w0, i64 w1, i64 w2) -> ptr
        let fn_ty = ptr_ty.fn_type(
            &[i64_ty.into(), i64_ty.into(), i64_ty.into(), i64_ty.into()],
            false,
        );
        let func = self
            .module
            .add_function(JSON_LOWER_HELPER, fn_ty, Some(Linkage::Internal));

        let entry_bb = ctx.append_basic_block(func, "entry");
        let null_bb = ctx.append_basic_block(func, "json.null");
        let bool_bb = ctx.append_basic_block(func, "json.bool");
        let number_bb = ctx.append_basic_block(func, "json.number");
        let string_bb = ctx.append_basic_block(func, "json.string");
        let array_entry_bb = ctx.append_basic_block(func, "json.array.entry");
        let array_loop_head_bb = ctx.append_basic_block(func, "json.array.head");
        let array_loop_body_bb = ctx.append_basic_block(func, "json.array.body");
        let array_finish_bb = ctx.append_basic_block(func, "json.array.finish");
        let object_entry_bb = ctx.append_basic_block(func, "json.object.entry");
        let object_loop_head_bb = ctx.append_basic_block(func, "json.object.head");
        let object_loop_body_bb = ctx.append_basic_block(func, "json.object.body");
        let object_finish_bb = ctx.append_basic_block(func, "json.object.finish");
        let default_bb = ctx.append_basic_block(func, "json.default");

        let tag = func.get_nth_param(0).unwrap().into_int_value();
        let w0 = func.get_nth_param(1).unwrap().into_int_value();
        let w1 = func.get_nth_param(2).unwrap().into_int_value();
        // w2 is reserved for the Vec capacity word in Array/Object cases;
        // we only need data (w0) and len (w1) for the stringify walk.
        let _w2 = func.get_nth_param(3).unwrap().into_int_value();

        // Entry: dispatch on tag.
        self.builder.position_at_end(entry_bb);
        let cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = vec![
            (i64_ty.const_int(JSON_TAG_NULL, false), null_bb),
            (i64_ty.const_int(JSON_TAG_BOOL, false), bool_bb),
            (i64_ty.const_int(JSON_TAG_NUMBER, false), number_bb),
            (i64_ty.const_int(JSON_TAG_STRING, false), string_bb),
            (i64_ty.const_int(JSON_TAG_ARRAY, false), array_entry_bb),
            (i64_ty.const_int(JSON_TAG_OBJECT, false), object_entry_bb),
        ];
        self.builder.build_switch(tag, default_bb, &cases).unwrap();

        // ── Null arm ────────────────────────────────────────────────
        self.builder.position_at_end(null_bb);
        let make_null = self
            .module
            .get_function("karac_runtime_json_make_null")
            .expect("declared in Codegen::new");
        let null_ret = self
            .builder
            .build_call(make_null, &[], "json.null.call")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&null_ret)).unwrap();

        // ── Bool arm ────────────────────────────────────────────────
        self.builder.position_at_end(bool_bb);
        let make_bool = self
            .module
            .get_function("karac_runtime_json_make_bool")
            .expect("declared in Codegen::new");
        // Kāra `bool` is stored as the i64 word; truncate to i8 for the
        // FFI signature (any non-zero byte is treated as true).
        let bool_i8 = self
            .builder
            .build_int_truncate(w0, i8_ty, "json.bool.trunc")
            .unwrap();
        let bool_ret = self
            .builder
            .build_call(make_bool, &[bool_i8.into()], "json.bool.call")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&bool_ret)).unwrap();

        // ── Number arm ──────────────────────────────────────────────
        self.builder.position_at_end(number_bb);
        let make_number = self
            .module
            .get_function("karac_runtime_json_make_number")
            .expect("declared in Codegen::new");
        // The f64 was stored as a bit-pattern in the i64 payload word.
        let num_f64 = self
            .builder
            .build_bit_cast(w0, f64_ty, "json.number.bitcast")
            .unwrap();
        let num_ret = self
            .builder
            .build_call(make_number, &[num_f64.into()], "json.number.call")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&num_ret)).unwrap();

        // ── String arm ──────────────────────────────────────────────
        self.builder.position_at_end(string_bb);
        let make_string = self
            .module
            .get_function("karac_runtime_json_make_string")
            .expect("declared in Codegen::new");
        // String payload words: w0 = data ptr (as i64), w1 = len, w2 = cap.
        let str_data = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "json.string.data")
            .unwrap();
        let str_ret = self
            .builder
            .build_call(
                make_string,
                &[str_data.into(), w1.into()],
                "json.string.call",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&str_ret)).unwrap();

        // ── Array arm ───────────────────────────────────────────────
        // Layout: w0 = Vec.data ptr, w1 = Vec.len, w2 = Vec.cap.
        // Each element is a 32-byte Json value (`{tag, w0, w1, w2}`).
        self.builder.position_at_end(array_entry_bb);
        let alloc_items = self
            .module
            .get_function("karac_runtime_json_alloc_items_buf")
            .expect("declared in Codegen::new");
        let items_buf = self
            .builder
            .build_call(alloc_items, &[w1.into()], "json.array.items")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let arr_data_ptr = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "json.array.data")
            .unwrap();
        // i = 0 alloca for the loop counter (LLVM phi would be cleaner but
        // alloca matches the rest of codegen's loop idiom and avoids the
        // builder-position dance for phis around recursive call sites).
        let i_slot = self
            .builder
            .build_alloca(i64_ty, "json.array.i.slot")
            .unwrap();
        self.builder
            .build_store(i_slot, i64_ty.const_zero())
            .unwrap();
        self.builder
            .build_unconditional_branch(array_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(array_loop_head_bb);
        let i_val = self
            .builder
            .build_load(i64_ty, i_slot, "json.array.i")
            .unwrap()
            .into_int_value();
        let done = self
            .builder
            .build_int_compare(IntPredicate::UGE, i_val, w1, "json.array.done")
            .unwrap();
        self.builder
            .build_conditional_branch(done, array_finish_bb, array_loop_body_bb)
            .unwrap();

        self.builder.position_at_end(array_loop_body_bb);
        let stride_arr = i64_ty.const_int(JSON_ELEM_STRIDE_BYTES, false);
        let elem_byte_off = self
            .builder
            .build_int_mul(i_val, stride_arr, "json.array.byte.off")
            .unwrap();
        let elem_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, arr_data_ptr, &[elem_byte_off], "json.array.elem.p")
                .unwrap()
        };
        let (c_tag, c_w0, c_w1, c_w2) =
            self.load_json_words(elem_addr, i64_ty, ptr_ty, "json.array.child");
        let child_ret = self
            .builder
            .build_call(
                func,
                &[c_tag.into(), c_w0.into(), c_w1.into(), c_w2.into()],
                "json.array.child.lower",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let slot_addr = unsafe {
            self.builder
                .build_in_bounds_gep(ptr_ty, items_buf, &[i_val], "json.array.slot")
                .unwrap()
        };
        self.builder.build_store(slot_addr, child_ret).unwrap();
        let i_next = self
            .builder
            .build_int_add(i_val, i64_ty.const_int(1, false), "json.array.i.next")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder
            .build_unconditional_branch(array_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(array_finish_bb);
        let make_array = self
            .module
            .get_function("karac_runtime_json_make_array")
            .expect("declared in Codegen::new");
        let arr_ret = self
            .builder
            .build_call(
                make_array,
                &[items_buf.into(), w1.into()],
                "json.array.call",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&arr_ret)).unwrap();

        // ── Object arm ──────────────────────────────────────────────
        // Layout: w0 = Vec.data ptr, w1 = Vec.len, w2 = Vec.cap.
        // Each element is a 56-byte (String, Json) pair:
        //   offset  0..24  String { data ptr (8) + len (8) + cap (8) }
        //   offset 24..56  Json   { tag (8) + w0..w2 (8 each) }
        self.builder.position_at_end(object_entry_bb);
        let alloc_keys = self
            .module
            .get_function("karac_runtime_json_alloc_keys_buf")
            .expect("declared in Codegen::new");
        let keys_buf = self
            .builder
            .build_call(alloc_keys, &[w1.into()], "json.object.keys")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let vals_buf = self
            .builder
            .build_call(alloc_items, &[w1.into()], "json.object.vals")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let obj_data_ptr = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "json.object.data")
            .unwrap();
        let oi_slot = self
            .builder
            .build_alloca(i64_ty, "json.object.i.slot")
            .unwrap();
        self.builder
            .build_store(oi_slot, i64_ty.const_zero())
            .unwrap();
        self.builder
            .build_unconditional_branch(object_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(object_loop_head_bb);
        let oi_val = self
            .builder
            .build_load(i64_ty, oi_slot, "json.object.i")
            .unwrap()
            .into_int_value();
        let odone = self
            .builder
            .build_int_compare(IntPredicate::UGE, oi_val, w1, "json.object.done")
            .unwrap();
        self.builder
            .build_conditional_branch(odone, object_finish_bb, object_loop_body_bb)
            .unwrap();

        self.builder.position_at_end(object_loop_body_bb);
        let pair_stride = i64_ty.const_int(JSON_OBJECT_PAIR_STRIDE_BYTES, false);
        let pair_byte_off = self
            .builder
            .build_int_mul(oi_val, pair_stride, "json.object.byte.off")
            .unwrap();
        let pair_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, obj_data_ptr, &[pair_byte_off], "json.object.pair.p")
                .unwrap()
        };
        // Key (String) — only the data ptr (offset 0) and len (offset 8)
        // are needed; cap is unused.
        let key_data_word = self
            .builder
            .build_load(i64_ty, pair_addr, "json.object.key.data.w")
            .unwrap()
            .into_int_value();
        let key_data_ptr = self
            .builder
            .build_int_to_ptr(key_data_word, ptr_ty, "json.object.key.data")
            .unwrap();
        let key_len_off = i64_ty.const_int(8, false);
        let key_len_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, pair_addr, &[key_len_off], "json.object.key.len.p")
                .unwrap()
        };
        let key_len = self
            .builder
            .build_load(i64_ty, key_len_addr, "json.object.key.len")
            .unwrap()
            .into_int_value();
        let alloc_key = self
            .module
            .get_function("karac_runtime_json_alloc_key")
            .expect("declared in Codegen::new");
        let key_cstr = self
            .builder
            .build_call(
                alloc_key,
                &[key_data_ptr.into(), key_len.into()],
                "json.object.key.cstr",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let key_slot_addr = unsafe {
            self.builder
                .build_in_bounds_gep(ptr_ty, keys_buf, &[oi_val], "json.object.key.slot")
                .unwrap()
        };
        self.builder.build_store(key_slot_addr, key_cstr).unwrap();

        // Value (Json) at offset 24 — load all four enum words and
        // recurse via `__karac_json_kara_to_ffi`.
        let val_byte_off = i64_ty.const_int(24, false);
        let val_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, pair_addr, &[val_byte_off], "json.object.val.p")
                .unwrap()
        };
        let (v_tag, v_w0, v_w1, v_w2) =
            self.load_json_words(val_addr, i64_ty, ptr_ty, "json.object.val");
        let val_ret = self
            .builder
            .build_call(
                func,
                &[v_tag.into(), v_w0.into(), v_w1.into(), v_w2.into()],
                "json.object.val.lower",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let val_slot_addr = unsafe {
            self.builder
                .build_in_bounds_gep(ptr_ty, vals_buf, &[oi_val], "json.object.val.slot")
                .unwrap()
        };
        self.builder.build_store(val_slot_addr, val_ret).unwrap();

        let oi_next = self
            .builder
            .build_int_add(oi_val, i64_ty.const_int(1, false), "json.object.i.next")
            .unwrap();
        self.builder.build_store(oi_slot, oi_next).unwrap();
        self.builder
            .build_unconditional_branch(object_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(object_finish_bb);
        let make_object = self
            .module
            .get_function("karac_runtime_json_make_object")
            .expect("declared in Codegen::new");
        let obj_ret = self
            .builder
            .build_call(
                make_object,
                &[keys_buf.into(), vals_buf.into(), w1.into()],
                "json.object.call",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&obj_ret)).unwrap();

        // ── Default arm — fall back to Null (defensive) ─────────────
        self.builder.position_at_end(default_bb);
        let fallback = self
            .builder
            .build_call(make_null, &[], "json.default.null")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&fallback)).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        func
    }

    /// Load four consecutive i64 words (tag, w0, w1, w2) starting at
    /// `base` into a tuple of IntValues. `base` is treated as an `i8*`
    /// for byte-stride GEPs; loads are typed as `i64`.
    fn load_json_words(
        &self,
        base: inkwell::values::PointerValue<'ctx>,
        i64_ty: inkwell::types::IntType<'ctx>,
        _ptr_ty: inkwell::types::PointerType<'ctx>,
        label: &str,
    ) -> (
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    ) {
        let i8_ty = self.context.i8_type();
        let mut words: [Option<inkwell::values::IntValue<'ctx>>; 4] = [None; 4];
        for (i, w) in words.iter_mut().enumerate() {
            let off = i64_ty.const_int((i as u64) * 8, false);
            let addr = unsafe {
                self.builder
                    .build_in_bounds_gep(i8_ty, base, &[off], &format!("{label}.w{i}.p"))
                    .unwrap()
            };
            let v = self
                .builder
                .build_load(i64_ty, addr, &format!("{label}.w{i}"))
                .unwrap()
                .into_int_value();
            *w = Some(v);
        }
        (
            words[0].unwrap(),
            words[1].unwrap(),
            words[2].unwrap(),
            words[3].unwrap(),
        )
    }

    /// Lower `j.stringify()` (and the non-identifier-receiver
    /// `Json.X(...).stringify()` shape) to:
    ///   1. four-word load of the Kāra `Json` enum value;
    ///   2. invocation of `__karac_json_kara_to_ffi` to produce a runtime
    ///      `*mut KaracJsonValue` tree;
    ///   3. `karac_runtime_json_stringify` → `*mut c_char`;
    ///   4. strlen + malloc + memcpy into a fresh Kāra `String { data,
    ///      len, cap }`;
    ///   5. `karac_runtime_json_free_string` + `karac_runtime_json_free_value`
    ///      on the runtime allocations.
    ///
    /// `receiver_val` is the loaded enum value as a 4-i64 struct.
    pub(super) fn compile_json_stringify(
        &mut self,
        receiver_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let lower_fn = self.emit_json_kara_to_ffi_helper();

        let i64_ty = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let struct_val = match receiver_val {
            BasicValueEnum::StructValue(sv) => sv,
            other => {
                return Err(format!(
                    "compile_json_stringify: receiver did not lower to a struct value (got {:?})",
                    other.get_type()
                ));
            }
        };
        let tag = self
            .builder
            .build_extract_value(struct_val, 0, "json.recv.tag")
            .unwrap()
            .into_int_value();
        let w0 = self
            .builder
            .build_extract_value(struct_val, 1, "json.recv.w0")
            .unwrap()
            .into_int_value();
        let w1 = self
            .builder
            .build_extract_value(struct_val, 2, "json.recv.w1")
            .unwrap()
            .into_int_value();
        let w2 = self
            .builder
            .build_extract_value(struct_val, 3, "json.recv.w2")
            .unwrap()
            .into_int_value();

        let ffi_ptr = self
            .builder
            .build_call(
                lower_fn,
                &[tag.into(), w0.into(), w1.into(), w2.into()],
                "json.lower",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let stringify_fn = self
            .module
            .get_function("karac_runtime_json_stringify")
            .expect("declared in Codegen::new");
        let cstr_ptr = self
            .builder
            .build_call(stringify_fn, &[ffi_ptr.into()], "json.cstr")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // strlen for the returned C string.
        let strlen_fn = self
            .module
            .get_function("strlen")
            .expect("declared in Codegen::new");
        let str_len = self
            .builder
            .build_call(strlen_fn, &[cstr_ptr.into()], "json.cstr.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let str_len_i64 = self
            .builder
            .build_int_z_extend_or_bit_cast(str_len, i64_ty, "json.cstr.len.i64")
            .unwrap();

        // Branch on len == 0 to skip the malloc+memcpy for the empty case.
        let fn_val = self
            .current_fn
            .ok_or_else(|| "Json.stringify lowered outside fn".to_string())?;
        let alloc_bb = self.context.append_basic_block(fn_val, "json.cstr.alloc");
        let empty_bb = self.context.append_basic_block(fn_val, "json.cstr.empty");
        let cont_bb = self.context.append_basic_block(fn_val, "json.cstr.cont");
        let buf_slot = self.create_entry_alloca(fn_val, "json.str.buf", ptr_ty.into());
        let zero = i64_ty.const_zero();
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, str_len_i64, zero, "json.cstr.is_empty")
            .unwrap();
        self.builder
            .build_conditional_branch(is_zero, empty_bb, alloc_bb)
            .unwrap();

        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(buf_slot, ptr_ty.const_null())
            .unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(alloc_bb);
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[str_len_i64.into()], "json.str.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 1, cstr_ptr, 1, str_len_i64)
            .unwrap();
        self.builder.build_store(buf_slot, buf).unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        self.builder.position_at_end(cont_bb);
        // Free the runtime-owned C string + FFI tree now that we've
        // copied out the bytes we need.
        let free_string_fn = self
            .module
            .get_function("karac_runtime_json_free_string")
            .expect("declared in Codegen::new");
        self.builder
            .build_call(free_string_fn, &[cstr_ptr.into()], "")
            .unwrap();
        let free_value_fn = self
            .module
            .get_function("karac_runtime_json_free_value")
            .expect("declared in Codegen::new");
        self.builder
            .build_call(free_value_fn, &[ffi_ptr.into()], "")
            .unwrap();

        let data = self
            .builder
            .build_load(ptr_ty, buf_slot, "json.str.data")
            .unwrap()
            .into_pointer_value();
        let str_ty = self.vec_struct_type();
        let mut str_val: BasicValueEnum<'ctx> = str_ty.get_undef().into();
        str_val = self
            .builder
            .build_insert_value(str_val.into_struct_value(), data, 0, "json.str.data.ins")
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(
                str_val.into_struct_value(),
                str_len_i64,
                1,
                "json.str.len.ins",
            )
            .unwrap()
            .into_struct_value()
            .into();
        str_val = self
            .builder
            .build_insert_value(
                str_val.into_struct_value(),
                str_len_i64,
                2,
                "json.str.cap.ins",
            )
            .unwrap()
            .into_struct_value()
            .into();
        Ok(str_val)
    }

    /// Synthesize (or reuse) the module-private `__karac_json_ffi_to_kara`
    /// walker — the inverse of `__karac_json_kara_to_ffi`. Takes a
    /// `*const KaracJsonValue` (heap tree allocated by
    /// `karac_runtime_json_parse`) and returns the corresponding Kāra
    /// `Json` enum value as a `{i64, i64, i64, i64}` struct. Recurses on
    /// Array / Object children. Phase-8 line 435 slice 2.
    ///
    /// String payloads are copied out (via `malloc` + `memcpy`) so the
    /// returned Kāra `String` owns its bytes — the caller can safely
    /// invoke `karac_runtime_json_free_value` on the FFI root immediately
    /// after this returns. Array / Object payload buffers are
    /// freshly-malloc'd Vec backing storage so Kāra-side `Vec.cap` lines
    /// up with the element count (no over-allocation).
    pub(super) fn emit_json_ffi_to_kara_helper(&mut self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(JSON_LIFT_HELPER) {
            return f;
        }

        let ctx = self.context;
        let i8_ty = ctx.i8_type();
        let i32_ty = ctx.i32_type();
        let i64_ty = ctx.i64_type();
        let f64_ty = ctx.f64_type();
        let ptr_ty = ctx.ptr_type(AddressSpace::default());
        let json_struct_ty = ctx.struct_type(
            &[i64_ty.into(), i64_ty.into(), i64_ty.into(), i64_ty.into()],
            false,
        );

        let saved_bb = self.builder.get_insert_block();

        // fn(*const KaracJsonValue) -> {i64, i64, i64, i64}
        let fn_ty = json_struct_ty.fn_type(&[ptr_ty.into()], false);
        let func = self
            .module
            .add_function(JSON_LIFT_HELPER, fn_ty, Some(Linkage::Internal));

        let entry_bb = ctx.append_basic_block(func, "entry");
        let null_in_bb = ctx.append_basic_block(func, "lift.null_input");
        let dispatch_bb = ctx.append_basic_block(func, "lift.dispatch");
        let null_bb = ctx.append_basic_block(func, "lift.null");
        let bool_bb = ctx.append_basic_block(func, "lift.bool");
        let number_bb = ctx.append_basic_block(func, "lift.number");
        let string_bb = ctx.append_basic_block(func, "lift.string");
        let array_entry_bb = ctx.append_basic_block(func, "lift.array.entry");
        let array_loop_head_bb = ctx.append_basic_block(func, "lift.array.head");
        let array_loop_body_bb = ctx.append_basic_block(func, "lift.array.body");
        let array_finish_bb = ctx.append_basic_block(func, "lift.array.finish");
        let object_entry_bb = ctx.append_basic_block(func, "lift.object.entry");
        let object_loop_head_bb = ctx.append_basic_block(func, "lift.object.head");
        let object_loop_body_bb = ctx.append_basic_block(func, "lift.object.body");
        let object_finish_bb = ctx.append_basic_block(func, "lift.object.finish");
        let default_bb = ctx.append_basic_block(func, "lift.default");

        let ffi_ptr = func.get_nth_param(0).unwrap().into_pointer_value();

        // Entry: guard against a null input (defensive — the runtime
        // contract is non-null on success, but a bug upstream would
        // otherwise dereference null below).
        self.builder.position_at_end(entry_bb);
        let is_null = self.builder.build_is_null(ffi_ptr, "lift.is_null").unwrap();
        self.builder
            .build_conditional_branch(is_null, null_in_bb, dispatch_bb)
            .unwrap();

        // Null-input fallback returns `Json.Null`.
        self.builder.position_at_end(null_in_bb);
        let null_for_null_input = json_struct_ty.const_zero();
        self.builder
            .build_return(Some(&null_for_null_input))
            .unwrap();

        // Dispatch on the tag byte at offset 0.
        self.builder.position_at_end(dispatch_bb);
        let tag_off = i64_ty.const_int(KARAC_JSON_VALUE_TAG_OFFSET, false);
        let tag_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[tag_off], "lift.tag.p")
                .unwrap()
        };
        let tag_u8 = self
            .builder
            .build_load(i8_ty, tag_addr, "lift.tag.u8")
            .unwrap()
            .into_int_value();
        let tag_i64 = self
            .builder
            .build_int_z_extend(tag_u8, i64_ty, "lift.tag.i64")
            .unwrap();
        let cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = vec![
            (i64_ty.const_int(JSON_TAG_NULL, false), null_bb),
            (i64_ty.const_int(JSON_TAG_BOOL, false), bool_bb),
            (i64_ty.const_int(JSON_TAG_NUMBER, false), number_bb),
            (i64_ty.const_int(JSON_TAG_STRING, false), string_bb),
            (i64_ty.const_int(JSON_TAG_ARRAY, false), array_entry_bb),
            (i64_ty.const_int(JSON_TAG_OBJECT, false), object_entry_bb),
        ];
        self.builder
            .build_switch(tag_i64, default_bb, &cases)
            .unwrap();

        // Per-variant helper: insert tag + up-to-3 payload words into a
        // {i64,i64,i64,i64} undef and return.
        let pack_and_return =
            |this: &Self,
             tag_const: u64,
             w0: Option<inkwell::values::IntValue<'ctx>>,
             w1: Option<inkwell::values::IntValue<'ctx>>,
             w2: Option<inkwell::values::IntValue<'ctx>>| {
                let mut agg = json_struct_ty.get_undef();
                agg = this
                    .builder
                    .build_insert_value(agg, i64_ty.const_int(tag_const, false), 0, "lift.tag.ins")
                    .unwrap()
                    .into_struct_value();
                let zero = i64_ty.const_zero();
                let w0v = w0.unwrap_or(zero);
                let w1v = w1.unwrap_or(zero);
                let w2v = w2.unwrap_or(zero);
                agg = this
                    .builder
                    .build_insert_value(agg, w0v, 1, "lift.w0.ins")
                    .unwrap()
                    .into_struct_value();
                agg = this
                    .builder
                    .build_insert_value(agg, w1v, 2, "lift.w1.ins")
                    .unwrap()
                    .into_struct_value();
                agg = this
                    .builder
                    .build_insert_value(agg, w2v, 3, "lift.w2.ins")
                    .unwrap()
                    .into_struct_value();
                this.builder.build_return(Some(&agg)).unwrap();
            };

        // ── Null arm ───────────────────────────────────────────────
        self.builder.position_at_end(null_bb);
        pack_and_return(self, JSON_TAG_NULL, None, None, None);

        // ── Bool arm ───────────────────────────────────────────────
        self.builder.position_at_end(bool_bb);
        let bool_off = i64_ty.const_int(KARAC_JSON_VALUE_BOOL_OFFSET, false);
        let bool_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[bool_off], "lift.bool.p")
                .unwrap()
        };
        let bool_u8 = self
            .builder
            .build_load(i8_ty, bool_addr, "lift.bool.u8")
            .unwrap()
            .into_int_value();
        let bool_i64 = self
            .builder
            .build_int_z_extend(bool_u8, i64_ty, "lift.bool.i64")
            .unwrap();
        pack_and_return(self, JSON_TAG_BOOL, Some(bool_i64), None, None);

        // ── Number arm ─────────────────────────────────────────────
        self.builder.position_at_end(number_bb);
        let num_off = i64_ty.const_int(KARAC_JSON_VALUE_NUM_OFFSET, false);
        let num_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[num_off], "lift.num.p")
                .unwrap()
        };
        let num_f64 = self
            .builder
            .build_load(f64_ty, num_addr, "lift.num.f64")
            .unwrap();
        let num_bits = self
            .builder
            .build_bit_cast(num_f64, i64_ty, "lift.num.bits")
            .unwrap()
            .into_int_value();
        pack_and_return(self, JSON_TAG_NUMBER, Some(num_bits), None, None);

        // ── String arm: copy bytes into a Kāra-owned buffer. ───────
        self.builder.position_at_end(string_bb);
        let str_ptr_off = i64_ty.const_int(KARAC_JSON_VALUE_STR_PTR_OFFSET, false);
        let str_ptr_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[str_ptr_off], "lift.str.ptr.p")
                .unwrap()
        };
        let str_src_ptr = self
            .builder
            .build_load(ptr_ty, str_ptr_addr, "lift.str.src.ptr")
            .unwrap()
            .into_pointer_value();
        let str_len_off = i64_ty.const_int(KARAC_JSON_VALUE_STR_LEN_OFFSET, false);
        let str_len_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[str_len_off], "lift.str.len.p")
                .unwrap()
        };
        let str_len_i64 = self
            .builder
            .build_load(i64_ty, str_len_addr, "lift.str.len")
            .unwrap()
            .into_int_value();
        // Empty-string fast path: avoid the zero-byte malloc.
        let str_is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                str_len_i64,
                i64_ty.const_zero(),
                "lift.str.is_empty",
            )
            .unwrap();
        let str_alloc_bb = ctx.append_basic_block(func, "lift.str.alloc");
        let str_empty_bb = ctx.append_basic_block(func, "lift.str.empty");
        let str_finish_bb = ctx.append_basic_block(func, "lift.str.finish");
        self.builder
            .build_conditional_branch(str_is_empty, str_empty_bb, str_alloc_bb)
            .unwrap();

        self.builder.position_at_end(str_empty_bb);
        let empty_ptr_int = i64_ty.const_zero();
        self.builder
            .build_unconditional_branch(str_finish_bb)
            .unwrap();

        self.builder.position_at_end(str_alloc_bb);
        let str_buf = self
            .builder
            .build_call(self.malloc_fn, &[str_len_i64.into()], "lift.str.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(str_buf, 1, str_src_ptr, 1, str_len_i64)
            .unwrap();
        let alloc_ptr_int = self
            .builder
            .build_ptr_to_int(str_buf, i64_ty, "lift.str.buf.i64")
            .unwrap();
        let str_alloc_end = self.builder.get_insert_block().unwrap();
        self.builder
            .build_unconditional_branch(str_finish_bb)
            .unwrap();

        self.builder.position_at_end(str_finish_bb);
        let str_data_phi = self.builder.build_phi(i64_ty, "lift.str.data.phi").unwrap();
        str_data_phi.add_incoming(&[
            (&empty_ptr_int, str_empty_bb),
            (&alloc_ptr_int, str_alloc_end),
        ]);
        let str_data_word = str_data_phi.as_basic_value().into_int_value();
        pack_and_return(
            self,
            JSON_TAG_STRING,
            Some(str_data_word),
            Some(str_len_i64),
            Some(str_len_i64),
        );

        // ── Array arm: walk arr_items[0..arr_len], recurse, pack into
        // a Vec[Json]-backing buffer (32-byte stride). ─────────────
        self.builder.position_at_end(array_entry_bb);
        let arr_items_off = i64_ty.const_int(KARAC_JSON_VALUE_ARR_ITEMS_OFFSET, false);
        let arr_items_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[arr_items_off], "lift.arr.items.p")
                .unwrap()
        };
        let arr_items_ptr = self
            .builder
            .build_load(ptr_ty, arr_items_addr, "lift.arr.items")
            .unwrap()
            .into_pointer_value();
        let arr_len_off = i64_ty.const_int(KARAC_JSON_VALUE_ARR_LEN_OFFSET, false);
        let arr_len_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[arr_len_off], "lift.arr.len.p")
                .unwrap()
        };
        let arr_len_i64 = self
            .builder
            .build_load(i64_ty, arr_len_addr, "lift.arr.len")
            .unwrap()
            .into_int_value();
        // Allocate `arr_len * 32` bytes for the Kāra Vec[Json] buffer.
        // Zero-length arrays still bypass the malloc call to avoid
        // pinging the allocator with a zero-byte request.
        let arr_stride_bytes = i64_ty.const_int(JSON_ELEM_STRIDE_BYTES, false);
        let arr_buf_bytes = self
            .builder
            .build_int_mul(arr_len_i64, arr_stride_bytes, "lift.arr.buf.bytes")
            .unwrap();
        let arr_is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                arr_len_i64,
                i64_ty.const_zero(),
                "lift.arr.is_empty",
            )
            .unwrap();
        let arr_alloc_bb = ctx.append_basic_block(func, "lift.arr.alloc");
        let arr_empty_bb = ctx.append_basic_block(func, "lift.arr.empty");
        let arr_post_bb = ctx.append_basic_block(func, "lift.arr.post");
        self.builder
            .build_conditional_branch(arr_is_empty, arr_empty_bb, arr_alloc_bb)
            .unwrap();

        self.builder.position_at_end(arr_empty_bb);
        let arr_empty_ptr_int = i64_ty.const_zero();
        self.builder
            .build_unconditional_branch(arr_post_bb)
            .unwrap();

        self.builder.position_at_end(arr_alloc_bb);
        let arr_buf_ptr = self
            .builder
            .build_call(self.malloc_fn, &[arr_buf_bytes.into()], "lift.arr.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let arr_alloc_ptr_int = self
            .builder
            .build_ptr_to_int(arr_buf_ptr, i64_ty, "lift.arr.buf.i64")
            .unwrap();
        self.builder
            .build_unconditional_branch(arr_post_bb)
            .unwrap();

        self.builder.position_at_end(arr_post_bb);
        let arr_data_phi = self.builder.build_phi(i64_ty, "lift.arr.data.phi").unwrap();
        arr_data_phi.add_incoming(&[
            (&arr_empty_ptr_int, arr_empty_bb),
            (&arr_alloc_ptr_int, arr_alloc_bb),
        ]);
        let arr_data_word = arr_data_phi.as_basic_value().into_int_value();
        // Cast back to a pointer for the GEP-by-index inside the loop.
        let arr_buf_ptr_back = self
            .builder
            .build_int_to_ptr(arr_data_word, ptr_ty, "lift.arr.buf.p")
            .unwrap();
        let arr_i_slot = self
            .builder
            .build_alloca(i64_ty, "lift.arr.i.slot")
            .unwrap();
        self.builder
            .build_store(arr_i_slot, i64_ty.const_zero())
            .unwrap();
        self.builder
            .build_unconditional_branch(array_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(array_loop_head_bb);
        let arr_i = self
            .builder
            .build_load(i64_ty, arr_i_slot, "lift.arr.i")
            .unwrap()
            .into_int_value();
        let arr_done = self
            .builder
            .build_int_compare(IntPredicate::UGE, arr_i, arr_len_i64, "lift.arr.done")
            .unwrap();
        self.builder
            .build_conditional_branch(arr_done, array_finish_bb, array_loop_body_bb)
            .unwrap();

        self.builder.position_at_end(array_loop_body_bb);
        // Load arr_items[i] (an FFI child pointer).
        let arr_child_slot_addr = unsafe {
            self.builder
                .build_in_bounds_gep(ptr_ty, arr_items_ptr, &[arr_i], "lift.arr.child.slot.p")
                .unwrap()
        };
        let arr_child_ffi = self
            .builder
            .build_load(ptr_ty, arr_child_slot_addr, "lift.arr.child.ffi")
            .unwrap()
            .into_pointer_value();
        // Recurse into self.
        let arr_child_kara = self
            .builder
            .build_call(func, &[arr_child_ffi.into()], "lift.arr.child.kara")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        // Store the 4-word child Json at arr_buf_ptr_back[i*32..i*32+32].
        let arr_byte_off = self
            .builder
            .build_int_mul(arr_i, arr_stride_bytes, "lift.arr.byte.off")
            .unwrap();
        let arr_dst_base = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, arr_buf_ptr_back, &[arr_byte_off], "lift.arr.dst.p")
                .unwrap()
        };
        for w_idx in 0..4u32 {
            let word_val = self
                .builder
                .build_extract_value(arr_child_kara, w_idx, &format!("lift.arr.child.w{w_idx}"))
                .unwrap()
                .into_int_value();
            let word_off = i64_ty.const_int((w_idx as u64) * 8, false);
            let word_dst = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        i8_ty,
                        arr_dst_base,
                        &[word_off],
                        &format!("lift.arr.dst.w{w_idx}.p"),
                    )
                    .unwrap()
            };
            self.builder.build_store(word_dst, word_val).unwrap();
        }
        let arr_i_next = self
            .builder
            .build_int_add(arr_i, i64_ty.const_int(1, false), "lift.arr.i.next")
            .unwrap();
        self.builder.build_store(arr_i_slot, arr_i_next).unwrap();
        self.builder
            .build_unconditional_branch(array_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(array_finish_bb);
        // Pack the Kāra Vec[Json] = {data_int, len, cap} into Json.Array
        // payload words (w0=data, w1=len, w2=cap=len).
        pack_and_return(
            self,
            JSON_TAG_ARRAY,
            Some(arr_data_word),
            Some(arr_len_i64),
            Some(arr_len_i64),
        );

        // ── Object arm: walk obj_keys + obj_vals in parallel, build
        // a Vec[(String, Json)]-backing buffer (56-byte stride). ───
        self.builder.position_at_end(object_entry_bb);
        let obj_keys_off = i64_ty.const_int(KARAC_JSON_VALUE_OBJ_KEYS_OFFSET, false);
        let obj_keys_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[obj_keys_off], "lift.obj.keys.p")
                .unwrap()
        };
        let obj_keys_ptr = self
            .builder
            .build_load(ptr_ty, obj_keys_addr, "lift.obj.keys")
            .unwrap()
            .into_pointer_value();
        let obj_vals_off = i64_ty.const_int(KARAC_JSON_VALUE_OBJ_VALS_OFFSET, false);
        let obj_vals_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[obj_vals_off], "lift.obj.vals.p")
                .unwrap()
        };
        let obj_vals_ptr = self
            .builder
            .build_load(ptr_ty, obj_vals_addr, "lift.obj.vals")
            .unwrap()
            .into_pointer_value();
        let obj_len_off = i64_ty.const_int(KARAC_JSON_VALUE_OBJ_LEN_OFFSET, false);
        let obj_len_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, ffi_ptr, &[obj_len_off], "lift.obj.len.p")
                .unwrap()
        };
        let obj_len_i64 = self
            .builder
            .build_load(i64_ty, obj_len_addr, "lift.obj.len")
            .unwrap()
            .into_int_value();
        let obj_stride_bytes = i64_ty.const_int(JSON_OBJECT_PAIR_STRIDE_BYTES, false);
        let obj_buf_bytes = self
            .builder
            .build_int_mul(obj_len_i64, obj_stride_bytes, "lift.obj.buf.bytes")
            .unwrap();
        let obj_is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                obj_len_i64,
                i64_ty.const_zero(),
                "lift.obj.is_empty",
            )
            .unwrap();
        let obj_alloc_bb = ctx.append_basic_block(func, "lift.obj.alloc");
        let obj_empty_bb = ctx.append_basic_block(func, "lift.obj.empty");
        let obj_post_bb = ctx.append_basic_block(func, "lift.obj.post");
        self.builder
            .build_conditional_branch(obj_is_empty, obj_empty_bb, obj_alloc_bb)
            .unwrap();

        self.builder.position_at_end(obj_empty_bb);
        let obj_empty_ptr_int = i64_ty.const_zero();
        self.builder
            .build_unconditional_branch(obj_post_bb)
            .unwrap();

        self.builder.position_at_end(obj_alloc_bb);
        let obj_buf_ptr = self
            .builder
            .build_call(self.malloc_fn, &[obj_buf_bytes.into()], "lift.obj.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let obj_alloc_ptr_int = self
            .builder
            .build_ptr_to_int(obj_buf_ptr, i64_ty, "lift.obj.buf.i64")
            .unwrap();
        self.builder
            .build_unconditional_branch(obj_post_bb)
            .unwrap();

        self.builder.position_at_end(obj_post_bb);
        let obj_data_phi = self.builder.build_phi(i64_ty, "lift.obj.data.phi").unwrap();
        obj_data_phi.add_incoming(&[
            (&obj_empty_ptr_int, obj_empty_bb),
            (&obj_alloc_ptr_int, obj_alloc_bb),
        ]);
        let obj_data_word = obj_data_phi.as_basic_value().into_int_value();
        let obj_buf_ptr_back = self
            .builder
            .build_int_to_ptr(obj_data_word, ptr_ty, "lift.obj.buf.p")
            .unwrap();
        let obj_i_slot = self
            .builder
            .build_alloca(i64_ty, "lift.obj.i.slot")
            .unwrap();
        self.builder
            .build_store(obj_i_slot, i64_ty.const_zero())
            .unwrap();
        self.builder
            .build_unconditional_branch(object_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(object_loop_head_bb);
        let obj_i = self
            .builder
            .build_load(i64_ty, obj_i_slot, "lift.obj.i")
            .unwrap()
            .into_int_value();
        let obj_done = self
            .builder
            .build_int_compare(IntPredicate::UGE, obj_i, obj_len_i64, "lift.obj.done")
            .unwrap();
        self.builder
            .build_conditional_branch(obj_done, object_finish_bb, object_loop_body_bb)
            .unwrap();

        self.builder.position_at_end(object_loop_body_bb);
        // Per-entry destination base: obj_buf_ptr_back + i*56.
        let pair_byte_off = self
            .builder
            .build_int_mul(obj_i, obj_stride_bytes, "lift.obj.byte.off")
            .unwrap();
        let pair_dst_base = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, obj_buf_ptr_back, &[pair_byte_off], "lift.obj.dst.p")
                .unwrap()
        };
        // ── Key (CString → Kāra String) ───────────────────────
        let key_cstr_slot_addr = unsafe {
            self.builder
                .build_in_bounds_gep(ptr_ty, obj_keys_ptr, &[obj_i], "lift.obj.key.slot.p")
                .unwrap()
        };
        let key_cstr = self
            .builder
            .build_load(ptr_ty, key_cstr_slot_addr, "lift.obj.key.cstr")
            .unwrap()
            .into_pointer_value();
        let strlen_fn = self
            .module
            .get_function("strlen")
            .expect("declared in Codegen::new");
        let key_len_usize = self
            .builder
            .build_call(strlen_fn, &[key_cstr.into()], "lift.obj.key.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let key_len_i64 = self
            .builder
            .build_int_z_extend_or_bit_cast(key_len_usize, i64_ty, "lift.obj.key.len.i64")
            .unwrap();
        // Branch on key_len == 0 to skip the malloc+memcpy.
        let key_is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                key_len_i64,
                i64_ty.const_zero(),
                "lift.obj.key.is_empty",
            )
            .unwrap();
        let key_alloc_bb = ctx.append_basic_block(func, "lift.obj.key.alloc");
        let key_empty_bb = ctx.append_basic_block(func, "lift.obj.key.empty");
        let key_finish_bb = ctx.append_basic_block(func, "lift.obj.key.finish");
        self.builder
            .build_conditional_branch(key_is_empty, key_empty_bb, key_alloc_bb)
            .unwrap();

        self.builder.position_at_end(key_empty_bb);
        let key_empty_ptr_int = i64_ty.const_zero();
        self.builder
            .build_unconditional_branch(key_finish_bb)
            .unwrap();

        self.builder.position_at_end(key_alloc_bb);
        let key_buf = self
            .builder
            .build_call(self.malloc_fn, &[key_len_i64.into()], "lift.obj.key.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(key_buf, 1, key_cstr, 1, key_len_i64)
            .unwrap();
        let key_alloc_int = self
            .builder
            .build_ptr_to_int(key_buf, i64_ty, "lift.obj.key.buf.i64")
            .unwrap();
        self.builder
            .build_unconditional_branch(key_finish_bb)
            .unwrap();

        self.builder.position_at_end(key_finish_bb);
        let key_data_phi = self
            .builder
            .build_phi(i64_ty, "lift.obj.key.data.phi")
            .unwrap();
        key_data_phi.add_incoming(&[
            (&key_empty_ptr_int, key_empty_bb),
            (&key_alloc_int, key_alloc_bb),
        ]);
        let key_data_word = key_data_phi.as_basic_value().into_int_value();
        // Write the String triple at pair_dst_base[0..24]:
        //   offset  0: data
        //   offset  8: len
        //   offset 16: cap (= len; the malloc'd buffer is sized exactly).
        for (slot_off, word_val) in [
            (0u64, key_data_word),
            (8u64, key_len_i64),
            (16u64, key_len_i64),
        ] {
            let word_off = i64_ty.const_int(slot_off, false);
            let word_dst = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        i8_ty,
                        pair_dst_base,
                        &[word_off],
                        &format!("lift.obj.key.dst.{slot_off}.p"),
                    )
                    .unwrap()
            };
            self.builder.build_store(word_dst, word_val).unwrap();
        }
        // ── Value (recurse) ────────────────────────────────────
        let val_slot_addr = unsafe {
            self.builder
                .build_in_bounds_gep(ptr_ty, obj_vals_ptr, &[obj_i], "lift.obj.val.slot.p")
                .unwrap()
        };
        let val_ffi = self
            .builder
            .build_load(ptr_ty, val_slot_addr, "lift.obj.val.ffi")
            .unwrap()
            .into_pointer_value();
        let val_kara = self
            .builder
            .build_call(func, &[val_ffi.into()], "lift.obj.val.kara")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        // Write the 4 Json words at pair_dst_base[24..56].
        for w_idx in 0..4u32 {
            let word_val = self
                .builder
                .build_extract_value(val_kara, w_idx, &format!("lift.obj.val.w{w_idx}"))
                .unwrap()
                .into_int_value();
            let word_off = i64_ty.const_int(24 + (w_idx as u64) * 8, false);
            let word_dst = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        i8_ty,
                        pair_dst_base,
                        &[word_off],
                        &format!("lift.obj.val.dst.w{w_idx}.p"),
                    )
                    .unwrap()
            };
            self.builder.build_store(word_dst, word_val).unwrap();
        }
        let obj_i_next = self
            .builder
            .build_int_add(obj_i, i64_ty.const_int(1, false), "lift.obj.i.next")
            .unwrap();
        self.builder.build_store(obj_i_slot, obj_i_next).unwrap();
        self.builder
            .build_unconditional_branch(object_loop_head_bb)
            .unwrap();

        self.builder.position_at_end(object_finish_bb);
        pack_and_return(
            self,
            JSON_TAG_OBJECT,
            Some(obj_data_word),
            Some(obj_len_i64),
            Some(obj_len_i64),
        );

        // ── Default arm (unknown tag) — fall back to Null. ─────────
        self.builder.position_at_end(default_bb);
        pack_and_return(self, JSON_TAG_NULL, None, None, None);

        // Suppress unused-warning on the held i32 type — the lift body
        // doesn't need it directly (line/col are read in `compile_json_parse`).
        let _ = i32_ty;

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        func
    }

    /// Lower `Json.parse(s)` to:
    ///   1. Build a null-terminated C-string copy of the input `s`
    ///      (`malloc(len+1)` + memcpy + zero terminator).
    ///   2. Allocate a `KaracJsonError` on the function-entry stack.
    ///   3. Call `karac_runtime_json_parse(cstr, &error)`.
    ///   4. Free the cstr copy (the runtime copies the input internally).
    ///   5. Branch on null return:
    ///      - non-null → walk the FFI tree via `__karac_json_ffi_to_kara`,
    ///        free the FFI tree, wrap the four resulting words in
    ///        `Result.Ok(<Json>)`;
    ///      - null     → read line/column/message from the error slot,
    ///        copy the message into a Kāra `String`, free the runtime's
    ///        C-string copy, wrap `JsonError { line, column, message }`
    ///        in `Result.Err(...)`.
    ///   6. Phi-merge both arms into a single 5-i64 Result struct value.
    ///
    /// The synthesized walker is shared module-wide: subsequent
    /// `Json.parse(...)` calls reuse the same function (Codegen-side memo
    /// via `module.get_function`).
    pub(super) fn compile_json_parse(
        &mut self,
        input_str_val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let lift_fn = self.emit_json_ffi_to_kara_helper();

        let ctx = self.context;
        let i8_ty = ctx.i8_type();
        let i64_ty = ctx.i64_type();
        let ptr_ty = ctx.ptr_type(AddressSpace::default());

        let fn_val = self
            .current_fn
            .ok_or_else(|| "Json.parse lowered outside fn".to_string())?;

        // Extract {data, len, cap} from the Kāra String.
        let str_sv = match input_str_val {
            BasicValueEnum::StructValue(sv) => sv,
            other => {
                return Err(format!(
                    "compile_json_parse: input did not lower to a String struct value (got {:?})",
                    other.get_type()
                ));
            }
        };
        let in_data = self
            .builder
            .build_extract_value(str_sv, 0, "parse.in.data")
            .unwrap()
            .into_pointer_value();
        let in_len = self
            .builder
            .build_extract_value(str_sv, 1, "parse.in.len")
            .unwrap()
            .into_int_value();

        // Allocate `in_len + 1` bytes, memcpy, write the trailing NUL.
        let one = i64_ty.const_int(1, false);
        let cstr_size = self
            .builder
            .build_int_add(in_len, one, "parse.cstr.size")
            .unwrap();
        let cstr_buf = self
            .builder
            .build_call(self.malloc_fn, &[cstr_size.into()], "parse.cstr.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // memcpy only when len > 0 (memcpy(_,_,0) is fine on any libc,
        // but the input data pointer might be null for empty strings).
        let in_is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                in_len,
                i64_ty.const_zero(),
                "parse.in.is_empty",
            )
            .unwrap();
        let copy_bb = ctx.append_basic_block(fn_val, "parse.cstr.copy");
        let skip_copy_bb = ctx.append_basic_block(fn_val, "parse.cstr.skip");
        self.builder
            .build_conditional_branch(in_is_empty, skip_copy_bb, copy_bb)
            .unwrap();
        self.builder.position_at_end(copy_bb);
        self.builder
            .build_memcpy(cstr_buf, 1, in_data, 1, in_len)
            .unwrap();
        self.builder
            .build_unconditional_branch(skip_copy_bb)
            .unwrap();
        self.builder.position_at_end(skip_copy_bb);
        // Write the NUL terminator at cstr_buf[in_len].
        let nul_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, cstr_buf, &[in_len], "parse.cstr.nul.p")
                .unwrap()
        };
        self.builder
            .build_store(nul_addr, i8_ty.const_int(0, false))
            .unwrap();

        // Stack-alloca the KaracJsonError (16 bytes). The byte slab is
        // raw — we GEP into it at the pinned offsets below.
        let err_slot = self.create_entry_alloca(
            fn_val,
            "parse.err.slot",
            i8_ty.array_type(KARAC_JSON_ERROR_SIZE as u32).into(),
        );

        let parse_fn = self
            .module
            .get_function("karac_runtime_json_parse")
            .expect("declared in Codegen::new");
        let ffi_root = self
            .builder
            .build_call(
                parse_fn,
                &[cstr_buf.into(), err_slot.into()],
                "parse.ffi.root",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Free the cstr copy regardless of success or failure — the
        // runtime parses by-value and doesn't retain a reference.
        let free_fn = self.module.get_function("free").unwrap_or_else(|| {
            let free_ty = ctx.void_type().fn_type(&[ptr_ty.into()], false);
            self.module
                .add_function("free", free_ty, Some(Linkage::External))
        });
        self.builder
            .build_call(free_fn, &[cstr_buf.into()], "")
            .unwrap();

        // Branch on null FFI root → Err arm; non-null → Ok arm.
        let is_null = self
            .builder
            .build_is_null(ffi_root, "parse.ffi.is_null")
            .unwrap();
        let ok_bb = ctx.append_basic_block(fn_val, "parse.ok");
        let err_bb = ctx.append_basic_block(fn_val, "parse.err");
        let cont_bb = ctx.append_basic_block(fn_val, "parse.cont");
        self.builder
            .build_conditional_branch(is_null, err_bb, ok_bb)
            .unwrap();

        // Look up the Result enum layout (widened to {i64*5} by
        // `seed_builtin_enum_layouts`).
        let result_ty = self
            .enum_layouts
            .get("Result")
            .expect("Result layout seeded before Json.parse dispatch")
            .llvm_type;

        // ── Ok arm: lift the FFI tree, free it, pack into Result.Ok ──
        self.builder.position_at_end(ok_bb);
        let lifted = self
            .builder
            .build_call(lift_fn, &[ffi_root.into()], "parse.lift")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        let free_value_fn = self
            .module
            .get_function("karac_runtime_json_free_value")
            .expect("declared in Codegen::new");
        self.builder
            .build_call(free_value_fn, &[ffi_root.into()], "")
            .unwrap();
        // Result.Ok tag = 1; payload words 0..3 hold the Json struct's
        // four i64 fields.
        let mut ok_agg = result_ty.get_undef();
        ok_agg = self
            .builder
            .build_insert_value(ok_agg, i64_ty.const_int(1, false), 0, "parse.ok.tag.ins")
            .unwrap()
            .into_struct_value();
        for field_idx in 0..4u32 {
            let word = self
                .builder
                .build_extract_value(lifted, field_idx, &format!("parse.ok.w{field_idx}"))
                .unwrap()
                .into_int_value();
            ok_agg = self
                .builder
                .build_insert_value(
                    ok_agg,
                    word,
                    field_idx + 1,
                    &format!("parse.ok.w{field_idx}.ins"),
                )
                .unwrap()
                .into_struct_value();
        }
        let ok_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Err arm: read KaracJsonError, copy message into Kāra
        // String, free the FFI message, pack into Result.Err ─────────
        self.builder.position_at_end(err_bb);
        let line_off = i64_ty.const_int(KARAC_JSON_ERROR_LINE_OFFSET, false);
        let line_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, err_slot, &[line_off], "parse.err.line.p")
                .unwrap()
        };
        let line_i32 = self
            .builder
            .build_load(ctx.i32_type(), line_addr, "parse.err.line")
            .unwrap()
            .into_int_value();
        let line_i64 = self
            .builder
            .build_int_z_extend(line_i32, i64_ty, "parse.err.line.i64")
            .unwrap();
        let col_off = i64_ty.const_int(KARAC_JSON_ERROR_COLUMN_OFFSET, false);
        let col_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, err_slot, &[col_off], "parse.err.col.p")
                .unwrap()
        };
        let col_i32 = self
            .builder
            .build_load(ctx.i32_type(), col_addr, "parse.err.col")
            .unwrap()
            .into_int_value();
        let col_i64 = self
            .builder
            .build_int_z_extend(col_i32, i64_ty, "parse.err.col.i64")
            .unwrap();
        let msg_off = i64_ty.const_int(KARAC_JSON_ERROR_MESSAGE_OFFSET, false);
        let msg_addr = unsafe {
            self.builder
                .build_in_bounds_gep(i8_ty, err_slot, &[msg_off], "parse.err.msg.p")
                .unwrap()
        };
        let msg_cstr = self
            .builder
            .build_load(ptr_ty, msg_addr, "parse.err.msg.cstr")
            .unwrap()
            .into_pointer_value();
        // The message may be null on edge cases (defensive — the runtime
        // currently always populates it on error, but a future tweak
        // could return null). Use strlen on non-null only.
        let msg_is_null = self
            .builder
            .build_is_null(msg_cstr, "parse.err.msg.is_null")
            .unwrap();
        let msg_with_bb = ctx.append_basic_block(fn_val, "parse.err.msg.with");
        let msg_null_bb = ctx.append_basic_block(fn_val, "parse.err.msg.null");
        let msg_finish_bb = ctx.append_basic_block(fn_val, "parse.err.msg.finish");
        self.builder
            .build_conditional_branch(msg_is_null, msg_null_bb, msg_with_bb)
            .unwrap();

        self.builder.position_at_end(msg_null_bb);
        let msg_null_ptr_int = i64_ty.const_zero();
        let msg_null_len = i64_ty.const_zero();
        self.builder
            .build_unconditional_branch(msg_finish_bb)
            .unwrap();

        self.builder.position_at_end(msg_with_bb);
        let strlen_fn = self
            .module
            .get_function("strlen")
            .expect("declared in Codegen::new");
        let msg_len_usize = self
            .builder
            .build_call(strlen_fn, &[msg_cstr.into()], "parse.err.msg.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let msg_len_i64 = self
            .builder
            .build_int_z_extend_or_bit_cast(msg_len_usize, i64_ty, "parse.err.msg.len.i64")
            .unwrap();
        let msg_is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                msg_len_i64,
                i64_ty.const_zero(),
                "parse.err.msg.is_empty",
            )
            .unwrap();
        let msg_alloc_bb = ctx.append_basic_block(fn_val, "parse.err.msg.alloc");
        let msg_empty_bb = ctx.append_basic_block(fn_val, "parse.err.msg.empty");
        let msg_with_finish_bb = ctx.append_basic_block(fn_val, "parse.err.msg.with.finish");
        self.builder
            .build_conditional_branch(msg_is_empty, msg_empty_bb, msg_alloc_bb)
            .unwrap();

        self.builder.position_at_end(msg_empty_bb);
        let msg_empty_ptr_int = i64_ty.const_zero();
        self.builder
            .build_unconditional_branch(msg_with_finish_bb)
            .unwrap();

        self.builder.position_at_end(msg_alloc_bb);
        let msg_buf = self
            .builder
            .build_call(self.malloc_fn, &[msg_len_i64.into()], "parse.err.msg.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(msg_buf, 1, msg_cstr, 1, msg_len_i64)
            .unwrap();
        let msg_alloc_int = self
            .builder
            .build_ptr_to_int(msg_buf, i64_ty, "parse.err.msg.buf.i64")
            .unwrap();
        self.builder
            .build_unconditional_branch(msg_with_finish_bb)
            .unwrap();

        self.builder.position_at_end(msg_with_finish_bb);
        let msg_with_ptr_phi = self
            .builder
            .build_phi(i64_ty, "parse.err.msg.with.ptr.phi")
            .unwrap();
        msg_with_ptr_phi.add_incoming(&[
            (&msg_empty_ptr_int, msg_empty_bb),
            (&msg_alloc_int, msg_alloc_bb),
        ]);
        let msg_with_ptr_int = msg_with_ptr_phi.as_basic_value().into_int_value();
        self.builder
            .build_unconditional_branch(msg_finish_bb)
            .unwrap();

        self.builder.position_at_end(msg_finish_bb);
        let msg_ptr_phi = self
            .builder
            .build_phi(i64_ty, "parse.err.msg.ptr.phi")
            .unwrap();
        msg_ptr_phi.add_incoming(&[
            (&msg_null_ptr_int, msg_null_bb),
            (&msg_with_ptr_int, msg_with_finish_bb),
        ]);
        let msg_len_phi = self
            .builder
            .build_phi(i64_ty, "parse.err.msg.len.phi")
            .unwrap();
        msg_len_phi.add_incoming(&[
            (&msg_null_len, msg_null_bb),
            (&msg_len_i64, msg_with_finish_bb),
        ]);
        let msg_ptr_word = msg_ptr_phi.as_basic_value().into_int_value();
        let msg_len_word = msg_len_phi.as_basic_value().into_int_value();

        // Free the runtime-owned message string now that we copied it.
        let free_string_fn = self
            .module
            .get_function("karac_runtime_json_free_string")
            .expect("declared in Codegen::new");
        self.builder
            .build_call(free_string_fn, &[msg_cstr.into()], "")
            .unwrap();

        // Build Result.Err(JsonError { line, column, message }) by
        // packing into the widened {i64*5} struct. Word layout:
        //   w0 = line (zext from u32)
        //   w1 = column (zext from u32)
        //   w2 = message data ptr (as i64)
        //   w3 = message len
        //   (cap is truncated — see Result widening comment in
        //   declarations.rs).
        let mut err_agg = result_ty.get_undef();
        err_agg = self
            .builder
            .build_insert_value(err_agg, i64_ty.const_int(0, false), 0, "parse.err.tag.ins")
            .unwrap()
            .into_struct_value();
        for (field_idx, word) in [
            (1u32, line_i64),
            (2u32, col_i64),
            (3u32, msg_ptr_word),
            (4u32, msg_len_word),
        ] {
            err_agg = self
                .builder
                .build_insert_value(err_agg, word, field_idx, "parse.err.w.ins")
                .unwrap()
                .into_struct_value();
        }
        let err_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(cont_bb).unwrap();

        // ── Merge both arms ─────────────────────────────────────────
        self.builder.position_at_end(cont_bb);
        let phi = self
            .builder
            .build_phi(result_ty, "parse.result.phi")
            .unwrap();
        phi.add_incoming(&[
            (&ok_agg.as_basic_value_enum(), ok_end_bb),
            (&err_agg.as_basic_value_enum(), err_end_bb),
        ]);
        Ok(phi.as_basic_value())
    }
}
