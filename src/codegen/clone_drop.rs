//! Synthesized per-type clone and drop functions.
//!
//! Houses the `emit_*_clone_fn` and `emit_*_drop_fn` families plus
//! the `karac_map_insert_fn` lazy-extern accessor consumed inside
//! `emit_map_clone_fn`. Both clone and drop fns lazy-emit per-type
//! and are cached in `clone_fn_cache` / `drop_fn_cache`. Mirrors
//! the dispatch shape of `synth.rs`'s display / hash / eq emitters
//! but lives in its own submodule because the bodies are
//! collection-shape-aware (recurse through Vec/Map/Set/Tuple/String
//! element types via per-shape helpers).

use crate::ast::*;

use super::state::EnumDropKind;
use inkwell::basic_block::BasicBlock;
use inkwell::module::Linkage;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{FunctionValue, IntValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    /// TypeExpr-aware clone-fn dispatcher. Canonical entry point for any
    /// caller that needs a `void karac_clone_<typename>(*const T, *mut T)`
    /// function for type `T`. Routes by shape: primitives (load+store),
    /// String (call runtime helper), Vec[T] (deep clone with elem
    /// recursion), Map[K, V] (iterate + insert into fresh map),
    /// Set[T] (Map[T, ()]), Tuple (per-field recurse). Mirrors
    /// `emit_display_fn_for_type_expr` / `emit_hash_fn_for_type_expr`.
    /// Cached via `clone_fn_cache` on `display_mangle_te(te)`.
    ///
    /// `#[derive(Clone)]` user struct support is a follow-up — emit at the
    /// derive site by walking field types and recursing through this fn.
    pub(super) fn emit_clone_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::display_mangle_te(te);
        if let Some(&f) = self.clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name, f);
            return f;
        }
        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => self.emit_tuple_clone_fn(elems),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str);
                if head == Some("Vec") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        return self.emit_vec_clone_fn(&elem_te);
                    }
                }
                if head == Some("Map") {
                    let args = p.generic_args.as_ref();
                    let k_te = args.and_then(|a| a.first()).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    let v_te = args.and_then(|a| a.get(1)).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    if let (Some(k), Some(v)) = (k_te, v_te) {
                        return self.emit_map_clone_fn(&k, &v);
                    }
                }
                if head == Some("Set") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        // Set[T] clones as Map[T, ()] — same iterator + insert
                        // path with a zero-byte value half. The runtime's
                        // `(key_size + val_size).max(1)` keeps allocations
                        // valid (val_size = 0).
                        let unit_te = TypeExpr {
                            kind: TypeKind::Tuple(Vec::new()),
                            span: elem_te.span.clone(),
                        };
                        return self.emit_map_clone_fn(&elem_te, &unit_te);
                    }
                }
                if head == Some("String") {
                    return self.emit_string_clone_fn();
                }
                // User struct / enum: deep-clone the heap payload (the
                // `#[derive(Clone)]` analog of the synthesized drop). Without
                // this, a `let w = v[i]` move-out of a `Vec[E]`/`Vec[S]` whose
                // element carries a String/Vec shallow-copies the buffer ptr —
                // both the binding's drop and the container's element-drop free
                // it (B-2026-06-14-12, sibling of B-2026-06-14-11). Falls
                // through to the shallow primitive clone for shared (RC) types
                // and layout-block structs (handled / unsupported elsewhere).
                if let Some(name) = head {
                    if self.struct_types.contains_key(name) {
                        if let Some(f) = self.emit_struct_clone_fn(name) {
                            self.clone_fn_cache.insert(type_name, f);
                            return f;
                        }
                    }
                    if self.enum_layouts.contains_key(name) {
                        if let Some(f) = self.emit_enum_clone_fn(name) {
                            self.clone_fn_cache.insert(type_name, f);
                            return f;
                        }
                    }
                }
                // Primitive (or unsupported path) — emit the load+store body.
                self.emit_primitive_clone_fn(&type_name, te)
            }
            _ => self.emit_primitive_clone_fn(&type_name, te),
        }
    }

    /// Emit a primitive `karac_clone_<typename>(*const T, *mut T)` whose
    /// body is `*dst = *src` — single load + store. Covers every Copy-by-
    /// memcpy type (i8…i64, u8…u64, f32/f64, bool, char, unit). Cache-keyed
    /// on `type_name` so repeat callers reuse the same fn.
    pub(super) fn emit_primitive_clone_fn(
        &mut self,
        type_name: &str,
        te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name.to_string(), f);
            return f;
        }
        let val_ty = self.llvm_type_for_type_expr(te);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let clone_fn = self
            .module
            .add_function(&fn_name, clone_fn_ty, Some(Linkage::Internal));
        self.clone_fn_cache.insert(type_name.to_string(), clone_fn);

        let entry_bb = self.context.append_basic_block(clone_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let src = clone_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst = clone_fn.get_nth_param(1).unwrap().into_pointer_value();
        let v = self.builder.build_load(val_ty, src, "v").unwrap();
        self.builder.build_store(dst, v).unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        clone_fn
    }

    /// Emit (or fetch) the cloned-String fn — a thin wrapper that just
    /// tail-calls the `karac_string_clone` runtime helper. The wrapper
    /// keeps the per-type clone-fn signature uniform with other types so
    /// callers don't special-case Strings.
    pub(super) fn emit_string_clone_fn(&mut self) -> FunctionValue<'ctx> {
        let type_name = "String".to_string();
        if let Some(&f) = self.clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = "karac_clone_String".to_string();
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let clone_fn = self
            .module
            .add_function(&fn_name, clone_fn_ty, Some(Linkage::Internal));
        self.clone_fn_cache.insert(type_name, clone_fn);

        let entry_bb = self.context.append_basic_block(clone_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let src = clone_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst = clone_fn.get_nth_param(1).unwrap().into_pointer_value();
        self.builder
            .build_call(self.karac_string_clone_fn, &[src.into(), dst.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        clone_fn
    }

    /// Emit `karac_clone_Vec_<elem>` — read the source `{data, len, cap}`,
    /// allocate a fresh buffer of the same capacity, walk `0..len` calling
    /// the per-element clone fn through the new dispatcher, write the new
    /// `{data, len, cap}` to dst.
    ///
    /// Empty-source fast path (subtask 9): `len == 0` skips the malloc;
    /// dst gets `{null, 0, 0}` with `cap == 0` matching the static-literal
    /// convention so scope-exit cleanup is a no-op.
    pub(super) fn emit_vec_clone_fn(&mut self, elem_te: &TypeExpr) -> FunctionValue<'ctx> {
        let elem_name = Self::display_mangle_te(elem_te);
        let type_name = format!("Vec_{elem_name}");
        if let Some(&f) = self.clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);
        // Recurse first — emit may switch the builder's insert block.
        let elem_clone = self.emit_clone_fn_for_type_expr(elem_te);

        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let clone_fn = self
            .module
            .add_function(&fn_name, clone_fn_ty, Some(Linkage::Internal));
        self.clone_fn_cache.insert(type_name, clone_fn);

        let entry_bb = self.context.append_basic_block(clone_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let src = clone_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst = clone_fn.get_nth_param(1).unwrap().into_pointer_value();

        // Load src.{data, len, cap}
        let src_data_pp = self
            .builder
            .build_struct_gep(vec_ty, src, 0, "src.data.pp")
            .unwrap();
        let src_len_p = self
            .builder
            .build_struct_gep(vec_ty, src, 1, "src.len.p")
            .unwrap();
        let src_cap_p = self
            .builder
            .build_struct_gep(vec_ty, src, 2, "src.cap.p")
            .unwrap();
        let src_data = self
            .builder
            .build_load(ptr_ty, src_data_pp, "src.data")
            .unwrap()
            .into_pointer_value();
        let src_len = self
            .builder
            .build_load(i64_t, src_len_p, "src.len")
            .unwrap()
            .into_int_value();
        let src_cap = self
            .builder
            .build_load(i64_t, src_cap_p, "src.cap")
            .unwrap()
            .into_int_value();

        // dst.{data, len, cap} GEPs
        let dst_data_pp = self
            .builder
            .build_struct_gep(vec_ty, dst, 0, "dst.data.pp")
            .unwrap();
        let dst_len_p = self
            .builder
            .build_struct_gep(vec_ty, dst, 1, "dst.len.p")
            .unwrap();
        let dst_cap_p = self
            .builder
            .build_struct_gep(vec_ty, dst, 2, "dst.cap.p")
            .unwrap();

        // Empty fast path: len == 0 → {null, 0, 0}.
        let empty_bb = self.context.append_basic_block(clone_fn, "empty");
        let alloc_bb = self.context.append_basic_block(clone_fn, "alloc");
        let is_empty = self
            .builder
            .build_int_compare(IntPredicate::EQ, src_len, i64_t.const_zero(), "is.empty")
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, empty_bb, alloc_bb)
            .unwrap();

        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(dst_data_pp, ptr_ty.const_null())
            .unwrap();
        self.builder
            .build_store(dst_len_p, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(dst_cap_p, i64_t.const_zero())
            .unwrap();
        self.builder.build_return(None).unwrap();

        // alloc + memcpy-loop path.
        self.builder.position_at_end(alloc_bb);
        let raw_size = elem_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let elem_size = if raw_size.get_type().get_bit_width() == 64 {
            raw_size
        } else {
            self.builder
                .build_int_z_extend(raw_size, i64_t, "esz64")
                .unwrap()
        };
        // Buffer cap matches src.cap when > 0; otherwise (static-literal
        // source with cap=0 but non-zero len) allocate len-byte buffer.
        let cap_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, src_cap, i64_t.const_zero(), "cap.zero")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(cap_zero, src_len, src_cap, "new.cap")
            .unwrap()
            .into_int_value();
        let alloc_bytes = self
            .builder
            .build_int_mul(new_cap, elem_size, "alloc.bytes")
            .unwrap();
        let new_data = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "new.data")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Loop: i in 0..len; call elem_clone(src.data + i*size, new_data + i*size).
        let pre_bb = self.builder.get_insert_block().unwrap();
        let hdr_bb = self.context.append_basic_block(clone_fn, "loop.hdr");
        let bdy_bb = self.context.append_basic_block(clone_fn, "loop.bdy");
        let exit_bb = self.context.append_basic_block(clone_fn, "loop.exit");
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let i_phi = self.builder.build_phi(i64_t, "i").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), pre_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let cond = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_val, src_len, "cond")
            .unwrap();
        self.builder
            .build_conditional_branch(cond, bdy_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(bdy_bb);
        let offset = self.builder.build_int_mul(i_val, elem_size, "off").unwrap();
        let src_elem = unsafe {
            self.builder
                .build_gep(i8_t, src_data, &[offset], "src.elem")
                .unwrap()
        };
        let dst_elem = unsafe {
            self.builder
                .build_gep(i8_t, new_data, &[offset], "dst.elem")
                .unwrap()
        };
        self.builder
            .build_call(elem_clone, &[src_elem.into(), dst_elem.into()], "")
            .unwrap();
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next")
            .unwrap();
        i_phi.add_incoming(&[(&i_next, bdy_bb)]);
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_store(dst_data_pp, new_data).unwrap();
        self.builder.build_store(dst_len_p, src_len).unwrap();
        self.builder.build_store(dst_cap_p, new_cap).unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        clone_fn
    }

    /// Emit a Map[K, V] clone fn. Iterates the source via `karac_map_iter_*`,
    /// per-entry: clone K and V into stack allocas, then `karac_map_insert`
    /// into the fresh destination map. Hash/eq fn pointers come from the
    /// existing TypeExpr-aware emit fns, so compound keys (`Map[(i64, String), V]`)
    /// compose correctly.
    ///
    /// Set[T] reuses this path with V = unit (empty-tuple). The runtime's
    /// `(key_size + val_size).max(1)` keeps the bucket allocation valid
    /// when val_size = 0.
    pub(super) fn emit_map_clone_fn(
        &mut self,
        key_te: &TypeExpr,
        val_te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let key_name = Self::display_mangle_te(key_te);
        let val_name = Self::display_mangle_te(val_te);
        let type_name = format!("Map_{key_name}_{val_name}");
        if let Some(&f) = self.clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let key_ty = self.llvm_type_for_type_expr(key_te);
        let val_ty = self.llvm_type_for_type_expr(val_te);
        let hash_fn = self.emit_hash_fn_for_type_expr(key_te);
        let eq_fn = self.emit_eq_fn_for_type_expr(key_te);
        let key_clone = self.emit_clone_fn_for_type_expr(key_te);
        let val_clone = self.emit_clone_fn_for_type_expr(val_te);

        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let clone_fn = self
            .module
            .add_function(&fn_name, clone_fn_ty, Some(Linkage::Internal));
        self.clone_fn_cache.insert(type_name, clone_fn);

        let entry_bb = self.context.append_basic_block(clone_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let src = clone_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst = clone_fn.get_nth_param(1).unwrap().into_pointer_value();

        // Load source map handle.
        let src_handle = self
            .builder
            .build_load(ptr_ty, src, "src.handle")
            .unwrap()
            .into_pointer_value();

        // Allocate a fresh map. Sizes = sizeof(K), sizeof(V); val_size = 0
        // for Set's unit-tuple case is fine since llvm_type_for_type_expr
        // on empty-tuple returns i64 → size 8. For a true zero-size value,
        // we'd need extra plumbing; the runtime's `.max(1)` already keeps
        // the allocation valid so 8-byte slots are harmless overhead.
        let key_size = key_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let val_size = val_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let new_handle = self
            .builder
            .build_call(
                self.karac_map_new_fn,
                &[
                    key_size.into(),
                    val_size.into(),
                    hash_fn.as_global_value().as_pointer_value().into(),
                    eq_fn.as_global_value().as_pointer_value().into(),
                ],
                "new.map",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Stack allocas for the iterator's key/val out-slots and for the
        // cloned key/val we pass to `karac_map_insert`.
        let key_out = self.create_entry_alloca(clone_fn, "k.out", key_ty);
        let val_out = self.create_entry_alloca(clone_fn, "v.out", val_ty);
        let key_clone_slot = self.create_entry_alloca(clone_fn, "k.clone", key_ty);
        let val_clone_slot = self.create_entry_alloca(clone_fn, "v.clone", val_ty);

        // Iterator handle.
        let iter_handle = self
            .builder
            .build_call(self.karac_map_iter_new_fn, &[src_handle.into()], "iter")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        let hdr_bb = self.context.append_basic_block(clone_fn, "iter.hdr");
        let bdy_bb = self.context.append_basic_block(clone_fn, "iter.bdy");
        let exit_bb = self.context.append_basic_block(clone_fn, "iter.exit");
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(hdr_bb);
        let has = self
            .builder
            .build_call(
                self.karac_map_iter_next_fn,
                &[iter_handle.into(), key_out.into(), val_out.into()],
                "iter.has",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(has, bdy_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(bdy_bb);
        // Clone key and value into fresh allocas, then insert.
        self.builder
            .build_call(key_clone, &[key_out.into(), key_clone_slot.into()], "")
            .unwrap();
        self.builder
            .build_call(val_clone, &[val_out.into(), val_clone_slot.into()], "")
            .unwrap();
        self.builder
            .build_call(
                self.karac_map_insert_fn(),
                &[
                    new_handle.into(),
                    key_clone_slot.into(),
                    val_clone_slot.into(),
                ],
                "",
            )
            .unwrap();
        self.builder.build_unconditional_branch(hdr_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder
            .build_call(self.karac_map_iter_free_fn, &[iter_handle.into()], "")
            .unwrap();
        self.builder.build_store(dst, new_handle).unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        clone_fn
    }

    /// Helper: get-or-declare the `karac_map_insert(map, key, val) -> void`
    /// runtime fn. We don't use `karac_map_insert_old` here because the
    /// fresh destination map is empty by construction — there's no old
    /// value to capture.
    pub(super) fn karac_map_insert_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("karac_map_insert") {
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        self.module
            .add_function("karac_map_insert", ty, Some(Linkage::External))
    }

    /// Emit a per-field-recursive clone fn for an n-tuple. Mirrors the
    /// tuple Hash/Eq/Display pattern — recursive per-field calls into the
    /// per-field clone fn via struct GEP.
    pub(super) fn emit_tuple_clone_fn(&mut self, elems: &[TypeExpr]) -> FunctionValue<'ctx> {
        let elems_owned: Vec<TypeExpr> = elems.to_vec();
        let parts: Vec<String> = elems_owned.iter().map(Self::display_mangle_te).collect();
        let type_name = format!("tuple_{}", parts.join("_"));
        if let Some(&f) = self.clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name, f);
            return f;
        }

        let child_fns: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_clone_fn_for_type_expr(e))
            .collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let clone_fn = self
            .module
            .add_function(&fn_name, clone_fn_ty, Some(Linkage::Internal));
        self.clone_fn_cache.insert(type_name, clone_fn);

        let entry_bb = self.context.append_basic_block(clone_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let src = clone_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst = clone_fn.get_nth_param(1).unwrap().into_pointer_value();
        for (i, child_fn) in child_fns.iter().enumerate() {
            let src_field = self
                .builder
                .build_struct_gep(tuple_ty, src, i as u32, &format!("t.f{i}.s"))
                .unwrap();
            let dst_field = self
                .builder
                .build_struct_gep(tuple_ty, dst, i as u32, &format!("t.f{i}.d"))
                .unwrap();
            self.builder
                .build_call(*child_fn, &[src_field.into(), dst_field.into()], "")
                .unwrap();
        }
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        clone_fn
    }

    /// `#[derive(Clone)]` for a non-shared user struct: GEP each field of the
    /// registered struct LLVM type and recurse through
    /// `emit_clone_fn_for_type_expr`, so a `String`/`Vec`/`Map`/nested-struct
    /// field gets an independent heap buffer instead of a shared pointer
    /// (mirrors `emit_tuple_clone_fn`, keyed on `struct_field_type_exprs`).
    ///
    /// Returns `None` — caller falls back to the shallow primitive clone — for:
    /// a shared struct (RC machinery owns its lifecycle), an unknown struct, or
    /// a layout-block / SoA struct whose physical LLVM field count diverges from
    /// its logical field list (can't be GEP'd field-for-field here).
    pub(super) fn emit_struct_clone_fn(
        &mut self,
        struct_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        if self.shared_types.contains_key(struct_name) {
            return None;
        }
        let type_name = format!("struct_{struct_name}");
        if let Some(&f) = self.clone_fn_cache.get(&type_name) {
            return Some(f);
        }
        let fn_name = format!("karac_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name, f);
            return Some(f);
        }
        let struct_ty = *self.struct_types.get(struct_name)?;
        let field_tes = self.struct_field_type_exprs.get(struct_name)?.clone();
        // Plain field-ordered struct only: a layout-block / SoA struct whose
        // physical field count differs from its logical field list can't be
        // walked field-for-field against the source TypeExprs.
        if struct_ty.count_fields() as usize != field_tes.len() {
            return None;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let clone_fn = self
            .module
            .add_function(&fn_name, clone_fn_ty, Some(Linkage::Internal));
        // Cache the declaration BEFORE emitting child clone fns so a
        // self-recursive struct (`S { next: Vec[S] }`) terminates.
        self.clone_fn_cache.insert(type_name, clone_fn);

        // Emit per-field child clone fns up front (each repositions the
        // builder, so finish them before filling this fn's entry block).
        let child_fns: Vec<FunctionValue<'ctx>> = field_tes
            .iter()
            .map(|te| self.emit_clone_fn_for_type_expr(te))
            .collect();

        let entry_bb = self.context.append_basic_block(clone_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let src = clone_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst = clone_fn.get_nth_param(1).unwrap().into_pointer_value();
        for (i, child_fn) in child_fns.iter().enumerate() {
            let src_field = self
                .builder
                .build_struct_gep(struct_ty, src, i as u32, &format!("s.f{i}.s"))
                .unwrap();
            let dst_field = self
                .builder
                .build_struct_gep(struct_ty, dst, i as u32, &format!("s.f{i}.d"))
                .unwrap();
            self.builder
                .build_call(*child_fn, &[src_field.into(), dst_field.into()], "")
                .unwrap();
        }
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(clone_fn)
    }

    /// `#[derive(Clone)]` for a non-shared user enum: shallow-copy the whole
    /// value (tag + every payload word), then switch on the tag and overwrite
    /// each heap-bearing field of the active variant with an independent deep
    /// clone — the clone-side mirror of `emit_enum_drop_switch`. The field's
    /// `{data,len,cap}` words live at LLVM index `start_word + 1` (tag is field
    /// 0), byte-compatible with the vec-struct the `String`/`Vec`/nested-struct
    /// clone fn expects, so `emit_clone_fn_for_type_expr(field_te)` handles
    /// every payload shape uniformly.
    ///
    /// Returns `None` (caller falls back to the shallow primitive clone) for a
    /// shared enum (RC) or an enum whose every variant is heap-free (the shallow
    /// copy is already a complete clone).
    pub(super) fn emit_enum_clone_fn(&mut self, enum_name: &str) -> Option<FunctionValue<'ctx>> {
        let layout = self.enum_layouts.get(enum_name)?.clone();
        if layout.is_shared {
            return None; // shared enums use the RC machinery
        }
        // Every variant heap-free → the shallow whole-value copy is complete.
        let any_heap = layout
            .field_drop_kinds
            .values()
            .any(|kinds| kinds.iter().any(|k| *k != EnumDropKind::None));
        if !any_heap {
            return None;
        }
        let type_name = format!("enum_{enum_name}");
        if let Some(&f) = self.clone_fn_cache.get(&type_name) {
            return Some(f);
        }
        let fn_name = format!("karac_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.clone_fn_cache.insert(type_name, f);
            return Some(f);
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let saved_bb = self.builder.get_insert_block();
        let clone_fn_ty = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let clone_fn = self
            .module
            .add_function(&fn_name, clone_fn_ty, Some(Linkage::Internal));
        // Cache before recursing so a self-recursive enum terminates.
        self.clone_fn_cache.insert(type_name, clone_fn);

        // Per-variant field TypeExprs — drive the per-field deep clone.
        let variant_field_tes: Vec<(String, Vec<TypeExpr>)> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .map(|(_tag, name, tes)| (name, tes))
            .collect();

        // Sort variants by discriminant for deterministic IR.
        let mut tag_entries: Vec<(String, u64)> =
            layout.tags.iter().map(|(n, t)| (n.clone(), *t)).collect();
        tag_entries.sort_by_key(|(_, t)| *t);

        // Pre-emit every child clone fn (each repositions the builder), keyed
        // by (variant, llvm field index). Done before any BB of this fn is
        // filled so the builder isn't yanked mid-block.
        let mut field_clones: Vec<(String, u32, FunctionValue<'ctx>)> = Vec::new();
        for (variant_name, _tag) in &tag_entries {
            let (Some(kinds), Some(offsets)) = (
                layout.field_drop_kinds.get(variant_name),
                layout.field_word_offsets.get(variant_name),
            ) else {
                continue;
            };
            let field_tes = variant_field_tes
                .iter()
                .find(|(n, _)| n == variant_name)
                .map(|(_, tes)| tes.clone())
                .unwrap_or_default();
            for (fi, (kind, (start_word, _num_words))) in
                kinds.iter().zip(offsets.iter()).enumerate()
            {
                if *kind == EnumDropKind::None {
                    continue;
                }
                let Some(field_te) = field_tes.get(fi).cloned() else {
                    continue;
                };
                let child_fn = self.emit_clone_fn_for_type_expr(&field_te);
                // Field index in `llvm_type`: data/struct word at `start_word
                // + 1` (tag is field 0), matching `emit_enum_drop_switch`.
                field_clones.push((variant_name.clone(), (*start_word + 1) as u32, child_fn));
            }
        }

        let entry_bb = self.context.append_basic_block(clone_fn, "entry");
        let exit_bb = self.context.append_basic_block(clone_fn, "exit");
        self.builder.position_at_end(entry_bb);
        let src = clone_fn.get_nth_param(0).unwrap().into_pointer_value();
        let dst = clone_fn.get_nth_param(1).unwrap().into_pointer_value();

        // 1. Shallow bitcopy the whole enum (tag + all payload words, sharing
        //    heap pointers). Heap fields of the live variant are overwritten
        //    with deep clones below; primitive fields keep this copy.
        let whole = self
            .builder
            .build_load(layout.llvm_type, src, "enum.whole")
            .unwrap();
        self.builder.build_store(dst, whole).unwrap();

        // 2. Switch on the tag (field 0).
        let tag_ptr = self
            .builder
            .build_struct_gep(layout.llvm_type, src, 0, "clone.tag.p")
            .unwrap();
        let tag_val = self
            .builder
            .build_load(i64_t, tag_ptr, "clone.tag")
            .unwrap()
            .into_int_value();

        let mut switch_cases: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();
        let case_bbs: Vec<(String, BasicBlock<'ctx>)> = tag_entries
            .iter()
            .map(|(name, tag)| {
                let bb = self
                    .context
                    .append_basic_block(clone_fn, &format!("clone.{name}"));
                switch_cases.push((i64_t.const_int(*tag, false), bb));
                (name.clone(), bb)
            })
            .collect();
        self.builder
            .build_switch(tag_val, exit_bb, &switch_cases)
            .unwrap();

        // 3. Per-variant: deep-clone each heap field, overwriting the
        //    shallow-copied shared pointer in `dst` with its own buffer.
        for (variant_name, bb) in &case_bbs {
            self.builder.position_at_end(*bb);
            for (vn, field_idx, child_fn) in &field_clones {
                if vn != variant_name {
                    continue;
                }
                let src_field = self
                    .builder
                    .build_struct_gep(layout.llvm_type, src, *field_idx, "clone.fld.s")
                    .unwrap();
                let dst_field = self
                    .builder
                    .build_struct_gep(layout.llvm_type, dst, *field_idx, "clone.fld.d")
                    .unwrap();
                self.builder
                    .build_call(*child_fn, &[src_field.into(), dst_field.into()], "")
                    .unwrap();
            }
            self.builder.build_unconditional_branch(exit_bb).unwrap();
        }

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(clone_fn)
    }

    // ── Fallible (try_clone) fn framework (mirror of clone, but i1 + cleanup) ──

    /// True when `te`'s type tree contains no `Map`/`Set`/`SortedSet`/`SortedMap` node —
    /// i.e. every allocation in its deep clone is a plain buffer that
    /// `karac_alloc_fallible` covers. `try_clone` codegen is emitted only for
    /// supported trees; Map/Set-bearing types need a fallible `karac_map_*`
    /// runtime API (phase-8-stdlib-floor item 8, the `try_insert` blocker) and
    /// are rejected at the dispatch guard with the interpreter-only message
    /// before any IR is emitted.
    pub(super) fn type_expr_try_clone_supported(te: &TypeExpr) -> bool {
        match &te.kind {
            TypeKind::Tuple(elems) => elems.iter().all(Self::type_expr_try_clone_supported),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str);
                match head {
                    Some("Map") | Some("Set") | Some("SortedSet") | Some("SortedMap") => false,
                    Some("Vec") | Some("VecDeque") => p
                        .generic_args
                        .as_ref()
                        .and_then(|a| a.first())
                        .map(|a| match a {
                            GenericArg::Type(t) => Self::type_expr_try_clone_supported(t),
                            _ => true,
                        })
                        .unwrap_or(true),
                    _ => true,
                }
            }
            _ => true,
        }
    }

    /// Declare an `i1 karac_try_clone_<typename>(*const T, *mut T, *mut i64)`
    /// shell with internal linkage. The third out-parameter receives the
    /// failed allocation's byte count when the fn returns `false`.
    fn declare_try_clone_fn(&self, fn_name: &str) -> FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        self.module
            .add_function(fn_name, fn_ty, Some(Linkage::Internal))
    }

    /// Fallible analog of `emit_clone_fn_for_type_expr`. Emits (or fetches)
    /// `i1 karac_try_clone_<typename>(*const T, *mut T, *mut i64)` — clones
    /// `src` into `dst` via `karac_alloc_fallible`, returning `true` on
    /// success or `false` (after freeing any partial heap + storing the
    /// failed byte count through the out-param) on the first OOM. Routes by
    /// shape: primitive (memcpy, always succeeds), String (fallible buffer
    /// alloc), Vec[T] (fallible buffer + per-element recursion with
    /// partial-clone cleanup on failure), Tuple (per-field recursion with
    /// cleanup). Map/Set are unreachable — `type_expr_try_clone_supported`
    /// gates them out at the dispatch site before emission.
    pub(super) fn emit_try_clone_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::display_mangle_te(te);
        if let Some(&f) = self.try_clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_try_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.try_clone_fn_cache.insert(type_name, f);
            return f;
        }
        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => self.emit_tuple_try_clone_fn(elems),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str);
                if head == Some("Vec") || head == Some("VecDeque") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        return self.emit_vec_try_clone_fn(&elem_te);
                    }
                }
                if head == Some("String") {
                    return self.emit_string_try_clone_fn();
                }
                self.emit_primitive_try_clone_fn(&type_name, te)
            }
            _ => self.emit_primitive_try_clone_fn(&type_name, te),
        }
    }

    /// Fallible clone of a primitive (Copy-by-memcpy) type: `*dst = *src`,
    /// always returns `true` (no allocation, never fails).
    pub(super) fn emit_primitive_try_clone_fn(
        &mut self,
        type_name: &str,
        te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_try_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.try_clone_fn_cache.insert(type_name.to_string(), f);
            return f;
        }
        let val_ty = self.llvm_type_for_type_expr(te);
        let bool_t = self.context.bool_type();
        let saved_bb = self.builder.get_insert_block();
        let f = self.declare_try_clone_fn(&fn_name);
        self.try_clone_fn_cache.insert(type_name.to_string(), f);

        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let src = f.get_nth_param(0).unwrap().into_pointer_value();
        let dst = f.get_nth_param(1).unwrap().into_pointer_value();
        let v = self.builder.build_load(val_ty, src, "v").unwrap();
        self.builder.build_store(dst, v).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Fallible String clone — empty-source fast path (`{null, 0, 0}`),
    /// else `karac_alloc_fallible(len + 1)`; null → store `len+1` to the
    /// out-param and return `false`; success → memcpy `len`, NUL-terminate,
    /// publish `{buf, len, len}`, return `true`. Inlines the buffer alloc
    /// (rather than calling the panicking `karac_string_clone` runtime helper)
    /// so the only allocation is fallible.
    pub(super) fn emit_string_try_clone_fn(&mut self) -> FunctionValue<'ctx> {
        let type_name = "String".to_string();
        if let Some(&f) = self.try_clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = "karac_try_clone_String".to_string();
        if let Some(f) = self.module.get_function(&fn_name) {
            self.try_clone_fn_cache.insert(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let vec_ty = self.vec_struct_type();
        let saved_bb = self.builder.get_insert_block();
        let f = self.declare_try_clone_fn(&fn_name);
        self.try_clone_fn_cache.insert(type_name, f);

        let entry = self.context.append_basic_block(f, "entry");
        let empty_bb = self.context.append_basic_block(f, "empty");
        let alloc_bb = self.context.append_basic_block(f, "alloc");
        let oom_bb = self.context.append_basic_block(f, "oom");
        let copy_bb = self.context.append_basic_block(f, "copy");

        self.builder.position_at_end(entry);
        let src = f.get_nth_param(0).unwrap().into_pointer_value();
        let dst = f.get_nth_param(1).unwrap().into_pointer_value();
        let failed = f.get_nth_param(2).unwrap().into_pointer_value();

        let src_data_pp = self
            .builder
            .build_struct_gep(vec_ty, src, 0, "s.data.pp")
            .unwrap();
        let src_len_p = self
            .builder
            .build_struct_gep(vec_ty, src, 1, "s.len.p")
            .unwrap();
        let src_data = self
            .builder
            .build_load(ptr_ty, src_data_pp, "s.data")
            .unwrap()
            .into_pointer_value();
        let src_len = self
            .builder
            .build_load(i64_t, src_len_p, "s.len")
            .unwrap()
            .into_int_value();
        let dst_data_pp = self
            .builder
            .build_struct_gep(vec_ty, dst, 0, "d.data.pp")
            .unwrap();
        let dst_len_p = self
            .builder
            .build_struct_gep(vec_ty, dst, 1, "d.len.p")
            .unwrap();
        let dst_cap_p = self
            .builder
            .build_struct_gep(vec_ty, dst, 2, "d.cap.p")
            .unwrap();
        let is_empty = self
            .builder
            .build_int_compare(IntPredicate::EQ, src_len, i64_t.const_zero(), "s.empty")
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, empty_bb, alloc_bb)
            .unwrap();

        // Empty → {null, 0, 0}, true.
        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(dst_data_pp, ptr_ty.const_null())
            .unwrap();
        self.builder
            .build_store(dst_len_p, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(dst_cap_p, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // Alloc len+1 fallibly.
        self.builder.position_at_end(alloc_bb);
        let alloc_bytes = self
            .builder
            .build_int_add(src_len, i64_t.const_int(1, false), "s.alloc.bytes")
            .unwrap();
        let new_data = self
            .builder
            .build_call(self.alloc_fallible_fn, &[alloc_bytes.into()], "s.new")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let is_null = self.builder.build_is_null(new_data, "s.null").unwrap();
        self.builder
            .build_conditional_branch(is_null, oom_bb, copy_bb)
            .unwrap();

        // OOM → store failed bytes, return false.
        self.builder.position_at_end(oom_bb);
        self.builder.build_store(failed, alloc_bytes).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        // Copy: memcpy len bytes, NUL-terminate, publish {new, len, len}, true.
        self.builder.position_at_end(copy_bb);
        self.builder
            .build_memcpy(new_data, 1, src_data, 1, src_len)
            .unwrap();
        let nul_ptr = unsafe {
            self.builder
                .build_gep(i8_t, new_data, &[src_len], "s.nul")
                .unwrap()
        };
        self.builder
            .build_store(nul_ptr, i8_t.const_zero())
            .unwrap();
        self.builder.build_store(dst_data_pp, new_data).unwrap();
        self.builder.build_store(dst_len_p, src_len).unwrap();
        self.builder.build_store(dst_cap_p, src_len).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Fallible `Vec[T]` clone. Empty-source fast path; else fallible buffer
    /// alloc (null → store bytes, return false), then a `0..len` loop calling
    /// the per-element fallible clone. If an element clone fails, drop the
    /// `[0..i)` already-cloned elements (via the per-element drop fn), free
    /// the buffer, and return false — the failed element's byte count is
    /// already recorded by the recursive callee, so nothing leaks and the
    /// `AllocError` carries the deepest failing allocation. Success publishes
    /// `{buf, len, new_cap}` and returns true.
    pub(super) fn emit_vec_try_clone_fn(&mut self, elem_te: &TypeExpr) -> FunctionValue<'ctx> {
        let elem_name = Self::display_mangle_te(elem_te);
        let type_name = format!("Vec_{elem_name}");
        if let Some(&f) = self.try_clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_try_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.try_clone_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);
        // Recurse first — emit may switch the builder's insert block.
        let elem_try_clone = self.emit_try_clone_fn_for_type_expr(elem_te);
        let elem_drop = self.emit_drop_fn_for_type_expr(elem_te);

        let saved_bb = self.builder.get_insert_block();
        let f = self.declare_try_clone_fn(&fn_name);
        self.try_clone_fn_cache.insert(type_name, f);

        let entry = self.context.append_basic_block(f, "entry");
        let empty_bb = self.context.append_basic_block(f, "empty");
        let alloc_bb = self.context.append_basic_block(f, "alloc");
        let oom_bb = self.context.append_basic_block(f, "oom");
        let loop_hdr = self.context.append_basic_block(f, "loop.hdr");
        let loop_bdy = self.context.append_basic_block(f, "loop.bdy");
        let loop_nxt = self.context.append_basic_block(f, "loop.nxt");
        let publish_bb = self.context.append_basic_block(f, "publish");
        let cln_hdr = self.context.append_basic_block(f, "cleanup.hdr");
        let cln_bdy = self.context.append_basic_block(f, "cleanup.bdy");
        let cln_done = self.context.append_basic_block(f, "cleanup.done");

        // Loop counters (entry-block allocas).
        let i_alloca = self.create_entry_alloca(f, "i", i64_t.into());
        let j_alloca = self.create_entry_alloca(f, "j", i64_t.into());

        self.builder.position_at_end(entry);
        let src = f.get_nth_param(0).unwrap().into_pointer_value();
        let dst = f.get_nth_param(1).unwrap().into_pointer_value();
        let failed = f.get_nth_param(2).unwrap().into_pointer_value();

        let src_data_pp = self
            .builder
            .build_struct_gep(vec_ty, src, 0, "src.data.pp")
            .unwrap();
        let src_len_p = self
            .builder
            .build_struct_gep(vec_ty, src, 1, "src.len.p")
            .unwrap();
        let src_cap_p = self
            .builder
            .build_struct_gep(vec_ty, src, 2, "src.cap.p")
            .unwrap();
        let src_data = self
            .builder
            .build_load(ptr_ty, src_data_pp, "src.data")
            .unwrap()
            .into_pointer_value();
        let src_len = self
            .builder
            .build_load(i64_t, src_len_p, "src.len")
            .unwrap()
            .into_int_value();
        let src_cap = self
            .builder
            .build_load(i64_t, src_cap_p, "src.cap")
            .unwrap()
            .into_int_value();

        let dst_data_pp = self
            .builder
            .build_struct_gep(vec_ty, dst, 0, "dst.data.pp")
            .unwrap();
        let dst_len_p = self
            .builder
            .build_struct_gep(vec_ty, dst, 1, "dst.len.p")
            .unwrap();
        let dst_cap_p = self
            .builder
            .build_struct_gep(vec_ty, dst, 2, "dst.cap.p")
            .unwrap();

        let is_empty = self
            .builder
            .build_int_compare(IntPredicate::EQ, src_len, i64_t.const_zero(), "is.empty")
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, empty_bb, alloc_bb)
            .unwrap();

        // Empty → {null, 0, 0}, true.
        self.builder.position_at_end(empty_bb);
        self.builder
            .build_store(dst_data_pp, ptr_ty.const_null())
            .unwrap();
        self.builder
            .build_store(dst_len_p, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_store(dst_cap_p, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // Fallible buffer alloc.
        self.builder.position_at_end(alloc_bb);
        let raw_size = elem_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let elem_size = if raw_size.get_type().get_bit_width() == 64 {
            raw_size
        } else {
            self.builder
                .build_int_z_extend(raw_size, i64_t, "esz64")
                .unwrap()
        };
        let cap_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, src_cap, i64_t.const_zero(), "cap.zero")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(cap_zero, src_len, src_cap, "new.cap")
            .unwrap()
            .into_int_value();
        let alloc_bytes = self
            .builder
            .build_int_mul(new_cap, elem_size, "alloc.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.alloc_fallible_fn, &[alloc_bytes.into()], "buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let buf_null = self.builder.build_is_null(buf, "buf.null").unwrap();
        self.builder
            .build_store(i_alloca, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_conditional_branch(buf_null, oom_bb, loop_hdr)
            .unwrap();

        // OOM (top-level buffer) → store bytes, return false.
        self.builder.position_at_end(oom_bb);
        self.builder.build_store(failed, alloc_bytes).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        // Per-element clone loop.
        self.builder.position_at_end(loop_hdr);
        let i_val = self
            .builder
            .build_load(i64_t, i_alloca, "i.val")
            .unwrap()
            .into_int_value();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, i_val, src_len, "in.range")
            .unwrap();
        self.builder
            .build_conditional_branch(in_range, loop_bdy, publish_bb)
            .unwrap();

        self.builder.position_at_end(loop_bdy);
        let off = self.builder.build_int_mul(i_val, elem_size, "off").unwrap();
        let src_ep = unsafe {
            self.builder
                .build_gep(i8_t, src_data, &[off], "src.ep")
                .unwrap()
        };
        let dst_ep = unsafe { self.builder.build_gep(i8_t, buf, &[off], "dst.ep").unwrap() };
        let elem_ok = self
            .builder
            .build_call(
                elem_try_clone,
                &[src_ep.into(), dst_ep.into(), failed.into()],
                "elem.ok",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(elem_ok, loop_nxt, cln_hdr)
            .unwrap();

        self.builder.position_at_end(loop_nxt);
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next")
            .unwrap();
        self.builder.build_store(i_alloca, i_next).unwrap();
        self.builder.build_unconditional_branch(loop_hdr).unwrap();

        // All elements cloned → publish.
        self.builder.position_at_end(publish_bb);
        self.builder.build_store(dst_data_pp, buf).unwrap();
        self.builder.build_store(dst_len_p, src_len).unwrap();
        self.builder.build_store(dst_cap_p, new_cap).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // Element clone failed at index `i_val` — drop [0..i_val), free buf,
        // return false (the failed byte count is already set by the callee).
        self.builder.position_at_end(cln_hdr);
        self.builder
            .build_store(j_alloca, i64_t.const_zero())
            .unwrap();
        self.builder.build_unconditional_branch(cln_bdy).unwrap();
        // cln_bdy guards the `j < i_val` loop at its top, dropping buf[j].
        self.builder.position_at_end(cln_bdy);
        let j_val = self
            .builder
            .build_load(i64_t, j_alloca, "j.val")
            .unwrap()
            .into_int_value();
        let j_in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, j_val, i_val, "j.in.range")
            .unwrap();
        let cln_drop = self.context.append_basic_block(f, "cleanup.drop");
        self.builder
            .build_conditional_branch(j_in_range, cln_drop, cln_done)
            .unwrap();
        self.builder.position_at_end(cln_drop);
        let joff = self
            .builder
            .build_int_mul(j_val, elem_size, "joff")
            .unwrap();
        let j_ep = unsafe { self.builder.build_gep(i8_t, buf, &[joff], "j.ep").unwrap() };
        self.builder
            .build_call(elem_drop, &[j_ep.into()], "")
            .unwrap();
        let j_next = self
            .builder
            .build_int_add(j_val, i64_t.const_int(1, false), "j.next")
            .unwrap();
        self.builder.build_store(j_alloca, j_next).unwrap();
        self.builder.build_unconditional_branch(cln_bdy).unwrap();

        self.builder.position_at_end(cln_done);
        self.builder
            .build_call(self.free_fn, &[buf.into()], "")
            .unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(0, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Fallible n-tuple clone — per-field recursion. On field `i`'s failure,
    /// drop the already-cloned fields `[0..i)` (statically unrolled, since the
    /// field count is known) and return false; the failing byte count is set
    /// by the recursive callee. All fields cloned → return true.
    pub(super) fn emit_tuple_try_clone_fn(&mut self, elems: &[TypeExpr]) -> FunctionValue<'ctx> {
        let elems_owned: Vec<TypeExpr> = elems.to_vec();
        let parts: Vec<String> = elems_owned.iter().map(Self::display_mangle_te).collect();
        let type_name = format!("tuple_{}", parts.join("_"));
        if let Some(&f) = self.try_clone_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_try_clone_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.try_clone_fn_cache.insert(type_name, f);
            return f;
        }

        let child_clones: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_try_clone_fn_for_type_expr(e))
            .collect();
        let child_drops: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_drop_fn_for_type_expr(e))
            .collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);
        let bool_t = self.context.bool_type();

        let saved_bb = self.builder.get_insert_block();
        let f = self.declare_try_clone_fn(&fn_name);
        self.try_clone_fn_cache.insert(type_name, f);

        let entry = self.context.append_basic_block(f, "entry");
        self.builder.position_at_end(entry);
        let src = f.get_nth_param(0).unwrap().into_pointer_value();
        let dst = f.get_nth_param(1).unwrap().into_pointer_value();
        let failed = f.get_nth_param(2).unwrap().into_pointer_value();

        for (i, child) in child_clones.iter().enumerate() {
            let src_field = self
                .builder
                .build_struct_gep(tuple_ty, src, i as u32, &format!("t.f{i}.s"))
                .unwrap();
            let dst_field = self
                .builder
                .build_struct_gep(tuple_ty, dst, i as u32, &format!("t.f{i}.d"))
                .unwrap();
            let ok = self
                .builder
                .build_call(
                    *child,
                    &[src_field.into(), dst_field.into(), failed.into()],
                    &format!("t.f{i}.ok"),
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let next_bb = self.context.append_basic_block(f, &format!("t.f{i}.next"));
            let fail_bb = self.context.append_basic_block(f, &format!("t.f{i}.fail"));
            self.builder
                .build_conditional_branch(ok, next_bb, fail_bb)
                .unwrap();
            // Failure: drop fields [0..i) (already cloned into dst), return false.
            self.builder.position_at_end(fail_bb);
            for (j, drop_fn) in child_drops.iter().enumerate().take(i) {
                let dst_j = self
                    .builder
                    .build_struct_gep(tuple_ty, dst, j as u32, &format!("t.f{i}.cln.{j}"))
                    .unwrap();
                self.builder
                    .build_call(*drop_fn, &[dst_j.into()], "")
                    .unwrap();
            }
            self.builder
                .build_return(Some(&bool_t.const_int(0, false)))
                .unwrap();
            self.builder.position_at_end(next_bb);
        }
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    // ── Drop fn framework (per-type drop emitters, mirror of clone) ──

    /// Emit (or fetch) `karac_drop_<typename>(value: *mut T)` for a given
    /// `TypeExpr`. Mirrors `emit_clone_fn_for_type_expr` (`src/codegen.rs:13367`)
    /// — same dispatcher shape with per-shape sub-emitters and the same
    /// cache-by-`display_mangle_te(te)` pattern.
    ///
    /// The emitted fn has signature `void karac_drop_<typename>(*mut T)`.
    /// Body releases any heap the value owns:
    /// - Primitives: no-op (`ret void`).
    /// - String: free the data buffer when `cap > 0` (skips static-literal
    ///   strings whose data lives in the program's read-only string pool).
    /// - Vec[T]: iterate `0..len` calling `emit_drop_fn_for_type_expr(T)` on
    ///   each element, then free the data buffer when `cap > 0`. Improvement
    ///   over the existing `CleanupAction::FreeVecBuffer` cleanup which only
    ///   recurses one level (`Vec[Vec[T]]` works; `Vec[Vec[Vec[T]]]`
    ///   previously leaked the innermost buffers — tracked in `deferred.md`).
    /// - Tuple: iterate fields, calling each field's drop fn through the
    ///   tuple's `build_struct_gep` offsets.
    /// - Map[K, V] / Set[T]: **placeholder this slice (0.c)** — delegates to
    ///   the existing `karac_map_free` runtime. Per-K/V specialization
    ///   happens in Slice 1+ alongside the monomorphized Map layout.
    ///
    /// Caller convention: takes a pointer to the value's storage (not the
    /// value itself). The pointer-by-reference shape mirrors clone so the
    /// dispatcher returns a uniform signature regardless of type shape.
    ///
    /// See [`wip-monomorphized-collections.md`](../docs/implementation_checklist/wip-monomorphized-collections.md)
    /// §3.3 for the locked design position.
    ///
    /// `#[allow(dead_code)]` (and on each sub-emitter below) until Slice 1
    /// lands the first production consumer. End-to-end tests come with
    /// that slice; the framework is foundation only.
    #[allow(dead_code)]
    pub(super) fn emit_drop_fn_for_type_expr(&mut self, te: &TypeExpr) -> FunctionValue<'ctx> {
        let type_name = Self::display_mangle_te(te);
        if let Some(&f) = self.drop_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_drop_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.drop_fn_cache.insert(type_name, f);
            return f;
        }
        match &te.kind {
            TypeKind::Tuple(elems) if !elems.is_empty() => self.emit_tuple_drop_fn(elems),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str);
                if head == Some("Vec") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        return self.emit_vec_drop_fn(&elem_te);
                    }
                }
                if head == Some("Map") {
                    let args = p.generic_args.as_ref();
                    let k_te = args.and_then(|a| a.first()).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    let v_te = args.and_then(|a| a.get(1)).and_then(|a| match a {
                        GenericArg::Type(t) => Some(t.clone()),
                        _ => None,
                    });
                    if let (Some(k), Some(v)) = (k_te, v_te) {
                        return self.emit_map_drop_fn(&k, &v);
                    }
                }
                if head == Some("Set") {
                    if let Some(GenericArg::Type(elem_te)) =
                        p.generic_args.as_ref().and_then(|a| a.first()).cloned()
                    {
                        // Per §3.4 lock: Set[T] drops as Map[T, ()] — same
                        // delegation pattern emit_clone_fn_for_type_expr
                        // uses at line 13402–13416.
                        let unit_te = TypeExpr {
                            kind: TypeKind::Tuple(Vec::new()),
                            span: elem_te.span.clone(),
                        };
                        return self.emit_map_drop_fn(&elem_te, &unit_te);
                    }
                }
                if head == Some("String") {
                    return self.emit_string_drop_fn();
                }
                self.emit_primitive_drop_fn(&type_name)
            }
            _ => self.emit_primitive_drop_fn(&type_name),
        }
    }

    /// Emit `karac_drop_<typename>` for a primitive (Copy-by-memcpy) type.
    /// Body is `ret void` — primitives don't own heap. Cache-keyed on
    /// `type_name` so repeat callers reuse the same fn.
    #[allow(dead_code)]
    pub(super) fn emit_primitive_drop_fn(&mut self, type_name: &str) -> FunctionValue<'ctx> {
        let fn_name = format!("karac_drop_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.drop_fn_cache.insert(type_name.to_string(), f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        self.drop_fn_cache.insert(type_name.to_string(), drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        drop_fn
    }

    /// Emit `karac_drop_String` — read `cap`; if `cap > 0`, load `data` and
    /// call `free(data)`. Mirrors the existing scope-exit String cleanup's
    /// cap-zero static-buffer skip (see `CleanupAction::FreeVecBuffer`
    /// handling at `src/codegen.rs:3216+`). Does NOT zero out the `{data,
    /// len, cap}` fields after free — caller's responsibility if the slot
    /// is reused; in scope-exit usage the slot is dead anyway.
    #[allow(dead_code)]
    pub(super) fn emit_string_drop_fn(&mut self) -> FunctionValue<'ctx> {
        let type_name = "String".to_string();
        if let Some(&f) = self.drop_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = "karac_drop_String".to_string();
        if let Some(f) = self.module.get_function(&fn_name) {
            self.drop_fn_cache.insert(type_name, f);
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();
        let saved_bb = self.builder.get_insert_block();
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        self.drop_fn_cache.insert(type_name, drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        let free_bb = self.context.append_basic_block(drop_fn, "free");
        let exit_bb = self.context.append_basic_block(drop_fn, "exit");

        self.builder.position_at_end(entry_bb);
        let val = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let cap_p = self
            .builder
            .build_struct_gep(vec_ty, val, 2, "cap.p")
            .unwrap();
        let cap = self
            .builder
            .build_load(i64_t, cap_p, "cap")
            .unwrap()
            .into_int_value();
        let is_heap = self
            .builder
            .build_int_compare(IntPredicate::UGT, cap, i64_t.const_zero(), "is.heap")
            .unwrap();
        self.builder
            .build_conditional_branch(is_heap, free_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, val, 0, "data.pp")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "data")
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        drop_fn
    }

    /// Emit `karac_drop_Vec_<elem>` — iterate `0..len` calling the per-
    /// element drop fn on each `data[i]`, then `free(data)` when `cap > 0`.
    /// Strictly recursive: nested `Vec[Vec[T]]` correctly recurses through
    /// the cache to drop every level, closing the deeper-nesting leak the
    /// existing `FreeVecBuffer` cleanup carries (tracked in `deferred.md` §
    /// *Recursive Drop for Heap-Owned Collection Elements*).
    #[allow(dead_code)]
    pub(super) fn emit_vec_drop_fn(&mut self, elem_te: &TypeExpr) -> FunctionValue<'ctx> {
        let elem_name = Self::display_mangle_te(elem_te);
        let type_name = format!("Vec_{elem_name}");
        if let Some(&f) = self.drop_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_drop_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.drop_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.llvm_type_for_type_expr(elem_te);
        // Recurse first — sub-emitter may switch the builder's insert block.
        let elem_drop = self.emit_drop_fn_for_type_expr(elem_te);

        let saved_bb = self.builder.get_insert_block();
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        self.drop_fn_cache.insert(type_name, drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        let loop_cond_bb = self.context.append_basic_block(drop_fn, "loop.cond");
        let loop_body_bb = self.context.append_basic_block(drop_fn, "loop.body");
        let loop_incr_bb = self.context.append_basic_block(drop_fn, "loop.incr");
        let after_loop_bb = self.context.append_basic_block(drop_fn, "after.loop");
        let free_bb = self.context.append_basic_block(drop_fn, "free");
        let exit_bb = self.context.append_basic_block(drop_fn, "exit");

        self.builder.position_at_end(entry_bb);
        let val = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, val, 0, "data.pp")
            .unwrap();
        let len_p = self
            .builder
            .build_struct_gep(vec_ty, val, 1, "len.p")
            .unwrap();
        let cap_p = self
            .builder
            .build_struct_gep(vec_ty, val, 2, "cap.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_p, "len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_load(i64_t, cap_p, "cap")
            .unwrap()
            .into_int_value();
        let counter = self.create_entry_alloca(drop_fn, "i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_zero())
            .unwrap();
        self.builder
            .build_unconditional_branch(loop_cond_bb)
            .unwrap();

        // Loop: for i in 0..len { drop(data[i]); }
        self.builder.position_at_end(loop_cond_bb);
        let cur = self
            .builder
            .build_load(i64_t, counter, "i.cur")
            .unwrap()
            .into_int_value();
        let lt = self
            .builder
            .build_int_compare(IntPredicate::ULT, cur, len, "i.lt.len")
            .unwrap();
        self.builder
            .build_conditional_branch(lt, loop_body_bb, after_loop_bb)
            .unwrap();

        self.builder.position_at_end(loop_body_bb);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[cur], "elem.ptr")
                .unwrap()
        };
        self.builder
            .build_call(elem_drop, &[elem_ptr.into()], "")
            .unwrap();
        self.builder
            .build_unconditional_branch(loop_incr_bb)
            .unwrap();

        self.builder.position_at_end(loop_incr_bb);
        let next = self
            .builder
            .build_int_add(cur, i64_t.const_int(1, false), "i.next")
            .unwrap();
        self.builder.build_store(counter, next).unwrap();
        self.builder
            .build_unconditional_branch(loop_cond_bb)
            .unwrap();

        // After the per-element loop, free the data buffer if cap > 0
        // (static-literal Vecs with cap=0 skip the free — same convention
        // as the existing FreeVecBuffer cleanup).
        self.builder.position_at_end(after_loop_bb);
        let is_heap = self
            .builder
            .build_int_compare(IntPredicate::UGT, cap, i64_t.const_zero(), "is.heap")
            .unwrap();
        self.builder
            .build_conditional_branch(is_heap, free_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        self.builder
            .build_call(self.free_fn, &[data.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        drop_fn
    }

    /// Emit `karac_drop_tuple_<f0>_<f1>_...` — iterate fields calling each
    /// field's drop fn through `build_struct_gep` offsets. Empty tuples
    /// (unit type `()`) are handled at the dispatcher by the
    /// `TypeKind::Tuple(elems) if !elems.is_empty()` guard — they fall
    /// through to `emit_primitive_drop_fn` with `type_name = "unit"` (or
    /// whatever `display_mangle_te` produces) and emit a `ret void` no-op.
    #[allow(dead_code)]
    pub(super) fn emit_tuple_drop_fn(&mut self, elems: &[TypeExpr]) -> FunctionValue<'ctx> {
        let elems_owned: Vec<TypeExpr> = elems.to_vec();
        let parts: Vec<String> = elems_owned.iter().map(Self::display_mangle_te).collect();
        let type_name = format!("tuple_{}", parts.join("_"));
        if let Some(&f) = self.drop_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_drop_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.drop_fn_cache.insert(type_name, f);
            return f;
        }

        // Recurse first — sub-emitters may switch the builder's insert block.
        let child_fns: Vec<FunctionValue<'ctx>> = elems_owned
            .iter()
            .map(|e| self.emit_drop_fn_for_type_expr(e))
            .collect();
        let field_tys: Vec<BasicTypeEnum<'ctx>> = elems_owned
            .iter()
            .map(|e| self.llvm_type_for_type_expr(e))
            .collect();
        let tuple_ty = self.context.struct_type(&field_tys, false);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        self.drop_fn_cache.insert(type_name, drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let val = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        for (i, child_fn) in child_fns.iter().enumerate() {
            let field_ptr = self
                .builder
                .build_struct_gep(tuple_ty, val, i as u32, &format!("t.f{i}"))
                .unwrap();
            self.builder
                .build_call(*child_fn, &[field_ptr.into()], "")
                .unwrap();
        }
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        drop_fn
    }

    /// Emit `karac_drop_Map_<K>_<V>` — **placeholder this slice (0.c)**.
    /// Body delegates to the existing `karac_map_free` runtime, preserving
    /// today's type-erased Map drop behavior. Slice 1+ replaces this body
    /// (per K/V tuple) with a monomorphized drop sequence that inlines the
    /// per-K and per-V drops without going through the runtime fn.
    ///
    /// The placeholder exists so the framework is complete enough that
    /// callers can request a drop fn for any TypeExpr; it does not commit
    /// to the monomorphized layout.
    #[allow(dead_code)]
    pub(super) fn emit_map_drop_fn(
        &mut self,
        key_te: &TypeExpr,
        val_te: &TypeExpr,
    ) -> FunctionValue<'ctx> {
        let k_name = Self::display_mangle_te(key_te);
        let v_name = Self::display_mangle_te(val_te);
        let type_name = format!("Map_{k_name}_{v_name}");
        if let Some(&f) = self.drop_fn_cache.get(&type_name) {
            return f;
        }
        let fn_name = format!("karac_drop_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.drop_fn_cache.insert(type_name, f);
            return f;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        self.drop_fn_cache.insert(type_name, drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let val = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        // Load the Map handle from the alloca and pass to karac_map_free.
        let handle = self
            .builder
            .build_load(ptr_ty, val, "map.handle")
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_call(self.karac_map_free_fn, &[handle.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        drop_fn
    }
}
