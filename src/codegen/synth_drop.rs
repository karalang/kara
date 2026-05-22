//! Drop-fn synthesis: per-type `__karac_drop_<T>` LLVM functions.
//!
//! Phase 7.2 Slice DP. Houses the two drop-fn synthesisers:
//!
//! - `emit_enum_drop_switch` — for value-type enums with heap-bearing
//!   payloads. Body is a tag switch with per-variant cleanup BBs that
//!   emit the `cap > 0 ? free(data)` pattern for each
//!   `EnumDropKind::VecOrString` field. Memoized in `enum_drop_fns`;
//!   returns `None` for enums with no heap-bearing payload.
//! - `emit_struct_drop_synthesis` — for value-type structs. Walks each
//!   field's drop kind and emits the matching cleanup IR.
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use inkwell::basic_block::BasicBlock;
use inkwell::module::Linkage;
use inkwell::values::FunctionValue;
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use crate::ast::TypeKind;

use super::state::EnumDropKind;

impl<'ctx> super::Codegen<'ctx> {
    /// Phase 7.2 Slice DP — synthesize (or reuse) the per-enum drop
    /// function `__karac_drop_<EnumName>` for value-type enums.
    ///
    /// Body shape:
    /// ```text
    /// fn __karac_drop_E(p: *const E) {
    ///   let tag = (*p).tag;
    ///   switch tag {
    ///     0 => cleanup_variant_0(p);
    ///     1 => cleanup_variant_1(p);
    ///     ...
    ///     default => {}
    ///   }
    ///   ret void
    /// }
    /// ```
    ///
    /// Each per-variant cleanup BB walks the variant's
    /// `field_drop_kinds`; for every `EnumDropKind::VecOrString` field
    /// the BB emits the same `cap > 0 ? free(data)` pattern that
    /// `CleanupAction::FreeVecBuffer` uses inline at the top-level
    /// scope-cleanup drain. Field word offsets come from
    /// `EnumLayout::field_word_offsets` (laid out by `declare_enums`).
    ///
    /// Returns `None` when the enum has no heap-bearing payload anywhere
    /// — saves the synth cost and lets `track_enum_var` skip
    /// registration entirely (no payload to free, no IR bloat from a
    /// tag-switch with all-`ret` arms).
    ///
    /// Lazily memoized in `enum_drop_fns`. Mirrors the existing
    /// `emit_hash_fn_for_type` lazy-synth pattern: the saved insert
    /// block is restored on exit so callers don't have to.
    pub(super) fn emit_enum_drop_switch(&mut self, enum_name: &str) -> Option<FunctionValue<'ctx>> {
        if let Some(f) = self.enum_drop_fns.get(enum_name) {
            return Some(*f);
        }
        // Snapshot what we need before mutably borrowing `self.module`
        // / `self.builder`. The layout is reconstituted from
        // `enum_layouts`; we clone the relevant pieces so the loop body
        // doesn't fight the builder over `&mut self`.
        let layout = self.enum_layouts.get(enum_name)?.clone();
        if layout.is_shared {
            return None; // DP3 — shared enums use RC machinery
        }
        // Skip enums whose every variant has zero heap-bearing fields.
        let any_heap = layout
            .field_drop_kinds
            .values()
            .any(|kinds| kinds.iter().any(|k| *k != EnumDropKind::None));
        if !any_heap {
            return None;
        }

        let fn_name = format!("__karac_drop_{enum_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.enum_drop_fns.insert(enum_name.to_string(), f);
            return Some(f);
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let void_ty = self.context.void_type();
        let vec_ty = self.vec_struct_type();

        let saved_bb = self.builder.get_insert_block();

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        let exit_bb = self.context.append_basic_block(drop_fn, "exit");
        self.builder.position_at_end(entry_bb);
        let p_arg = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        // Load tag (field 0 of the enum struct).
        let tag_ptr = self
            .builder
            .build_struct_gep(layout.llvm_type, p_arg, 0, "drop.tag.p")
            .unwrap();
        let tag_val = self
            .builder
            .build_load(i64_t, tag_ptr, "drop.tag")
            .unwrap()
            .into_int_value();

        // Sort variants by tag for deterministic IR. `tags` HashMap
        // doesn't preserve insertion order; sorting on the discriminant
        // makes the BB layout reproducible across runs.
        let mut tag_entries: Vec<(String, u64)> =
            layout.tags.iter().map(|(n, t)| (n.clone(), *t)).collect();
        tag_entries.sort_by_key(|(_, t)| *t);

        // One BB per variant, all branching to `exit_bb` after their
        // cleanup work.
        let mut switch_cases: Vec<(inkwell::values::IntValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();
        let case_bbs: Vec<(String, u64, BasicBlock<'ctx>)> = tag_entries
            .iter()
            .map(|(name, tag)| {
                let bb = self
                    .context
                    .append_basic_block(drop_fn, &format!("drop.{}", name));
                switch_cases.push((i64_t.const_int(*tag, false), bb));
                (name.clone(), *tag, bb)
            })
            .collect();

        self.builder
            .build_switch(tag_val, exit_bb, &switch_cases)
            .unwrap();

        // Per-variant cleanup BBs — for each heap-bearing payload field
        // (`EnumDropKind::VecOrString`), reload the (data, len, cap)
        // payload words and free the data pointer when cap > 0.
        for (variant_name, _tag, bb) in &case_bbs {
            self.builder.position_at_end(*bb);
            if let Some(kinds) = layout.field_drop_kinds.get(variant_name) {
                if let Some(offsets) = layout.field_word_offsets.get(variant_name) {
                    for (kind, (start_word, _num_words)) in kinds.iter().zip(offsets.iter()) {
                        if *kind != EnumDropKind::VecOrString {
                            continue;
                        }
                        // Field index in `llvm_type` is `start_word + 1`
                        // for the data ptr (tag is field 0); +2 for len;
                        // +3 for cap. Match the insert-side at
                        // `try_compile_enum_variant`.
                        let data_idx = (*start_word + 1) as u32;
                        let cap_idx = (*start_word + 3) as u32;

                        let cap_ptr = self
                            .builder
                            .build_struct_gep(layout.llvm_type, p_arg, cap_idx, "drop.cap.p")
                            .unwrap();
                        let cap_val = self
                            .builder
                            .build_load(i64_t, cap_ptr, "drop.cap")
                            .unwrap()
                            .into_int_value();
                        let zero = i64_t.const_int(0, false);
                        let is_heap = self
                            .builder
                            .build_int_compare(IntPredicate::UGT, cap_val, zero, "drop.is_heap")
                            .unwrap();
                        let free_bb = self.context.append_basic_block(drop_fn, "drop.free");
                        let skip_bb = self.context.append_basic_block(drop_fn, "drop.skip");
                        self.builder
                            .build_conditional_branch(is_heap, free_bb, skip_bb)
                            .unwrap();

                        self.builder.position_at_end(free_bb);
                        // Payload words are stored as i64 at the start_word
                        // slot — for VecOrString that's the data pointer
                        // bit-cast to i64. Load it and convert back to
                        // a pointer for the free call.
                        let data_word_ptr = self
                            .builder
                            .build_struct_gep(layout.llvm_type, p_arg, data_idx, "drop.data.wp")
                            .unwrap();
                        let data_word = self
                            .builder
                            .build_load(i64_t, data_word_ptr, "drop.data.w")
                            .unwrap()
                            .into_int_value();
                        let data_ptr = self
                            .builder
                            .build_int_to_ptr(data_word, ptr_ty, "drop.data.p")
                            .unwrap();
                        self.builder
                            .build_call(self.free_fn, &[data_ptr.into()], "")
                            .unwrap();
                        // After freeing, zero the cap word so a
                        // re-entrant invocation (via aliased binding,
                        // unusual in v1 but defensive) becomes a no-op
                        // through the cap > 0 guard. Mirrors the
                        // FreeVecBuffer semantics implicitly carried by
                        // the runtime's own grow/clear paths.
                        self.builder.build_store(cap_ptr, zero).unwrap();
                        self.builder.build_unconditional_branch(skip_bb).unwrap();

                        self.builder.position_at_end(skip_bb);
                    }
                }
            }
            // Reference the vec_ty so the unused-binding lint stays
            // quiet on builds that don't enter the inner loop with
            // VecOrString fields. (Most do, but the suppress here keeps
            // the helper robust to future drop-kind additions.)
            let _ = vec_ty;
            self.builder.build_unconditional_branch(exit_bb).unwrap();
        }

        self.builder.position_at_end(exit_bb);
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        self.enum_drop_fns.insert(enum_name.to_string(), drop_fn);
        Some(drop_fn)
    }

    /// TypeExpr-aware hash-fn wrapper. Dispatches tuples to a recursive
    /// composition (per-field hash + FNV tail-mix combine) and falls through
    /// Synthesize (or fetch from cache) a per-struct drop function for a
    /// non-shared user struct. Returns `None` when the struct has no
    /// heap-owning fields (every field is primitive / Slice / Ref / etc.)
    /// — in that case no cleanup is needed and `track_struct_var` skips
    /// `CleanupAction::StructDrop` registration entirely. Otherwise emits
    /// `__karac_drop_struct_<Name>(*mut StructTy)` once per struct type
    /// (cached in `struct_drop_fns`) that iterates fields and frees:
    ///
    /// - **Vec / String fields** (`{ptr, len, cap}` layout): load `cap`,
    ///   if > 0 free `(field).data`. Same shape as `FreeVecBuffer`'s
    ///   inline cleanup, just GEP'd into the struct.
    /// - **Map / Set handle fields** (single `ptr`): call
    ///   `karac_map_free` (primitive K / V) or `karac_map_free_with_drop_vec`
    ///   when the field's K or V is itself Vec/String. The drop fn does
    ///   NOT have per-field-instance K/V type info — it conservatively
    ///   routes every Map/Set field to `karac_map_free_with_drop_vec`
    ///   with both flags set, which is correct (the runtime helper
    ///   reads no key/value heap when the relevant size is 0 or the
    ///   field's `cap == 0`).
    ///
    /// Limited to direct Vec/String/Map/Set fields. Nested-struct /
    /// enum / Vec[Vec[T]] field types are NOT recursed in this slice
    /// — that's slice δ's `emit_drop_fn_for_type` framework. Field
    /// type identification uses `struct_field_type_names` (first path
    /// segment of each field's source TypeExpr), so a field typed
    /// `Vec[i64]` is detected by its first segment "Vec".
    pub(super) fn emit_struct_drop_synthesis(
        &mut self,
        struct_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        if let Some(f) = self.struct_drop_fns.get(struct_name) {
            return Some(*f);
        }
        // Shared structs use the RC machinery; their cleanup is via
        // `track_rc_var` / `emit_refcount_dec`, not a synthesized
        // per-value drop fn.
        if self.shared_types.contains_key(struct_name) {
            return None;
        }
        let st = *self.struct_types.get(struct_name)?;
        let field_kinds = self.struct_field_type_names.get(struct_name)?.clone();

        // Classify each field: Vec/String (vec-struct layout), Map/Set
        // (single ptr handle), or no-cleanup. If every field is no-cleanup,
        // skip emission entirely — `track_struct_var` will get `None`
        // and skip the `StructDrop` cleanup action.
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum FieldDrop {
            None,
            VecOrString,
            MapOrSet,
        }
        let kinds: Vec<FieldDrop> = field_kinds
            .iter()
            .map(|opt_name| match opt_name.as_deref() {
                Some("Vec") | Some("VecDeque") | Some("String") => FieldDrop::VecOrString,
                Some("Map") | Some("HashMap") | Some("Set") | Some("HashSet") => {
                    FieldDrop::MapOrSet
                }
                _ => FieldDrop::None,
            })
            .collect();
        if kinds.iter().all(|k| *k == FieldDrop::None) {
            return None;
        }

        let fn_name = format!("__karac_drop_struct_{struct_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.struct_drop_fns.insert(struct_name.to_string(), f);
            return Some(f);
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let void_ty = self.context.void_type();
        let vec_ty = self.vec_struct_type();

        let saved_bb = self.builder.get_insert_block();

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        self.struct_drop_fns
            .insert(struct_name.to_string(), drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let p_arg = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        for (field_idx, kind) in kinds.iter().enumerate() {
            match kind {
                FieldDrop::None => {}
                FieldDrop::VecOrString => {
                    // GEP the Vec struct field within the parent struct.
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            st,
                            p_arg,
                            field_idx as u32,
                            &format!("drop.field{field_idx}.p"),
                        )
                        .unwrap();
                    // Load cap (Vec struct field index 2).
                    let cap_ptr = self
                        .builder
                        .build_struct_gep(
                            vec_ty,
                            field_ptr,
                            2,
                            &format!("drop.field{field_idx}.cap.p"),
                        )
                        .unwrap();
                    let cap = self
                        .builder
                        .build_load(i64_t, cap_ptr, &format!("drop.field{field_idx}.cap"))
                        .unwrap()
                        .into_int_value();
                    let zero = i64_t.const_int(0, false);
                    let is_heap = self
                        .builder
                        .build_int_compare(
                            IntPredicate::UGT,
                            cap,
                            zero,
                            &format!("drop.field{field_idx}.is_heap"),
                        )
                        .unwrap();
                    let free_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("drop.field{field_idx}.free"));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("drop.field{field_idx}.skip"));
                    self.builder
                        .build_conditional_branch(is_heap, free_bb, skip_bb)
                        .unwrap();
                    self.builder.position_at_end(free_bb);
                    let data_ptr_ptr = self
                        .builder
                        .build_struct_gep(
                            vec_ty,
                            field_ptr,
                            0,
                            &format!("drop.field{field_idx}.data.p"),
                        )
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_ptr_ptr, &format!("drop.field{field_idx}.data"))
                        .unwrap()
                        .into_pointer_value();
                    self.builder
                        .build_call(self.free_fn, &[data.into()], "")
                        .unwrap();
                    self.builder.build_unconditional_branch(skip_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                }
                FieldDrop::MapOrSet => {
                    // Map/Set field is a single opaque ptr stored inline.
                    // Load the handle; if non-null, route to
                    // `karac_map_free_with_drop_vec(handle, 1, 1)` —
                    // conservatively drop both sides. The runtime helper
                    // is a no-op for the side whose `cap == 0` or whose
                    // `_size == 0`, so over-flagging is correctness-safe
                    // even on `Map[i64, i64]` / `Set[i64]` fields (those
                    // never had a `data` ptr to free; `cap == 0` skips
                    // the free). When per-field K/V type info is wired
                    // through (slice δ), tighten the flags to the
                    // minimum needed.
                    //
                    // Shared-half rc_dec walks (item 4, 2026-05-16):
                    // when the field's K or V is a shared struct /
                    // shared enum, emit the per-bucket walk BEFORE
                    // the runtime free releases the bucket storage —
                    // mirrors the `CleanupAction::FreeMapHandle`
                    // ordering. Without this, a `struct Owner { m:
                    // Map[i64, Node] }` (with `Node` shared) drops
                    // the Map's bucket storage without ever dec'ing
                    // the shared values, leaking one ref per live
                    // entry. The runtime helper is type-erased and
                    // can't see the shared layout itself.
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            st,
                            p_arg,
                            field_idx as u32,
                            &format!("drop.field{field_idx}.p"),
                        )
                        .unwrap();
                    let handle = self
                        .builder
                        .build_load(ptr_ty, field_ptr, &format!("drop.field{field_idx}.handle"))
                        .unwrap()
                        .into_pointer_value();
                    if let Some(field_te) = self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .cloned()
                    {
                        // The walks build their own basic blocks via
                        // `self.current_fn.unwrap()`, which at this
                        // point still points at the outer function
                        // that triggered drop synthesis (e.g.,
                        // `main`). Swap to `drop_fn` so the blocks
                        // attach to the synthesized drop body, then
                        // restore. Save / restore matches the
                        // surrounding `saved_bb` discipline so the
                        // outer caller's builder position is
                        // untouched.
                        let saved_fn = self.current_fn;
                        self.current_fn = Some(drop_fn);
                        if let Some((k_te, v_te)) = super::helpers::map_kv_type_exprs(&field_te) {
                            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&v_te) {
                                self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, true);
                            }
                            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&k_te) {
                                self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, false);
                            }
                        } else if let Some(elem_te) = super::helpers::set_inner_type_expr(&field_te)
                        {
                            // `Set[T]` lowers to `Map[T, ()]`; the
                            // element occupies the key half.
                            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&elem_te) {
                                self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, false);
                            }
                        }
                        self.current_fn = saved_fn;
                    }
                    let one = i32_t.const_int(1, false);
                    self.builder
                        .build_call(
                            self.karac_map_free_with_drop_vec_fn,
                            &[handle.into(), one.into(), one.into()],
                            "",
                        )
                        .unwrap();
                }
            }
        }

        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        Some(drop_fn)
    }

    /// Synthesize (or reuse) the per-shared-struct recursive drop fn
    /// `__karac_rc_drop_<Name>(ptr)` for shared structs whose heap
    /// payload owns transitive refs. Mirrors `emit_struct_drop_synthesis`'s
    /// shape but operates on heap-allocated rc-headed objects and
    /// recursively dec's shared / `Option[shared T]` fields before
    /// `free(ptr)`.
    ///
    /// Body shape:
    /// ```text
    /// fn __karac_rc_drop_S(p: *mut S_heap) {
    ///   // field walk — per field type:
    ///   //   shared T  →  rc_dec(load p->field) (calls __karac_rc_drop_T inline)
    ///   //   Option[shared T] →  if tag==Some, rc_dec(load p->field_w0)
    ///   //   Vec/String        →  cap > 0 ? free(data)
    ///   //   Map/Set           →  karac_map_free*
    ///   //   primitive         →  no-op
    ///   free(p)
    ///   ret void
    /// }
    /// ```
    ///
    /// Returns `None` for shared structs with no heap-owning fields
    /// (every field is primitive / `Slice` / `Ref` / etc.) — `emit_rc_dec`
    /// falls through to plain `free(ptr)` for those. Memoized in
    /// `rc_drop_fns`; recursive calls handle cyclic / self-referential
    /// shared-struct shapes (`shared struct Node { next: Option[Node] }`
    /// is the canonical case) via the standard "insert the FunctionValue
    /// into the cache BEFORE filling in the body" trick.
    ///
    /// Lazily called from `track_rc_var` / `track_rc_option_var` so the
    /// synthesis cost is paid once per shared-struct type that actually
    /// reaches RC cleanup. Closes the LeetCode #2 kata bench's
    /// 100-node-chain leak (2026-05-17): without recursive drop, every
    /// `add_two_numbers` call leaked 99 transitive nodes (only the
    /// head was freed at scope-exit; `head.next.…` was stranded).
    pub(super) fn emit_shared_struct_rc_drop_fn(
        &mut self,
        struct_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        // Memoized — both `Some(fn)` (drop fn emitted) and `None`
        // (struct has no walkable fields, plain `free` suffices) cache.
        if let Some(slot) = self.rc_drop_fns.get(struct_name) {
            return *slot;
        }
        let info = self.shared_types.get(struct_name)?.clone();
        if info.is_enum {
            // Shared enums aren't handled here in v1 — the kata
            // bench shape is shared structs only. Caching `None`
            // tells `emit_rc_dec` to use plain free for shared
            // enums (matching the pre-fix behavior).
            self.rc_drop_fns.insert(struct_name.to_string(), None);
            return None;
        }
        let heap_type = info.heap_type;
        let field_type_exprs = self
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
            .unwrap_or_default();
        let field_kinds = self
            .struct_field_type_names
            .get(struct_name)
            .cloned()
            .unwrap_or_default();

        // Classify each field. Recurse-shared is the new entry —
        // a shared-struct field whose inner heap layout we already
        // know how to walk. `OptionShared` is the `Option[shared T]`
        // field shape. The other kinds mirror `emit_struct_drop_synthesis`'s
        // FieldDrop classifier (Vec/String/Map/Set) so a `shared struct
        // Holder { v: Vec[i64], next: Option[Holder] }` is handled
        // uniformly: free the Vec buffer, then dec the Option-inner,
        // then free the Holder itself.
        #[derive(Clone)]
        enum SharedFieldKind<'a, 'ctx> {
            None,
            VecOrString,
            MapOrSet,
            RecurseShared(super::state::SharedTypeInfo<'ctx>),
            OptionShared(super::state::SharedTypeInfo<'ctx>),
            /// Niche-optimized `Option[shared T]` field: heap slot is a
            /// single `ptr` (null = None, non-null = Some), not the 4-i64
            /// Option enum. Drop path collapses to one null-check + dec.
            OptionSharedNiche(super::state::SharedTypeInfo<'ctx>),
            #[allow(dead_code)]
            _Phantom(&'a ()),
        }
        let kinds: Vec<SharedFieldKind<'_, 'ctx>> = field_type_exprs
            .iter()
            .enumerate()
            .map(|(i, te)| {
                let head_name = field_kinds.get(i).and_then(|n| n.as_deref());
                // Option[shared T]?
                if let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(te) {
                    if self.niche_field_inner_heap_type(struct_name, i).is_some() {
                        return SharedFieldKind::OptionSharedNiche(inner_info);
                    }
                    return SharedFieldKind::OptionShared(inner_info);
                }
                // Plain shared T?
                if let TypeKind::Path(p) = &te.kind {
                    if let Some(seg) = p.segments.last() {
                        if let Some(info) = self.shared_types.get(seg.as_str()).cloned() {
                            return SharedFieldKind::RecurseShared(info);
                        }
                    }
                }
                match head_name {
                    Some("Vec") | Some("VecDeque") | Some("String") => SharedFieldKind::VecOrString,
                    Some("Map") | Some("HashMap") | Some("Set") | Some("HashSet") => {
                        SharedFieldKind::MapOrSet
                    }
                    _ => SharedFieldKind::None,
                }
            })
            .collect();

        // No heap-owning / no-recursive-drop fields — fall back to
        // plain `free(ptr)`. Cache `None` so subsequent calls skip
        // the field walk.
        let any_walkable = kinds
            .iter()
            .any(|k| !matches!(k, SharedFieldKind::None | SharedFieldKind::_Phantom(_)));
        if !any_walkable {
            self.rc_drop_fns.insert(struct_name.to_string(), None);
            return None;
        }

        // Pre-pass: synthesize the drop fns for any sibling shared
        // types referenced by this struct's fields (recursive + option
        // arms). `emit_rc_dec` consults `rc_drop_fns` at body-fill
        // time via `&self`, so the entries must exist before we
        // start emitting field-walk IR. Self-referential
        // `shared struct Node { next: Option[Node] }` shapes are
        // handled by inserting `struct_name`'s entry first (below);
        // sibling cross-type chains (`Node → Other → Node`) come
        // back here.
        for kind in &kinds {
            match kind {
                SharedFieldKind::RecurseShared(info)
                | SharedFieldKind::OptionShared(info)
                | SharedFieldKind::OptionSharedNiche(info) => {
                    if let Some(inner_name) = self.struct_name_for_heap_type(info.heap_type) {
                        if inner_name != struct_name && !self.rc_drop_fns.contains_key(&inner_name)
                        {
                            let _ = self.emit_shared_struct_rc_drop_fn(&inner_name);
                        }
                    }
                }
                _ => {}
            }
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let void_ty = self.context.void_type();
        let vec_ty = self.vec_struct_type();
        let option_ty = self.enum_layouts["Option"].llvm_type;

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let fn_name = format!("__karac_rc_drop_{struct_name}");
        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        // Insert into cache BEFORE filling in the body so self-
        // recursive fields (`shared struct Node { next: Option[Node] }`)
        // resolve to the in-progress FunctionValue rather than
        // recursing into a fresh synthesis. Standard memoization
        // discipline; mirrors how `clone_fn_cache` handles self-
        // recursive types.
        self.rc_drop_fns
            .insert(struct_name.to_string(), Some(drop_fn));
        self.current_fn = Some(drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let p_arg = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        for (field_idx, kind) in kinds.iter().enumerate() {
            // Heap layout: refcount at idx 0, then user fields. So
            // user field `field_idx` lives at heap index `field_idx + 1`.
            let heap_field_idx = (field_idx + 1) as u32;
            match kind {
                SharedFieldKind::None | SharedFieldKind::_Phantom(_) => {}
                SharedFieldKind::RecurseShared(inner_info) => {
                    // Load the field's inner pointer and recursively
                    // dec its refcount. The inner heap layout's drop
                    // fn is synth'd on demand by the next call to
                    // `emit_shared_struct_rc_drop_fn` (or already
                    // cached). The inner dec uses the inner's heap
                    // type so the GEP into refcount field is correct.
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            heap_type,
                            p_arg,
                            heap_field_idx,
                            &format!("rcdrop.field{field_idx}.p"),
                        )
                        .unwrap();
                    let inner = self
                        .builder
                        .build_load(ptr_ty, field_ptr, &format!("rcdrop.field{field_idx}.ptr"))
                        .unwrap()
                        .into_pointer_value();
                    let inner_is_null = self
                        .builder
                        .build_is_null(inner, &format!("rcdrop.field{field_idx}.is_null"))
                        .unwrap();
                    let do_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.field{field_idx}.do"));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.field{field_idx}.skip"));
                    self.builder
                        .build_conditional_branch(inner_is_null, skip_bb, do_bb)
                        .unwrap();
                    self.builder.position_at_end(do_bb);
                    self.emit_rc_dec(inner_info.heap_type, inner);
                    self.builder.build_unconditional_branch(skip_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                }
                SharedFieldKind::OptionShared(inner_info) => {
                    // GEP to the Option field's tag, branch on
                    // Some, then dec the inner pointer at w0.
                    // Mirrors `CleanupAction::RcDecOption`'s shape
                    // (see `emit_cleanup_action`).
                    let opt_field_ptr = self
                        .builder
                        .build_struct_gep(
                            heap_type,
                            p_arg,
                            heap_field_idx,
                            &format!("rcdrop.opt{field_idx}.p"),
                        )
                        .unwrap();
                    let tag_ptr = self
                        .builder
                        .build_struct_gep(
                            option_ty,
                            opt_field_ptr,
                            0,
                            &format!("rcdrop.opt{field_idx}.tag.p"),
                        )
                        .unwrap();
                    let tag = self
                        .builder
                        .build_load(i64_t, tag_ptr, &format!("rcdrop.opt{field_idx}.tag"))
                        .unwrap()
                        .into_int_value();
                    let some_tag = self
                        .enum_layouts
                        .get("Option")
                        .and_then(|l| l.tags.get("Some").copied())
                        .unwrap_or(1);
                    let is_some = self
                        .builder
                        .build_int_compare(
                            IntPredicate::EQ,
                            tag,
                            i64_t.const_int(some_tag, false),
                            &format!("rcdrop.opt{field_idx}.is_some"),
                        )
                        .unwrap();
                    let do_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.opt{field_idx}.do"));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.opt{field_idx}.skip"));
                    self.builder
                        .build_conditional_branch(is_some, do_bb, skip_bb)
                        .unwrap();
                    self.builder.position_at_end(do_bb);
                    let w0_ptr = self
                        .builder
                        .build_struct_gep(
                            option_ty,
                            opt_field_ptr,
                            1,
                            &format!("rcdrop.opt{field_idx}.w0.p"),
                        )
                        .unwrap();
                    let w0 = self
                        .builder
                        .build_load(i64_t, w0_ptr, &format!("rcdrop.opt{field_idx}.w0"))
                        .unwrap()
                        .into_int_value();
                    let inner = self
                        .builder
                        .build_int_to_ptr(w0, ptr_ty, &format!("rcdrop.opt{field_idx}.inner"))
                        .unwrap();
                    let inner_is_null = self
                        .builder
                        .build_is_null(inner, &format!("rcdrop.opt{field_idx}.inner.is_null"))
                        .unwrap();
                    let inner_do_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.opt{field_idx}.inner.do"));
                    let inner_skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.opt{field_idx}.inner.skip"));
                    self.builder
                        .build_conditional_branch(inner_is_null, inner_skip_bb, inner_do_bb)
                        .unwrap();
                    self.builder.position_at_end(inner_do_bb);
                    self.emit_rc_dec(inner_info.heap_type, inner);
                    self.builder
                        .build_unconditional_branch(inner_skip_bb)
                        .unwrap();
                    self.builder.position_at_end(inner_skip_bb);
                    self.builder.build_unconditional_branch(skip_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                }
                SharedFieldKind::OptionSharedNiche(inner_info) => {
                    // Niche layout: the heap field is a single nullable
                    // pointer. Same drop discipline as `RecurseShared`
                    // (the conventional non-Option `shared T` field
                    // arm) — load, null-check, dec_ref — but reached
                    // via the source-level `Option[shared T]` field
                    // type rather than a bare `shared T`. The 24-byte
                    // saving over `OptionShared` is the niche-opt
                    // payoff for self-referential / linked-list shapes.
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            heap_type,
                            p_arg,
                            heap_field_idx,
                            &format!("rcdrop.niche{field_idx}.p"),
                        )
                        .unwrap();
                    let inner = self
                        .builder
                        .build_load(ptr_ty, field_ptr, &format!("rcdrop.niche{field_idx}.ptr"))
                        .unwrap()
                        .into_pointer_value();
                    let inner_is_null = self
                        .builder
                        .build_is_null(inner, &format!("rcdrop.niche{field_idx}.is_null"))
                        .unwrap();
                    let do_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.niche{field_idx}.do"));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.niche{field_idx}.skip"));
                    self.builder
                        .build_conditional_branch(inner_is_null, skip_bb, do_bb)
                        .unwrap();
                    self.builder.position_at_end(do_bb);
                    self.emit_rc_dec(inner_info.heap_type, inner);
                    self.builder.build_unconditional_branch(skip_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                }
                SharedFieldKind::VecOrString => {
                    // GEP to the Vec/String struct field, check cap
                    // > 0, free data. Mirrors
                    // `emit_struct_drop_synthesis`'s VecOrString arm.
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            heap_type,
                            p_arg,
                            heap_field_idx,
                            &format!("rcdrop.vec{field_idx}.p"),
                        )
                        .unwrap();
                    let cap_ptr = self
                        .builder
                        .build_struct_gep(
                            vec_ty,
                            field_ptr,
                            2,
                            &format!("rcdrop.vec{field_idx}.cap.p"),
                        )
                        .unwrap();
                    let cap = self
                        .builder
                        .build_load(i64_t, cap_ptr, &format!("rcdrop.vec{field_idx}.cap"))
                        .unwrap()
                        .into_int_value();
                    let zero = i64_t.const_int(0, false);
                    let is_heap = self
                        .builder
                        .build_int_compare(
                            IntPredicate::UGT,
                            cap,
                            zero,
                            &format!("rcdrop.vec{field_idx}.is_heap"),
                        )
                        .unwrap();
                    let free_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.vec{field_idx}.free"));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("rcdrop.vec{field_idx}.skip"));
                    self.builder
                        .build_conditional_branch(is_heap, free_bb, skip_bb)
                        .unwrap();
                    self.builder.position_at_end(free_bb);
                    let data_ptr_ptr = self
                        .builder
                        .build_struct_gep(
                            vec_ty,
                            field_ptr,
                            0,
                            &format!("rcdrop.vec{field_idx}.data.p"),
                        )
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_ptr_ptr, &format!("rcdrop.vec{field_idx}.data"))
                        .unwrap()
                        .into_pointer_value();
                    self.builder
                        .build_call(self.free_fn, &[data.into()], "")
                        .unwrap();
                    self.builder.build_unconditional_branch(skip_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                }
                SharedFieldKind::MapOrSet => {
                    // Map/Set: conservative `karac_map_free_with_drop_vec`
                    // with both flags set. The runtime helper no-ops
                    // each side whose `cap == 0` / `_size == 0`, so
                    // over-flagging is correctness-safe for
                    // primitive-only maps. Mirrors
                    // `emit_struct_drop_synthesis`'s Map/Set arm.
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            heap_type,
                            p_arg,
                            heap_field_idx,
                            &format!("rcdrop.map{field_idx}.p"),
                        )
                        .unwrap();
                    let handle = self
                        .builder
                        .build_load(ptr_ty, field_ptr, &format!("rcdrop.map{field_idx}.handle"))
                        .unwrap()
                        .into_pointer_value();
                    let one = i32_t.const_int(1, false);
                    self.builder
                        .build_call(
                            self.karac_map_free_with_drop_vec_fn,
                            &[handle.into(), one.into(), one.into()],
                            "",
                        )
                        .unwrap();
                }
            }
        }

        // Finally, free the heap allocation itself.
        self.builder
            .build_call(self.free_fn, &[p_arg.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        self.current_fn = saved_fn;

        Some(drop_fn)
    }
}
