//! Vec method dispatch + sort closure thunks.
//!
//! Houses `compile_vec_method` (the big per-Vec-method dispatch
//! covering `push`, `pop`, `len`, `is_empty`, `clear`, `iter`, `sort`,
//! `sort_by`, `sort_by_key`, slicing, indexing, etc.) plus the
//! sort-closure thunk emitters `emit_sort_by_inline_thunk` and
//! `emit_sort_by_thunk` that produce stable C-compatible
//! `int (*)(const void*, const void*)` adapters for the libc `qsort`
//! runtime.

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue,
};
use inkwell::AddressSpace;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_vec_method(
        &mut self,
        var_name: &str,
        data_ptr: PointerValue<'ctx>,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let elem_ty = self.vec_elem_type_for_var(var_name);

        match method {
            "len" => {
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let len = self.builder.build_load(i64_t, len_ptr, "vec.len").unwrap();
                Ok(len)
            }
            // `String.starts_with(prefix: String) -> bool`. The typechecker
            // arm in `stdlib_seq.rs::infer_str_method` accepts this only on
            // `Type::Str` receivers, but the codegen lives here because
            // Strings share the `{ptr, len, cap}` shape with `Vec[T]` and
            // route through `compile_vec_method` for `.len()` and friends.
            // Implementation: load `recv.len`, evaluate the prefix String,
            // extract `prefix.len`; short-circuit to `false` when
            // `recv.len < prefix.len`; otherwise `memcmp(recv.data,
            // prefix.data, prefix.len) == 0`. Uses the same `self.memcmp_fn`
            // declared in `Codegen::new` that `compile_string_binop` uses
            // for the `==` operator.
            "starts_with" => {
                if args.is_empty() {
                    return Err("String.starts_with requires a prefix argument".to_string());
                }
                let bool_t = self.context.bool_type();
                let i32_t = self.context.i32_type();

                // Receiver: load data ptr + len from {ptr, len, cap}.
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "sw.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "sw.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "sw.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "sw.recv.len")
                    .unwrap()
                    .into_int_value();

                // Prefix: evaluate the arg; expect a String struct value.
                let prefix_val = self.compile_expr(&args[0].value)?;
                let prefix_struct = prefix_val.into_struct_value();
                let prefix_data = self
                    .builder
                    .build_extract_value(prefix_struct, 0, "sw.prefix.ptr")
                    .unwrap()
                    .into_pointer_value();
                let prefix_len = self
                    .builder
                    .build_extract_value(prefix_struct, 1, "sw.prefix.len")
                    .unwrap()
                    .into_int_value();

                // recv_len >= prefix_len?
                let has_len = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::UGE,
                        recv_len,
                        prefix_len,
                        "sw.has_len",
                    )
                    .unwrap();

                let fn_val = self.current_fn.unwrap();
                let cmp_bb = self.context.append_basic_block(fn_val, "sw.cmp");
                let cont_bb = self.context.append_basic_block(fn_val, "sw.cont");

                // Result slot: i1, default false (taken when has_len is false).
                let result_slot = self.create_entry_alloca(fn_val, "sw.result", bool_t.into());
                self.builder
                    .build_store(result_slot, bool_t.const_zero())
                    .unwrap();
                self.builder
                    .build_conditional_branch(has_len, cmp_bb, cont_bb)
                    .unwrap();

                // memcmp(recv.data, prefix.data, prefix.len) — compare the
                // first prefix.len bytes. memcmp returns 0 iff equal.
                self.builder.position_at_end(cmp_bb);
                let cmp_result = self
                    .builder
                    .build_call(
                        self.memcmp_fn,
                        &[recv_data.into(), prefix_data.into(), prefix_len.into()],
                        "sw.memcmp",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let is_eq = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        cmp_result,
                        i32_t.const_zero(),
                        "sw.eq",
                    )
                    .unwrap();
                self.builder.build_store(result_slot, is_eq).unwrap();
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                self.builder.position_at_end(cont_bb);
                let result = self
                    .builder
                    .build_load(bool_t, result_slot, "sw.load")
                    .unwrap();
                Ok(result)
            }
            // `String.substring(start: i64) -> String`. Returns a fresh
            // owned String of the receiver's bytes from byte offset
            // `start` to the end. Out-of-range / negative starts
            // saturate to an empty String (route-prefix-friendly).
            //
            // Implementation:
            //   1. Load receiver `{data, len}`.
            //   2. Evaluate `start`. If `start < 0 || start >= len`,
            //      produce an empty String `{null, 0, 0}`.
            //   3. Otherwise allocate `len - start` bytes via malloc,
            //      memcpy from `data + start`, and assemble the result
            //      String `{buf, len-start, len-start}`. cap == len so
            //      the freshly-malloc'd buffer is freed at scope exit
            //      (mirrors `compile_request_string_method`'s pattern).
            "substring" => {
                if args.is_empty() {
                    return Err("String.substring requires a start index argument".to_string());
                }
                let str_ty = self.vec_struct_type();

                // Receiver: load `{data, len}`.
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "ss.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "ss.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "ss.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "ss.recv.len")
                    .unwrap()
                    .into_int_value();

                // Evaluate the start index (must be i64).
                let start_val = self.compile_expr(&args[0].value)?;
                let start = start_val.into_int_value();

                // out_of_range = (start < 0) || (start >= len)
                let zero64 = i64_t.const_zero();
                let lt_zero = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, start, zero64, "ss.lt_zero")
                    .unwrap();
                let ge_len = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGE, start, recv_len, "ss.ge_len")
                    .unwrap();
                let out_of_range = self.builder.build_or(lt_zero, ge_len, "ss.oor").unwrap();

                let fn_val = self.current_fn.unwrap();
                let copy_bb = self.context.append_basic_block(fn_val, "ss.copy");
                let empty_bb = self.context.append_basic_block(fn_val, "ss.empty");
                let cont_bb = self.context.append_basic_block(fn_val, "ss.cont");

                // Result slot for the assembled String aggregate.
                let result_slot = self.create_entry_alloca(fn_val, "ss.result", str_ty.into());
                self.builder
                    .build_conditional_branch(out_of_range, empty_bb, copy_bb)
                    .unwrap();

                // Empty branch: store {null, 0, 0}.
                self.builder.position_at_end(empty_bb);
                let null = ptr_ty.const_null();
                let mut empty_agg = str_ty.get_undef();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, null, 0, "ss.empty.ptr")
                    .unwrap()
                    .into_struct_value();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, zero64, 1, "ss.empty.len")
                    .unwrap()
                    .into_struct_value();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, zero64, 2, "ss.empty.cap")
                    .unwrap()
                    .into_struct_value();
                self.builder.build_store(result_slot, empty_agg).unwrap();
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                // Copy branch: malloc + memcpy from data+start.
                self.builder.position_at_end(copy_bb);
                let new_len = self
                    .builder
                    .build_int_nsw_sub(recv_len, start, "ss.new_len")
                    .unwrap();
                let buf = self
                    .builder
                    .build_call(self.malloc_fn, &[new_len.into()], "ss.buf")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // src = recv_data + start (byte-stride GEP via i8).
                let src = unsafe {
                    self.builder
                        .build_gep(self.context.i8_type(), recv_data, &[start], "ss.src")
                        .unwrap()
                };
                self.builder.build_memcpy(buf, 1, src, 1, new_len).unwrap();
                let mut copy_agg = str_ty.get_undef();
                copy_agg = self
                    .builder
                    .build_insert_value(copy_agg, buf, 0, "ss.copy.ptr")
                    .unwrap()
                    .into_struct_value();
                copy_agg = self
                    .builder
                    .build_insert_value(copy_agg, new_len, 1, "ss.copy.len")
                    .unwrap()
                    .into_struct_value();
                copy_agg = self
                    .builder
                    .build_insert_value(copy_agg, new_len, 2, "ss.copy.cap")
                    .unwrap()
                    .into_struct_value();
                self.builder.build_store(result_slot, copy_agg).unwrap();
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                self.builder.position_at_end(cont_bb);
                let result = self
                    .builder
                    .build_load(str_ty, result_slot, "ss.load")
                    .unwrap();
                Ok(result)
            }
            // String.push(char): same {ptr,len,cap} layout as Vec but the
            // arg is a Unicode scalar that needs UTF-8 encoding before the
            // append. Routed here based on `string_vars` membership — the
            // disambiguator between String and Vec[u8], which share the
            // i8 element width but differ semantically on iteration and
            // method surface. Surfaced 2026-05-25 by
            // kata-katas/leetcode/71-simplify-path; the existing
            // `out = f"{out}{c}"` self-append was O(n²) per call. This
            // arm gives the natural `out.push(c)` a 1–4-byte memcpy + an
            // amortized power-of-two growth, matching `push_str` and
            // analog of Rust's `String::push`. The encoding shape reuses
            // `emit_codepoint_to_utf8` (already in use by print /
            // f-string lowering, runtime.rs § Codepoint utilities).
            "push" if self.string_vars.contains(var_name) => {
                if args.is_empty() {
                    return Err("String.push requires a Char argument".to_string());
                }
                let cp_val = self.compile_expr(&args[0].value)?;
                let cp = cp_val.into_int_value();
                let (enc_buf, enc_len) = self.emit_codepoint_to_utf8(cp);

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "spush.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "spush.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "spush.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "spush.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "spush.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "spush.cap")
                    .unwrap()
                    .into_int_value();

                // Required capacity = len + enc_len. enc_len ∈ [1,4]; the
                // grow path doubles capacity so amortized cost is O(1)
                // per push despite the byte-level memcpy.
                let new_len = self
                    .builder
                    .build_int_add(len, enc_len, "spush.new_len")
                    .unwrap();
                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "spush.grow");
                let copy_bb = self.context.append_basic_block(fn_val, "spush.copy");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, cap, "spush.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, copy_bb)
                    .unwrap();

                // Grow: new_cap = max(new_len, max(4, cap * 2)) — same
                // geometry as `push_str`.
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self
                    .builder
                    .build_int_mul(cap, two, "spush.doubled")
                    .unwrap();
                let cmp1 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "spush.cmp1")
                    .unwrap();
                let growth_min = self
                    .builder
                    .build_select(cmp1, doubled, four, "spush.growth_min")
                    .unwrap()
                    .into_int_value();
                let cmp2 = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::UGT,
                        new_len,
                        growth_min,
                        "spush.cmp2",
                    )
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp2, new_len, growth_min, "spush.new_cap")
                    .unwrap()
                    .into_int_value();
                let new_data = self
                    .builder
                    .build_call(self.malloc_fn, &[new_cap.into()], "spush.new_data")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // Copy old data (`len` bytes).
                self.builder
                    .build_memcpy(new_data, 1, data, 1, len)
                    .unwrap();
                // Free old heap buffer if any (`cap > 0` guard mirrors
                // push_str — static-literal Strings have cap == 0 and
                // their ptr is in the read-only string pool).
                let zero_val = i64_t.const_int(0, false);
                let was_heap = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, cap, zero_val, "spush.was_heap")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "spush.free");
                let after_free_bb = self.context.append_basic_block(fn_val, "spush.after_free");
                self.builder
                    .build_conditional_branch(was_heap, free_bb, after_free_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();
                self.builder
                    .build_unconditional_branch(after_free_bb)
                    .unwrap();
                self.builder.position_at_end(after_free_bb);

                self.builder.build_store(data_ptr_ptr, new_data).unwrap();
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(copy_bb).unwrap();

                // Copy encoded bytes (1–4) into data + len.
                self.builder.position_at_end(copy_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "spush.cur_data")
                    .unwrap()
                    .into_pointer_value();
                let cur_len = self
                    .builder
                    .build_load(i64_t, len_ptr, "spush.cur_len")
                    .unwrap()
                    .into_int_value();
                let dest = unsafe {
                    self.builder
                        .build_gep(self.context.i8_type(), cur_data, &[cur_len], "spush.dest")
                        .unwrap()
                };
                self.builder
                    .build_memcpy(dest, 1, enc_buf, 1, enc_len)
                    .unwrap();
                let updated_len = self
                    .builder
                    .build_int_add(cur_len, enc_len, "spush.updated_len")
                    .unwrap();
                self.builder.build_store(len_ptr, updated_len).unwrap();

                Ok(i64_t.const_int(0, false).into())
            }
            // VecDeque codegen alias: `push_back` is identical to Vec
            // `push` (append at index `len`); the VecDeque interpreter
            // ship at `4227e21` documented this front/back-shared
            // storage shape, and codegen mirrors it.
            "push" | "push_back" => {
                if args.is_empty() {
                    return Err("Vec.push requires an argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;
                // Move semantics: when the argument is a tracked Vec /
                // String binding, push bit-copies its `{ptr, len, cap}`
                // into the container's data buffer. Both source and
                // container now alias the same heap pointer; the source's
                // scope-exit `FreeVecBuffer` and the container's
                // recursive-drop pass would both free it (double-free
                // → macOS `mfm_free.cold.4` spin / abort). Zero the
                // source's `cap` so its cleanup's `cap > 0` guard skips
                // — the container becomes the unique owner.
                self.suppress_source_vec_cleanup_for_arg(&args[0].value);

                // Load current vec fields.
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "vec.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "cap")
                    .unwrap()
                    .into_int_value();

                // Growth check: if len == cap, grow.
                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "push.grow");
                let store_bb = self.context.append_basic_block(fn_val, "push.store");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, store_bb)
                    .unwrap();

                // Grow path: new_cap = max(4, cap * 2); malloc; memcpy; free old.
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self.builder.build_int_mul(cap, two, "doubled").unwrap();
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "cmp")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp, doubled, four, "new_cap")
                    .unwrap()
                    .into_int_value();

                // Compute byte size: new_cap * sizeof(elem)
                let elem_size = elem_ty.size_of().unwrap();
                let alloc_bytes = self
                    .builder
                    .build_int_mul(new_cap, elem_size, "alloc_bytes")
                    .unwrap();
                let new_data = self
                    .builder
                    .build_call(self.malloc_fn, &[alloc_bytes.into()], "new_data")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();

                // memcpy old data if non-null.
                let old_bytes = self
                    .builder
                    .build_int_mul(len, elem_size, "old_bytes")
                    .unwrap();
                self.builder
                    .build_memcpy(new_data, 8, data, 8, old_bytes)
                    .unwrap();

                // Free old buffer (free(null) is a no-op per C spec).
                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();

                // Update vec fields.
                self.builder.build_store(data_ptr_ptr, new_data).unwrap();
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(store_bb).unwrap();

                // Store element at data[len].
                self.builder.position_at_end(store_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "cur_data")
                    .unwrap()
                    .into_pointer_value();
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, cur_data, &[len], "elem.ptr")
                        .unwrap()
                };
                self.builder.build_store(elem_ptr, elem_val).unwrap();

                // Increment len.
                let one = i64_t.const_int(1, false);
                let new_len = self.builder.build_int_add(len, one, "new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // VecDeque codegen — `push_front` inserts at index 0,
            // shifting all existing elements right by 1. The
            // interpreter ship at `4227e21` translates to
            // `Vec::insert(0, …)`; codegen does the same via an
            // `llvm.memmove` over `len * sizeof(elem)` bytes from
            // `data` to `data + sizeof(elem)`. Growth path is
            // identical to `push` (max(4, cap * 2)).
            "push_front" => {
                if args.is_empty() {
                    return Err("VecDeque.push_front requires an argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vd.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vd.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "vd.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "cap")
                    .unwrap()
                    .into_int_value();

                // Growth check: if len == cap, grow (same shape as push).
                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "pushf.grow");
                let shift_bb = self.context.append_basic_block(fn_val, "pushf.shift");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, shift_bb)
                    .unwrap();

                // Grow: new_cap = max(4, cap * 2); malloc; memcpy old; free old.
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self.builder.build_int_mul(cap, two, "doubled").unwrap();
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "cmp")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp, doubled, four, "new_cap")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();
                let alloc_bytes = self
                    .builder
                    .build_int_mul(new_cap, elem_size, "alloc_bytes")
                    .unwrap();
                let new_data = self
                    .builder
                    .build_call(self.malloc_fn, &[alloc_bytes.into()], "new_data")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let old_bytes = self
                    .builder
                    .build_int_mul(len, elem_size, "old_bytes")
                    .unwrap();
                self.builder
                    .build_memcpy(new_data, 8, data, 8, old_bytes)
                    .unwrap();
                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();
                self.builder.build_store(data_ptr_ptr, new_data).unwrap();
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(shift_bb).unwrap();

                // Shift existing [0..len) elements right by 1 — memmove
                // (overlapping ranges, so memmove not memcpy). Then
                // store the new element at index 0 and increment len.
                self.builder.position_at_end(shift_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "cur_data")
                    .unwrap()
                    .into_pointer_value();
                let shifted_dst = unsafe {
                    self.builder
                        .build_gep(elem_ty, cur_data, &[i64_t.const_int(1, false)], "shift.dst")
                        .unwrap()
                };
                let shift_bytes = self
                    .builder
                    .build_int_mul(len, elem_size, "shift_bytes")
                    .unwrap();
                self.builder
                    .build_memmove(shifted_dst, 8, cur_data, 8, shift_bytes)
                    .unwrap();
                self.builder.build_store(cur_data, elem_val).unwrap();
                let one = i64_t.const_int(1, false);
                let new_len = self.builder.build_int_add(len, one, "new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `Vec.remove(idx) -> T` — remove the element at `idx`,
            // shift the tail down by one, return the removed value.
            // Mirrors the `pop_front` shape (load + memmove + len--)
            // but at an arbitrary index. v1 matches Rust's contract:
            // out-of-bounds idx is UB — no bounds check, no graceful
            // Option. Callers ensure idx < len.
            "remove" => {
                if args.is_empty() {
                    return Err("Vec.remove requires an index argument".to_string());
                }
                let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "remove.len.ptr")
                    .unwrap();
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "remove.data.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "remove.len")
                    .unwrap()
                    .into_int_value();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "remove.data")
                    .unwrap()
                    .into_pointer_value();
                let one = i64_t.const_int(1, false);

                // Load the element being removed (becomes the return value).
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[idx_val], "remove.elem.ptr")
                        .unwrap()
                };
                let elem_val = self
                    .builder
                    .build_load(elem_ty, elem_ptr, "remove.elem")
                    .unwrap();

                // memmove(data + idx, data + idx + 1, (len - 1 - idx) * sizeof(elem))
                let new_len = self
                    .builder
                    .build_int_sub(len, one, "remove.new_len")
                    .unwrap();
                let tail_count = self
                    .builder
                    .build_int_sub(new_len, idx_val, "remove.tail_count")
                    .unwrap();
                let elem_size = elem_ty.size_of().unwrap();
                let tail_bytes = self
                    .builder
                    .build_int_mul(tail_count, elem_size, "remove.tail_bytes")
                    .unwrap();
                let next_idx = self
                    .builder
                    .build_int_add(idx_val, one, "remove.next_idx")
                    .unwrap();
                let src = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[next_idx], "remove.shift.src")
                        .unwrap()
                };
                self.builder
                    .build_memmove(elem_ptr, 8, src, 8, tail_bytes)
                    .unwrap();

                // Decrement len.
                self.builder.build_store(len_ptr, new_len).unwrap();

                Ok(elem_val)
            }
            // `Vec.pop` / `VecDeque.pop_back` / `VecDeque.pop_front` —
            // return `Option[T]` per design.md. None when empty;
            // Some(elem) when non-empty. Multi-word payload via
            // `coerce_to_payload_words` so tuple / Vec / String
            // element types fit the widened Option layout. pop_back
            // / pop drop the element at `len-1`; pop_front loads at
            // index 0 and memmoves the remaining tail left by 1.
            "pop" | "pop_back" | "pop_front" => {
                let is_front = method == "pop_front";
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "pop.len")
                    .unwrap()
                    .into_int_value();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "pop.data")
                    .unwrap()
                    .into_pointer_value();

                let fn_val = self.current_fn.unwrap();
                let empty_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{method}.empty"));
                let some_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{method}.some"));
                let merge_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{method}.merge"));

                let zero = i64_t.const_int(0, false);
                let is_empty = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "pop.is_empty")
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_empty, empty_bb, some_bb)
                    .unwrap();

                // Empty branch: no len decrement, no load.
                self.builder.position_at_end(empty_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Some branch: load elem, decrement len, memmove (front
                // only). Compute payload words from the loaded value.
                self.builder.position_at_end(some_bb);
                let one = i64_t.const_int(1, false);
                let read_idx = if is_front {
                    zero
                } else {
                    self.builder
                        .build_int_sub(len, one, "pop.last_idx")
                        .unwrap()
                };
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[read_idx], "pop.elem.ptr")
                        .unwrap()
                };
                let elem_val = self
                    .builder
                    .build_load(elem_ty, elem_ptr, "pop.elem")
                    .unwrap();
                if is_front {
                    // memmove(data, data + 1, (len - 1) * sizeof(elem))
                    let tail_count = self
                        .builder
                        .build_int_sub(len, one, "pop.tail_count")
                        .unwrap();
                    let elem_size = elem_ty.size_of().unwrap();
                    let tail_bytes = self
                        .builder
                        .build_int_mul(tail_count, elem_size, "pop.tail_bytes")
                        .unwrap();
                    let src = unsafe {
                        self.builder
                            .build_gep(elem_ty, data, &[one], "pop.shift.src")
                            .unwrap()
                    };
                    self.builder
                        .build_memmove(data, 8, src, 8, tail_bytes)
                        .unwrap();
                }
                let new_len = self.builder.build_int_sub(len, one, "pop.new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();
                let some_payload_words = self.coerce_to_payload_words(elem_val, 3)?;
                let some_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge: build Option struct via phi on tag + each
                // payload word. PHI nodes MUST be grouped at the top
                // of the basic block (LLVM rule), so create all phis
                // first, then build_insert_value into the aggregate.
                self.builder.position_at_end(merge_bb);
                let option_ty = self.enum_layouts["Option"].llvm_type;
                let tag_phi = self.builder.build_phi(i64_t, "pop.opt.tag").unwrap();
                tag_phi.add_incoming(&[(&zero, empty_bb), (&one, some_end_bb)]);
                let mut word_phis: Vec<inkwell::values::PhiValue<'ctx>> =
                    Vec::with_capacity(some_payload_words.len());
                for (i, w) in some_payload_words.iter().enumerate() {
                    let word_phi = self
                        .builder
                        .build_phi(i64_t, &format!("pop.opt.w{i}"))
                        .unwrap();
                    word_phi.add_incoming(&[(&zero, empty_bb), (w, some_end_bb)]);
                    word_phis.push(word_phi);
                }
                let mut agg: BasicValueEnum<'ctx> = option_ty.get_undef().into();
                agg = self
                    .builder
                    .build_insert_value(
                        agg.into_struct_value(),
                        tag_phi.as_basic_value(),
                        0,
                        "pop.opt.tag.ins",
                    )
                    .unwrap()
                    .into_struct_value()
                    .into();
                for (i, phi) in word_phis.iter().enumerate() {
                    agg = self
                        .builder
                        .build_insert_value(
                            agg.into_struct_value(),
                            phi.as_basic_value(),
                            (i + 1) as u32,
                            &format!("pop.opt.w{i}.ins"),
                        )
                        .unwrap()
                        .into_struct_value()
                        .into();
                }
                Ok(agg)
            }
            "push_str" => {
                if args.is_empty() {
                    return Err("push_str requires an argument".to_string());
                }
                let src_val = self.compile_expr(&args[0].value)?;
                // Extract src string's ptr and len.
                let src_ptr = self
                    .builder
                    .build_extract_value(src_val.into_struct_value(), 0, "src.ptr")
                    .unwrap()
                    .into_pointer_value();
                let src_len = self
                    .builder
                    .build_extract_value(src_val.into_struct_value(), 1, "src.len")
                    .unwrap()
                    .into_int_value();

                // Load target fields.
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "t.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "t.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "t.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "t.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "t.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "t.cap")
                    .unwrap()
                    .into_int_value();

                // Required capacity = len + src_len.
                let new_len = self.builder.build_int_add(len, src_len, "new_len").unwrap();

                // Growth check: if new_len > cap, grow.
                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "pstr.grow");
                let copy_bb = self.context.append_basic_block(fn_val, "pstr.copy");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, cap, "needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, copy_bb)
                    .unwrap();

                // Grow: new_cap = max(new_len, max(4, cap * 2))
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self.builder.build_int_mul(cap, two, "doubled").unwrap();
                let cmp1 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "cmp1")
                    .unwrap();
                let growth_min = self
                    .builder
                    .build_select(cmp1, doubled, four, "growth_min")
                    .unwrap()
                    .into_int_value();
                let cmp2 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, growth_min, "cmp2")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp2, new_len, growth_min, "new_cap")
                    .unwrap()
                    .into_int_value();

                let new_data = self
                    .builder
                    .build_call(self.malloc_fn, &[new_cap.into()], "new_data")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // Copy old data.
                self.builder
                    .build_memcpy(new_data, 1, data, 1, len)
                    .unwrap();
                // Free old if cap > 0 (heap-allocated).
                let zero_val = i64_t.const_int(0, false);
                let was_heap = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, cap, zero_val, "was_heap")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "pstr.free");
                let after_free_bb = self.context.append_basic_block(fn_val, "pstr.after_free");
                self.builder
                    .build_conditional_branch(was_heap, free_bb, after_free_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();
                self.builder
                    .build_unconditional_branch(after_free_bb)
                    .unwrap();
                self.builder.position_at_end(after_free_bb);

                self.builder.build_store(data_ptr_ptr, new_data).unwrap();
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(copy_bb).unwrap();

                // Copy src bytes to data + len.
                self.builder.position_at_end(copy_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "cur_data")
                    .unwrap()
                    .into_pointer_value();
                let cur_len = self
                    .builder
                    .build_load(i64_t, len_ptr, "cur_len")
                    .unwrap()
                    .into_int_value();
                let dest = unsafe {
                    self.builder
                        .build_gep(self.context.i8_type(), cur_data, &[cur_len], "dest")
                        .unwrap()
                };
                self.builder
                    .build_memcpy(dest, 1, src_ptr, 1, src_len)
                    .unwrap();
                // Update len.
                let updated_len = self
                    .builder
                    .build_int_add(cur_len, src_len, "updated_len")
                    .unwrap();
                self.builder.build_store(len_ptr, updated_len).unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `extend_from_slice(other: mut Slice[T])` — bulk-append all
            // elements of `other` to `self`. Same shape as `push_str`
            // but parameterized over the receiver's element type (rather
            // than byte-typed). Source may be a Slice / Vec / Array,
            // resolved via `coerce_to_slice` which returns a 2-field
            // `{data, len}` slice header.
            //
            // Memcpy is sound only because both source and dest hold
            // independent storage in the simple-element case. For RC-
            // bearing element types (Vec[String], Vec[Vec[T]]), this
            // bit-copies the inner aggregates — same shape as
            // `Vec.from_slice`'s codegen path (see assoc_call.rs:911-913)
            // and inherits the same v1 limitation: source and dest
            // observers will both see the inner pointers. A follow-up
            // slice should emit per-element clone for non-trivially-
            // copyable element types via the synth_clone machinery.
            "extend_from_slice" => {
                if args.len() != 1 {
                    return Err(format!(
                        "extend_from_slice expects 1 argument (source), got {}",
                        args.len()
                    ));
                }
                // Source coercion: try the Identifier / Range fast paths
                // via `coerce_to_slice` first, then fall back to
                // compile_expr-and-extract for arbitrary expressions
                // that produce a Vec (`{ptr, len, cap}`) or Slice
                // (`{ptr, len}`) struct — `rows[r]` on `Vec[Vec[T]]`,
                // `vec.clone()`, etc. Keeping the fallback local so
                // `coerce_to_slice` doesn't grow a compile-then-discard
                // path that would double-emit allocations for its other
                // callers (call_dispatch slice-param coercion).
                let src_data;
                let src_len;
                if let Some(slice_val) = self.coerce_to_slice(&args[0].value, elem_ty)? {
                    let slice_sv = slice_val.into_struct_value();
                    src_data = self
                        .builder
                        .build_extract_value(slice_sv, 0, "efs.src.data")
                        .unwrap()
                        .into_pointer_value();
                    src_len = self
                        .builder
                        .build_extract_value(slice_sv, 1, "efs.src.len")
                        .unwrap()
                        .into_int_value();
                } else {
                    let compiled = self.compile_expr(&args[0].value)?;
                    let sv = match compiled {
                        BasicValueEnum::StructValue(sv) => sv,
                        _ => {
                            return Err(format!(
                                "extend_from_slice: source expression does not produce a slice or vec value (got {compiled:?})"
                            ))
                        }
                    };
                    let n_fields = sv.get_type().count_fields();
                    if n_fields != 2 && n_fields != 3 {
                        return Err(format!(
                            "extend_from_slice: source struct has {n_fields} fields; expected 2 (Slice) or 3 (Vec)"
                        ));
                    }
                    src_data = self
                        .builder
                        .build_extract_value(sv, 0, "efs.src.data")
                        .unwrap()
                        .into_pointer_value();
                    src_len = self
                        .builder
                        .build_extract_value(sv, 1, "efs.src.len")
                        .unwrap()
                        .into_int_value();
                }
                let elem_size = elem_ty.size_of().unwrap();
                let src_bytes = self
                    .builder
                    .build_int_mul(src_len, elem_size, "efs.src.bytes")
                    .unwrap();

                // Load target fields.
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "efs.t.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "efs.t.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "efs.t.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "efs.t.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "efs.t.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "efs.t.cap")
                    .unwrap()
                    .into_int_value();

                let new_len = self
                    .builder
                    .build_int_add(len, src_len, "efs.new_len")
                    .unwrap();

                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "efs.grow");
                let copy_bb = self.context.append_basic_block(fn_val, "efs.copy");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, cap, "efs.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, copy_bb)
                    .unwrap();

                // Grow: new_cap = max(new_len, max(4, cap * 2)). Identical
                // policy to `push` / `push_str` — keeps capacity geometry
                // consistent so re-entry to grow logic always picks the
                // same multipliers.
                self.builder.position_at_end(grow_bb);

                // Overlap guard. When the source slice points into the
                // receiver's own heap buffer (`v.extend_from_slice(v
                // .as_slice())` and any expression that produces a
                // slice over `data..data+cap*elem_size`), the grow
                // path is about to `free(data)` before reading from
                // `src_data` — which would dangle. `push` / `push_str`
                // don't carry this hazard (source is a by-value element
                // / static-storage byte slice). The cost is paid only
                // in the rare grow case, already the cold path. Use
                // ptrtoint+i64 compares so the predicate is portable
                // across address spaces and target widths.
                let src_int = self
                    .builder
                    .build_ptr_to_int(src_data, i64_t, "efs.src.int")
                    .unwrap();
                let data_int = self
                    .builder
                    .build_ptr_to_int(data, i64_t, "efs.data.int")
                    .unwrap();
                let cap_bytes_grow = self
                    .builder
                    .build_int_mul(cap, elem_size, "efs.cap.bytes")
                    .unwrap();
                let data_end = self
                    .builder
                    .build_int_add(data_int, cap_bytes_grow, "efs.data.end")
                    .unwrap();
                let ge_start = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::UGE,
                        src_int,
                        data_int,
                        "efs.ge.start",
                    )
                    .unwrap();
                let lt_end = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, src_int, data_end, "efs.lt.end")
                    .unwrap();
                let overlap = self
                    .builder
                    .build_and(ge_start, lt_end, "efs.overlap")
                    .unwrap();
                let panic_bb = self.context.append_basic_block(fn_val, "efs.alias.panic");
                let no_overlap_bb = self.context.append_basic_block(fn_val, "efs.no_overlap");
                self.builder
                    .build_conditional_branch(overlap, panic_bb, no_overlap_bb)
                    .unwrap();
                self.builder.position_at_end(panic_bb);
                self.emit_panic(
                    "Vec.extend_from_slice: source slice aliases destination buffer (use a distinct source when grow is required)",
                );
                self.builder.build_unreachable().unwrap();
                self.builder.position_at_end(no_overlap_bb);

                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self.builder.build_int_mul(cap, two, "efs.doubled").unwrap();
                let cmp1 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "efs.cmp1")
                    .unwrap();
                let growth_min = self
                    .builder
                    .build_select(cmp1, doubled, four, "efs.growth_min")
                    .unwrap()
                    .into_int_value();
                let cmp2 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, growth_min, "efs.cmp2")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp2, new_len, growth_min, "efs.new_cap")
                    .unwrap()
                    .into_int_value();

                // Allocate new buffer sized by new_cap * elem_size.
                let new_alloc_bytes = self
                    .builder
                    .build_int_mul(new_cap, elem_size, "efs.new.bytes")
                    .unwrap();
                let new_data = self
                    .builder
                    .build_call(self.malloc_fn, &[new_alloc_bytes.into()], "efs.new_data")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // Copy existing elements over (len * elem_size bytes).
                let old_bytes = self
                    .builder
                    .build_int_mul(len, elem_size, "efs.old.bytes")
                    .unwrap();
                self.builder
                    .build_memcpy(new_data, 8, data, 8, old_bytes)
                    .unwrap();
                // Free old buffer if cap > 0 (heap-allocated).
                let zero_val = i64_t.const_int(0, false);
                let was_heap = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, cap, zero_val, "efs.was_heap")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "efs.free");
                let after_free_bb = self.context.append_basic_block(fn_val, "efs.after_free");
                self.builder
                    .build_conditional_branch(was_heap, free_bb, after_free_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();
                self.builder
                    .build_unconditional_branch(after_free_bb)
                    .unwrap();
                self.builder.position_at_end(after_free_bb);

                self.builder.build_store(data_ptr_ptr, new_data).unwrap();
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(copy_bb).unwrap();

                // Copy src elements to data + len * elem_size (i.e., GEP
                // by len in elem_ty stride). Two paths: memcpy fast path
                // for trivially-copyable elements (primitives), or
                // per-element synth_clone for anything that carries a
                // heap pointer (String, Vec, Map, Set, shared T, tuples
                // / structs that recursively contain any of those).
                //
                // Without the clone path, `Vec[String].extend_from_slice`
                // and `Vec[Vec[T]].extend_from_slice` bit-copy aggregate
                // values whose inner `{ptr, len, cap}` triples then
                // alias the source's heap buffers in dest. Both scope-
                // exit frees fire on the same pointers → double-free /
                // UAF (ASAN-flagged in `tests/memory_sanitizer.rs ::
                // asan_vec_extend_from_slice_nested_vec_elements_independent`).
                self.builder.position_at_end(copy_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "efs.cur_data")
                    .unwrap()
                    .into_pointer_value();
                let cur_len = self
                    .builder
                    .build_load(i64_t, len_ptr, "efs.cur_len")
                    .unwrap()
                    .into_int_value();
                let elem_te = self.var_elem_type_exprs.get(var_name).cloned();
                let trivial = elem_te
                    .as_ref()
                    .map(is_trivially_copyable_te)
                    .unwrap_or(true);
                if trivial {
                    let dest = unsafe {
                        self.builder
                            .build_gep(elem_ty, cur_data, &[cur_len], "efs.dest")
                            .unwrap()
                    };
                    self.builder
                        .build_memcpy(dest, 8, src_data, 8, src_bytes)
                        .unwrap();
                } else {
                    let elem_te = elem_te.unwrap();
                    let clone_fn = self.emit_clone_fn_for_type_expr(&elem_te);
                    // Per-element clone loop:
                    //   for i in 0..src_len:
                    //     src_ep = src_data + i * elem_size
                    //     dst_ep = cur_data + (cur_len + i) * elem_size
                    //     karac_clone_<T>(src_ep, dst_ep)
                    let loop_cond_bb = self.context.append_basic_block(fn_val, "efs.clone.cond");
                    let loop_body_bb = self.context.append_basic_block(fn_val, "efs.clone.body");
                    let loop_exit_bb = self.context.append_basic_block(fn_val, "efs.clone.exit");
                    let i_alloca = self.create_entry_alloca(fn_val, "efs.clone.i", i64_t.into());
                    self.builder
                        .build_store(i_alloca, i64_t.const_zero())
                        .unwrap();
                    self.builder
                        .build_unconditional_branch(loop_cond_bb)
                        .unwrap();

                    self.builder.position_at_end(loop_cond_bb);
                    let i_cur = self
                        .builder
                        .build_load(i64_t, i_alloca, "efs.clone.i.cur")
                        .unwrap()
                        .into_int_value();
                    let cond = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::ULT,
                            i_cur,
                            src_len,
                            "efs.clone.lt",
                        )
                        .unwrap();
                    self.builder
                        .build_conditional_branch(cond, loop_body_bb, loop_exit_bb)
                        .unwrap();

                    self.builder.position_at_end(loop_body_bb);
                    let src_ep = unsafe {
                        self.builder
                            .build_gep(elem_ty, src_data, &[i_cur], "efs.clone.src.ep")
                            .unwrap()
                    };
                    let dst_idx = self
                        .builder
                        .build_int_add(cur_len, i_cur, "efs.clone.dst.idx")
                        .unwrap();
                    let dst_ep = unsafe {
                        self.builder
                            .build_gep(elem_ty, cur_data, &[dst_idx], "efs.clone.dst.ep")
                            .unwrap()
                    };
                    self.builder
                        .build_call(clone_fn, &[src_ep.into(), dst_ep.into()], "")
                        .unwrap();
                    let one = i64_t.const_int(1, false);
                    let i_next = self
                        .builder
                        .build_int_add(i_cur, one, "efs.clone.i.next")
                        .unwrap();
                    self.builder.build_store(i_alloca, i_next).unwrap();
                    self.builder
                        .build_unconditional_branch(loop_cond_bb)
                        .unwrap();

                    self.builder.position_at_end(loop_exit_bb);
                }
                let updated_len = self
                    .builder
                    .build_int_add(cur_len, src_len, "efs.updated_len")
                    .unwrap();
                self.builder.build_store(len_ptr, updated_len).unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "is_empty" => {
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "vec.len")
                    .unwrap()
                    .into_int_value();
                let zero = i64_t.const_int(0, false);
                let is_empty = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "is_empty")
                    .unwrap();
                Ok(is_empty.into())
            }
            "bytes" => {
                // `String.bytes() -> Slice[u8]` (design.md § Character
                // type). Zero-copy view: String's runtime layout is
                // `{ptr, len, cap}`, so a `Slice[u8]` is just the first
                // two fields packed into the `{ptr, i64}` slice header.
                // No new allocation, no buffer copy — the caller observes
                // bytes through the same heap (or .rodata) storage the
                // source String owns. The returned slice is read-only;
                // mutating through it would alias the source's bytes
                // (and could produce invalid UTF-8), so the typechecker
                // hands back `Slice[u8]`, not `mut Slice[u8]`.
                //
                // The dispatch reaches here only for String-typed
                // bindings — `bytes` is not a Vec method. The
                // `compile_vec_method` entry point is shared because
                // Vec and String have the same `{ptr, len, cap}` runtime
                // shape; the typechecker has already gated the receiver.
                let slice_ty = self.slice_struct_type();
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "bytes.data.pp")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "bytes.data")
                    .unwrap()
                    .into_pointer_value();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "bytes.len.p")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "bytes.len")
                    .unwrap()
                    .into_int_value();
                Ok(self.build_slice_header(slice_ty, data, len))
            }
            "first" | "last" => {
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();

                let fn_val = self.current_fn.unwrap();
                let empty_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{method}.empty"));
                let some_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{method}.some"));
                let merge_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{method}.merge"));

                let zero = i64_t.const_int(0, false);
                let is_empty = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "is_empty")
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_empty, empty_bb, some_bb)
                    .unwrap();

                // Empty branch — return None.
                self.builder.position_at_end(empty_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Some branch — load element at index 0 (first) or len-1 (last).
                self.builder.position_at_end(some_bb);
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let idx = if method == "first" {
                    zero
                } else {
                    let one = i64_t.const_int(1, false);
                    self.builder.build_int_sub(len, one, "last_idx").unwrap()
                };
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[idx], "elem.ptr")
                        .unwrap()
                };
                let elem_val = self.builder.build_load(elem_ty, elem_ptr, "elem").unwrap();
                // Multi-word payload: split V into 3 i64 words to fit the
                // widened Option layout (`{i64 tag, i64 w0, i64 w1, i64 w2}`
                // — see `seed_builtin_enum_layouts` line 3445). Mirrors the
                // `Vec.pop` precedent (line 8580). Single-word V (i64, ptr,
                // bool, etc.) flows through `coerce_to_payload_words`'s
                // primitive fast path; multi-word V (Vec, String, tuples)
                // gets per-field decomposition. Without this, non-scalar V
                // truncates to its first word and the destructure-side
                // `pattern_payload_word_count` reads undef for fields 2..=3.
                let some_payload_words = self.coerce_to_payload_words(elem_val, 3)?;
                let some_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge — phi on tag and per-payload-word, then build Option struct.
                self.builder.position_at_end(merge_bb);
                let agg = self.build_option_some_via_phis(
                    &some_payload_words,
                    some_end_bb,
                    empty_bb,
                    "opt",
                );
                Ok(agg)
            }
            "get" => {
                if args.is_empty() {
                    return Err("Vec.get requires an index argument".to_string());
                }
                let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();

                let fn_val = self.current_fn.unwrap();
                let oob_bb = self.context.append_basic_block(fn_val, "get.oob");
                let valid_bb = self.context.append_basic_block(fn_val, "get.valid");
                let merge_bb = self.context.append_basic_block(fn_val, "get.merge");

                let in_bounds = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, idx_val, len, "in_bounds")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_bounds, valid_bb, oob_bb)
                    .unwrap();

                // Out-of-bounds branch — return None.
                self.builder.position_at_end(oob_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Valid branch — return Some(data[idx]).
                self.builder.position_at_end(valid_bb);
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[idx_val], "elem.ptr")
                        .unwrap()
                };
                let elem_val = self.builder.build_load(elem_ty, elem_ptr, "elem").unwrap();
                // Multi-word payload via `coerce_to_payload_words` — see
                // `Vec.first`/`Vec.last` arm above for the rationale.
                let some_payload_words = self.coerce_to_payload_words(elem_val, 3)?;
                let valid_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge — phi, then build Option struct.
                self.builder.position_at_end(merge_bb);
                let agg = self.build_option_some_via_phis(
                    &some_payload_words,
                    valid_end_bb,
                    oob_bb,
                    "opt",
                );
                Ok(agg)
            }
            // `Vec[T].get_unchecked(i: i64) -> T` — direct-index read with
            // NO bounds check. UB on out-of-range. Mirrors the `"get"` arm's
            // GEP+load lead but skips the `oob_bb` / `valid_bb` CFG split
            // and returns the loaded element directly rather than wrapping
            // in `Option`. The unsafe-block requirement is enforced upstream
            // by `unsafe_lint::build_unsafe_fn_registry`; reaching this arm
            // implies the caller already passed that check.
            "get_unchecked" => {
                if args.is_empty() {
                    return Err("Vec.get_unchecked requires an index argument".to_string());
                }
                let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[idx_val], "v.unchecked.elem.ptr")
                        .unwrap()
                };
                let val = self
                    .builder
                    .build_load(elem_ty, elem_ptr, "v.unchecked.elem")
                    .unwrap();
                Ok(val)
            }
            "sort_by" => {
                if args.len() != 1 {
                    return Err(format!(
                        "Vec.sort_by expects 1 argument (comparator closure), got {}",
                        args.len()
                    ));
                }

                // Two thunk shapes:
                //   (a) inline closure expression — fuse the closure body
                //       into the bridge thunk, so each comparison is a
                //       single direct function call from the runtime helper
                //       (LLVM can then inline it freely);
                //   (b) named callee / closure-typed value — fall back to
                //       compile_expr → fat pointer → indirect-call thunk.
                let (thunk, ctx_alloca): (FunctionValue<'ctx>, PointerValue<'ctx>) =
                    if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                        self.emit_sort_by_inline_thunk(params, body, elem_ty)?
                    } else {
                        self.pending_closure_param_hints = Some(vec![elem_ty, elem_ty]);
                        let closure_val = self.compile_expr(&args[0].value)?;
                        self.pending_closure_param_hints = None;
                        let closure_fn_type = self
                            .pending_closure_fn_type
                            .take()
                            .ok_or_else(|| "Vec.sort_by: closure missing fn_type".to_string())?;
                        let outer_fn = self.current_fn.unwrap();
                        let fat_ty = self.closure_value_type();
                        let cls_alloca =
                            self.create_entry_alloca(outer_fn, "sort_by.cls", fat_ty.into());
                        self.builder.build_store(cls_alloca, closure_val).unwrap();
                        (
                            self.emit_sort_by_thunk(elem_ty, closure_fn_type),
                            cls_alloca,
                        )
                    };

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();

                let runtime_fn = self
                    .module
                    .get_function("karac_vec_sort_by")
                    .unwrap_or_else(|| {
                        let void_t = self.context.void_type();
                        let fn_ty = void_t.fn_type(
                            &[
                                ptr_ty.into(),
                                i64_t.into(),
                                i64_t.into(),
                                ptr_ty.into(),
                                ptr_ty.into(),
                            ],
                            false,
                        );
                        self.module.add_function(
                            "karac_vec_sort_by",
                            fn_ty,
                            Some(Linkage::External),
                        )
                    });

                let thunk_ptr = thunk.as_global_value().as_pointer_value();
                self.builder
                    .build_call(
                        runtime_fn,
                        &[
                            BasicMetadataValueEnum::from(data),
                            BasicMetadataValueEnum::from(len),
                            BasicMetadataValueEnum::from(elem_size),
                            BasicMetadataValueEnum::from(thunk_ptr),
                            BasicMetadataValueEnum::from(ctx_alloca),
                        ],
                        "",
                    )
                    .unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "sort" => {
                if !args.is_empty() {
                    return Err(format!("Vec.sort expects 0 arguments, got {}", args.len()));
                }
                // Bare `sort()` is `sort_by` with the natural ascending order.
                // Only integer element types have a default comparator in
                // codegen today — consistent with the signed `.cmp` lowering
                // in method_call.rs. Other element types (floats, tuples,
                // strings) must use `sort_by(|a, b| ...)` with an explicit
                // comparator; the typechecker accepts them but the default
                // ordering has no lowering yet, so error loudly here rather
                // than silently leaving the Vec unsorted.
                if !elem_ty.is_int_type() {
                    return Err(
                        "Vec.sort() in codegen supports only integer element types; \
                         use sort_by(|a, b| a.cmp(b)) for other element types"
                            .to_string(),
                    );
                }
                let thunk = self.emit_default_sort_thunk(elem_ty);

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();

                let runtime_fn = self
                    .module
                    .get_function("karac_vec_sort_by")
                    .unwrap_or_else(|| {
                        let void_t = self.context.void_type();
                        let fn_ty = void_t.fn_type(
                            &[
                                ptr_ty.into(),
                                i64_t.into(),
                                i64_t.into(),
                                ptr_ty.into(),
                                ptr_ty.into(),
                            ],
                            false,
                        );
                        self.module.add_function(
                            "karac_vec_sort_by",
                            fn_ty,
                            Some(Linkage::External),
                        )
                    });

                let thunk_ptr = thunk.as_global_value().as_pointer_value();
                let null_ctx = ptr_ty.const_null();
                self.builder
                    .build_call(
                        runtime_fn,
                        &[
                            BasicMetadataValueEnum::from(data),
                            BasicMetadataValueEnum::from(len),
                            BasicMetadataValueEnum::from(elem_size),
                            BasicMetadataValueEnum::from(thunk_ptr),
                            BasicMetadataValueEnum::from(null_ctx),
                        ],
                        "",
                    )
                    .unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "reverse" => {
                if !args.is_empty() {
                    return Err(format!(
                        "Vec.reverse expects 0 arguments, got {}",
                        args.len()
                    ));
                }
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();

                let runtime_fn = self
                    .module
                    .get_function("karac_vec_reverse")
                    .unwrap_or_else(|| {
                        let void_t = self.context.void_type();
                        let fn_ty =
                            void_t.fn_type(&[ptr_ty.into(), i64_t.into(), i64_t.into()], false);
                        self.module.add_function(
                            "karac_vec_reverse",
                            fn_ty,
                            Some(Linkage::External),
                        )
                    });

                self.builder
                    .build_call(
                        runtime_fn,
                        &[
                            BasicMetadataValueEnum::from(data),
                            BasicMetadataValueEnum::from(len),
                            BasicMetadataValueEnum::from(elem_size),
                        ],
                        "",
                    )
                    .unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            "sort_by_key" => {
                if args.len() != 1 {
                    return Err(format!(
                        "Vec.sort_by_key expects 1 argument (key closure), got {}",
                        args.len()
                    ));
                }
                // V1: only inline-closure callees. A named function or
                // closure-typed local would need a bridge thunk with an
                // indirect call through the closure's fat pointer (mirror
                // of `emit_sort_by_thunk` for sort_by) — not yet wired up.
                let (params, body) = match &args[0].value.kind {
                    ExprKind::Closure { params, body, .. } => (params, body.as_ref()),
                    _ => {
                        return Err(
                            "Vec.sort_by_key in codegen only supports an inline closure \
                             argument today (a named function or closure-typed local \
                             is not yet wired through the bridge thunk)"
                                .to_string(),
                        );
                    }
                };
                // Look up the Vec element's Kāra type name so the thunk can
                // register `var_type_names` for the closure param. Without
                // that, `compile_field_access` on a body like `|s| s.field`
                // can't recover the struct shape and the field load is
                // silently elided. Pulls from `var_elem_type_exprs` (the
                // canonical record of a Vec binding's element type expr) —
                // the canonical first segment is the struct name when the
                // element is a path-typed struct; for tuple / generic / etc.
                // shapes we just pass `None` and the thunk falls back to
                // the existing param-only binding.
                let elem_type_name: Option<String> = self
                    .var_elem_type_exprs
                    .get(var_name)
                    .and_then(|te| match &te.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    });
                let (thunk, ctx_alloca) = self.emit_sort_by_key_inline_thunk(
                    params,
                    body,
                    elem_ty,
                    elem_type_name.as_deref(),
                )?;

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vec.len.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "len")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();

                let runtime_fn = self
                    .module
                    .get_function("karac_vec_sort_by")
                    .unwrap_or_else(|| {
                        let void_t = self.context.void_type();
                        let fn_ty = void_t.fn_type(
                            &[
                                ptr_ty.into(),
                                i64_t.into(),
                                i64_t.into(),
                                ptr_ty.into(),
                                ptr_ty.into(),
                            ],
                            false,
                        );
                        self.module.add_function(
                            "karac_vec_sort_by",
                            fn_ty,
                            Some(Linkage::External),
                        )
                    });

                let thunk_ptr = thunk.as_global_value().as_pointer_value();
                self.builder
                    .build_call(
                        runtime_fn,
                        &[
                            BasicMetadataValueEnum::from(data),
                            BasicMetadataValueEnum::from(len),
                            BasicMetadataValueEnum::from(elem_size),
                            BasicMetadataValueEnum::from(thunk_ptr),
                            BasicMetadataValueEnum::from(ctx_alloca),
                        ],
                        "",
                    )
                    .unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // No silent fall-through: a Vec/String method the typechecker
            // accepts but codegen has no arm for must fail the build loudly,
            // not return a stand-in `0` that masquerades as a no-op result
            // (the bug that hid `sort` / `sort_by_key` / `reverse` silently
            // doing nothing in compiled binaries). See design.md § Codegen.
            other => Err(format!(
                "Vec/String method '{}' is not yet supported in codegen",
                other
            )),
        }
    }

    /// Default ascending-order comparator thunk for `Vec.sort()` on integer
    /// element types. Signature `extern "C" fn(ctx, *a, *b) -> i64` matching
    /// `karac_vec_sort_by`'s contract; `ctx` is unused (no captures). Returns
    /// `-1 / 0 / +1` via a signed compare, mirroring the `.cmp` lowering in
    /// method_call.rs so `sort()` and `sort_by(|a, b| a.cmp(b))` agree.
    pub(super) fn emit_default_sort_thunk(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_default_cmp_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(thunk_fn);

        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        let a_ptr = thunk_fn.get_nth_param(1).unwrap().into_pointer_value();
        let b_ptr = thunk_fn.get_nth_param(2).unwrap().into_pointer_value();
        let a = self
            .builder
            .build_load(elem_ty, a_ptr, "a")
            .unwrap()
            .into_int_value();
        let b = self
            .builder
            .build_load(elem_ty, b_ptr, "b")
            .unwrap()
            .into_int_value();
        let lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, a, b, "lt")
            .unwrap();
        let gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, a, b, "gt")
            .unwrap();
        let zero = i64_t.const_zero();
        let neg_one = i64_t.const_int((-1i64) as u64, true);
        let pos_one = i64_t.const_int(1, false);
        let gt_sel = self
            .builder
            .build_select(gt, pos_one, zero, "gt.sel")
            .unwrap()
            .into_int_value();
        let res = self
            .builder
            .build_select(lt, neg_one, gt_sel, "cmp.sel")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&res)).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        thunk_fn
    }

    /// Inline-closure fast path for `Vec.sort_by_key`. The closure takes ONE
    /// param and returns a key; the bridge thunk computes the key for each
    /// of the two elements by compiling the closure body twice into itself
    /// (so both key extractions inline cleanly under LLVM's later passes),
    /// then returns the signed compare of the two keys as `-1 / 0 / +1` —
    /// the same comparator contract `karac_vec_sort_by` consumes. Captures
    /// ride the same env-struct + outer-stack-alloca shape as
    /// `emit_sort_by_inline_thunk`. The compiler restricts the key type to
    /// integers (consistent with the `.cmp` lowering in method_call.rs and
    /// the default-order `sort()` thunk above), so non-integer keys error
    /// loudly rather than silently producing wrong output.
    #[allow(clippy::too_many_lines)]
    pub(super) fn emit_sort_by_key_inline_thunk(
        &mut self,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_type_name: Option<&str>,
    ) -> Result<(FunctionValue<'ctx>, PointerValue<'ctx>), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        if params.len() != 1 {
            return Err(format!(
                "Vec.sort_by_key key closure must take exactly 1 argument, got {}",
                params.len()
            ));
        }

        // 1. Captures (same shape as emit_sort_by_inline_thunk).
        let free_vars = self.collect_closure_free_vars(params, body);
        let env_field_types: Vec<BasicTypeEnum<'ctx>> = if free_vars.is_empty() {
            vec![self.context.i8_type().into()]
        } else {
            free_vars.iter().map(|n| self.variables[n].ty).collect()
        };
        let env_struct_ty = self.context.struct_type(&env_field_types, false);

        // 2. Stack-allocate + populate env in the outer frame.
        let outer_fn = self.current_fn.unwrap();
        let env_alloca =
            self.create_entry_alloca(outer_fn, "sort_by_key.env", env_struct_ty.into());
        if !free_vars.is_empty() {
            let mut env_agg = env_struct_ty.get_undef();
            for (i, var_name) in free_vars.iter().enumerate() {
                let slot = self.variables[var_name];
                let val = self
                    .builder
                    .build_load(slot.ty, slot.ptr, var_name)
                    .unwrap();
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, val, i as u32, "env.field")
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
        }

        // 3. Declare thunk: extern "C" fn(ctx, *a, *b) -> i64.
        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_by_key_inline_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        // 4. Save outer codegen state.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_subst = std::mem::take(&mut self.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_fn_types);
        let saved_pct = self.pending_closure_fn_type.take();

        // 5. Build thunk body.
        self.current_fn = Some(thunk_fn);
        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        let ctx_ptr = thunk_fn.get_nth_param(0).unwrap().into_pointer_value();
        let a_ptr = thunk_fn.get_nth_param(1).unwrap().into_pointer_value();
        let b_ptr = thunk_fn.get_nth_param(2).unwrap().into_pointer_value();

        if !free_vars.is_empty() {
            let env_val = self
                .builder
                .build_load(env_struct_ty, ctx_ptr, "env")
                .unwrap()
                .into_struct_value();
            for (i, var_name) in free_vars.iter().enumerate() {
                let cap_ty = env_field_types[i];
                let field_val = self
                    .builder
                    .build_extract_value(env_val, i as u32, var_name)
                    .unwrap();
                let alloca = self.create_entry_alloca(thunk_fn, var_name, cap_ty);
                self.builder.build_store(alloca, field_val).unwrap();
                self.variables.insert(
                    var_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: cap_ty,
                    },
                );
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
            }
        }

        // 6. Load both elements through their typed pointers.
        let a_val = self.builder.build_load(elem_ty, a_ptr, "a.val").unwrap();
        let b_val = self.builder.build_load(elem_ty, b_ptr, "b.val").unwrap();

        // 7. Resolve the key-closure's single param name.
        let param_name = match &params[0].pattern.kind {
            PatternKind::Binding(n) => n.clone(),
            _ => "_kp".to_string(),
        };
        let param_ty = a_val.get_type();

        // Register the closure param's Kāra type name (the Vec's element
        // type) under `var_type_names` so `compile_field_access` can
        // resolve struct field reads inside the closure body. Without
        // this, a body like `|s| s.v` compiles to just the struct load —
        // the field-extract step silently elides because
        // `type_name_of_expr(s)` returns `None`. The registration applies
        // to both compiles below (first and second body recompile);
        // saved_var_types is restored when the thunk emitter returns.
        if let Some(name) = elem_type_name {
            self.var_type_names
                .insert(param_name.clone(), name.to_string());
        }

        // 8. First compile (key_a): bind param to element a, compile body.
        let alloca_a = self.create_entry_alloca(thunk_fn, &format!("{}.a", param_name), param_ty);
        self.builder.build_store(alloca_a, a_val).unwrap();
        self.variables.insert(
            param_name.clone(),
            VarSlot {
                ptr: alloca_a,
                ty: param_ty,
            },
        );
        let key_a_val = self.compile_expr(body)?;

        // 9. Second compile (key_b): rebind param to element b, compile body
        // again. Compiling the body twice produces two copies of the key
        // expression in the thunk, but for the realistic key shapes
        // (`|x| x`, `|x| -x`, `|x| x.field`) the body is small and the
        // duplication folds away under LLVM's later optimisation passes.
        let alloca_b = self.create_entry_alloca(thunk_fn, &format!("{}.b", param_name), param_ty);
        self.builder.build_store(alloca_b, b_val).unwrap();
        self.variables.insert(
            param_name.clone(),
            VarSlot {
                ptr: alloca_b,
                ty: param_ty,
            },
        );
        let key_b_val = self.compile_expr(body)?;

        // 10. Compare the two keys → i64 `-1 / 0 / +1`. Three key shapes:
        //   (a) plain integer key — signed compare, matching the
        //       default-order `sort()` thunk and the `.cmp` lowering in
        //       method_call.rs.
        //   (b) integer-tuple key (`(i64, i64)`, `(i64, i64, i64)`, …) —
        //       lexicographic compare, equivalent to Rust's derived
        //       `Ord` on tuples. Detectable without Kāra-type plumbing
        //       because all-integer tuples are unambiguous at the LLVM
        //       struct level. Implemented as a cascade of selects: build
        //       the result from the last field backward, with each
        //       earlier field's `(neq ? cmp_i : rest)` overriding the
        //       accumulated rest when it differs. Pure data-flow, no new
        //       basic blocks.
        //   (c) String key — `karac_string_cmp` runtime fn (lexicographic
        //       byte compare with length tie-break). String and `Vec[T]`
        //       share the LLVM struct shape `{ptr, i64, i64}`, so the
        //       value alone can't tell them apart; this arm fires when the
        //       body Expr's span is in `string_typed_exprs` (populated by
        //       the lowering pass from `TypeCheckResult.expr_types`).
        // Other key shapes (structs implementing Ord via user `cmp`,
        // floats) still error loudly — see the *non-integer key type*
        // follow-on entry in docs/implementation_checklist/phase-7-codegen.md.
        let i64_zero = i64_t.const_zero();
        let i64_neg_one = i64_t.const_int((-1i64) as u64, true);
        let i64_pos_one = i64_t.const_int(1, false);
        let key_body_span = (body.span.offset, body.span.length);
        let res = if self.string_typed_exprs.contains(&key_body_span) {
            match (key_a_val, key_b_val) {
                (BasicValueEnum::StructValue(ka), BasicValueEnum::StructValue(kb)) => {
                    let a_ptr = self
                        .builder
                        .build_extract_value(ka, 0, "ka.str.ptr")
                        .unwrap()
                        .into_pointer_value();
                    let a_len = self
                        .builder
                        .build_extract_value(ka, 1, "ka.str.len")
                        .unwrap()
                        .into_int_value();
                    let b_ptr = self
                        .builder
                        .build_extract_value(kb, 0, "kb.str.ptr")
                        .unwrap()
                        .into_pointer_value();
                    let b_len = self
                        .builder
                        .build_extract_value(kb, 1, "kb.str.len")
                        .unwrap()
                        .into_int_value();
                    let runtime_fn =
                        self.module
                            .get_function("karac_string_cmp")
                            .unwrap_or_else(|| {
                                let fn_ty = i64_t.fn_type(
                                    &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                                    false,
                                );
                                self.module.add_function(
                                    "karac_string_cmp",
                                    fn_ty,
                                    Some(Linkage::External),
                                )
                            });
                    let call = self
                        .builder
                        .build_call(
                            runtime_fn,
                            &[
                                BasicMetadataValueEnum::from(a_ptr),
                                BasicMetadataValueEnum::from(a_len),
                                BasicMetadataValueEnum::from(b_ptr),
                                BasicMetadataValueEnum::from(b_len),
                            ],
                            "str.cmp",
                        )
                        .unwrap();
                    call.try_as_basic_value().unwrap_basic().into_int_value()
                }
                _ => {
                    return Err(
                        "Vec.sort_by_key: String-typed key did not compile to a struct value \
                         (compiler bug — string_typed_exprs and the closure body's value type \
                         disagree)"
                            .to_string(),
                    );
                }
            }
        } else if let Some(cmp_callee_key) = self.user_ord_typed_exprs.get(&key_body_span).cloned()
        {
            // User `impl Ord for T` struct key — dispatch to the user's
            // compiled `Type.cmp` via direct call. Takes precedence over
            // the field cascade below: the user's cmp may encode logic
            // (reverse order, custom tiebreaks, partial-field orderings)
            // that the derive-equivalent cascade can't reproduce. Gated
            // by the typechecker change in `derives.rs` (has_user_impl_ord)
            // so this path only fires when the user opted in via
            // `impl Ord` rather than `#[derive(Ord)]`.
            let cmp_fn = match self.module.get_function(&cmp_callee_key) {
                Some(f) => f,
                None => {
                    return Err(format!(
                        "Vec.sort_by_key: user `impl Ord` callee '{}' not found in the \
                         module (compiler bug — typechecker accepted impl Ord but codegen \
                         never emitted the cmp function)",
                        cmp_callee_key
                    ));
                }
            };
            // Inspect the cmp function's first param to decide the
            // calling convention: pointer-typed (`ref self`) means
            // alloca + store + pass pointer; struct-typed (owned `self`)
            // means pass by value. Mirrors the receiver-convention
            // inspection in `compile_method_call:951`.
            let first_param_is_ptr = cmp_fn
                .get_type()
                .get_param_types()
                .first()
                .map(|t| matches!(t, BasicMetadataTypeEnum::PointerType(_)))
                .unwrap_or(false);
            let (a_arg, b_arg): (BasicMetadataValueEnum<'ctx>, BasicMetadataValueEnum<'ctx>) =
                if first_param_is_ptr {
                    let val_ty = key_a_val.get_type();
                    let alloca_a = self.create_entry_alloca(thunk_fn, "user_cmp.a", val_ty);
                    let alloca_b = self.create_entry_alloca(thunk_fn, "user_cmp.b", val_ty);
                    self.builder.build_store(alloca_a, key_a_val).unwrap();
                    self.builder.build_store(alloca_b, key_b_val).unwrap();
                    (alloca_a.into(), alloca_b.into())
                } else {
                    (
                        BasicMetadataValueEnum::from(key_a_val),
                        BasicMetadataValueEnum::from(key_b_val),
                    )
                };
            let call = self
                .builder
                .build_call(cmp_fn, &[a_arg, b_arg], "user.cmp")
                .unwrap();
            let ord_val = call.try_as_basic_value().unwrap_basic();
            // Ordering lowers to `{ i64 tag }` (unit-only enum, Less=0,
            // Equal=1, Greater=2 from `seed_builtin_enum_layouts`).
            // `tag - 1` yields `-1 / 0 / +1` — same conversion
            // `emit_sort_by_thunk` uses for sort_by's named-callee path.
            let tag = if ord_val.is_struct_value() {
                self.builder
                    .build_extract_value(ord_val.into_struct_value(), 0, "user.cmp.tag")
                    .unwrap()
                    .into_int_value()
            } else {
                ord_val.into_int_value()
            };
            let one = i64_t.const_int(1, false);
            self.builder
                .build_int_sub(tag, one, "user.cmp.shift")
                .unwrap()
        } else if let Some(struct_name) = self.expr_struct_type_names.get(&key_body_span).cloned() {
            // Struct-typed key (`sort_by_key(|item| item)` where
            // `item: MyStruct`). Delegate to the recursive cascade helper —
            // it handles single-struct, mixed-int+String fields, and nested
            // struct fields by recursing on any field whose Kāra type is
            // itself a `Named` struct registered in `struct_field_type_names`.
            let (ka, kb) = match (key_a_val, key_b_val) {
                (BasicValueEnum::StructValue(ka), BasicValueEnum::StructValue(kb)) => (ka, kb),
                _ => {
                    return Err(format!(
                        "Vec.sort_by_key: struct-typed key '{}' did not compile to a struct \
                         value (compiler bug — expr_struct_type_names and the closure body's \
                         value type disagree)",
                        struct_name
                    ));
                }
            };
            self.emit_struct_cmp_cascade(ka, kb, &struct_name, 0)?
        } else {
            match (key_a_val, key_b_val) {
                (BasicValueEnum::IntValue(ka), BasicValueEnum::IntValue(kb)) => {
                    let lt = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SLT, ka, kb, "key.lt")
                        .unwrap();
                    let gt = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SGT, ka, kb, "key.gt")
                        .unwrap();
                    let gt_sel = self
                        .builder
                        .build_select(gt, i64_pos_one, i64_zero, "key.gt.sel")
                        .unwrap()
                        .into_int_value();
                    self.builder
                        .build_select(lt, i64_neg_one, gt_sel, "key.cmp.sel")
                        .unwrap()
                        .into_int_value()
                }
                (BasicValueEnum::FloatValue(ka), BasicValueEnum::FloatValue(kb)) => {
                    // Float key: dispatch to `karac_float_cmp` (total-order
                    // semantics on the bit pattern, equivalent to Rust's
                    // `f64::total_cmp`). f32 keys are widened to f64 first —
                    // the conversion is exact and preserves the total order,
                    // so a single f64 entry-point covers every float width
                    // the language supports. The typechecker accepts floats
                    // here as a sort_by_key-scoped concession; other Ord
                    // consumers still reject them (see check_sort_key_closure
                    // in src/typechecker/stdlib_seq.rs).
                    let f64_t = self.context.f64_type();
                    let ka_f64 = if ka.get_type() == f64_t {
                        ka
                    } else {
                        self.builder
                            .build_float_ext(ka, f64_t, "key.a.f64")
                            .unwrap()
                    };
                    let kb_f64 = if kb.get_type() == f64_t {
                        kb
                    } else {
                        self.builder
                            .build_float_ext(kb, f64_t, "key.b.f64")
                            .unwrap()
                    };
                    let runtime_fn =
                        self.module
                            .get_function("karac_float_cmp")
                            .unwrap_or_else(|| {
                                let fn_ty = i64_t.fn_type(&[f64_t.into(), f64_t.into()], false);
                                self.module.add_function(
                                    "karac_float_cmp",
                                    fn_ty,
                                    Some(Linkage::External),
                                )
                            });
                    let call = self
                        .builder
                        .build_call(
                            runtime_fn,
                            &[
                                BasicMetadataValueEnum::from(ka_f64),
                                BasicMetadataValueEnum::from(kb_f64),
                            ],
                            "key.float.cmp",
                        )
                        .unwrap();
                    call.try_as_basic_value().unwrap_basic().into_int_value()
                }
                (BasicValueEnum::StructValue(ka), BasicValueEnum::StructValue(kb)) => {
                    let struct_ty = ka.get_type();
                    let n_fields = struct_ty.count_fields();
                    if n_fields == 0 {
                        return Err(
                            "Vec.sort_by_key key cannot be an empty tuple / unit type".to_string()
                        );
                    }
                    let all_int = (0..n_fields).all(|i| {
                        struct_ty
                            .get_field_type_at_index(i)
                            .map(|t| t.is_int_type())
                            .unwrap_or(false)
                    });
                    if !all_int {
                        return Err(
                            "Vec.sort_by_key in codegen supports integer and integer-tuple key \
                         types today; use sort_by(|a, b| ...) with an explicit comparator \
                         for other key types"
                                .to_string(),
                        );
                    }
                    // Cascade from the last field backward so the FIRST field
                    // takes priority (its `(neq ? cmp_0 : rest)` wraps the
                    // accumulated rest from fields 1..n).
                    let mut result = i64_zero;
                    for i in (0..n_fields).rev() {
                        let ai = self
                            .builder
                            .build_extract_value(ka, i, &format!("ka.f{}", i))
                            .unwrap()
                            .into_int_value();
                        let bi = self
                            .builder
                            .build_extract_value(kb, i, &format!("kb.f{}", i))
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::SLT,
                                ai,
                                bi,
                                &format!("f{}.lt", i),
                            )
                            .unwrap();
                        let gt = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::SGT,
                                ai,
                                bi,
                                &format!("f{}.gt", i),
                            )
                            .unwrap();
                        let neq = self
                            .builder
                            .build_or(lt, gt, &format!("f{}.neq", i))
                            .unwrap();
                        let gt_sel = self
                            .builder
                            .build_select(gt, i64_pos_one, i64_zero, &format!("f{}.gt.sel", i))
                            .unwrap()
                            .into_int_value();
                        let cmp_i = self
                            .builder
                            .build_select(lt, i64_neg_one, gt_sel, &format!("f{}.cmp", i))
                            .unwrap()
                            .into_int_value();
                        result = self
                            .builder
                            .build_select(neq, cmp_i, result, &format!("f{}.acc", i))
                            .unwrap()
                            .into_int_value();
                    }
                    result
                }
                _ => {
                    return Err(
                        "Vec.sort_by_key in codegen supports integer, integer-tuple, and \
                     String key types today; use sort_by(|a, b| ...) with an explicit \
                     comparator for other key types"
                            .to_string(),
                    );
                }
            }
        };
        self.builder.build_return(Some(&res)).unwrap();

        // 11. Restore outer state.
        self.type_subst = saved_subst;
        self.loop_stack = saved_loop_stack;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_fn_types = saved_cfn;
        self.pending_closure_fn_type = saved_pct;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok((thunk_fn, env_alloca))
    }

    /// Recursive lex-cascade compare for a struct value. Walks `struct_name`'s
    /// fields in declaration order via `self.struct_field_type_names`,
    /// dispatching per field: integer fields use the signed `-1 / 0 / +1`
    /// select; `String` fields call `karac_string_cmp`; fields whose Kāra
    /// type is itself a `Named` struct (present in `struct_field_type_names`)
    /// recurse. The cascade is built last-field-backward into selects
    /// (`result_i = (cmp_i != 0) ? cmp_i : result_{i+1}`), so the first
    /// differing field wins — equivalent to the lex order `#[derive(Ord)]`
    /// would produce. `depth` is threaded into LLVM value names so they
    /// stay unique across recursive entries (the same struct can appear
    /// at multiple depths in a key).
    #[allow(clippy::too_many_lines)]
    pub(super) fn emit_struct_cmp_cascade(
        &mut self,
        ka: inkwell::values::StructValue<'ctx>,
        kb: inkwell::values::StructValue<'ctx>,
        struct_name: &str,
        depth: usize,
    ) -> Result<IntValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let i64_zero = i64_t.const_zero();
        let i64_neg_one = i64_t.const_int((-1i64) as u64, true);
        let i64_pos_one = i64_t.const_int(1, false);

        let field_type_names = match self.struct_field_type_names.get(struct_name).cloned() {
            Some(v) => v,
            None => {
                return Err(format!(
                    "Vec.sort_by_key: struct '{}' has no field-type info in codegen \
                     (struct_field_type_names lookup miss — likely a generic-args \
                     monomorphization edge case)",
                    struct_name
                ));
            }
        };
        let n_fields = ka.get_type().count_fields();
        if n_fields == 0 {
            return Err(format!(
                "Vec.sort_by_key: struct '{}' has zero fields; cannot derive an order",
                struct_name
            ));
        }
        let mut result = i64_zero;
        for i in (0..n_fields).rev() {
            let ai = self
                .builder
                .build_extract_value(ka, i, &format!("d{}.ka.{}.f{}", depth, struct_name, i))
                .unwrap();
            let bi = self
                .builder
                .build_extract_value(kb, i, &format!("d{}.kb.{}.f{}", depth, struct_name, i))
                .unwrap();
            let field_ty_name = field_type_names.get(i as usize).and_then(|o| o.as_deref());
            let cmp_i = match (ai, bi, field_ty_name) {
                (BasicValueEnum::IntValue(av), BasicValueEnum::IntValue(bv), _) => {
                    let lt = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::SLT,
                            av,
                            bv,
                            &format!("d{}.f{}.lt", depth, i),
                        )
                        .unwrap();
                    let gt = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::SGT,
                            av,
                            bv,
                            &format!("d{}.f{}.gt", depth, i),
                        )
                        .unwrap();
                    let gt_sel = self
                        .builder
                        .build_select(
                            gt,
                            i64_pos_one,
                            i64_zero,
                            &format!("d{}.f{}.gt.sel", depth, i),
                        )
                        .unwrap()
                        .into_int_value();
                    self.builder
                        .build_select(lt, i64_neg_one, gt_sel, &format!("d{}.f{}.cmp", depth, i))
                        .unwrap()
                        .into_int_value()
                }
                (
                    BasicValueEnum::StructValue(av),
                    BasicValueEnum::StructValue(bv),
                    Some("String"),
                ) => {
                    let a_ptr = self
                        .builder
                        .build_extract_value(av, 0, &format!("d{}.f{}.ka.ptr", depth, i))
                        .unwrap()
                        .into_pointer_value();
                    let a_len = self
                        .builder
                        .build_extract_value(av, 1, &format!("d{}.f{}.ka.len", depth, i))
                        .unwrap()
                        .into_int_value();
                    let b_ptr = self
                        .builder
                        .build_extract_value(bv, 0, &format!("d{}.f{}.kb.ptr", depth, i))
                        .unwrap()
                        .into_pointer_value();
                    let b_len = self
                        .builder
                        .build_extract_value(bv, 1, &format!("d{}.f{}.kb.len", depth, i))
                        .unwrap()
                        .into_int_value();
                    let runtime_fn =
                        self.module
                            .get_function("karac_string_cmp")
                            .unwrap_or_else(|| {
                                let fn_ty = i64_t.fn_type(
                                    &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                                    false,
                                );
                                self.module.add_function(
                                    "karac_string_cmp",
                                    fn_ty,
                                    Some(Linkage::External),
                                )
                            });
                    let call = self
                        .builder
                        .build_call(
                            runtime_fn,
                            &[
                                BasicMetadataValueEnum::from(a_ptr),
                                BasicMetadataValueEnum::from(a_len),
                                BasicMetadataValueEnum::from(b_ptr),
                                BasicMetadataValueEnum::from(b_len),
                            ],
                            &format!("d{}.f{}.str.cmp", depth, i),
                        )
                        .unwrap();
                    call.try_as_basic_value().unwrap_basic().into_int_value()
                }
                (
                    BasicValueEnum::StructValue(av),
                    BasicValueEnum::StructValue(bv),
                    Some(nested_name),
                ) if self.struct_field_type_names.contains_key(nested_name) => {
                    // Nested struct field: recurse. The nested struct's own
                    // `struct_field_type_names` entry exists at codegen time
                    // because `declare_structs` registers every user struct
                    // before any function body compiles.
                    let nested_name_owned = nested_name.to_string();
                    self.emit_struct_cmp_cascade(av, bv, &nested_name_owned, depth + 1)?
                }
                _ => {
                    return Err(format!(
                        "Vec.sort_by_key: struct '{}' field {} has unsupported type {:?} \
                         for codegen cascade — supported field types today are signed \
                         integers, String, and other registered Named structs. Use \
                         sort_by(|a, b| ...) with an explicit comparator if the struct \
                         has other Ord-implementing field types.",
                        struct_name,
                        i,
                        field_ty_name.unwrap_or("<unknown>"),
                    ));
                }
            };
            let neq = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::NE,
                    cmp_i,
                    i64_zero,
                    &format!("d{}.f{}.neq", depth, i),
                )
                .unwrap();
            result = self
                .builder
                .build_select(neq, cmp_i, result, &format!("d{}.f{}.acc", depth, i))
                .unwrap()
                .into_int_value();
        }
        Ok(result)
    }

    /// Inline-closure fast path for `Vec.sort_by`. Fuses the closure body
    /// into a single `(ctx, *a, *b) -> i64` thunk: the runtime helper calls
    /// directly into a function whose body IS the user comparator, so LLVM
    /// can inline the body across the call (the previous shape went through
    /// a separately-emitted `__closure_N` and an indirect call through the
    /// fat-pointer's fn-pointer field, which the optimiser cannot see
    /// through). Captures are stashed in a stack-allocated env struct in
    /// the outer frame, the alloca is handed to the runtime as `ctx`, and
    /// the thunk re-loads them on entry — same shape `compile_closure` uses
    /// for its `env_ptr`, just with the closure call elided.
    ///
    /// Returns `(thunk_fn, ctx_alloca)`. Caller threads `ctx_alloca` into
    /// `karac_vec_sort_by` as the comparator context.
    #[allow(clippy::too_many_lines)]
    pub(super) fn emit_sort_by_inline_thunk(
        &mut self,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
    ) -> Result<(FunctionValue<'ctx>, PointerValue<'ctx>), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // 1. Captures (mirrors `compile_closure` step 1+2).
        let free_vars = self.collect_closure_free_vars(params, body);
        let env_field_types: Vec<BasicTypeEnum<'ctx>> = if free_vars.is_empty() {
            vec![self.context.i8_type().into()]
        } else {
            free_vars.iter().map(|n| self.variables[n].ty).collect()
        };
        let env_struct_ty = self.context.struct_type(&env_field_types, false);

        // 2. Stack-allocate + populate env in the outer frame.
        let outer_fn = self.current_fn.unwrap();
        let env_alloca = self.create_entry_alloca(outer_fn, "sort_by.env", env_struct_ty.into());
        if !free_vars.is_empty() {
            let mut env_agg = env_struct_ty.get_undef();
            for (i, var_name) in free_vars.iter().enumerate() {
                let slot = self.variables[var_name];
                let val = self
                    .builder
                    .build_load(slot.ty, slot.ptr, var_name)
                    .unwrap();
                env_agg = self
                    .builder
                    .build_insert_value(env_agg, val, i as u32, "env.field")
                    .unwrap()
                    .into_struct_value();
            }
            self.builder.build_store(env_alloca, env_agg).unwrap();
        }

        // 3. Declare thunk: extern "C" fn(ctx, *a, *b) -> i64.
        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_by_inline_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        // 4. Save outer codegen state — we're about to compile into a new fn.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_subst = std::mem::take(&mut self.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_fn_types);
        let saved_pct = self.pending_closure_fn_type.take();

        // 5. Build thunk body.
        self.current_fn = Some(thunk_fn);
        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        let ctx_ptr = thunk_fn.get_nth_param(0).unwrap().into_pointer_value();
        let a_ptr = thunk_fn.get_nth_param(1).unwrap().into_pointer_value();
        let b_ptr = thunk_fn.get_nth_param(2).unwrap().into_pointer_value();

        if !free_vars.is_empty() {
            let env_val = self
                .builder
                .build_load(env_struct_ty, ctx_ptr, "env")
                .unwrap()
                .into_struct_value();
            for (i, var_name) in free_vars.iter().enumerate() {
                let cap_ty = env_field_types[i];
                let field_val = self
                    .builder
                    .build_extract_value(env_val, i as u32, var_name)
                    .unwrap();
                let alloca = self.create_entry_alloca(thunk_fn, var_name, cap_ty);
                self.builder.build_store(alloca, field_val).unwrap();
                self.variables.insert(
                    var_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: cap_ty,
                    },
                );
                if let Some(type_name) = saved_var_types.get(var_name) {
                    self.var_type_names
                        .insert(var_name.clone(), type_name.clone());
                }
            }
        }

        // 6. Bind closure params to typed loads through a_ptr / b_ptr.
        let a_val = self.builder.build_load(elem_ty, a_ptr, "a.val").unwrap();
        let b_val = self.builder.build_load(elem_ty, b_ptr, "b.val").unwrap();
        let param_vals = [a_val, b_val];
        for (i, cp) in params.iter().enumerate().take(2) {
            let val = param_vals[i];
            let param_name = match &cp.pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                _ => format!("_cp{}", i),
            };
            let ty = val.get_type();
            let alloca = self.create_entry_alloca(thunk_fn, &param_name, ty);
            self.builder.build_store(alloca, val).unwrap();
            self.variables
                .insert(param_name, VarSlot { ptr: alloca, ty });
        }

        // 7. Compile body, transform Ordering result → signed `tag - 1`.
        let result = self.compile_expr(body)?;
        let tag = if result.is_struct_value() {
            self.builder
                .build_extract_value(result.into_struct_value(), 0, "tag")
                .unwrap()
                .into_int_value()
        } else {
            result.into_int_value()
        };
        let one = i64_t.const_int(1, false);
        let final_result = self.builder.build_int_sub(tag, one, "result").unwrap();
        self.builder.build_return(Some(&final_result)).unwrap();

        // 8. Restore outer state.
        self.type_subst = saved_subst;
        self.loop_stack = saved_loop_stack;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_fn_types = saved_cfn;
        self.pending_closure_fn_type = saved_pct;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok((thunk_fn, env_alloca))
    }

    /// Emit a per-call-site bridge thunk for `Vec.sort_by`. Signature:
    /// `extern "C" fn(ctx: *mut u8, a_ptr: *const u8, b_ptr: *const u8) -> i64`,
    /// where `ctx` is a pointer to the user closure's spilled fat-pointer
    /// (`{ fn_ptr, env_ptr }`). The thunk loads each element through the
    /// element-type-specific `load`, calls the closure to get an `Ordering`
    /// struct `{ i64 tag }`, and returns `tag - 1` — which yields
    /// `-1 / 0 / +1` for `Less / Equal / Greater` since tags are assigned in
    /// declaration order (see `declare_enums`). The runtime helper
    /// `karac_vec_sort_by` uses that signed value with `Ord::cmp(&0)`.
    /// This is the slow-path fallback for non-inline-closure arguments to
    /// `Vec.sort_by` (e.g. a named function or a closure-typed local);
    /// inline closures route through `emit_sort_by_inline_thunk` above.
    pub(super) fn emit_sort_by_thunk(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
        closure_fn_type: FunctionType<'ctx>,
    ) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_by_thunk_{}", id);

        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(thunk_fn);

        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        let ctx = thunk_fn.get_nth_param(0).unwrap().into_pointer_value();
        let a_ptr = thunk_fn.get_nth_param(1).unwrap().into_pointer_value();
        let b_ptr = thunk_fn.get_nth_param(2).unwrap().into_pointer_value();

        let fat_ty = self.closure_value_type();
        let fat = self
            .builder
            .build_load(fat_ty, ctx, "fat")
            .unwrap()
            .into_struct_value();
        let cls_fn = self
            .builder
            .build_extract_value(fat, 0, "cls.fn")
            .unwrap()
            .into_pointer_value();
        let cls_env = self
            .builder
            .build_extract_value(fat, 1, "cls.env")
            .unwrap()
            .into_pointer_value();

        let a_val = self.builder.build_load(elem_ty, a_ptr, "a").unwrap();
        let b_val = self.builder.build_load(elem_ty, b_ptr, "b").unwrap();

        let call_args: Vec<BasicMetadataValueEnum<'ctx>> = vec![
            BasicMetadataValueEnum::from(cls_env),
            BasicMetadataValueEnum::from(a_val),
            BasicMetadataValueEnum::from(b_val),
        ];
        let call = self
            .builder
            .build_indirect_call(closure_fn_type, cls_fn, &call_args, "ord")
            .unwrap();
        let ord_val = call.try_as_basic_value().unwrap_basic();

        // Ordering lowers to `{ i64 tag }` (unit-only enum with three variants).
        // Extract field 0, defaulting to the raw int if the closure already
        // returns a bare i64 — robust to any future reshape.
        let tag = if ord_val.is_struct_value() {
            self.builder
                .build_extract_value(ord_val.into_struct_value(), 0, "tag")
                .unwrap()
                .into_int_value()
        } else {
            ord_val.into_int_value()
        };

        let one = i64_t.const_int(1, false);
        let result = self.builder.build_int_sub(tag, one, "result").unwrap();
        self.builder.build_return(Some(&result)).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        thunk_fn
    }
}

/// True if `te` is a bit-copyable primitive (i*, u*, f*, bool, char).
/// Conservative: anything else — String, Vec[T], Map, Set, shared T,
/// tuples, structs, enums — needs per-element synth_clone for correct
/// ownership transfer in `Vec.extend_from_slice` / `Vec.from_slice`.
/// Same conservative shape as `ownership::is_copy_type_basic`, but
/// works on the AST `TypeExpr` rather than the resolved `Type`.
pub(super) fn is_trivially_copyable_te(te: &TypeExpr) -> bool {
    let TypeKind::Path(p) = &te.kind else {
        return false;
    };
    if p.segments.len() != 1 {
        return false;
    }
    if p.generic_args.is_some() {
        return false;
    }
    matches!(
        p.segments[0].as_str(),
        "i8" | "i16"
            | "i32"
            | "i64"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}
