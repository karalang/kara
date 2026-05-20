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
use inkwell::types::{BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};
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
            _ => Ok(self.context.i64_type().const_int(0, false).into()),
        }
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
