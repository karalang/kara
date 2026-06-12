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
use inkwell::values::{FunctionValue, PointerValue};
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
            /// Phase-8 line 39 follow-up — an i64 field that's an opaque
            /// handle into a runtime side-table; free it via the named
            /// extern (guarded on `handle != 0`) at scope exit.
            HttpHandleFree(&'static str),
            /// A nested *anonymous tuple* field whose own fields carry heap
            /// (Vec/String). The name-based classifier above sees no type name
            /// for a tuple and leaves it `None`, so its inner String/Vec
            /// buffers leak on drop (B-2026-06-11-4 part c). Recurse via
            /// `emit_aggregate_heap_field_frees` (which descends into nested
            /// aggregates, cap-guarding each Vec/String free). *Named* nested
            /// struct fields take the `NestedStruct` path instead — the
            /// type-driven recursion here is enum- and Map-blind.
            NestedAggregate,
            /// #18 — a field whose declared type is a *named* non-shared user
            /// struct. Routed through that struct's own
            /// `__karac_drop_struct_<S>` (synthesized on demand) rather than
            /// the type-driven `emit_aggregate_heap_field_frees`. Strictly more
            /// complete: the nested struct's drop fn frees its Vec/String
            /// **and** its enum fields (post-#15) **and** its Map/Set fields —
            /// none of which the LLVM-type-driven aggregate walk reaches (an
            /// enum's layout is all-i64 words, a Map is a bare `ptr`; both are
            /// invisible to `aggregate_has_heap_field`). Closes the canonical
            /// leak `struct Wrap { sp: Span }` where `Span` holds a heap enum:
            /// dropping a `Wrap` undestructured leaked `sp.tok`'s payload. The
            /// struct name is the field's declared type name (read from
            /// `field_kinds` at emit time). If the nested struct turns out to
            /// need no drop, `emit_struct_drop_synthesis` returns `None` and
            /// nothing is emitted.
            NestedStruct,
            /// #15 — a field whose declared type is a heap-bearing, non-shared
            /// *user enum* (e.g. `tok: Token`). The name-based pass leaves it
            /// `None` (its name isn't Vec/String/Map), and the nested-aggregate
            /// pass misses it because an enum's LLVM layout is all-i64 payload
            /// words (no `vec_struct_type` field), so `aggregate_has_heap_field`
            /// returns false. Without this kind the live variant's String/Vec
            /// payload leaks at the owning struct's scope exit. Freed by
            /// invoking the enum's own `__karac_drop_<E>` switch on the field
            /// ptr (`emit_enum_drop_switch`). `Option`/`Result` are excluded
            /// (their inline payloads are handled by the let-binding inline-drop
            /// machinery, not struct drop) — their struct-field payload leak is
            /// a separate, still-bounded remainder.
            EnumField,
        }
        let mut kinds: Vec<FieldDrop> = field_kinds
            .iter()
            .map(|opt_name| match opt_name.as_deref() {
                Some("Vec") | Some("VecDeque") | Some("String") => FieldDrop::VecOrString,
                Some("Map") | Some("HashMap") | Some("Set") | Some("HashSet") => {
                    FieldDrop::MapOrSet
                }
                _ => FieldDrop::None,
            })
            .collect();
        // Nested-aggregate / nested-struct detection: a field the name-based
        // pass left `None` whose LLVM type is a struct/tuple (not the Vec
        // struct). A *named* non-shared user struct (#18) routes through its
        // own `__karac_drop_struct_<S>` — which frees its Vec/String, enum
        // (post-#15), and Map/Set fields, none reachable by the enum- and
        // Map-blind type-driven walk. An *anonymous tuple* with direct
        // Vec/String heap routes through `emit_aggregate_heap_field_frees`.
        {
            let vec_ty = self.vec_struct_type();
            // Phase 1: classify the tuple case inline; defer named nested
            // structs (their drop synth needs `&mut self`, which conflicts
            // with `kinds.iter_mut()`).
            let mut named_struct_fields: Vec<usize> = Vec::new();
            for (idx, k) in kinds.iter_mut().enumerate() {
                if *k != FieldDrop::None {
                    continue;
                }
                let Some(inkwell::types::BasicTypeEnum::StructType(fst)) =
                    st.get_field_type_at_index(idx as u32)
                else {
                    continue;
                };
                if fst == vec_ty {
                    continue;
                }
                // Named non-shared user struct -> NestedStruct (decided in
                // phase 2 after synth, so a no-heap nested struct emits
                // nothing). Note this PRECEDES the type-driven check: a named
                // struct whose only heap is inside an enum has
                // `aggregate_has_heap_field == false`, so the old path would
                // misclassify it `None` and leak (the exact #18 bug).
                if let Some(Some(name)) = field_kinds.get(idx) {
                    if self.struct_types.contains_key(name) && !self.shared_types.contains_key(name)
                    {
                        named_struct_fields.push(idx);
                        continue;
                    }
                }
                // Anonymous tuple (no declared type name) with direct heap.
                if self.aggregate_has_heap_field(fst) {
                    *k = FieldDrop::NestedAggregate;
                }
            }
            // Phase 2: synthesize each named nested struct's drop fn; mark
            // `NestedStruct` only when one is actually needed (`Some`).
            for idx in named_struct_fields {
                if let Some(Some(name)) = field_kinds.get(idx).cloned() {
                    if self.emit_struct_drop_synthesis(&name).is_some() {
                        kinds[idx] = FieldDrop::NestedStruct;
                    }
                }
            }
        }
        // #15 — enum-field detection: a field the passes above left `None`
        // whose declared type name is a heap-bearing, non-shared user enum.
        // Name-based (an enum's LLVM layout — all-i64 words — is invisible to
        // the type-driven nested-aggregate pass). `Option`/`Result` are
        // skipped: their inline payloads are dropped by the let-binding
        // inline-drop machinery, and routing them through the enum drop switch
        // here would risk double-freeing that path.
        for (idx, k) in kinds.iter_mut().enumerate() {
            if *k != FieldDrop::None {
                continue;
            }
            let Some(Some(name)) = field_kinds.get(idx) else {
                continue;
            };
            if name == "Option" || name == "Result" {
                continue;
            }
            if let Some(layout) = self.enum_layouts.get(name) {
                let heap_bearing = !layout.is_shared
                    && layout
                        .field_drop_kinds
                        .values()
                        .any(|kinds| kinds.iter().any(|dk| *dk != EnumDropKind::None));
                if heap_bearing {
                    *k = FieldDrop::EnumField;
                }
            }
        }
        // Phase-8 line 39 follow-up — the seeded HTTP handle-structs carry
        // an i64 side-table key (`Response.headers` / `RequestBuilder.handle`)
        // that leaks until process exit without an explicit free. Override
        // the classifier for the exact (struct, field) the client path
        // seeds, GUARDED on the LLVM field actually being i64 — so a
        // user-defined struct that shares the name (e.g. a server-side
        // `Response { status, body, headers: Vec[(String, String)] }`
        // whose field 2 is a Vec aggregate, not an i64) is never misread
        // as a handle and double-freed. (For such a user struct, field 2
        // stays classified `VecOrString` by its type name above.)
        let http_handle: Option<(usize, &'static str)> = match struct_name {
            "Response" => Some((2, "karac_runtime_http_response_headers_free")),
            "RequestBuilder" => Some((0, "karac_runtime_http_builder_free")),
            _ => None,
        };
        if let Some((idx, extern_name)) = http_handle {
            let is_i64_field = matches!(
                st.get_field_type_at_index(idx as u32),
                Some(inkwell::types::BasicTypeEnum::IntType(t)) if t.get_bit_width() == 64
            );
            if idx < kinds.len() && is_i64_field {
                kinds[idx] = FieldDrop::HttpHandleFree(extern_name);
            }
        }
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
                FieldDrop::HttpHandleFree(extern_name) => {
                    // Phase-8 line 39 follow-up — load the i64 side-table
                    // handle; free the entry only when non-zero. A zeroed
                    // handle means the value was move-suppressed (the
                    // consumer owns it now) or is the Err-path sentinel —
                    // mirrors the `cap > 0` guard the VecOrString arm uses.
                    // The runtime free is itself a no-op on 0 / unknown, so
                    // the guard is an optimization, not a correctness
                    // requirement (a missed move-suppression degrades to a
                    // harmless idempotent double-remove, never a corruption).
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            st,
                            p_arg,
                            field_idx as u32,
                            &format!("drop.field{field_idx}.handle.p"),
                        )
                        .unwrap();
                    let handle = self
                        .builder
                        .build_load(i64_t, field_ptr, &format!("drop.field{field_idx}.handle"))
                        .unwrap()
                        .into_int_value();
                    let zero = i64_t.const_int(0, false);
                    let is_live = self
                        .builder
                        .build_int_compare(
                            IntPredicate::NE,
                            handle,
                            zero,
                            &format!("drop.field{field_idx}.handle.live"),
                        )
                        .unwrap();
                    let free_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("drop.field{field_idx}.handle.free"));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("drop.field{field_idx}.handle.skip"));
                    self.builder
                        .build_conditional_branch(is_live, free_bb, skip_bb)
                        .unwrap();
                    self.builder.position_at_end(free_bb);
                    let free_fn = self
                        .module
                        .get_function(extern_name)
                        .unwrap_or_else(|| panic!("{extern_name} declared in Codegen::new"));
                    self.builder
                        .build_call(free_fn, &[handle.into()], "")
                        .unwrap();
                    self.builder.build_unconditional_branch(skip_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                }
                FieldDrop::NestedAggregate => {
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            st,
                            p_arg,
                            field_idx as u32,
                            &format!("drop.field{field_idx}.nested.p"),
                        )
                        .unwrap();
                    let fst = match st.get_field_type_at_index(field_idx as u32) {
                        Some(inkwell::types::BasicTypeEnum::StructType(t)) => t,
                        _ => continue,
                    };
                    // `emit_aggregate_heap_field_frees` appends cap-guard basic
                    // blocks to `current_fn`; point it at this drop fn during
                    // the recursion, then restore (same discipline as the
                    // `MapOrSet` shared-half walk above).
                    let saved_fn = self.current_fn;
                    self.current_fn = Some(drop_fn);
                    self.emit_aggregate_heap_field_frees(field_ptr, fst);
                    self.current_fn = saved_fn;
                }
                FieldDrop::NestedStruct => {
                    // #18 — route the nested struct field through its own
                    // `__karac_drop_struct_<S>` (synthesized during
                    // classification, fetched here as a cache hit). That fn
                    // frees the nested struct's Vec/String, enum (post-#15),
                    // and Map/Set fields — the canonical case being
                    // `Wrap { sp: Span }` where `Span` holds a heap enum, which
                    // the enum-blind `emit_aggregate_heap_field_frees` leaked.
                    let Some(Some(nested_name)) = field_kinds.get(field_idx).cloned() else {
                        continue;
                    };
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            st,
                            p_arg,
                            field_idx as u32,
                            &format!("drop.field{field_idx}.nstruct.p"),
                        )
                        .unwrap();
                    // Memoized — the synth in phase 2 of detection already
                    // emitted it; this is a cache hit that touches no IR until
                    // the call below.
                    if let Some(nested_drop_fn) = self.emit_struct_drop_synthesis(&nested_name) {
                        self.builder
                            .build_call(nested_drop_fn, &[field_ptr.into()], "")
                            .unwrap();
                    }
                }
                FieldDrop::EnumField => {
                    // #15 — the field is an inline user-enum value. GEP its
                    // ptr within the parent struct and invoke the enum's own
                    // `__karac_drop_<E>` switch, which tag-dispatches and frees
                    // the live variant's heap payload (cap-guarded, then zeroes
                    // the cap word so a re-entrant call no-ops). The enum name
                    // is the field's declared type name.
                    let Some(Some(enum_name)) = field_kinds.get(field_idx).cloned() else {
                        continue;
                    };
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            st,
                            p_arg,
                            field_idx as u32,
                            &format!("drop.field{field_idx}.enum.p"),
                        )
                        .unwrap();
                    // `emit_enum_drop_switch` builds its own function/blocks and
                    // saves/restores the current builder block, so the call site
                    // below resumes in this drop fn's body. Memoized — repeated
                    // struct fields of the same enum reuse one switch.
                    if let Some(enum_drop_fn) = self.emit_enum_drop_switch(&enum_name) {
                        self.builder
                            .build_call(enum_drop_fn, &[field_ptr.into()], "")
                            .unwrap();
                    }
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

        let any_walkable = kinds
            .iter()
            .any(|k| !matches!(k, SharedFieldKind::None | SharedFieldKind::_Phantom(_)));
        // A user `impl Drop for <SharedType>` must fire at refcount→0,
        // before field cleanup and the heap free — RAII parity with the
        // value-type path's `emit_user_drop_wrapper`. Gate on the
        // authoritative `drop_method_keys`; the `<Type>.drop` LLVM symbol
        // is already declared by the time this synth runs (the impl-method
        // declare pass in `compile_program` precedes all body lowering,
        // and `track_rc_var` triggers this fn during body lowering).
        let has_user_drop = self
            .program_snapshot
            .as_ref()
            .is_some_and(|p| p.drop_method_keys.contains_key(struct_name));
        // No heap-owning / no-recursive-drop fields AND no user drop —
        // plain `free(ptr)` suffices, cache `None`. A user drop forces a
        // synth fn even for a primitive-only shared struct so its body
        // fires before the free (the field walk below is then all no-ops).
        if !any_walkable && !has_user_drop {
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

        // User `impl Drop` body runs first. The refcount has already
        // reached 0 by the time this fn is invoked (`emit_rc_dec` /
        // `emit_arc_dec` dispatch here only inside their last-dec free
        // branch), so the heap object is uniquely owned and the body
        // observes live fields before the walk below frees them.
        if has_user_drop {
            if let Some(user_drop_fn) = self.module.get_function(&format!("{struct_name}.drop")) {
                // Shared-struct methods receive `self` as a pointer to the
                // binding slot that holds the heap pointer (`get_data_ptr`
                // hands normal dispatch the alloca, and the body loads the
                // heap pointer out of it). `p_arg` is the heap pointer
                // itself, so wrap it in a temp slot to match that ABI —
                // passing `p_arg` directly would make the body load
                // `heap[0]` (the refcount, already 0 here) as the self
                // pointer and dereference null.
                let self_slot =
                    self.create_entry_alloca(drop_fn, "rcdrop.self.slot", ptr_ty.into());
                self.builder.build_store(self_slot, p_arg).unwrap();
                self.builder
                    .build_call(user_drop_fn, &[self_slot.into()], "")
                    .unwrap();
            }
        }

        // Iterative-drop fast path. When the struct's only walkable
        // field is a niche-optimized `Option[Self]` in tail position
        // (the linked-list shape: `shared struct Node { val: i64,
        // mut next: Option[Node] }`), specialize the body to a
        // while-loop with inline rc-dec on the chain pointer. The
        // recursive version emits one indirect call to drop_fn per
        // chain link via `emit_rc_dec`; the iterative version emits
        // one loop. Same correctness — only recursion → iteration.
        // Cuts ~99% of function-call overhead in the chain drop on
        // the kata #2 workload (50M dec_refs across all runs).
        let iterative_niche_field_idx = kinds.iter().enumerate().find_map(|(i, k)| match k {
            SharedFieldKind::OptionSharedNiche(inner_info) if inner_info.heap_type == heap_type => {
                let other_walkable = kinds.iter().enumerate().any(|(j, k2)| {
                    j != i && !matches!(k2, SharedFieldKind::None | SharedFieldKind::_Phantom(_))
                });
                if other_walkable {
                    None
                } else {
                    Some(i)
                }
            }
            _ => None,
        });
        // The iterative self-chain fast path drops each link inline
        // without a per-link `drop_fn` call, so it would skip the user
        // body on every link but the head. With a user drop present,
        // stay on the recursive path: each link's `emit_rc_dec`
        // dispatches back through this fn and fires the body. (Trades
        // the chain-drop perf win for correctness — only when the type
        // actually has a user `impl Drop`.)
        if let Some(niche_idx) = iterative_niche_field_idx.filter(|_| !has_user_drop) {
            self.emit_iterative_self_chain_drop(drop_fn, p_arg, heap_type, (niche_idx + 1) as u32);
            self.current_fn = saved_fn;
            if let Some(bb) = saved_bb {
                self.builder.position_at_end(bb);
            }
            return Some(drop_fn);
        }

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
                    self.emit_refcount_dec_by_type(inner_info.heap_type, inner);
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
                    self.emit_refcount_dec_by_type(inner_info.heap_type, inner);
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
                    self.emit_refcount_dec_by_type(inner_info.heap_type, inner);
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

    /// Iterative-drop body for self-referential linked-list-shaped
    /// shared structs (precondition checked at the call site:
    /// exactly one heap-owning field, which is a niche-optimized
    /// `Option[Self]`). Emits the LLVM IR equivalent of:
    ///
    /// ```text
    /// let mut p = p_arg;
    /// loop {
    ///     let next = load(p + niche_field_offset);
    ///     free(p);
    ///     if next == null { return; }
    ///     // inline rc-dec on next:
    ///     let rc = load(next + 0);
    ///     let new_rc = rc - 1;
    ///     store(next + 0, new_rc);
    ///     if new_rc != 0 { return; }
    ///     p = next;
    /// }
    /// ```
    ///
    /// Equivalent to the recursive `emit_rc_dec(heap_type, next)` walk
    /// the conventional body emits — the rc-dec semantics are inlined
    /// here so the next iteration's free + load happens in the same
    /// stack frame. Caller positions the builder at the drop_fn's
    /// `entry_bb`; this method emits the loop and `ret void`, then
    /// leaves the builder at the `end` block (positioned past the
    /// `ret` so callers don't add stray instructions to a terminated
    /// block).
    pub(super) fn emit_iterative_self_chain_drop(
        &self,
        drop_fn: FunctionValue<'ctx>,
        p_arg: PointerValue<'ctx>,
        heap_type: inkwell::types::StructType<'ctx>,
        niche_field_heap_idx: u32,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let entry_bb = self.builder.get_insert_block().unwrap();
        let loop_bb = self.context.append_basic_block(drop_fn, "iterdrop.loop");
        let dec_bb = self.context.append_basic_block(drop_fn, "iterdrop.dec");
        let continue_bb = self
            .context
            .append_basic_block(drop_fn, "iterdrop.continue");
        let end_bb = self.context.append_basic_block(drop_fn, "iterdrop.end");

        self.builder.build_unconditional_branch(loop_bb).unwrap();

        // ── loop_bb: load p's next ptr, free p, branch on null ──
        self.builder.position_at_end(loop_bb);
        let p = self.builder.build_phi(ptr_ty, "iterdrop.p").unwrap();
        let next_addr = self
            .builder
            .build_struct_gep(
                heap_type,
                p.as_basic_value().into_pointer_value(),
                niche_field_heap_idx,
                "iterdrop.next.addr",
            )
            .unwrap();
        let next_ptr = self
            .builder
            .build_load(ptr_ty, next_addr, "iterdrop.next")
            .unwrap()
            .into_pointer_value();
        self.builder
            .build_call(
                self.free_fn,
                &[p.as_basic_value().into_pointer_value().into()],
                "",
            )
            .unwrap();
        let next_is_null = self
            .builder
            .build_is_null(next_ptr, "iterdrop.next.is_null")
            .unwrap();
        self.builder
            .build_conditional_branch(next_is_null, end_bb, dec_bb)
            .unwrap();

        // ── dec_bb: inline rc-dec on next; branch on new_rc==0 ──
        self.builder.position_at_end(dec_bb);
        let rc_addr = self
            .builder
            .build_struct_gep(heap_type, next_ptr, 0, "iterdrop.rc.addr")
            .unwrap();
        let rc = self
            .builder
            .build_load(i64_t, rc_addr, "iterdrop.rc")
            .unwrap()
            .into_int_value();
        let new_rc = self
            .builder
            .build_int_sub(rc, i64_t.const_int(1, false), "iterdrop.new_rc")
            .unwrap();
        self.builder.build_store(rc_addr, new_rc).unwrap();
        let rc_zero = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                new_rc,
                i64_t.const_zero(),
                "iterdrop.rc.is_zero",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(rc_zero, continue_bb, end_bb)
            .unwrap();

        // ── continue_bb: br back to loop_bb with p = next_ptr ──
        self.builder.position_at_end(continue_bb);
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        // ── wire up phi node ──
        p.add_incoming(&[(&p_arg, entry_bb), (&next_ptr, continue_bb)]);

        // ── end_bb: ret void ──
        self.builder.position_at_end(end_bb);
        self.builder.build_return(None).unwrap();
    }

    /// Phase 6 line 17 slice 9d — hand-rolled bodies for
    /// `@TcpListener.drop` and `@TcpStream.drop`. These two stdlib
    /// types declare `impl Drop` in `runtime/stdlib/tcp.kara` (so the
    /// typechecker registers them in `drop_method_keys`), but the
    /// existing impl-method orchestration in `compile_program` only
    /// walks user `program.items` — stdlib impl bodies never reach the
    /// `compile_function` pass. Without this helper, Prereq.2's
    /// `emit_user_drop_wrapper` would fail to find the `@TcpStream.drop`
    /// LLVM symbol and silently skip wrapper emission, leaving
    /// `TcpStream` / `TcpListener` bindings to leak their fd at scope
    /// exit. The helper mirrors the always-emitted pattern from
    /// `karac_park_on_fd` (declarations.rs `synthesize_park_on_fd_layout`
    /// + `emit_park_on_fd_poll_body`): the LLVM function is declared
    ///   and bodied unconditionally whenever the stdlib registers the
    ///   impl, and the linker dead-strips when no caller exists.
    ///
    /// Body shape (for both types — `TcpListener` / `TcpStream` are
    /// structurally identical, single `i32 fd` field):
    /// ```text
    /// define internal void @<Type>.drop(ptr %self) {
    ///   %fd_ptr = getelementptr {i32}, ptr %self, i32 0, i32 0
    ///   %fd     = load i32, ptr %fd_ptr
    ///   %_      = call i32 @karac_runtime_tcp_close(i32 %fd)
    ///   ret void
    /// }
    /// ```
    ///
    /// Must run BEFORE `emit_user_drop_wrappers` so the wrapper synth's
    /// `module.get_function("<Type>.drop")` lookup succeeds.
    pub(super) fn emit_hardcoded_stdlib_drop_bodies(&mut self, program: &crate::ast::Program) {
        // `WebSocket` (slice 9e.1) shares the same single-i32-field
        // layout as `TcpListener` / `TcpStream`, so `emit_tcp_drop_body_for`
        // applies verbatim — the hand-rolled body extracts `self.fd`
        // and calls `karac_runtime_tcp_close(fd)`. When slice 9e.3
        // adds WebSocket-specific drop steps (e.g., sending a close
        // frame before close(2)), this loop will need to dispatch
        // to a WS-specific body emitter for that type.
        for type_name in ["TcpListener", "TcpStream", "WebSocket"] {
            if !program.drop_method_keys.contains_key(type_name) {
                continue;
            }
            self.emit_tcp_drop_body_for(type_name);
        }

        // Phase 6 line 236 slice 2: TLS drop bodies. `TlsStream` shares
        // `TcpStream`'s `{i32 fd}` layout so the TCP body emitter
        // applies verbatim; `TlsListener` needs a dedicated emitter
        // because its struct is `{i32 fd, ptr config}` and the drop
        // body has to call `_tls_config_free(self.config)` before
        // `_tls_close(self.fd)`.
        if program.drop_method_keys.contains_key("TlsStream") {
            self.emit_tls_stream_drop_body();
        }
        if program.drop_method_keys.contains_key("TlsListener") {
            self.emit_tls_listener_drop_body();
        }

        // Phase 6 line 218 slice 5: `TaskGroup.drop` hand-rolled body.
        // Loads `self.id` (i64), casts to `*KaracTaskGroupHandle`, calls
        // `karac_runtime_taskgroup_join_and_free(group_ptr)`. The
        // runtime helper blocks until every registered child reaches a
        // terminal state, then frees the group itself. Slice 1's stdlib
        // declares `impl Drop for TaskGroup` so `drop_method_keys`
        // contains `"TaskGroup"` when the prelude is in scope.
        if program.drop_method_keys.contains_key("TaskGroup") {
            self.emit_taskgroup_drop_body();
        }

        // `BoundedChannel.drop` hand-rolled body — loads `self.handle_id`
        // (i64), casts to `*KaracBoundedChannel`, calls
        // `karac_runtime_bounded_channel_drop` (free the queue + payloads).
        // Single-owner (no refcount); the user-Drop dispatch is move-aware,
        // so a moved-from `BoundedChannel` is not double-dropped. The stdlib
        // declares `impl Drop for BoundedChannel` so `drop_method_keys`
        // contains `"BoundedChannel"` when the prelude is in scope.
        if program.drop_method_keys.contains_key("BoundedChannel") {
            self.emit_bounded_channel_drop_body();
        }
    }

    /// Hand-roll `@BoundedChannel.drop(ptr) -> void`. Mirrors
    /// `emit_taskgroup_drop_body`: load the `{i64}` handle, `inttoptr`, free.
    fn emit_bounded_channel_drop_body(&mut self) {
        let fn_name = "BoundedChannel.drop";
        if self.module.get_function(fn_name).is_some() {
            return;
        }
        let free_fn = match self
            .module
            .get_function("karac_runtime_bounded_channel_drop")
        {
            Some(f) => f,
            None => return,
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();
        let struct_ty = self.context.struct_type(&[i64_ty.into()], false);

        let saved_bb = self.builder.get_insert_block();

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(fn_name, drop_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let self_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let handle_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 0, "handle_ptr")
            .unwrap();
        let handle = self
            .builder
            .build_load(i64_ty, handle_ptr, "handle")
            .unwrap()
            .into_int_value();
        let ch_ptr = self
            .builder
            .build_int_to_ptr(handle, ptr_ty, "ch_ptr")
            .unwrap();
        self.builder
            .build_call(free_fn, &[ch_ptr.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Hand-roll `@TaskGroup.drop(ptr) -> void`. Body:
    ///
    /// ```text
    /// %id_ptr = getelementptr {i64}, ptr %self, i32 0, i32 0
    /// %id     = load i64, ptr %id_ptr
    /// %g_ptr  = inttoptr i64 %id to ptr
    /// call void @karac_runtime_taskgroup_join_and_free(ptr %g_ptr)
    /// ret void
    /// ```
    fn emit_taskgroup_drop_body(&mut self) {
        let fn_name = "TaskGroup.drop";
        if self.module.get_function(fn_name).is_some() {
            return;
        }
        let join_fn = match self
            .module
            .get_function("karac_runtime_taskgroup_join_and_free")
        {
            Some(f) => f,
            None => return,
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();
        let struct_ty = self.context.struct_type(&[i64_ty.into()], false);

        let saved_bb = self.builder.get_insert_block();

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(fn_name, drop_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let self_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let id_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 0, "id_ptr")
            .unwrap();
        let id = self
            .builder
            .build_load(i64_ty, id_ptr, "id")
            .unwrap()
            .into_int_value();
        let group_ptr = self
            .builder
            .build_int_to_ptr(id, ptr_ty, "group_ptr")
            .unwrap();
        self.builder
            .build_call(join_fn, &[group_ptr.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Hand-roll a `@<type_name>.drop(ptr) -> void` LLVM function whose
    /// body calls `karac_runtime_tcp_close(self.fd)`. Used for both
    /// `TcpListener` and `TcpStream` (their LLVM struct layout is
    /// identical — a single `i32` field at offset 0). The struct type
    /// is constructed inline (matching the convention in `src/codegen/tcp.rs`
    /// where `lower_tcp_listener_bind` builds `context.struct_type(&[i32])`
    /// inline) rather than reading from `self.struct_types`, because
    /// stdlib structs aren't registered in `struct_types` — that map
    /// is populated by `declare_structs` walking `program.items`, and
    /// stdlib items live outside the user program.
    fn emit_tcp_drop_body_for(&mut self, type_name: &str) {
        let fn_name = format!("{}.drop", type_name);
        if self.module.get_function(&fn_name).is_some() {
            return;
        }
        let close_fn = match self.module.get_function("karac_runtime_tcp_close") {
            Some(f) => f,
            None => return,
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let void_ty = self.context.void_type();
        let struct_ty = self.context.struct_type(&[i32_ty.into()], false);

        let saved_bb = self.builder.get_insert_block();

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let self_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let fd_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 0, "fd_ptr")
            .unwrap();
        let fd = self
            .builder
            .build_load(i32_ty, fd_ptr, "fd")
            .unwrap()
            .into_int_value();
        self.builder.build_call(close_fn, &[fd.into()], "").unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Hand-roll `@TlsStream.drop(ptr) -> void`. Body:
    ///
    /// ```text
    /// %fd_ptr = getelementptr {i32}, ptr %self, i32 0, i32 0
    /// %fd     = load i32, ptr %fd_ptr
    /// call i32 @karac_runtime_tls_close(i32 %fd)
    /// ret void
    /// ```
    ///
    /// Routes through `_tls_close` (not `_tcp_close`) so the runtime
    /// removes the per-fd `TlsSession` entry from `SESSIONS` before
    /// closing the underlying TCP fd — without this the rustls
    /// `ServerConnection` allocation leaks until process exit.
    fn emit_tls_stream_drop_body(&mut self) {
        let fn_name = "TlsStream.drop";
        if self.module.get_function(fn_name).is_some() {
            return;
        }
        let close_fn = match self.module.get_function("karac_runtime_tls_close") {
            Some(f) => f,
            None => return,
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let void_ty = self.context.void_type();
        let struct_ty = self.context.struct_type(&[i32_ty.into()], false);

        let saved_bb = self.builder.get_insert_block();

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(fn_name, drop_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let self_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let fd_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 0, "fd_ptr")
            .unwrap();
        let fd = self
            .builder
            .build_load(i32_ty, fd_ptr, "fd")
            .unwrap()
            .into_int_value();
        self.builder.build_call(close_fn, &[fd.into()], "").unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Hand-roll `@TlsListener.drop(ptr) -> void`. Body:
    ///
    /// ```text
    /// %fd_ptr     = getelementptr {i32, ptr}, ptr %self, i32 0, i32 0
    /// %fd         = load i32, ptr %fd_ptr
    /// %config_ptr = getelementptr {i32, ptr}, ptr %self, i32 0, i32 1
    /// %config     = load ptr,  ptr %config_ptr
    /// call void @karac_runtime_tls_config_free(ptr %config)
    /// call i32  @karac_runtime_tls_close(i32 %fd)
    /// ret void
    /// ```
    ///
    /// Free-then-close order matters: closing the listener fd while
    /// the config is still live is fine (sessions opened from this
    /// listener carry independent `Arc<ServerConfig>` clones), but
    /// freeing the config before closing the listener means the
    /// final close-on-drop can't race a leftover accept attempt. v1
    /// listeners drop without outstanding accepts because the kara
    /// accept-loop owns the listener exclusively, so this ordering
    /// is correctness-by-construction rather than load-bearing —
    /// documenting it for the inevitable future where the listener
    /// is shared across tasks via a mutex.
    fn emit_tls_listener_drop_body(&mut self) {
        let fn_name = "TlsListener.drop";
        if self.module.get_function(fn_name).is_some() {
            return;
        }
        let free_fn = match self.module.get_function("karac_runtime_tls_config_free") {
            Some(f) => f,
            None => return,
        };
        let close_fn = match self.module.get_function("karac_runtime_tls_close") {
            Some(f) => f,
            None => return,
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let void_ty = self.context.void_type();
        let struct_ty = self
            .context
            .struct_type(&[i32_ty.into(), ptr_ty.into()], false);

        let saved_bb = self.builder.get_insert_block();

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(fn_name, drop_fn_ty, Some(Linkage::Internal));

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let self_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        let fd_field_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 0, "fd_ptr")
            .unwrap();
        let fd = self
            .builder
            .build_load(i32_ty, fd_field_ptr, "fd")
            .unwrap()
            .into_int_value();

        let config_field_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 1, "config_ptr")
            .unwrap();
        let config = self
            .builder
            .build_load(ptr_ty, config_field_ptr, "config")
            .unwrap()
            .into_pointer_value();

        self.builder
            .build_call(free_fn, &[config.into()], "")
            .unwrap();
        self.builder.build_call(close_fn, &[fd.into()], "").unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Prereq.2 of the user-`impl Drop` dispatch slice — synthesize the
    /// per-type drop-wrapper `karac_drop_<Type>` for each entry in
    /// `program.drop_method_keys`. The wrapper body:
    ///
    /// ```text
    /// fn karac_drop_T(self: *mut T) {
    ///   T.drop(self);                  // (a) user body
    ///   __karac_drop_struct_T(self);   // (b) field cleanup — only when
    ///                                  //     the struct has heap-owning
    ///                                  //     fields (`emit_struct_drop_
    ///                                  //     synthesis` returns Some)
    ///   ret void
    /// }
    /// ```
    ///
    /// Runs after the impl-method body pass in `compile_program` so the
    /// user-defined `Type.drop` LLVM symbol already exists when the
    /// wrapper is built. Each wrapper is independent — order across
    /// `drop_method_keys` entries does not matter at v1.
    ///
    /// No call sites are emitted by this slice — Prereq.3 lowers
    /// scope-exit drop calls to invocations of these wrappers via
    /// `module.get_function("karac_drop_<Type>")`.
    pub(super) fn emit_user_drop_wrappers(&mut self, program: &crate::ast::Program) {
        // Iterate in deterministic (sorted) order so the emitted IR is
        // stable across runs — eases IR-grep test debugging when failures
        // surface ordering-dependent symbols.
        let mut type_names: Vec<&String> = program.drop_method_keys.keys().collect();
        type_names.sort();
        for type_name in type_names {
            self.emit_user_drop_wrapper(type_name);
        }
    }

    /// Lazy, memoized per-type emitter for `karac_drop_<Type>`. Returns
    /// `None` only when the user `Type.drop` LLVM symbol is absent —
    /// shouldn't happen in normal pipelines (the typechecker only
    /// records entries in `drop_method_keys` for impl blocks that
    /// reached `env.add_impl`, and the impl-method compile pass emits
    /// `Type.drop` for every such block).
    fn emit_user_drop_wrapper(&mut self, type_name: &str) -> Option<FunctionValue<'ctx>> {
        if let Some(f) = self.user_drop_wrapper_fns.get(type_name) {
            return Some(*f);
        }
        let user_drop_fn = self.module.get_function(&format!("{type_name}.drop"))?;

        let fn_name = format!("karac_drop_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.user_drop_wrapper_fns.insert(type_name.to_string(), f);
            return Some(f);
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let void_ty = self.context.void_type();

        let saved_bb = self.builder.get_insert_block();

        let wrapper_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let wrapper = self
            .module
            .add_function(&fn_name, wrapper_ty, Some(Linkage::Internal));
        self.user_drop_wrapper_fns
            .insert(type_name.to_string(), wrapper);

        let entry_bb = self.context.append_basic_block(wrapper, "entry");
        self.builder.position_at_end(entry_bb);
        let self_ptr = wrapper.get_nth_param(0).unwrap().into_pointer_value();

        // (a) Invoke the user-defined drop body. Signature is
        // `fn drop(mut ref self)` so the LLVM symbol takes a single
        // pointer arg, matching our wrapper's self pointer.
        self.builder
            .build_call(user_drop_fn, &[self_ptr.into()], "")
            .unwrap();

        // (b) Hand off to the existing per-struct field-cleanup
        // synthesizer for heap-owning fields. Returns `None` for structs
        // with no heap-bearing fields (primitive-only) — skip the call
        // in that case since there's nothing to free.
        if let Some(field_drop_fn) = self.emit_struct_drop_synthesis(type_name) {
            self.builder
                .build_call(field_drop_fn, &[self_ptr.into()], "")
                .unwrap();
        }

        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(wrapper)
    }
}
