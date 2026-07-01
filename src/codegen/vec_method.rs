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
    /// Get-or-declare the panicking reallocation wrapper
    /// (`ptr karac_realloc_or_panic(ptr, i64)`, or the `__karac_realloc_or_panic64`
    /// i64-size shim on wasm). The grow paths call this to extend a heap buffer
    /// in place where the allocator can — avoiding the malloc-new + memcpy +
    /// free-old churn and the transient old+new 2× peak. `realloc(null, n)` is
    /// `malloc(n)`, so it is a clean drop-in for any buffer that is null-or-heap
    /// (Vec data always is); a String's static-literal `cap == 0` rodata view is
    /// the one buffer it must NOT touch — those grow paths guard with `cap > 0`.
    pub(super) fn realloc_or_panic_fn_decl(&self) -> inkwell::values::FunctionValue<'ctx> {
        let sym = crate::codegen::driver::c_realloc_or_panic_symbol();
        self.module.get_function(sym).unwrap_or_else(|| {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i64_t = self.context.i64_type();
            let ty = ptr_ty.fn_type(&[ptr_ty.into(), i64_t.into()], false);
            self.module.add_function(sym, ty, Some(Linkage::External))
        })
    }

    /// Grow a String's byte buffer to `new_cap` bytes, preserving the first
    /// `len` bytes, and return the new data pointer (builder left positioned at
    /// the merge block, ready for the `data`/`cap` stores). A heap buffer
    /// (`cap > 0`) is `realloc`'d so the allocator can extend it in place; a
    /// static-literal / empty buffer (`cap == 0` — its pointer is in the
    /// read-only string pool, or null) is **not** realloc'd or freed, taking a
    /// fresh malloc + copy instead. Shared by `String.push` and
    /// `String.push_str`; `prefix` namespaces the emitted basic blocks/values.
    pub(super) fn emit_string_buffer_grow(
        &self,
        fn_val: inkwell::values::FunctionValue<'ctx>,
        data: inkwell::values::PointerValue<'ctx>,
        cap: inkwell::values::IntValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        new_cap: inkwell::values::IntValue<'ctx>,
        prefix: &str,
    ) -> inkwell::values::PointerValue<'ctx> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let zero_val = i64_t.const_int(0, false);
        let was_heap = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGT,
                cap,
                zero_val,
                &format!("{prefix}.was_heap"),
            )
            .unwrap();
        let realloc_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.realloc"));
        let fresh_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.fresh"));
        let grow_done_bb = self
            .context
            .append_basic_block(fn_val, &format!("{prefix}.grow_done"));
        self.builder
            .build_conditional_branch(was_heap, realloc_bb, fresh_bb)
            .unwrap();

        // Heap path: realloc(data, new_cap) — extend in place where possible
        // (realloc preserves the first `len` bytes, so no separate memcpy).
        self.builder.position_at_end(realloc_bb);
        let realloc_fn = self.realloc_or_panic_fn_decl();
        let re_data = self
            .builder
            .build_call(
                realloc_fn,
                &[data.into(), new_cap.into()],
                &format!("{prefix}.re_data"),
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_unconditional_branch(grow_done_bb)
            .unwrap();
        let realloc_pred = self.builder.get_insert_block().unwrap();

        // Static/null path: fresh malloc + copy the old `len` bytes; the old
        // buffer is rodata or null, so it is neither freed nor moved.
        self.builder.position_at_end(fresh_bb);
        let fr_data = self
            .builder
            .build_call(
                self.alloc_or_panic_fn,
                &[new_cap.into()],
                &format!("{prefix}.fr_data"),
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_memcpy(fr_data, 1, data, 1, len).unwrap();
        self.builder
            .build_unconditional_branch(grow_done_bb)
            .unwrap();
        let fresh_pred = self.builder.get_insert_block().unwrap();

        // Merge: pick the grown buffer pointer.
        self.builder.position_at_end(grow_done_bb);
        let new_data_phi = self
            .builder
            .build_phi(ptr_ty, &format!("{prefix}.new_data"))
            .unwrap();
        new_data_phi.add_incoming(&[(&re_data, realloc_pred), (&fr_data, fresh_pred)]);
        new_data_phi.as_basic_value().into_pointer_value()
    }

    /// Build `Result.Err(AllocError.OutOfMemory{requested_bytes})` — the OOM
    /// arm every fallible `try_*` collection method returns when
    /// `karac_alloc_fallible` yields null (phase-8-stdlib-floor item 8).
    pub(super) fn build_alloc_oom_result(
        &mut self,
        requested_bytes: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let alloc_err = self.build_nonshared_enum_value(
            "AllocError",
            "OutOfMemory",
            &[requested_bytes.into()],
        )?;
        self.build_nonshared_enum_value("Result", "Err", &[alloc_err])
    }

    /// Load `{data, len}` (fields 0 and 1) from a `{ptr, len, cap}` String
    /// struct at `data_ptr`. `tag` prefixes the IR value names.
    fn load_string_data_len(
        &self,
        vec_ty: inkwell::types::StructType<'ctx>,
        data_ptr: PointerValue<'ctx>,
        tag: &str,
    ) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let data_p = self
            .builder
            .build_struct_gep(vec_ty, data_ptr, 0, &format!("{tag}.recv.ptr.p"))
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_p, &format!("{tag}.recv.ptr"))
            .unwrap()
            .into_pointer_value();
        let len_p = self
            .builder
            .build_struct_gep(vec_ty, data_ptr, 1, &format!("{tag}.recv.len.p"))
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_p, &format!("{tag}.recv.len"))
            .unwrap()
            .into_int_value();
        (data, len)
    }

    /// Call an allocating String→String runtime helper whose final parameter is
    /// an `*mut i64 out_len` and which returns the fresh buffer pointer, then
    /// build the `{ptr, out_len, out_len}` (cap == len) String aggregate. `args`
    /// are the helper's leading parameters (the out-len slot is appended here).
    fn build_string_xform_result(
        &self,
        func: FunctionValue<'ctx>,
        mut args: Vec<BasicMetadataValueEnum<'ctx>>,
        name: &str,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let out_len_slot = self.create_entry_alloca(fn_val, "xform.outlen", i64_t.into());
        args.push(out_len_slot.into());
        let new_ptr = self
            .builder
            .build_call(func, &args, name)
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let new_len = self
            .builder
            .build_load(i64_t, out_len_slot, "xform.len")
            .unwrap()
            .into_int_value();
        let str_ty = self.vec_struct_type();
        let mut out = str_ty.get_undef();
        out = self
            .builder
            .build_insert_value(out, new_ptr, 0, "xform.ptr")
            .unwrap()
            .into_struct_value();
        out = self
            .builder
            .build_insert_value(out, new_len, 1, "xform.len.f")
            .unwrap()
            .into_struct_value();
        out = self
            .builder
            .build_insert_value(out, new_len, 2, "xform.cap")
            .unwrap()
            .into_struct_value();
        out.into()
    }

    /// The element type NAME of a `Vec`/`Slice` variable (`var_elem_type_exprs`
    /// records the element `TypeExpr`), e.g. `"i64"` / `"u32"` / `"String"`.
    /// `None` when the binding has no recorded element type or it isn't a plain
    /// named type.
    pub(super) fn vec_elem_type_name(&self, var_name: &str) -> Option<String> {
        match self.var_elem_type_exprs.get(var_name).map(|te| &te.kind) {
            Some(TypeKind::Path(p)) => p.segments.last().cloned(),
            _ => None,
        }
    }

    /// Three-way compare of an `elem` against the search `needle` for
    /// `Vec.binary_search`, returning an i64 sign (`<0` / `0` / `>0`) consistent
    /// with the interpreter's `value_compare`. Integer elements (any width,
    /// signed or unsigned) widen to i64 and compare signed (uint values are
    /// non-negative i64, matching the interpreter's signed `i64::cmp`); `String`
    /// elements route through `karac_string_cmp` (the same byte-lexicographic
    /// order). Other element types are an honest "not yet supported" error — the
    /// interpreter still handles them under `karac run`. Emits no basic blocks
    /// (pure data-flow), so the caller's bisection loop stays simple.
    fn emit_binary_search_cmp(
        &mut self,
        elem_val: BasicValueEnum<'ctx>,
        needle_val: BasicValueEnum<'ctx>,
        elem_name: &str,
    ) -> Result<IntValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let is_uint = matches!(elem_name, "u8" | "u16" | "u32" | "u64" | "usize");
        let is_int = is_uint || matches!(elem_name, "i8" | "i16" | "i32" | "i64" | "isize");
        if is_int {
            let (BasicValueEnum::IntValue(a), BasicValueEnum::IntValue(b)) = (elem_val, needle_val)
            else {
                return Err("Vec.binary_search: integer element/needle expected".to_string());
            };
            let a = self.widen_int_to_i64(a.into(), is_uint);
            let b = self.widen_int_to_i64(b.into(), is_uint);
            let lt = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, a, b, "bs.lt")
                .unwrap();
            let gt = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SGT, a, b, "bs.gt")
                .unwrap();
            let neg1 = i64_t.const_int((-1i64) as u64, true);
            let pos1 = i64_t.const_int(1, false);
            let zero = i64_t.const_zero();
            let gt_sel = self
                .builder
                .build_select(gt, pos1, zero, "bs.gtsel")
                .unwrap()
                .into_int_value();
            Ok(self
                .builder
                .build_select(lt, neg1, gt_sel, "bs.cmp")
                .unwrap()
                .into_int_value())
        } else if elem_name == "String" {
            let (BasicValueEnum::StructValue(a), BasicValueEnum::StructValue(b)) =
                (elem_val, needle_val)
            else {
                return Err("Vec.binary_search: String element/needle expected".to_string());
            };
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let a_ptr = self
                .builder
                .build_extract_value(a, 0, "bs.a.ptr")
                .unwrap()
                .into_pointer_value();
            let a_len = self
                .builder
                .build_extract_value(a, 1, "bs.a.len")
                .unwrap()
                .into_int_value();
            let b_ptr = self
                .builder
                .build_extract_value(b, 0, "bs.b.ptr")
                .unwrap()
                .into_pointer_value();
            let b_len = self
                .builder
                .build_extract_value(b, 1, "bs.b.len")
                .unwrap()
                .into_int_value();
            let cmp_fn = self
                .module
                .get_function("karac_string_cmp")
                .unwrap_or_else(|| {
                    let fn_ty = i64_t.fn_type(
                        &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                        false,
                    );
                    self.module
                        .add_function("karac_string_cmp", fn_ty, Some(Linkage::External))
                });
            Ok(self
                .builder
                .build_call(
                    cmp_fn,
                    &[a_ptr.into(), a_len.into(), b_ptr.into(), b_len.into()],
                    "bs.scmp",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value())
        } else {
            Err(format!(
                "`Vec.binary_search` on element type `{elem_name}` is not yet supported under \
                 `karac build` (codegen); it works under `karac run`. Integer and String \
                 element types are supported."
            ))
        }
    }

    /// Emit `binary_search(needle)` over a contiguous buffer `data` of `len`
    /// elements (LLVM type `elem_ty`, Kāra type name `elem_name`), returning an
    /// `Option[i64]` aggregate. Shared by the `Vec` and `Slice` receiver paths
    /// (they differ only in how `data`/`len` are loaded from their headers).
    ///
    /// Replicates Rust's current `slice::binary_search_by` (branchless
    /// narrow-to-`base`) EXACTLY — the textbook return-on-first-equal variant
    /// picks a different index among duplicate keys, and the interpreter uses
    /// std's, so codegen must match it:
    ///   size = len; base = 0
    ///   while size > 1 { half = size/2; mid = base + half;
    ///       base = cmp(v[mid], x) > 0 ? base : mid; size -= half }
    ///   cmp(v[base], x) == 0 ? Some(base) : None
    pub(super) fn compile_binary_search(
        &mut self,
        data: PointerValue<'ctx>,
        len: IntValue<'ctx>,
        elem_ty: inkwell::types::BasicTypeEnum<'ctx>,
        elem_name: &str,
        needle_arg: &CallArg,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        // Evaluate the needle once, before the loop.
        let needle_val = self.compile_expr(&needle_arg.value)?;

        let fn_val = self.current_fn.unwrap();
        let head_bb = self.context.append_basic_block(fn_val, "bs.head");
        let body_bb = self.context.append_basic_block(fn_val, "bs.body");
        let final_bb = self.context.append_basic_block(fn_val, "bs.final");
        let found_bb = self.context.append_basic_block(fn_val, "bs.found");
        let none_bb = self.context.append_basic_block(fn_val, "bs.none");
        let merge_bb = self.context.append_basic_block(fn_val, "bs.merge");

        let size_slot = self.create_entry_alloca(fn_val, "bs.size", i64_t.into());
        let base_slot = self.create_entry_alloca(fn_val, "bs.base", i64_t.into());
        self.builder.build_store(size_slot, len).unwrap();
        self.builder
            .build_store(base_slot, i64_t.const_zero())
            .unwrap();
        // Empty receiver → None (the loop + final load assume a valid base).
        let is_empty = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                len,
                i64_t.const_zero(),
                "bs.empty",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, none_bb, head_bb)
            .unwrap();

        // head: continue while size > 1.
        self.builder.position_at_end(head_bb);
        let size = self
            .builder
            .build_load(i64_t, size_slot, "bs.size.l")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                size,
                i64_t.const_int(1, false),
                "bs.cont",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body_bb, final_bb)
            .unwrap();

        // body: half = size/2; mid = base + half; base = cmp>0 ? base : mid;
        //       size -= half.
        self.builder.position_at_end(body_bb);
        let size_b = self
            .builder
            .build_load(i64_t, size_slot, "bs.size.b")
            .unwrap()
            .into_int_value();
        let base_b = self
            .builder
            .build_load(i64_t, base_slot, "bs.base.b")
            .unwrap()
            .into_int_value();
        let half = self
            .builder
            .build_right_shift(size_b, i64_t.const_int(1, false), false, "bs.half")
            .unwrap();
        let mid = self.builder.build_int_add(base_b, half, "bs.mid").unwrap();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[mid], "bs.elem.p")
                .unwrap()
        };
        let elem_val = self
            .builder
            .build_load(elem_ty, elem_ptr, "bs.elem")
            .unwrap();
        let sign = self.emit_binary_search_cmp(elem_val, needle_val, elem_name)?;
        let is_gt = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                sign,
                i64_t.const_zero(),
                "bs.is.gt",
            )
            .unwrap();
        let new_base = self
            .builder
            .build_select(is_gt, base_b, mid, "bs.new.base")
            .unwrap()
            .into_int_value();
        self.builder.build_store(base_slot, new_base).unwrap();
        let new_size = self
            .builder
            .build_int_sub(size_b, half, "bs.new.size")
            .unwrap();
        self.builder.build_store(size_slot, new_size).unwrap();
        self.builder.build_unconditional_branch(head_bb).unwrap();

        // final: cmp(v[base], x) == 0 ? Some(base) : None.
        self.builder.position_at_end(final_bb);
        let base_f = self
            .builder
            .build_load(i64_t, base_slot, "bs.base.f")
            .unwrap()
            .into_int_value();
        let elem_f_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[base_f], "bs.elem.f.p")
                .unwrap()
        };
        let elem_f = self
            .builder
            .build_load(elem_ty, elem_f_ptr, "bs.elem.f")
            .unwrap();
        let sign_f = self.emit_binary_search_cmp(elem_f, needle_val, elem_name)?;
        let is_eq = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::EQ,
                sign_f,
                i64_t.const_zero(),
                "bs.is.eq",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_eq, found_bb, none_bb)
            .unwrap();

        // found: carry `base` into the Some phi.
        self.builder.position_at_end(found_bb);
        let found_base = self
            .builder
            .build_load(i64_t, base_slot, "bs.found.base")
            .unwrap()
            .into_int_value();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // none: not found.
        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // merge: Some(found_base) from found_bb, None from none_bb.
        self.builder.position_at_end(merge_bb);
        let agg = self.build_option_some_via_phis(&[found_base], found_bb, none_bb, "bs.opt");
        // A fresh-owned String needle temp (`v.binary_search(make_s())`) must be
        // freed; a borrowed/literal needle is a no-op.
        if needle_val.is_struct_value() {
            self.free_fresh_owned_str_arg(&needle_arg.value, needle_val);
        }
        Ok(agg)
    }

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
            // `Vec[T].as_ptr()` / `.as_mut_ptr()` — raw element-0 pointer of
            // the heap buffer, the FFI handoff (mirrors `Array.as_ptr` /
            // `CStr.as_ptr`; typed `*const T` / `*mut T` by the `as_ptr` arm
            // in `infer_method_call`). Field 0 of the `{ptr, len, cap}` header
            // IS the data buffer pointer — load + hand it out (both spellings
            // lower to the same LLVM `ptr`). The buffer must outlive the call;
            // a *synchronous* host fn (a framebuffer blit reads the bytes
            // before returning) satisfies that, so the pointer never dangles
            // while in use. The pointer carries no lifetime — the unsafe
            // contract is the programmer's (design.md § FFI).
            "as_ptr" | "as_mut_ptr" => {
                let buf_ptr_field = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vec.asptr.p")
                    .unwrap();
                let buf = self
                    .builder
                    .build_load(ptr_ty, buf_ptr_field, "vec.asptr")
                    .unwrap();
                Ok(buf)
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
            "starts_with" | "ends_with" => {
                if args.is_empty() {
                    return Err(format!("String.{method} requires an argument"));
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

                // Compare prefix.len bytes — the first ones for `starts_with`,
                // the trailing ones (`recv.data + (recv_len - prefix_len)`) for
                // `ends_with`. `has_len` (recv_len >= prefix_len) guards this
                // block, so the byte offset is non-negative. memcmp returns 0
                // iff equal.
                self.builder.position_at_end(cmp_bb);
                let cmp_ptr = if method == "ends_with" {
                    let off = self
                        .builder
                        .build_int_sub(recv_len, prefix_len, "ew.off")
                        .unwrap();
                    unsafe {
                        self.builder
                            .build_gep(self.context.i8_type(), recv_data, &[off], "ew.cmp.ptr")
                            .unwrap()
                    }
                } else {
                    recv_data
                };
                let cmp_result = self
                    .builder
                    .build_call(
                        self.memcmp_fn,
                        &[cmp_ptr.into(), prefix_data.into(), prefix_len.into()],
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
                // Free a fresh-owned String prefix temp (`s.starts_with(tok)`
                // where `tok` is a substring / call result). The comparison is
                // complete at `cont_bb`, so the buffer is no longer read.
                self.free_fresh_owned_str_arg(&args[0].value, prefix_val);
                Ok(result)
            }
            // `String.split(sep) -> Vec[String]` (GAP-W2). Delegates to the
            // runtime `karac_runtime_string_split`, which builds the
            // `Vec[String]` `{data, len, cap}` with malloc'd buffers (the
            // Vec buffer + each element String's buffer) the binding's
            // scope-exit drop frees. Out-param ABI (pointer args only — no
            // struct return). `sep` is a `char` (UTF-8 encoded here) or a
            // `String` (its `{data, len}`). All targets: on wasm the runtime
            // helper allocates from the unified wasi-libc heap (`wasm_alloc.rs`)
            // that codegen's `free` reclaims from.
            "split" => {
                if args.is_empty() {
                    return Err("String.split requires a separator argument".to_string());
                }
                // Receiver `{data, len}`.
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "spl.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "spl.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "spl.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "spl.recv.len")
                    .unwrap()
                    .into_int_value();

                // Separator (ptr, len): a `char` UTF-8-encodes to a stack buf;
                // a `String` contributes its `{data, len}` directly.
                let sep_val = self.compile_expr(&args[0].value)?;
                let (sep_ptr, sep_len): (PointerValue<'ctx>, IntValue<'ctx>) = match sep_val {
                    BasicValueEnum::IntValue(cp) => self.emit_codepoint_to_utf8(cp),
                    BasicValueEnum::StructValue(sv) => {
                        let d = self
                            .builder
                            .build_extract_value(sv, 0, "spl.sep.ptr")
                            .unwrap()
                            .into_pointer_value();
                        let l = self
                            .builder
                            .build_extract_value(sv, 1, "spl.sep.len")
                            .unwrap()
                            .into_int_value();
                        (d, l)
                    }
                    _ => return Err("String.split separator must be a char or String".to_string()),
                };

                let split_fn = match self.module.get_function("karac_runtime_string_split") {
                    Some(f) => f,
                    None => {
                        let ft = self.context.void_type().fn_type(
                            &[
                                ptr_ty.into(), // s
                                i64_t.into(),  // s_len
                                ptr_ty.into(), // sep
                                i64_t.into(),  // sep_len
                                ptr_ty.into(), // out_data
                                ptr_ty.into(), // out_len
                                ptr_ty.into(), // out_cap
                            ],
                            false,
                        );
                        self.module.add_function(
                            "karac_runtime_string_split",
                            ft,
                            Some(Linkage::External),
                        )
                    }
                };

                // Result Vec slot; pass pointers to its three fields as the
                // out-params, then load the assembled `{data, len, cap}`.
                let fn_val = self.current_fn.unwrap();
                let result_slot = self.create_entry_alloca(fn_val, "spl.result", vec_ty.into());
                let out_data = self
                    .builder
                    .build_struct_gep(vec_ty, result_slot, 0, "spl.out.data")
                    .unwrap();
                let out_len = self
                    .builder
                    .build_struct_gep(vec_ty, result_slot, 1, "spl.out.len")
                    .unwrap();
                let out_cap = self
                    .builder
                    .build_struct_gep(vec_ty, result_slot, 2, "spl.out.cap")
                    .unwrap();
                self.builder
                    .build_call(
                        split_fn,
                        &[
                            recv_data.into(),
                            recv_len.into(),
                            sep_ptr.into(),
                            sep_len.into(),
                            out_data.into(),
                            out_len.into(),
                            out_cap.into(),
                        ],
                        "",
                    )
                    .unwrap();

                // Free a fresh-owned String separator temp (e.g. `s.split("::")`)
                // — the runtime copied its bytes into the pieces. No-op for a
                // char separator (no String to free).
                if sep_val.is_struct_value() {
                    self.free_fresh_owned_str_arg(&args[0].value, sep_val);
                }

                let result = self
                    .builder
                    .build_load(vec_ty, result_slot, "spl.load")
                    .unwrap();
                Ok(result)
            }
            "slice" => {
                // `String.slice(start, end) -> StringSlice` — a zero-copy
                // borrowed view over the half-open byte range `[start, end)`:
                // the aggregate `{recv_data + start, end - start, cap = 0}`. No
                // allocation, no memcpy (unlike `substring`, which copies into
                // an owned String). `cap == 0` marks it a non-owning borrow, so
                // the scope-exit drop's `cap > 0` guard no-ops — the view never
                // frees the source's buffer (design.md § StringSlice). Bounds
                // saturate like `substring`: a negative / past-end `start`, or
                // an empty range, yields the empty view `{null, 0, 0}`.
                if args.len() != 2 {
                    return Err("String.slice requires start and end index arguments".to_string());
                }
                let str_ty = self.vec_struct_type();
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "sl.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "sl.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "sl.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "sl.recv.len")
                    .unwrap()
                    .into_int_value();

                let start = self.compile_expr(&args[0].value)?.into_int_value();
                let end_raw = self.compile_expr(&args[1].value)?.into_int_value();
                let zero64 = i64_t.const_zero();
                // end = max(min(end_raw, len), start) — clamp into [start, len].
                let e_lt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, end_raw, recv_len, "sl.e.lt")
                    .unwrap();
                let e_min = self
                    .builder
                    .build_select(e_lt, end_raw, recv_len, "sl.e.min")
                    .unwrap()
                    .into_int_value();
                let e_gt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, e_min, start, "sl.e.gt")
                    .unwrap();
                let end = self
                    .builder
                    .build_select(e_gt, e_min, start, "sl.e")
                    .unwrap()
                    .into_int_value();

                // empty = (start < 0) || (start > len) || (end == start)
                let s_lt0 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, start, zero64, "sl.s.lt0")
                    .unwrap();
                let s_gt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, start, recv_len, "sl.s.gtlen")
                    .unwrap();
                let empty_rng = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, end, start, "sl.empty.cmp")
                    .unwrap();
                let oob = self.builder.build_or(s_lt0, s_gt, "sl.oob").unwrap();
                let out_of_range = self.builder.build_or(oob, empty_rng, "sl.empty").unwrap();

                let fn_val = self.current_fn.unwrap();
                let view_bb = self.context.append_basic_block(fn_val, "sl.view");
                let empty_bb = self.context.append_basic_block(fn_val, "sl.empty");
                let cont_bb = self.context.append_basic_block(fn_val, "sl.cont");
                let result_slot = self.create_entry_alloca(fn_val, "sl.result", str_ty.into());
                self.builder
                    .build_conditional_branch(out_of_range, empty_bb, view_bb)
                    .unwrap();

                // Empty: {null, 0, 0}.
                self.builder.position_at_end(empty_bb);
                let null = ptr_ty.const_null();
                let mut empty_agg = str_ty.get_undef();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, null, 0, "sl.empty.ptr")
                    .unwrap()
                    .into_struct_value();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, zero64, 1, "sl.empty.len")
                    .unwrap()
                    .into_struct_value();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, zero64, 2, "sl.empty.cap")
                    .unwrap()
                    .into_struct_value();
                self.builder.build_store(result_slot, empty_agg).unwrap();
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                // View: {recv_data + start, end - start, cap = 0}. No alloc.
                self.builder.position_at_end(view_bb);
                let view_ptr = unsafe {
                    self.builder
                        .build_gep(self.context.i8_type(), recv_data, &[start], "sl.view.ptr")
                        .unwrap()
                };
                let view_len = self
                    .builder
                    .build_int_nsw_sub(end, start, "sl.view.len")
                    .unwrap();
                let mut view_agg = str_ty.get_undef();
                view_agg = self
                    .builder
                    .build_insert_value(view_agg, view_ptr, 0, "sl.view.f0")
                    .unwrap()
                    .into_struct_value();
                view_agg = self
                    .builder
                    .build_insert_value(view_agg, view_len, 1, "sl.view.f1")
                    .unwrap()
                    .into_struct_value();
                view_agg = self
                    .builder
                    .build_insert_value(view_agg, zero64, 2, "sl.view.f2")
                    .unwrap()
                    .into_struct_value();
                self.builder.build_store(result_slot, view_agg).unwrap();
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                self.builder.position_at_end(cont_bb);
                let result = self
                    .builder
                    .build_load(str_ty, result_slot, "sl.load")
                    .unwrap();
                Ok(result)
            }
            "find" => {
                // `String.find(needle) -> Option[i64]` — byte offset of the
                // first occurrence of `needle` (a `char` or `String`), else
                // `None`. Inline scan mirroring `contains`'s `memcmp` window
                // loop, but the result is `Some(i)` (the match offset) rather
                // than a bool. The needle's `{ptr,len}`: a `char` UTF-8-encodes
                // to a stack buffer; a `String` contributes its bytes directly
                // (same as `split`). Empty needle → `Some(0)` (memcmp of 0
                // bytes matches at i=0), matching Rust `str::find`.
                if args.len() != 1 {
                    return Err("String.find requires a needle argument".to_string());
                }
                let i8_t = self.context.i8_type();
                let i32_t = self.context.i32_type();
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "fd.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "fd.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "fd.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "fd.recv.len")
                    .unwrap()
                    .into_int_value();

                let needle_val = self.compile_expr(&args[0].value)?;
                let (needle_data, needle_len): (PointerValue<'ctx>, IntValue<'ctx>) =
                    match needle_val {
                        BasicValueEnum::IntValue(cp) => self.emit_codepoint_to_utf8(cp),
                        BasicValueEnum::StructValue(sv) => {
                            let d = self
                                .builder
                                .build_extract_value(sv, 0, "fd.needle.ptr")
                                .unwrap()
                                .into_pointer_value();
                            let l = self
                                .builder
                                .build_extract_value(sv, 1, "fd.needle.len")
                                .unwrap()
                                .into_int_value();
                            (d, l)
                        }
                        _ => return Err("String.find needle must be a char or String".to_string()),
                    };

                let fn_val = self.current_fn.unwrap();
                let head_bb = self.context.append_basic_block(fn_val, "fd.head");
                let body_bb = self.context.append_basic_block(fn_val, "fd.body");
                let found_bb = self.context.append_basic_block(fn_val, "fd.found");
                let next_bb = self.context.append_basic_block(fn_val, "fd.next");
                let none_bb = self.context.append_basic_block(fn_val, "fd.none");
                let merge_bb = self.context.append_basic_block(fn_val, "fd.merge");

                let i_slot = self.create_entry_alloca(fn_val, "fd.i", i64_t.into());
                self.builder
                    .build_store(i_slot, i64_t.const_zero())
                    .unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                // head: continue while `i + needle_len <= recv_len`.
                self.builder.position_at_end(head_bb);
                let i = self
                    .builder
                    .build_load(i64_t, i_slot, "fd.i.load")
                    .unwrap()
                    .into_int_value();
                let i_end = self
                    .builder
                    .build_int_add(i, needle_len, "fd.i_end")
                    .unwrap();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULE, i_end, recv_len, "fd.in_range")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, none_bb)
                    .unwrap();

                // body: memcmp(recv_data + i, needle_data, needle_len) == 0?
                self.builder.position_at_end(body_bb);
                let window = unsafe {
                    self.builder
                        .build_gep(i8_t, recv_data, &[i], "fd.window")
                        .unwrap()
                };
                let cmp = self
                    .builder
                    .build_call(
                        self.memcmp_fn,
                        &[window.into(), needle_data.into(), needle_len.into()],
                        "fd.memcmp",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let is_match = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        cmp,
                        i32_t.const_zero(),
                        "fd.match",
                    )
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_match, found_bb, next_bb)
                    .unwrap();

                // found: carry `i` (the match offset) into the Some phi.
                self.builder.position_at_end(found_bb);
                let found_off = self
                    .builder
                    .build_load(i64_t, i_slot, "fd.found.off")
                    .unwrap()
                    .into_int_value();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // next: i++, loop.
                self.builder.position_at_end(next_bb);
                let i_next = self
                    .builder
                    .build_int_add(i, i64_t.const_int(1, false), "fd.i.next")
                    .unwrap();
                self.builder.build_store(i_slot, i_next).unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                // none: not found.
                self.builder.position_at_end(none_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // merge: Some(found_off) from `found_bb`, None from `none_bb`.
                self.builder.position_at_end(merge_bb);
                let agg =
                    self.build_option_some_via_phis(&[found_off], found_bb, none_bb, "fd.opt");
                // Free a fresh-owned String needle temp (`s.find("foo")`); a
                // char needle used a stack buffer (no free). The scan is done.
                if needle_val.is_struct_value() {
                    self.free_fresh_owned_str_arg(&args[0].value, needle_val);
                }
                Ok(agg)
            }
            "char_count" => {
                // `String.char_count() -> i64` — O(n) Unicode scalar count
                // (design.md § String, vs the O(1) byte count `s.bytes().len()`).
                // Load the String's {ptr,len} and call the runtime decoder-counter.
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "cc.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "cc.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "cc.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "cc.len")
                    .unwrap()
                    .into_int_value();
                let f = self
                    .module
                    .get_function("karac_runtime_string_char_count")
                    .expect("char_count extern declared in Codegen::new");
                let cnt = self
                    .builder
                    .build_call(f, &[recv_data.into(), recv_len.into()], "cc.count")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                Ok(cnt)
            }
            "char_at" => {
                // `String.char_at(idx) -> Option[char]` — O(n) Unicode-aware
                // access (design.md § String: returns `None` past the end, no
                // panic). The runtime decoder writes the idx-th scalar's
                // codepoint through an out-slot and returns 1 in range / 0 past
                // the end; branch into Some(char)/None and phi-merge the Option
                // aggregate, mirroring `find`'s `Option[i64]` shape (the char
                // codepoint is zero-extended into the i64 payload word).
                if args.len() != 1 {
                    return Err("String.char_at requires an index argument".to_string());
                }
                let i32_t = self.context.i32_type();
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "ca.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "ca.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "ca.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "ca.len")
                    .unwrap()
                    .into_int_value();
                let idx = self.compile_expr(&args[0].value)?.into_int_value();
                let fn_val = self.current_fn.unwrap();
                let out_cp = self.create_entry_alloca(fn_val, "ca.out_cp", i32_t.into());
                let f = self
                    .module
                    .get_function("karac_runtime_string_char_at")
                    .expect("char_at extern declared in Codegen::new");
                let found = self
                    .builder
                    .build_call(
                        f,
                        &[recv_data.into(), recv_len.into(), idx.into(), out_cp.into()],
                        "ca.found",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let is_some = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        found,
                        self.context.i8_type().const_zero(),
                        "ca.is_some",
                    )
                    .unwrap();
                let some_bb = self.context.append_basic_block(fn_val, "ca.some");
                let none_bb = self.context.append_basic_block(fn_val, "ca.none");
                let merge_bb = self.context.append_basic_block(fn_val, "ca.merge");
                self.builder
                    .build_conditional_branch(is_some, some_bb, none_bb)
                    .unwrap();

                // some: load the codepoint, zero-extend to the i64 payload word.
                self.builder.position_at_end(some_bb);
                let cp = self
                    .builder
                    .build_load(i32_t, out_cp, "ca.cp")
                    .unwrap()
                    .into_int_value();
                let cp_word = self
                    .builder
                    .build_int_z_extend(cp, i64_t, "ca.cp.word")
                    .unwrap();
                let some_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // none: past the end.
                self.builder.position_at_end(none_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // merge: Some(codepoint) from some_end, None from none_bb.
                self.builder.position_at_end(merge_bb);
                let agg = self.build_option_some_via_phis(&[cp_word], some_end, none_bb, "ca.opt");
                Ok(agg)
            }
            // `String.substring(start) -> String` — bytes from `start` to end.
            // `String.substring(start, end) -> String` — bytes in `[start, end)`.
            // Both indices are byte offsets (matching the `bytes()` view).
            // Out-of-range / negative / inverted bounds saturate to an empty
            // String, so the self-hosted lexer can do
            // `source.substring(start, current)` for `token_text` and the
            // `[2..]` hex/bin/oct prefix strip.
            //
            // Implementation:
            //   1. Load receiver `{data, len}`.
            //   2. Evaluate `start`, and `end` (= len if the one-arg form).
            //   3. Clamp: `s = clamp(start, 0, len)`, `e = clamp(end, s, len)`,
            //      `new_len = e - s`. If `new_len == 0`, produce an empty
            //      String `{null, 0, 0}`.
            //   4. Otherwise malloc `new_len` bytes, memcpy from `data + s`,
            //      and assemble `{buf, new_len, new_len}` (cap == len so the
            //      buffer is freed at scope exit).
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

                // Evaluate start; end defaults to len for the one-arg form.
                let start_raw = self.compile_expr(&args[0].value)?.into_int_value();
                let end_raw = if args.len() >= 2 {
                    self.compile_expr(&args[1].value)?.into_int_value()
                } else {
                    recv_len
                };

                let zero64 = i64_t.const_zero();
                // smin/smax via select.
                let smin = |b: &Self,
                            a: inkwell::values::IntValue<'ctx>,
                            c: inkwell::values::IntValue<'ctx>,
                            n: &str| {
                    let lt = b
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SLT, a, c, "ss.min.cmp")
                        .unwrap();
                    b.builder
                        .build_select(lt, a, c, n)
                        .unwrap()
                        .into_int_value()
                };
                let smax = |b: &Self,
                            a: inkwell::values::IntValue<'ctx>,
                            c: inkwell::values::IntValue<'ctx>,
                            n: &str| {
                    let gt = b
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SGT, a, c, "ss.max.cmp")
                        .unwrap();
                    b.builder
                        .build_select(gt, a, c, n)
                        .unwrap()
                        .into_int_value()
                };
                // Established one-arg contract: a `start` that is negative or
                // past the end yields an empty String (not a clamp-to-0). Keep
                // that for both forms. `start` is then in [0, len]; `end` is
                // clamped to [start, len]; an empty range yields empty too.
                let start = start_raw;
                let end = smax(
                    self,
                    smin(self, end_raw, recv_len, "ss.e.min"),
                    start,
                    "ss.e",
                );

                // empty = (start < 0) || (start > len) || (end == start)
                let start_lt0 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, start, zero64, "ss.s.lt0")
                    .unwrap();
                let start_gt_len = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, start, recv_len, "ss.s.gtlen")
                    .unwrap();
                let empty_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, end, start, "ss.empty.cmp")
                    .unwrap();
                let oob = self
                    .builder
                    .build_or(start_lt0, start_gt_len, "ss.oob")
                    .unwrap();
                let out_of_range = self.builder.build_or(oob, empty_range, "ss.empty").unwrap();

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
                    .build_int_nsw_sub(end, start, "ss.new_len")
                    .unwrap();
                let buf = self
                    .builder
                    .build_call(self.alloc_or_panic_fn, &[new_len.into()], "ss.buf")
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
            // Allocating String→String transforms via the runtime helpers
            // (`karac_string_{trim,to_lowercase,to_uppercase}`), which compute the
            // identical full-Unicode result as the interpreter's Rust stdlib —
            // the only way to keep the two backends bit-identical on Unicode case
            // mapping / whitespace without shipping Unicode tables into codegen.
            // Each returns a fresh `{ptr, out_len, out_len}` String (null + 0 for
            // an empty result, which becomes `{null, 0, 0}`).
            "trim" | "to_lowercase" | "to_uppercase" => {
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "sx");
                let func = match method {
                    "trim" => self.karac_string_trim_fn,
                    "to_lowercase" => self.karac_string_to_lowercase_fn,
                    "to_uppercase" => self.karac_string_to_uppercase_fn,
                    _ => unreachable!(),
                };
                Ok(self.build_string_xform_result(
                    func,
                    vec![recv_data.into(), recv_len.into()],
                    "str.xform",
                ))
            }
            // `String.sorted() -> String` — chars sorted ascending, the anagram
            // key (LeetCode #49). Guarded on `string_vars` so a String receiver
            // routes to `karac_string_sorted` while a `Vec[T].sorted()` still
            // falls through to the Vec arms / catch-all (same pattern as the
            // `push` String-vs-Vec disambiguation above).
            "sorted" if self.string_vars.contains(var_name) => {
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "ss");
                let func = self.karac_string_sorted_fn;
                Ok(self.build_string_xform_result(
                    func,
                    vec![recv_data.into(), recv_len.into()],
                    "str.sorted",
                ))
            }
            // `String.replace(from, to) -> String` via `karac_string_replace`
            // (Rust `str::replace`). Receiver + both args are passed as
            // `(ptr, len)` pairs.
            "replace" => {
                if args.len() != 2 {
                    return Err("String.replace requires (from, to) arguments".to_string());
                }
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "rp");
                let (from_data, from_len) = {
                    let s = self.compile_expr(&args[0].value)?.into_struct_value();
                    (
                        self.builder
                            .build_extract_value(s, 0, "rp.from.ptr")
                            .unwrap()
                            .into_pointer_value(),
                        self.builder
                            .build_extract_value(s, 1, "rp.from.len")
                            .unwrap()
                            .into_int_value(),
                    )
                };
                let (to_data, to_len) = {
                    let s = self.compile_expr(&args[1].value)?.into_struct_value();
                    (
                        self.builder
                            .build_extract_value(s, 0, "rp.to.ptr")
                            .unwrap()
                            .into_pointer_value(),
                        self.builder
                            .build_extract_value(s, 1, "rp.to.len")
                            .unwrap()
                            .into_int_value(),
                    )
                };
                Ok(self.build_string_xform_result(
                    self.karac_string_replace_fn,
                    vec![
                        recv_data.into(),
                        recv_len.into(),
                        from_data.into(),
                        from_len.into(),
                        to_data.into(),
                        to_len.into(),
                    ],
                    "str.replace",
                ))
            }
            // `String.repeat(n) -> String` — receiver bytes concatenated `n`
            // times into one fresh allocation; `n <= 0` yields the empty String
            // `{null, 0, 0}`. Single `malloc(n*len)` + an `n`-iteration memcpy
            // loop (output-size work, fewer reallocs than a `push_str` loop).
            // Surfaced by kata-katas #394 decode-string (the `k[encoded]` repeat
            // storm). Mirrors `substring`'s malloc + struct-assembly shape.
            "repeat" => {
                if args.is_empty() {
                    return Err("String.repeat requires a count argument".to_string());
                }
                let str_ty = self.vec_struct_type();
                let fn_val = self.current_fn.unwrap();
                let zero64 = i64_t.const_zero();

                // Receiver {data, len}.
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "rep.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "rep.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "rep.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "rep.recv.len")
                    .unwrap()
                    .into_int_value();

                // count = max(0, arg).
                let count_raw = self.compile_expr(&args[0].value)?.into_int_value();
                let is_neg = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, count_raw, zero64, "rep.neg")
                    .unwrap();
                let count = self
                    .builder
                    .build_select(is_neg, zero64, count_raw, "rep.count")
                    .unwrap()
                    .into_int_value();
                // total = count * len.
                let total = self
                    .builder
                    .build_int_nsw_mul(count, recv_len, "rep.total")
                    .unwrap();
                let total_zero = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, total, zero64, "rep.total.z")
                    .unwrap();

                let fill_bb = self.context.append_basic_block(fn_val, "rep.fill");
                let empty_bb = self.context.append_basic_block(fn_val, "rep.empty");
                let cont_bb = self.context.append_basic_block(fn_val, "rep.cont");
                let result_slot = self.create_entry_alloca(fn_val, "rep.result", str_ty.into());
                self.builder
                    .build_conditional_branch(total_zero, empty_bb, fill_bb)
                    .unwrap();

                // Empty branch: {null, 0, 0}.
                self.builder.position_at_end(empty_bb);
                let mut empty_agg = str_ty.get_undef();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, ptr_ty.const_null(), 0, "rep.e.ptr")
                    .unwrap()
                    .into_struct_value();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, zero64, 1, "rep.e.len")
                    .unwrap()
                    .into_struct_value();
                empty_agg = self
                    .builder
                    .build_insert_value(empty_agg, zero64, 2, "rep.e.cap")
                    .unwrap()
                    .into_struct_value();
                self.builder.build_store(result_slot, empty_agg).unwrap();
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                // Fill branch: malloc(total) + `count` memcpys of the receiver.
                self.builder.position_at_end(fill_bb);
                let buf = self
                    .builder
                    .build_call(self.alloc_or_panic_fn, &[total.into()], "rep.buf")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let r_slot = self.create_entry_alloca(fn_val, "rep.r", i64_t.into());
                self.builder.build_store(r_slot, zero64).unwrap();
                let head_bb = self.context.append_basic_block(fn_val, "rep.head");
                let body_bb = self.context.append_basic_block(fn_val, "rep.body");
                let done_bb = self.context.append_basic_block(fn_val, "rep.done");
                self.builder.build_unconditional_branch(head_bb).unwrap();

                self.builder.position_at_end(head_bb);
                let r = self
                    .builder
                    .build_load(i64_t, r_slot, "rep.r.load")
                    .unwrap()
                    .into_int_value();
                let r_lt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, r, count, "rep.r.lt")
                    .unwrap();
                self.builder
                    .build_conditional_branch(r_lt, body_bb, done_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                let off = self
                    .builder
                    .build_int_nsw_mul(r, recv_len, "rep.off")
                    .unwrap();
                let dest = unsafe {
                    self.builder
                        .build_gep(self.context.i8_type(), buf, &[off], "rep.dest")
                        .unwrap()
                };
                self.builder
                    .build_memcpy(dest, 1, recv_data, 1, recv_len)
                    .unwrap();
                let r_next = self
                    .builder
                    .build_int_nsw_add(r, i64_t.const_int(1, false), "rep.r.next")
                    .unwrap();
                self.builder.build_store(r_slot, r_next).unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                self.builder.position_at_end(done_bb);
                let mut fill_agg = str_ty.get_undef();
                fill_agg = self
                    .builder
                    .build_insert_value(fill_agg, buf, 0, "rep.f.ptr")
                    .unwrap()
                    .into_struct_value();
                fill_agg = self
                    .builder
                    .build_insert_value(fill_agg, total, 1, "rep.f.len")
                    .unwrap()
                    .into_struct_value();
                fill_agg = self
                    .builder
                    .build_insert_value(fill_agg, total, 2, "rep.f.cap")
                    .unwrap()
                    .into_struct_value();
                self.builder.build_store(result_slot, fill_agg).unwrap();
                self.builder.build_unconditional_branch(cont_bb).unwrap();

                self.builder.position_at_end(cont_bb);
                let result = self
                    .builder
                    .build_load(str_ty, result_slot, "rep.load")
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
                // UTF-8 encoded length from the codepoint, computed INLINE (no
                // runtime call): 1 if < 0x80, 2 if < 0x800, 3 if < 0x10000, else 4.
                // The ASCII fast-path in the copy section then stores the single
                // byte directly, so a pure-ASCII push needs neither an encode call
                // (`karac_string_encode_char`) nor a variable-length `memcpy` —
                // which LLVM lowers to a libc `memmove` call even for one byte, the
                // dominant cost on string-build workloads (kata:38 profile:
                // memmove/memcpy ~40%, encode ~20% of the hot time). Only the rare
                // multibyte path pays the call + copy.
                let cp_ty = cp.get_type();
                let lt_0x80 = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::ULT,
                        cp,
                        cp_ty.const_int(0x80, false),
                        "spush.lt80",
                    )
                    .unwrap();
                let lt_0x800 = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::ULT,
                        cp,
                        cp_ty.const_int(0x800, false),
                        "spush.lt800",
                    )
                    .unwrap();
                let lt_0x10000 = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::ULT,
                        cp,
                        cp_ty.const_int(0x10000, false),
                        "spush.lt10000",
                    )
                    .unwrap();
                let enc_len_3or4 = self
                    .builder
                    .build_select(
                        lt_0x10000,
                        i64_t.const_int(3, false),
                        i64_t.const_int(4, false),
                        "spush.l34",
                    )
                    .unwrap()
                    .into_int_value();
                let enc_len_2plus = self
                    .builder
                    .build_select(
                        lt_0x800,
                        i64_t.const_int(2, false),
                        enc_len_3or4,
                        "spush.l234",
                    )
                    .unwrap()
                    .into_int_value();
                let enc_len = self
                    .builder
                    .build_select(
                        lt_0x80,
                        i64_t.const_int(1, false),
                        enc_len_2plus,
                        "spush.enc_len",
                    )
                    .unwrap()
                    .into_int_value();

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

                // Grow: new_cap = max(new_len, max(8, cap * 2)) — same
                // geometry as `push_str`. The min-cap floor is 8, not 4: a
                // String is a 1-byte-element buffer, and Rust's `RawVec` floors
                // the first allocation at 8 for 1-byte elements (4 for wider),
                // so an ≤8-byte string (the common token / number-render case)
                // lands in ONE allocation instead of growing 0→4→8 — halving
                // the realloc traffic on short-string-heavy workloads.
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let min_cap = i64_t.const_int(8, false);
                let doubled = self
                    .builder
                    .build_int_mul(cap, two, "spush.doubled")
                    .unwrap();
                let cmp1 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, min_cap, "spush.cmp1")
                    .unwrap();
                let growth_min = self
                    .builder
                    .build_select(cmp1, doubled, min_cap, "spush.growth_min")
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
                // Grow via realloc where the buffer is heap (cap > 0); a
                // static-literal / empty buffer takes a fresh malloc + copy.
                let new_data =
                    self.emit_string_buffer_grow(fn_val, data, cap, len, new_cap, "spush");

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
                // ASCII fast-path: a codepoint < 0x80 is its own single UTF-8 byte,
                // so store it directly (truncate the i32 codepoint to i8) — no
                // encode call, no memcpy. The rare multibyte path encodes into a
                // scratch buffer and memcpy's the 1–4 bytes as before.
                let ascii_bb = self.context.append_basic_block(fn_val, "spush.ascii");
                let multi_bb = self.context.append_basic_block(fn_val, "spush.multi");
                let stored_bb = self.context.append_basic_block(fn_val, "spush.stored");
                self.builder
                    .build_conditional_branch(lt_0x80, ascii_bb, multi_bb)
                    .unwrap();

                self.builder.position_at_end(ascii_bb);
                let byte = self
                    .builder
                    .build_int_truncate(cp, self.context.i8_type(), "spush.byte")
                    .unwrap();
                self.builder.build_store(dest, byte).unwrap();
                self.builder.build_unconditional_branch(stored_bb).unwrap();

                self.builder.position_at_end(multi_bb);
                let (enc_buf, _enc_len_runtime) = self.emit_codepoint_to_utf8(cp);
                self.builder
                    .build_memcpy(dest, 1, enc_buf, 1, enc_len)
                    .unwrap();
                self.builder.build_unconditional_branch(stored_bb).unwrap();

                self.builder.position_at_end(stored_bb);
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
                // F-string argument (`v.push(f"…")`): the accumulator's
                // queued scope-exit `FreeVecBuffer` must be disarmed —
                // the container takes the buffer (move), and without the
                // take both the acc cleanup and the container's
                // recursive drop free the same data pointer (SIGTRAP,
                // kata-22 2026-06-06). Same take-point as the Let /
                // Assign / struct-field / tail-return consumers of
                // `last_fstr_acc`.
                self.suppress_fstr_acc_if_moved_out(&args[0].value);
                // Owned String/Vec PARAM argument (`out.push(cur)` where
                // `cur: String` is a parameter): the caller retains the
                // buffer's free under the by-value header ABI, so the
                // container must own a deep copy, not an alias. See
                // `emit_vecstr_defensive_copy`.
                let elem_val = self.maybe_defensive_copy_param_arg(&args[0].value, elem_val);
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
                // Map/Set source moved into the Vec: the Vec now owns the
                // handle and frees it via the `Vec[Map]` element drop
                // (`track_vec_of_maps_var`), so drop the source binding's
                // `FreeMapHandle` — otherwise both free the same handle
                // (double-free) or, if the Vec escapes the source's scope,
                // the source frees a handle the Vec still points at
                // (premature-free / UAF, Cluster 1). No-op when the arg is
                // not a tracked map/set identifier. The Map sibling of the
                // `suppress_source_vec_cleanup_for_arg` cap-zeroing above;
                // mirrors the enum-variant move path in `call_dispatch.rs`.
                if let ExprKind::Identifier(n) = &args[0].value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }

                // Vec-store slice (B-2026-06-22-2): pushing a heap-env closure
                // BINDING (`v.push(f)`) into a heap-env Vec owner co-owns the env
                // box — the source binding's scope-exit `FreeClosureEnv` AND the
                // Vec's per-element drop loop both decrement it, so bump the
                // refcount here. A fresh `v.push(make(k))` element is already rc 1
                // (no inc). Mirrors the array/tuple binding-source store inc.
                if self.heap_env_vec_owners.contains(var_name) {
                    if let ExprKind::Identifier(src) = &args[0].value.kind {
                        if self.heap_env_closure_vars.contains(src) {
                            self.emit_heap_closure_env_inc(elem_val);
                        }
                    }
                }

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
                // Grow via realloc: the allocator extends in place where it can,
                // avoiding the malloc-new + memcpy + free-old churn (and the
                // transient old+new 2× peak). Vec data is always null (cap 0) or
                // heap, and realloc(null, n) == malloc(n), so this is a clean
                // drop-in — no static-buffer hazard (unlike String literals).
                let realloc_fn = self.realloc_or_panic_fn_decl();
                let new_data = self
                    .builder
                    .build_call(realloc_fn, &[data.into(), alloc_bytes.into()], "new_data")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();

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
                // Narrow the value to the element width before storing. A
                // sub-word element type (`Vec[u8]` / `Vec[bool]` / `Vec[u16]`
                // / `Vec[u32]`) has a 1/2/4-byte stride and allocation, but a
                // computed scalar (`v.push(b'a' + (i as u8))`) compiles to the
                // default i64 — storing it raw writes 8 bytes over a 1-byte
                // slot, smearing 7 bytes past the buffer on the slot that fills
                // an exact-size-class allocation (heap overflow → corruption,
                // ASLR-intermittent crash). Mirrors `coerce_to_struct_field_ty`
                // for struct fields and the index-store path below.
                let elem_val = self.coerce_scalar_to_type(elem_val, elem_ty);
                self.builder.build_store(elem_ptr, elem_val).unwrap();

                // Increment len.
                let one = i64_t.const_int(1, false);
                let new_len = self.builder.build_int_add(len, one, "new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `Vec.try_push(x)` / `VecDeque.try_push_back(x)` — fallible append
            // (phase-8-stdlib-floor item 8). Identical to `push`/`push_back`
            // except the grow allocation uses `karac_alloc_fallible`; a null
            // result short-circuits to
            // `Result.Err(AllocError.OutOfMemory{requested_bytes})` instead of
            // aborting. On success the element is stored and `Result.Ok(())` is
            // returned. The element type comes from the receiver binding, so —
            // unlike `try_with_capacity` — there is no element-type-through-
            // `Result` recovery problem.
            "try_push" | "try_push_back" => {
                if args.is_empty() {
                    return Err("Vec.try_push requires an argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;
                self.suppress_fstr_acc_if_moved_out(&args[0].value);
                let elem_val = self.maybe_defensive_copy_param_arg(&args[0].value, elem_val);
                self.suppress_source_vec_cleanup_for_arg(&args[0].value);
                // Map/Set source moved into the Vec: the Vec now owns the
                // handle and frees it via the `Vec[Map]` element drop
                // (`track_vec_of_maps_var`), so drop the source binding's
                // `FreeMapHandle` — otherwise both free the same handle
                // (double-free) or, if the Vec escapes the source's scope,
                // the source frees a handle the Vec still points at
                // (premature-free / UAF, Cluster 1). No-op when the arg is
                // not a tracked map/set identifier. The Map sibling of the
                // `suppress_source_vec_cleanup_for_arg` cap-zeroing above;
                // mirrors the enum-variant move path in `call_dispatch.rs`.
                if let ExprKind::Identifier(n) = &args[0].value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "tpush.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "tpush.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "tpush.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tpush.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "tpush.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "tpush.cap")
                    .unwrap()
                    .into_int_value();

                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "tpush.grow");
                let grow_ok_bb = self.context.append_basic_block(fn_val, "tpush.grow.ok");
                let oom_bb = self.context.append_basic_block(fn_val, "tpush.oom");
                let store_bb = self.context.append_basic_block(fn_val, "tpush.store");
                let merge_bb = self.context.append_basic_block(fn_val, "tpush.merge");

                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "tpush.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, store_bb)
                    .unwrap();

                // Grow: new_cap = max(4, cap*2); bytes = new_cap * sizeof(elem);
                // fallible alloc.
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self
                    .builder
                    .build_int_mul(cap, two, "tpush.doubled")
                    .unwrap();
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "tpush.cmp")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp, doubled, four, "tpush.new_cap")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();
                let alloc_bytes = self
                    .builder
                    .build_int_mul(new_cap, elem_size, "tpush.alloc_bytes")
                    .unwrap();
                let new_data = self
                    .builder
                    .build_call(
                        self.alloc_fallible_fn,
                        &[alloc_bytes.into()],
                        "tpush.new_data",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let is_null = self
                    .builder
                    .build_is_null(new_data, "tpush.is_null")
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_null, oom_bb, grow_ok_bb)
                    .unwrap();

                // Grow succeeded: memcpy old → new, free old, update fields.
                self.builder.position_at_end(grow_ok_bb);
                let old_bytes = self
                    .builder
                    .build_int_mul(len, elem_size, "tpush.old_bytes")
                    .unwrap();
                self.builder
                    .build_memcpy(new_data, 8, data, 8, old_bytes)
                    .unwrap();
                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();
                self.builder.build_store(data_ptr_ptr, new_data).unwrap();
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(store_bb).unwrap();

                // OOM → Result.Err(AllocError.OutOfMemory{requested_bytes}).
                self.builder.position_at_end(oom_bb);
                let alloc_err = self.build_nonshared_enum_value(
                    "AllocError",
                    "OutOfMemory",
                    &[alloc_bytes.into()],
                )?;
                let err_result = self.build_nonshared_enum_value("Result", "Err", &[alloc_err])?;
                let oom_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Store element at data[len], len++, → Result.Ok(()).
                self.builder.position_at_end(store_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tpush.cur_data")
                    .unwrap()
                    .into_pointer_value();
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, cur_data, &[len], "tpush.elem.ptr")
                        .unwrap()
                };
                // Narrow to element width — see the `push` store note.
                let elem_val = self.coerce_scalar_to_type(elem_val, elem_ty);
                self.builder.build_store(elem_ptr, elem_val).unwrap();
                let one = i64_t.const_int(1, false);
                let new_len = self
                    .builder
                    .build_int_add(len, one, "tpush.new_len")
                    .unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();
                let unit_val = i64_t.const_zero().into();
                let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[unit_val])?;
                let store_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge the two `Result` aggregates.
                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(ok_result.get_type(), "tpush.result")
                    .unwrap();
                phi.add_incoming(&[(&ok_result, store_end), (&err_result, oom_end)]);
                Ok(phi.as_basic_value())
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
                // Same consume-site ownership pair as the "push" arm: an
                // f-string temp moves in (disarm its acc cleanup); an
                // owned String/Vec param deep-copies (caller keeps the
                // free).
                self.suppress_fstr_acc_if_moved_out(&args[0].value);
                let elem_val = self.maybe_defensive_copy_param_arg(&args[0].value, elem_val);
                // And the local-binding move: zero the source's cap so its
                // scope-exit cleanup skips — the deque owns the buffer now
                // (mirrors the "push" arm; push_front was missing it).
                self.suppress_source_vec_cleanup_for_arg(&args[0].value);
                // Map/Set source moved into the Vec: the Vec now owns the
                // handle and frees it via the `Vec[Map]` element drop
                // (`track_vec_of_maps_var`), so drop the source binding's
                // `FreeMapHandle` — otherwise both free the same handle
                // (double-free) or, if the Vec escapes the source's scope,
                // the source frees a handle the Vec still points at
                // (premature-free / UAF, Cluster 1). No-op when the arg is
                // not a tracked map/set identifier. The Map sibling of the
                // `suppress_source_vec_cleanup_for_arg` cap-zeroing above;
                // mirrors the enum-variant move path in `call_dispatch.rs`.
                if let ExprKind::Identifier(n) = &args[0].value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }

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
                    .build_call(self.alloc_or_panic_fn, &[alloc_bytes.into()], "new_data")
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
                let elem_val = self.coerce_scalar_to_type(elem_val, elem_ty);
                self.builder.build_store(cur_data, elem_val).unwrap();
                let one = i64_t.const_int(1, false);
                let new_len = self.builder.build_int_add(len, one, "new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `VecDeque.try_push_front(x)` — fallible `push_front`
            // (phase-8-stdlib-floor item 8). Same shift-right-by-1 insert at
            // index 0 as `push_front`, but the grow uses `karac_alloc_fallible`;
            // a null result short-circuits to
            // `Result.Err(AllocError.OutOfMemory{requested_bytes})`. On success
            // returns `Result.Ok(())`.
            "try_push_front" => {
                if args.is_empty() {
                    return Err("VecDeque.try_push_front requires an argument".to_string());
                }
                let elem_val = self.compile_expr(&args[0].value)?;
                self.suppress_fstr_acc_if_moved_out(&args[0].value);
                let elem_val = self.maybe_defensive_copy_param_arg(&args[0].value, elem_val);
                self.suppress_source_vec_cleanup_for_arg(&args[0].value);
                // Map/Set source moved into the Vec: the Vec now owns the
                // handle and frees it via the `Vec[Map]` element drop
                // (`track_vec_of_maps_var`), so drop the source binding's
                // `FreeMapHandle` — otherwise both free the same handle
                // (double-free) or, if the Vec escapes the source's scope,
                // the source frees a handle the Vec still points at
                // (premature-free / UAF, Cluster 1). No-op when the arg is
                // not a tracked map/set identifier. The Map sibling of the
                // `suppress_source_vec_cleanup_for_arg` cap-zeroing above;
                // mirrors the enum-variant move path in `call_dispatch.rs`.
                if let ExprKind::Identifier(n) = &args[0].value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "tpf.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "tpf.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "tpf.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tpf.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "tpf.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "tpf.cap")
                    .unwrap()
                    .into_int_value();

                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "tpf.grow");
                let grow_ok_bb = self.context.append_basic_block(fn_val, "tpf.grow.ok");
                let oom_bb = self.context.append_basic_block(fn_val, "tpf.oom");
                let shift_bb = self.context.append_basic_block(fn_val, "tpf.shift");
                let merge_bb = self.context.append_basic_block(fn_val, "tpf.merge");

                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "tpf.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, shift_bb)
                    .unwrap();

                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self.builder.build_int_mul(cap, two, "tpf.doubled").unwrap();
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "tpf.cmp")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp, doubled, four, "tpf.new_cap")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();
                let alloc_bytes = self
                    .builder
                    .build_int_mul(new_cap, elem_size, "tpf.alloc_bytes")
                    .unwrap();
                let new_data = self
                    .builder
                    .build_call(
                        self.alloc_fallible_fn,
                        &[alloc_bytes.into()],
                        "tpf.new_data",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let is_null = self.builder.build_is_null(new_data, "tpf.is_null").unwrap();
                self.builder
                    .build_conditional_branch(is_null, oom_bb, grow_ok_bb)
                    .unwrap();

                self.builder.position_at_end(grow_ok_bb);
                let old_bytes = self
                    .builder
                    .build_int_mul(len, elem_size, "tpf.old_bytes")
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

                self.builder.position_at_end(oom_bb);
                let err_result = self.build_alloc_oom_result(alloc_bytes)?;
                let oom_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Shift [0..len) right by 1, store new element at index 0.
                self.builder.position_at_end(shift_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tpf.cur_data")
                    .unwrap()
                    .into_pointer_value();
                let one = i64_t.const_int(1, false);
                let shifted_dst = unsafe {
                    self.builder
                        .build_gep(elem_ty, cur_data, &[one], "tpf.shift.dst")
                        .unwrap()
                };
                let elem_size2 = elem_ty.size_of().unwrap();
                let shift_bytes = self
                    .builder
                    .build_int_mul(len, elem_size2, "tpf.shift_bytes")
                    .unwrap();
                self.builder
                    .build_memmove(shifted_dst, 8, cur_data, 8, shift_bytes)
                    .unwrap();
                let elem_val = self.coerce_scalar_to_type(elem_val, elem_ty);
                self.builder.build_store(cur_data, elem_val).unwrap();
                let new_len = self.builder.build_int_add(len, one, "tpf.new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();
                let unit_val = i64_t.const_zero().into();
                let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[unit_val])?;
                let shift_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(ok_result.get_type(), "tpf.result")
                    .unwrap();
                phi.add_incoming(&[(&ok_result, shift_end), (&err_result, oom_end)]);
                Ok(phi.as_basic_value())
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
                // A string-slice argument (`s[a..b]`) is BORROWED, not copied:
                // `push_str` only reads the bytes into the destination, so the
                // allocating slice (`karac_string_slice`, which malloc's an
                // n+1-byte copy) is pure waste — it allocated *and* freed a temp
                // String on every call. The borrowed view `{ptr, len, cap: 0}`
                // points into the source, which is live for the synchronous copy
                // below; the cap-0 view is non-owning and never freed. This was
                // ~28M wasted temp allocs in the #405 hex-render kata (and is the
                // self-hosted lexer's token-text `push_str(src[a..b])` hot path)
                // — measured 30× on a slice-push microbench.
                let (src_val, src_borrowed) =
                    match self.try_compile_borrowed_string_slice(&args[0].value)? {
                        Some(view) => (view, true),
                        None => (self.compile_expr(&args[0].value)?, false),
                    };
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
                // Aliasing guard — borrowed-source only. A borrowed slice arg
                // (`out.push_str(src[a..b])`) points INTO `src`'s buffer; if that
                // buffer is the destination's own (`s.push_str(s[a..b])`), the
                // `free(data)` below would dangle the source before the copy.
                // Detect the overlap and panic with a clear message, matching
                // `Vec.extend_from_slice`'s policy. The common case (distinct
                // source) never overlaps, so this is a cold, grow-only check; an
                // owned-temp source is a fresh copy that can't alias, so the
                // guard is emitted only for the borrowed path.
                if src_borrowed {
                    let src_int = self
                        .builder
                        .build_ptr_to_int(src_ptr, i64_t, "pstr.src.int")
                        .unwrap();
                    let data_int = self
                        .builder
                        .build_ptr_to_int(data, i64_t, "pstr.data.int")
                        .unwrap();
                    let data_end = self
                        .builder
                        .build_int_add(data_int, cap, "pstr.data.end")
                        .unwrap();
                    let ge = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::UGE,
                            src_int,
                            data_int,
                            "pstr.alias.ge",
                        )
                        .unwrap();
                    let lt = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::ULT,
                            src_int,
                            data_end,
                            "pstr.alias.lt",
                        )
                        .unwrap();
                    let overlap = self.builder.build_and(ge, lt, "pstr.alias").unwrap();
                    let panic_bb = self.context.append_basic_block(fn_val, "pstr.alias.panic");
                    let ok_bb = self.context.append_basic_block(fn_val, "pstr.alias.ok");
                    self.builder
                        .build_conditional_branch(overlap, panic_bb, ok_bb)
                        .unwrap();
                    self.builder.position_at_end(panic_bb);
                    self.emit_panic(
                        "String.push_str: source slice aliases destination buffer (use a distinct source when grow is required)",
                    );
                    self.builder.build_unreachable().unwrap();
                    self.builder.position_at_end(ok_bb);
                }
                let two = i64_t.const_int(2, false);
                // String byte buffer: floor the first allocation at 8 (not 4),
                // matching Rust's `RawVec` min-cap for 1-byte elements, so a
                // short string (≤8 bytes) lands in one allocation rather than
                // growing 0→4→8 — fewer reallocs on short-string workloads.
                let min_cap = i64_t.const_int(8, false);
                let doubled = self.builder.build_int_mul(cap, two, "doubled").unwrap();
                let cmp1 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, min_cap, "cmp1")
                    .unwrap();
                let growth_min = self
                    .builder
                    .build_select(cmp1, doubled, min_cap, "growth_min")
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

                // Grow via realloc where the buffer is heap (cap > 0) — the
                // allocator extends in place where it can, avoiding the malloc-
                // new + memcpy + free-old churn and the transient old+new 2×
                // peak (dominant when a large output buffer doubles). A static-
                // literal / empty buffer (cap == 0) takes a fresh malloc + copy.
                let new_data =
                    self.emit_string_buffer_grow(fn_val, data, cap, len, new_cap, "pstr");

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

                // Free a fresh-owned String temp argument now that its bytes are
                // copied. `buffer.push_str(source.substring(start, cur))` — the
                // lexer's token-text shape — passes a freshly-malloc'd String
                // that nothing else owns; without this its heap buffer leaks
                // once per call (kata-katas #722 bench measured ~48 bytes/iter,
                // unbounded). Immediate-free here (not scope-deferred
                // materialize_owned_temp) keeps a hot loop from accumulating
                // temps until function exit. The insert point is already at the
                // post-copy block, so every read of the source dominates the
                // free. A borrowed slice arg (`src_borrowed`) is a cap-0 view
                // that owns nothing — skip the free entirely (it never allocated
                // a temp), instead of emitting a runtime cap-0 guard that always
                // falls through.
                if !src_borrowed {
                    self.free_fresh_owned_str_arg(&args[0].value, src_val);
                }

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `String.try_push_str(s)` — fallible `push_str`
            // (phase-8-stdlib-floor item 8). Identical to `push_str` except the
            // grow allocation uses `karac_alloc_fallible`; a null result
            // short-circuits to `Result.Err(AllocError.OutOfMemory{new_cap})`.
            // On success the bytes are appended and `Result.Ok(())` is returned.
            "try_push_str" => {
                if args.is_empty() {
                    return Err("String.try_push_str requires an argument".to_string());
                }
                let src_val = self.compile_expr(&args[0].value)?;
                let src_ptr = self
                    .builder
                    .build_extract_value(src_val.into_struct_value(), 0, "tss.src.ptr")
                    .unwrap()
                    .into_pointer_value();
                let src_len = self
                    .builder
                    .build_extract_value(src_val.into_struct_value(), 1, "tss.src.len")
                    .unwrap()
                    .into_int_value();

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "tss.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "tss.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "tss.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tss.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "tss.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "tss.cap")
                    .unwrap()
                    .into_int_value();
                let new_len = self
                    .builder
                    .build_int_add(len, src_len, "tss.new_len")
                    .unwrap();

                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "tss.grow");
                let grow_ok_bb = self.context.append_basic_block(fn_val, "tss.grow.ok");
                let free_bb = self.context.append_basic_block(fn_val, "tss.free");
                let after_free_bb = self.context.append_basic_block(fn_val, "tss.after_free");
                let oom_bb = self.context.append_basic_block(fn_val, "tss.oom");
                let copy_bb = self.context.append_basic_block(fn_val, "tss.copy");
                let merge_bb = self.context.append_basic_block(fn_val, "tss.merge");

                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, cap, "tss.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, copy_bb)
                    .unwrap();

                // Grow: new_cap = max(new_len, max(4, cap*2)); fallible alloc.
                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                // String byte buffer floors at 8 (Rust `RawVec` 1-byte min-cap);
                // see the `push_str` grow path for the rationale.
                let min_cap = i64_t.const_int(8, false);
                let doubled = self.builder.build_int_mul(cap, two, "tss.doubled").unwrap();
                let cmp1 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, min_cap, "tss.cmp1")
                    .unwrap();
                let growth_min = self
                    .builder
                    .build_select(cmp1, doubled, min_cap, "tss.growth_min")
                    .unwrap()
                    .into_int_value();
                let cmp2 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, growth_min, "tss.cmp2")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp2, new_len, growth_min, "tss.new_cap")
                    .unwrap()
                    .into_int_value();
                let new_data = self
                    .builder
                    .build_call(self.alloc_fallible_fn, &[new_cap.into()], "tss.new_data")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let is_null = self.builder.build_is_null(new_data, "tss.is_null").unwrap();
                self.builder
                    .build_conditional_branch(is_null, oom_bb, grow_ok_bb)
                    .unwrap();

                // Grow succeeded: memcpy old bytes, free old if heap, update.
                self.builder.position_at_end(grow_ok_bb);
                self.builder
                    .build_memcpy(new_data, 1, data, 1, len)
                    .unwrap();
                let zero_val = i64_t.const_int(0, false);
                let was_heap = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, cap, zero_val, "tss.was_heap")
                    .unwrap();
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

                // OOM → Result.Err(AllocError.OutOfMemory{new_cap}).
                self.builder.position_at_end(oom_bb);
                let err_result = self.build_alloc_oom_result(new_cap)?;
                let oom_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Copy src bytes to data+len, update len, → Result.Ok(()).
                self.builder.position_at_end(copy_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tss.cur_data")
                    .unwrap()
                    .into_pointer_value();
                let cur_len = self
                    .builder
                    .build_load(i64_t, len_ptr, "tss.cur_len")
                    .unwrap()
                    .into_int_value();
                let dest = unsafe {
                    self.builder
                        .build_gep(self.context.i8_type(), cur_data, &[cur_len], "tss.dest")
                        .unwrap()
                };
                self.builder
                    .build_memcpy(dest, 1, src_ptr, 1, src_len)
                    .unwrap();
                let updated_len = self
                    .builder
                    .build_int_add(cur_len, src_len, "tss.updated_len")
                    .unwrap();
                self.builder.build_store(len_ptr, updated_len).unwrap();
                let unit_val = i64_t.const_zero().into();
                let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[unit_val])?;
                let copy_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(ok_result.get_type(), "tss.result")
                    .unwrap();
                phi.add_incoming(&[(&ok_result, copy_end), (&err_result, oom_end)]);
                Ok(phi.as_basic_value())
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
                    .build_call(
                        self.alloc_or_panic_fn,
                        &[new_alloc_bytes.into()],
                        "efs.new_data",
                    )
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
            // `Vec.try_extend_from_slice(src)` — fallible `extend_from_slice`
            // (phase-8-stdlib-floor item 8). Same append-with-grow shape as the
            // `extend_from_slice` arm above (overlap guard, geometric growth,
            // trivial-memcpy vs per-element clone), but the grow allocation goes
            // through `karac_alloc_fallible`: a null result short-circuits to
            // `Result.Err(AllocError.OutOfMemory{requested_bytes})` instead of
            // aborting, and the success path returns `Result.Ok(())`. The
            // aliasing **overlap guard stays a panic** — a source slice that
            // points into the receiver's own buffer is a caller logic error, not
            // an allocation failure, so it must not be reported as recoverable
            // OOM. The panic block (`unreachable` terminator) and the OOM block
            // (branches to merge) simply coexist as distinct successors of the
            // grow block.
            "try_extend_from_slice" => {
                if args.len() != 1 {
                    return Err(format!(
                        "try_extend_from_slice expects 1 argument (source), got {}",
                        args.len()
                    ));
                }
                // Source coercion — identical to `extend_from_slice`: slice
                // fast path, else compile-and-extract a Vec/Slice struct.
                let src_data;
                let src_len;
                if let Some(slice_val) = self.coerce_to_slice(&args[0].value, elem_ty)? {
                    let slice_sv = slice_val.into_struct_value();
                    src_data = self
                        .builder
                        .build_extract_value(slice_sv, 0, "tefs.src.data")
                        .unwrap()
                        .into_pointer_value();
                    src_len = self
                        .builder
                        .build_extract_value(slice_sv, 1, "tefs.src.len")
                        .unwrap()
                        .into_int_value();
                } else {
                    let compiled = self.compile_expr(&args[0].value)?;
                    let sv = match compiled {
                        BasicValueEnum::StructValue(sv) => sv,
                        _ => {
                            return Err(format!(
                                "try_extend_from_slice: source expression does not produce a slice or vec value (got {compiled:?})"
                            ))
                        }
                    };
                    let n_fields = sv.get_type().count_fields();
                    if n_fields != 2 && n_fields != 3 {
                        return Err(format!(
                            "try_extend_from_slice: source struct has {n_fields} fields; expected 2 (Slice) or 3 (Vec)"
                        ));
                    }
                    src_data = self
                        .builder
                        .build_extract_value(sv, 0, "tefs.src.data")
                        .unwrap()
                        .into_pointer_value();
                    src_len = self
                        .builder
                        .build_extract_value(sv, 1, "tefs.src.len")
                        .unwrap()
                        .into_int_value();
                }
                let elem_size = elem_ty.size_of().unwrap();
                let src_bytes = self
                    .builder
                    .build_int_mul(src_len, elem_size, "tefs.src.bytes")
                    .unwrap();

                // Load target fields.
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "tefs.t.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "tefs.t.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "tefs.t.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tefs.t.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "tefs.t.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "tefs.t.cap")
                    .unwrap()
                    .into_int_value();

                let new_len = self
                    .builder
                    .build_int_add(len, src_len, "tefs.new_len")
                    .unwrap();

                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "tefs.grow");
                let copy_bb = self.context.append_basic_block(fn_val, "tefs.copy");
                let oom_bb = self.context.append_basic_block(fn_val, "tefs.oom");
                let merge_bb = self.context.append_basic_block(fn_val, "tefs.merge");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, cap, "tefs.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, copy_bb)
                    .unwrap();

                // Grow path. Overlap guard first (panic on alias — a logic error,
                // not OOM), then geometric growth + fallible alloc.
                self.builder.position_at_end(grow_bb);
                let src_int = self
                    .builder
                    .build_ptr_to_int(src_data, i64_t, "tefs.src.int")
                    .unwrap();
                let data_int = self
                    .builder
                    .build_ptr_to_int(data, i64_t, "tefs.data.int")
                    .unwrap();
                let cap_bytes_grow = self
                    .builder
                    .build_int_mul(cap, elem_size, "tefs.cap.bytes")
                    .unwrap();
                let data_end = self
                    .builder
                    .build_int_add(data_int, cap_bytes_grow, "tefs.data.end")
                    .unwrap();
                let ge_start = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::UGE,
                        src_int,
                        data_int,
                        "tefs.ge.start",
                    )
                    .unwrap();
                let lt_end = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, src_int, data_end, "tefs.lt.end")
                    .unwrap();
                let overlap = self
                    .builder
                    .build_and(ge_start, lt_end, "tefs.overlap")
                    .unwrap();
                let panic_bb = self.context.append_basic_block(fn_val, "tefs.alias.panic");
                let no_overlap_bb = self.context.append_basic_block(fn_val, "tefs.no_overlap");
                self.builder
                    .build_conditional_branch(overlap, panic_bb, no_overlap_bb)
                    .unwrap();
                self.builder.position_at_end(panic_bb);
                self.emit_panic(
                    "Vec.try_extend_from_slice: source slice aliases destination buffer (use a distinct source when grow is required)",
                );
                self.builder.build_unreachable().unwrap();
                self.builder.position_at_end(no_overlap_bb);

                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self
                    .builder
                    .build_int_mul(cap, two, "tefs.doubled")
                    .unwrap();
                let cmp1 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "tefs.cmp1")
                    .unwrap();
                let growth_min = self
                    .builder
                    .build_select(cmp1, doubled, four, "tefs.growth_min")
                    .unwrap()
                    .into_int_value();
                let cmp2 = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, new_len, growth_min, "tefs.cmp2")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp2, new_len, growth_min, "tefs.new_cap")
                    .unwrap()
                    .into_int_value();

                // Fallible allocation: null → OOM Result.Err.
                let new_alloc_bytes = self
                    .builder
                    .build_int_mul(new_cap, elem_size, "tefs.new.bytes")
                    .unwrap();
                let new_data = self
                    .builder
                    .build_call(
                        self.alloc_fallible_fn,
                        &[new_alloc_bytes.into()],
                        "tefs.new_data",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let alloc_ok_bb = self.context.append_basic_block(fn_val, "tefs.grow.ok");
                let is_null = self
                    .builder
                    .build_is_null(new_data, "tefs.is_null")
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_null, oom_bb, alloc_ok_bb)
                    .unwrap();

                // Grow succeeded: memcpy old elements, free old buffer if heap,
                // publish the new {ptr, cap}.
                self.builder.position_at_end(alloc_ok_bb);
                let old_bytes = self
                    .builder
                    .build_int_mul(len, elem_size, "tefs.old.bytes")
                    .unwrap();
                self.builder
                    .build_memcpy(new_data, 8, data, 8, old_bytes)
                    .unwrap();
                let zero_val = i64_t.const_int(0, false);
                let was_heap = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, cap, zero_val, "tefs.was_heap")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "tefs.free");
                let after_free_bb = self.context.append_basic_block(fn_val, "tefs.after_free");
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

                // OOM → Result.Err(AllocError.OutOfMemory{requested_bytes}).
                self.builder.position_at_end(oom_bb);
                let err_result = self.build_alloc_oom_result(new_alloc_bytes)?;
                let oom_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Copy src elements into dest[len..] — memcpy for trivially-
                // copyable elements, per-element synth_clone otherwise (same
                // double-free avoidance as the panicking arm). Reached from the
                // no-grow path (entry) and the grow-success path (after_free).
                self.builder.position_at_end(copy_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "tefs.cur_data")
                    .unwrap()
                    .into_pointer_value();
                let cur_len = self
                    .builder
                    .build_load(i64_t, len_ptr, "tefs.cur_len")
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
                            .build_gep(elem_ty, cur_data, &[cur_len], "tefs.dest")
                            .unwrap()
                    };
                    self.builder
                        .build_memcpy(dest, 8, src_data, 8, src_bytes)
                        .unwrap();
                } else {
                    let elem_te = elem_te.unwrap();
                    let clone_fn = self.emit_clone_fn_for_type_expr(&elem_te);
                    let loop_cond_bb = self.context.append_basic_block(fn_val, "tefs.clone.cond");
                    let loop_body_bb = self.context.append_basic_block(fn_val, "tefs.clone.body");
                    let loop_exit_bb = self.context.append_basic_block(fn_val, "tefs.clone.exit");
                    let i_alloca = self.create_entry_alloca(fn_val, "tefs.clone.i", i64_t.into());
                    self.builder
                        .build_store(i_alloca, i64_t.const_zero())
                        .unwrap();
                    self.builder
                        .build_unconditional_branch(loop_cond_bb)
                        .unwrap();

                    self.builder.position_at_end(loop_cond_bb);
                    let i_cur = self
                        .builder
                        .build_load(i64_t, i_alloca, "tefs.clone.i.cur")
                        .unwrap()
                        .into_int_value();
                    let cond = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::ULT,
                            i_cur,
                            src_len,
                            "tefs.clone.lt",
                        )
                        .unwrap();
                    self.builder
                        .build_conditional_branch(cond, loop_body_bb, loop_exit_bb)
                        .unwrap();

                    self.builder.position_at_end(loop_body_bb);
                    let src_ep = unsafe {
                        self.builder
                            .build_gep(elem_ty, src_data, &[i_cur], "tefs.clone.src.ep")
                            .unwrap()
                    };
                    let dst_idx = self
                        .builder
                        .build_int_add(cur_len, i_cur, "tefs.clone.dst.idx")
                        .unwrap();
                    let dst_ep = unsafe {
                        self.builder
                            .build_gep(elem_ty, cur_data, &[dst_idx], "tefs.clone.dst.ep")
                            .unwrap()
                    };
                    self.builder
                        .build_call(clone_fn, &[src_ep.into(), dst_ep.into()], "")
                        .unwrap();
                    let one = i64_t.const_int(1, false);
                    let i_next = self
                        .builder
                        .build_int_add(i_cur, one, "tefs.clone.i.next")
                        .unwrap();
                    self.builder.build_store(i_alloca, i_next).unwrap();
                    self.builder
                        .build_unconditional_branch(loop_cond_bb)
                        .unwrap();

                    self.builder.position_at_end(loop_exit_bb);
                }
                let updated_len = self
                    .builder
                    .build_int_add(cur_len, src_len, "tefs.updated_len")
                    .unwrap();
                self.builder.build_store(len_ptr, updated_len).unwrap();
                let unit_val = i64_t.const_zero().into();
                let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[unit_val])?;
                let ok_end = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge the two `Result` aggregates.
                self.builder.position_at_end(merge_bb);
                let phi = self
                    .builder
                    .build_phi(ok_result.get_type(), "tefs.result")
                    .unwrap();
                phi.add_incoming(&[(&ok_result, ok_end), (&err_result, oom_end)]);
                Ok(phi.as_basic_value())
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

                // Slice 6.1: monomorphized fast path for
                // `Vec[i64].sort_by(inline_closure)` with no captures. Emits a
                // per-call-site sort function (insertion sort over data) with
                // the comparator closure inlined at the inner compare — no
                // `karac_vec_sort_by` callback dispatch, LLVM has full
                // visibility into both the sort algorithm and the comparator.
                // All other shapes (non-i64 element, non-inline callee,
                // captures present) fall through to the existing thunk path
                // below. Surfaced by kata 16 (3Sum Closest) — see the
                // `Slice 6 (Vec[T]) — natural-pull trigger event` entry in
                // `docs/implementation_checklist/phase-7-codegen.md`.
                if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                    if self.should_use_mono_vec_sort_by_for(elem_ty)
                        && self.collect_closure_free_vars(params, body).is_empty()
                    {
                        // For named-struct elements, pull the Kāra type
                        // name so the mono emitter can register
                        // var_type_names for closure params and the
                        // body's named-field access resolves. Tuples
                        // (TypeKind::Tuple) and other shapes pass None;
                        // the .0/.1 numeric-index path doesn't need it.
                        let elem_type_name: Option<String> = self
                            .var_elem_type_exprs
                            .get(var_name)
                            .and_then(|te| match &te.kind {
                                TypeKind::Path(p) => p.segments.last().cloned(),
                                _ => None,
                            });
                        // Emit BOTH the mono fast path AND the runtime
                        // fallback path. Insertion sort is O(N²), which
                        // beats the runtime callback's per-compare
                        // indirect-call cost up to ~N=32–64 but loses
                        // hard above that (surfaced 2026-05-29 by kata
                        // 1665's N=50000 workload regressing from 3.2 ms
                        // to 1.1 s under a strawman mono-only dispatch).
                        // Runtime length check picks the right path per
                        // call.
                        let mono_fn = self.emit_sort_by_mono(
                            params,
                            body,
                            elem_ty,
                            elem_type_name.as_deref(),
                        )?;
                        let (thunk_fn, ctx_alloca) = self.emit_sort_by_inline_thunk(
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

                        // Threshold: 64 (power of 2; insertion sort
                        // competitive against the runtime callback's
                        // ~10 ns/compare overhead up to roughly this N).
                        // Above 64 the asymptotic O(N²) of insertion sort
                        // wins by tens of milliseconds even on small
                        // workloads.
                        let outer_fn = self.current_fn.unwrap();
                        let mono_call_bb =
                            self.context.append_basic_block(outer_fn, "sort_by.mono");
                        let runtime_call_bb =
                            self.context.append_basic_block(outer_fn, "sort_by.runtime");
                        let join_bb = self.context.append_basic_block(outer_fn, "sort_by.join");
                        let threshold = i64_t.const_int(64, false);
                        let use_runtime = self
                            .builder
                            .build_int_compare(
                                inkwell::IntPredicate::SGT,
                                len,
                                threshold,
                                "sort_by.use_runtime",
                            )
                            .unwrap();
                        self.builder
                            .build_conditional_branch(use_runtime, runtime_call_bb, mono_call_bb)
                            .unwrap();

                        self.builder.position_at_end(mono_call_bb);
                        self.builder
                            .build_call(
                                mono_fn,
                                &[
                                    BasicMetadataValueEnum::from(data),
                                    BasicMetadataValueEnum::from(len),
                                ],
                                "",
                            )
                            .unwrap();
                        self.builder.build_unconditional_branch(join_bb).unwrap();

                        self.builder.position_at_end(runtime_call_bb);
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
                        let thunk_ptr = thunk_fn.as_global_value().as_pointer_value();
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
                        self.builder.build_unconditional_branch(join_bb).unwrap();

                        self.builder.position_at_end(join_bb);
                        return Ok(self.context.i64_type().const_int(0, false).into());
                    }
                }

                // Three thunk shapes, dispatched by AST kind (mirror of
                // `sort_by_key` above):
                //   (a) inline closure expression — fuse the closure body
                //       into the bridge thunk, so each comparison is a
                //       single direct function call from the runtime helper
                //       (LLVM can then inline it freely);
                //   (b) closure-typed local Identifier — spill fat pointer,
                //       thunk does an indirect call through {fn_ptr,env_ptr};
                //   (c) named function Identifier — direct ABI, no env.
                // Named-struct elem type name for the inline-closure path
                // (captures present, or a non-mono-eligible elem — the
                // shapes the mono fast path above declines). Same lookup
                // and rationale as the mono dispatch: the inline thunk
                // re-compiles the body and needs it to resolve `a.field`.
                let elem_type_name: Option<String> = self
                    .var_elem_type_exprs
                    .get(var_name)
                    .and_then(|te| match &te.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    });
                let (thunk, ctx_alloca): (FunctionValue<'ctx>, PointerValue<'ctx>) = match &args[0]
                    .value
                    .kind
                {
                    ExprKind::Closure { params, body, .. } => self.emit_sort_by_inline_thunk(
                        params,
                        body,
                        elem_ty,
                        elem_type_name.as_deref(),
                    )?,
                    ExprKind::Identifier(name) => {
                        if let Some(&closure_fn_type) = self.closure_fn_types.get(name) {
                            let closure_val = self.compile_expr(&args[0].value)?;
                            let outer_fn = self.current_fn.unwrap();
                            let fat_ty = self.closure_value_type();
                            let cls_alloca =
                                self.create_entry_alloca(outer_fn, "sort_by.cls", fat_ty.into());
                            self.builder.build_store(cls_alloca, closure_val).unwrap();
                            (
                                self.emit_sort_by_thunk(elem_ty, closure_fn_type),
                                cls_alloca,
                            )
                        } else if let Some(named_fn) = self.module.get_function(name) {
                            let null_ctx = ptr_ty.const_null();
                            (self.emit_sort_by_named_thunk(elem_ty, named_fn), null_ctx)
                        } else {
                            return Err(format!(
                                "Vec.sort_by: identifier '{}' is neither a closure-typed \
                                 local nor a known function",
                                name
                            ));
                        }
                    }
                    _ => {
                        return Err("Vec.sort_by in codegen accepts an inline closure, a \
                             closure-typed local identifier, or a named function identifier; \
                             other callee shapes are not yet wired through the bridge thunk"
                            .to_string());
                    }
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
                // Integer elements use the signed-compare thunk; String
                // elements (the `{ptr,len,cap}` header) use the
                // `karac_string_cmp` byte-lexicographic thunk — the same
                // comparator `Vec.binary_search` / `sort_by` use for String
                // keys (so `keys().sort()` over a `Map[String,_]` report is
                // A/B-portable). Other element types (floats, tuples, user
                // structs) must use `sort_by(|a, b| ...)` with an explicit
                // comparator; their default ordering has no lowering yet, so
                // error loudly rather than silently leaving the Vec unsorted.
                let thunk = if elem_ty.is_int_type() {
                    self.emit_default_sort_thunk(elem_ty)
                } else if self.vec_elem_type_name(var_name).as_deref() == Some("String") {
                    self.emit_default_sort_thunk_string()
                } else {
                    return Err(
                        "Vec.sort() in codegen supports integer and String element types; \
                         use sort_by(|a, b| a.cmp(b)) for other element types"
                            .to_string(),
                    );
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
                // Three callee shapes, dispatched by AST kind:
                //   (a) inline closure → fuse body into the bridge thunk
                //       (per-key-type dispatch: int, string, struct, float,
                //       user-Ord, all via emit_sort_by_key_inline_thunk);
                //   (b) closure-typed local Identifier → spill fat pointer,
                //       thunk does an indirect call through {fn_ptr,env_ptr}
                //       (integer key only — non-inline path can't recover
                //       body span info for non-integer key dispatch);
                //   (c) named function Identifier → direct ABI, thunk calls
                //       the fn straight on each element (integer key only,
                //       same reason).
                let (thunk, ctx_alloca) = match &args[0].value.kind {
                    ExprKind::Closure { params, body, .. } => {
                        // Look up the Vec element's Kāra type name so the
                        // inline thunk can register `var_type_names` for
                        // the closure param. Without that, a body like
                        // `|s| s.field` can't recover the struct shape and
                        // the field load is silently elided. Pulls from
                        // `var_elem_type_exprs`; canonical first segment is
                        // the struct name for path-typed struct elements;
                        // tuple / generic / etc. fall back to `None`.
                        let elem_type_name: Option<String> = self
                            .var_elem_type_exprs
                            .get(var_name)
                            .and_then(|te| match &te.kind {
                                TypeKind::Path(p) => p.segments.last().cloned(),
                                _ => None,
                            });
                        self.emit_sort_by_key_inline_thunk(
                            params,
                            body.as_ref(),
                            elem_ty,
                            elem_type_name.as_deref(),
                        )?
                    }
                    ExprKind::Identifier(name) => {
                        if let Some(&closure_fn_type) = self.closure_fn_types.get(name) {
                            // Closure-typed local: compile to fat pointer,
                            // spill into an alloca, thunk reads it back.
                            let closure_val = self.compile_expr(&args[0].value)?;
                            let outer_fn = self.current_fn.unwrap();
                            let fat_ty = self.closure_value_type();
                            let cls_alloca = self.create_entry_alloca(
                                outer_fn,
                                "sort_by_key.cls",
                                fat_ty.into(),
                            );
                            self.builder.build_store(cls_alloca, closure_val).unwrap();
                            (
                                self.emit_sort_by_key_closure_thunk(elem_ty, closure_fn_type)?,
                                cls_alloca,
                            )
                        } else if let Some(named_fn) = self.module.get_function(name) {
                            // Named fn: direct ABI, no env. Pass a null ctx
                            // (the thunk ignores it).
                            let null_ctx = ptr_ty.const_null();
                            (
                                self.emit_sort_by_key_named_thunk(elem_ty, named_fn)?,
                                null_ctx,
                            )
                        } else {
                            return Err(format!(
                                "Vec.sort_by_key: identifier '{}' is neither a closure-typed \
                                 local nor a known function",
                                name
                            ));
                        }
                    }
                    _ => {
                        return Err("Vec.sort_by_key in codegen accepts an inline closure, a \
                             closure-typed local identifier, or a named function identifier; \
                             other callee shapes are not yet wired through the bridge thunk"
                            .to_string());
                    }
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
            // `String.contains(sub: String) -> bool` — substring search.
            // Disambiguated from `Vec.contains` via `string_vars`
            // membership, exactly like the `push` arm (String and Vec[u8]
            // share the `{ptr, len, cap}` shape). Naive O(n*m) scan: for
            // each start offset `i` where `i + sub.len <= recv.len`,
            // `memcmp(recv.data + i, sub.data, sub.len) == 0`. An empty
            // needle matches at i==0 (memcmp(.,.,0)==0), and a needle
            // longer than the haystack never enters the loop — both match
            // Rust's `str::contains` (and the interpreter's
            // `method_call_seq.rs` arm). Surfaced by B-2026-06-10-1.
            "contains" if self.string_vars.contains(var_name) => {
                if args.is_empty() {
                    return Err("String.contains requires a substring argument".to_string());
                }
                let bool_t = self.context.bool_type();
                let i32_t = self.context.i32_type();
                let i8_t = self.context.i8_type();

                // Receiver {data, len}.
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "ct.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "ct.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "ct.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "ct.recv.len")
                    .unwrap()
                    .into_int_value();

                // Needle: evaluate the arg, extract {data, len}.
                let needle_val = self.compile_expr(&args[0].value)?;
                let needle_struct = needle_val.into_struct_value();
                let needle_data = self
                    .builder
                    .build_extract_value(needle_struct, 0, "ct.needle.ptr")
                    .unwrap()
                    .into_pointer_value();
                let needle_len = self
                    .builder
                    .build_extract_value(needle_struct, 1, "ct.needle.len")
                    .unwrap()
                    .into_int_value();

                let fn_val = self.current_fn.unwrap();
                let head_bb = self.context.append_basic_block(fn_val, "ct.head");
                let body_bb = self.context.append_basic_block(fn_val, "ct.body");
                let found_bb = self.context.append_basic_block(fn_val, "ct.found");
                let next_bb = self.context.append_basic_block(fn_val, "ct.next");
                let done_bb = self.context.append_basic_block(fn_val, "ct.done");

                let i_slot = self.create_entry_alloca(fn_val, "ct.i", i64_t.into());
                let result_slot = self.create_entry_alloca(fn_val, "ct.result", bool_t.into());
                self.builder
                    .build_store(i_slot, i64_t.const_zero())
                    .unwrap();
                self.builder
                    .build_store(result_slot, bool_t.const_zero())
                    .unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                // head: continue while `i + needle_len <= recv_len`.
                self.builder.position_at_end(head_bb);
                let i = self
                    .builder
                    .build_load(i64_t, i_slot, "ct.i.load")
                    .unwrap()
                    .into_int_value();
                let i_end = self
                    .builder
                    .build_int_add(i, needle_len, "ct.i_end")
                    .unwrap();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULE, i_end, recv_len, "ct.in_range")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, done_bb)
                    .unwrap();

                // body: memcmp(recv_data + i, needle_data, needle_len) == 0?
                self.builder.position_at_end(body_bb);
                let window = unsafe {
                    self.builder
                        .build_gep(i8_t, recv_data, &[i], "ct.window")
                        .unwrap()
                };
                let cmp = self
                    .builder
                    .build_call(
                        self.memcmp_fn,
                        &[window.into(), needle_data.into(), needle_len.into()],
                        "ct.memcmp",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let is_match = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        cmp,
                        i32_t.const_zero(),
                        "ct.match",
                    )
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_match, found_bb, next_bb)
                    .unwrap();

                // found: record true, exit.
                self.builder.position_at_end(found_bb);
                self.builder
                    .build_store(result_slot, bool_t.const_int(1, false))
                    .unwrap();
                self.builder.build_unconditional_branch(done_bb).unwrap();

                // next: i++, loop.
                self.builder.position_at_end(next_bb);
                let i_next = self
                    .builder
                    .build_int_add(i, i64_t.const_int(1, false), "ct.i.next")
                    .unwrap();
                self.builder.build_store(i_slot, i_next).unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                self.builder.position_at_end(done_bb);
                let result = self
                    .builder
                    .build_load(bool_t, result_slot, "ct.load")
                    .unwrap();
                // Free a fresh-owned String needle temp (`keyword.contains(
                // s.substring(a, b))` — the lexer's keyword-membership shape).
                // The scan is complete at `done_bb`, so the needle buffer is no
                // longer read.
                self.free_fresh_owned_str_arg(&args[0].value, needle_val);
                Ok(result)
            }
            // `Vec.binary_search(x) -> Option[i64]` — Some(index) of a matching
            // element in the SORTED receiver, else None. Replicates Rust's
            // `slice::binary_search_by` midpoint loop EXACTLY (mid = left +
            // (right-left)/2, return on the first Equal mid) so the returned
            // index matches the interpreter even when duplicate keys are present.
            // The 3-way element compare (`emit_binary_search_cmp`) supports
            // integer (any width, signed/unsigned) and String element types; on
            // other element types it errors honestly (works under `karac run`).
            "binary_search" => {
                if args.len() != 1 {
                    return Err("Vec.binary_search requires 1 argument".to_string());
                }
                let elem_name = self.vec_elem_type_name(var_name).ok_or_else(|| {
                    "Vec.binary_search: could not resolve the element type in codegen".to_string()
                })?;
                // The `{ptr, len, cap}` header; binary_search reads {ptr, len}.
                let data = {
                    let p = self
                        .builder
                        .build_struct_gep(vec_ty, data_ptr, 0, "bs.data.p")
                        .unwrap();
                    self.builder
                        .build_load(ptr_ty, p, "bs.data")
                        .unwrap()
                        .into_pointer_value()
                };
                let len = {
                    let p = self
                        .builder
                        .build_struct_gep(vec_ty, data_ptr, 1, "bs.len.p")
                        .unwrap();
                    self.builder
                        .build_load(i64_t, p, "bs.len")
                        .unwrap()
                        .into_int_value()
                };
                self.compile_binary_search(data, len, elem_ty, &elem_name, &args[0])
            }
            // `Vec.contains(x) -> bool` / `Slice.contains(x) -> bool` —
            // linear element scan. Each element is loaded and compared to
            // the (once-evaluated) needle via the same `==` lowering the
            // binary operator uses (`compile_binop(BinOp::Eq, ..)`), so
            // scalar, String, and user-struct element types all work
            // (the typechecker already enforces `arg : elem`). Mirrors the
            // interpreter's `v.contains(&needle)`. Surfaced by
            // B-2026-06-10-1.
            "contains" => {
                if args.is_empty() {
                    return Err("Vec.contains requires an argument".to_string());
                }
                let bool_t = self.context.bool_type();

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vct.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vct.len.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "vct.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "vct.len")
                    .unwrap()
                    .into_int_value();

                // Evaluate the needle once, before the loop.
                let needle_val = self.compile_expr(&args[0].value)?;

                let fn_val = self.current_fn.unwrap();
                let head_bb = self.context.append_basic_block(fn_val, "vct.head");
                let body_bb = self.context.append_basic_block(fn_val, "vct.body");
                let found_bb = self.context.append_basic_block(fn_val, "vct.found");
                let next_bb = self.context.append_basic_block(fn_val, "vct.next");
                let done_bb = self.context.append_basic_block(fn_val, "vct.done");

                let i_slot = self.create_entry_alloca(fn_val, "vct.i", i64_t.into());
                let result_slot = self.create_entry_alloca(fn_val, "vct.result", bool_t.into());
                self.builder
                    .build_store(i_slot, i64_t.const_zero())
                    .unwrap();
                self.builder
                    .build_store(result_slot, bool_t.const_zero())
                    .unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                // head: continue while `i < len`.
                self.builder.position_at_end(head_bb);
                let i = self
                    .builder
                    .build_load(i64_t, i_slot, "vct.i.load")
                    .unwrap()
                    .into_int_value();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, i, len, "vct.in_range")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, done_bb)
                    .unwrap();

                // body: load data[i], compare to needle.
                self.builder.position_at_end(body_bb);
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[i], "vct.elem.ptr")
                        .unwrap()
                };
                let elem_val = self
                    .builder
                    .build_load(elem_ty, elem_ptr, "vct.elem")
                    .unwrap();
                let eq = self
                    .compile_binop(&BinOp::Eq, elem_val, needle_val)?
                    .into_int_value();
                // `compile_binop` may have emitted its own blocks (e.g.
                // struct element equality); branch from wherever it left
                // the insert point, not the assumed `body_bb`.
                self.builder
                    .build_conditional_branch(eq, found_bb, next_bb)
                    .unwrap();

                // found: record true, exit.
                self.builder.position_at_end(found_bb);
                self.builder
                    .build_store(result_slot, bool_t.const_int(1, false))
                    .unwrap();
                self.builder.build_unconditional_branch(done_bb).unwrap();

                // next: i++, loop.
                self.builder.position_at_end(next_bb);
                let i_next = self
                    .builder
                    .build_int_add(i, i64_t.const_int(1, false), "vct.i.next")
                    .unwrap();
                self.builder.build_store(i_slot, i_next).unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                self.builder.position_at_end(done_bb);
                let result = self
                    .builder
                    .build_load(bool_t, result_slot, "vct.load")
                    .unwrap();
                Ok(result)
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

    /// Default-order comparator thunk for `Vec[String].sort()`: each element is
    /// the `{ptr, len, cap}` String header, compared byte-lexicographically via
    /// the `karac_string_cmp` runtime fn (the same comparator
    /// `Vec.binary_search` and the String-key `sort_by` path use). The
    /// bare-`sort()` String analog of [`emit_default_sort_thunk`]. A `Vec[T]`
    /// element can't reach here — `Vec[T]` is not `Ord`, so the typechecker only
    /// admits `.sort()` on a String-element Vec among the heap `{ptr,len,cap}`
    /// shapes (the sort arm gates on the `String` element type name).
    pub(super) fn emit_default_sort_thunk_string(&mut self) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();

        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_default_strcmp_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(thunk_fn);

        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        // params: (ctx, *a, *b) — a/b point to the String header in the buffer.
        let a_ptr = thunk_fn.get_nth_param(1).unwrap().into_pointer_value();
        let b_ptr = thunk_fn.get_nth_param(2).unwrap().into_pointer_value();
        let a = self
            .builder
            .build_load(vec_ty, a_ptr, "a.str")
            .unwrap()
            .into_struct_value();
        let b = self
            .builder
            .build_load(vec_ty, b_ptr, "b.str")
            .unwrap()
            .into_struct_value();
        let a_data = self
            .builder
            .build_extract_value(a, 0, "a.str.ptr")
            .unwrap()
            .into_pointer_value();
        let a_len = self
            .builder
            .build_extract_value(a, 1, "a.str.len")
            .unwrap()
            .into_int_value();
        let b_data = self
            .builder
            .build_extract_value(b, 0, "b.str.ptr")
            .unwrap()
            .into_pointer_value();
        let b_len = self
            .builder
            .build_extract_value(b, 1, "b.str.len")
            .unwrap()
            .into_int_value();

        let cmp_fn = self
            .module
            .get_function("karac_string_cmp")
            .unwrap_or_else(|| {
                let fn_ty = i64_t.fn_type(
                    &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                    false,
                );
                self.module
                    .add_function("karac_string_cmp", fn_ty, Some(Linkage::External))
            });
        let res = self
            .builder
            .build_call(
                cmp_fn,
                &[a_data.into(), a_len.into(), b_data.into(), b_len.into()],
                "str.cmp",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
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
        // Clear the par-branch cancel pointer for the thunk body (B-2026-06-18-10):
        // the comparator is a separate fn, so a method call in it (e.g. `a.cmp(b)`)
        // must not load the enclosing par-branch's `cancel_flag` arg.
        let saved_cancel_ptr = self.branch_cancel_ptr.take();

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
        self.branch_cancel_ptr = saved_cancel_ptr;
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
        elem_type_name: Option<&str>,
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
        // Clear the par-branch cancel pointer for the thunk body (B-2026-06-18-10):
        // the comparator is a separate fn, so a method call in it (e.g. `a.cmp(b)`)
        // must not load the enclosing par-branch's `cancel_flag` arg.
        let saved_cancel_ptr = self.branch_cancel_ptr.take();

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
            // For named-struct elements, register the closure param's
            // Kāra type name so the body's `a.field` / `b.field` access
            // resolves to the right field index. Without this the runtime
            // path's inline thunk silently mis-lowered named-field
            // comparisons (the compare returned a constant → an
            // always-equal comparator → `karac_vec_sort_by` left the vec
            // in original order at N>64, while the mono path at N≤64 — which
            // already registered this — sorted correctly). Mirrors the
            // mono emitter's registration; tuples pass None and route
            // through the numeric `.0`/`.1` index path that needs no name.
            if let Some(name) = elem_type_name {
                self.record_var_type_name(param_name.clone(), name.to_string());
            }
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
        self.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok((thunk_fn, env_alloca))
    }

    /// Gate predicate for the monomorphized `Vec[T].sort_by` fast path.
    /// Slice 6.1 shipped `T = i64`; Slice 6.4 widens to LLVM struct types
    /// whose fields are all integers — covers integer tuples like
    /// `(i64, i64)` (kata 56's natural-pull trigger), `(i64, i64, i64)`
    /// (kata 1665's secondary witness), and integer-field user structs
    /// (`struct Score { v: i64 }`). The mono emitter treats the elem as
    /// an opaque-sized blob for the sort's load / store / copy machinery,
    /// and the closure body's `.0` / `.1` / `.field_name` accesses route
    /// through `compile_expr`'s existing tuple-index / named-field
    /// extract paths. For named structs the caller passes an
    /// `elem_type_name` so the emitter can register `var_type_names`
    /// for the closure params (mirrors `emit_sort_by_key_inline_thunk`'s
    /// var_type_names fix at commit `079f5d7f`).
    ///
    /// Non-integer fields (Float / Pointer / String 3-word struct) fall
    /// through because their compare lowering isn't yet wired into the
    /// mono path's `tag - 1` Ordering contract — those are sibling Slice
    /// 6.2+ entries (see `docs/implementation_checklist/phase-7-codegen.md`
    /// Slice 6 trigger entry). Cross-ref: kata 56's
    /// `merge_intervals.kara` + kata 1665's `greedy.kara` are the corpus
    /// witnesses for tuple-elem; kata 15 / 16 are the i64 witnesses.
    pub(super) fn should_use_mono_vec_sort_by_for(&self, elem_ty: BasicTypeEnum<'ctx>) -> bool {
        match elem_ty {
            BasicTypeEnum::IntType(t) => t == self.context.i64_type(),
            BasicTypeEnum::StructType(s) => {
                let n = s.count_fields();
                if n == 0 {
                    return false;
                }
                (0..n).all(|i| {
                    s.get_field_type_at_index(i)
                        .is_some_and(|f| f.is_int_type())
                })
            }
            _ => false,
        }
    }

    /// Per-call-site monomorphized sort function for
    /// `Vec[T].sort_by(inline_closure)`. Signature:
    /// `void __vec_<elem_mangle>_sort_by_mono_<id>(data: *mut T, len: i64)`
    /// (Internal linkage). The function body is an insertion sort with the
    /// user's comparator inlined at the inner compare — no `karac_vec_sort_by`
    /// callback, LLVM has direct visibility into both the sort algorithm and
    /// the comparator, so the compare-and-shift loop optimises end-to-end
    /// (branchless compares, hoisted loads, fused arithmetic).
    ///
    /// **Element type parameterisation.** `elem_ty` flows through every
    /// load/store/GEP that touches the data buffer or the closure-param
    /// slots — Slice 6.1 hardcoded `i64`; Slice 6.4 parameterised over
    /// any shape `should_use_mono_vec_sort_by_for` accepts (i64 plus
    /// LLVM struct types whose fields are all integers, i.e. integer
    /// tuples and `#[derive(Ord)]` integer-field structs). For struct
    /// elems the loads/stores treat the value as opaque-sized
    /// `BasicValueEnum`; the closure body's `.0` / `.field_name` access
    /// goes through `compile_expr`'s existing tuple-/struct-extract path
    /// when the per-call-site comparator references it.
    ///
    /// **Algorithm choice — insertion sort.** Simple (~30 lines of IR
    /// builder), validated by the kata-16 README's inline-insertion-sort
    /// A/B experiment that closed 76% of the gap to Rust (96.8 → 70.6 ms at
    /// N=16). O(N²) is fine for the current corpus (kata 15 / 16 / 56 / 1665
    /// all run N ≤ 50). A future slice can swap in a PDQ small-sort network
    /// or call out to a typed `karac_vec_sort_<T>_*` runtime helper when a
    /// larger-N workload pulls — the gate predicate above is the chokepoint.
    ///
    /// **Captures unsupported in this slice.** The caller's free-vars check
    /// gates entry on `collect_closure_free_vars` returning empty. Closures
    /// that capture outer scope (e.g. `s.sort_by(|a, b| (a - pivot).cmp(b - pivot))`
    /// referencing `pivot`) fall through to the existing thunk path. Future
    /// slice threads captures as extra params or via an env struct mirror.
    ///
    /// **Ordering result handling** mirrors `emit_sort_by_inline_thunk` —
    /// the closure body returns either an `Ordering` struct `{ i64 tag }`
    /// (for `a.cmp(b)` shapes) or a bare `i64` (for hand-rolled `if a < b
    /// { -1i64 } else if ...` shapes); we extract the tag in the struct
    /// case and subtract 1 to get the `-1 / 0 / +1` signed comparator
    /// value. We then test `> 0` (meaning `a > b`) to decide whether to
    /// shift `data[jj]` rightward — i.e. the closure controls the sort
    /// ORDER, not just the value extraction.
    #[allow(clippy::too_many_lines)]
    pub(super) fn emit_sort_by_mono(
        &mut self,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_type_name: Option<&str>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let void_t = self.context.void_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // 1. Declare per-call-site mono fn. Internal linkage — each call
        //    site emits a fresh copy (closure body varies per call site, so
        //    LinkOnceODR would risk silent body-mismatch across TUs sharing
        //    a counter id). The elem-type token in the name keeps mono
        //    symbols across different sort_by call sites textually distinct
        //    when their counter ids overlap across TUs.
        let id = self.closure_counter;
        self.closure_counter += 1;
        let elem_mangle = self.llvm_type_to_mangle_str(elem_ty);
        let name = format!("__vec_{}_sort_by_mono_{}", elem_mangle, id);
        let fn_ty = void_t.fn_type(&[ptr_ty.into(), i64_t.into()], false);
        let sort_fn = self
            .module
            .add_function(&name, fn_ty, Some(Linkage::Internal));

        // 2. Save outer codegen state — we're about to compile into a new fn.
        //    Same save/restore dance as `emit_sort_by_inline_thunk`.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_subst = std::mem::take(&mut self.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_fn_types);
        let saved_pct = self.pending_closure_fn_type.take();
        // Clear the par-branch cancel pointer for the mono sort body
        // (B-2026-06-18-10): the comparison call this routine inlines must not
        // load the enclosing par-branch's `cancel_flag` arg — `%1` here is the
        // i64 length, not a cancel pointer.
        let saved_cancel_ptr = self.branch_cancel_ptr.take();

        self.current_fn = Some(sort_fn);

        let data = sort_fn.get_nth_param(0).unwrap().into_pointer_value();
        let len = sort_fn.get_nth_param(1).unwrap().into_int_value();

        // 3. BB scaffold for insertion sort:
        //
        //     entry        → ii = 1; goto outer_chk
        //     outer_chk    → if ii < len then outer_body else exit
        //     outer_body   → key = data[ii]; jj = ii - 1; goto inner_chk
        //     inner_chk    → if jj >= 0 then inner_cmp else inner_done
        //     inner_cmp    → load data[jj]; compile closure body with
        //                    a = data[jj], b = key; tag - 1 > 0 ?
        //                    inner_shift : inner_done
        //     inner_shift  → data[jj+1] = data[jj]; jj -= 1; goto inner_chk
        //     inner_done   → data[jj+1] = key; ii += 1; goto outer_chk
        //     exit         → ret void
        let entry = self.context.append_basic_block(sort_fn, "entry");
        let outer_chk = self.context.append_basic_block(sort_fn, "outer.chk");
        let outer_body = self.context.append_basic_block(sort_fn, "outer.body");
        let inner_chk = self.context.append_basic_block(sort_fn, "inner.chk");
        let inner_cmp = self.context.append_basic_block(sort_fn, "inner.cmp");
        let inner_shift = self.context.append_basic_block(sort_fn, "inner.shift");
        let inner_done = self.context.append_basic_block(sort_fn, "inner.done");
        let exit = self.context.append_basic_block(sort_fn, "exit");

        self.builder.position_at_end(entry);
        let ii_alloca = self.create_entry_alloca(sort_fn, "ii", i64_t.into());
        let jj_alloca = self.create_entry_alloca(sort_fn, "jj", i64_t.into());
        // `key` holds an elem-typed value (i64 in Slice 6.1, tuple/struct
        // in Slice 6.4+) — same stride as the data buffer.
        let key_alloca = self.create_entry_alloca(sort_fn, "key", elem_ty);
        let one = i64_t.const_int(1, false);
        let zero = i64_t.const_zero();
        self.builder.build_store(ii_alloca, one).unwrap();
        self.builder.build_unconditional_branch(outer_chk).unwrap();

        // outer_chk: ii < len ?
        self.builder.position_at_end(outer_chk);
        let ii_v = self
            .builder
            .build_load(i64_t, ii_alloca, "ii.load")
            .unwrap()
            .into_int_value();
        let outer_cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, ii_v, len, "outer.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(outer_cond, outer_body, exit)
            .unwrap();

        // outer_body: key = data[ii]; jj = ii - 1
        self.builder.position_at_end(outer_body);
        let ii_v2 = self
            .builder
            .build_load(i64_t, ii_alloca, "ii.load2")
            .unwrap()
            .into_int_value();
        // GEP stride is `elem_ty` — for `(i64, i64)` that's 16 bytes per
        // step, so `data[ii]` lands at the right offset for the elem layout.
        let key_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[ii_v2], "key.addr")
                .unwrap()
        };
        let key_v = self
            .builder
            .build_load(elem_ty, key_addr, "key.load")
            .unwrap();
        self.builder.build_store(key_alloca, key_v).unwrap();
        let jj_init = self.builder.build_int_sub(ii_v2, one, "jj.init").unwrap();
        self.builder.build_store(jj_alloca, jj_init).unwrap();
        self.builder.build_unconditional_branch(inner_chk).unwrap();

        // inner_chk: jj >= 0 ?
        self.builder.position_at_end(inner_chk);
        let jj_v = self
            .builder
            .build_load(i64_t, jj_alloca, "jj.load")
            .unwrap()
            .into_int_value();
        let jj_ge_0 = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGE, jj_v, zero, "jj.ge.0")
            .unwrap();
        self.builder
            .build_conditional_branch(jj_ge_0, inner_cmp, inner_done)
            .unwrap();

        // inner_cmp: load data[jj]; bind closure params (a = data[jj],
        // b = key); compile closure body; tag - 1 > 0 means "a > b" →
        // shift; otherwise done.
        self.builder.position_at_end(inner_cmp);
        let jj_v2 = self
            .builder
            .build_load(i64_t, jj_alloca, "jj.load2")
            .unwrap()
            .into_int_value();
        let dj_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jj_v2], "dj.addr")
                .unwrap()
        };
        let dj_v = self
            .builder
            .build_load(elem_ty, dj_addr, "dj.load")
            .unwrap();
        let key_v2 = self
            .builder
            .build_load(elem_ty, key_alloca, "key.load2")
            .unwrap();

        // Bind closure params. param[0] = data[jj] (the "left" side, swept
        // back through the sorted prefix); param[1] = key (the "right" side,
        // the freshly-chosen unsorted element being inserted). When the
        // elem is a named struct, also register `var_type_names` so the
        // closure body's `.field_name` lookups resolve through the named-
        // field path (mirrors `emit_sort_by_key_inline_thunk`'s fix at
        // commit `079f5d7f`; for anonymous tuples elem_type_name is None
        // and `.0`/`.1` indexing doesn't need the map).
        let param_vals = [dj_v, key_v2];
        for (i, cp) in params.iter().enumerate().take(2) {
            let val = param_vals[i];
            let param_name = match &cp.pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                _ => format!("_cp{}", i),
            };
            let ty = val.get_type();
            let alloca = self.create_entry_alloca(sort_fn, &param_name, ty);
            self.builder.build_store(alloca, val).unwrap();
            self.variables
                .insert(param_name.clone(), VarSlot { ptr: alloca, ty });
            if let Some(name) = elem_type_name {
                self.record_var_type_name(param_name, name.to_string());
            }
        }

        // Compile closure body; extract Ordering tag if struct-typed;
        // compute cmp = tag - 1 (yields -1 / 0 / +1).
        let result = self.compile_expr(body)?;
        let tag = if result.is_struct_value() {
            self.builder
                .build_extract_value(result.into_struct_value(), 0, "tag")
                .unwrap()
                .into_int_value()
        } else {
            result.into_int_value()
        };
        let cmp_value = self.builder.build_int_sub(tag, one, "cmp").unwrap();
        let cmp_gt_0 = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, cmp_value, zero, "cmp.gt.0")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp_gt_0, inner_shift, inner_done)
            .unwrap();

        // inner_shift: data[jj+1] = data[jj]; jj -= 1; goto inner_chk
        self.builder.position_at_end(inner_shift);
        let jj_v3 = self
            .builder
            .build_load(i64_t, jj_alloca, "jj.load3")
            .unwrap()
            .into_int_value();
        let dj_addr2 = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jj_v3], "dj.addr2")
                .unwrap()
        };
        let dj_v2 = self
            .builder
            .build_load(elem_ty, dj_addr2, "dj.load2")
            .unwrap();
        let jj_p1 = self.builder.build_int_add(jj_v3, one, "jj.p1").unwrap();
        let dst_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jj_p1], "dst.addr")
                .unwrap()
        };
        self.builder.build_store(dst_addr, dj_v2).unwrap();
        let jj_m1 = self.builder.build_int_sub(jj_v3, one, "jj.m1").unwrap();
        self.builder.build_store(jj_alloca, jj_m1).unwrap();
        self.builder.build_unconditional_branch(inner_chk).unwrap();

        // inner_done: data[jj+1] = key; ii += 1; goto outer_chk
        self.builder.position_at_end(inner_done);
        let jj_v4 = self
            .builder
            .build_load(i64_t, jj_alloca, "jj.load4")
            .unwrap()
            .into_int_value();
        let key_v3 = self
            .builder
            .build_load(elem_ty, key_alloca, "key.load3")
            .unwrap();
        let dst2_idx = self.builder.build_int_add(jj_v4, one, "dst2.idx").unwrap();
        let dst2_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[dst2_idx], "dst2.addr")
                .unwrap()
        };
        self.builder.build_store(dst2_addr, key_v3).unwrap();
        let ii_v3 = self
            .builder
            .build_load(i64_t, ii_alloca, "ii.load3")
            .unwrap()
            .into_int_value();
        let ii_new = self.builder.build_int_add(ii_v3, one, "ii.new").unwrap();
        self.builder.build_store(ii_alloca, ii_new).unwrap();
        self.builder.build_unconditional_branch(outer_chk).unwrap();

        // exit
        self.builder.position_at_end(exit);
        self.builder.build_return(None).unwrap();

        // Restore outer state.
        self.type_subst = saved_subst;
        self.loop_stack = saved_loop_stack;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_fn_types = saved_cfn;
        self.pending_closure_fn_type = saved_pct;
        self.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok(sort_fn)
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

    /// `Vec.sort_by_key` non-inline thunk for a **closure-typed local** key
    /// (`let k = |x| ...; v.sort_by_key(k)`). Mirror of `emit_sort_by_thunk`
    /// for the sort_by_key shape — ctx holds the closure's spilled fat
    /// pointer `{fn_ptr, env_ptr}`; the thunk extracts both, calls the
    /// closure indirectly *twice* (once per element) to get key_a / key_b,
    /// then returns the signed integer compare as `-1 / 0 / +1`. Only
    /// integer key types are supported on the non-inline path today —
    /// non-integer keys error loudly directing the user to the inline
    /// closure form (the per-key-type dispatch in the inline thunk needs
    /// the body Expr's span for `string_typed_exprs` etc., which the
    /// non-inline path doesn't have at the call site).
    pub(super) fn emit_sort_by_key_closure_thunk(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
        closure_fn_type: FunctionType<'ctx>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let key_ty = closure_fn_type
            .get_return_type()
            .ok_or_else(|| "Vec.sort_by_key: closure has no return type".to_string())?;
        if !key_ty.is_int_type() {
            return Err(
                "Vec.sort_by_key in codegen supports only integer key types for non-inline \
                 closure callees today; rewrite as an inline closure `|x| ...` for String, \
                 struct, float, or user-Ord keys"
                    .to_string(),
            );
        }

        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_by_key_closure_thunk_{}", id);
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

        let call_a = self
            .builder
            .build_indirect_call(
                closure_fn_type,
                cls_fn,
                &[
                    BasicMetadataValueEnum::from(cls_env),
                    BasicMetadataValueEnum::from(a_val),
                ],
                "key.a",
            )
            .unwrap();
        let key_a = call_a.try_as_basic_value().unwrap_basic().into_int_value();
        let call_b = self
            .builder
            .build_indirect_call(
                closure_fn_type,
                cls_fn,
                &[
                    BasicMetadataValueEnum::from(cls_env),
                    BasicMetadataValueEnum::from(b_val),
                ],
                "key.b",
            )
            .unwrap();
        let key_b = call_b.try_as_basic_value().unwrap_basic().into_int_value();

        let lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, key_a, key_b, "key.lt")
            .unwrap();
        let gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, key_a, key_b, "key.gt")
            .unwrap();
        let zero = i64_t.const_zero();
        let neg_one = i64_t.const_int((-1i64) as u64, true);
        let pos_one = i64_t.const_int(1, false);
        let gt_sel = self
            .builder
            .build_select(gt, pos_one, zero, "key.gt.sel")
            .unwrap()
            .into_int_value();
        let res = self
            .builder
            .build_select(lt, neg_one, gt_sel, "key.cmp.sel")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&res)).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Ok(thunk_fn)
    }

    /// `Vec.sort_by_key` non-inline thunk for a **named-function** key
    /// (`fn key(x) -> K { ... } ... v.sort_by_key(key)`). The named fn
    /// has the direct ABI (no `env_ptr` first param), so the thunk just
    /// calls it twice on the loaded elements with no closure machinery
    /// and ignores its own ctx pointer. Same integer-only key constraint
    /// as the closure-typed-local thunk above for the same reason.
    pub(super) fn emit_sort_by_key_named_thunk(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
        named_fn: FunctionValue<'ctx>,
    ) -> Result<FunctionValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let key_ty = named_fn
            .get_type()
            .get_return_type()
            .ok_or_else(|| "Vec.sort_by_key: named key fn has no return type".to_string())?;
        if !key_ty.is_int_type() {
            return Err(
                "Vec.sort_by_key in codegen supports only integer key types for non-inline \
                 named-function callees today; rewrite as an inline closure `|x| named_fn(x)` \
                 for String, struct, float, or user-Ord keys"
                    .to_string(),
            );
        }

        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_by_key_named_thunk_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(thunk_fn);

        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        // ctx (param 0) is unused for the named-fn path — direct ABI has no env.
        let a_ptr = thunk_fn.get_nth_param(1).unwrap().into_pointer_value();
        let b_ptr = thunk_fn.get_nth_param(2).unwrap().into_pointer_value();

        let a_val = self.builder.build_load(elem_ty, a_ptr, "a").unwrap();
        let b_val = self.builder.build_load(elem_ty, b_ptr, "b").unwrap();

        let call_a = self
            .builder
            .build_call(named_fn, &[BasicMetadataValueEnum::from(a_val)], "key.a")
            .unwrap();
        let key_a = call_a.try_as_basic_value().unwrap_basic().into_int_value();
        let call_b = self
            .builder
            .build_call(named_fn, &[BasicMetadataValueEnum::from(b_val)], "key.b")
            .unwrap();
        let key_b = call_b.try_as_basic_value().unwrap_basic().into_int_value();

        let lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, key_a, key_b, "key.lt")
            .unwrap();
        let gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, key_a, key_b, "key.gt")
            .unwrap();
        let zero = i64_t.const_zero();
        let neg_one = i64_t.const_int((-1i64) as u64, true);
        let pos_one = i64_t.const_int(1, false);
        let gt_sel = self
            .builder
            .build_select(gt, pos_one, zero, "key.gt.sel")
            .unwrap()
            .into_int_value();
        let res = self
            .builder
            .build_select(lt, neg_one, gt_sel, "key.cmp.sel")
            .unwrap()
            .into_int_value();
        self.builder.build_return(Some(&res)).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Ok(thunk_fn)
    }

    /// `Vec.sort_by` non-inline thunk for a **named-function** comparator
    /// (`fn cmp(a, b) -> Ordering ... v.sort_by(cmp)`). Direct ABI (no
    /// env_ptr); ctx is unused. The thunk calls the named fn directly with
    /// (a, b), extracts the Ordering tag (via the layout seeded in
    /// `seed_builtin_enum_layouts`), and returns `tag - 1` — same shape
    /// as `emit_sort_by_thunk`'s indirect path for closure-typed locals.
    pub(super) fn emit_sort_by_named_thunk(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
        named_fn: FunctionValue<'ctx>,
    ) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let id = self.closure_counter;
        self.closure_counter += 1;
        let name = format!("__sort_by_named_thunk_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(thunk_fn);

        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);

        // ctx (param 0) unused — direct ABI.
        let a_ptr = thunk_fn.get_nth_param(1).unwrap().into_pointer_value();
        let b_ptr = thunk_fn.get_nth_param(2).unwrap().into_pointer_value();

        let a_val = self.builder.build_load(elem_ty, a_ptr, "a").unwrap();
        let b_val = self.builder.build_load(elem_ty, b_ptr, "b").unwrap();

        let call = self
            .builder
            .build_call(
                named_fn,
                &[
                    BasicMetadataValueEnum::from(a_val),
                    BasicMetadataValueEnum::from(b_val),
                ],
                "ord",
            )
            .unwrap();
        let ord_val = call.try_as_basic_value().unwrap_basic();
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
