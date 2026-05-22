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
use inkwell::values::{BasicValueEnum, FunctionValue};
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
}
