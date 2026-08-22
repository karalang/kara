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

use inkwell::basic_block::BasicBlock;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue,
};
use inkwell::AddressSpace;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    /// B-2026-08-01-24: a heap-owning `for`-loop struct ELEMENT binding pushed
    /// whole into a container (`for h in headers { out.push(h) }`). The loop
    /// binding is a shallow bit-copy of the source container's element slot
    /// (`for_loop_owned_agg_vars`, B-2026-07-04-17), so the just-stored value's
    /// heap fields alias buffers the SOURCE container's per-element drop still
    /// frees — the move-suppression cap-zero (`suppress_source_vec_cleanup_for_
    /// arg`) neutralizes only the binding's ALLOCA, which nothing drops; the
    /// live owner is the source element slot the binding was loaded from.
    /// Deep-copy the stored element's heap fields in place at `elem_slot` so
    /// the destination container and the source container free independent
    /// buffers — the push/insert twin of the `let x = a` whole-move arm in
    /// `deep_copy_for_loop_agg_element_move` (copy-depth == drop-depth).
    /// Bare-`shared` fields rc-INC during the copy (the source drain decs its
    /// own handle; the destination's element walk decs the inc'd copy —
    /// balanced, same as the LET arm, B-2026-07-18-2). A value-ENUM element
    /// takes the same treatment through the entry-copy twin
    /// (`deep_copy_enum_heap_payload_in_place` — live-variant heap payload
    /// only, matching the enum drop's coverage). No-op unless the arg is a
    /// bare Identifier naming a for-loop struct/enum element.
    pub(super) fn deep_copy_pushed_for_loop_agg_element(
        &mut self,
        arg: &Expr,
        elem_slot: PointerValue<'ctx>,
    ) {
        let ExprKind::Identifier(src) = &arg.kind else {
            return;
        };
        if !self
            .borrow_vars
            .for_loop_owned_agg_vars
            .contains(src.as_str())
        {
            return;
        }
        let Some(type_name) = self.var_types.var_type_names.get(src.as_str()).cloned() else {
            return;
        };
        if self.type_decls.shared_types.contains_key(&type_name) {
            return;
        }
        if self.type_decls.struct_types.contains_key(&type_name) {
            let saved = self.drop_rc.deep_copy_rc_inc_bare_shared;
            self.drop_rc.deep_copy_rc_inc_bare_shared = true;
            self.deep_copy_struct_heap_fields_in_place(elem_slot, &type_name);
            self.drop_rc.deep_copy_rc_inc_bare_shared = saved;
            return;
        }
        if let Some(layout) = self.type_decls.enum_layouts.get(&type_name).cloned() {
            if !layout.is_shared {
                self.deep_copy_enum_heap_payload_in_place(&type_name, elem_slot, &layout);
            }
        }
    }

    /// Whether `arg` is a bare identifier naming a for-loop STRUCT element
    /// binding (`for_loop_owned_agg_vars`, non-shared) — the gate the staged
    /// deep-copy hook above fires on for struct elements. Used by no-adopt
    /// branches to decide whether a staged copy exists to reclaim
    /// (B-2026-08-01-29).
    pub(super) fn arg_is_for_loop_struct_elem(&self, arg: &Expr) -> bool {
        matches!(&arg.kind, ExprKind::Identifier(src)
            if self.borrow_vars.for_loop_owned_agg_vars.contains(src.as_str())
                && self
                    .var_types
                        .var_type_names
                    .get(src.as_str())
                    .is_some_and(|t| self.type_decls.struct_types.contains_key(t.as_str())
                        && !self.type_decls.shared_types.contains_key(t.as_str())))
    }

    /// B-2026-08-01-29 — reclaim the STAGED deep copy on a no-adopt branch.
    /// When a Map/Set insert's key slot was deep-copied for a for-loop
    /// struct element (`deep_copy_pushed_for_loop_agg_element`) and the
    /// runtime kept the bucket's existing key (duplicate) or left the
    /// container unchanged (OOM), the staged copy's field buffers are
    /// orphaned — the existing no-adopt frees are vec-struct-gated and skip
    /// struct aggregates. Run the memory-only `__karac_drop_struct_<T>` on
    /// the slot; Drop BODIES are the separate UserDrop channel and must not
    /// fire for a value the program never observed as stored. No-op unless
    /// the arg matches the same gate the copy hook fired on.
    pub(super) fn free_staged_for_loop_agg_copy_on_no_adopt(
        &mut self,
        arg: &Expr,
        slot: PointerValue<'ctx>,
    ) {
        if !self.arg_is_for_loop_struct_elem(arg) {
            return;
        }
        let ExprKind::Identifier(src) = &arg.kind else {
            return;
        };
        let Some(type_name) = self.var_types.var_type_names.get(src.as_str()).cloned() else {
            return;
        };
        if let Some(drop_fn) = self.emit_struct_drop_synthesis(&type_name) {
            self.builder
                .build_call(drop_fn, &[slot.into()], "staged.noadopt.drop")
                .unwrap();
        }
    }

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
            let f = self.module.add_function(sym, ty, Some(Linkage::External));
            // Realloc-family attrs (phase-10 line 284): `Realloc | Uninitialized`
            // (0b1010), reads the old buffer (argmem), resizes param 0
            // (`allocptr`), aborts on OOM (no `willreturn`).
            crate::codegen::apply_alloc_family_attrs(self.context, f, 0b1010, false, true, Some(0));
            f
        })
    }

    /// Lazily declare (get-or-add) the `calloc`-backed zeroed-allocation wrapper
    /// used by the `Vec.filled(n, 0)` fast path (B-2026-07-08-7). Unlike the
    /// byte-count `alloc_or_panic` it takes `(count, size)` — `ptr fn(i64, i64)`
    /// — so `calloc` does the multiply with its own overflow check. Cold enough
    /// (one call site) to declare on demand rather than cache a struct field.
    pub(super) fn alloc_zeroed_or_panic_fn_decl(&self) -> inkwell::values::FunctionValue<'ctx> {
        let sym = crate::codegen::driver::c_alloc_zeroed_or_panic_symbol();
        self.module.get_function(sym).unwrap_or_else(|| {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i64_t = self.context.i64_type();
            let ty = ptr_ty.fn_type(&[i64_t.into(), i64_t.into()], false);
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
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // SSO: tag-aware owned-heap gate (`SGT cap, 0`) — an inline string
        // (`cap < 0`) must NOT be realloc'd (its buffer is the struct itself),
        // so it correctly takes the fresh-malloc + copy path below. Proven
        // no-op today (no `cap` has bit 63 set); extends the Slice-1 free-gate
        // hardening to the `String.push`/`push_str` grow path. NOTE: the fresh
        // path's memcpy source (`data`) is still the raw field-0 load — making
        // it the tag-aware inline data ptr is the coupled construction-flip task.
        let was_heap = self.sso_string_is_owned_heap(cap);
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
                self.runtime_fns.alloc_or_panic_fn,
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
        match self
            .var_types
            .var_elem_type_exprs
            .get(var_name)
            .map(|te| &te.kind)
        {
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
        let is_uint = matches!(elem_name, "u8" | "u16" | "u32" | "u64" | "u128" | "usize");
        let is_int =
            is_uint || matches!(elem_name, "i8" | "i16" | "i32" | "i64" | "i128" | "isize");
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
                 `karac build` (codegen); it works under `karac run --interp`. Integer and String \
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
                // `!range [0, 2^61)` — lets LLVM fold overflow checks on
                // len-derived arithmetic (`n + 1`, hot-loop index steps);
                // see `annotate_len_load_range` for the soundness argument
                // (B-2026-07-10-5).
                self.annotate_len_load_range(len, Some(elem_ty));
                // Head-index deque (B-2026-07-30-5): the `len` field is the END
                // index of the live range, so the user-visible count is
                // `len - head`. The range annotation above still holds — the
                // difference is non-negative and no larger than the field.
                if let Some(head_slot) = self.deque_head_slot(var_name) {
                    let head = self
                        .builder
                        .build_load(i64_t, head_slot, "deque.head")
                        .unwrap()
                        .into_int_value();
                    let count = self
                        .builder
                        .build_int_sub(len.into_int_value(), head, "deque.count")
                        .unwrap();
                    return Ok(count.into());
                }
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
            // prefix.data, prefix.len) == 0`. Uses the same `self.runtime_fns.memcmp_fn`
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
                        self.runtime_fns.memcmp_fn,
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
            "lines" | "split_whitespace" => {
                // `String.lines()` / `.split_whitespace() -> Vec[String]`. No
                // separator argument — the runtime helper splits via Rust's own
                // `str::lines` / `str::split_whitespace`, so the pieces are
                // byte-identical to the interpreter. Each piece is a fresh
                // malloc'd String the result Vec owns; the result is registered
                // as `Vec[String]` by the typechecker, so its element drop +
                // buffer free run at scope exit (same ownership path as `split`).
                let sym = if method == "lines" {
                    "karac_runtime_string_lines"
                } else {
                    "karac_runtime_string_split_whitespace"
                };
                let recv_data_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "swss.recv.ptr.p")
                    .unwrap();
                let recv_data = self
                    .builder
                    .build_load(ptr_ty, recv_data_ptr, "swss.recv.ptr")
                    .unwrap()
                    .into_pointer_value();
                let recv_len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "swss.recv.len.p")
                    .unwrap();
                let recv_len = self
                    .builder
                    .build_load(i64_t, recv_len_ptr, "swss.recv.len")
                    .unwrap()
                    .into_int_value();

                let split_fn = match self.module.get_function(sym) {
                    Some(f) => f,
                    None => {
                        let ft = self.context.void_type().fn_type(
                            &[
                                ptr_ty.into(), // s
                                i64_t.into(),  // s_len
                                ptr_ty.into(), // out_data
                                ptr_ty.into(), // out_len
                                ptr_ty.into(), // out_cap
                            ],
                            false,
                        );
                        self.module.add_function(sym, ft, Some(Linkage::External))
                    }
                };

                let fn_val = self.current_fn.unwrap();
                let result_slot = self.create_entry_alloca(fn_val, "swss.result", vec_ty.into());
                let out_data = self
                    .builder
                    .build_struct_gep(vec_ty, result_slot, 0, "swss.out.data")
                    .unwrap();
                let out_len = self
                    .builder
                    .build_struct_gep(vec_ty, result_slot, 1, "swss.out.len")
                    .unwrap();
                let out_cap = self
                    .builder
                    .build_struct_gep(vec_ty, result_slot, 2, "swss.out.cap")
                    .unwrap();
                self.builder
                    .build_call(
                        split_fn,
                        &[
                            recv_data.into(),
                            recv_len.into(),
                            out_data.into(),
                            out_len.into(),
                            out_cap.into(),
                        ],
                        "",
                    )
                    .unwrap();

                let result = self
                    .builder
                    .build_load(vec_ty, result_slot, "swss.load")
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
                        self.runtime_fns.memcmp_fn,
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

                // B-2026-08-14-19 — reject a cut that lands INSIDE a codepoint,
                // matching the interpreter, which now raises the same error.
                // Before this the two surfaces produced results of DIFFERENT
                // LENGTHS for the same slice (`"日本語".substring(0, 2)` measured
                // 3/3/1 under `--interp` against 2/2/2 compiled), so a loop that
                // sliced and measured terminated differently under `karac run`
                // than under `karac build` — and the compiled side put invalid
                // UTF-8 on stdout, which `String` is not allowed to hold.
                //
                // Ordered to match the interpreter exactly: an out-of-range
                // START keeps the established empty-String contract and is NOT
                // boundary-checked, so only a slice that would really have been
                // taken can fault. An EMPTY range still is
                // (`"日本語".substring(1, 1)` faults on both), because the index
                // is just as invalid whether or not any bytes come back.
                self.emit_substring_boundary_checks(oob, recv_data, recv_len, start, end);

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
                    .build_call(
                        self.runtime_fns.alloc_or_panic_fn,
                        &[new_len.into()],
                        "ss.buf",
                    )
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
            "trim" | "trim_start" | "trim_end" | "to_lowercase" | "to_uppercase" => {
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "sx");
                let func = match method {
                    "trim" => self.runtime_fns.karac_string_trim_fn,
                    "trim_start" => self.runtime_fns.karac_string_trim_start_fn,
                    "trim_end" => self.runtime_fns.karac_string_trim_end_fn,
                    "to_lowercase" => self.runtime_fns.karac_string_to_lowercase_fn,
                    "to_uppercase" => self.runtime_fns.karac_string_to_uppercase_fn,
                    _ => unreachable!(),
                };
                Ok(self.build_string_xform_result(
                    func,
                    vec![recv_data.into(), recv_len.into()],
                    "str.xform",
                ))
            }
            // `String.normalize(form) -> String` — design.md § Strings
            // (Equality)'s normalization-aware comparison (B-2026-08-20-41).
            // Same allocating-transform shape as the arm above, plus an i32
            // form selector, and resolved from the opt-in
            // `libkarac_runtime_unicode.a` rather than the ordinary archive.
            //
            // The form is COMPILED, not pattern-matched on a literal: it is an
            // ordinary C-like enum value, so `let f = Nfd; s.normalize(f)` and
            // `s.normalize(Nfd)` both lower to the same discriminant. Matching
            // a variant literal instead — the narrower `parse_memory_ordering`
            // posture — would have compiled the literal spelling and failed the
            // variable one, which the interpreter accepts: a run-vs-build
            // divergence manufactured at the call site. The discriminant IS the
            // ABI here; `runtime/stdlib/normalization_form.kara` carries the
            // note that its declaration order is load-bearing.
            "normalize" if self.var_types.string_vars.contains(var_name) => {
                // A `NormalizationForm` value is the seeded 1-word
                // `{ i64 tag }` enum struct (`declarations.rs`'s
                // `seed_unit_enum`), so the discriminant is field 0. A bare
                // integer is accepted too — some paths hand back the tag
                // already unwrapped.
                let form = self.compile_expr(&args[0].value)?;
                let form_iv = match form {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::StructValue(sv) => self
                        .builder
                        .build_extract_value(sv, 0, "nform.tag")
                        .ok()
                        .and_then(|v| match v {
                            BasicValueEnum::IntValue(iv) => Some(iv),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            "codegen: String.normalize form argument has no integer tag".to_string()
                        })?,
                    _ => {
                        return Err(
                            "codegen: String.normalize expects a NormalizationForm (Nfc / Nfd / \
                             Nfkc / Nfkd)"
                                .to_string(),
                        )
                    }
                };
                let i32_t = self.context.i32_type();
                let form_i32 = match form_iv.get_type().get_bit_width() {
                    32 => form_iv,
                    w if w < 32 => self
                        .builder
                        .build_int_z_extend(form_iv, i32_t, "nform.z")
                        .unwrap(),
                    _ => self
                        .builder
                        .build_int_truncate(form_iv, i32_t, "nform.t")
                        .unwrap(),
                };
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "snorm");
                let func = self.runtime_fns.karac_unicode_normalize_fn;
                Ok(self.build_string_xform_result(
                    func,
                    vec![recv_data.into(), recv_len.into(), form_i32.into()],
                    "str.normalize",
                ))
            }
            // `String.sorted() -> String` — chars sorted ascending, the anagram
            // key (LeetCode #49). Guarded on `string_vars` so a String receiver
            // routes to `karac_string_sorted` while a `Vec[T].sorted()` still
            // falls through to the Vec arms / catch-all (same pattern as the
            // `push` String-vs-Vec disambiguation above).
            "sorted" if self.var_types.string_vars.contains(var_name) => {
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "ss");
                let func = self.runtime_fns.karac_string_sorted_fn;
                Ok(self.build_string_xform_result(
                    func,
                    vec![recv_data.into(), recv_len.into()],
                    "str.sorted",
                ))
            }
            // `Vec[T].sorted()` — immutable sort returning a NEW Vec, leaving the
            // receiver unsorted (B-2026-07-19-15). Desugar to
            // `{ let mut __srt: Vec[E] = <recv>.clone(); __srt.sort(); __srt }`,
            // reusing the deep-clone (`emit_clone_fn_for_type_expr`, per-element
            // heap-safe) and the in-place `sort` arm (all its element-type
            // comparator thunks — int / String / float / tuple / nested-Vec —
            // apply to the clone identically). An element type `sort()` can't
            // order fails LOUD via `sort`'s own error, exactly as an in-place
            // `.sort()` would. The clone is what makes it immutable; `sort()`
            // mutates only the fresh binding.
            "sorted" => {
                if !args.is_empty() {
                    return Err(format!(
                        "Vec.sorted expects 0 arguments, got {}",
                        args.len()
                    ));
                }
                let elem_te = self
                    .var_types
                    .var_elem_type_exprs
                    .get(var_name)
                    .cloned()
                    .ok_or_else(|| {
                        "Vec.sorted() in codegen requires a known element type".to_string()
                    })?;
                let uid = self.indexed_elem_counter;
                self.indexed_elem_counter += 1;
                // Synthetic span with a unique `usize::MAX`-based offset so no
                // typechecker side-table (keyed on real spans) is ever hit by the
                // desugar's nodes — the `let` carries an explicit annotation and
                // `sort` reads `var_elem_type_exprs["__srt_N"]`, so no span lookup
                // is needed anyway.
                let sp = crate::token::Span {
                    line: 0,
                    column: 0,
                    offset: usize::MAX - (uid as usize) - 1,
                    length: 1,
                };
                let tmp = format!("__srt_{}", uid);
                let ident = |n: &str| Expr {
                    kind: ExprKind::Identifier(n.to_string()),
                    span: sp,
                };
                let vec_te = TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec!["Vec".to_string()],
                        generic_args: Some(vec![GenericArg::Type(elem_te)]),
                        span: sp,
                    }),
                    span: sp,
                };
                let clone_call = Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(ident(var_name)),
                        method: "clone".to_string(),
                        turbofish: None,
                        args: vec![],
                        args_close_span: sp,
                    },
                    span: sp,
                };
                let let_tmp = Stmt {
                    kind: StmtKind::Let {
                        is_mut: true,
                        pattern: Pattern {
                            kind: PatternKind::Binding(tmp.clone()),
                            span: sp,
                        },
                        ty: Some(vec_te),
                        value: clone_call,
                    },
                    span: sp,
                };
                let sort_call = Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::MethodCall {
                            object: Box::new(ident(&tmp)),
                            method: "sort".to_string(),
                            turbofish: None,
                            args: vec![],
                            args_close_span: sp,
                        },
                        span: sp,
                    }),
                    span: sp,
                };
                let block = Expr {
                    kind: ExprKind::Block(Block {
                        stmts: vec![let_tmp, sort_call],
                        final_expr: Some(Box::new(ident(&tmp))),
                        span: sp,
                    }),
                    span: sp,
                };
                self.compile_expr(&block)
            }
            // `String.sorted_by(cmp: Fn(Char, Char) -> Ordering)` — the
            // char-sequence twin of `Vec.sorted_by` below. The runtime's
            // `karac_string_sorted` helper has no comparator variant (a user
            // comparator over `Char`s would need a per-char callback ABI), so
            // this stays interp-only for now — bail LOUD with the standard
            // pointer instead of the generic dispatch tail (B-2026-07-20-8,
            // Vec leg landed; String leg deferred).
            "sorted_by" if self.var_types.string_vars.contains(var_name) => Err(
                "`String.sorted_by(cmp)` is not yet supported under `karac build` \
                 (codegen); use `.sorted()` for ascending order, or re-run with \
                 `--interp` (or `KARAC_RUN_JIT=0`)."
                    .to_string(),
            ),
            // `Vec[T].sorted_by(cmp: Fn(T, T) -> Ordering)` — immutable
            // comparator sort returning a NEW Vec, leaving the receiver
            // unsorted (B-2026-07-20-8). Desugar to `{ let mut __srtb: Vec[E] =
            // <recv>.clone(); __srtb.sort_by(<cmp>); __srtb }` — the comparator
            // sibling of the `sorted` arm above, forwarding the user closure
            // verbatim (its ORIGINAL span survives the desugar, so any
            // span-keyed typechecker side-tables for the closure still
            // resolve). Everything the in-place `sort_by` arm supports — the
            // capture-free inline-closure mono fast path AND the runtime
            // callback thunk — applies to the clone identically; an
            // unsupported comparator shape fails LOUD via `sort_by`'s own
            // error, exactly as the in-place form would.
            // B-2026-08-11-23 folds `sorted_by_key` in here. It was the ONLY
            // hole in the six-method sort family — `sort`, `sort_by`,
            // `sort_by_key`, `sorted` and `sorted_by` all compiled, and
            // `sorted_by_key` alone passed `karac check` and then died at
            // build with "not yet supported in codegen". That shape is worse
            // than a plain missing method for the Mend loop, where `check` is
            // the primary tool: a program that checks clean and fails at build
            // gives the repair loop no diagnostic to act on.
            //
            // The two are the same desugar with a different inner method, so
            // they share an arm rather than being copied — the immutable form
            // is exactly the in-place form applied to a clone, and whatever
            // the in-place lowering supports (for `sort_by_key`: the
            // precompute-keys path and the float-key `karac_float_cmp`
            // dispatch) carries over unchanged, including its error messages
            // for shapes it cannot handle.
            "sorted_by" | "sorted_by_key" => {
                let inner = if method == "sorted_by" {
                    "sort_by"
                } else {
                    "sort_by_key"
                };
                let what = if method == "sorted_by" {
                    "comparator closure"
                } else {
                    "key closure"
                };
                if args.len() != 1 {
                    return Err(format!(
                        "Vec.{method} expects 1 argument ({what}), got {}",
                        args.len()
                    ));
                }
                let elem_te = self
                    .var_types
                    .var_elem_type_exprs
                    .get(var_name)
                    .cloned()
                    .ok_or_else(|| {
                        format!("Vec.{method}() in codegen requires a known element type")
                    })?;
                let uid = self.indexed_elem_counter;
                self.indexed_elem_counter += 1;
                // Synthetic span (unique `usize::MAX`-based offset) for the
                // desugar's own nodes; see the `sorted` arm's rationale.
                let sp = crate::token::Span {
                    line: 0,
                    column: 0,
                    offset: usize::MAX - (uid as usize) - 1,
                    length: 1,
                };
                let tmp = format!("__srtb_{}_{}", inner, uid);
                let ident = |n: &str| Expr {
                    kind: ExprKind::Identifier(n.to_string()),
                    span: sp,
                };
                let vec_te = TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec!["Vec".to_string()],
                        generic_args: Some(vec![GenericArg::Type(elem_te)]),
                        span: sp,
                    }),
                    span: sp,
                };
                let clone_call = Expr {
                    kind: ExprKind::MethodCall {
                        object: Box::new(ident(var_name)),
                        method: "clone".to_string(),
                        turbofish: None,
                        args: vec![],
                        args_close_span: sp,
                    },
                    span: sp,
                };
                let let_tmp = Stmt {
                    kind: StmtKind::Let {
                        is_mut: true,
                        pattern: Pattern {
                            kind: PatternKind::Binding(tmp.clone()),
                            span: sp,
                        },
                        ty: Some(vec_te),
                        value: clone_call,
                    },
                    span: sp,
                };
                let sort_call = Stmt {
                    kind: StmtKind::Expr(Expr {
                        kind: ExprKind::MethodCall {
                            object: Box::new(ident(&tmp)),
                            method: inner.to_string(),
                            turbofish: None,
                            args: vec![args[0].clone()],
                            args_close_span: sp,
                        },
                        span: sp,
                    }),
                    span: sp,
                };
                let block = Expr {
                    kind: ExprKind::Block(Block {
                        stmts: vec![let_tmp, sort_call],
                        final_expr: Some(Box::new(ident(&tmp))),
                        span: sp,
                    }),
                    span: sp,
                };
                self.compile_expr(&block)
            }
            // `Vec[String].join(sep) -> String` / `.concat() -> String` via
            // `karac_string_join` (B-2026-07-16-14). The receiver's
            // `{ptr, len, cap}` triple reads as (elements-buffer, count); the
            // runtime walks the element triples READ-ONLY (ownership stays
            // with the vector — no element consumption to suppress) and
            // returns a fresh owned buffer wrapped by the shared xform-result
            // path. Gated off String receivers (`string_vars`) so only the
            // Vec[String] form lands here; the typechecker's element gate
            // keeps non-String element vectors out.
            "join" | "concat" if !self.var_types.string_vars.contains(var_name) => {
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "jn");
                let sep_data = if method == "join" {
                    if args.len() != 1 {
                        return Err("Vec.join requires a separator argument".to_string());
                    }
                    let sep_val = self.compile_expr(&args[0].value)?.into_struct_value();
                    let d = self
                        .builder
                        .build_extract_value(sep_val, 0, "jn.sep.ptr")
                        .unwrap()
                        .into_pointer_value();
                    let l = self
                        .builder
                        .build_extract_value(sep_val, 1, "jn.sep.len")
                        .unwrap()
                        .into_int_value();
                    // A fresh-owned separator temp (`v.join("-".to_string())`)
                    // has no other owner once the runtime copies its bytes —
                    // free it after the call like `replace` frees its args.
                    // Deferred until after build_string_xform_result via the
                    // captured value below.
                    Some((sep_val, d, l))
                } else {
                    None
                };
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let i64_t = self.context.i64_type();
                let (sd, sl, sep_free) = match &sep_data {
                    Some((sv, d, l)) => (
                        inkwell::values::BasicValueEnum::from(*d),
                        inkwell::values::BasicValueEnum::from(*l),
                        Some(*sv),
                    ),
                    None => (ptr_ty.const_null().into(), i64_t.const_zero().into(), None),
                };
                let join_fn = self
                    .module
                    .get_function("karac_string_join")
                    .ok_or_else(|| "karac_string_join not declared".to_string())?;
                let result = self.build_string_xform_result(
                    join_fn,
                    vec![recv_data.into(), recv_len.into(), sd.into(), sl.into()],
                    "str.join",
                );
                if let Some(sv) = sep_free {
                    self.free_fresh_owned_str_arg(&args[0].value, sv.into());
                }
                Ok(result)
            }
            // `String.replace(from, to) -> String` via `karac_string_replace`
            // (Rust `str::replace`). Receiver + both args are passed as
            // `(ptr, len)` pairs.
            "replace" => {
                if args.len() != 2 {
                    return Err("String.replace requires (from, to) arguments".to_string());
                }
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "rp");
                let from_val = self.compile_expr(&args[0].value)?.into_struct_value();
                let from_data = self
                    .builder
                    .build_extract_value(from_val, 0, "rp.from.ptr")
                    .unwrap()
                    .into_pointer_value();
                let from_len = self
                    .builder
                    .build_extract_value(from_val, 1, "rp.from.len")
                    .unwrap()
                    .into_int_value();
                let to_val = self.compile_expr(&args[1].value)?.into_struct_value();
                let to_data = self
                    .builder
                    .build_extract_value(to_val, 0, "rp.to.ptr")
                    .unwrap()
                    .into_pointer_value();
                let to_len = self
                    .builder
                    .build_extract_value(to_val, 1, "rp.to.len")
                    .unwrap()
                    .into_int_value();
                let result = self.build_string_xform_result(
                    self.runtime_fns.karac_string_replace_fn,
                    vec![
                        recv_data.into(),
                        recv_len.into(),
                        from_data.into(),
                        from_len.into(),
                        to_data.into(),
                        to_len.into(),
                    ],
                    "str.replace",
                );
                // Free each fresh-owned String argument after the runtime call
                // reads it — `replace` copies the matched/replacement bytes into
                // its own result buffer, so a fresh-temp arg
                // (`s.replace("-".to_string(), "_".to_string())`) has no other
                // owner and otherwise leaks once per call (unbounded in a loop).
                // Mirrors the `contains` / `starts_with` / `split` arg cleanup;
                // `free_fresh_owned_str_arg` self-gates on a fresh-temp expr and
                // the cap>0 marker, so a borrowed / literal arg is never freed.
                self.free_fresh_owned_str_arg(&args[0].value, from_val.into());
                self.free_fresh_owned_str_arg(&args[1].value, to_val.into());
                Ok(result)
            }
            // `String.replacen(from, to, n) -> String` via
            // `karac_string_replacen` (Rust `str::replacen`). Same shape as
            // `replace` plus a trailing `i64` count; the runtime clamps a
            // negative count to 0.
            "replacen" => {
                if args.len() != 3 {
                    return Err("String.replacen requires (from, to, n) arguments".to_string());
                }
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "rpn");
                let from_val = self.compile_expr(&args[0].value)?.into_struct_value();
                let from_data = self
                    .builder
                    .build_extract_value(from_val, 0, "rpn.from.ptr")
                    .unwrap()
                    .into_pointer_value();
                let from_len = self
                    .builder
                    .build_extract_value(from_val, 1, "rpn.from.len")
                    .unwrap()
                    .into_int_value();
                let to_val = self.compile_expr(&args[1].value)?.into_struct_value();
                let to_data = self
                    .builder
                    .build_extract_value(to_val, 0, "rpn.to.ptr")
                    .unwrap()
                    .into_pointer_value();
                let to_len = self
                    .builder
                    .build_extract_value(to_val, 1, "rpn.to.len")
                    .unwrap()
                    .into_int_value();
                let n_val = self.compile_expr(&args[2].value)?.into_int_value();
                let result = self.build_string_xform_result(
                    self.runtime_fns.karac_string_replacen_fn,
                    vec![
                        recv_data.into(),
                        recv_len.into(),
                        from_data.into(),
                        from_len.into(),
                        to_data.into(),
                        to_len.into(),
                        n_val.into(),
                    ],
                    "str.replacen",
                );
                self.free_fresh_owned_str_arg(&args[0].value, from_val.into());
                self.free_fresh_owned_str_arg(&args[1].value, to_val.into());
                Ok(result)
            }
            // `String.strip_{prefix,suffix}(p) -> Option[String]` via
            // `karac_string_strip_{prefix,suffix}`, which allocates the owned
            // remainder copy and writes a `matched` flag through an out-slot.
            // The flag (not the null/0-len result) distinguishes a matched empty
            // remainder — `Some("")` = `{null,0,0}` — from a `None`. Wrapped into
            // `Option[String]` via `find`'s phi-merge shape, but with a 3-word
            // String payload (`ptr,len,cap`) instead of `find`'s single i64.
            "strip_prefix" | "strip_suffix" => {
                if args.len() != 1 {
                    return Err(
                        "String.strip_prefix/strip_suffix requires one argument".to_string()
                    );
                }
                let i32_t = self.context.i32_type();
                let (recv_data, recv_len) = self.load_string_data_len(vec_ty, data_ptr, "strip");
                let arg_val = self.compile_expr(&args[0].value)?;
                let arg_sv = arg_val.into_struct_value();
                let pfx_data = self
                    .builder
                    .build_extract_value(arg_sv, 0, "strip.p.ptr")
                    .unwrap()
                    .into_pointer_value();
                let pfx_len = self
                    .builder
                    .build_extract_value(arg_sv, 1, "strip.p.len")
                    .unwrap()
                    .into_int_value();
                let fn_val = self.current_fn.unwrap();
                let out_len_slot = self.create_entry_alloca(fn_val, "strip.outlen", i64_t.into());
                let out_matched_slot =
                    self.create_entry_alloca(fn_val, "strip.matched", i32_t.into());
                let func_name = if method == "strip_suffix" {
                    "karac_string_strip_suffix"
                } else {
                    "karac_string_strip_prefix"
                };
                let func = self
                    .module
                    .get_function(func_name)
                    .expect("strip extern declared in Codegen::new");
                let ret_ptr = self
                    .builder
                    .build_call(
                        func,
                        &[
                            recv_data.into(),
                            recv_len.into(),
                            pfx_data.into(),
                            pfx_len.into(),
                            out_len_slot.into(),
                            out_matched_slot.into(),
                        ],
                        "strip.call",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let out_len = self
                    .builder
                    .build_load(i64_t, out_len_slot, "strip.len")
                    .unwrap()
                    .into_int_value();
                let matched = self
                    .builder
                    .build_load(i32_t, out_matched_slot, "strip.m")
                    .unwrap()
                    .into_int_value();
                // Free a fresh-owned String arg temp (`s.strip_prefix(make())`);
                // a borrowed var / static literal arg is left untouched.
                self.free_fresh_owned_str_arg(&args[0].value, arg_val);

                let some_bb = self.context.append_basic_block(fn_val, "strip.some");
                let none_bb = self.context.append_basic_block(fn_val, "strip.none");
                let merge_bb = self.context.append_basic_block(fn_val, "strip.merge");
                let is_matched = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::NE,
                        matched,
                        i32_t.const_zero(),
                        "strip.is_m",
                    )
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_matched, some_bb, none_bb)
                    .unwrap();

                // some: String payload words {ptr (as i64), len, cap = len}.
                self.builder.position_at_end(some_bb);
                let ptr_word = self
                    .builder
                    .build_ptr_to_int(ret_ptr, i64_t, "strip.ptrw")
                    .unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // none.
                self.builder.position_at_end(none_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // merge: Some({ptr, len, cap}) from `some_bb`, None from `none_bb`.
                self.builder.position_at_end(merge_bb);
                let agg = self.build_option_some_via_phis(
                    &[ptr_word, out_len, out_len],
                    some_bb,
                    none_bb,
                    "strip.opt",
                );
                Ok(agg)
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
                // total = count * len — user-controlled count, so the
                // multiply is overflow-checked (`capacity overflow` on
                // wrap); see `checked_alloc_bytes`.
                let total = self.checked_alloc_bytes(count, recv_len, "rep.total")?;
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
                    .build_call(
                        self.runtime_fns.alloc_or_panic_fn,
                        &[total.into()],
                        "rep.buf",
                    )
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
            "push" if self.var_types.string_vars.contains(var_name) => {
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
                //
                // B-2026-08-01-33 stage 3c — a FROZEN-ELEMENT container takes
                // no count for what it stores, so the shared-struct transfer
                // inc this call normally emits must not fire. That inc is a
                // plain non-atomic load/add/store against a SHARED refcount
                // header; a `par` branch running this push would race every
                // other branch on it, which is exactly the hazard `frozen`
                // removes (B-2026-07-28-13's SIGSEGV came from that write).
                //
                // The `_ex(.., false)` form still performs every OTHER
                // suppression — moved-out caps, container bodies, map handles —
                // so this narrows the change to the one inc rather than
                // skipping the call. Paired with the scope-exit element-drain
                // skip in `stmts.rs`; neither is correct alone.
                let frozen_elem_target = self.frozen_elem_vec_owners.contains(var_name);
                // B-2026-08-08-5 — a `weak T` element takes NO strong count.
                // Computed here rather than at the store because the transfer
                // inc below happens first, and leaving it in place is a leak
                // the store cannot undo: the container releases through
                // `karac_weak_drop`, so a strong retain here is never balanced
                // and the target's strong count never reaches zero. Measured
                // exactly that way — the payloads freed and the control blocks
                // did not.
                let weak_elem = self
                    .var_types
                    .var_elem_type_exprs
                    .get(var_name)
                    .is_some_and(|te| matches!(te.kind, TypeKind::Weak(_)));
                self.suppress_source_vec_cleanup_for_arg_ex(
                    &args[0].value,
                    !frozen_elem_target && !weak_elem,
                );
                // Container-bodies twin of the cap-zero above: a bare-
                // identifier arg moves its container value in, so retract
                // its `__karac_dropelems_*` action or the body fires over
                // the zeroed moved-from slot.
                self.disarm_container_bodies_for_arg(&args[0].value);
                // And the binding's OWN body: the moved value belongs to the
                // container's element walk now (see
                // `disarm_moved_value_arg_user_drops`).
                self.disarm_moved_value_arg_user_drops(&args[0].value);
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
                // Option-binding source moved into a `Vec[Option[String]]`
                // (slice 3p): the per-element `karac_drop_Option_<payload>`
                // now frees the payload inside the container, so the source
                // binding's `FreeInlineOptionPayload` must be disarmed
                // (cap-zeroed) or both free the same payload buffer (SIGTRAP).
                // The Option sibling of the cap-zeroing / map-suppression
                // above. No-op for non-Option / non-identifier args. The
                // Result sibling (slice 3q) disarms `FreeInlineResultPayload`
                // the same way.
                self.suppress_inline_option_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_inline_result_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_boxed_enum_payload_cleanup_for_moved_arg(&args[0].value);

                // Vec-store slice (B-2026-06-22-2): pushing a heap-env closure
                // BINDING (`v.push(f)`) into a heap-env Vec owner co-owns the env
                // box — the source binding's scope-exit `FreeClosureEnv` AND the
                // Vec's per-element drop loop both decrement it, so bump the
                // refcount here. A fresh `v.push(make(k))` element is already rc 1
                // (no inc). Mirrors the array/tuple binding-source store inc.
                if self
                    .closure_state
                    .escape
                    .heap_env_vec_owners
                    .contains(var_name)
                {
                    if let ExprKind::Identifier(src) = &args[0].value.kind {
                        if self.closure_state.heap_env_closure_vars.contains(src) {
                            self.emit_heap_closure_env_inc(elem_val);
                        }
                    }
                }

                // A tracked `Option[shared T]` BINDING moved into a
                // `Vec[Option[shared T]]` (`out.push(orig)` / `out.push(s)`):
                // the container CO-OWNS the node — its per-element `RcDecOption`
                // drop AND the source binding's own scope-exit `RcDecOption`
                // both dec it. Under reference semantics the source stays a live
                // alias (usable after the push), so this is co-ownership, not a
                // move: inc the inner here so the container holds an independent
                // +1. The RC sibling of the closure-env co-ownership inc just
                // above, and the same `share_option_shared_ref_for_arg` retain
                // that consuming CALL sites (`clone_offset(s, ..)`) already emit
                // — push is a builtin so it never reaches the generic method-arg
                // path that would apply it. A fresh `push(Some(..))` /
                // `push(clone(..))` element is a non-Identifier arg (already owns
                // its +1) → skipped, so no double count. Without this, pushing
                // the same `Option[shared]` binding left the node UNDER-counted:
                // the first of the two drops freed it while the other owner still
                // pointed at it — a use-after-free (B-2026-07-11-29 `let s = v[i];
                // out.push(s)` and `let orig = Some(..); out.push(orig)`).
                self.share_option_shared_ref_for_arg(&args[0].value);
                // Field-read sibling of the Identifier retain above:
                // `stack.push(n.left)` — a FieldAccess reading an
                // `Option[shared T]` field of another shared node — is ALSO
                // aliasing co-ownership (the pushed handle stays live at its
                // source `n`), so the container needs an independent +1. The
                // generic method-arg loop applies this via
                // `share_option_shared_field_ref_for_arg`, but push is a builtin
                // that bypasses it and only the Identifier leg was mirrored here
                // — so a field-read push left the node UNDER-counted: the Vec's
                // per-element drop AND the source node's own drop both released
                // it, freeing it once and reading the freed block on the second
                // release (use-after-free, B-2026-07-12-4; a leak before the Vec
                // per-element drop began releasing residuals). A fresh
                // `push(Some(..))` is a non-place arg → does not fire → still
                // owns its sole +1, unchanged. NOTE: the direct index sibling
                // (`out.push(v[i])`) is deliberately NOT retained here — that
                // read already yields an independent element (deep-cloned), so an
                // extra inc would leak; only the aliasing field read needs it.
                //
                // This retain is only half the fix (B-2026-07-12-4): it co-owns
                // the node at rc 2, so the residual Vec drop is balanced, but the
                // DRAIN path (`match vec.pop() { Some(opt) => match opt { .. } }`)
                // needs the popped node dec'd exactly once too — that peer dec is
                // the boxed-scrutinee / let-binding inner drop registered via
                // `option_shared_payload_element_drop`. Retain-alone leaks the
                // drain path; the inner-drop-alone double-frees the residual path.
                self.share_option_shared_field_ref_for_arg(&args[0].value, elem_val);
                // Bare `shared struct` sibling (B-2026-07-21-13): pushing an
                // ALIASING indexed element / field of a bare shared struct
                // (`node.neighbors.push(nodes[j])`) co-owns the node, so retain
                // it — a reference-semantic read yields the box pointer with no
                // inc, and the source container's drop would otherwise free the
                // still-referenced node (use-after-free). Fresh constructions /
                // call move-outs are not place exprs and are skipped.
                // Stage 3c: the aliasing-read sibling of the transfer inc
                // suppressed above (`work.push(g.kids[j])` rather than
                // `work.push(g)`). Both retain channels into a frozen-element
                // container have to close, or the container ends up counting
                // some elements and not others and its all-or-nothing drop
                // cannot be right for both.
                if !frozen_elem_target && !weak_elem {
                    self.share_shared_struct_ref_for_arg(&args[0].value, elem_val);
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
                // B-2026-08-08-5 — a `weak T` ELEMENT slot. The typechecker
                // now accepts a strong handle here (the same downgrade
                // coercion a `weak` FIELD store has always had), so the store
                // must perform the downgrade or the container would hold a
                // STRONG pointer in a slot the author declared `weak` — the
                // cycle would still leak while the source says it does not,
                // which is strictly worse than the error this replaced.
                //
                // `emit_weak_field_init` is the right sibling rather than
                // `emit_weak_field_store`: a push writes a FRESH slot at
                // `data[len]`, so there is no prior occupant to weak-drop. The
                // matching decrement is the per-element
                // `__karac_vec_elem_weak_drop` the container's scope-exit
                // drain now runs (`vec_elem_agg_drop_for_type_expr`); the two
                // are a pair on the same terms as every other container
                // ownership rule here.
                if weak_elem {
                    if let BasicValueEnum::PointerValue(p) = elem_val {
                        self.emit_weak_field_init(elem_ptr, p);
                        let one_w = i64_t.const_int(1, false);
                        let new_len = self.builder.build_int_add(len, one_w, "new_len").unwrap();
                        self.builder.build_store(len_ptr, new_len).unwrap();
                        return Ok(self.context.i64_type().const_zero().into());
                    }
                }
                let elem_val = self.coerce_scalar_to_type_from(elem_val, elem_ty, &args[0].value);
                self.builder.build_store(elem_ptr, elem_val).unwrap();
                // A for-loop struct-element binding aliases the SOURCE
                // container's slot — deep-copy the stored fields so the two
                // containers own independent heap (B-2026-08-01-24).
                self.deep_copy_pushed_for_loop_agg_element(&args[0].value, elem_ptr);

                // Increment len.
                let one = i64_t.const_int(1, false);
                let new_len = self.builder.build_int_add(len, one, "new_len").unwrap();
                self.builder.build_store(len_ptr, new_len).unwrap();

                Ok(self.context.i64_type().const_int(0, false).into())
            }
            // `Vec[T].insert(idx, value) -> ()` — grow if full, memmove the
            // `[idx..len]` tail RIGHT by one, store `value` at `idx`, `len++`.
            // The value MOVES into the container exactly like `push`, so it
            // carries the identical ownership-suppression set (a heap
            // String/Vec/Map/Option source has its scope-exit cleanup disarmed
            // — else the source and the container's element-drop both free the
            // same buffer). `idx == len` appends (the memmove count is 0). The
            // interpreter twin is in method_call_seq.rs; the typechecker signs
            // it `(i64, T) -> ()` in expr_method_call.rs.
            "insert" => {
                if args.len() != 2 {
                    return Err("Vec.insert requires (index, value) arguments".to_string());
                }
                let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                let elem_val = self.compile_expr(&args[1].value)?;
                // Move-in ownership suppressions — identical to the `push` arm
                // (the value at arg 1 is the one that moves into the buffer).
                self.suppress_fstr_acc_if_moved_out(&args[1].value);
                let elem_val = self.maybe_defensive_copy_param_arg(&args[1].value, elem_val);
                self.suppress_source_vec_cleanup_for_arg(&args[1].value);
                // Container-bodies twin of the cap-zero above: a bare-
                // identifier arg moves its container value in, so retract
                // its `__karac_dropelems_*` action or the body fires over
                // the zeroed moved-from slot.
                self.disarm_container_bodies_for_arg(&args[1].value);
                // And the binding's OWN body: the moved value belongs to the
                // container's element walk now (see
                // `disarm_moved_value_arg_user_drops`).
                self.disarm_moved_value_arg_user_drops(&args[1].value);
                if let ExprKind::Identifier(n) = &args[1].value.kind {
                    let n = n.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&n);
                }
                self.suppress_inline_option_payload_cleanup_for_moved_arg(&args[1].value);
                self.suppress_inline_result_payload_cleanup_for_moved_arg(&args[1].value);
                self.suppress_boxed_enum_payload_cleanup_for_moved_arg(&args[1].value);

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "insert.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "insert.len.ptr")
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "insert.cap.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "insert.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "insert.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "insert.cap")
                    .unwrap()
                    .into_int_value();

                // Growth check (`len == cap`) → realloc to max(4, cap*2), same
                // amortized-doubling strategy as `push`.
                let fn_val = self.current_fn.unwrap();
                let grow_bb = self.context.append_basic_block(fn_val, "insert.grow");
                let shift_bb = self.context.append_basic_block(fn_val, "insert.shift");
                let needs_grow = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "insert.needs_grow")
                    .unwrap();
                self.builder
                    .build_conditional_branch(needs_grow, grow_bb, shift_bb)
                    .unwrap();

                self.builder.position_at_end(grow_bb);
                let two = i64_t.const_int(2, false);
                let four = i64_t.const_int(4, false);
                let doubled = self
                    .builder
                    .build_int_mul(cap, two, "insert.doubled")
                    .unwrap();
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::UGT, doubled, four, "insert.cmp")
                    .unwrap();
                let new_cap = self
                    .builder
                    .build_select(cmp, doubled, four, "insert.new_cap")
                    .unwrap()
                    .into_int_value();
                let elem_size = elem_ty.size_of().unwrap();
                let alloc_bytes = self
                    .builder
                    .build_int_mul(new_cap, elem_size, "insert.alloc_bytes")
                    .unwrap();
                let realloc_fn = self.realloc_or_panic_fn_decl();
                let new_data = self
                    .builder
                    .build_call(
                        realloc_fn,
                        &[data.into(), alloc_bytes.into()],
                        "insert.new_data",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.builder.build_store(data_ptr_ptr, new_data).unwrap();
                self.builder.build_store(cap_ptr, new_cap).unwrap();
                self.builder.build_unconditional_branch(shift_bb).unwrap();

                // Shift `[idx..len]` right by one (overlapping → memmove), then
                // store `value` at `idx` and bump `len`.
                self.builder.position_at_end(shift_bb);
                let cur_data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "insert.cur_data")
                    .unwrap()
                    .into_pointer_value();
                let one = i64_t.const_int(1, false);
                let dst_idx = self
                    .builder
                    .build_int_add(idx_val, one, "insert.dst_idx")
                    .unwrap();
                let dst = unsafe {
                    self.builder
                        .build_gep(elem_ty, cur_data, &[dst_idx], "insert.shift.dst")
                        .unwrap()
                };
                let src = unsafe {
                    self.builder
                        .build_gep(elem_ty, cur_data, &[idx_val], "insert.shift.src")
                        .unwrap()
                };
                let move_count = self
                    .builder
                    .build_int_sub(len, idx_val, "insert.move_count")
                    .unwrap();
                let move_bytes = self
                    .builder
                    .build_int_mul(move_count, elem_ty.size_of().unwrap(), "insert.move_bytes")
                    .unwrap();
                self.builder
                    .build_memmove(dst, 8, src, 8, move_bytes)
                    .unwrap();

                let slot = unsafe {
                    self.builder
                        .build_gep(elem_ty, cur_data, &[idx_val], "insert.slot")
                        .unwrap()
                };
                let elem_val = self.coerce_scalar_to_type_from(elem_val, elem_ty, &args[1].value);
                self.builder.build_store(slot, elem_val).unwrap();
                // For-loop struct-element source: copy-depth == drop-depth
                // (B-2026-08-01-24, same as the push arm).
                self.deep_copy_pushed_for_loop_agg_element(&args[1].value, slot);
                let new_len = self
                    .builder
                    .build_int_add(len, one, "insert.new_len")
                    .unwrap();
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
                // Container-bodies twin of the cap-zero above: a bare-
                // identifier arg moves its container value in, so retract
                // its `__karac_dropelems_*` action or the body fires over
                // the zeroed moved-from slot.
                self.disarm_container_bodies_for_arg(&args[0].value);
                // And the binding's OWN body: the moved value belongs to the
                // container's element walk now (see
                // `disarm_moved_value_arg_user_drops`).
                self.disarm_moved_value_arg_user_drops(&args[0].value);
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
                // Option-binding source moved into a `Vec[Option[String]]`
                // (slice 3p): the per-element `karac_drop_Option_<payload>`
                // now frees the payload inside the container, so the source
                // binding's `FreeInlineOptionPayload` must be disarmed
                // (cap-zeroed) or both free the same payload buffer (SIGTRAP).
                // The Option sibling of the cap-zeroing / map-suppression
                // above. No-op for non-Option / non-identifier args. The
                // Result sibling (slice 3q) disarms `FreeInlineResultPayload`
                // the same way.
                self.suppress_inline_option_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_inline_result_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_boxed_enum_payload_cleanup_for_moved_arg(&args[0].value);

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
                        self.runtime_fns.alloc_fallible_fn,
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
                    .build_call(self.runtime_fns.free_fn, &[data.into()], "")
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
                let elem_val = self.coerce_scalar_to_type_from(elem_val, elem_ty, &args[0].value);
                self.builder.build_store(elem_ptr, elem_val).unwrap();
                // For-loop struct-element source: copy-depth == drop-depth
                // (B-2026-08-01-24, same as the push arm).
                self.deep_copy_pushed_for_loop_agg_element(&args[0].value, elem_ptr);
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
                // Container-bodies twin of the cap-zero above: a bare-
                // identifier arg moves its container value in, so retract
                // its `__karac_dropelems_*` action or the body fires over
                // the zeroed moved-from slot.
                self.disarm_container_bodies_for_arg(&args[0].value);
                // And the binding's OWN body: the moved value belongs to the
                // container's element walk now (see
                // `disarm_moved_value_arg_user_drops`).
                self.disarm_moved_value_arg_user_drops(&args[0].value);
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
                // Option-binding source moved into a `Vec[Option[String]]`
                // (slice 3p): the per-element `karac_drop_Option_<payload>`
                // now frees the payload inside the container, so the source
                // binding's `FreeInlineOptionPayload` must be disarmed
                // (cap-zeroed) or both free the same payload buffer (SIGTRAP).
                // The Option sibling of the cap-zeroing / map-suppression
                // above. No-op for non-Option / non-identifier args. The
                // Result sibling (slice 3q) disarms `FreeInlineResultPayload`
                // the same way.
                self.suppress_inline_option_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_inline_result_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_boxed_enum_payload_cleanup_for_moved_arg(&args[0].value);

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
                    .build_call(
                        self.runtime_fns.alloc_or_panic_fn,
                        &[alloc_bytes.into()],
                        "new_data",
                    )
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
                    .build_call(self.runtime_fns.free_fn, &[data.into()], "")
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
                let elem_val = self.coerce_scalar_to_type_from(elem_val, elem_ty, &args[0].value);
                self.builder.build_store(cur_data, elem_val).unwrap();
                // For-loop struct-element source: copy-depth == drop-depth
                // (B-2026-08-01-24, same as the push arm).
                self.deep_copy_pushed_for_loop_agg_element(&args[0].value, cur_data);
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
                // Container-bodies twin of the cap-zero above: a bare-
                // identifier arg moves its container value in, so retract
                // its `__karac_dropelems_*` action or the body fires over
                // the zeroed moved-from slot.
                self.disarm_container_bodies_for_arg(&args[0].value);
                // And the binding's OWN body: the moved value belongs to the
                // container's element walk now (see
                // `disarm_moved_value_arg_user_drops`).
                self.disarm_moved_value_arg_user_drops(&args[0].value);
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
                // Option-binding source moved into a `Vec[Option[String]]`
                // (slice 3p): the per-element `karac_drop_Option_<payload>`
                // now frees the payload inside the container, so the source
                // binding's `FreeInlineOptionPayload` must be disarmed
                // (cap-zeroed) or both free the same payload buffer (SIGTRAP).
                // The Option sibling of the cap-zeroing / map-suppression
                // above. No-op for non-Option / non-identifier args. The
                // Result sibling (slice 3q) disarms `FreeInlineResultPayload`
                // the same way.
                self.suppress_inline_option_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_inline_result_payload_cleanup_for_moved_arg(&args[0].value);
                self.suppress_boxed_enum_payload_cleanup_for_moved_arg(&args[0].value);

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
                        self.runtime_fns.alloc_fallible_fn,
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
                    .build_call(self.runtime_fns.free_fn, &[data.into()], "")
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
                let elem_val = self.coerce_scalar_to_type_from(elem_val, elem_ty, &args[0].value);
                self.builder.build_store(cur_data, elem_val).unwrap();
                // For-loop struct-element source: copy-depth == drop-depth
                // (B-2026-08-01-24, same as the push arm).
                self.deep_copy_pushed_for_loop_agg_element(&args[0].value, cur_data);
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
            "swap_remove" => {
                // `Vec[T].swap_remove(i) -> T` — O(1) remove: return element `i`
                // (moved out to the caller) and move the LAST element into slot
                // `i` (order NOT preserved), then `len--`. No memmove, no clone:
                // element `i`'s buffer transfers to the return value and the
                // last element's buffer transfers into slot `i`, so the vacated
                // tail slot is abandoned by the `len--` with nothing to free —
                // pure moves, no double-free. Mirrors `remove`'s no-codegen-
                // bounds-check precedent (the interpreter checks + panics).
                if args.is_empty() {
                    return Err("Vec.swap_remove requires an index argument".to_string());
                }
                let idx_val = self.compile_expr(&args[0].value)?.into_int_value();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "swrm.len.ptr")
                    .unwrap();
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "swrm.data.ptr")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "swrm.len")
                    .unwrap()
                    .into_int_value();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "swrm.data")
                    .unwrap()
                    .into_pointer_value();
                let one = i64_t.const_int(1, false);

                // Element being removed (the return value, moved out).
                let elem_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[idx_val], "swrm.elem.ptr")
                        .unwrap()
                };
                let elem_val = self
                    .builder
                    .build_load(elem_ty, elem_ptr, "swrm.elem")
                    .unwrap();

                // Move the last element into slot `idx` (self-store when
                // idx == last — harmless). `new_len = len - 1`.
                let new_len = self
                    .builder
                    .build_int_sub(len, one, "swrm.new_len")
                    .unwrap();
                let last_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[new_len], "swrm.last.ptr")
                        .unwrap()
                };
                let last_val = self
                    .builder
                    .build_load(elem_ty, last_ptr, "swrm.last")
                    .unwrap();
                self.builder.build_store(elem_ptr, last_val).unwrap();

                // Decrement len (the vacated tail slot's value was moved out).
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
                // For a head-index deque `len` is the END index, so emptiness
                // is `len == head`, not `len == 0`.
                let head_slot = if is_front {
                    self.deque_head_slot(var_name)
                } else {
                    None
                };
                let head_val = head_slot.map(|hs| {
                    self.builder
                        .build_load(i64_t, hs, "pop.head")
                        .unwrap()
                        .into_int_value()
                });
                let is_empty = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        len,
                        head_val.unwrap_or(zero),
                        "pop.is_empty",
                    )
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
                    head_val.unwrap_or(zero)
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
                if let (Some(head_slot), Some(head)) = (head_slot, head_val) {
                    // Head-index deque (B-2026-07-30-5). `len` is the END
                    // index here, so popping the front just advances `head`:
                    // no memmove, and `len` is left alone. That is the whole
                    // fix — this pop is O(1) where the memmove was O(n).
                    let new_head = self
                        .builder
                        .build_int_add(head, one, "pop.new_head")
                        .unwrap();

                    // Amortized compaction. Once the dead prefix is at least
                    // half the occupied span (and non-trivial), slide the live
                    // range back to 0. Moving `live <= new_head` elements after
                    // `new_head` pops keeps this O(1) amortized, and it is what
                    // bounds the buffer by the LIVE depth instead of by total
                    // enqueues — without it a long-running queue would grow
                    // without bound, trading a quadratic for a leak.
                    let compact_bb = self.context.append_basic_block(fn_val, "pop_front.compact");
                    let keep_bb = self.context.append_basic_block(fn_val, "pop_front.keep");
                    let done_bb = self.context.append_basic_block(fn_val, "pop_front.done");

                    let min_dead = i64_t.const_int(16, false);
                    let big_enough = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::UGE,
                            new_head,
                            min_dead,
                            "pop.compact.big",
                        )
                        .unwrap();
                    let doubled = self
                        .builder
                        .build_int_mul(new_head, i64_t.const_int(2, false), "pop.compact.2h")
                        .unwrap();
                    let mostly_dead = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::UGE,
                            doubled,
                            len,
                            "pop.compact.half",
                        )
                        .unwrap();
                    let should_compact = self
                        .builder
                        .build_and(big_enough, mostly_dead, "pop.compact.cond")
                        .unwrap();
                    self.builder
                        .build_conditional_branch(should_compact, compact_bb, keep_bb)
                        .unwrap();

                    self.builder.position_at_end(compact_bb);
                    let live = self
                        .builder
                        .build_int_sub(len, new_head, "pop.compact.live")
                        .unwrap();
                    let elem_size = elem_ty.size_of().unwrap();
                    let live_bytes = self
                        .builder
                        .build_int_mul(live, elem_size, "pop.compact.bytes")
                        .unwrap();
                    let src = unsafe {
                        self.builder
                            .build_gep(elem_ty, data, &[new_head], "pop.compact.src")
                            .unwrap()
                    };
                    self.builder
                        .build_memmove(data, 8, src, 8, live_bytes)
                        .unwrap();
                    self.builder.build_store(len_ptr, live).unwrap();
                    self.builder.build_store(head_slot, zero).unwrap();
                    self.builder.build_unconditional_branch(done_bb).unwrap();

                    self.builder.position_at_end(keep_bb);
                    self.builder.build_store(head_slot, new_head).unwrap();
                    self.builder.build_unconditional_branch(done_bb).unwrap();

                    self.builder.position_at_end(done_bb);
                } else {
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
                }
                let some_payload_words = self.coerce_to_payload_words(elem_val, 3)?;
                let some_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge: build Option struct via phi on tag + each
                // payload word. PHI nodes MUST be grouped at the top
                // of the basic block (LLVM rule), so create all phis
                // first, then build_insert_value into the aggregate.
                self.builder.position_at_end(merge_bb);
                let option_ty = self.type_decls.enum_layouts["Option"].llvm_type;
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

                // B-2026-08-15-2 — SELF-APPEND. Measure the source's offset into
                // the destination's buffer BEFORE the grow, so the copy below
                // can rebuild the pointer from wherever the buffer ends up.
                //
                // `s.push_str(s)` is the shape: the argument compiles to a
                // `{ptr,len,cap}` header aliasing the destination's own buffer,
                // `emit_string_buffer_grow` reallocs it (which may MOVE it, and
                // frees the old block when it does), and the copy then read
                // through the stale `src_ptr` — a heap-use-after-free of the
                // whole string on every call. It printed the correct answer
                // anyway, because the freed bytes are usually still mapped, so
                // nothing surfaced without a sanitizer.
                //
                // Rebasing rather than rejecting: the source lies inside
                // `[data, data + cap)` and the grow preserves those bytes at the
                // same offsets, so `new_data + offset` names the same content.
                // That makes the aliasing CORRECT instead of merely detected,
                // which is what lets the panic below be deleted — and it is what
                // an `s += s` lowered onto this in-place path (B-2026-08-14-22 /
                // -23) needs in order to be safe at all.
                //
                // `cap` is the OLD capacity, loaded above: a `cap == 0` receiver
                // (static literal / empty) has an empty range, so `aliases` is
                // false and the source — which the grow's malloc-and-copy path
                // leaves untouched — is used as-is.
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
                let src_offset = self
                    .builder
                    .build_int_sub(src_int, data_int, "pstr.src.off")
                    .unwrap();
                let src_aliases = {
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
                    self.builder.build_and(ge, lt, "pstr.alias").unwrap()
                };

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
                // B-2026-08-15-2 — the borrowed-source aliasing PANIC that stood
                // here is gone, replaced by the rebase above/below. It fired for
                // `out.push_str(src[a..b])` when `src` was the destination, on
                // the reasoning that the grow's `free(data)` dangles the source;
                // rebasing removes the dangle instead of reporting it, so the
                // shape is now simply correct. It was also only half a guard: it
                // was emitted `if src_borrowed` only, because "an owned-temp
                // source is a fresh copy that can't alias" — true of a temporary
                // and false of the destination VARIABLE itself, which is exactly
                // how `s.push_str(s)` walked past it into a use-after-free.
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
                // B-2026-08-15-2 — rebuild an ALIASING source from the buffer's
                // current address. `cur_data` is reloaded from the slot, so it
                // is the post-grow pointer on the grow edge and unchanged on the
                // no-grow edge — which makes the select the identity there
                // (`cur_data + offset == data + offset == src_ptr`) and correct
                // on the other, with no phi needed.
                //
                // The two ranges cannot overlap, so a plain `memcpy` stays
                // valid: the source sits within `[0, len)` of the buffer and the
                // destination starts AT `len`.
                let eff_src = {
                    let rebased = unsafe {
                        self.builder
                            .build_gep(
                                self.context.i8_type(),
                                cur_data,
                                &[src_offset],
                                "pstr.src.rebased",
                            )
                            .unwrap()
                    };
                    self.builder
                        .build_select(src_aliases, rebased, src_ptr, "pstr.src.eff")
                        .unwrap()
                        .into_pointer_value()
                };
                self.builder
                    .build_memcpy(dest, 1, eff_src, 1, src_len)
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
                    .build_call(
                        self.runtime_fns.alloc_fallible_fn,
                        &[new_cap.into()],
                        "tss.new_data",
                    )
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
                // SSO: use the tag-aware owned-heap gate (`SGT cap, 0`) so an
                // inline string (`cap < 0`) is never freed. Proven no-op today
                // (no `cap` has bit 63 set) — Slice-1 free-gate hardening
                // extended to the from-slice free path. NOTE: the memcpy SOURCE
                // above (`data`) is still the raw field-0 load; making it the
                // tag-aware `string_data_ptr` is the coupled construction-flip
                // task (SSO spike Slice 2, step 1's memcpy-source half).
                let was_heap = self.sso_string_is_owned_heap(cap);
                self.builder
                    .build_conditional_branch(was_heap, free_bb, after_free_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                self.builder
                    .build_call(self.runtime_fns.free_fn, &[data.into()], "")
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
            "extend_from_slice" | "extend" => {
                if args.len() != 1 {
                    return Err(format!(
                        "{method} expects 1 argument (source), got {}",
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
                        self.runtime_fns.alloc_or_panic_fn,
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
                // Free old buffer if owned-heap. SSO: tag-aware gate
                // (`SGT cap, 0`) so an inline string (`cap < 0`) is never freed
                // — proven no-op today, Slice-1 hardening extended here. The
                // memcpy source (`data`) stays raw = the coupled flip task.
                let was_heap = self.sso_string_is_owned_heap(cap);
                let free_bb = self.context.append_basic_block(fn_val, "efs.free");
                let after_free_bb = self.context.append_basic_block(fn_val, "efs.after_free");
                self.builder
                    .build_conditional_branch(was_heap, free_bb, after_free_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                self.builder
                    .build_call(self.runtime_fns.free_fn, &[data.into()], "")
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
                let elem_te = self.var_types.var_elem_type_exprs.get(var_name).cloned();
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
                        self.runtime_fns.alloc_fallible_fn,
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
                // SSO: tag-aware owned-heap gate (`SGT cap, 0`) — inline
                // (`cap < 0`) is never freed. Proven no-op today; Slice-1
                // hardening extended to the from-slice free path. The memcpy
                // source (`data`) stays raw = the coupled construction-flip task.
                let was_heap = self.sso_string_is_owned_heap(cap);
                let free_bb = self.context.append_basic_block(fn_val, "tefs.free");
                let after_free_bb = self.context.append_basic_block(fn_val, "tefs.after_free");
                self.builder
                    .build_conditional_branch(was_heap, free_bb, after_free_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                self.builder
                    .build_call(self.runtime_fns.free_fn, &[data.into()], "")
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
                let elem_te = self.var_types.var_elem_type_exprs.get(var_name).cloned();
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
                // Head-index deque: empty is `len == head` (see the `len` arm).
                let empty_mark = match self.deque_head_slot(var_name) {
                    Some(head_slot) => self
                        .builder
                        .build_load(i64_t, head_slot, "deque.head")
                        .unwrap()
                        .into_int_value(),
                    None => zero,
                };
                let is_empty = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, empty_mark, "is_empty")
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
                // B-2026-08-20-39 — `last(n)` is end-relative: `n` counts back
                // from the end and defaults to 0. `first` never takes one.
                //
                // The ARGUMENT IS EVALUATED BEFORE THE BRANCH, unconditionally,
                // so a side-effecting `n` runs exactly once on every path — the
                // empty receiver included. Sinking it into `some_bb` would make
                // the effect depend on the length.
                let n = match (method, args.first()) {
                    ("last", Some(a)) => self.compile_expr(&a.value)?.into_int_value(),
                    _ => zero,
                };
                // No element when the receiver is empty, when `n` is negative,
                // or when it reaches past the front. With `n == 0` this is
                // exactly the `len == 0` test it replaces, so `first` and a
                // bare `last()` are unchanged.
                let n_negative = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, n, zero, "n.neg")
                    .unwrap();
                let n_past_front = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGE, n, len, "n.past")
                    .unwrap();
                let no_elem = self
                    .builder
                    .build_or(n_negative, n_past_front, "no_elem")
                    .unwrap();
                self.builder
                    .build_conditional_branch(no_elem, empty_bb, some_bb)
                    .unwrap();

                // Empty branch — return None.
                self.builder.position_at_end(empty_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Some branch — index 0 (first) or `len - 1 - n` (last).
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
                    let last_idx = self.builder.build_int_sub(len, one, "last_idx").unwrap();
                    self.builder
                        .build_int_sub(last_idx, n, "last_idx.n")
                        .unwrap()
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
            "retain" => {
                // `Vec[T].retain(|x| pred)` — keep each element the inline
                // predicate returns `true` for, compacting in place with a write
                // cursor and freeing the filtered-out heap elements
                // (B-2026-07-15-19). Interp-parity with method_call_seq.rs's
                // snapshot-filter-writeback, lowered to a single linear IR pass:
                //
                //   w = 0
                //   for r in 0..len:
                //       x = data[r]
                //       if pred(x): data[w] = x; w += 1     // byte-move forward
                //       else:       drop_glue(data[r])      // free heap element
                //   len = w
                //
                // Only the INLINE-closure form is lowered here (codegen owns the
                // element drop glue, which a runtime callback couldn't emit); a
                // captured-closure VALUE or fn-ref receiver falls through to the
                // existing deferral bail. Compaction is drop-safe: a kept element
                // moved from r→w (w<r) leaves a stale byte-duplicate at r, but
                // `len = w` excludes [w, len) from every later drop, so the
                // buffer is freed exactly once; a self-store at w==r is a no-op.
                // The predicate reads `x` as a borrow (never frees it), matching
                // the language's read-only retain semantics.
                if args.len() != 1 {
                    return Err(format!(
                        "Vec.retain expects 1 argument (predicate closure), got {}",
                        args.len()
                    ));
                }
                let ExprKind::Closure { params, body, .. } = &args[0].value.kind else {
                    return Err(
                        "codegen: Vec.retain is lowered only for an inline predicate \
                         closure (`retain(|x| …)`); a captured-closure value is \
                         deferred — run it under the interpreter (`karac run --interp`)"
                            .to_string(),
                    );
                };
                if params.len() != 1 {
                    return Err(format!(
                        "Vec.retain predicate takes 1 parameter, got {}",
                        params.len()
                    ));
                }
                let params = params.clone();
                let body = (**body).clone();
                // B-2026-08-10-18 — `retain`'s predicate is inlined into the
                // compaction loop the same way the fused adaptors' bodies are,
                // so an explicit `return` in it lands in the enclosing
                // function. Guarded with the same actionable message rather
                // than left to fail LLVM verification.
                self.register_iter_body_retarget(&body);
                let elem_te = self.var_types.var_elem_type_exprs.get(var_name).cloned();
                let fn_val = self.current_fn.unwrap();

                // Header: data buffer ptr + len.
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "ret.data.p")
                    .unwrap();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "ret.len.p")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "ret.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "ret.len")
                    .unwrap()
                    .into_int_value();

                let zero = i64_t.const_zero();
                let one = i64_t.const_int(1, false);
                let r_slot = self.create_entry_alloca(fn_val, "ret.r", i64_t.into());
                let w_slot = self.create_entry_alloca(fn_val, "ret.w", i64_t.into());
                self.builder.build_store(r_slot, zero).unwrap();
                self.builder.build_store(w_slot, zero).unwrap();

                let cond_bb = self.context.append_basic_block(fn_val, "ret.cond");
                let body_bb = self.context.append_basic_block(fn_val, "ret.body");
                let keep_bb = self.context.append_basic_block(fn_val, "ret.keep");
                let drop_bb = self.context.append_basic_block(fn_val, "ret.drop");
                let step_bb = self.context.append_basic_block(fn_val, "ret.step");
                let done_bb = self.context.append_basic_block(fn_val, "ret.done");
                self.builder.build_unconditional_branch(cond_bb).unwrap();

                // cond: r < len ?
                self.builder.position_at_end(cond_bb);
                let r_cur = self
                    .builder
                    .build_load(i64_t, r_slot, "ret.r.cur")
                    .unwrap()
                    .into_int_value();
                let r_lt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, r_cur, len, "ret.r.lt")
                    .unwrap();
                self.builder
                    .build_conditional_branch(r_lt, body_bb, done_bb)
                    .unwrap();

                // body: x = data[r]; bind param; keep = pred(x)?
                self.builder.position_at_end(body_bb);
                let r_v = self
                    .builder
                    .build_load(i64_t, r_slot, "ret.r.v")
                    .unwrap()
                    .into_int_value();
                let elem_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ty, data, &[r_v], "ret.elem.p")
                        .unwrap()
                };
                let elem_val = self
                    .builder
                    .build_load(elem_ty, elem_ptr, "ret.elem")
                    .unwrap();
                let param_name = match &params[0].pattern.kind {
                    PatternKind::Binding(n) => n.clone(),
                    _ => "__retain_x".to_string(),
                };
                // Bind the closure param by shadowing (the predicate may also
                // capture outer variables, so the outer scope must stay visible).
                // Snapshot the slot + per-name sidecar metadata so a param that
                // shadows an outer binding of the same name is restored intact.
                let saved_slot = self.variables.get(&param_name).copied();
                let saved_meta = self.take_var_metadata(&param_name);
                let palloca = self.create_entry_alloca(fn_val, &param_name, elem_ty);
                self.builder.build_store(palloca, elem_val).unwrap();
                self.variables.insert(
                    param_name.clone(),
                    VarSlot {
                        ptr: palloca,
                        ty: elem_ty,
                    },
                );
                if let Some(te) = &elem_te {
                    self.register_var_from_type_expr(&param_name, te);
                    if let TypeKind::Path(p) = &te.kind {
                        if let Some(seg) = p.segments.first() {
                            if self.type_decls.struct_types.contains_key(seg.as_str())
                                || self.type_decls.shared_types.contains_key(seg.as_str())
                            {
                                self.record_var_type_name(param_name.clone(), seg.clone());
                            }
                        }
                    }
                }
                let keep_val = self.compile_expr(&body)?;
                // Drop the param's own registrations, then reinstate any shadowed
                // outer binding (slot + metadata) exactly as it was.
                let _ = self.take_var_metadata(&param_name);
                self.restore_var_metadata(&param_name, saved_meta);
                match saved_slot {
                    Some(slot) => {
                        self.variables.insert(param_name.clone(), slot);
                    }
                    None => {
                        self.variables.remove(&param_name);
                    }
                }
                let keep_int = keep_val.into_int_value();
                let keep_bool = if keep_int.get_type().get_bit_width() == 1 {
                    keep_int
                } else {
                    self.builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            keep_int,
                            keep_int.get_type().const_zero(),
                            "ret.keep.b",
                        )
                        .unwrap()
                };
                self.builder
                    .build_conditional_branch(keep_bool, keep_bb, drop_bb)
                    .unwrap();

                // keep: data[w] = x (byte-move forward; self-store when w==r); w += 1
                self.builder.position_at_end(keep_bb);
                let w_k = self
                    .builder
                    .build_load(i64_t, w_slot, "ret.w.k")
                    .unwrap()
                    .into_int_value();
                let dst = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ty, data, &[w_k], "ret.dst")
                        .unwrap()
                };
                self.builder.build_store(dst, elem_val).unwrap();
                let w_next = self.builder.build_int_add(w_k, one, "ret.w.next").unwrap();
                self.builder.build_store(w_slot, w_next).unwrap();
                self.builder.build_unconditional_branch(step_bb).unwrap();

                // drop: free the filtered-out heap element (skip trivially-copyable).
                self.builder.position_at_end(drop_bb);
                if let Some(te) = &elem_te {
                    if !is_trivially_copyable_te(te) {
                        let elem_drop = self.emit_drop_fn_for_type_expr(te);
                        let r_d = self
                            .builder
                            .build_load(i64_t, r_slot, "ret.r.d")
                            .unwrap()
                            .into_int_value();
                        let ep = unsafe {
                            self.builder
                                .build_in_bounds_gep(elem_ty, data, &[r_d], "ret.drop.p")
                                .unwrap()
                        };
                        self.builder
                            .build_call(elem_drop, &[ep.into()], "")
                            .unwrap();
                    }
                }
                self.builder.build_unconditional_branch(step_bb).unwrap();

                // step: r += 1
                self.builder.position_at_end(step_bb);
                let r_s = self
                    .builder
                    .build_load(i64_t, r_slot, "ret.r.s")
                    .unwrap()
                    .into_int_value();
                let r_ns = self.builder.build_int_add(r_s, one, "ret.r.ns").unwrap();
                self.builder.build_store(r_slot, r_ns).unwrap();
                self.builder.build_unconditional_branch(cond_bb).unwrap();

                // done: len = w. Buffer + cap unchanged (a later push reuses cap).
                self.builder.position_at_end(done_bb);
                let w_final = self
                    .builder
                    .build_load(i64_t, w_slot, "ret.w.final")
                    .unwrap()
                    .into_int_value();
                self.builder.build_store(len_p, w_final).unwrap();
                Ok(i64_t.const_zero().into())
            }
            "dedup" => {
                // `Vec[T].dedup()` — remove CONSECUTIVE duplicate elements,
                // keeping the first of each run (Rust `Vec::dedup`). Same in-place
                // compaction skeleton as `retain`, but the keep decision is
                // "differs from the PREVIOUS KEPT element" (`data[w-1] != data[r]`)
                // via `compile_binop(Eq)` (scalar icmp / String memcmp / struct
                // field-eq — the same equality `contains` uses), and removed
                // duplicates are freed through the element drop glue. Interp-parity
                // with method_call_seq.rs's snapshot-dedup-writeback.
                //
                //   w = 0
                //   for r in 0..len:
                //       x = data[r]
                //       dup = w != 0 && data[w-1] == x
                //       if dup: drop_glue(data[r])          // free the duplicate
                //       else:   data[w] = x; w += 1         // keep (move forward)
                //   len = w
                if !args.is_empty() {
                    return Err(format!(
                        "Vec.dedup expects no arguments, got {}",
                        args.len()
                    ));
                }
                let elem_te = self.var_types.var_elem_type_exprs.get(var_name).cloned();
                let fn_val = self.current_fn.unwrap();

                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "dd.data.p")
                    .unwrap();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "dd.len.p")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "dd.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "dd.len")
                    .unwrap()
                    .into_int_value();

                let zero = i64_t.const_zero();
                let one = i64_t.const_int(1, false);
                let r_slot = self.create_entry_alloca(fn_val, "dd.r", i64_t.into());
                let w_slot = self.create_entry_alloca(fn_val, "dd.w", i64_t.into());
                self.builder.build_store(r_slot, zero).unwrap();
                self.builder.build_store(w_slot, zero).unwrap();

                let cond_bb = self.context.append_basic_block(fn_val, "dd.cond");
                let body_bb = self.context.append_basic_block(fn_val, "dd.body");
                let cmp_bb = self.context.append_basic_block(fn_val, "dd.cmp");
                let keep_bb = self.context.append_basic_block(fn_val, "dd.keep");
                let drop_bb = self.context.append_basic_block(fn_val, "dd.drop");
                let step_bb = self.context.append_basic_block(fn_val, "dd.step");
                let done_bb = self.context.append_basic_block(fn_val, "dd.done");
                self.builder.build_unconditional_branch(cond_bb).unwrap();

                // cond: r < len ?
                self.builder.position_at_end(cond_bb);
                let r_cur = self
                    .builder
                    .build_load(i64_t, r_slot, "dd.r.c")
                    .unwrap()
                    .into_int_value();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, r_cur, len, "dd.in")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, done_bb)
                    .unwrap();

                // body: x = data[r]; the FIRST element (w==0) is always kept.
                self.builder.position_at_end(body_bb);
                let r_b = self
                    .builder
                    .build_load(i64_t, r_slot, "dd.r.b")
                    .unwrap()
                    .into_int_value();
                let x_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ty, data, &[r_b], "dd.x.p")
                        .unwrap()
                };
                let x_val = self.builder.build_load(elem_ty, x_ptr, "dd.x").unwrap();
                let w_b = self
                    .builder
                    .build_load(i64_t, w_slot, "dd.w.b")
                    .unwrap()
                    .into_int_value();
                let w_is_zero = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, w_b, zero, "dd.w0")
                    .unwrap();
                self.builder
                    .build_conditional_branch(w_is_zero, keep_bb, cmp_bb)
                    .unwrap();

                // cmp: prev = data[w-1]; dup = (prev == x).
                self.builder.position_at_end(cmp_bb);
                let w_m1 = self.builder.build_int_sub(w_b, one, "dd.wm1").unwrap();
                let prev_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ty, data, &[w_m1], "dd.prev.p")
                        .unwrap()
                };
                let prev_val = self
                    .builder
                    .build_load(elem_ty, prev_ptr, "dd.prev")
                    .unwrap();
                let eq = self
                    .compile_binop(&crate::ast::BinOp::Eq, prev_val, x_val)?
                    .into_int_value();
                // `compile_binop` may emit its own blocks (String/struct eq);
                // branch from the CURRENT insert point, not the assumed `cmp_bb`.
                self.builder
                    .build_conditional_branch(eq, drop_bb, keep_bb)
                    .unwrap();

                // keep: data[w] = x; w += 1.
                self.builder.position_at_end(keep_bb);
                let w_k = self
                    .builder
                    .build_load(i64_t, w_slot, "dd.w.k")
                    .unwrap()
                    .into_int_value();
                let dst = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ty, data, &[w_k], "dd.dst")
                        .unwrap()
                };
                self.builder.build_store(dst, x_val).unwrap();
                let w_next = self.builder.build_int_add(w_k, one, "dd.w.next").unwrap();
                self.builder.build_store(w_slot, w_next).unwrap();
                self.builder.build_unconditional_branch(step_bb).unwrap();

                // drop: free the removed duplicate heap element (skip POD).
                self.builder.position_at_end(drop_bb);
                if let Some(te) = &elem_te {
                    if !is_trivially_copyable_te(te) {
                        let elem_drop = self.emit_drop_fn_for_type_expr(te);
                        let r_d = self
                            .builder
                            .build_load(i64_t, r_slot, "dd.r.d")
                            .unwrap()
                            .into_int_value();
                        let ep = unsafe {
                            self.builder
                                .build_in_bounds_gep(elem_ty, data, &[r_d], "dd.drop.p")
                                .unwrap()
                        };
                        self.builder
                            .build_call(elem_drop, &[ep.into()], "")
                            .unwrap();
                    }
                }
                self.builder.build_unconditional_branch(step_bb).unwrap();

                // step: r += 1
                self.builder.position_at_end(step_bb);
                let r_s = self
                    .builder
                    .build_load(i64_t, r_slot, "dd.r.s")
                    .unwrap()
                    .into_int_value();
                let r_ns = self.builder.build_int_add(r_s, one, "dd.r.ns").unwrap();
                self.builder.build_store(r_slot, r_ns).unwrap();
                self.builder.build_unconditional_branch(cond_bb).unwrap();

                // done: len = w. Buffer + cap unchanged.
                self.builder.position_at_end(done_bb);
                let w_final = self
                    .builder
                    .build_load(i64_t, w_slot, "dd.w.final")
                    .unwrap()
                    .into_int_value();
                self.builder.build_store(len_p, w_final).unwrap();
                Ok(i64_t.const_zero().into())
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
                        let elem_type_name: Option<String> =
                            self.var_types.var_elem_type_exprs.get(var_name).and_then(
                                |te| match &te.kind {
                                    TypeKind::Path(p) => p.segments.last().cloned(),
                                    _ => None,
                                },
                            );
                        // The mono path is a stable O(N log N) merge sort
                        // (B-2026-07-30-2), so it is now the ONLY path for
                        // eligible shapes — no runtime callback, no length
                        // split. The previous dispatch emitted both and
                        // branched on `len > 64` because the mono sort was
                        // insertion sort: correct below the threshold, but
                        // above it every comparison went through
                        // `karac_vec_sort_by`'s function pointer, which is
                        // what put #1665 at parity with C's qsort and ~2x
                        // behind Rust.
                        let mono_fn = self.emit_sort_by_mono(
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
                        self.builder
                            .build_call(
                                mono_fn,
                                &[
                                    BasicMetadataValueEnum::from(data),
                                    BasicMetadataValueEnum::from(len),
                                    BasicMetadataValueEnum::from(i64_t.const_int(1, false)),
                                ],
                                "",
                            )
                            .unwrap();
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
                    .var_types
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
                    ExprKind::Closure { params, body, .. } => {
                        let elem_te_owned =
                            self.var_types.var_elem_type_exprs.get(var_name).cloned();
                        self.emit_sort_by_inline_thunk(
                            params,
                            body,
                            elem_ty,
                            elem_type_name.as_deref(),
                            elem_te_owned.as_ref(),
                        )?
                    }
                    ExprKind::Identifier(name) => {
                        if let Some(&closure_fn_type) =
                            self.closure_state.closure_fn_types.get(name)
                        {
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
                // Fast path: 8-byte integer elements (`i64`/`u64`/`isize`/…) sort
                // via the type-specialized runtime `karac_vec_sort_i64_8`, whose
                // inner compare is the primitive's native `Ord` (Rust
                // `sort_unstable`) rather than a per-comparison indirect
                // comparator callback — matching what the Rust mirror does.
                // Narrower ints, String, floats, and compound elements keep the
                // comparator-thunk path below.
                if elem_ty.is_int_type() && elem_ty.into_int_type().get_bit_width() == 64 {
                    let unsigned = matches!(
                        self.vec_elem_type_name(var_name).as_deref(),
                        Some("u8" | "u16" | "u32" | "u64" | "usize" | "uint")
                    );
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
                    let sort_fn = self
                        .module
                        .get_function("karac_vec_sort_i64_8")
                        .unwrap_or_else(|| {
                            let void_t = self.context.void_type();
                            let fn_ty =
                                void_t.fn_type(&[ptr_ty.into(), i64_t.into(), i64_t.into()], false);
                            self.module.add_function(
                                "karac_vec_sort_i64_8",
                                fn_ty,
                                Some(Linkage::External),
                            )
                        });
                    let is_signed = i64_t.const_int(u64::from(!unsigned), false);
                    self.builder
                        .build_call(sort_fn, &[data.into(), len.into(), is_signed.into()], "")
                        .unwrap();
                    return Ok(i64_t.const_zero().into());
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
                    // Unsigned element widths compare unsigned so a high-bit-set
                    // value doesn't sort to the front (B-2026-07-04-8) — matches
                    // the interpreter's `Vec[uN].sort()`.
                    let unsigned = matches!(
                        self.vec_elem_type_name(var_name).as_deref(),
                        Some("u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "uint")
                    );
                    self.emit_default_sort_thunk(elem_ty, unsigned)
                } else if self.vec_elem_type_name(var_name).as_deref() == Some("String") {
                    self.emit_default_sort_thunk_string()
                } else if let Some(cmp_fn) = self
                    .var_types
                    .var_elem_type_exprs
                    .get(var_name)
                    .cloned()
                    .and_then(|ete| self.emit_cmp_fn_for_type_expr(&ete))
                {
                    // B-2026-06-30-15: general default-order elements —
                    // floats, nested Vec/VecDeque (lexicographic, matching
                    // the interpreter's value_compare), tuples of ordered
                    // leaves — via the recursive karac_cmp_<T> family.
                    self.emit_cmp_family_sort_thunk(cmp_fn)
                } else {
                    return Err(
                        "Vec.sort() in codegen supports integer, String, float, tuple, and \
                         nested-Vec element types; use sort_by(|a, b| ...) for other element \
                         types"
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
            // `Vec[T].clear()` — empty the Vec. Drop every element and free the
            // buffer via the SAME per-element drop fn scope-cleanup uses
            // (`emit_vec_drop_fn`), so heap-owning elements (`Vec[String]`,
            // `Vec[Vec[T]]`) can't leak, then reset the header to `{null, 0, 0}`
            // — an empty Vec identical to `Vec.new()`. A later `push` reallocs
            // from scratch, and the reset header makes the scope-end drop a
            // no-op (len 0 loop, cap 0 skips the free), so there's no
            // double-free. `capacity()` is not an observable method, so
            // resetting the capacity (rather than retaining it, as Rust's
            // `Vec::clear` does) is invisible to Kāra programs.
            "clear" => {
                if !args.is_empty() {
                    return Err(format!("Vec.clear expects 0 arguments, got {}", args.len()));
                }
                if let Some(elem_te) = self.var_types.var_elem_type_exprs.get(var_name).cloned() {
                    // B-2026-08-03-2 (class 1) — the element's user Drop BODY,
                    // which this arm never ran: it went straight to the memory
                    // drop, so `v.clear()` on a `Vec[Res]` reclaimed the heap
                    // (vg-clean) while silently skipping every destructor. Same
                    // walker the binding-death path uses, and it frees nothing,
                    // so it cannot disturb the drop below. BEFORE the memory
                    // drop, which is what lets the bodies read the fields they
                    // print — the same ordering rule as every other site that
                    // pairs these two channels.
                    if let Some(bodies) = self.emit_nested_vec_elem_bodies_fn(&elem_te) {
                        self.builder
                            .build_call(bodies, &[data_ptr.into()], "")
                            .unwrap();
                    }
                    let drop_fn = self.emit_vec_drop_fn(&elem_te);
                    self.builder
                        .build_call(drop_fn, &[data_ptr.into()], "")
                        .unwrap();
                }
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "clear.data.p")
                    .unwrap();
                self.builder
                    .build_store(data_pp, ptr_ty.const_null())
                    .unwrap();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "clear.len.p")
                    .unwrap();
                self.builder.build_store(len_p, i64_t.const_zero()).unwrap();
                let cap_p = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 2, "clear.cap.p")
                    .unwrap();
                self.builder.build_store(cap_p, i64_t.const_zero()).unwrap();
                Ok(i64_t.const_zero().into())
            }
            "truncate" => {
                // `Vec.truncate(n)` — shorten to at most `n` elements, dropping
                // (and freeing, for heap-owning element types) the [n, len) tail,
                // then set `len = n` (the buffer + cap are unchanged, so a later
                // push reuses the capacity). `n >= len` is a no-op; `n < 0`
                // clamps to 0. Unlike `clear`, the buffer is NOT freed.
                if args.len() != 1 {
                    return Err(format!(
                        "Vec.truncate expects 1 argument (new length), got {}",
                        args.len()
                    ));
                }
                let fn_val = self.current_fn.unwrap();
                let new_len_raw = self.compile_expr(&args[0].value)?.into_int_value();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "trunc.len.p")
                    .unwrap();
                let cur_len = self
                    .builder
                    .build_load(i64_t, len_p, "trunc.len")
                    .unwrap()
                    .into_int_value();
                // Clamp n into [0, cur_len] (signed: negative → 0).
                let zero = i64_t.const_zero();
                let is_neg = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, new_len_raw, zero, "trunc.neg")
                    .unwrap();
                let n0 = self
                    .builder
                    .build_select(is_neg, zero, new_len_raw, "trunc.n0")
                    .unwrap()
                    .into_int_value();
                let gt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, n0, cur_len, "trunc.gt")
                    .unwrap();
                let n = self
                    .builder
                    .build_select(gt, cur_len, n0, "trunc.n")
                    .unwrap()
                    .into_int_value();
                // Drop the [n, cur_len) tail for heap-owning element types
                // (primitives skip the loop — nothing to free).
                let elem_te = self.var_types.var_elem_type_exprs.get(var_name).cloned();
                if let Some(elem_te) = elem_te {
                    if !is_trivially_copyable_te(&elem_te) {
                        let elem_drop = self.emit_drop_fn_for_type_expr(&elem_te);
                        let elem_ty = self.llvm_type_for_type_expr(&elem_te);
                        let data_pp = self
                            .builder
                            .build_struct_gep(vec_ty, data_ptr, 0, "trunc.data.p")
                            .unwrap();
                        let data = self
                            .builder
                            .build_load(ptr_ty, data_pp, "trunc.data")
                            .unwrap()
                            .into_pointer_value();
                        let i_slot = self.create_entry_alloca(fn_val, "trunc.i", i64_t.into());
                        self.builder.build_store(i_slot, n).unwrap();
                        let cond_bb = self.context.append_basic_block(fn_val, "trunc.cond");
                        let body_bb = self.context.append_basic_block(fn_val, "trunc.body");
                        let done_bb = self.context.append_basic_block(fn_val, "trunc.done");
                        self.builder.build_unconditional_branch(cond_bb).unwrap();
                        self.builder.position_at_end(cond_bb);
                        let i = self
                            .builder
                            .build_load(i64_t, i_slot, "trunc.i.cur")
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(inkwell::IntPredicate::SLT, i, cur_len, "trunc.i.lt")
                            .unwrap();
                        self.builder
                            .build_conditional_branch(lt, body_bb, done_bb)
                            .unwrap();
                        self.builder.position_at_end(body_bb);
                        let elem_ptr = unsafe {
                            self.builder
                                .build_gep(elem_ty, data, &[i], "trunc.elem.ptr")
                                .unwrap()
                        };
                        // B-2026-08-03-2 (class 1) — the REMOVED tail element's
                        // user Drop BODY. This loop already walked [n, len) to
                        // free each element's memory and ran no body, so the
                        // discarded tail lost its destructors while the
                        // survivors (dropped later, at binding death) still
                        // fired — a fire count that looks plausible and is
                        // wrong. Unlike `clear`, a whole-container walker is
                        // NOT usable here: it would re-fire the survivors. The
                        // per-slot dispatcher runs on exactly the elements this
                        // loop is already destroying, ahead of the free so the
                        // body can still read them.
                        self.emit_slot_drop_bodies_at(elem_ptr, &elem_te);
                        self.builder
                            .build_call(elem_drop, &[elem_ptr.into()], "")
                            .unwrap();
                        let i_next = self
                            .builder
                            .build_int_add(i, i64_t.const_int(1, false), "trunc.i.next")
                            .unwrap();
                        self.builder.build_store(i_slot, i_next).unwrap();
                        self.builder.build_unconditional_branch(cond_bb).unwrap();
                        self.builder.position_at_end(done_bb);
                    }
                }
                // Set len = n; buffer + cap unchanged.
                self.builder.build_store(len_p, n).unwrap();
                Ok(i64_t.const_zero().into())
            }
            "split_off" => {
                // `Vec[T].split_off(i) -> Vec[T]` — split at index i: self keeps
                // [0, i), the returned Vec OWNS [i, len). The tail elements MOVE
                // (byte-copy of each `{ptr,len,cap}` for heap types) into a fresh
                // buffer; `self.len = i` excludes them from self's scope-exit
                // drop, so each element frees exactly once (no drop-glue). i is
                // clamped to [0, len].
                if args.len() != 1 {
                    return Err(format!(
                        "Vec.split_off expects 1 argument (index), got {}",
                        args.len()
                    ));
                }
                let i_raw = self.compile_expr(&args[0].value)?.into_int_value();
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "so.data.p")
                    .unwrap();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "so.len.p")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "so.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "so.len")
                    .unwrap()
                    .into_int_value();
                // Clamp i into [0, len] (signed).
                let zero = i64_t.const_zero();
                let is_neg = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, i_raw, zero, "so.neg")
                    .unwrap();
                let i0 = self
                    .builder
                    .build_select(is_neg, zero, i_raw, "so.i0")
                    .unwrap()
                    .into_int_value();
                let gt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, i0, len, "so.gt")
                    .unwrap();
                let i = self
                    .builder
                    .build_select(gt, len, i0, "so.i")
                    .unwrap()
                    .into_int_value();
                let tail_count = self.builder.build_int_sub(len, i, "so.tail").unwrap();
                let elem_size = elem_ty.size_of().unwrap();
                let tail_bytes = self
                    .builder
                    .build_int_mul(tail_count, elem_size, "so.bytes")
                    .unwrap();
                // Fresh buffer for the tail; byte-copy [i, len).
                let new_buf = self
                    .builder
                    .build_call(self.runtime_fns.malloc_fn, &[tail_bytes.into()], "so.buf")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let tail_src = unsafe {
                    self.builder
                        .build_in_bounds_gep(elem_ty, data, &[i], "so.src")
                        .unwrap()
                };
                self.builder
                    .build_memcpy(new_buf, 1, tail_src, 1, tail_bytes)
                    .unwrap();
                // self keeps [0, i).
                self.builder.build_store(len_p, i).unwrap();
                // Build the returned Vec `{ new_buf, tail_count, tail_count }`.
                let mut agg = vec_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, new_buf, 0, "so.r.data")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, tail_count, 1, "so.r.len")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, tail_count, 2, "so.r.cap")
                    .unwrap()
                    .into_struct_value();
                Ok(agg.into())
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
                        let elem_type_name: Option<String> =
                            self.var_types.var_elem_type_exprs.get(var_name).and_then(
                                |te| match &te.kind {
                                    TypeKind::Path(p) => p.segments.last().cloned(),
                                    _ => None,
                                },
                            );
                        {
                            let elem_te_owned =
                                self.var_types.var_elem_type_exprs.get(var_name).cloned();
                            self.emit_sort_by_key_inline_thunk(
                                params,
                                body.as_ref(),
                                elem_ty,
                                elem_type_name.as_deref(),
                                elem_te_owned.as_ref(),
                            )?
                        }
                    }
                    ExprKind::Identifier(name) => {
                        if let Some(&closure_fn_type) =
                            self.closure_state.closure_fn_types.get(name)
                        {
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
            "contains" if self.var_types.string_vars.contains(var_name) => {
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
                        self.runtime_fns.memcmp_fn,
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
            // other element types it errors honestly (works under `karac run --interp`).
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
            // `is_sorted() -> bool` (B-2026-08-21-10) — non-strict ascending;
            // 0 or 1 elements is vacuously sorted.
            //
            // Adjacent pairs go through the SAME `karac_cmp_<T>` family that
            // `sort`'s general path uses, rather than an open-coded `<=`. That
            // is the whole parity argument: the comparator family is built to
            // mirror the interpreter's `value_compare` (unsigned widths
            // compare unsigned, `String` routes to `karac_string_cmp`, structs
            // and enums order by declaration position, `F64` uses the IEEE
            // total order), so `is_sorted` cannot drift from either the
            // interpreter's answer or from the order `sort` just produced. An
            // open-coded `build_int_compare(SLE, ..)` would have read a
            // `Vec[u64]` element past `i64::MAX` as negative — agreeing with
            // neither.
            "is_sorted" => {
                if !args.is_empty() {
                    return Err(format!(
                        "Vec.is_sorted expects 0 arguments, got {}",
                        args.len()
                    ));
                }
                let bool_t = self.context.bool_type();
                let elem_te = self
                    .var_types
                    .var_elem_type_exprs
                    .get(var_name)
                    .cloned()
                    .ok_or_else(|| {
                        format!("Vec.is_sorted: unknown element type for '{var_name}'")
                    })?;
                let cmp_fn = self.emit_cmp_fn_for_type_expr(&elem_te).ok_or_else(|| {
                    "Vec.is_sorted() in codegen supports integer, char, bool, String, float, \
                     tuple, nested-Vec and derived-`Ord` struct/enum element types; add \
                     `#[derive(Ord, Eq)]` to the element type"
                        .to_string()
                })?;

                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 0, "vis.data.ptr")
                    .unwrap();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, data_ptr, 1, "vis.len.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "vis.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, "vis.len")
                    .unwrap()
                    .into_int_value();

                let fn_val = self.current_fn.unwrap();
                let head_bb = self.context.append_basic_block(fn_val, "vis.head");
                let body_bb = self.context.append_basic_block(fn_val, "vis.body");
                let bad_bb = self.context.append_basic_block(fn_val, "vis.bad");
                let next_bb = self.context.append_basic_block(fn_val, "vis.next");
                let done_bb = self.context.append_basic_block(fn_val, "vis.done");

                let i_slot = self.create_entry_alloca(fn_val, "vis.i", i64_t.into());
                let result_slot = self.create_entry_alloca(fn_val, "vis.result", bool_t.into());
                self.builder
                    .build_store(i_slot, i64_t.const_int(1, false))
                    .unwrap();
                self.builder
                    .build_store(result_slot, bool_t.const_int(1, false))
                    .unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                // head: `i < len`, starting at 1 — so an empty or 1-element
                // Vec never enters the body and keeps the initial `true`.
                self.builder.position_at_end(head_bb);
                let i = self
                    .builder
                    .build_load(i64_t, i_slot, "vis.i.load")
                    .unwrap()
                    .into_int_value();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, i, len, "vis.in_range")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, done_bb)
                    .unwrap();

                // body: `cmp(data[i-1], data[i]) > 0` means a descent.
                self.builder.position_at_end(body_bb);
                let prev_i = self
                    .builder
                    .build_int_sub(i, i64_t.const_int(1, false), "vis.i.prev")
                    .unwrap();
                let prev_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[prev_i], "vis.prev.ptr")
                        .unwrap()
                };
                let cur_ptr = unsafe {
                    self.builder
                        .build_gep(elem_ty, data, &[i], "vis.cur.ptr")
                        .unwrap()
                };
                let sign = self
                    .builder
                    .build_call(cmp_fn, &[prev_ptr.into(), cur_ptr.into()], "vis.cmp")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let descends = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::SGT,
                        sign,
                        i64_t.const_zero(),
                        "vis.descends",
                    )
                    .unwrap();
                self.builder
                    .build_conditional_branch(descends, bad_bb, next_bb)
                    .unwrap();

                // bad: record false, exit early.
                self.builder.position_at_end(bad_bb);
                self.builder
                    .build_store(result_slot, bool_t.const_zero())
                    .unwrap();
                self.builder.build_unconditional_branch(done_bb).unwrap();

                // next: i++, loop.
                self.builder.position_at_end(next_bb);
                let i_next = self
                    .builder
                    .build_int_add(i, i64_t.const_int(1, false), "vis.i.next")
                    .unwrap();
                self.builder.build_store(i_slot, i_next).unwrap();
                self.builder.build_unconditional_branch(head_bb).unwrap();

                self.builder.position_at_end(done_bb);
                let result = self
                    .builder
                    .build_load(bool_t, result_slot, "vis.result.load")
                    .unwrap();
                Ok(result)
            }
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
                // Coerce the PROBE to the declared element type
                // (B-2026-08-14-6). The store side already converts — a
                // `Vec[f64].push(some_u8)` really holds 200.0 — but the needle
                // arrived at its own width and class, so the compare below was
                // an `iN` against a `double` and answered `false` for a value
                // that IS in the container. Measured with a genuine-float
                // control: `vd.push(200.0); vd.contains(some_u8)` was false on
                // both surfaces, which is what shows this is the probe rather
                // than the store. `coerce_literal_elem_to_type_from` carries
                // both the int-width and the int->float legs and picks
                // zext/uitofp from the SOURCE's signedness, so `200u8` lands as
                // 200.0 rather than -56.0.
                let needle_val =
                    self.coerce_literal_elem_to_type_from(needle_val, elem_ty, &args[0].value);

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
    /// B-2026-06-30-15 — recursive default-order comparator family:
    /// `i64 karac_cmp_<T>(ptr a, ptr b)` over IN-PLACE values, mirroring
    /// the interpreter's `value_compare` semantics: scalars by value
    /// (signedness from the surface name), floats by partial order (NaN
    /// compares Equal — `unwrap_or(Equal)` parity), String by
    /// `karac_string_cmp`, `Vec`/`VecDeque` lexicographic elementwise then
    /// by length, tuples per-field. Returns `None` for element shapes the
    /// family can't order (user structs/enums, Map/Set, shared) — the
    /// caller keeps its explicit error. Cached by mangled name via
    /// `module.get_function` with the declare-before-body discipline the
    /// clone/drop families use.
    /// Collapse any NaN to the one canonical quiet NaN of its width, so a
    /// float sort's order does not depend on a NaN's sign or payload
    /// (B-2026-08-12-9, the sort-comparator sibling of B-2026-08-11-13's
    /// wrapper canonicalization and B-2026-08-11-17's interpreter one).
    ///
    /// Without it the total-order key is still a valid total order, but a
    /// NEGATIVE NaN sorts before `-Infinity` while a POSITIVE one sorts after
    /// `+Infinity` — and nothing at the source level chooses which you get
    /// (x86 runtime division yields one, LLVM's constant folder the other), so
    /// the same program would sort differently by optimization level and
    /// disagree with the interpreter.
    fn canonicalize_sort_float_nan(
        &mut self,
        v: inkwell::values::FloatValue<'ctx>,
        int_ty: inkwell::types::IntType<'ctx>,
        top: u64,
    ) -> inkwell::values::FloatValue<'ctx> {
        // Positive quiet NaN for this width: exponent all ones, mantissa MSB
        // set. Derived from `top` so f32 (31) and f64 (63) share one path.
        let canon_bits: u64 = if top == 31 {
            0x7FC0_0000
        } else {
            0x7FF8_0000_0000_0000
        };
        let canon = self
            .builder
            .build_bit_cast(
                int_ty.const_int(canon_bits, false),
                v.get_type(),
                "srt.qnan",
            )
            .unwrap()
            .into_float_value();
        let is_nan = self
            .builder
            .build_float_compare(inkwell::FloatPredicate::UNO, v, v, "srt.isnan")
            .unwrap();
        self.builder
            .build_select(is_nan, canon, v, "srt.canon")
            .unwrap()
            .into_float_value()
    }

    pub(super) fn emit_cmp_fn_for_type_expr(
        &mut self,
        te: &TypeExpr,
    ) -> Option<FunctionValue<'ctx>> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let mangled = format!("karac_cmp_{}", Self::display_mangle_te(te));
        if let Some(f) = self.module.get_function(&mangled) {
            return Some(f);
        }
        // Named user struct / enum: derived-`Ord` ordering by field / variant
        // DECLARATION order (B-2026-07-03-7), mirroring the interpreter's
        // `value_compare` (B-2026-07-03-12). Handled in dedicated helpers that
        // pre-guard against self-recursion; both fall through to the scalar /
        // container logic below for every other shape.
        if let TypeKind::Path(p) = &te.kind {
            if p.generic_args.is_none() {
                if let Some(head) = p.segments.first() {
                    // F32/F64/F16/Bf16 are seeded into `struct_field_type_exprs`
                    // as `{ value: f32/f64/f16/bf16 }` (B-2026-07-22-11) so
                    // construction / field-access work, but their comparator
                    // must be the TOTAL order (below), NOT the derived field
                    // comparator which would use the IEEE partial order on
                    // `value`.
                    if !matches!(head.as_str(), "F32" | "F64" | "F16" | "Bf16") {
                        if self.type_decls.struct_field_type_exprs.contains_key(head)
                            && !self.type_decls.shared_types.contains_key(head)
                        {
                            return self.emit_cmp_fn_for_struct(head, &mangled);
                        }
                        if self.type_decls.enum_layouts.contains_key(head) {
                            return self.emit_cmp_fn_for_enum(head, &mangled);
                        }
                    }
                }
            }
        }
        enum Body<'c> {
            IntScalar {
                signed: bool,
            },
            FloatScalar,
            /// Total-order float wrapper (`F32`/`F64`/`F16`/`Bf16`) — a
            /// `{ f32/f64/f16/bf16 }` struct compared by the IEEE 754 total
            /// order (B-2026-07-22-11). `int_ty` is the inner float's bit-
            /// pattern integer type; `top` is its sign-bit index.
            TotalFloatScalar {
                int_ty: inkwell::types::IntType<'c>,
                top: u64,
            },
            Str,
            VecLike {
                child: FunctionValue<'c>,
                elem_llvm: BasicTypeEnum<'c>,
            },
            Tuple {
                children: Vec<FunctionValue<'c>>,
                tuple_llvm: inkwell::types::StructType<'c>,
            },
        }
        let body = match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => {
                let mut children = Vec::with_capacity(elems.len());
                for e in elems {
                    children.push(self.emit_cmp_fn_for_type_expr(e)?);
                }
                let field_tys: Vec<BasicTypeEnum> = elems
                    .iter()
                    .map(|e| self.llvm_type_for_type_expr(e))
                    .collect();
                Body::Tuple {
                    children,
                    tuple_llvm: self.context.struct_type(&field_tys, false),
                }
            }
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                match head {
                    "i8" | "i16" | "i32" | "i64" | "isize" => Body::IntScalar { signed: true },
                    "u8" | "u16" | "u32" | "u64" | "usize" | "bool" | "char" => {
                        Body::IntScalar { signed: false }
                    }
                    "f32" | "f64" => Body::FloatScalar,
                    "F32" | "F64" | "F16" | "Bf16" => {
                        let (_ft, int_ty, top) = self
                            .total_float_wrapper_widths(head)
                            .expect("F32/F64/F16/Bf16 have wrapper widths");
                        Body::TotalFloatScalar { int_ty, top }
                    }
                    // Both String spellings (the 3p discipline).
                    "String" | "str" => Body::Str,
                    "Vec" | "VecDeque" => {
                        let elem_te = match p.generic_args.as_ref()?.first()? {
                            GenericArg::Type(t) => t.clone(),
                            _ => return None,
                        };
                        let child = self.emit_cmp_fn_for_type_expr(&elem_te)?;
                        Body::VecLike {
                            child,
                            elem_llvm: self.llvm_type_for_type_expr(&elem_te),
                        }
                    }
                    _ => return None,
                }
            }
            _ => return None,
        };

        let fn_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let cmp_fn = self
            .module
            .add_function(&mangled, fn_ty, Some(Linkage::Internal));
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(cmp_fn);
        let entry = self.context.append_basic_block(cmp_fn, "entry");
        self.builder.position_at_end(entry);
        let a_ptr = cmp_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = cmp_fn.get_nth_param(1).unwrap().into_pointer_value();
        let neg_one = i64_t.const_int((-1i64) as u64, true);
        let pos_one = i64_t.const_int(1, false);
        let zero = i64_t.const_zero();

        match body {
            Body::IntScalar { signed } => {
                let elem_llvm = self.llvm_type_for_type_expr(te);
                let a = self
                    .builder
                    .build_load(elem_llvm, a_ptr, "a")
                    .unwrap()
                    .into_int_value();
                let b = self
                    .builder
                    .build_load(elem_llvm, b_ptr, "b")
                    .unwrap()
                    .into_int_value();
                let (lt_p, gt_p) = if signed {
                    (inkwell::IntPredicate::SLT, inkwell::IntPredicate::SGT)
                } else {
                    (inkwell::IntPredicate::ULT, inkwell::IntPredicate::UGT)
                };
                let lt = self.builder.build_int_compare(lt_p, a, b, "lt").unwrap();
                let gt = self.builder.build_int_compare(gt_p, a, b, "gt").unwrap();
                let gt_sel = self
                    .builder
                    .build_select(gt, pos_one, zero, "gtsel")
                    .unwrap();
                let r = self
                    .builder
                    .build_select(lt, neg_one.into(), gt_sel, "cmp")
                    .unwrap();
                self.builder.build_return(Some(&r)).unwrap();
            }
            Body::FloatScalar => {
                // B-2026-08-12-9 — TOTAL order, the same key `TotalFloatScalar`
                // below and `karac_float_cmp` use, with NaN canonicalized
                // first (B-2026-08-11-13 / -17).
                //
                // This used to be `OLT`/`OGT`, the ORDERED IEEE predicates:
                // with a NaN both are false, so every comparison involving one
                // returned 0 — "equal". That is not a misplaced NaN, it is an
                // INTRANSITIVE comparator, and the merge sort built on it
                // returns the sequence essentially untouched.
                //
                // A bare `f64` element is not supposed to reach a `sort()` at
                // all — design.md § Float semantics makes `f64` non-`Ord` and
                // B-2026-08-11-7 gates `Vec[f64].sort()` / `.sorted()` at
                // typecheck. But the gate sees the element type at the CALL
                // SITE, so a generic (`fn gsorted[T](v: ref Vec[T]) -> Vec[T]
                // { v.sorted() }` at `T = f64`) passes it and monomorphizes
                // straight to this emitter. Measured before this change:
                // `[3.5, NaN, 1.25, 2.0]` came back `3.5 NaN 1.25 2` compiled
                // while the interpreter returned it correctly sorted with NaN
                // last — a silent wrong answer AND a run-vs-build split, the
                // latter newly created when B-2026-08-11-17 made the
                // interpreter's float ordering total.
                //
                // So the front-end gate states the language rule and this
                // states what the machine does when the rule is bypassed:
                // the same defined total order the interpreter, the `F64`
                // wrapper, and `sort_by_key`'s sanctioned float-key path all
                // already use. Defense in depth, not a relaxation of the rule.
                let elem_llvm = self.llvm_type_for_type_expr(te);
                let a = self
                    .builder
                    .build_load(elem_llvm, a_ptr, "a")
                    .unwrap()
                    .into_float_value();
                let b = self
                    .builder
                    .build_load(elem_llvm, b_ptr, "b")
                    .unwrap()
                    .into_float_value();
                // Width-generic: f32 keeps its own 32-bit key rather than
                // widening, which preserves the order exactly and avoids a
                // conversion in the inner loop.
                let (int_ty, top) = if elem_llvm.into_float_type() == self.context.f32_type() {
                    (self.context.i32_type(), 31u64)
                } else {
                    (self.context.i64_type(), 63u64)
                };
                let a = self.canonicalize_sort_float_nan(a, int_ty, top);
                let b = self.canonicalize_sort_float_nan(b, int_ty, top);
                let a_bits = self
                    .builder
                    .build_bit_cast(a, int_ty, "a.b")
                    .unwrap()
                    .into_int_value();
                let b_bits = self
                    .builder
                    .build_bit_cast(b, int_ty, "b.b")
                    .unwrap()
                    .into_int_value();
                let a_key = self.total_order_key(a_bits, top);
                let b_key = self.total_order_key(b_bits, top);
                let lt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, a_key, b_key, "lt")
                    .unwrap();
                let gt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, a_key, b_key, "gt")
                    .unwrap();
                let gt_sel = self
                    .builder
                    .build_select(gt, pos_one, zero, "gtsel")
                    .unwrap();
                let r = self
                    .builder
                    .build_select(lt, neg_one.into(), gt_sel, "cmp")
                    .unwrap();
                self.builder.build_return(Some(&r)).unwrap();
            }
            Body::TotalFloatScalar { int_ty, top } => {
                // Element is a `{ f32/f64/f16/bf16 }` wrapper struct. Load,
                // extract the inner float, and compare on the TOTAL-order key
                // (the same transform `compile_total_order_wrapper_cmp` emits):
                // signed integer compare of `bits ^ ((bits >> top) >>u 1)`.
                let elem_llvm = self.llvm_type_for_type_expr(te);
                let a_s = self
                    .builder
                    .build_load(elem_llvm, a_ptr, "a.tf")
                    .unwrap()
                    .into_struct_value();
                let b_s = self
                    .builder
                    .build_load(elem_llvm, b_ptr, "b.tf")
                    .unwrap()
                    .into_struct_value();
                let a_f = self
                    .builder
                    .build_extract_value(a_s, 0, "a.tf.v")
                    .unwrap()
                    .into_float_value();
                let b_f = self
                    .builder
                    .build_extract_value(b_s, 0, "b.tf.v")
                    .unwrap()
                    .into_float_value();
                let a_bits = self
                    .builder
                    .build_bit_cast(a_f, int_ty, "a.tf.b")
                    .unwrap()
                    .into_int_value();
                let b_bits = self
                    .builder
                    .build_bit_cast(b_f, int_ty, "b.tf.b")
                    .unwrap()
                    .into_int_value();
                let a_key = self.total_order_key(a_bits, top);
                let b_key = self.total_order_key(b_bits, top);
                let lt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, a_key, b_key, "lt")
                    .unwrap();
                let gt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, a_key, b_key, "gt")
                    .unwrap();
                let gt_sel = self
                    .builder
                    .build_select(gt, pos_one, zero, "gtsel")
                    .unwrap();
                let r = self
                    .builder
                    .build_select(lt, neg_one.into(), gt_sel, "cmp")
                    .unwrap();
                self.builder.build_return(Some(&r)).unwrap();
            }
            Body::Str => {
                let vec_ty = self.vec_struct_type();
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
                let a_data = self.builder.build_extract_value(a, 0, "ad").unwrap();
                let a_len = self.builder.build_extract_value(a, 1, "al").unwrap();
                let b_data = self.builder.build_extract_value(b, 0, "bd").unwrap();
                let b_len = self.builder.build_extract_value(b, 1, "bl").unwrap();
                let scmp = self
                    .module
                    .get_function("karac_string_cmp")
                    .unwrap_or_else(|| {
                        let fn_ty2 = i64_t.fn_type(
                            &[ptr_ty.into(), i64_t.into(), ptr_ty.into(), i64_t.into()],
                            false,
                        );
                        self.module.add_function(
                            "karac_string_cmp",
                            fn_ty2,
                            Some(Linkage::External),
                        )
                    });
                let r = self
                    .builder
                    .build_call(
                        scmp,
                        &[a_data.into(), a_len.into(), b_data.into(), b_len.into()],
                        "strcmp",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                self.builder.build_return(Some(&r)).unwrap();
            }
            Body::VecLike { child, elem_llvm } => {
                let vec_ty = self.vec_struct_type();
                // Load both headers.
                let a_hdr = self
                    .builder
                    .build_load(vec_ty, a_ptr, "a.hdr")
                    .unwrap()
                    .into_struct_value();
                let b_hdr = self
                    .builder
                    .build_load(vec_ty, b_ptr, "b.hdr")
                    .unwrap()
                    .into_struct_value();
                let a_data = self
                    .builder
                    .build_extract_value(a_hdr, 0, "a.data")
                    .unwrap()
                    .into_pointer_value();
                let a_len = self
                    .builder
                    .build_extract_value(a_hdr, 1, "a.len")
                    .unwrap()
                    .into_int_value();
                let b_data = self
                    .builder
                    .build_extract_value(b_hdr, 0, "b.data")
                    .unwrap()
                    .into_pointer_value();
                let b_len = self
                    .builder
                    .build_extract_value(b_hdr, 1, "b.len")
                    .unwrap()
                    .into_int_value();
                let min_gt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, a_len, b_len, "alt")
                    .unwrap();
                let min_len = self
                    .builder
                    .build_select(min_gt, a_len, b_len, "minlen")
                    .unwrap()
                    .into_int_value();
                let idx = self.create_entry_alloca(cmp_fn, "i", i64_t.into());
                self.builder.build_store(idx, i64_t.const_zero()).unwrap();
                let cond_bb = self.context.append_basic_block(cmp_fn, "loop.cond");
                let body_bb = self.context.append_basic_block(cmp_fn, "loop.body");
                let neq_bb = self.context.append_basic_block(cmp_fn, "elem.neq");
                let incr_bb = self.context.append_basic_block(cmp_fn, "loop.incr");
                let len_bb = self.context.append_basic_block(cmp_fn, "len.cmp");
                self.builder.build_unconditional_branch(cond_bb).unwrap();
                self.builder.position_at_end(cond_bb);
                let cur = self
                    .builder
                    .build_load(i64_t, idx, "cur")
                    .unwrap()
                    .into_int_value();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, cur, min_len, "inr")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, len_bb)
                    .unwrap();
                self.builder.position_at_end(body_bb);
                let a_elem = unsafe {
                    self.builder
                        .build_gep(elem_llvm, a_data, &[cur], "a.el")
                        .unwrap()
                };
                let b_elem = unsafe {
                    self.builder
                        .build_gep(elem_llvm, b_data, &[cur], "b.el")
                        .unwrap()
                };
                let r = self
                    .builder
                    .build_call(child, &[a_elem.into(), b_elem.into()], "elcmp")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let nz = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::NE, r, zero, "nz")
                    .unwrap();
                self.builder
                    .build_conditional_branch(nz, neq_bb, incr_bb)
                    .unwrap();
                self.builder.position_at_end(neq_bb);
                self.builder.build_return(Some(&r)).unwrap();
                self.builder.position_at_end(incr_bb);
                let next = self
                    .builder
                    .build_int_add(cur, i64_t.const_int(1, false), "next")
                    .unwrap();
                self.builder.build_store(idx, next).unwrap();
                self.builder.build_unconditional_branch(cond_bb).unwrap();
                self.builder.position_at_end(len_bb);
                let llt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, a_len, b_len, "llt")
                    .unwrap();
                let lgt = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SGT, a_len, b_len, "lgt")
                    .unwrap();
                let gt_sel = self
                    .builder
                    .build_select(lgt, pos_one, zero, "lgts")
                    .unwrap();
                let r2 = self
                    .builder
                    .build_select(llt, neg_one.into(), gt_sel, "lencmp")
                    .unwrap();
                self.builder.build_return(Some(&r2)).unwrap();
            }
            Body::Tuple {
                children,
                tuple_llvm,
            } => {
                let mut next_bb = self.context.append_basic_block(cmp_fn, "t.f0");
                self.builder.build_unconditional_branch(next_bb).unwrap();
                for (i, child) in children.iter().enumerate() {
                    self.builder.position_at_end(next_bb);
                    let a_f = self
                        .builder
                        .build_struct_gep(tuple_llvm, a_ptr, i as u32, "a.f")
                        .unwrap();
                    let b_f = self
                        .builder
                        .build_struct_gep(tuple_llvm, b_ptr, i as u32, "b.f")
                        .unwrap();
                    let r = self
                        .builder
                        .build_call(*child, &[a_f.into(), b_f.into()], "fcmp")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value();
                    let nz = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::NE, r, zero, "nz")
                        .unwrap();
                    let ret_bb = self.context.append_basic_block(cmp_fn, "t.ret");
                    let cont_bb = self
                        .context
                        .append_basic_block(cmp_fn, &format!("t.f{}", i + 1));
                    self.builder
                        .build_conditional_branch(nz, ret_bb, cont_bb)
                        .unwrap();
                    self.builder.position_at_end(ret_bb);
                    self.builder.build_return(Some(&r)).unwrap();
                    next_bb = cont_bb;
                }
                self.builder.position_at_end(next_bb);
                self.builder.build_return(Some(&zero)).unwrap();
            }
        }
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(cmp_fn)
    }

    /// `karac_cmp_<Struct>` for a `#[derive(Ord)]` user struct: compare fields
    /// in DECLARATION order via the recursive `karac_cmp_<T>` family, returning
    /// the first non-`Equal` field's result (the tuple template, keyed on the
    /// struct's LLVM layout). Mirrors the interpreter's declaration-order
    /// comparison (B-2026-07-03-12). `None` — caller keeps its loud "use
    /// sort_by" error — for a shared struct, an unknown struct, a layout-block /
    /// SoA struct whose physical field count diverges from its logical field
    /// list, a self-recursive struct, or any field whose own type is
    /// unorderable.
    fn emit_cmp_fn_for_struct(
        &mut self,
        struct_name: &str,
        mangled: &str,
    ) -> Option<FunctionValue<'ctx>> {
        // Only types that opt into ordering (`#[derive(Ord/PartialOrd)]` or a
        // user impl) are orderable; others stay rejected at the sort site.
        if !self.type_decls.ord_orderable_types.contains(struct_name) {
            return None;
        }
        // A field that recurses back into this same type (`S { next: Vec[S] }`)
        // → unorderable, rather than infinite compile-time recursion.
        if self.cmp_fn_in_progress.contains(struct_name) {
            return None;
        }
        let struct_ty = *self.type_decls.struct_types.get(struct_name)?;
        let field_tes = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)?
            .clone();
        // Plain field-ordered struct only (mirrors emit_struct_clone_fn's guard).
        if struct_ty.count_fields() as usize != field_tes.len() {
            return None;
        }

        self.cmp_fn_in_progress.insert(struct_name.to_string());
        let children: Option<Vec<FunctionValue<'ctx>>> = field_tes
            .iter()
            .map(|te| self.emit_cmp_fn_for_type_expr(te))
            .collect();
        self.cmp_fn_in_progress.remove(struct_name);
        let children = children?;

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();
        let fn_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let cmp_fn = self
            .module
            .add_function(mangled, fn_ty, Some(Linkage::Internal));
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(cmp_fn);
        let entry = self.context.append_basic_block(cmp_fn, "entry");
        self.builder.position_at_end(entry);
        let a_ptr = cmp_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = cmp_fn.get_nth_param(1).unwrap().into_pointer_value();

        let mut next_bb = self.context.append_basic_block(cmp_fn, "s.f0");
        self.builder.build_unconditional_branch(next_bb).unwrap();
        for (i, child) in children.iter().enumerate() {
            self.builder.position_at_end(next_bb);
            let a_f = self
                .builder
                .build_struct_gep(struct_ty, a_ptr, i as u32, "a.f")
                .unwrap();
            let b_f = self
                .builder
                .build_struct_gep(struct_ty, b_ptr, i as u32, "b.f")
                .unwrap();
            let r = self
                .builder
                .build_call(*child, &[a_f.into(), b_f.into()], "fcmp")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let nz = self
                .builder
                .build_int_compare(inkwell::IntPredicate::NE, r, zero, "nz")
                .unwrap();
            let ret_bb = self.context.append_basic_block(cmp_fn, "s.ret");
            let cont_bb = self
                .context
                .append_basic_block(cmp_fn, &format!("s.f{}", i + 1));
            self.builder
                .build_conditional_branch(nz, ret_bb, cont_bb)
                .unwrap();
            self.builder.position_at_end(ret_bb);
            self.builder.build_return(Some(&r)).unwrap();
            next_bb = cont_bb;
        }
        self.builder.position_at_end(next_bb);
        self.builder.build_return(Some(&zero)).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(cmp_fn)
    }

    /// `karac_cmp_<Enum>` for a `#[derive(Ord)]` user enum: order by variant
    /// DISCRIMINANT first (the tag at field 0 — assigned in DECLARATION order,
    /// so `Priority { Low, Med, High }` yields `Low < Med < High`), then, for
    /// two values of the same variant, by payload fields in declaration order
    /// via the `karac_cmp_<T>` family. Matches the interpreter's declaration-
    /// order enum comparison (B-2026-07-03-12). `None` — caller keeps its loud
    /// error — for a shared enum, a self-recursive enum, or any payload field
    /// whose type is unorderable.
    fn emit_cmp_fn_for_enum(
        &mut self,
        enum_name: &str,
        mangled: &str,
    ) -> Option<FunctionValue<'ctx>> {
        // Only types that opt into ordering (`#[derive(Ord/PartialOrd)]` or a
        // user impl) are orderable; others stay rejected at the sort site.
        if !self.type_decls.ord_orderable_types.contains(enum_name) {
            return None;
        }
        if self.cmp_fn_in_progress.contains(enum_name) {
            return None;
        }
        let layout = self.type_decls.enum_layouts.get(enum_name)?.clone();
        if layout.is_shared {
            return None;
        }

        // Per-variant payload field TypeExprs (declaration order).
        let variant_field_tes: Vec<(String, Vec<TypeExpr>)> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .map(|(_tag, name, tes)| (name, tes))
            .collect();

        // Pre-emit a child cmp fn per payload field, keyed by (variant, llvm
        // field index = start_word + 1). Guard self-recursion; any unorderable
        // field aborts before this enum's fn is declared (no undefined IR).
        self.cmp_fn_in_progress.insert(enum_name.to_string());
        let mut field_cmps: Vec<(String, u32, FunctionValue<'ctx>)> = Vec::new();
        let mut unorderable = false;
        'variants: for (variant_name, field_tes) in &variant_field_tes {
            let Some(offsets) = layout.field_word_offsets.get(variant_name) else {
                continue;
            };
            for (fi, (start_word, _num_words)) in offsets.iter().enumerate() {
                let Some(field_te) = field_tes.get(fi) else {
                    continue;
                };
                match self.emit_cmp_fn_for_type_expr(field_te) {
                    Some(cf) => {
                        field_cmps.push((variant_name.clone(), (*start_word + 1) as u32, cf))
                    }
                    None => {
                        unorderable = true;
                        break 'variants;
                    }
                }
            }
        }
        self.cmp_fn_in_progress.remove(enum_name);
        if unorderable {
            return None;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();
        let neg_one = i64_t.const_int((-1i64) as u64, true);
        let pos_one = i64_t.const_int(1, false);
        let fn_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let cmp_fn = self
            .module
            .add_function(mangled, fn_ty, Some(Linkage::Internal));
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(cmp_fn);
        let enum_llvm = layout.llvm_type;

        let entry = self.context.append_basic_block(cmp_fn, "entry");
        self.builder.position_at_end(entry);
        let a_ptr = cmp_fn.get_nth_param(0).unwrap().into_pointer_value();
        let b_ptr = cmp_fn.get_nth_param(1).unwrap().into_pointer_value();
        let a_tag_ptr = self
            .builder
            .build_struct_gep(enum_llvm, a_ptr, 0, "a.tag.p")
            .unwrap();
        let b_tag_ptr = self
            .builder
            .build_struct_gep(enum_llvm, b_ptr, 0, "b.tag.p")
            .unwrap();
        let a_tag = self
            .builder
            .build_load(i64_t, a_tag_ptr, "a.tag")
            .unwrap()
            .into_int_value();
        let b_tag = self
            .builder
            .build_load(i64_t, b_tag_ptr, "b.tag")
            .unwrap()
            .into_int_value();
        let tags_eq = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, a_tag, b_tag, "tags.eq")
            .unwrap();
        let tagdiff_bb = self.context.append_basic_block(cmp_fn, "tag.diff");
        let payload_bb = self.context.append_basic_block(cmp_fn, "payload");
        let equal_bb = self.context.append_basic_block(cmp_fn, "eq");
        self.builder
            .build_conditional_branch(tags_eq, payload_bb, tagdiff_bb)
            .unwrap();

        // Tags differ → order by discriminant (declaration order). Unsigned:
        // tags are small non-negative discriminants.
        self.builder.position_at_end(tagdiff_bb);
        let tag_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::ULT, a_tag, b_tag, "tag.lt")
            .unwrap();
        let tag_gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, a_tag, b_tag, "tag.gt")
            .unwrap();
        let gt_sel = self
            .builder
            .build_select(tag_gt, pos_one, zero, "tag.gtsel")
            .unwrap();
        let tr = self
            .builder
            .build_select(tag_lt, neg_one.into(), gt_sel, "tag.cmp")
            .unwrap();
        self.builder.build_return(Some(&tr)).unwrap();

        // Equal tags → compare the active variant's payload fields.
        self.builder.position_at_end(equal_bb);
        self.builder.build_return(Some(&zero)).unwrap();

        self.builder.position_at_end(payload_bb);
        // One case BB per variant that has payload comparisons; the rest
        // (unit variants) fall to the default `equal_bb`.
        let mut variants_with_fields: Vec<String> = Vec::new();
        for (vn, _, _) in &field_cmps {
            if !variants_with_fields.contains(vn) {
                variants_with_fields.push(vn.clone());
            }
        }
        let mut switch_cases: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();
        let case_bbs: Vec<(String, BasicBlock<'ctx>)> = variants_with_fields
            .iter()
            .filter_map(|vn| {
                let tag = *layout.tags.get(vn)?;
                let bb = self.context.append_basic_block(cmp_fn, &format!("v.{vn}"));
                switch_cases.push((i64_t.const_int(tag, false), bb));
                Some((vn.clone(), bb))
            })
            .collect();
        self.builder
            .build_switch(a_tag, equal_bb, &switch_cases)
            .unwrap();

        for (variant_name, bb) in &case_bbs {
            self.builder.position_at_end(*bb);
            let fields: Vec<(u32, FunctionValue<'ctx>)> = field_cmps
                .iter()
                .filter(|(vn, _, _)| vn == variant_name)
                .map(|(_, idx, f)| (*idx, *f))
                .collect();
            let mut next_bb = *bb;
            for (i, (field_idx, child)) in fields.iter().enumerate() {
                self.builder.position_at_end(next_bb);
                let a_f = self
                    .builder
                    .build_struct_gep(enum_llvm, a_ptr, *field_idx, "a.pf")
                    .unwrap();
                let b_f = self
                    .builder
                    .build_struct_gep(enum_llvm, b_ptr, *field_idx, "b.pf")
                    .unwrap();
                let r = self
                    .builder
                    .build_call(*child, &[a_f.into(), b_f.into()], "pfcmp")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let nz = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::NE, r, zero, "nz")
                    .unwrap();
                let ret_bb = self.context.append_basic_block(cmp_fn, "v.ret");
                let cont_bb = self
                    .context
                    .append_basic_block(cmp_fn, &format!("v.{variant_name}.{}", i + 1));
                self.builder
                    .build_conditional_branch(nz, ret_bb, cont_bb)
                    .unwrap();
                self.builder.position_at_end(ret_bb);
                self.builder.build_return(Some(&r)).unwrap();
                next_bb = cont_bb;
            }
            self.builder.position_at_end(next_bb);
            self.builder.build_unconditional_branch(equal_bb).unwrap();
        }

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(cmp_fn)
    }

    /// Adapt a `karac_cmp_<T>(a, b)` family fn to the sort-thunk ABI
    /// `(ctx, a, b) -> i64` (ctx ignored).
    fn emit_cmp_family_sort_thunk(&mut self, cmp_fn: FunctionValue<'ctx>) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
        let name = format!("__sort_family_cmp_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(thunk_fn);
        let entry = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry);
        let a = thunk_fn.get_nth_param(1).unwrap();
        let b = thunk_fn.get_nth_param(2).unwrap();
        let r = self
            .builder
            .build_call(cmp_fn, &[a.into(), b.into()], "cmp")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&r)).unwrap();
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        thunk_fn
    }

    pub(super) fn emit_default_sort_thunk(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
        // Compare the loaded integer elements as unsigned (`ULT`/`UGT`) when the
        // source element type is unsigned. Without this a `u8` value like 200
        // read as a signed i8 (-56) — and any `u64` value ≥ 2⁶³ — sorts to the
        // front, diverging from the interpreter's now-unsigned `Vec[uN].sort()`
        // (B-2026-07-04-8). The narrow widths always mis-sorted values with the
        // width's high bit set; u64 only started diverging once the interpreter
        // gained its u64 model.
        unsigned: bool,
    ) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
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
        let (lt_pred, gt_pred) = if unsigned {
            (inkwell::IntPredicate::ULT, inkwell::IntPredicate::UGT)
        } else {
            (inkwell::IntPredicate::SLT, inkwell::IntPredicate::SGT)
        };
        let lt = self.builder.build_int_compare(lt_pred, a, b, "lt").unwrap();
        let gt = self.builder.build_int_compare(gt_pred, a, b, "gt").unwrap();
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

        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
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
    /// B-2026-08-10-16 — compile a sort comparator / key body with an
    /// explicit `return` retargeted to the comparator's own result, instead of
    /// letting it emit a real function return.
    ///
    /// The comparator body is INLINED into the thunk (or, on the mono path,
    /// straight into the sort function). A `return E` inside it was lowered by
    /// the ordinary fn-level machinery, which is wrong twice over:
    ///
    ///   * it emits `ret <Ordering>` in a function whose LLVM return type is
    ///     `i64` (thunk) or `void` (mono sort) — "Found return instr that
    ///     returns non-void in Function of void return type", and
    ///   * it drains the WHOLE cleanup stack, including the CALLER's frames,
    ///     so the caller's allocas get referenced from inside the emitted sort
    ///     function — "Instruction does not dominate all uses".
    ///
    /// Both disappear by reusing the `ReturnRetarget` machinery
    /// `with_provider` already uses for the same problem (B-2026-07-31-16): a
    /// retargeted `return` stores its value into a slot, drains only the
    /// frames pushed since the retarget, and branches to a merge block. The
    /// joined value is then an ordinary value, so the caller's existing
    /// Ordering-unwrap (`{ i64 }` → tag − 1) applies to the `return` path and
    /// the implicit-tail path identically — which is the whole bug, since only
    /// the tail path had been unwrapping.
    ///
    /// `fn_val` must be the function the body is being emitted into;
    /// `wp_return_retarget_active` tags on it, so a real closure compiled
    /// while this is live (which switches `current_fn`) correctly does NOT
    /// retarget.
    /// B-2026-08-10-18 — the fused-iterator sibling of
    /// [`Self::compile_sort_body_retargeting_return`], reached from
    /// `compile_expr`'s span hook rather than from an emitter that owns the
    /// call. Same machinery, same reason: a `return` inside an INLINED closure
    /// body must produce the body's value, not return from the function the
    /// body was spliced into.
    pub(super) fn compile_expr_retargeting_return(
        &mut self,
        body: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let Some(fn_val) = self.current_fn else {
            return self.compile_expr(body);
        };
        let cleanup_depth = self.drop_rc.scope_cleanup_actions.len();
        self.fn_ctx
            .return_retargets
            .push(super::state::ReturnRetarget {
                fn_val,
                cleanup_depth,
                merge_bb: None,
                result_slot: None,
                result_ty: None,
                result_type_expr: None,
            });
        let body_result = self.compile_expr(body);
        let rt = self
            .fn_ctx
            .return_retargets
            .pop()
            .expect("retarget pushed just above is still on the stack");
        let fall_through = body_result?;
        self.join_retargeted_returns(rt, fall_through)
    }

    fn compile_sort_body_retargeting_return(
        &mut self,
        fn_val: FunctionValue<'ctx>,
        body: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let cleanup_depth = self.drop_rc.scope_cleanup_actions.len();
        self.fn_ctx
            .return_retargets
            .push(super::state::ReturnRetarget {
                fn_val,
                cleanup_depth,
                merge_bb: None,
                result_slot: None,
                result_ty: None,
                result_type_expr: None,
            });
        let body_result = self.compile_expr(body);
        let rt = self
            .fn_ctx
            .return_retargets
            .pop()
            .expect("retarget pushed just above is still on the stack");
        let fall_through = body_result?;
        self.join_retargeted_returns(rt, fall_through)
    }

    pub(super) fn emit_sort_by_key_inline_thunk(
        &mut self,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_type_name: Option<&str>,
        elem_te: Option<&TypeExpr>,
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
        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
        let name = format!("__sort_by_key_inline_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        // 4. Save outer codegen state.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_subst = std::mem::take(&mut self.mono_state.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_state.closure_fn_types);
        let saved_pct = self.closure_state.pending_closure_fn_type.take();
        // Clear the par-branch cancel pointer for the thunk body (B-2026-06-18-10):
        // the comparator is a separate fn, so a method call in it (e.g. `a.cmp(b)`)
        // must not load the enclosing par-branch's `cancel_flag` arg.
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();

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
                    self.var_types
                        .var_type_names
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
            self.var_types
                .var_type_names
                .insert(param_name.clone(), name.to_string());
        }
        // B-2026-08-10-13 — `sort_by_key` has the same gap as `sort_by` and is
        // fixed with it: `v.sort_by_key(|x| x.len())` over a `Vec[String]`
        // failed identically, because the name registration above serves field
        // access only and method dispatch resolves through
        // `vec_elem_types` / `slice_elem_types` / `var_elem_type_exprs`.
        // Found by probing the sibling after fixing the reported method,
        // rather than left for the next kata to rediscover.
        if let Some(te) = elem_te {
            self.register_var_from_type_expr(&param_name, te);
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
        let key_a_val = self.compile_sort_body_retargeting_return(thunk_fn, body)?;

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
        let key_b_val = self.compile_sort_body_retargeting_return(thunk_fn, body)?;

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
        let res = if self.span_tables.string_typed_exprs.contains(&key_body_span) {
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
        } else if let Some(cmp_callee_key) = self
            .span_tables
            .user_ord_typed_exprs
            .get(&key_body_span)
            .cloned()
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
        } else if let Some(struct_name) = self
            .span_tables
            .expr_struct_type_names
            .get(&key_body_span)
            .cloned()
        {
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
        // B-2026-08-10-13 — drop the key param's container registrations; the
        // name is thunk-local and must not shadow an enclosing binding.
        self.var_types.vec_elem_types.remove(&param_name);
        self.var_types.slice_elem_types.remove(&param_name);
        self.var_types.var_elem_type_exprs.remove(&param_name);
        self.mono_state.type_subst = saved_subst;
        self.fn_ctx.loop_stack = saved_loop_stack;
        self.var_types.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_state.closure_fn_types = saved_cfn;
        self.closure_state.pending_closure_fn_type = saved_pct;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok((thunk_fn, env_alloca))
    }

    /// Recursive lex-cascade compare for a struct value. Walks `struct_name`'s
    /// fields in declaration order via `self.type_decls.struct_field_type_names`,
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

        let field_type_names = match self
            .type_decls
            .struct_field_type_names
            .get(struct_name)
            .cloned()
        {
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
                ) if self
                    .type_decls
                    .struct_field_type_names
                    .contains_key(nested_name) =>
                {
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
        elem_te: Option<&TypeExpr>,
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
        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
        let name = format!("__sort_by_inline_{}", id);
        let thunk_ty = i64_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let thunk_fn = self
            .module
            .add_function(&name, thunk_ty, Some(Linkage::Internal));

        // 4. Save outer codegen state — we're about to compile into a new fn.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_subst = std::mem::take(&mut self.mono_state.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_state.closure_fn_types);
        let saved_pct = self.closure_state.pending_closure_fn_type.take();
        // Clear the par-branch cancel pointer for the thunk body (B-2026-06-18-10):
        // the comparator is a separate fn, so a method call in it (e.g. `a.cmp(b)`)
        // must not load the enclosing par-branch's `cancel_flag` arg.
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();

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
                    self.var_types
                        .var_type_names
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
            // B-2026-08-10-13 — the name alone stops being enough once the
            // element is itself a CONTAINER. `record_var_type_name` serves
            // `compile_field_access`, which needs only a struct name; method
            // dispatch and index lowering instead look the param up in
            // `vec_elem_types` / `slice_elem_types` / `var_elem_type_exprs`,
            // and nothing ever registered those for a comparator param. So
            // `|x, y| x.len().cmp(y.len())` over a `Vec[Vec[i64]]` or
            // `Vec[String]` reached codegen with an untyped `x` and fell
            // through method dispatch, while `|a, b| a.0.cmp(b.0)` built —
            // tuple-field access needs no type lookup, which is why every
            // earlier `sort_by` in the corpus missed this.
            //
            // Registering from the full element `TypeExpr` covers the family
            // in one arm (Vec, String, Slice, nested), because it is the same
            // registrar an ordinary `let` binding goes through.
            //
            // Only the THUNK path needs this. Its sibling
            // `emit_sort_by_inline_compare` (the mono path) is gated by
            // `should_use_mono_vec_sort_by_for` to all-int-field elements, so
            // a container element never reaches it and the registration would
            // be dead code there.
            if let Some(te) = elem_te {
                self.register_var_from_type_expr(&param_name, te);
            }
            self.variables
                .insert(param_name, VarSlot { ptr: alloca, ty });
        }

        // 7. Compile body, transform Ordering result → signed `tag - 1`.
        let result = self.compile_sort_body_retargeting_return(thunk_fn, body)?;
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
        // B-2026-08-10-13 — tear down the comparator params' container
        // registrations. These names are thunk-local; leaving them behind
        // would let a param named `x` shadow an enclosing `x` of a different
        // shape for the rest of the function. Recomputed from `params` rather
        // than tracked, so it cannot drift from the binding loop above.
        for (i, cp) in params.iter().enumerate().take(2) {
            let n = match &cp.pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                _ => format!("_cp{}", i),
            };
            self.var_types.vec_elem_types.remove(&n);
            self.var_types.slice_elem_types.remove(&n);
            self.var_types.var_elem_type_exprs.remove(&n);
        }
        self.mono_state.type_subst = saved_subst;
        self.fn_ctx.loop_stack = saved_loop_stack;
        self.var_types.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_state.closure_fn_types = saved_cfn;
        self.closure_state.pending_closure_fn_type = saved_pct;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
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

    /// Bind the comparator closure's two params to `a_val` / `b_val`, compile
    /// its body inline, and return the signed `-1 / 0 / +1` comparison.
    ///
    /// The body returns either an `Ordering` struct `{ i64 tag }` (for
    /// `a.cmp(b)` shapes) or a bare `i64` (hand-rolled `if a < b { -1i64 }
    /// ...`); we extract the tag in the struct case and subtract 1, since
    /// `Less / Equal / Greater` tags are assigned in declaration order (see
    /// `declare_enums`). Factored out of `emit_sort_by_mono` because the
    /// merge sort compares in two places (the insertion base case and the
    /// merge) and the two must not drift.
    fn emit_sort_by_inline_compare(
        &mut self,
        host_fn: FunctionValue<'ctx>,
        params: &[ClosureParam],
        body: &Expr,
        elem_type_name: Option<&str>,
        a_val: BasicValueEnum<'ctx>,
        b_val: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let param_vals = [a_val, b_val];
        for (i, cp) in params.iter().enumerate().take(2) {
            let val = param_vals[i];
            let param_name = match &cp.pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                _ => format!("_cp{}", i),
            };
            let ty = val.get_type();
            let alloca = self.create_entry_alloca(host_fn, &param_name, ty);
            self.builder.build_store(alloca, val).unwrap();
            self.variables
                .insert(param_name.clone(), VarSlot { ptr: alloca, ty });
            if let Some(name) = elem_type_name {
                self.record_var_type_name(param_name, name.to_string());
            }
        }
        let result = self.compile_sort_body_retargeting_return(host_fn, body)?;
        let tag = if result.is_struct_value() {
            self.builder
                .build_extract_value(result.into_struct_value(), 0, "tag")
                .unwrap()
                .into_int_value()
        } else {
            result.into_int_value()
        };
        Ok(self
            .builder
            .build_int_sub(tag, i64_t.const_int(1, false), "cmp")
            .unwrap())
    }

    /// Per-call-site monomorphized sort function for
    /// `Vec[T].sort_by(inline_closure)`. Signature:
    /// `void __vec_<elem_mangle>_sort_by_mono_<id>(data: *mut T, len: i64)`
    /// (internal linkage). The user's comparator is inlined at every compare
    /// — no `karac_vec_sort_by` callback — so LLVM sees through both the sort
    /// algorithm and the comparison and can optimise them together.
    ///
    /// **Algorithm — stable NATURAL-RUN merge sort** (B-2026-08-10-9).
    /// Phase 1 splits the array into maximal already-sorted runs: at each
    /// position probe the first pair, extend an ascending run while
    /// `cmp <= 0` or a STRICTLY descending one while `cmp > 0` (reversing
    /// the latter in place), then pad anything shorter than `RUN` out to
    /// `RUN` by insertion sort. Run ends are recorded in a scratch `i64`
    /// list. Phase 2 merges adjacent runs pairwise — ping-ponging both the
    /// element buffers and the run lists — until one run remains, copying
    /// back at the end only if an odd number of passes left the live data in
    /// scratch. The merge takes the LEFT run on a tie (`cmp <= 0`), which is
    /// what makes it stable — required by design.md ("In-place stable
    /// sort"), which is also why heapsort/introsort are not options here.
    /// Reversing a descending run is stable precisely because the extension
    /// test is strict: such a run has no two elements that compare equal.
    ///
    /// Phase 2 ships **two merge kernels with identical output** and picks
    /// one per pass (B-2026-08-10-19): a branchy one and a branchless one,
    /// chosen by simulating a small branch predictor over the pass's first
    /// `PROBE` outputs. Shuffled input mispredicts ~13% and takes the
    /// branchless kernel; ordered-ish input mispredicts ~2% and keeps the
    /// branchy one, where speculation is already winning; inputs shorter
    /// than `4 * PROBE` skip the measurement and stay branchy, so a small
    /// sort runs exactly the code it ran before. The long comment above
    /// `p2.mode.chk` has the measurements and the reason a plain
    /// switch-rate counter is the wrong signal.
    ///
    /// **Why natural runs.** The previous shape (B-2026-07-30-2) insertion-
    /// sorted fixed 32-element runs and then merged at doubling widths, so
    /// it did `ceil(log2(n/32))` full passes over the array *regardless of
    /// input order* — 13 passes over 2.4 MB for 150k elements even when the
    /// input was already sorted. B-2026-08-10-9 measured that against Rust's
    /// driftsort across six input patterns: 2.1x on shuffled-uniform but
    /// **54x on sorted and 39x on reverse-sorted** input. Detecting runs
    /// makes those cases collapse to a single scan (one run ⇒ phase 2 is
    /// skipped entirely) while costing shuffled input nothing: every natural
    /// run there is ~2 elements, so the `RUN` padding reproduces exactly the
    /// old 32-element runs and the old pass count, plus one O(n) probe.
    ///
    /// That spike also established this is **not** a lowering-quality gap —
    /// hand-writing the identical algorithm and comparator shape in Rust put
    /// karac 5-8% *ahead* of rustc on shuffled input. B-2026-08-10-19 then
    /// found what the remaining shuffled-input gap actually is, and it is
    /// not the algorithm either: against driftsort karac executes 1.09x the
    /// instructions and 0.62x the data references with the same cache-miss
    /// profile, and loses only on branch mispredicts (4.56x), which is what
    /// the per-pass kernel choice above addresses. See
    /// `docs/spikes/sort-algorithm-gap.md`.
    ///
    /// Stability means the two backends still agree element-for-element with
    /// the runtime's `sort_fixed_width` even though they now run different
    /// algorithms — any two stable sorts produce the same permutation — so
    /// the runtime does not have to change in lockstep.
    ///
    /// **Element type parameterisation.** `elem_ty` flows through every
    /// load/store/GEP that touches the data or scratch buffer — i64 plus any
    /// LLVM struct whose fields are all integers (integer tuples and
    /// `#[derive(Ord)]` integer-field structs), per
    /// `should_use_mono_vec_sort_by_for`. Struct elems are moved as opaque
    /// `BasicValueEnum` loads/stores; the closure body's `.0` /
    /// `.field_name` access goes through `compile_expr`'s existing extract
    /// path, which is why `elem_type_name` is threaded in.
    ///
    /// **Allocation failure is a no-op sort**, matching the runtime helper:
    /// a null scratch or run list returns with the buffer untouched rather
    /// than panicking, keeping this path free of any reachable panic so the
    /// ~262 KiB DWARF symbolizer stays dead-strippable. The three
    /// allocations unwind through paired bail blocks so a later failure
    /// frees the earlier buffers.
    ///
    /// **Captures unsupported.** The caller gates entry on
    /// `collect_closure_free_vars` being empty; capturing comparators fall
    /// through to the thunk path.
    #[allow(clippy::too_many_lines)]
    pub(super) fn emit_sort_by_mono(
        &mut self,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_type_name: Option<&str>,
    ) -> Result<FunctionValue<'ctx>, String> {
        /// Insertion-sort base run length. Matches the runtime's
        /// `sort_fixed_width::RUN` so the two backends do the same work.
        const RUN: u64 = 32;
        /// Outputs each phase-2 pass spends measuring merge predictability
        /// before committing that pass to the branchy or the branchless
        /// merge kernel (B-2026-08-10-19). A pass emits ~`len` outputs, so
        /// at the `4 * PROBE` length gate the simulation touches at most a
        /// quarter of a pass and at 150k elements 0.17% of the sort;
        /// shorter inputs skip it entirely and keep the pre-existing path.
        const PROBE: u64 = 256;
        /// Below this length the probe is not worth running, and keeping it at
        /// or above the partition's own leaf `SPAN` is what guarantees a
        /// handed-back range can never re-enter the partition path.
        const PART_MIN: u64 = 4096;

        let i64_t = self.context.i64_type();
        let void_t = self.context.void_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // Declare per-call-site mono fn. Internal linkage — each call site
        // emits a fresh copy (the closure body varies per site, so
        // LinkOnceODR would risk silent body-mismatch across TUs sharing a
        // counter id). The elem-type token keeps mono symbols textually
        // distinct when counter ids collide across TUs.
        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
        let elem_mangle = self.llvm_type_to_mangle_str(elem_ty);
        let name = format!("__vec_{}_sort_by_mono_{}", elem_mangle, id);
        // Third parameter: `allow_part`. Call sites pass 1; the partition
        // helper passes 0 when it hands a range back, which is what stops an
        // abandoned range from being re-probed and bounced straight back.
        let fn_ty = void_t.fn_type(&[ptr_ty.into(), i64_t.into(), i64_t.into()], false);
        let sort_fn = self
            .module
            .add_function(&name, fn_ty, Some(Linkage::Internal));
        // Declared here, defined after this function's body: the two are
        // mutually recursive, so the declaration has to exist first.
        let qpart_name = format!("__vec_{}_qpart_{}", elem_mangle, id);
        // Seven params: (data, scratch, lo, hi, in_a, depth, gate). `gate`
        // carries WHICH probe arm admitted the range, because the two arms want
        // opposite per-range behaviour (B-2026-08-15-30):
        //   1  low-cardinality — keep the per-range tie test, so a mixed input
        //      partitions only the part that pays
        //   0  unstructured    — the range was admitted BECAUSE its keys are
        //      spread, so the tie test would reject every range and fall
        //      straight back to the merge, having paid a counting pass for
        //      nothing
        let qpart_ty = void_t.fn_type(
            &[
                ptr_ty.into(),
                ptr_ty.into(),
                i64_t.into(),
                i64_t.into(),
                i64_t.into(),
                i64_t.into(),
                i64_t.into(),
            ],
            false,
        );
        let qpart_fn = self
            .module
            .add_function(&qpart_name, qpart_ty, Some(Linkage::Internal));
        // The entry probe lives in its OWN function rather than inline here.
        // Measured: inlining the user comparator two more times into this
        // function cost sawtooth 22.5M -> 23.4M and nearly-sorted 31.5M ->
        // 32.9M — patterns that never take the partition path at all. The
        // probe itself is under 0.05M (sorted and reverse, whose whole sort is
        // 1.0M and 1.8M, did not move); the loss was phase 2 being compiled
        // differently in a bigger function.
        let probe_name = format!("__vec_{}_sprobe_{}", elem_mangle, id);
        let probe_ty = i64_t.fn_type(&[ptr_ty.into(), i64_t.into()], false);
        let probe_fn = self
            .module
            .add_function(&probe_name, probe_ty, Some(Linkage::Internal));
        // The partition's leaf sorter (B-2026-08-16-3). Its own function for the
        // same reason the probe is: it inlines the user comparator once more,
        // and folding that into `qpart` compiles the partition's own two loops
        // differently. `void isort(data, len)` sorts `[0,len)` in place.
        let isort_name = format!("__vec_{}_isort_{}", elem_mangle, id);
        let isort_ty = void_t.fn_type(&[ptr_ty.into(), i64_t.into()], false);
        let isort_fn = self
            .module
            .add_function(&isort_name, isort_ty, Some(Linkage::Internal));

        // Save outer codegen state — we're about to compile into a new fn.
        // Same save/restore dance as `emit_sort_by_inline_thunk`.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_subst = std::mem::take(&mut self.mono_state.type_subst);
        let saved_cfn = std::mem::take(&mut self.closure_state.closure_fn_types);
        let saved_pct = self.closure_state.pending_closure_fn_type.take();
        // Clear the par-branch cancel pointer for the mono sort body
        // (B-2026-06-18-10): the comparison this routine inlines must not
        // load the enclosing par-branch's `cancel_flag` arg — `%1` here is
        // the i64 length, not a cancel pointer.
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();

        self.current_fn = Some(sort_fn);

        let data = sort_fn.get_nth_param(0).unwrap().into_pointer_value();
        let len = sort_fn.get_nth_param(1).unwrap().into_int_value();

        let zero = i64_t.const_zero();
        let one = i64_t.const_int(1, false);
        let two = i64_t.const_int(2, false);
        let run_c = i64_t.const_int(RUN, false);
        let probe_c = i64_t.const_int(PROBE, false);
        // Below this length the probe is not worth running — see `p2.pass`.
        let probe_min_c = i64_t.const_int(PROBE * 4, false);
        let five = i64_t.const_int(5, false);
        // 4 bits of take history — see the `p2.mode.chk` comment for why the
        // width is load-bearing rather than arbitrary.
        let hist_mask = i64_t.const_int(15, false);

        // ── Block plan ─────────────────────────────────────────────────────
        let entry = self.context.append_basic_block(sort_fn, "entry");
        let do_alloc = self.context.append_basic_block(sort_fn, "do.alloc");
        let alloc_runs = self.context.append_basic_block(sort_fn, "alloc.runs");
        let alloc_runs2 = self.context.append_basic_block(sort_fn, "alloc.runs2");
        let alloc_ok = self.context.append_basic_block(sort_fn, "alloc.ok");
        let bail_runs = self.context.append_basic_block(sort_fn, "bail.runs");
        let bail_scratch = self.context.append_basic_block(sort_fn, "bail.scratch");
        // Phase 1 — natural-run detection, then MIN_RUN extension.
        let p1_chk = self.context.append_basic_block(sort_fn, "p1.chk");
        let p1_body = self.context.append_basic_block(sort_fn, "p1.body");
        let p1_dir = self.context.append_basic_block(sort_fn, "p1.dir");
        let p1_desc_chk = self.context.append_basic_block(sort_fn, "p1.desc.chk");
        let p1_desc_cmp = self.context.append_basic_block(sort_fn, "p1.desc.cmp");
        let p1_desc_body = self.context.append_basic_block(sort_fn, "p1.desc.body");
        let p1_rev_init = self.context.append_basic_block(sort_fn, "p1.rev.init");
        let p1_rev_chk = self.context.append_basic_block(sort_fn, "p1.rev.chk");
        let p1_rev_body = self.context.append_basic_block(sort_fn, "p1.rev.body");
        let p1_asc_chk = self.context.append_basic_block(sort_fn, "p1.asc.chk");
        let p1_asc_cmp = self.context.append_basic_block(sort_fn, "p1.asc.cmp");
        let p1_asc_body = self.context.append_basic_block(sort_fn, "p1.asc.body");
        let p1_ext = self.context.append_basic_block(sort_fn, "p1.ext");
        let p1_ins_chk = self.context.append_basic_block(sort_fn, "p1.ins.chk");
        let p1_ins_body = self.context.append_basic_block(sort_fn, "p1.ins.body");
        let p1_j_chk = self.context.append_basic_block(sort_fn, "p1.j.chk");
        let p1_j_cmp = self.context.append_basic_block(sort_fn, "p1.j.cmp");
        let p1_j_shift = self.context.append_basic_block(sort_fn, "p1.j.shift");
        let p1_j_done = self.context.append_basic_block(sort_fn, "p1.j.done");
        let p1_record = self.context.append_basic_block(sort_fn, "p1.record");
        let p1_done = self.context.append_basic_block(sort_fn, "p1.done");
        // Phase 2 — pairwise merge over the run list.
        let p2_chk = self.context.append_basic_block(sort_fn, "p2.chk");
        let p2_pass = self.context.append_basic_block(sort_fn, "p2.pass");
        let p2_i_chk = self.context.append_basic_block(sort_fn, "p2.i.chk");
        let p2_pair_chk = self.context.append_basic_block(sort_fn, "p2.pair.chk");
        let p2_merge_init = self.context.append_basic_block(sort_fn, "p2.merge.init");
        // Per-pass merge-kernel selection: probe, decide, then dispatch to
        // either the branchy kernel (`p2.m.*`) or the branchless one
        // (`p2.bl.*`).
        let p2_mode_chk = self.context.append_basic_block(sort_fn, "p2.mode.chk");
        let p2_mode_pick = self.context.append_basic_block(sort_fn, "p2.mode.pick");
        let p2_pr_chk_a = self.context.append_basic_block(sort_fn, "p2.pr.chk.a");
        let p2_pr_chk_b = self.context.append_basic_block(sort_fn, "p2.pr.chk.b");
        let p2_pr_cmp = self.context.append_basic_block(sort_fn, "p2.pr.cmp");
        let p2_pr_take_a = self.context.append_basic_block(sort_fn, "p2.pr.take.a");
        let p2_pr_take_b = self.context.append_basic_block(sort_fn, "p2.pr.take.b");
        let p2_pr_decide = self.context.append_basic_block(sort_fn, "p2.pr.decide");
        let p2_bl_chk_a = self.context.append_basic_block(sort_fn, "p2.bl.chk.a");
        let p2_bl_chk_b = self.context.append_basic_block(sort_fn, "p2.bl.chk.b");
        let p2_bl_body = self.context.append_basic_block(sort_fn, "p2.bl.body");
        let p2_m_chk_a = self.context.append_basic_block(sort_fn, "p2.m.chk.a");
        let p2_m_chk_b = self.context.append_basic_block(sort_fn, "p2.m.chk.b");
        let p2_m_cmp = self.context.append_basic_block(sort_fn, "p2.m.cmp");
        let p2_m_take_a = self.context.append_basic_block(sort_fn, "p2.m.take.a");
        let p2_m_take_b = self.context.append_basic_block(sort_fn, "p2.m.take.b");
        let p2_da_chk = self.context.append_basic_block(sort_fn, "p2.da.chk");
        let p2_da_body = self.context.append_basic_block(sort_fn, "p2.da.body");
        let p2_db_chk = self.context.append_basic_block(sort_fn, "p2.db.chk");
        let p2_db_body = self.context.append_basic_block(sort_fn, "p2.db.body");
        let p2_pair_done = self.context.append_basic_block(sort_fn, "p2.pair.done");
        let p2_tail = self.context.append_basic_block(sort_fn, "p2.tail");
        let p2_pass_end = self.context.append_basic_block(sort_fn, "p2.pass.end");
        let p2_done = self.context.append_basic_block(sort_fn, "p2.done");
        // Entry probe — the O(1) cardinality/orderedness sample that decides
        // whether the partition path is worth trying at all (B-2026-08-11-10
        // § Direction 7).
        let pt_chk = self.context.append_basic_block(sort_fn, "pt.chk");
        let pt_call = self.context.append_basic_block(sort_fn, "pt.call");
        let pt_go = self.context.append_basic_block(sort_fn, "pt.go");
        let copy_back = self.context.append_basic_block(sort_fn, "copy.back");
        let fini = self.context.append_basic_block(sort_fn, "fini");
        let exit = self.context.append_basic_block(sort_fn, "exit");

        // ── entry: bail on len < 2, allocate scratch + the two run lists ────
        self.builder.position_at_end(entry);
        let start_a = self.create_entry_alloca(sort_fn, "start", i64_t.into());
        let e_a = self.create_entry_alloca(sort_fn, "e", i64_t.into());
        let lim_a = self.create_entry_alloca(sort_fn, "lim", i64_t.into());
        let jj_a = self.create_entry_alloca(sort_fn, "jj", i64_t.into());
        let hold_a = self.create_entry_alloca(sort_fn, "hold", elem_ty);
        let ra_a = self.create_entry_alloca(sort_fn, "ra", i64_t.into());
        let rb_a = self.create_entry_alloca(sort_fn, "rb", i64_t.into());
        let nr_a = self.create_entry_alloca(sort_fn, "nr", i64_t.into());
        let onr_a = self.create_entry_alloca(sort_fn, "onr", i64_t.into());
        let ii_a = self.create_entry_alloca(sort_fn, "i", i64_t.into());
        let lo_a = self.create_entry_alloca(sort_fn, "lo", i64_t.into());
        let mid_a = self.create_entry_alloca(sort_fn, "mid", i64_t.into());
        let hi_a = self.create_entry_alloca(sort_fn, "hi", i64_t.into());
        let aa_a = self.create_entry_alloca(sort_fn, "a", i64_t.into());
        let bb_a = self.create_entry_alloca(sort_fn, "b", i64_t.into());
        let kk_a = self.create_entry_alloca(sort_fn, "k", i64_t.into());
        let src_a = self.create_entry_alloca(sort_fn, "src", ptr_ty.into());
        let dst_a = self.create_entry_alloca(sort_fn, "dst", ptr_ty.into());
        let runs_a = self.create_entry_alloca(sort_fn, "runs", ptr_ty.into());
        let runs2_a = self.create_entry_alloca(sort_fn, "runs2", ptr_ty.into());
        // Per-pass merge-kernel selection state (B-2026-08-10-19).
        let blmode_a = self.create_entry_alloca(sort_fn, "blmode", i64_t.into());
        let prleft_a = self.create_entry_alloca(sort_fn, "prleft", i64_t.into());
        let prmiss_a = self.create_entry_alloca(sort_fn, "prmiss", i64_t.into());
        let prtot_a = self.create_entry_alloca(sort_fn, "prtot", i64_t.into());
        let prhist_a = self.create_entry_alloca(sort_fn, "prhist", i64_t.into());
        let prtab_a = self.create_entry_alloca(sort_fn, "prtab", i64_t.into());

        let len_lt_2 = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, len, two, "len.lt2")
            .unwrap();
        let elem_size = elem_ty.size_of().unwrap();
        let total_bytes = self
            .builder
            .build_int_mul(len, elem_size, "total.bytes")
            .unwrap();
        // Run-list capacity. Every run but the last is >= RUN long (short
        // natural runs are extended by insertion below), so the count is
        // bounded by len/RUN + 1; +2 gives slack for the trailing partial.
        let runs_cap = {
            let q = self
                .builder
                .build_int_signed_div(len, run_c, "runs.q")
                .unwrap();
            self.builder
                .build_int_add(q, i64_t.const_int(2, false), "runs.cap")
                .unwrap()
        };
        let runs_bytes = self
            .builder
            .build_int_mul(runs_cap, i64_t.const_int(8, false), "runs.bytes")
            .unwrap();
        let malloc_fn = {
            let sym = crate::codegen::driver::c_malloc_symbol();
            self.module.get_function(sym).unwrap_or_else(|| {
                let mty = ptr_ty.fn_type(&[i64_t.into()], false);
                self.module.add_function(sym, mty, Some(Linkage::External))
            })
        };
        let free_fn = self.module.get_function("free").unwrap_or_else(|| {
            let free_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
            self.module
                .add_function("free", free_ty, Some(Linkage::External))
        });
        self.builder
            .build_conditional_branch(len_lt_2, exit, do_alloc)
            .unwrap();

        self.builder.position_at_end(do_alloc);
        let scratch = self
            .builder
            .build_call(malloc_fn, &[total_bytes.into()], "scratch")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let scratch_null = self.builder.build_is_null(scratch, "scratch.null").unwrap();
        self.builder
            .build_conditional_branch(scratch_null, exit, alloc_runs)
            .unwrap();

        self.builder.position_at_end(alloc_runs);
        let runs0 = self
            .builder
            .build_call(malloc_fn, &[runs_bytes.into()], "runs0")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let runs0_null = self.builder.build_is_null(runs0, "runs0.null").unwrap();
        self.builder
            .build_conditional_branch(runs0_null, bail_scratch, alloc_runs2)
            .unwrap();

        self.builder.position_at_end(alloc_runs2);
        let runs1 = self
            .builder
            .build_call(malloc_fn, &[runs_bytes.into()], "runs1")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let runs1_null = self.builder.build_is_null(runs1, "runs1.null").unwrap();
        self.builder
            .build_conditional_branch(runs1_null, bail_runs, alloc_ok)
            .unwrap();

        // Allocation failure sorts nothing rather than panicking — mirrors
        // the runtime helper, and keeps this path free of any reachable
        // panic so the DWARF symbolizer stays dead-strippable.
        self.builder.position_at_end(bail_runs);
        self.builder
            .build_call(free_fn, &[runs0.into()], "")
            .unwrap();
        self.builder
            .build_unconditional_branch(bail_scratch)
            .unwrap();

        self.builder.position_at_end(bail_scratch);
        self.builder
            .build_call(free_fn, &[scratch.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(exit).unwrap();

        self.builder.position_at_end(alloc_ok);
        self.builder.build_store(runs_a, runs0).unwrap();
        self.builder.build_store(runs2_a, runs1).unwrap();
        self.builder.build_store(start_a, zero).unwrap();
        self.builder.build_store(nr_a, zero).unwrap();
        self.builder.build_unconditional_branch(pt_chk).unwrap();

        // ── Entry probe: is the partition path worth trying? ────────────────
        // Out of line, in `__vec_<m>_sprobe_<id>` — see the declaration above
        // for the measurement that put it there.
        self.builder.position_at_end(pt_chk);
        let allow_part = sort_fn.get_nth_param(2).unwrap().into_int_value();
        let allow_b = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, allow_part, zero, "pt.allow")
            .unwrap();
        let big = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGT,
                len,
                i64_t.const_int(PART_MIN, false),
                "pt.big",
            )
            .unwrap();
        let try_part = self.builder.build_and(allow_b, big, "pt.try").unwrap();
        self.builder
            .build_conditional_branch(try_part, pt_call, p1_chk)
            .unwrap();

        self.builder.position_at_end(pt_call);
        let probe_r = self
            .builder
            .build_call(probe_fn, &[data.into(), len.into()], "pt.r")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        // The probe answers with the ARM, not a bool: 0 reject, 1 admitted on
        // low cardinality, 2 admitted on being unstructured. `gate` is 1 only
        // for arm 1 — see the qpart declaration for why the arms differ.
        let accept = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, probe_r, zero, "pt.accept")
            .unwrap();
        let gate_b = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, probe_r, one, "pt.gate.b")
            .unwrap();
        let gate_v = self
            .builder
            .build_int_z_extend(gate_b, i64_t, "pt.gate")
            .unwrap();
        self.builder
            .build_conditional_branch(accept, pt_go, p1_chk)
            .unwrap();

        // The partition borrows the scratch phase 2 already allocated, so it
        // needs nothing of its own, and `fini` frees all three buffers on the
        // way out exactly as the merge path does.
        self.builder.position_at_end(pt_go);
        self.builder
            .build_call(
                qpart_fn,
                &[
                    data.into(),
                    scratch.into(),
                    zero.into(),
                    len.into(),
                    one.into(),
                    zero.into(),
                    gate_v.into(),
                ],
                "",
            )
            .unwrap();
        self.builder.build_unconditional_branch(fini).unwrap();

        // ── Phase 1: split the array into maximal sorted runs ───────────────
        // `while start < len`
        self.builder.position_at_end(p1_chk);
        let start_v = self
            .builder
            .build_load(i64_t, start_a, "start.v")
            .unwrap()
            .into_int_value();
        let p1_cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, start_v, len, "p1.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(p1_cond, p1_body, p1_done)
            .unwrap();

        // e = start + 1; a lone trailing element needs no direction probe.
        self.builder.position_at_end(p1_body);
        let start_v1 = self
            .builder
            .build_load(i64_t, start_a, "start.v1")
            .unwrap()
            .into_int_value();
        let e_init = self.builder.build_int_add(start_v1, one, "e.init").unwrap();
        self.builder.build_store(e_a, e_init).unwrap();
        let has_pair = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, e_init, len, "has.pair")
            .unwrap();
        self.builder
            .build_conditional_branch(has_pair, p1_dir, p1_ext)
            .unwrap();

        // Descending run iff `cmp(data[start], data[start+1]) > 0` — STRICT,
        // which is what keeps the reversal below stable: a strictly
        // descending run has no two elements that compare equal, so
        // reversing it cannot reorder equal elements.
        self.builder.position_at_end(p1_dir);
        let d0_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[start_v1], "d0.addr")
                .unwrap()
        };
        let d1_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[e_init], "d1.addr")
                .unwrap()
        };
        let d0_v = self.builder.build_load(elem_ty, d0_addr, "d0").unwrap();
        let d1_v = self.builder.build_load(elem_ty, d1_addr, "d1").unwrap();
        let c_dir =
            self.emit_sort_by_inline_compare(sort_fn, params, body, elem_type_name, d0_v, d1_v)?;
        let is_desc = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, c_dir, zero, "is.desc")
            .unwrap();
        self.builder
            .build_conditional_branch(is_desc, p1_desc_chk, p1_asc_chk)
            .unwrap();

        // Descending: extend while strictly descending, then reverse in place.
        self.builder.position_at_end(p1_desc_chk);
        let de_v = self
            .builder
            .build_load(i64_t, e_a, "de.v")
            .unwrap()
            .into_int_value();
        let de_in = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, de_v, len, "de.in")
            .unwrap();
        self.builder
            .build_conditional_branch(de_in, p1_desc_cmp, p1_rev_init)
            .unwrap();

        self.builder.position_at_end(p1_desc_cmp);
        let de_v1 = self
            .builder
            .build_load(i64_t, e_a, "de.v1")
            .unwrap()
            .into_int_value();
        let de_m1 = self.builder.build_int_sub(de_v1, one, "de.m1").unwrap();
        let dp_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[de_m1], "dp.addr")
                .unwrap()
        };
        let dc_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[de_v1], "dc.addr")
                .unwrap()
        };
        let dp_v = self.builder.build_load(elem_ty, dp_addr, "dp").unwrap();
        let dc_v = self.builder.build_load(elem_ty, dc_addr, "dc").unwrap();
        let c_desc =
            self.emit_sort_by_inline_compare(sort_fn, params, body, elem_type_name, dp_v, dc_v)?;
        let desc_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, c_desc, zero, "desc.go")
            .unwrap();
        self.builder
            .build_conditional_branch(desc_go, p1_desc_body, p1_rev_init)
            .unwrap();

        self.builder.position_at_end(p1_desc_body);
        let de_v2 = self
            .builder
            .build_load(i64_t, e_a, "de.v2")
            .unwrap()
            .into_int_value();
        let de_next = self.builder.build_int_add(de_v2, one, "de.next").unwrap();
        self.builder.build_store(e_a, de_next).unwrap();
        self.builder
            .build_unconditional_branch(p1_desc_chk)
            .unwrap();

        self.builder.position_at_end(p1_rev_init);
        let rs_v = self
            .builder
            .build_load(i64_t, start_a, "rs.v")
            .unwrap()
            .into_int_value();
        let re_v = self
            .builder
            .build_load(i64_t, e_a, "re.v")
            .unwrap()
            .into_int_value();
        let re_m1 = self.builder.build_int_sub(re_v, one, "re.m1").unwrap();
        self.builder.build_store(ra_a, rs_v).unwrap();
        self.builder.build_store(rb_a, re_m1).unwrap();
        self.builder.build_unconditional_branch(p1_rev_chk).unwrap();

        self.builder.position_at_end(p1_rev_chk);
        let ra_v = self
            .builder
            .build_load(i64_t, ra_a, "ra.v")
            .unwrap()
            .into_int_value();
        let rb_v = self
            .builder
            .build_load(i64_t, rb_a, "rb.v")
            .unwrap()
            .into_int_value();
        let rev_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, ra_v, rb_v, "rev.go")
            .unwrap();
        self.builder
            .build_conditional_branch(rev_go, p1_rev_body, p1_ext)
            .unwrap();

        self.builder.position_at_end(p1_rev_body);
        let ra_v1 = self
            .builder
            .build_load(i64_t, ra_a, "ra.v1")
            .unwrap()
            .into_int_value();
        let rb_v1 = self
            .builder
            .build_load(i64_t, rb_a, "rb.v1")
            .unwrap()
            .into_int_value();
        let pa_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[ra_v1], "pa.addr")
                .unwrap()
        };
        let pb_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[rb_v1], "pb.addr")
                .unwrap()
        };
        let pa_v = self.builder.build_load(elem_ty, pa_addr, "pa.v").unwrap();
        let pb_v = self.builder.build_load(elem_ty, pb_addr, "pb.v").unwrap();
        self.builder.build_store(pa_addr, pb_v).unwrap();
        self.builder.build_store(pb_addr, pa_v).unwrap();
        let ra_next = self.builder.build_int_add(ra_v1, one, "ra.next").unwrap();
        let rb_next = self.builder.build_int_sub(rb_v1, one, "rb.next").unwrap();
        self.builder.build_store(ra_a, ra_next).unwrap();
        self.builder.build_store(rb_a, rb_next).unwrap();
        self.builder.build_unconditional_branch(p1_rev_chk).unwrap();

        // Ascending: extend while non-descending (`<= 0` keeps equal elements
        // in their original order, so the run stays stable).
        self.builder.position_at_end(p1_asc_chk);
        let ae_v = self
            .builder
            .build_load(i64_t, e_a, "ae.v")
            .unwrap()
            .into_int_value();
        let ae_in = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, ae_v, len, "ae.in")
            .unwrap();
        self.builder
            .build_conditional_branch(ae_in, p1_asc_cmp, p1_ext)
            .unwrap();

        self.builder.position_at_end(p1_asc_cmp);
        let ae_v1 = self
            .builder
            .build_load(i64_t, e_a, "ae.v1")
            .unwrap()
            .into_int_value();
        let ae_m1 = self.builder.build_int_sub(ae_v1, one, "ae.m1").unwrap();
        let ap_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[ae_m1], "ap.addr")
                .unwrap()
        };
        let ac_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[ae_v1], "ac.addr")
                .unwrap()
        };
        let ap_v = self.builder.build_load(elem_ty, ap_addr, "ap").unwrap();
        let ac_v = self.builder.build_load(elem_ty, ac_addr, "ac").unwrap();
        let c_asc =
            self.emit_sort_by_inline_compare(sort_fn, params, body, elem_type_name, ap_v, ac_v)?;
        let asc_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, c_asc, zero, "asc.go")
            .unwrap();
        self.builder
            .build_conditional_branch(asc_go, p1_asc_body, p1_ext)
            .unwrap();

        self.builder.position_at_end(p1_asc_body);
        let ae_v2 = self
            .builder
            .build_load(i64_t, e_a, "ae.v2")
            .unwrap()
            .into_int_value();
        let ae_next = self.builder.build_int_add(ae_v2, one, "ae.next").unwrap();
        self.builder.build_store(e_a, ae_next).unwrap();
        self.builder.build_unconditional_branch(p1_asc_chk).unwrap();

        // Pad a short natural run out to RUN elements by insertion sort, so
        // random input still yields RUN-sized runs (and thus exactly the old
        // pass count) instead of degenerating to runs of 1-2.
        self.builder.position_at_end(p1_ext);
        let xs_v = self
            .builder
            .build_load(i64_t, start_a, "xs.v")
            .unwrap()
            .into_int_value();
        let xs_run = self.builder.build_int_add(xs_v, run_c, "xs.run").unwrap();
        let lim_fits = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, xs_run, len, "lim.fits")
            .unwrap();
        let lim_v = self
            .builder
            .build_select(lim_fits, xs_run, len, "lim.v")
            .unwrap()
            .into_int_value();
        self.builder.build_store(lim_a, lim_v).unwrap();
        self.builder.build_unconditional_branch(p1_ins_chk).unwrap();

        self.builder.position_at_end(p1_ins_chk);
        let ie_v = self
            .builder
            .build_load(i64_t, e_a, "ie.v")
            .unwrap()
            .into_int_value();
        let ilim_v = self
            .builder
            .build_load(i64_t, lim_a, "ilim.v")
            .unwrap()
            .into_int_value();
        let ins_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, ie_v, ilim_v, "ins.go")
            .unwrap();
        self.builder
            .build_conditional_branch(ins_go, p1_ins_body, p1_record)
            .unwrap();

        self.builder.position_at_end(p1_ins_body);
        let ie_v1 = self
            .builder
            .build_load(i64_t, e_a, "ie.v1")
            .unwrap()
            .into_int_value();
        let hold_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[ie_v1], "hold.addr")
                .unwrap()
        };
        let hold_v = self
            .builder
            .build_load(elem_ty, hold_addr, "hold.load")
            .unwrap();
        self.builder.build_store(hold_a, hold_v).unwrap();
        self.builder.build_store(jj_a, ie_v1).unwrap();
        self.builder.build_unconditional_branch(p1_j_chk).unwrap();

        self.builder.position_at_end(p1_j_chk);
        let jj_v = self
            .builder
            .build_load(i64_t, jj_a, "jj.v")
            .unwrap()
            .into_int_value();
        let jstart_v = self
            .builder
            .build_load(i64_t, start_a, "jstart.v")
            .unwrap()
            .into_int_value();
        let j_cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, jj_v, jstart_v, "p1.j.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(j_cond, p1_j_cmp, p1_j_done)
            .unwrap();

        self.builder.position_at_end(p1_j_cmp);
        let jj_v1 = self
            .builder
            .build_load(i64_t, jj_a, "jj.v1")
            .unwrap()
            .into_int_value();
        let jm1 = self.builder.build_int_sub(jj_v1, one, "jj.m1").unwrap();
        let prev_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jm1], "prev.addr")
                .unwrap()
        };
        let prev_v = self
            .builder
            .build_load(elem_ty, prev_addr, "prev.load")
            .unwrap();
        let hold_v1 = self.builder.build_load(elem_ty, hold_a, "hold.v1").unwrap();
        let c1 = self.emit_sort_by_inline_compare(
            sort_fn,
            params,
            body,
            elem_type_name,
            prev_v,
            hold_v1,
        )?;
        let c1_gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, c1, zero, "p1.cmp.gt")
            .unwrap();
        self.builder
            .build_conditional_branch(c1_gt, p1_j_shift, p1_j_done)
            .unwrap();

        self.builder.position_at_end(p1_j_shift);
        let jj_v2 = self
            .builder
            .build_load(i64_t, jj_a, "jj.v2")
            .unwrap()
            .into_int_value();
        let jm1b = self.builder.build_int_sub(jj_v2, one, "jj.m1b").unwrap();
        let src_e = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jm1b], "shift.src")
                .unwrap()
        };
        let src_v = self
            .builder
            .build_load(elem_ty, src_e, "shift.load")
            .unwrap();
        let dst_e = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jj_v2], "shift.dst")
                .unwrap()
        };
        self.builder.build_store(dst_e, src_v).unwrap();
        self.builder.build_store(jj_a, jm1b).unwrap();
        self.builder.build_unconditional_branch(p1_j_chk).unwrap();

        self.builder.position_at_end(p1_j_done);
        let jj_v3 = self
            .builder
            .build_load(i64_t, jj_a, "jj.v3")
            .unwrap()
            .into_int_value();
        let hold_v2 = self.builder.build_load(elem_ty, hold_a, "hold.v2").unwrap();
        let land = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jj_v3], "hold.dst")
                .unwrap()
        };
        self.builder.build_store(land, hold_v2).unwrap();
        let ie_v2 = self
            .builder
            .build_load(i64_t, e_a, "ie.v2")
            .unwrap()
            .into_int_value();
        let ie_next = self.builder.build_int_add(ie_v2, one, "ie.next").unwrap();
        self.builder.build_store(e_a, ie_next).unwrap();
        self.builder.build_unconditional_branch(p1_ins_chk).unwrap();

        // runs[nr++] = e; start = e
        self.builder.position_at_end(p1_record);
        let rec_e = self
            .builder
            .build_load(i64_t, e_a, "rec.e")
            .unwrap()
            .into_int_value();
        let rec_nr = self
            .builder
            .build_load(i64_t, nr_a, "rec.nr")
            .unwrap()
            .into_int_value();
        let rec_runs = self
            .builder
            .build_load(ptr_ty, runs_a, "rec.runs")
            .unwrap()
            .into_pointer_value();
        let rec_slot = unsafe {
            self.builder
                .build_in_bounds_gep(i64_t, rec_runs, &[rec_nr], "rec.slot")
                .unwrap()
        };
        self.builder.build_store(rec_slot, rec_e).unwrap();
        let nr_next = self.builder.build_int_add(rec_nr, one, "nr.next").unwrap();
        self.builder.build_store(nr_a, nr_next).unwrap();
        self.builder.build_store(start_a, rec_e).unwrap();
        self.builder.build_unconditional_branch(p1_chk).unwrap();

        // ── Phase 2: merge adjacent runs pairwise until one remains ─────────
        // An already-sorted input yields nr == 1 here, so this whole phase is
        // skipped and the sort costs one scan — that is the adaptivity the
        // old fixed-width loop lacked.
        self.builder.position_at_end(p1_done);
        self.builder.build_store(src_a, data).unwrap();
        self.builder.build_store(dst_a, scratch).unwrap();
        self.builder.build_unconditional_branch(p2_chk).unwrap();

        self.builder.position_at_end(p2_chk);
        let nr_v = self
            .builder
            .build_load(i64_t, nr_a, "nr.v")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, nr_v, one, "p2.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, p2_pass, p2_done)
            .unwrap();

        self.builder.position_at_end(p2_pass);
        self.builder.build_store(ii_a, zero).unwrap();
        self.builder.build_store(onr_a, zero).unwrap();
        self.builder.build_store(lo_a, zero).unwrap();
        // Re-arm the kernel probe for this pass. The decision is per-pass,
        // not per-merge and not global: run character changes as the passes
        // proceed (few-unique input looks shuffled at pass 1 and blocky by
        // pass 8, once each run holds long stretches of one key), and the
        // per-merge alternative could not amortise the simulation over the
        // 4687 tiny merges that make up pass 1.
        //
        // Short inputs skip the probe entirely: a pass emits about `len`
        // outputs, so below `4 * PROBE` the simulation stops being amortised
        // and starts being a tax on the common case of sorting a small Vec.
        // A zero budget lands straight in `p2.pr.decide` with `prtot == 0`,
        // which the STRICT threshold there resolves to the branchy kernel —
        // so a short sort runs exactly the code it ran before this change.
        self.builder.build_store(blmode_a, zero).unwrap();
        let want_probe = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGE, len, probe_min_c, "want.probe")
            .unwrap();
        let probe_budget = self
            .builder
            .build_select(want_probe, probe_c, zero, "probe.budget")
            .unwrap();
        self.builder.build_store(prleft_a, probe_budget).unwrap();
        self.builder.build_store(prmiss_a, zero).unwrap();
        self.builder.build_store(prtot_a, zero).unwrap();
        self.builder.build_store(prhist_a, zero).unwrap();
        self.builder.build_store(prtab_a, zero).unwrap();
        self.builder.build_unconditional_branch(p2_i_chk).unwrap();

        self.builder.position_at_end(p2_i_chk);
        let i_v = self
            .builder
            .build_load(i64_t, ii_a, "i.v")
            .unwrap()
            .into_int_value();
        let nr_v1 = self
            .builder
            .build_load(i64_t, nr_a, "nr.v1")
            .unwrap()
            .into_int_value();
        let i_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i_v, nr_v1, "i.go")
            .unwrap();
        self.builder
            .build_conditional_branch(i_go, p2_pair_chk, p2_pass_end)
            .unwrap();

        self.builder.position_at_end(p2_pair_chk);
        let i_v1 = self
            .builder
            .build_load(i64_t, ii_a, "i.v1")
            .unwrap()
            .into_int_value();
        let nr_v2 = self
            .builder
            .build_load(i64_t, nr_a, "nr.v2")
            .unwrap()
            .into_int_value();
        let i_p1 = self.builder.build_int_add(i_v1, one, "i.p1").unwrap();
        let has_partner = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i_p1, nr_v2, "has.partner")
            .unwrap();
        self.builder
            .build_conditional_branch(has_partner, p2_merge_init, p2_tail)
            .unwrap();

        self.builder.position_at_end(p2_merge_init);
        let mi_runs = self
            .builder
            .build_load(ptr_ty, runs_a, "mi.runs")
            .unwrap()
            .into_pointer_value();
        let mi_i = self
            .builder
            .build_load(i64_t, ii_a, "mi.i")
            .unwrap()
            .into_int_value();
        let mi_i1 = self.builder.build_int_add(mi_i, one, "mi.i1").unwrap();
        let mid_slot = unsafe {
            self.builder
                .build_in_bounds_gep(i64_t, mi_runs, &[mi_i], "mid.slot")
                .unwrap()
        };
        let hi_slot = unsafe {
            self.builder
                .build_in_bounds_gep(i64_t, mi_runs, &[mi_i1], "hi.slot")
                .unwrap()
        };
        let mid_v = self
            .builder
            .build_load(i64_t, mid_slot, "mid.v")
            .unwrap()
            .into_int_value();
        let hi_v = self
            .builder
            .build_load(i64_t, hi_slot, "hi.v")
            .unwrap()
            .into_int_value();
        let mi_lo = self
            .builder
            .build_load(i64_t, lo_a, "mi.lo")
            .unwrap()
            .into_int_value();
        self.builder.build_store(mid_a, mid_v).unwrap();
        self.builder.build_store(hi_a, hi_v).unwrap();
        self.builder.build_store(aa_a, mi_lo).unwrap();
        self.builder.build_store(bb_a, mid_v).unwrap();
        self.builder.build_store(kk_a, mi_lo).unwrap();
        self.builder
            .build_unconditional_branch(p2_mode_chk)
            .unwrap();

        // while a < mid && b < hi  (short-circuit via two blocks)
        self.builder.position_at_end(p2_m_chk_a);
        let a_v = self
            .builder
            .build_load(i64_t, aa_a, "a.v")
            .unwrap()
            .into_int_value();
        let mid_v1 = self
            .builder
            .build_load(i64_t, mid_a, "mid.v1")
            .unwrap()
            .into_int_value();
        let a_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, a_v, mid_v1, "a.lt.mid")
            .unwrap();
        self.builder
            .build_conditional_branch(a_lt, p2_m_chk_b, p2_da_chk)
            .unwrap();

        self.builder.position_at_end(p2_m_chk_b);
        let b_v = self
            .builder
            .build_load(i64_t, bb_a, "b.v")
            .unwrap()
            .into_int_value();
        let hi_v1 = self
            .builder
            .build_load(i64_t, hi_a, "hi.v1")
            .unwrap()
            .into_int_value();
        let b_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, b_v, hi_v1, "b.lt.hi")
            .unwrap();
        self.builder
            .build_conditional_branch(b_lt, p2_m_cmp, p2_da_chk)
            .unwrap();

        // `cmp(src[a], src[b]) <= 0` keeps the left run first on a tie —
        // this is the stability guarantee.
        self.builder.position_at_end(p2_m_cmp);
        let src_p = self
            .builder
            .build_load(ptr_ty, src_a, "src.p")
            .unwrap()
            .into_pointer_value();
        let a_v1 = self
            .builder
            .build_load(i64_t, aa_a, "a.v1")
            .unwrap()
            .into_int_value();
        let b_v1 = self
            .builder
            .build_load(i64_t, bb_a, "b.v1")
            .unwrap()
            .into_int_value();
        let ea_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, src_p, &[a_v1], "ea.addr")
                .unwrap()
        };
        let eb_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, src_p, &[b_v1], "eb.addr")
                .unwrap()
        };
        let ea_v = self.builder.build_load(elem_ty, ea_addr, "ea").unwrap();
        let eb_v = self.builder.build_load(elem_ty, eb_addr, "eb").unwrap();
        let c2 =
            self.emit_sort_by_inline_compare(sort_fn, params, body, elem_type_name, ea_v, eb_v)?;
        let take_a = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, c2, zero, "take.a")
            .unwrap();
        self.builder
            .build_conditional_branch(take_a, p2_m_take_a, p2_m_take_b)
            .unwrap();

        self.builder.position_at_end(p2_m_take_a);
        self.emit_merge_take(elem_ty, src_a, dst_a, aa_a, kk_a, ptr_ty, i64_t, one);
        self.builder.build_unconditional_branch(p2_m_chk_a).unwrap();

        self.builder.position_at_end(p2_m_take_b);
        self.emit_merge_take(elem_ty, src_a, dst_a, bb_a, kk_a, ptr_ty, i64_t, one);
        self.builder.build_unconditional_branch(p2_m_chk_a).unwrap();

        // ── Per-pass merge-kernel selection (B-2026-08-10-19) ──────────────
        //
        // The branchy kernel above and the branchless one below produce
        // identical output and differ only in how the take decision reaches
        // the hardware: one branches on `cmp <= 0`, the other selects the
        // value and advances both cursors by `zext` of the predicate, so the
        // CPU never speculates. Which wins depends entirely on whether the
        // branch predictor can learn the take sequence.
        //
        // Usually it can. Merging two runs drawn from a small key alphabet,
        // or two runs that barely overlap, yields long stretches from one
        // side; even the perfect alternation that merging two identical runs
        // produces is a period-2 pattern any modern predictor nails.
        // Measured over a whole 150k sort, the mispredict rate is 2.75% on
        // few-unique input and 1.86% on sawtooth, and on those shapes the
        // branchless kernel is a pure loss — it trades speculation that was
        // working for a serial load -> compare -> cursor -> next-load
        // dependency chain, and measured 1.8-2.0x SLOWER.
        //
        // On shuffled-uniform input it cannot: the take sequence is a coin
        // flip, the rate is 12.96%, and those mispredicts are the *entire*
        // gap to Rust's driftsort. `cachegrind --branch-sim` over one 150k
        // sort puts karac at 1.09x driftsort's instructions, 0.62x its data
        // references and the same cache-miss profile — but 4.56x its
        // mispredicts (1.061M vs 0.233M). The 0.83M excess at ~20 cycles is
        // 5.9 ms, which is the measured 14.82 - 8.93 ms gap.
        //
        // So the question the code has to answer is exactly "would the
        // hardware predictor do well here?", and the cheapest honest way to
        // answer it is to simulate one. `p2.pr.*` runs the branchy merge for
        // the first `PROBE` outputs of the pass while feeding each take
        // through a 16-entry, 1-bit-per-entry predictor indexed by four bits
        // of global take history — six integer ops on one register-width
        // table.
        //
        // The history width is load-bearing, not a round number. A sawtooth
        // input merges two runs that hold each key `2^(pass-1)` times, so its
        // take sequence is periodic with period `2^pass`: alternation at pass
        // 1, AABB at pass 2, AAAABBBB at pass 3. An n-bit history predicts a
        // period-`p` sequence perfectly exactly when its p windows are
        // distinct, i.e. when `p <= 2^n`, so 4 bits covers periods up to 8
        // and degrades gracefully past that (one miss per period: 6% at
        // period 16). Two bits does not cover AAAABBBB — it reads 25% there,
        // close enough to the threshold to misroute, which is what the first
        // version of this code did.
        //
        // A plain switch-rate counter is the tempting simplification and it
        // is wrong in the other direction: it reads 1.0 on perfect
        // alternation and would route sawtooth — where the hardware
        // mispredicts 1.86% — straight to the branchless kernel.
        // Predictability, not switch frequency, is the question being asked.
        self.builder.position_at_end(p2_mode_chk);
        let pr_left = self
            .builder
            .build_load(i64_t, prleft_a, "pr.left")
            .unwrap()
            .into_int_value();
        let pr_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, pr_left, zero, "pr.go")
            .unwrap();
        self.builder
            .build_conditional_branch(pr_go, p2_pr_chk_a, p2_pr_decide)
            .unwrap();

        // Commit the pass once the probe budget is spent. Idempotent — later
        // merges in the same pass re-enter here and recompute the same answer
        // from counters that stopped moving when the budget hit zero.
        self.builder.position_at_end(p2_pr_decide);
        let d_miss = self
            .builder
            .build_load(i64_t, prmiss_a, "d.miss")
            .unwrap()
            .into_int_value();
        let d_tot = self
            .builder
            .build_load(i64_t, prtot_a, "d.tot")
            .unwrap()
            .into_int_value();
        let d_m5 = self.builder.build_int_mul(d_miss, five, "d.m5").unwrap();
        let d_t2 = self.builder.build_int_mul(d_tot, two, "d.t2").unwrap();
        // Go branchless only above a 40% simulated miss rate. The asymmetry
        // is deliberate: choosing branchless wrongly costs up to 2x, choosing
        // branchy wrongly costs ~1.3x, so the threshold sits well above where
        // any semi-ordered shape lands and just below the ~50% a coin-flip
        // take sequence produces. An earlier 1-in-3 threshold measured 8%
        // and 6% regressions on few-unique and sawtooth from exactly this
        // misrouting. The comparison is STRICT so that a skipped probe
        // (`prtot == 0`, short input) resolves to the branchy kernel rather
        // than reading 0 >= 0 as "unpredictable".
        let d_bl = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, d_m5, d_t2, "d.bl")
            .unwrap();
        // `KARAC_SORT_FORCE=branchy|branchless` pins the kernel instead of
        // letting the probe pick. Default behaviour is UNCHANGED; this exists
        // because the probe's calibration is x86-derived and re-measuring it on
        // another microarch needs an A/B that does not require patching the
        // compiler (B-2026-08-15-30).
        //
        // WHAT IT FOUND ON ARM64, and why the default is nonetheless untouched.
        // Isolated 150k x 25-round kernel over six patterns, each kernel forced
        // (branchless/branchy, cycles): random 1.28x, sorted 1.07x, reverse
        // 1.01x, few-unique 0.98x, sawtooth 7.98x, nearly-sorted 5.44x — so on
        // this host branchless loses almost everywhere, the reverse of the x86
        // result the 40% threshold encodes. Pinning branchy corpus-wide is
        // still NOT a win: measured against a same-commit control over every
        // kata that sorts, with binary-hash gating to exclude the untouched,
        //
        //   #252 0.815x   #253 0.865x   #1665 0.976x   #40 0.996x   #47 0.996x
        //   #18  1.010x   #15  1.031x   #16   1.037x   #56 1.042x
        //
        // — two large wins against four real regressions, median ~flat. And the
        // obvious discriminator is wrong: #56 sorts `(i64,i64)` exactly as #252
        // does and moves the other way, so element type does not separate them
        // either. Until something does, a blanket arch gate trades one set of
        // katas for another, which is not an improvement.
        // THE ELEMENT-WIDTH GATE THAT USED TO LIVE HERE IS GONE, because the
        // thing it worked around is fixed. It forced the branchy kernel for
        // elements wider than a register on aarch64, on the measurement that
        // branchless lost 1.27x to branchy on random `(i64,i64)` while winning
        // 1.24x on `i64`. That asymmetry was not about width as such — it was
        // the branchless kernel selecting the VALUE, which pinned both elements
        // wide and round-tripped them through SIMD registers (see the
        // `bl.src.sel` comment). Selecting the source ADDRESS instead took that
        // kernel from 834M to 564M cycles on random `(i64,i64)`, which puts it
        // BACK AHEAD of branchy there (0.86x) and leaves the 8-byte case
        // unchanged.
        //
        // With the kernel fixed the probe's own premise holds on this host as
        // it does on x86 — branchless for an unpredictable take sequence,
        // branchy for a predictable one — and it is a better decider than width
        // ever was, because the split is not width-shaped: on sawtooth and
        // nearly-sorted the branchy kernel still wins by 5.0x and 3.6x at the
        // SAME 16-byte width. A width gate cannot express that; the probe can,
        // so let it.
        let d_mode = match std::env::var("KARAC_SORT_FORCE").as_deref() {
            Ok("branchless") => one.into(),
            Ok("branchy") => zero.into(),
            _ => self
                .builder
                .build_select(d_bl, one, zero, "d.mode")
                .unwrap(),
        };
        self.builder.build_store(blmode_a, d_mode).unwrap();
        self.builder
            .build_unconditional_branch(p2_mode_pick)
            .unwrap();

        self.builder.position_at_end(p2_mode_pick);
        let mp_v = self
            .builder
            .build_load(i64_t, blmode_a, "mp.v")
            .unwrap()
            .into_int_value();
        let mp_bl = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, mp_v, zero, "mp.bl")
            .unwrap();
        self.builder
            .build_conditional_branch(mp_bl, p2_bl_chk_a, p2_m_chk_a)
            .unwrap();

        // ── Probe kernel: the branchy merge plus the predictor simulation ──
        self.builder.position_at_end(p2_pr_chk_a);
        let pr_a_v = self
            .builder
            .build_load(i64_t, aa_a, "pr.a.v")
            .unwrap()
            .into_int_value();
        let pr_mid = self
            .builder
            .build_load(i64_t, mid_a, "pr.mid")
            .unwrap()
            .into_int_value();
        let pr_a_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, pr_a_v, pr_mid, "pr.a.lt")
            .unwrap();
        self.builder
            .build_conditional_branch(pr_a_lt, p2_pr_chk_b, p2_da_chk)
            .unwrap();

        self.builder.position_at_end(p2_pr_chk_b);
        let pr_b_v = self
            .builder
            .build_load(i64_t, bb_a, "pr.b.v")
            .unwrap()
            .into_int_value();
        let pr_hi = self
            .builder
            .build_load(i64_t, hi_a, "pr.hi")
            .unwrap()
            .into_int_value();
        let pr_b_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, pr_b_v, pr_hi, "pr.b.lt")
            .unwrap();
        self.builder
            .build_conditional_branch(pr_b_lt, p2_pr_cmp, p2_da_chk)
            .unwrap();

        self.builder.position_at_end(p2_pr_cmp);
        let pr_src = self
            .builder
            .build_load(ptr_ty, src_a, "pr.src")
            .unwrap()
            .into_pointer_value();
        let pr_a1 = self
            .builder
            .build_load(i64_t, aa_a, "pr.a1")
            .unwrap()
            .into_int_value();
        let pr_b1 = self
            .builder
            .build_load(i64_t, bb_a, "pr.b1")
            .unwrap()
            .into_int_value();
        let pr_ea_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, pr_src, &[pr_a1], "pr.ea.addr")
                .unwrap()
        };
        let pr_eb_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, pr_src, &[pr_b1], "pr.eb.addr")
                .unwrap()
        };
        let pr_ea = self
            .builder
            .build_load(elem_ty, pr_ea_addr, "pr.ea")
            .unwrap();
        let pr_eb = self
            .builder
            .build_load(elem_ty, pr_eb_addr, "pr.eb")
            .unwrap();
        let pr_c =
            self.emit_sort_by_inline_compare(sort_fn, params, body, elem_type_name, pr_ea, pr_eb)?;
        let pr_take_a = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, pr_c, zero, "pr.take.a")
            .unwrap();
        // `cur` is the outcome being predicted: 0 = took the left run,
        // 1 = took the right. `tab` holds four one-bit predictions indexed by
        // `hist`, the last two outcomes. Predict, score, then train — the
        // `xor` writes `cur` into `tab[hist]` exactly when the prediction
        // missed, which is the whole update.
        let pr_take_z = self
            .builder
            .build_int_z_extend(pr_take_a, i64_t, "pr.take.z")
            .unwrap();
        let pr_cur = self
            .builder
            .build_int_sub(one, pr_take_z, "pr.cur")
            .unwrap();
        let pr_tab = self
            .builder
            .build_load(i64_t, prtab_a, "pr.tab")
            .unwrap()
            .into_int_value();
        let pr_hist = self
            .builder
            .build_load(i64_t, prhist_a, "pr.hist")
            .unwrap()
            .into_int_value();
        let pr_shifted = self
            .builder
            .build_right_shift(pr_tab, pr_hist, false, "pr.shifted")
            .unwrap();
        let pr_pred = self.builder.build_and(pr_shifted, one, "pr.pred").unwrap();
        let pr_miss = self.builder.build_xor(pr_pred, pr_cur, "pr.miss").unwrap();
        let pr_miss_acc = self
            .builder
            .build_load(i64_t, prmiss_a, "pr.miss.acc")
            .unwrap()
            .into_int_value();
        let pr_miss_n = self
            .builder
            .build_int_add(pr_miss_acc, pr_miss, "pr.miss.n")
            .unwrap();
        self.builder.build_store(prmiss_a, pr_miss_n).unwrap();
        let pr_tot_acc = self
            .builder
            .build_load(i64_t, prtot_a, "pr.tot.acc")
            .unwrap()
            .into_int_value();
        let pr_tot_n = self
            .builder
            .build_int_add(pr_tot_acc, one, "pr.tot.n")
            .unwrap();
        self.builder.build_store(prtot_a, pr_tot_n).unwrap();
        let pr_flip = self
            .builder
            .build_left_shift(pr_miss, pr_hist, "pr.flip")
            .unwrap();
        let pr_tab_n = self.builder.build_xor(pr_tab, pr_flip, "pr.tab.n").unwrap();
        self.builder.build_store(prtab_a, pr_tab_n).unwrap();
        let pr_h_sh = self
            .builder
            .build_left_shift(pr_hist, one, "pr.h.sh")
            .unwrap();
        let pr_h_or = self.builder.build_or(pr_h_sh, pr_cur, "pr.h.or").unwrap();
        let pr_hist_n = self
            .builder
            .build_and(pr_h_or, hist_mask, "pr.hist.n")
            .unwrap();
        self.builder.build_store(prhist_a, pr_hist_n).unwrap();
        let pr_left_v = self
            .builder
            .build_load(i64_t, prleft_a, "pr.left.v")
            .unwrap()
            .into_int_value();
        let pr_left_n = self
            .builder
            .build_int_sub(pr_left_v, one, "pr.left.n")
            .unwrap();
        self.builder.build_store(prleft_a, pr_left_n).unwrap();
        self.builder
            .build_conditional_branch(pr_take_a, p2_pr_take_a, p2_pr_take_b)
            .unwrap();

        self.builder.position_at_end(p2_pr_take_a);
        self.emit_merge_take(elem_ty, src_a, dst_a, aa_a, kk_a, ptr_ty, i64_t, one);
        self.builder
            .build_unconditional_branch(p2_mode_chk)
            .unwrap();

        self.builder.position_at_end(p2_pr_take_b);
        self.emit_merge_take(elem_ty, src_a, dst_a, bb_a, kk_a, ptr_ty, i64_t, one);
        self.builder
            .build_unconditional_branch(p2_mode_chk)
            .unwrap();

        // ── Branchless kernel ──────────────────────────────────────────────
        // Same `cmp <= 0` tie-break as the branchy kernel, so it takes the
        // left run on a tie and stability is identical. Both element loads
        // issue unconditionally and the cursors advance by `zext` of the
        // predicate, so the only conditional branches left per element are
        // the two loop bounds — and those stay taken until a run drains.
        self.builder.position_at_end(p2_bl_chk_a);
        let bl_a_v = self
            .builder
            .build_load(i64_t, aa_a, "bl.a.v")
            .unwrap()
            .into_int_value();
        let bl_mid = self
            .builder
            .build_load(i64_t, mid_a, "bl.mid")
            .unwrap()
            .into_int_value();
        let bl_a_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, bl_a_v, bl_mid, "bl.a.lt")
            .unwrap();
        self.builder
            .build_conditional_branch(bl_a_lt, p2_bl_chk_b, p2_da_chk)
            .unwrap();

        self.builder.position_at_end(p2_bl_chk_b);
        let bl_b_v = self
            .builder
            .build_load(i64_t, bb_a, "bl.b.v")
            .unwrap()
            .into_int_value();
        let bl_hi = self
            .builder
            .build_load(i64_t, hi_a, "bl.hi")
            .unwrap()
            .into_int_value();
        let bl_b_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, bl_b_v, bl_hi, "bl.b.lt")
            .unwrap();
        self.builder
            .build_conditional_branch(bl_b_lt, p2_bl_body, p2_da_chk)
            .unwrap();

        self.builder.position_at_end(p2_bl_body);
        let bl_src = self
            .builder
            .build_load(ptr_ty, src_a, "bl.src")
            .unwrap()
            .into_pointer_value();
        let bl_dst = self
            .builder
            .build_load(ptr_ty, dst_a, "bl.dst")
            .unwrap()
            .into_pointer_value();
        let bl_a1 = self
            .builder
            .build_load(i64_t, aa_a, "bl.a1")
            .unwrap()
            .into_int_value();
        let bl_b1 = self
            .builder
            .build_load(i64_t, bb_a, "bl.b1")
            .unwrap()
            .into_int_value();
        let bl_k = self
            .builder
            .build_load(i64_t, kk_a, "bl.k")
            .unwrap()
            .into_int_value();
        let bl_ea_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, bl_src, &[bl_a1], "bl.ea.addr")
                .unwrap()
        };
        let bl_eb_addr = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, bl_src, &[bl_b1], "bl.eb.addr")
                .unwrap()
        };
        let bl_ea = self
            .builder
            .build_load(elem_ty, bl_ea_addr, "bl.ea")
            .unwrap();
        let bl_eb = self
            .builder
            .build_load(elem_ty, bl_eb_addr, "bl.eb")
            .unwrap();
        let bl_c =
            self.emit_sort_by_inline_compare(sort_fn, params, body, elem_type_name, bl_ea, bl_eb)?;
        let bl_take_a = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, bl_c, zero, "bl.take.a")
            .unwrap();
        // SELECT THE SOURCE ADDRESS, NOT THE VALUE (B-2026-08-15-30). Selecting
        // the value forces BOTH elements to be live in registers at the select,
        // and for an element wider than a register that is expensive on arm64:
        // LLVM materialises each one in a SIMD register and extracts every field
        // back to GP registers so the `csel` pair can run on them. Measured on
        // `(i64,i64)`, the emitted inner loop was
        //
        //     ldr q0,[..]; mov.d x4,v0[1]; fmov x5,d0     <- 3 instrs per operand
        //     ldr q0,[..]; mov.d x6,v0[1]; fmov x7,d0
        //     cmp; csel; csel; stp; cinc; cinc; ...        = 17 instrs/element
        //
        // with the SIMD->GP moves sitting on the loop-carried critical path,
        // which is what put this kernel at IPC ~1.4 and made it LOSE to the
        // branchy merge on wide elements — the opposite of its purpose.
        //
        // Selecting the pointer instead leaves one `csel` on a GP register and
        // one element-sized copy from the winner. The comparator's own loads
        // then narrow to whatever it actually reads (just the key, for the
        // usual `|a, b| a.0.cmp(b.0)`), instead of being pinned wide by the
        // select. Semantics are unchanged: same element chosen, same store.
        let bl_src_sel = self
            .builder
            .build_select(bl_take_a, bl_ea_addr, bl_eb_addr, "bl.src.sel")
            .unwrap()
            .into_pointer_value();
        let bl_val = self
            .builder
            .build_load(elem_ty, bl_src_sel, "bl.val")
            .unwrap();
        let bl_to = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, bl_dst, &[bl_k], "bl.to")
                .unwrap()
        };
        self.builder.build_store(bl_to, bl_val).unwrap();
        let bl_inc_a = self
            .builder
            .build_int_z_extend(bl_take_a, i64_t, "bl.inc.a")
            .unwrap();
        let bl_inc_b = self
            .builder
            .build_int_sub(one, bl_inc_a, "bl.inc.b")
            .unwrap();
        let bl_a_n = self
            .builder
            .build_int_add(bl_a1, bl_inc_a, "bl.a.n")
            .unwrap();
        let bl_b_n = self
            .builder
            .build_int_add(bl_b1, bl_inc_b, "bl.b.n")
            .unwrap();
        let bl_k_n = self.builder.build_int_add(bl_k, one, "bl.k.n").unwrap();
        self.builder.build_store(aa_a, bl_a_n).unwrap();
        self.builder.build_store(bb_a, bl_b_n).unwrap();
        self.builder.build_store(kk_a, bl_k_n).unwrap();
        self.builder
            .build_unconditional_branch(p2_bl_chk_a)
            .unwrap();

        // Drain the left run, then the right run.
        self.builder.position_at_end(p2_da_chk);
        let a_v2 = self
            .builder
            .build_load(i64_t, aa_a, "a.v2")
            .unwrap()
            .into_int_value();
        let mid_v2 = self
            .builder
            .build_load(i64_t, mid_a, "mid.v2")
            .unwrap()
            .into_int_value();
        let da_cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, a_v2, mid_v2, "da.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(da_cond, p2_da_body, p2_db_chk)
            .unwrap();

        self.builder.position_at_end(p2_da_body);
        self.emit_merge_take(elem_ty, src_a, dst_a, aa_a, kk_a, ptr_ty, i64_t, one);
        self.builder.build_unconditional_branch(p2_da_chk).unwrap();

        self.builder.position_at_end(p2_db_chk);
        let b_v2 = self
            .builder
            .build_load(i64_t, bb_a, "b.v2")
            .unwrap()
            .into_int_value();
        let hi_v2 = self
            .builder
            .build_load(i64_t, hi_a, "hi.v2")
            .unwrap()
            .into_int_value();
        let db_cond = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, b_v2, hi_v2, "db.cond")
            .unwrap();
        self.builder
            .build_conditional_branch(db_cond, p2_db_body, p2_pair_done)
            .unwrap();

        self.builder.position_at_end(p2_db_body);
        self.emit_merge_take(elem_ty, src_a, dst_a, bb_a, kk_a, ptr_ty, i64_t, one);
        self.builder.build_unconditional_branch(p2_db_chk).unwrap();

        // The merged pair becomes one run in the output list.
        self.builder.position_at_end(p2_pair_done);
        let pd_hi = self
            .builder
            .build_load(i64_t, hi_a, "pd.hi")
            .unwrap()
            .into_int_value();
        let pd_runs2 = self
            .builder
            .build_load(ptr_ty, runs2_a, "pd.runs2")
            .unwrap()
            .into_pointer_value();
        let pd_onr = self
            .builder
            .build_load(i64_t, onr_a, "pd.onr")
            .unwrap()
            .into_int_value();
        let pd_slot = unsafe {
            self.builder
                .build_in_bounds_gep(i64_t, pd_runs2, &[pd_onr], "pd.slot")
                .unwrap()
        };
        self.builder.build_store(pd_slot, pd_hi).unwrap();
        let pd_onr_n = self.builder.build_int_add(pd_onr, one, "pd.onr.n").unwrap();
        self.builder.build_store(onr_a, pd_onr_n).unwrap();
        self.builder.build_store(lo_a, pd_hi).unwrap();
        let pd_i = self
            .builder
            .build_load(i64_t, ii_a, "pd.i")
            .unwrap()
            .into_int_value();
        let pd_i_n = self.builder.build_int_add(pd_i, two, "pd.i.n").unwrap();
        self.builder.build_store(ii_a, pd_i_n).unwrap();
        self.builder.build_unconditional_branch(p2_i_chk).unwrap();

        // Odd run out: copy it across unchanged so `dst` holds the whole
        // array, and carry its boundary into the output list.
        self.builder.position_at_end(p2_tail);
        let tl_runs = self
            .builder
            .build_load(ptr_ty, runs_a, "tl.runs")
            .unwrap()
            .into_pointer_value();
        let tl_i = self
            .builder
            .build_load(i64_t, ii_a, "tl.i")
            .unwrap()
            .into_int_value();
        let tl_slot = unsafe {
            self.builder
                .build_in_bounds_gep(i64_t, tl_runs, &[tl_i], "tl.slot")
                .unwrap()
        };
        let tl_hi = self
            .builder
            .build_load(i64_t, tl_slot, "tl.hi")
            .unwrap()
            .into_int_value();
        let tl_lo = self
            .builder
            .build_load(i64_t, lo_a, "tl.lo")
            .unwrap()
            .into_int_value();
        let tl_cnt = self.builder.build_int_sub(tl_hi, tl_lo, "tl.cnt").unwrap();
        let tl_bytes = self
            .builder
            .build_int_mul(tl_cnt, elem_size, "tl.bytes")
            .unwrap();
        let tl_src_p = self
            .builder
            .build_load(ptr_ty, src_a, "tl.src.p")
            .unwrap()
            .into_pointer_value();
        let tl_dst_p = self
            .builder
            .build_load(ptr_ty, dst_a, "tl.dst.p")
            .unwrap()
            .into_pointer_value();
        let tl_from = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, tl_src_p, &[tl_lo], "tl.from")
                .unwrap()
        };
        let tl_to = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, tl_dst_p, &[tl_lo], "tl.to")
                .unwrap()
        };
        // Alignment 1: these are interior pointers, so the element type's own
        // alignment is the most that can be claimed and a struct of i32s only
        // has 4. Claiming more than a pointer actually has would be UB, and
        // this path runs at most once per pass.
        self.builder
            .build_memcpy(tl_to, 1, tl_from, 1, tl_bytes)
            .unwrap();
        let tl_runs2 = self
            .builder
            .build_load(ptr_ty, runs2_a, "tl.runs2")
            .unwrap()
            .into_pointer_value();
        let tl_onr = self
            .builder
            .build_load(i64_t, onr_a, "tl.onr")
            .unwrap()
            .into_int_value();
        let tl_out = unsafe {
            self.builder
                .build_in_bounds_gep(i64_t, tl_runs2, &[tl_onr], "tl.out")
                .unwrap()
        };
        self.builder.build_store(tl_out, tl_hi).unwrap();
        let tl_onr_n = self.builder.build_int_add(tl_onr, one, "tl.onr.n").unwrap();
        self.builder.build_store(onr_a, tl_onr_n).unwrap();
        self.builder.build_store(lo_a, tl_hi).unwrap();
        let tl_i_n = self.builder.build_int_add(tl_i, one, "tl.i.n").unwrap();
        self.builder.build_store(ii_a, tl_i_n).unwrap();
        self.builder.build_unconditional_branch(p2_i_chk).unwrap();

        // End of pass: swap both the element buffers and the run lists.
        self.builder.position_at_end(p2_pass_end);
        let s_old = self
            .builder
            .build_load(ptr_ty, src_a, "s.old")
            .unwrap()
            .into_pointer_value();
        let d_old = self
            .builder
            .build_load(ptr_ty, dst_a, "d.old")
            .unwrap()
            .into_pointer_value();
        self.builder.build_store(src_a, d_old).unwrap();
        self.builder.build_store(dst_a, s_old).unwrap();
        let r_old = self
            .builder
            .build_load(ptr_ty, runs_a, "r.old")
            .unwrap()
            .into_pointer_value();
        let r2_old = self
            .builder
            .build_load(ptr_ty, runs2_a, "r2.old")
            .unwrap()
            .into_pointer_value();
        self.builder.build_store(runs_a, r2_old).unwrap();
        self.builder.build_store(runs2_a, r_old).unwrap();
        let onr_fin = self
            .builder
            .build_load(i64_t, onr_a, "onr.fin")
            .unwrap()
            .into_int_value();
        self.builder.build_store(nr_a, onr_fin).unwrap();
        self.builder.build_unconditional_branch(p2_chk).unwrap();

        // ── An odd number of passes leaves the live data in scratch ─────────
        self.builder.position_at_end(p2_done);
        let final_src = self
            .builder
            .build_load(ptr_ty, src_a, "final.src")
            .unwrap()
            .into_pointer_value();
        let fs_int = self
            .builder
            .build_ptr_to_int(final_src, i64_t, "fs.int")
            .unwrap();
        let data_int = self
            .builder
            .build_ptr_to_int(data, i64_t, "data.int")
            .unwrap();
        let needs_copy = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, fs_int, data_int, "needs.copy")
            .unwrap();
        self.builder
            .build_conditional_branch(needs_copy, copy_back, fini)
            .unwrap();

        self.builder.position_at_end(copy_back);
        self.builder
            .build_memcpy(data, 8, final_src, 8, total_bytes)
            .unwrap();
        self.builder.build_unconditional_branch(fini).unwrap();

        // Free all three buffers. The run lists are freed through the allocas
        // because the pass loop swaps them; between the two they always name
        // the two `malloc`s regardless of how many swaps happened.
        self.builder.position_at_end(fini);
        let fr_runs = self
            .builder
            .build_load(ptr_ty, runs_a, "fr.runs")
            .unwrap()
            .into_pointer_value();
        let fr_runs2 = self
            .builder
            .build_load(ptr_ty, runs2_a, "fr.runs2")
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_call(free_fn, &[fr_runs.into()], "")
            .unwrap();
        self.builder
            .build_call(free_fn, &[fr_runs2.into()], "")
            .unwrap();
        self.builder
            .build_call(free_fn, &[scratch.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(exit).unwrap();

        self.builder.position_at_end(exit);
        self.builder.build_return(None).unwrap();

        // sort_fn is complete, so the mutually recursive partner can be
        // defined now.
        self.emit_sort_partition_body(
            qpart_fn,
            sort_fn,
            isort_fn,
            params,
            body,
            elem_ty,
            elem_type_name,
        )?;
        self.emit_sort_isort_body(isort_fn, params, body, elem_ty, elem_type_name)?;
        self.emit_sort_probe_body(probe_fn, params, body, elem_ty, elem_type_name)?;

        // Restore outer state.
        self.mono_state.type_subst = saved_subst;
        self.fn_ctx.loop_stack = saved_loop_stack;
        self.var_types.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_state.closure_fn_types = saved_cfn;
        self.closure_state.pending_closure_fn_type = saved_pct;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Ok(sort_fn)
    }

    /// splitmix64's finalizer. Used to derive pivot indices deterministically
    /// from a range's `(lo, hi, depth)`, so a build is reproducible and a
    /// pivot is still uncorrelated with any structure in the input.
    fn emit_splitmix64(&mut self, seed: IntValue<'ctx>) -> IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let b = &self.builder;
        let z = b
            .build_int_add(seed, i64_t.const_int(0x9E37_79B9_7F4A_7C15, false), "sm.z")
            .unwrap();
        let s30 = b
            .build_right_shift(z, i64_t.const_int(30, false), false, "sm.s30")
            .unwrap();
        let x1 = b.build_xor(z, s30, "sm.x1").unwrap();
        let m1 = b
            .build_int_mul(x1, i64_t.const_int(0xBF58_476D_1CE4_E5B9, false), "sm.m1")
            .unwrap();
        let s27 = b
            .build_right_shift(m1, i64_t.const_int(27, false), false, "sm.s27")
            .unwrap();
        let x2 = b.build_xor(m1, s27, "sm.x2").unwrap();
        let m2 = b
            .build_int_mul(x2, i64_t.const_int(0x94D0_49BB_1331_11EB, false), "sm.m2")
            .unwrap();
        let s31 = b
            .build_right_shift(m2, i64_t.const_int(31, false), false, "sm.s31")
            .unwrap();
        b.build_xor(m2, s31, "sm.out").unwrap()
    }

    /// Emit the body of `__vec_<m>_qpart_<id>` — the full-array stable
    /// partition of B-2026-08-11-10 § Direction 7, ported from the validated
    /// mirror at `docs/spikes/sortbench/mirror.rs`.
    ///
    /// `void qpart(data, scratch, lo, hi, in_a, depth)` sorts `[lo,hi)`, whose
    /// live elements are in `data` iff `in_a`, and leaves the result sorted,
    /// stable and **in `data`**. It ping-pongs between the two buffers exactly
    /// as phase 2 does, borrowing phase 2's own scratch, so it allocates
    /// nothing.
    ///
    /// Why this beats a merge on low-cardinality input, and why the bounded
    /// run-builder reverted for B-2026-08-10-20 could not: a merge pass writes
    /// all `n` elements no matter what the keys are, so `n·log2(n/RUN)` merge
    /// outputs is fixed. A partition can **stop early** — a range whose every
    /// element ties with the pivot is already sorted and stable — so 8 distinct
    /// keys resolve in ~3 levels rather than 13 passes. Measured 2.5x
    /// instructions and 1.75x wall clock on that shape; see the spike.
    ///
    /// Stability is the count-then-scatter shape: one pass counts, a second
    /// walks the range in order writing each element to one of two advancing
    /// cursors, so relative order is preserved within both halves.
    #[allow(clippy::too_many_arguments)]
    fn emit_sort_partition_body(
        &mut self,
        qpart_fn: FunctionValue<'ctx>,
        sort_fn: FunctionValue<'ctx>,
        isort_fn: FunctionValue<'ctx>,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_type_name: Option<&str>,
    ) -> Result<(), String> {
        /// Ranges at or below this stop partitioning and are sorted directly by
        /// `isort_fn` (a longer one that reaches the leaf from the tie gate or
        /// the depth backstop keeps the merge — see `leaf_call`). Kept at or
        /// below the entry probe's own length floor so a handoff can never
        /// re-probe.
        ///
        /// 4096 until B-2026-08-15-30. It was sized for the low-cardinality arm,
        /// which bottoms out in a few levels and never reaches a deep leaf, so
        /// the value did not matter there — measured across 32..4096 on
        /// few-unique it moves nothing (1.00-1.01x). It matters a great deal on
        /// the unstructured arm, where every element descends the full
        /// log2(n/SPAN).
        ///
        /// Re-swept for B-2026-08-16-3, because the leaf sorter changed and the
        /// old optimum was the optimum for a MERGE leaf. Random, cycles, both
        /// arms from one karac via `KARAC_SORT_LEAF`:
        ///
        ///   SPAN         8      16      24      32      48      64      96
        ///   merge    384.7M  377.9M  367.2M  359.6M  351.4M  353.1M  351.1M
        ///   insert   311.2M  315.0M  316.3M  313.9M  313.0M  312.8M  321.8M
        ///
        /// The merge leaf's tight U is gone: an insertion leaf is FLAT from 8 to
        /// 64 (311-316M, a 1.6% spread that is inside run-to-run noise) and only
        /// degrades at 96. So 64 stays — it is inside the new plateau as well as
        /// the old one, and holding it constant keeps this change to one
        /// variable.
        const SPAN: u64 = 64;
        // `KARAC_SORT_PART_SPAN` overrides the leaf handoff, the second half of
        // the B-2026-08-15-30 lever, and the axis the sweep above is taken on.
        let span_v = std::env::var("KARAC_SORT_PART_SPAN")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(SPAN);
        /// Partition levels before forcing the merge. Each level splits into
        /// two strictly smaller non-empty ranges, so this is a backstop, not a
        /// normal exit — but it is what preserves the O(n log n) worst case
        /// that a randomised-pivot partition would otherwise give up.
        const DEPTH_MAX: u64 = 64;
        /// Partition a range only if at least `len / GATE` of its elements tie
        /// with the pivot, i.e. estimated distinct keys <= GATE. Measured
        /// crossover is between 64 and 128 distinct keys, matching the
        /// arithmetic (partitioning costs two passes per level against the
        /// merge's one, so it wins while 2·log2(d) < log2(n/RUN)).
        const GATE: u64 = 64;

        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();
        let one = i64_t.const_int(1, false);

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_cfn = std::mem::take(&mut self.closure_state.closure_fn_types);
        let saved_pct = self.closure_state.pending_closure_fn_type.take();
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();
        self.current_fn = Some(qpart_fn);

        let data = qpart_fn.get_nth_param(0).unwrap().into_pointer_value();
        let scratch = qpart_fn.get_nth_param(1).unwrap().into_pointer_value();
        let lo = qpart_fn.get_nth_param(2).unwrap().into_int_value();
        let hi = qpart_fn.get_nth_param(3).unwrap().into_int_value();
        let in_a = qpart_fn.get_nth_param(4).unwrap().into_int_value();
        let depth = qpart_fn.get_nth_param(5).unwrap().into_int_value();
        let gate = qpart_fn.get_nth_param(6).unwrap().into_int_value();

        let bb = |s: &Self, n: &str| s.context.append_basic_block(qpart_fn, n);
        let entry = bb(self, "entry");
        let leaf = bb(self, "leaf");
        let leaf_cp = bb(self, "leaf.cp");
        let leaf_call = bb(self, "leaf.call");
        let leaf_ins = bb(self, "leaf.ins");
        let leaf_merge = bb(self, "leaf.merge");
        let pv = bb(self, "pivot");
        let cnt_chk = bb(self, "cnt.chk");
        let cnt_body = bb(self, "cnt.body");
        let gate_chk = bb(self, "gate.chk");
        let alleq_chk = bb(self, "alleq.chk");
        let alleq = bb(self, "alleq");
        let alleq_cp = bb(self, "alleq.cp");
        let pick = bb(self, "pick");
        // The scatter is emitted once per split predicate — see the block
        // comment on `split_scatter` below. `KARAC_SORT_SCATTER_SPLIT=0` keeps
        // the single fused loop, in which case only the `lt` pair is created.
        let split_scatter = std::env::var("KARAC_SORT_SCATTER_SPLIT").as_deref() != Ok("0");
        let sc_chk = bb(self, "sc.chk");
        let sc_body = bb(self, "sc.body");
        let sc_chk_le = split_scatter.then(|| bb(self, "sc.chk.le"));
        let sc_body_le = split_scatter.then(|| bb(self, "sc.body.le"));
        // ── the FUSED single pass (B-2026-08-16-9) ─────────────────────────
        // Replaces count-then-scatter with ONE pass, on the unstructured arm
        // only. DEFAULT ON; `KARAC_SORT_FUSED=0` restores count-then-scatter,
        // and the blocks are not even created then.
        //
        // Worth 68.75 -> 66.58 cycles per element on shuffled `(i64,i64)` at
        // n=150k, which moves the gap to driftsort from 1.297x to 1.256x. The
        // win is IPC, not work: instructions go UP 2.2% (the reversal costs more
        // than the count saved) while IPC goes 3.95 -> 4.10. Validated against
        // layout rather than as a point estimate — five emission orders per arm,
        // and the two distributions do not overlap (fused max 66.71 < count min
        // 68.37), which matters because a same-day investigation found a 15.5%
        // swing on kata 236 from emission order alone.
        //
        // WHY IT CAN BE ONE PASS. The count exists to place the right cursor,
        // which cannot be known before the range is read. Writing the `>=` side
        // BACKWARD from `hi` needs no such base: the two cursors meet exactly at
        // `lo + nlt`. The right half comes out reversed, so a reversal pass
        // restores stability — but a reversal is a dependency-free pair swap,
        // where the count pass is a full read of the range whose result the
        // scatter must wait for.
        //
        // WHY ONLY THE UNSTRUCTURED ARM. `gate == 0` marks a range admitted for
        // being unstructured, and it is exactly there that the gate test below
        // is skipped — so the count feeds nothing but cursor placement and can
        // be deleted. On the low-cardinality arm the same count answers the tie
        // gate, and deleting it would cost a routing decision worth far more
        // than the pass.
        //
        // WHY A RETRY. Both halves must be non-empty or the recursion does not
        // terminate. `nlt == 0` (the pivot is the range minimum) is the one case
        // a `<` split cannot satisfy, and the count path handles it by splitting
        // on `<=` instead — a choice that needs the count. The fused pass cannot
        // know it in advance, so it re-runs the count path for that range. The
        // source buffer is untouched by the fused pass (it writes only to
        // `dead`), which is what makes the retry safe. On unstructured data a
        // median-of-3 pivot is the range minimum with probability ~3/m², so this
        // is a correctness backstop, not a cost.
        //
        // `KARAC_SORT_FUSED=2` is a TEST mode: it routes every range through the
        // fused pass, gate or not. The retry is near-unreachable on the arm the
        // fused path actually serves — a median-of-3 pivot is the range minimum
        // with probability ~3/m² on unstructured data — so without this the
        // retry would ship untested. Forcing it onto low-cardinality ranges,
        // where the pivot IS frequently the minimum, exercises it hard.
        let fused_mode = std::env::var("KARAC_SORT_FUSED")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);
        let fused = fused_mode >= 1;
        let fu = fused.then(|| {
            (
                bb(self, "fu.chk"),
                bb(self, "fu.body"),
                bb(self, "fu.done"),
                bb(self, "fu.retry"),
                bb(self, "fu.rev.chk"),
                bb(self, "fu.rev.body"),
            )
        });
        let rec = bb(self, "rec");
        let l_eq = bb(self, "l.eq");
        let l_eq_cp = bb(self, "l.eq.cp");
        let l_rec = bb(self, "l.rec");
        let r_disp = bb(self, "r.disp");
        let r_eq = bb(self, "r.eq");
        let r_eq_cp = bb(self, "r.eq.cp");
        let r_rec = bb(self, "r.rec");
        let ret = bb(self, "ret");

        // ── entry: pick the live/dead buffers, bail to the merge if small ──
        self.builder.position_at_end(entry);
        // MEASUREMENT ONLY, default 0 and inert. `KARAC_SORT_RANGE_PAD=N`
        // executes once per partition CALL, so it counts ranges rather than
        // elements — the denominator needed to turn the unattributed remainder
        // into a per-range cost, and the one thing the per-element pads cannot
        // give.
        let range_pad = std::env::var("KARAC_SORT_RANGE_PAD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if range_pad > 0 {
            let p = self.create_entry_alloca(qpart_fn, "rgpad", i64_t.into());
            for _ in 0..range_pad {
                let st = self.builder.build_store(p, zero).unwrap();
                st.set_volatile(true).unwrap();
            }
        }
        let piv_a = self.create_entry_alloca(qpart_fn, "piv", elem_ty);
        let nlt_a = self.create_entry_alloca(qpart_fn, "nlt", i64_t.into());
        let nle_a = self.create_entry_alloca(qpart_fn, "nle", i64_t.into());
        let ii_a = self.create_entry_alloca(qpart_fn, "qi", i64_t.into());
        let lc_a = self.create_entry_alloca(qpart_fn, "lcur", i64_t.into());
        let rc_a = self.create_entry_alloca(qpart_fn, "rcur", i64_t.into());
        let split_a = self.create_entry_alloca(qpart_fn, "split", i64_t.into());
        // Only the fused path needs these. `no_lt` and `right_eq` are SSA values
        // defined on the COUNT path, so a fused path reaching `rec` without
        // passing through those blocks cannot see them; routing them through
        // memory lets both paths converge and leaves mem2reg to rebuild the phi.
        // `rv` is the reversal's descending cursor.
        // MEASUREMENT ONLY, default 0 and inert. `KARAC_SORT_FU_PAD=N` /
        // `KARAC_SORT_REV_PAD=N` emit N volatile stores into the fused pass's
        // body and the reversal's body respectively.
        //
        // A volatile store of a constant to a dead alloca is exactly one `str`
        // that LLVM may not eliminate, so the instruction delta between PAD=0
        // and PAD=N is N x (iterations of that loop) — which turns an iteration
        // count into something COUNTED rather than modelled. That matters here
        // because the levers that did this before (SCATTER_SPLIT, ISORT_NE) only
        // reach loops the fused path does not use, so without this the fused
        // build's level count and reversal cost are both guesses.
        let fu_pad = std::env::var("KARAC_SORT_FU_PAD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let rev_pad = std::env::var("KARAC_SORT_REV_PAD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let pad_a = (fused && (fu_pad > 0 || rev_pad > 0))
            .then(|| self.create_entry_alloca(qpart_fn, "qpad", i64_t.into()));
        let bool_t = self.context.bool_type();
        let fu_slots = fu.map(|_| {
            (
                self.create_entry_alloca(qpart_fn, "nolt", bool_t.into()),
                self.create_entry_alloca(qpart_fn, "req", bool_t.into()),
                self.create_entry_alloca(qpart_fn, "rv", i64_t.into()),
            )
        });

        let len = self.builder.build_int_sub(hi, lo, "q.len").unwrap();
        let elem_size = elem_ty.size_of().unwrap();
        let span_bytes = self
            .builder
            .build_int_mul(len, elem_size, "q.bytes")
            .unwrap();
        let in_a_b = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, in_a, zero, "q.ina")
            .unwrap();
        let live = self
            .builder
            .build_select(in_a_b, data, scratch, "q.live")
            .unwrap()
            .into_pointer_value();
        let dead = self
            .builder
            .build_select(in_a_b, scratch, data, "q.dead")
            .unwrap()
            .into_pointer_value();
        // Addresses of this range in each buffer — the only two the copies use.
        let d_lo = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[lo], "q.d.lo")
                .unwrap()
        };
        let s_lo = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, scratch, &[lo], "q.s.lo")
                .unwrap()
        };
        let small = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLE,
                len,
                i64_t.const_int(span_v, false),
                "q.small",
            )
            .unwrap();
        let deep = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGE,
                depth,
                i64_t.const_int(DEPTH_MAX, false),
                "q.deep",
            )
            .unwrap();
        // Computed here rather than in `leaf`: `alleq` needs it too, and
        // `leaf` does not dominate `alleq`.
        let need_cp = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, in_a, zero, "q.needcp")
            .unwrap();
        let bail = self.builder.build_or(small, deep, "q.bail").unwrap();
        self.builder
            .build_conditional_branch(bail, leaf, pv)
            .unwrap();

        // ── leaf: bring the range home to `data`, then sort it ─────────────
        // `allow_part = 0` on the merge callee: without it an abandoned range
        // would be re-probed, accepted by the sampling estimate that the exact
        // tie count just rejected, and handed straight back here — the same
        // range, the same pivot, forever.
        //
        // TWO leaf sorters, chosen on length (B-2026-08-16-3). A range that
        // arrived because it is SHORT (`small`) gets the insertion sort; one
        // that arrived from the tie gate or the depth backstop can be any size
        // at all and must keep the merge, which is O(n log n).
        //
        // Why not just keep the merge everywhere: measured, the leaves were
        // 46.4% of the whole shuffled sort — 162.9M cycles against driftsort's
        // 194.0M for the entire sort — while the partition tree above them was
        // only 188.2M. Sorting a 64-element block with the natural-run merge
        // costs an allocation pair, run detection, and ~log2(64/RUN) passes on
        // input whose natural runs are ~2 long. Insertion needs no buffer, no
        // run detection, and one pass with a short shift distance.
        self.builder.position_at_end(leaf);
        self.builder
            .build_conditional_branch(need_cp, leaf_cp, leaf_call)
            .unwrap();
        self.builder.position_at_end(leaf_cp);
        self.builder
            .build_memcpy(d_lo, 8, s_lo, 8, span_bytes)
            .unwrap();
        self.builder.build_unconditional_branch(leaf_call).unwrap();
        self.builder.position_at_end(leaf_call);
        // `KARAC_SORT_LEAF=merge` pins the pre-B-2026-08-16-3 behaviour, so the
        // two leaf sorters stay A/B-able from one karac — the same lever shape
        // as `KARAC_SORT_FORCE`. `insertion` forces it on for any length, which
        // is only meaningful as a measurement (it is O(n^2) above the span).
        let leaf_short = match std::env::var("KARAC_SORT_LEAF").as_deref() {
            Ok("merge") => self.context.bool_type().const_zero(),
            Ok("insertion") => self.context.bool_type().const_int(1, false),
            _ => self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::SLE,
                    len,
                    i64_t.const_int(span_v, false),
                    "q.leaf.short",
                )
                .unwrap(),
        };
        self.builder
            .build_conditional_branch(leaf_short, leaf_ins, leaf_merge)
            .unwrap();
        self.builder.position_at_end(leaf_ins);
        self.builder
            .build_call(isort_fn, &[d_lo.into(), len.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(ret).unwrap();
        self.builder.position_at_end(leaf_merge);
        self.builder
            .build_call(sort_fn, &[d_lo.into(), len.into(), zero.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(ret).unwrap();

        // ── pivot: median of three splitmix-chosen samples ─────────────────
        // A FIXED-POSITION median-of-3 is not interchangeable here. Sampling
        // lo / lo+len/2 / hi-1 is degenerate on periodic input: on a sawtooth
        // those hold 0, 0 and 999, the median is 0, and 0 is the range MINIMUM,
        // so each level peels off only the copies of the minimum. Measured at
        // 2328.7M instructions against the merge's 23.8M — ~100x.
        self.builder.position_at_end(pv);
        let h_lo = self
            .builder
            .build_int_mul(lo, i64_t.const_int(0x9E37_79B9_7F4A_7C15, false), "q.h1")
            .unwrap();
        let h_hi = self
            .builder
            .build_int_mul(hi, i64_t.const_int(0xBF58_476D_1CE4_E5B9, false), "q.h2")
            .unwrap();
        let h_dp = self
            .builder
            .build_int_mul(depth, i64_t.const_int(0x94D0_49BB_1331_11EB, false), "q.h3")
            .unwrap();
        let hx = self.builder.build_xor(h_lo, h_hi, "q.hx").unwrap();
        let seed0 = self.builder.build_xor(hx, h_dp, "q.seed").unwrap();
        let s1 = self.emit_splitmix64(seed0);
        let s2 = self.emit_splitmix64(s1);
        let s3 = self.emit_splitmix64(s2);
        // Lemire's multiply-shift range reduction rather than `s % len`
        // (B-2026-08-16-9): `(s * len) >> 64` is uniform enough over [0,len) and
        // lowers to a single `umulh`, where the remainder needs a runtime
        // `udiv` + `msub` — three of them per call, on a divider that is not
        // pipelined. Nothing here needs a rigorously unbiased index: ANY index
        // in range is a valid pivot, so the residual modulo bias buys nothing.
        // `s < 2^64` and `len >= 1` gives `s*len < len * 2^64`, so the high word
        // is strictly below `len` and the sample stays in range.
        //
        // `KARAC_SORT_PIVOT_REM=1` restores the remainder for A/B.
        let use_rem = std::env::var("KARAC_SORT_PIVOT_REM").as_deref() == Ok("1");
        let i128_t = self.context.i128_type();
        let mut idxs = Vec::with_capacity(3);
        for (n, s) in [("a", s1), ("b", s2), ("c", s3)] {
            let m = if use_rem {
                self.builder
                    .build_int_unsigned_rem(s, len, &format!("q.pv.{n}.m"))
                    .unwrap()
            } else {
                let sw = self
                    .builder
                    .build_int_z_extend(s, i128_t, &format!("q.pv.{n}.sw"))
                    .unwrap();
                let lw = self
                    .builder
                    .build_int_z_extend(len, i128_t, &format!("q.pv.{n}.lw"))
                    .unwrap();
                let pr = self
                    .builder
                    .build_int_mul(sw, lw, &format!("q.pv.{n}.pr"))
                    .unwrap();
                let hi_w = self
                    .builder
                    .build_right_shift(
                        pr,
                        i128_t.const_int(64, false),
                        false,
                        &format!("q.pv.{n}.hw"),
                    )
                    .unwrap();
                self.builder
                    .build_int_truncate(hi_w, i64_t, &format!("q.pv.{n}.m"))
                    .unwrap()
            };
            idxs.push(
                self.builder
                    .build_int_add(lo, m, &format!("q.pv.{n}.i"))
                    .unwrap(),
            );
        }
        let mut vals = Vec::with_capacity(3);
        for (n, ix) in ["a", "b", "c"].iter().zip(&idxs) {
            let addr = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_ty, live, &[*ix], &format!("q.pv.{n}.p"))
                    .unwrap()
            };
            vals.push(
                self.builder
                    .build_load(elem_ty, addr, &format!("q.pv.{n}.v"))
                    .unwrap(),
            );
        }
        // Select the median's INDEX rather than its value: i64 selects are
        // unconditionally safe for every element type this path admits.
        let c_ab = self.emit_sort_by_inline_compare(
            qpart_fn,
            params,
            body,
            elem_type_name,
            vals[0],
            vals[1],
        )?;
        let c_bc = self.emit_sort_by_inline_compare(
            qpart_fn,
            params,
            body,
            elem_type_name,
            vals[1],
            vals[2],
        )?;
        let c_ac = self.emit_sort_by_inline_compare(
            qpart_fn,
            params,
            body,
            elem_type_name,
            vals[0],
            vals[2],
        )?;
        let lt = |s: &mut Self, c: IntValue<'ctx>, n: &str| {
            s.builder
                .build_int_compare(inkwell::IntPredicate::SLT, c, zero, n)
                .unwrap()
        };
        let ab = lt(self, c_ab, "q.ab");
        let bc = lt(self, c_bc, "q.bc");
        let ac = lt(self, c_ac, "q.ac");
        let t1 = self
            .builder
            .build_select(ac, idxs[2], idxs[0], "q.t1")
            .unwrap()
            .into_int_value();
        let m1 = self
            .builder
            .build_select(bc, idxs[1], t1, "q.m1")
            .unwrap()
            .into_int_value();
        let t2 = self
            .builder
            .build_select(bc, idxs[2], idxs[1], "q.t2")
            .unwrap()
            .into_int_value();
        let m2 = self
            .builder
            .build_select(ac, idxs[0], t2, "q.m2")
            .unwrap()
            .into_int_value();
        let med = self
            .builder
            .build_select(ab, m1, m2, "q.med")
            .unwrap()
            .into_int_value();
        let med_p = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, live, &[med], "q.med.p")
                .unwrap()
        };
        let med_v = self.builder.build_load(elem_ty, med_p, "q.med.v").unwrap();
        self.builder.build_store(piv_a, med_v).unwrap();
        self.builder.build_store(nlt_a, zero).unwrap();
        self.builder.build_store(nle_a, zero).unwrap();
        self.builder.build_store(ii_a, lo).unwrap();
        let mut fu_inv_slot: Option<inkwell::values::IntValue<'ctx>> = None;
        match fu {
            // The fused pass seeds its cursors here; the count path overwrites
            // both in `pick`, so these stores are dead on that route.
            Some((fu_chk, ..)) => {
                let hi_1 = self.builder.build_int_sub(hi, one, "q.hi.1").unwrap();
                self.builder.build_store(lc_a, lo).unwrap();
                self.builder.build_store(rc_a, hi_1).unwrap();
                // `hi - 1`, the invariant half of the right destination.
                // DERIVATION, because the `lo` terms cancel and adding one is
                // an off-by-lo that mis-sorts silently at lo > 0 (it crashed
                // outright here, which was luck): with `k` the count of `>=`
                // seen and `lcur = lo + nlt`, the destination is
                //   hi-1-k = hi-1-((i-lo)-nlt) = hi-1-i+lo+nlt = (hi-1)-i+lcur.
                fu_inv_slot = Some(hi_1);
                if fused_mode >= 2 {
                    self.builder.build_unconditional_branch(fu_chk).unwrap();
                } else {
                    let unstructured = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::EQ, gate, zero, "q.unstr")
                        .unwrap();
                    self.builder
                        .build_conditional_branch(unstructured, fu_chk, cnt_chk)
                        .unwrap();
                }
            }
            None => {
                self.builder.build_unconditional_branch(cnt_chk).unwrap();
            }
        }

        // ── count: one pass tallies BOTH `< pivot` and `<= pivot` ──────────
        // Two tallies from one comparison is what lets the split predicate be
        // chosen without a second pass, folds the old `t = 1` retry into the
        // same pass, and hands the gate below its answer for free.
        self.builder.position_at_end(cnt_chk);
        let ci = self
            .builder
            .build_load(i64_t, ii_a, "q.ci")
            .unwrap()
            .into_int_value();
        let c_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, ci, hi, "q.c.go")
            .unwrap();
        self.builder
            .build_conditional_branch(c_go, cnt_body, gate_chk)
            .unwrap();

        self.builder.position_at_end(cnt_body);
        let cv_p = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, live, &[ci], "q.cv.p")
                .unwrap()
        };
        let cv = self.builder.build_load(elem_ty, cv_p, "q.cv").unwrap();
        let pv_v = self.builder.build_load(elem_ty, piv_a, "q.pv.v").unwrap();
        let cc =
            self.emit_sort_by_inline_compare(qpart_fn, params, body, elem_type_name, cv, pv_v)?;
        let is_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, cc, zero, "q.is.lt")
            .unwrap();
        let is_le = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, cc, zero, "q.is.le")
            .unwrap();
        for (slot, bit, n) in [(nlt_a, is_lt, "nlt"), (nle_a, is_le, "nle")] {
            let cur = self
                .builder
                .build_load(i64_t, slot, &format!("q.{n}.v"))
                .unwrap()
                .into_int_value();
            let inc = self
                .builder
                .build_int_z_extend(bit, i64_t, &format!("q.{n}.z"))
                .unwrap();
            let nv = self
                .builder
                .build_int_add(cur, inc, &format!("q.{n}.n"))
                .unwrap();
            self.builder.build_store(slot, nv).unwrap();
        }
        let ci_n = self
            .builder
            .build_load(i64_t, ii_a, "q.ci2")
            .unwrap()
            .into_int_value();
        let ci_n1 = self.builder.build_int_add(ci_n, one, "q.ci.n").unwrap();
        self.builder.build_store(ii_a, ci_n1).unwrap();
        let cnt_latch = self.builder.build_unconditional_branch(cnt_chk).unwrap();
        // The count pass is a pure branchless reduction and vectorises 8-wide
        // in isolation (3.25 instr/elem) but is DECLINED BY THE COST MODEL in
        // the shipped compiler (5.00, scalar) — B-2026-08-16-9. The hint
        // overrides the cost model only; if the loop were illegal to vectorise
        // this would change nothing. `KARAC_SORT_COUNT_VEC=0` opts out.
        if std::env::var("KARAC_SORT_COUNT_VEC").as_deref() != Ok("0") {
            self.attach_vectorize_enable_metadata(cnt_latch);
        }

        // ── gate: abandon, having written nothing ──────────────────────────
        // `neq / len` is an unbiased estimate of 1 / distinct-keys for a
        // randomly chosen pivot, and it is already computed. A range that
        // fails is merged exactly as it is today, so the decision is per
        // range, not a mode: a mixed input partitions the part that pays.
        self.builder.position_at_end(gate_chk);
        let nlt_v = self
            .builder
            .build_load(i64_t, nlt_a, "q.nlt")
            .unwrap()
            .into_int_value();
        let nle_v = self
            .builder
            .build_load(i64_t, nle_a, "q.nle")
            .unwrap()
            .into_int_value();
        let neq = self.builder.build_int_sub(nle_v, nlt_v, "q.neq").unwrap();
        let scaled = self
            .builder
            .build_int_mul(neq, i64_t.const_int(GATE, false), "q.neq.s")
            .unwrap();
        // Only the low-cardinality arm applies this test — `gate == 0` means
        // the range was admitted for being UNSTRUCTURED, where a tie count is
        // near zero by construction and this would reject every range, fall
        // back to the merge, and have paid a counting pass for nothing. That
        // is not hypothetical: forcing the entry probe open while leaving this
        // gate in place measured 550M vs the merge's 545M on random, which
        // reads as "the partition does not help" when the partition never ran.
        let tie_poor = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, scaled, len, "q.tie.poor")
            .unwrap();
        let gate_on = self
            .builder
            .build_int_compare(inkwell::IntPredicate::NE, gate, zero, "q.gate.on")
            .unwrap();
        let poor = self.builder.build_and(gate_on, tie_poor, "q.poor").unwrap();
        self.builder
            .build_conditional_branch(poor, leaf, alleq_chk)
            .unwrap();

        // ── all-equal: sorted and stable already; this is the early exit ───
        self.builder.position_at_end(alleq_chk);
        let no_lt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, nlt_v, zero, "q.nolt")
            .unwrap();
        let all_le = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, nle_v, len, "q.allle")
            .unwrap();
        let is_eq = self.builder.build_and(no_lt, all_le, "q.iseq").unwrap();
        self.builder
            .build_conditional_branch(is_eq, alleq, pick)
            .unwrap();

        self.builder.position_at_end(alleq);
        self.builder
            .build_conditional_branch(need_cp, alleq_cp, ret)
            .unwrap();
        self.builder.position_at_end(alleq_cp);
        self.builder
            .build_memcpy(d_lo, 8, s_lo, 8, span_bytes)
            .unwrap();
        self.builder.build_unconditional_branch(ret).unwrap();

        // ── pick the split predicate ───────────────────────────────────────
        //   nlt > 0            split on `<` at nlt; the right half is entirely
        //                      equal iff nle == len (the pivot was the maximum)
        //   nlt == 0           split on `<=` at nle; the LEFT half is then the
        //                      block equal to the pivot — sorted, stable, and
        //                      needing no recursion
        // Both halves are always non-empty (the pivot is drawn from the range),
        // which is what makes the recursion terminate.
        self.builder.position_at_end(pick);
        let split = self
            .builder
            .build_select(no_lt, nle_v, nlt_v, "q.split")
            .unwrap()
            .into_int_value();
        self.builder.build_store(split_a, split).unwrap();
        let not_lt0 = self.builder.build_not(no_lt, "q.notlt0").unwrap();
        let right_eq = self.builder.build_and(not_lt0, all_le, "q.req").unwrap();
        let mid = self.builder.build_int_add(lo, split, "q.mid").unwrap();
        self.builder.build_store(lc_a, lo).unwrap();
        self.builder.build_store(rc_a, mid).unwrap();
        self.builder.build_store(ii_a, lo).unwrap();
        if let Some((nolt_a, req_a, _)) = fu_slots {
            self.builder.build_store(nolt_a, no_lt).unwrap();
            self.builder.build_store(req_a, right_eq).unwrap();
        }
        match sc_chk_le {
            Some(le) => self
                .builder
                .build_conditional_branch(no_lt, le, sc_chk)
                .unwrap(),
            None => self.builder.build_unconditional_branch(sc_chk).unwrap(),
        };

        // ── scatter: one in-order pass, two advancing cursors ──────────────
        // This is the whole stability argument: elements are visited in
        // original order and appended to their side, so relative order is
        // preserved within each half. Branchless — the destination index is a
        // select and both cursors advance by a bool.
        //
        // EMITTED ONCE PER SPLIT PREDICATE (B-2026-08-16-9). Which of `<` and
        // `<=` decides the side is fixed for the whole range — it is `no_lt`,
        // computed before the loop — but writing that as an in-loop
        // `select(no_lt, sc <= 0, sc < 0)` costs six instructions per element on
        // arm64, because the select needs its operands as VALUES and so forces
        // the comparator's three-way result to be materialised:
        //
        //   cmp x16, x10 / cset w16, lt / cset w17, le      <- both predicates
        //   cmp x12, #0  / csel w16, w17, w16, eq           <- invariant, in-loop
        //   cmp w16, #0  / csel x17, x15, x14, ne
        //
        // The counting pass above is the control: same comparator, same two
        // predicates, but consumed directly as condition codes rather than
        // through a select, and it compiles to `cmp / cinc lt / cinc le`. With
        // one predicate per arm the scatter reaches the same form — `cmp / csel
        // / cinc / cinc` — for 9 instructions per element against 15.
        //
        // LLVM will not do this itself: SimpleLoopUnswitch unswitches invariant
        // BRANCHES, and this is a select. Nor is folding the two predicates into
        // one comparison against a precomputed threshold an option — `sc < thr`
        // for a runtime `thr` would materialise `sc`, which is the cost being
        // removed. Duplicating the loop is what keeps the comparison in flags.
        //
        // `KARAC_SORT_SCATTER_SPLIT=0` restores the single fused loop for A/B.
        let arms: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>, Option<bool>)> =
            match (sc_chk_le, sc_body_le) {
                (Some(c), Some(b)) => vec![(sc_chk, sc_body, Some(false)), (c, b, Some(true))],
                _ => vec![(sc_chk, sc_body, None)],
            };
        for (chk, body_bb, pred) in arms {
            self.builder.position_at_end(chk);
            let si = self
                .builder
                .build_load(i64_t, ii_a, "q.si")
                .unwrap()
                .into_int_value();
            let s_go = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, si, hi, "q.s.go")
                .unwrap();
            self.builder
                .build_conditional_branch(s_go, body_bb, rec)
                .unwrap();

            self.builder.position_at_end(body_bb);
            let sv_p = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_ty, live, &[si], "q.sv.p")
                    .unwrap()
            };
            let sv = self.builder.build_load(elem_ty, sv_p, "q.sv").unwrap();
            let pv_v2 = self.builder.build_load(elem_ty, piv_a, "q.pv.v2").unwrap();
            let sc = self.emit_sort_by_inline_compare(
                qpart_fn,
                params,
                body,
                elem_type_name,
                sv,
                pv_v2,
            )?;
            let goes_left = match pred {
                // `nlt == 0`: the pivot is the range minimum, so the left half
                // is the block equal to it and the split is on `<=`.
                Some(true) => self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLE, sc, zero, "q.s.le")
                    .unwrap(),
                Some(false) => self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, sc, zero, "q.s.lt")
                    .unwrap(),
                None => {
                    let s_lt = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SLT, sc, zero, "q.s.lt")
                        .unwrap();
                    let s_le = self
                        .builder
                        .build_int_compare(inkwell::IntPredicate::SLE, sc, zero, "q.s.le")
                        .unwrap();
                    self.builder
                        .build_select(no_lt, s_le, s_lt, "q.left")
                        .unwrap()
                        .into_int_value()
                }
            };
            let lcur = self
                .builder
                .build_load(i64_t, lc_a, "q.lc")
                .unwrap()
                .into_int_value();
            let rcur = self
                .builder
                .build_load(i64_t, rc_a, "q.rc")
                .unwrap()
                .into_int_value();
            let dst_i = self
                .builder
                .build_select(goes_left, lcur, rcur, "q.dsti")
                .unwrap()
                .into_int_value();
            let dst_p = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_ty, dead, &[dst_i], "q.dst.p")
                    .unwrap()
            };
            self.builder.build_store(dst_p, sv).unwrap();
            let l_inc = self
                .builder
                .build_int_z_extend(goes_left, i64_t, "q.linc")
                .unwrap();
            let r_bit = self.builder.build_not(goes_left, "q.rbit").unwrap();
            let r_inc = self
                .builder
                .build_int_z_extend(r_bit, i64_t, "q.rinc")
                .unwrap();
            let l_n = self.builder.build_int_add(lcur, l_inc, "q.lc.n").unwrap();
            let r_n = self.builder.build_int_add(rcur, r_inc, "q.rc.n").unwrap();
            self.builder.build_store(lc_a, l_n).unwrap();
            self.builder.build_store(rc_a, r_n).unwrap();
            let si2 = self
                .builder
                .build_load(i64_t, ii_a, "q.si2")
                .unwrap()
                .into_int_value();
            let si_n = self.builder.build_int_add(si2, one, "q.si.n").unwrap();
            self.builder.build_store(ii_a, si_n).unwrap();
            self.builder.build_unconditional_branch(chk).unwrap();
        }

        // ── fused: one in-order pass, cursors converging from both ends ────
        if let (
            Some((fu_chk, fu_body, fu_done, fu_retry, fu_rev_chk, fu_rev_body)),
            Some((nolt_a, req_a, rv_a)),
        ) = (fu, fu_slots)
        {
            self.builder.position_at_end(fu_chk);
            let fi = self
                .builder
                .build_load(i64_t, ii_a, "q.fi")
                .unwrap()
                .into_int_value();
            let f_go = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, fi, hi, "q.f.go")
                .unwrap();
            self.builder
                .build_conditional_branch(f_go, fu_body, fu_done)
                .unwrap();

            // Same shape as one arm of the split scatter — a single predicate
            // consumed as flags, a `csel` for the destination — except the right
            // cursor DESCENDS, which is what removes the need for a counted base.
            self.builder.position_at_end(fu_body);
            let fv_p = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_ty, live, &[fi], "q.fv.p")
                    .unwrap()
            };
            let fv = self.builder.build_load(elem_ty, fv_p, "q.fv").unwrap();
            let fpv = self.builder.build_load(elem_ty, piv_a, "q.fpv").unwrap();
            let fc =
                self.emit_sort_by_inline_compare(qpart_fn, params, body, elem_type_name, fv, fpv)?;
            let f_left = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, fc, zero, "q.f.lt")
                .unwrap();
            let flc = self
                .builder
                .build_load(i64_t, lc_a, "q.flc")
                .unwrap()
                .into_int_value();
            // THE DESCENDING CURSOR IS THE DEFAULT, AND IT IS THE FASTER ONE
            // DESPITE COSTING MORE INSTRUCTIONS — the alternative was built and
            // measured rather than argued about, and it loses.
            //
            // The alternative (`KARAC_SORT_FUSED_ADDR=affine`): every element
            // goes exactly one way, so `l + k == i - lo` and the right
            // destination `hi-1-k` equals `(hi-1) - i + lcur`. That needs no
            // descending cursor at all — both cursors ascend, both advance with
            // `cinc` straight out of the flags, and the loop drops from 11
            // instructions to 9. It is correct (six patterns, 462-case
            // stability sweep) and it removes 13.7 instructions per element.
            //
            // It buys NOTHING: 279.1 -> 265.4 instr/elem for 66.89 -> 66.96
            // cycles, 1.001x, layout-validated across five emission orders.
            // IPC falls 4.17 -> 3.96 and eats the entire saving. The reason is
            // the identity itself — deriving the right index FROM `lcur` makes
            // the two cursors one loop-carried dependency chain, where a
            // descending `rcur` is an independent chain advancing in parallel
            // with the ascending one. The two extra instructions buy cursor
            // independence and are worth exactly what they cost.
            let fu_inv = fu_inv_slot.expect("fused preheader must have run");
            let descend = std::env::var("KARAC_SORT_FUSED_ADDR").as_deref() != Ok("affine");
            let f_dsti = if descend {
                let frc = self
                    .builder
                    .build_load(i64_t, rc_a, "q.frc")
                    .unwrap()
                    .into_int_value();
                let d = self
                    .builder
                    .build_select(f_left, flc, frc, "q.f.dsti")
                    .unwrap()
                    .into_int_value();
                let f_rbit = self.builder.build_not(f_left, "q.f.rbit").unwrap();
                let f_rdec = self
                    .builder
                    .build_int_z_extend(f_rbit, i64_t, "q.f.rdec")
                    .unwrap();
                let frc_n = self.builder.build_int_sub(frc, f_rdec, "q.frc.n").unwrap();
                self.builder.build_store(rc_a, frc_n).unwrap();
                d
            } else {
                let back = self.builder.build_int_sub(fu_inv, fi, "q.f.back").unwrap();
                let off = self
                    .builder
                    .build_select(f_left, zero, back, "q.f.off")
                    .unwrap()
                    .into_int_value();
                self.builder.build_int_add(flc, off, "q.f.dsti").unwrap()
            };
            let f_dstp = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_ty, dead, &[f_dsti], "q.f.dst.p")
                    .unwrap()
            };
            self.builder.build_store(f_dstp, fv).unwrap();
            let f_linc = self
                .builder
                .build_int_z_extend(f_left, i64_t, "q.f.linc")
                .unwrap();
            let flc_n = self.builder.build_int_add(flc, f_linc, "q.flc.n").unwrap();
            self.builder.build_store(lc_a, flc_n).unwrap();
            let fi2 = self
                .builder
                .build_load(i64_t, ii_a, "q.fi2")
                .unwrap()
                .into_int_value();
            let fi_n = self.builder.build_int_add(fi2, one, "q.fi.n").unwrap();
            self.builder.build_store(ii_a, fi_n).unwrap();
            if let Some(p) = pad_a {
                for _ in 0..fu_pad {
                    let st = self.builder.build_store(p, zero).unwrap();
                    st.set_volatile(true).unwrap();
                }
            }
            self.builder.build_unconditional_branch(fu_chk).unwrap();

            // The cursors met at `lo + nlt`, so the left cursor IS the split.
            self.builder.position_at_end(fu_done);
            let f_lend = self
                .builder
                .build_load(i64_t, lc_a, "q.f.lend")
                .unwrap()
                .into_int_value();
            let f_nlt = self.builder.build_int_sub(f_lend, lo, "q.f.nlt").unwrap();
            let f_empty = self
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, f_nlt, zero, "q.f.empty")
                .unwrap();
            self.builder.build_store(split_a, f_nlt).unwrap();
            // The fused route always splits on `<` and never proves the right
            // half all-equal, so both routing flags are false. The all-equal
            // early exit is given up with them: on unstructured data a range
            // whose every element ties the pivot is vanishingly rare, and the
            // cost of missing it is one extra level, not a wrong answer.
            let f_false = bool_t.const_zero();
            self.builder.build_store(nolt_a, f_false).unwrap();
            self.builder.build_store(req_a, f_false).unwrap();
            self.builder.build_store(ii_a, f_lend).unwrap();
            let f_hi1 = self.builder.build_int_sub(hi, one, "q.f.hi1").unwrap();
            self.builder.build_store(rv_a, f_hi1).unwrap();
            self.builder
                .build_conditional_branch(f_empty, fu_retry, fu_rev_chk)
                .unwrap();

            // `dead` is scrap on this route — the count path rebuilds it from
            // `live`, which the fused pass never wrote.
            self.builder.position_at_end(fu_retry);
            self.builder.build_store(nlt_a, zero).unwrap();
            self.builder.build_store(nle_a, zero).unwrap();
            self.builder.build_store(ii_a, lo).unwrap();
            self.builder.build_unconditional_branch(cnt_chk).unwrap();

            // Restore stability: the `>=` side was appended descending, so it
            // holds the right elements in reverse of their original order.
            self.builder.position_at_end(fu_rev_chk);
            let ra = self
                .builder
                .build_load(i64_t, ii_a, "q.ra")
                .unwrap()
                .into_int_value();
            let rb = self
                .builder
                .build_load(i64_t, rv_a, "q.rb")
                .unwrap()
                .into_int_value();
            let r_go = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, ra, rb, "q.r.go")
                .unwrap();
            self.builder
                .build_conditional_branch(r_go, fu_rev_body, rec)
                .unwrap();

            self.builder.position_at_end(fu_rev_body);
            let ra_p = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_ty, dead, &[ra], "q.ra.p")
                    .unwrap()
            };
            let rb_p = unsafe {
                self.builder
                    .build_in_bounds_gep(elem_ty, dead, &[rb], "q.rb.p")
                    .unwrap()
            };
            let va = self.builder.build_load(elem_ty, ra_p, "q.va").unwrap();
            let vb = self.builder.build_load(elem_ty, rb_p, "q.vb").unwrap();
            self.builder.build_store(ra_p, vb).unwrap();
            self.builder.build_store(rb_p, va).unwrap();
            let ra_n = self.builder.build_int_add(ra, one, "q.ra.n").unwrap();
            let rb_n = self.builder.build_int_sub(rb, one, "q.rb.n").unwrap();
            self.builder.build_store(ii_a, ra_n).unwrap();
            self.builder.build_store(rv_a, rb_n).unwrap();
            if let Some(p) = pad_a {
                for _ in 0..rev_pad {
                    let st = self.builder.build_store(p, zero).unwrap();
                    st.set_volatile(true).unwrap();
                }
            }
            // DELIBERATELY NOT VECTORISED. The count pass this replaces was
            // 8-wide NEON, so widening the swap is the obvious way to pay for
            // the +2.2% instructions the reversal costs — but the same
            // `vectorize.enable` hint the count uses is INERT here and was
            // removed rather than left in. A two-ended swap needs a reversing
            // shuffle, which LLVM's loop vectoriser does not form, so the hint
            // changed the instruction count by 0.1 per element (279.2 -> 279.1)
            // while emitting a "loop not vectorized" warning for every sort in
            // the program. Widening this loop needs an explicit shuffle in the
            // emitter, not a request to the cost model.
            self.builder.build_unconditional_branch(fu_rev_chk).unwrap();
        }

        // ── recurse: the scatter moved both halves to the other buffer ─────
        self.builder.position_at_end(rec);
        // Both routes converge here, so the routing flags come from memory when
        // the fused path exists (see `fu_slots`).
        let (no_lt, right_eq) = match fu_slots {
            Some((nolt_a, req_a, _)) => (
                self.builder
                    .build_load(bool_t, nolt_a, "q.nolt.v")
                    .unwrap()
                    .into_int_value(),
                self.builder
                    .build_load(bool_t, req_a, "q.req.v")
                    .unwrap()
                    .into_int_value(),
            ),
            None => (no_lt, right_eq),
        };
        let nin = self.builder.build_int_sub(one, in_a, "q.nin").unwrap();
        let dep_n = self.builder.build_int_add(depth, one, "q.dep.n").unwrap();
        let split_v = self
            .builder
            .build_load(i64_t, split_a, "q.split.v")
            .unwrap()
            .into_int_value();
        let mid_v = self.builder.build_int_add(lo, split_v, "q.mid.v").unwrap();
        // A half that is entirely equal is finished — only its parity needs
        // fixing. `nin == 0` means the live copy is in scratch.
        let nin_z = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, nin, zero, "q.nin.z")
            .unwrap();
        self.builder
            .build_conditional_branch(no_lt, l_eq, l_rec)
            .unwrap();

        self.builder.position_at_end(l_eq);
        self.builder
            .build_conditional_branch(nin_z, l_eq_cp, r_disp)
            .unwrap();
        self.builder.position_at_end(l_eq_cp);
        let l_bytes = self
            .builder
            .build_int_mul(split_v, elem_size, "q.l.bytes")
            .unwrap();
        self.builder
            .build_memcpy(d_lo, 8, s_lo, 8, l_bytes)
            .unwrap();
        self.builder.build_unconditional_branch(r_disp).unwrap();

        self.builder.position_at_end(l_rec);
        self.builder
            .build_call(
                qpart_fn,
                &[
                    data.into(),
                    scratch.into(),
                    lo.into(),
                    mid_v.into(),
                    nin.into(),
                    dep_n.into(),
                    gate.into(),
                ],
                "",
            )
            .unwrap();
        self.builder.build_unconditional_branch(r_disp).unwrap();

        self.builder.position_at_end(r_disp);
        self.builder
            .build_conditional_branch(right_eq, r_eq, r_rec)
            .unwrap();

        self.builder.position_at_end(r_eq);
        self.builder
            .build_conditional_branch(nin_z, r_eq_cp, ret)
            .unwrap();
        self.builder.position_at_end(r_eq_cp);
        let d_mid = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[mid_v], "q.d.mid")
                .unwrap()
        };
        let s_mid = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, scratch, &[mid_v], "q.s.mid")
                .unwrap()
        };
        let r_len = self.builder.build_int_sub(hi, mid_v, "q.r.len").unwrap();
        let r_bytes = self
            .builder
            .build_int_mul(r_len, elem_size, "q.r.bytes")
            .unwrap();
        self.builder
            .build_memcpy(d_mid, 8, s_mid, 8, r_bytes)
            .unwrap();
        self.builder.build_unconditional_branch(ret).unwrap();

        self.builder.position_at_end(r_rec);
        self.builder
            .build_call(
                qpart_fn,
                &[
                    data.into(),
                    scratch.into(),
                    mid_v.into(),
                    hi.into(),
                    nin.into(),
                    dep_n.into(),
                    gate.into(),
                ],
                "",
            )
            .unwrap();
        self.builder.build_unconditional_branch(ret).unwrap();

        self.builder.position_at_end(ret);
        self.builder.build_return(None).unwrap();

        self.fn_ctx.loop_stack = saved_loop_stack;
        self.var_types.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_state.closure_fn_types = saved_cfn;
        self.closure_state.pending_closure_fn_type = saved_pct;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(b) = saved_bb {
            self.builder.position_at_end(b);
        }
        Ok(())
    }

    /// Emit the body of `__vec_<m>_isort_<id>` — the partition's leaf sorter
    /// (B-2026-08-16-3). `void isort(data, len)` sorts `[0,len)` in place.
    ///
    /// STABLE, and by the same argument phase 1's insertion padding is: an
    /// element shifts left only past elements that compare STRICTLY greater
    /// (`cmp(prev, hold) > 0`), so it comes to rest AFTER every element equal
    /// to it and relative order within a key is preserved. The two sites must
    /// not drift — both compare in that direction for that reason.
    ///
    /// Only the partition calls this, and only for a range that reached the
    /// leaf by being SHORT. It is O(n^2), so the caller gates it on length
    /// rather than letting the tie-gate or depth-backstop paths reach it.
    fn emit_sort_isort_body(
        &mut self,
        isort_fn: FunctionValue<'ctx>,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_type_name: Option<&str>,
    ) -> Result<(), String> {
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();
        let one = i64_t.const_int(1, false);

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_cfn = std::mem::take(&mut self.closure_state.closure_fn_types);
        let saved_pct = self.closure_state.pending_closure_fn_type.take();
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();
        self.current_fn = Some(isort_fn);

        let data = isort_fn.get_nth_param(0).unwrap().into_pointer_value();
        let len = isort_fn.get_nth_param(1).unwrap().into_int_value();

        let bb = |s: &Self, n: &str| s.context.append_basic_block(isort_fn, n);
        let entry = bb(self, "entry");
        let i_chk = bb(self, "i.chk");
        let i_body = bb(self, "i.body");
        let j_chk = bb(self, "j.chk");
        let j_cmp = bb(self, "j.cmp");
        let j_shift = bb(self, "j.shift");
        let j_done = bb(self, "j.done");
        let ret = bb(self, "ret");

        self.builder.position_at_end(entry);
        let hold_a = self.create_entry_alloca(isort_fn, "hold", elem_ty);
        let i_a = self.create_entry_alloca(isort_fn, "isi", i64_t.into());
        let j_a = self.create_entry_alloca(isort_fn, "isj", i64_t.into());
        self.builder.build_store(i_a, one).unwrap();
        self.builder.build_unconditional_branch(i_chk).unwrap();

        // for i in 1..len
        self.builder.position_at_end(i_chk);
        let i_v = self
            .builder
            .build_load(i64_t, i_a, "is.i")
            .unwrap()
            .into_int_value();
        let i_go = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i_v, len, "is.i.go")
            .unwrap();
        self.builder
            .build_conditional_branch(i_go, i_body, ret)
            .unwrap();

        // hold = data[i]; j = i
        self.builder.position_at_end(i_body);
        let hold_src = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[i_v], "is.hold.p")
                .unwrap()
        };
        let hold_v = self
            .builder
            .build_load(elem_ty, hold_src, "is.hold")
            .unwrap();
        self.builder.build_store(hold_a, hold_v).unwrap();
        self.builder.build_store(j_a, i_v).unwrap();
        self.builder.build_unconditional_branch(j_chk).unwrap();

        // while j > 0 && cmp(data[j-1], hold) > 0 — the bound is checked in its
        // own block so `data[j-1]` is never loaded at j == 0.
        //
        // The bound is spelled `j != 0`, not `j > 0`. They are the same test
        // here — `j` starts at `i >= 1` and is only decremented under this very
        // guard, so it can never go negative — but the signed form costs two
        // extra instructions per shift step on arm64. LLVM rewrites the
        // induction variable to `j-1` and then re-derives the old `j` just to
        // run the comparison, emitting `add x13, x10, #1 / cmp x13, #1 / b.gt`
        // where `cbnz x10` would do. An equality test against zero has no such
        // reassociation to get wrong. `KARAC_SORT_ISORT_NE=0` restores `> 0`.
        self.builder.position_at_end(j_chk);
        let j_v = self
            .builder
            .build_load(i64_t, j_a, "is.j")
            .unwrap()
            .into_int_value();
        let j_pred = match std::env::var("KARAC_SORT_ISORT_NE").as_deref() {
            Ok("0") => inkwell::IntPredicate::SGT,
            _ => inkwell::IntPredicate::NE,
        };
        let j_go = self
            .builder
            .build_int_compare(j_pred, j_v, zero, "is.j.go")
            .unwrap();
        self.builder
            .build_conditional_branch(j_go, j_cmp, j_done)
            .unwrap();

        self.builder.position_at_end(j_cmp);
        let j_v1 = self
            .builder
            .build_load(i64_t, j_a, "is.j1")
            .unwrap()
            .into_int_value();
        let jm1 = self.builder.build_int_sub(j_v1, one, "is.jm1").unwrap();
        let prev_p = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jm1], "is.prev.p")
                .unwrap()
        };
        let prev_v = self.builder.build_load(elem_ty, prev_p, "is.prev").unwrap();
        let hold_v1 = self
            .builder
            .build_load(elem_ty, hold_a, "is.hold1")
            .unwrap();
        let c = self.emit_sort_by_inline_compare(
            isort_fn,
            params,
            body,
            elem_type_name,
            prev_v,
            hold_v1,
        )?;
        let c_gt = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, c, zero, "is.cmp.gt")
            .unwrap();
        self.builder
            .build_conditional_branch(c_gt, j_shift, j_done)
            .unwrap();

        // data[j] = data[j-1]; j -= 1
        self.builder.position_at_end(j_shift);
        let j_v2 = self
            .builder
            .build_load(i64_t, j_a, "is.j2")
            .unwrap()
            .into_int_value();
        let jm1b = self.builder.build_int_sub(j_v2, one, "is.jm1b").unwrap();
        let src_p = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[jm1b], "is.src.p")
                .unwrap()
        };
        let src_v = self.builder.build_load(elem_ty, src_p, "is.src").unwrap();
        let dst_p = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[j_v2], "is.dst.p")
                .unwrap()
        };
        self.builder.build_store(dst_p, src_v).unwrap();
        self.builder.build_store(j_a, jm1b).unwrap();
        self.builder.build_unconditional_branch(j_chk).unwrap();

        // data[j] = hold; i += 1
        self.builder.position_at_end(j_done);
        let j_v3 = self
            .builder
            .build_load(i64_t, j_a, "is.j3")
            .unwrap()
            .into_int_value();
        let hold_v2 = self
            .builder
            .build_load(elem_ty, hold_a, "is.hold2")
            .unwrap();
        let land_p = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[j_v3], "is.land.p")
                .unwrap()
        };
        self.builder.build_store(land_p, hold_v2).unwrap();
        let i_v2 = self
            .builder
            .build_load(i64_t, i_a, "is.i2")
            .unwrap()
            .into_int_value();
        let i_next = self.builder.build_int_add(i_v2, one, "is.i.next").unwrap();
        self.builder.build_store(i_a, i_next).unwrap();
        // MEASUREMENT ONLY, default 0 and inert. `KARAC_SORT_LEAFOUT_PAD=N`
        // counts the leaf's OUTER iterations — one per element handed to the
        // leaf — the way FU_PAD/REV_PAD count the fused pass and the reversal.
        // The shift loop's cost is already known; this separates the leaf's
        // per-element overhead from the per-range cost inside the partition's
        // unattributed remainder, which is the only block left unmeasured.
        let leafout_pad = std::env::var("KARAC_SORT_LEAFOUT_PAD")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if leafout_pad > 0 {
            let p = self.create_entry_alloca(isort_fn, "ispad", i64_t.into());
            for _ in 0..leafout_pad {
                let st = self.builder.build_store(p, zero).unwrap();
                st.set_volatile(true).unwrap();
            }
        }
        self.builder.build_unconditional_branch(i_chk).unwrap();

        self.builder.position_at_end(ret);
        self.builder.build_return(None).unwrap();

        self.fn_ctx.loop_stack = saved_loop_stack;
        self.var_types.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_state.closure_fn_types = saved_cfn;
        self.closure_state.pending_closure_fn_type = saved_pct;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(b) = saved_bb {
            self.builder.position_at_end(b);
        }
        Ok(())
    }

    /// Emit the body of `__vec_<m>_sprobe_<id>`: the O(1) entry probe for the
    /// partition path (B-2026-08-11-10 § Direction 7). Returns 1 if the range
    /// looks worth partitioning.
    ///
    /// It has to be O(1) in `len`, because both full-pass placements lose.
    /// Probing BEFORE phase 1 costs one counting pass (~1.3M instructions at
    /// 150k), which is 42% of an already-sorted input's entire budget — and
    /// sorted/reverse are the shapes this sort beats driftsort 7x on. Probing
    /// AFTER phase 1 is free for those, but by then phase 1 has spent ~14.5M
    /// insertion-padding an input whose natural runs are ~2 long, and the
    /// partition throws that work away.
    ///
    /// 512 samples settle both halves of the question. The count of samples
    /// TYING with a randomly chosen pivot estimates cardinality — the same
    /// quantity the partition's own counting pass gates on, so entry and
    /// recursion agree on what they are measuring — and the fraction of
    /// sampled ADJACENT PAIRS already in order estimates what phase 1's run
    /// detection would find. Both are needed: cardinality alone would send an
    /// input that is ALREADY SORTED over few keys down the partition path,
    /// when phase 1 resolves it in a single run.
    fn emit_sort_probe_body(
        &mut self,
        probe_fn: FunctionValue<'ctx>,
        params: &[ClosureParam],
        body: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_type_name: Option<&str>,
    ) -> Result<(), String> {
        const PROBE_N: u64 = 512;
        const PART_GATE: u64 = 64;
        const ORD_PCT: u64 = 95;
        /// The UNSTRUCTURED band, in percent of sampled adjacent pairs that are
        /// ordered. Inside it the merge has no runs to exploit and the
        /// partition wins; outside it the merge's natural-run pass wins, and by
        /// a lot. Measured on M5, `(i64,i64)`, n=150k, runs of length R with
        /// random starts (`qs/merge`, lower is better):
        ///
        ///   ord   0.004  0.125  0.25  0.50  0.75  0.875  0.996
        ///   ratio  3.34   1.05  0.77  0.64  0.78   1.04   3.36
        ///
        /// The curve is a V because the partition exploits no structure at all
        /// — its cost is flat across the whole sweep — while the merge's cost
        /// peaks exactly at shuffled. So the rule is a BAND around 0.5, not a
        /// threshold: reverse-sorted input samples at ord ~= 0, and a bare
        /// `ord < 95%` test would admit it and lose 13x.
        ///
        /// Edges are set one measured step inside the crossover (both 0.25 and
        /// 0.75 are wins at 0.77x / 0.78x; 0.125 and 0.875 are already ties or
        /// losses). At 512 samples the standard error on ord is ~0.022, so the
        /// edges sit >3 sigma from a true 0.5.
        const ORD_BAND_LO: u64 = 25;
        const ORD_BAND_HI: u64 = 80;

        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();
        let one = i64_t.const_int(1, false);

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_cfn = std::mem::take(&mut self.closure_state.closure_fn_types);
        let saved_pct = self.closure_state.pending_closure_fn_type.take();
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();
        self.current_fn = Some(probe_fn);

        let data = probe_fn.get_nth_param(0).unwrap().into_pointer_value();
        let len = probe_fn.get_nth_param(1).unwrap().into_int_value();

        let entry = self.context.append_basic_block(probe_fn, "entry");
        let lchk = self.context.append_basic_block(probe_fn, "l.chk");
        let lbody = self.context.append_basic_block(probe_fn, "l.body");
        let decide = self.context.append_basic_block(probe_fn, "decide");

        self.builder.position_at_end(entry);
        let s_a = self.create_entry_alloca(probe_fn, "s", i64_t.into());
        let rng_a = self.create_entry_alloca(probe_fn, "rng", i64_t.into());
        let tie_a = self.create_entry_alloca(probe_fn, "tie", i64_t.into());
        let ord_a = self.create_entry_alloca(probe_fn, "ord", i64_t.into());
        let piv_a = self.create_entry_alloca(probe_fn, "piv", elem_ty);

        let seed0 = self
            .builder
            .build_xor(
                len,
                i64_t.const_int(0xA076_1D64_78BD_642F, false),
                "pt.seed0",
            )
            .unwrap();
        let seed = self.emit_splitmix64(seed0);
        self.builder.build_store(rng_a, seed).unwrap();
        let pidx = self
            .builder
            .build_int_unsigned_rem(seed, len, "pt.pidx")
            .unwrap();
        let pp = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[pidx], "pt.pp")
                .unwrap()
        };
        let pvv = self.builder.build_load(elem_ty, pp, "pt.pv").unwrap();
        self.builder.build_store(piv_a, pvv).unwrap();
        self.builder.build_store(tie_a, zero).unwrap();
        self.builder.build_store(ord_a, zero).unwrap();
        self.builder.build_store(s_a, zero).unwrap();
        // Sampling an ADJACENT PAIR needs `idx + 1` in range, so draw from
        // `len - 1`. The caller only reaches here for `len > PART_MIN`.
        let len_m1 = self.builder.build_int_sub(len, one, "pt.len.m1").unwrap();
        self.builder.build_unconditional_branch(lchk).unwrap();

        self.builder.position_at_end(lchk);
        let sv = self
            .builder
            .build_load(i64_t, s_a, "pt.s.v")
            .unwrap()
            .into_int_value();
        let go = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                sv,
                i64_t.const_int(PROBE_N, false),
                "pt.go",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(go, lbody, decide)
            .unwrap();

        self.builder.position_at_end(lbody);
        let r = self
            .builder
            .build_load(i64_t, rng_a, "pt.r")
            .unwrap()
            .into_int_value();
        let r2 = self.emit_splitmix64(r);
        self.builder.build_store(rng_a, r2).unwrap();
        let idx = self
            .builder
            .build_int_unsigned_rem(r2, len_m1, "pt.i")
            .unwrap();
        let idx1 = self.builder.build_int_add(idx, one, "pt.i1").unwrap();
        let xp = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[idx], "pt.xp")
                .unwrap()
        };
        let x = self.builder.build_load(elem_ty, xp, "pt.x").unwrap();
        let yp = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, data, &[idx1], "pt.yp")
                .unwrap()
        };
        let y = self.builder.build_load(elem_ty, yp, "pt.y").unwrap();
        let pv2 = self.builder.build_load(elem_ty, piv_a, "pt.pv2").unwrap();
        let ct =
            self.emit_sort_by_inline_compare(probe_fn, params, body, elem_type_name, x, pv2)?;
        let tie_b = self
            .builder
            .build_int_compare(inkwell::IntPredicate::EQ, ct, zero, "pt.tie.b")
            .unwrap();
        let co = self.emit_sort_by_inline_compare(probe_fn, params, body, elem_type_name, x, y)?;
        let ord_b = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, co, zero, "pt.ord.b")
            .unwrap();
        for (slot, bit, n) in [(tie_a, tie_b, "tie"), (ord_a, ord_b, "ord")] {
            let cur = self
                .builder
                .build_load(i64_t, slot, &format!("pt.{n}.v"))
                .unwrap()
                .into_int_value();
            let inc = self
                .builder
                .build_int_z_extend(bit, i64_t, &format!("pt.{n}.z"))
                .unwrap();
            let nv = self
                .builder
                .build_int_add(cur, inc, &format!("pt.{n}.n"))
                .unwrap();
            self.builder.build_store(slot, nv).unwrap();
        }
        let sv2 = self
            .builder
            .build_load(i64_t, s_a, "pt.s2")
            .unwrap()
            .into_int_value();
        let sn = self.builder.build_int_add(sv2, one, "pt.s.n").unwrap();
        self.builder.build_store(s_a, sn).unwrap();
        self.builder.build_unconditional_branch(lchk).unwrap();

        self.builder.position_at_end(decide);
        let tie_v = self
            .builder
            .build_load(i64_t, tie_a, "pt.tie.f")
            .unwrap()
            .into_int_value();
        let ord_v = self
            .builder
            .build_load(i64_t, ord_a, "pt.ord.f")
            .unwrap()
            .into_int_value();
        let tie_scaled = self
            .builder
            .build_int_mul(tie_v, i64_t.const_int(PART_GATE, false), "pt.tie.s")
            .unwrap();
        let card_ok = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGE,
                tie_scaled,
                i64_t.const_int(PROBE_N, false),
                "pt.card.ok",
            )
            .unwrap();
        let ord_scaled = self
            .builder
            .build_int_mul(ord_v, i64_t.const_int(100, false), "pt.ord.s")
            .unwrap();
        let shuffled = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLT,
                ord_scaled,
                i64_t.const_int(PROBE_N * ORD_PCT, false),
                "pt.shuffled",
            )
            .unwrap();
        // ARM 1 — low cardinality. Unchanged: few distinct keys resolve in
        // ~log2(d) partition levels because a range that ties the pivot
        // throughout is finished, which no number of merge passes can shortcut.
        let arm_lowcard = self
            .builder
            .build_and(card_ok, shuffled, "pt.arm.lowcard")
            .unwrap();

        // ARM 2 — unstructured. New for B-2026-08-15-30. The old gate admitted
        // ONLY arm 1, because its cost model priced a partition level at two
        // merge passes ("two passes per level against the merge's one"). That
        // prices the two by pass COUNT. Measured per element on M5 they are not
        // comparable: a merge pass runs at IPC 1.93 — its READ cursor is
        // data-dependent, so every element sits on a serial
        // load -> cmp -> cursor -> load chain — against a partition scatter's
        // IPC 6.83, which reads sequentially and only picks the write
        // destination. 11.75 vs 2.49 cycles/element. So a level is cheaper than
        // a pass, and high-cardinality shuffled input wins too: 544M -> 362M,
        // 1.50x, closing the gap to Rust's driftsort from 2.73x to 1.82x.
        let ord_lo_ok = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SGE,
                ord_scaled,
                i64_t.const_int(PROBE_N * ORD_BAND_LO, false),
                "pt.ord.lo",
            )
            .unwrap();
        let ord_hi_ok = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::SLE,
                ord_scaled,
                i64_t.const_int(PROBE_N * ORD_BAND_HI, false),
                "pt.ord.hi",
            )
            .unwrap();
        let arm_band = self
            .builder
            .build_and(ord_lo_ok, ord_hi_ok, "pt.arm.band")
            .unwrap();

        // Answer with the arm, not a bool — the two want opposite per-range
        // gating inside qpart. Arm 1 wins the tie when both match, preserving
        // today's behaviour exactly on low-cardinality input.
        let two = i64_t.const_int(2, false);
        let band_v = self
            .builder
            .build_select(arm_band, two, zero, "pt.band.v")
            .unwrap()
            .into_int_value();
        let arm = self
            .builder
            .build_select(arm_lowcard, one, band_v, "pt.arm")
            .unwrap()
            .into_int_value();
        // `KARAC_SORT_PART=always|never` pins the routing, the A/B lever the
        // measurements above were taken with. `always` selects arm 2 (gate
        // off), which is what "run the partition on everything" means.
        let out = match std::env::var("KARAC_SORT_PART").as_deref() {
            Ok("always") => two,
            Ok("never") => zero,
            _ => arm,
        };
        self.builder.build_return(Some(&out)).unwrap();

        self.fn_ctx.loop_stack = saved_loop_stack;
        self.var_types.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        self.closure_state.closure_fn_types = saved_cfn;
        self.closure_state.pending_closure_fn_type = saved_pct;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
        if let Some(b) = saved_bb {
            self.builder.position_at_end(b);
        }
        Ok(())
    }

    /// One merge step: `dst[k] = src[idx]; idx += 1; k += 1`. Shared by the
    /// merge proper and both drain loops so the four copy sites cannot drift.
    #[allow(clippy::too_many_arguments)]
    fn emit_merge_take(
        &mut self,
        elem_ty: BasicTypeEnum<'ctx>,
        src_a: PointerValue<'ctx>,
        dst_a: PointerValue<'ctx>,
        idx_a: PointerValue<'ctx>,
        k_a: PointerValue<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        i64_t: inkwell::types::IntType<'ctx>,
        one: inkwell::values::IntValue<'ctx>,
    ) {
        let src_p = self
            .builder
            .build_load(ptr_ty, src_a, "mt.src")
            .unwrap()
            .into_pointer_value();
        let dst_p = self
            .builder
            .build_load(ptr_ty, dst_a, "mt.dst")
            .unwrap()
            .into_pointer_value();
        let idx = self
            .builder
            .build_load(i64_t, idx_a, "mt.idx")
            .unwrap()
            .into_int_value();
        let k = self
            .builder
            .build_load(i64_t, k_a, "mt.k")
            .unwrap()
            .into_int_value();
        let from = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, src_p, &[idx], "mt.from")
                .unwrap()
        };
        let val = self.builder.build_load(elem_ty, from, "mt.val").unwrap();
        let to = unsafe {
            self.builder
                .build_in_bounds_gep(elem_ty, dst_p, &[k], "mt.to")
                .unwrap()
        };
        self.builder.build_store(to, val).unwrap();
        let idx_n = self.builder.build_int_add(idx, one, "mt.idx.n").unwrap();
        let k_n = self.builder.build_int_add(k, one, "mt.k.n").unwrap();
        self.builder.build_store(idx_a, idx_n).unwrap();
        self.builder.build_store(k_a, k_n).unwrap();
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

        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
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

        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
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

        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
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

        let id = self.closure_state.closure_counter;
        self.closure_state.closure_counter += 1;
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

    /// B-2026-08-14-19 — fault a `String.substring` whose start or end lands
    /// inside a multi-byte codepoint, so the compiled backends agree with the
    /// interpreter instead of handing back raw bytes.
    ///
    /// The test is `str::is_char_boundary`'s: index `len` is always a boundary
    /// (nothing follows it), and any other index is one unless the byte there is
    /// a UTF-8 CONTINUATION byte — `0b10xxxxxx`, i.e. `b & 0xC0 == 0x80`. Two
    /// loads and two compares on a method that already mallocs and memcpys, so
    /// the cost is not measurable against what it guards.
    ///
    /// `skip` short-circuits the whole check: the caller passes its
    /// out-of-range predicate, because a start outside `[0, len]` keeps the
    /// established empty-String contract and must not fault. Emitted as one
    /// guarded region rather than two so a valid slice pays a single branch.
    fn emit_substring_boundary_checks(
        &mut self,
        skip: inkwell::values::IntValue<'ctx>,
        data: inkwell::values::PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        start: inkwell::values::IntValue<'ctx>,
        end: inkwell::values::IntValue<'ctx>,
    ) {
        let fn_val = self.current_fn.unwrap();
        let i8_t = self.context.i8_type();
        let check_bb = self.context.append_basic_block(fn_val, "ss.chk");
        let done_bb = self.context.append_basic_block(fn_val, "ss.chk.done");
        self.builder
            .build_conditional_branch(skip, done_bb, check_bb)
            .unwrap();
        self.builder.position_at_end(check_bb);
        for (idx, which) in [(start, "start"), (end, "end")] {
            let interior = self
                .builder
                .build_int_compare(inkwell::IntPredicate::SLT, idx, len, "ss.chk.lt")
                .unwrap();
            let byte_bb = self.context.append_basic_block(fn_val, "ss.chk.byte");
            let next_bb = self.context.append_basic_block(fn_val, "ss.chk.next");
            self.builder
                .build_conditional_branch(interior, byte_bb, next_bb)
                .unwrap();
            self.builder.position_at_end(byte_bb);
            let slot = unsafe {
                self.builder
                    .build_gep(i8_t, data, &[idx], "ss.chk.p")
                    .unwrap()
            };
            let b = self
                .builder
                .build_load(i8_t, slot, "ss.chk.b")
                .unwrap()
                .into_int_value();
            let masked = self
                .builder
                .build_and(b, i8_t.const_int(0xC0, false), "ss.chk.mask")
                .unwrap();
            let is_cont = self
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    masked,
                    i8_t.const_int(0x80, false),
                    "ss.chk.cont",
                )
                .unwrap();
            let bad_bb = self.context.append_basic_block(fn_val, "ss.chk.bad");
            self.builder
                .build_conditional_branch(is_cont, bad_bb, next_bb)
                .unwrap();
            self.builder.position_at_end(bad_bb);
            self.emit_panic(&format!(
                "String.substring: {which} byte index is not a UTF-8 codepoint boundary — \
                 substring takes BYTE offsets (like `bytes()`), so a multi-byte character \
                 must be cut on its edge; use `char_at`/`chars()` to work in codepoints, or \
                 `find` to locate a valid cut point"
            ));
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(next_bb);
        }
        self.builder.build_unconditional_branch(done_bb).unwrap();
        self.builder.position_at_end(done_bb);
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
            | "isize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}
