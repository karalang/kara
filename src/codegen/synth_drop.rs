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
use inkwell::types::StructType;
use inkwell::values::{FunctionValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use crate::ast::{GenericArg, Item, TypeExpr, TypeKind, VariantKind};

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

        // Per-variant field TypeExprs — needed to recover the struct name for a
        // `NestedStruct` payload (B-2026-06-13-13). Built once; small N.
        let variant_field_tes: Vec<(String, Vec<TypeExpr>)> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .map(|(_tag, name, tes)| (name, tes))
            .collect();

        // Per-variant cleanup BBs. For each heap-bearing payload field:
        //   * `VecOrString` — reload (data, len, cap) and free the data ptr
        //     when cap > 0 (outer buffer only; see `EnumDropKind`);
        //   * `NestedStruct` — call the nested struct's own
        //     `__karac_drop_struct_<S>` on the field's inline word region,
        //     which recursively frees its Vec/String/enum/Map heap
        //     (B-2026-06-13-13, mirroring the struct-drop `NestedStruct` arm).
        for (variant_name, _tag, bb) in &case_bbs {
            self.builder.position_at_end(*bb);
            if let (Some(kinds), Some(offsets)) = (
                layout.field_drop_kinds.get(variant_name),
                layout.field_word_offsets.get(variant_name),
            ) {
                for (fi, (kind, (start_word, _num_words))) in
                    kinds.iter().zip(offsets.iter()).enumerate()
                {
                    match kind {
                        EnumDropKind::None => {}
                        EnumDropKind::VecOrString => {
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
                            // SSO forward-prep (see `sso.rs`): owned-heap ⇔
                            // signed `cap > 0`; inline/static skip the free.
                            let is_heap = self.sso_string_is_owned_heap(cap_val);
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
                            // Recycling-aware release; hint sized by the
                            // payload field type (phase-10 line 282) so a
                            // mid-size `Vec[T]` enum payload is parked.
                            let payload_hint_elem_size = variant_field_tes
                                .iter()
                                .find(|(n, _)| n == variant_name)
                                .and_then(|(_, tes)| tes.get(fi))
                                .map(|fte| self.vec_field_free_hint_elem_size(fte))
                                .unwrap_or(1);
                            self.emit_free_buf_call(data_ptr, cap_val, payload_hint_elem_size);
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
                        EnumDropKind::NestedStruct => {
                            // The inline struct payload starts at word
                            // `start_word`; its first LLVM field index is
                            // `start_word + 1` (tag is field 0). Pass that
                            // word-region pointer to the struct's drop fn —
                            // its fields are 8-byte words at the same offsets
                            // the enum payload uses, so the layouts coincide.
                            let struct_name = variant_field_tes
                                .iter()
                                .find(|(n, _)| n == variant_name)
                                .and_then(|(_, tes)| tes.get(fi))
                                .and_then(|te| match &te.kind {
                                    crate::ast::TypeKind::Path(p) => p.segments.first().cloned(),
                                    _ => None,
                                });
                            if let Some(sname) = struct_name {
                                let field_idx = (*start_word + 1) as u32;
                                let field_ptr = self
                                    .builder
                                    .build_struct_gep(
                                        layout.llvm_type,
                                        p_arg,
                                        field_idx,
                                        "drop.nstruct.p",
                                    )
                                    .unwrap();
                                // B-2026-06-14-28 — the value-path struct drop
                                // (`__karac_drop_struct_<S>`) frees Vec/String/
                                // enum/Map buffers but has NO shared-field arm
                                // (a struct's `shared` fields are rc-dec'd by
                                // the let/param cleanup actions when it's a
                                // local, not by its drop fn). As a shared-enum
                                // payload (`Add(BinOp)`) the struct has no such
                                // binding, so its inline RC children leak. Walk
                                // them here and rc-dec each — the box has hit
                                // refcount 0 (uniquely owned), so this is the
                                // sole dec of its one ref to each child.
                                //
                                // VALUE-drop path: the walker must NOT free the
                                // struct's Vec/String buffers (`owns_buffer_free=
                                // false`) — `__karac_drop_struct_<S>` does that.
                                // It only rc-dec's the inline `shared` children
                                // and drains `Vec[shared]` element boxes the
                                // struct drop leaves untouched. B-2026-06-14-34:
                                // the walker MUST run BEFORE the struct drop —
                                // the element drain reads each element slot out
                                // of the Vec's `data` buffer, which the struct
                                // drop frees; running the struct drop first would
                                // leave the drain reading freed memory (SEGV /
                                // use-after-free).
                                self.emit_nested_struct_shared_rc_decs(
                                    field_ptr, &sname, drop_fn, false,
                                );
                                // Memoized; saves/restores the builder block,
                                // so we resume in this drop fn's BB. `None`
                                // when the nested struct needs no drop.
                                if let Some(struct_drop_fn) =
                                    self.emit_struct_drop_synthesis(&sname)
                                {
                                    self.builder
                                        .build_call(struct_drop_fn, &[field_ptr.into()], "")
                                        .unwrap();
                                }
                            }
                        }
                        EnumDropKind::MapOrSet => {
                            // B-2026-07-23-11 — a `Map`/`Set`(-family) payload is
                            // a single heap-handle word at `start_word` (LLVM
                            // field `start_word + 1`, tag is field 0). Load the
                            // handle and free it via `karac_map_free_with_drop_vec`
                            // — the same runtime entrypoint the tuple/struct Map
                            // drop uses. The `(drop_key, drop_val)` flags come
                            // from the field's K/V types (`map_drop_flags`) so a
                            // heap-K/V map also releases its per-entry buffers. A
                            // moved-out payload has this word zeroed to null
                            // (`zero_enum_payload_caps`); the runtime null-guards,
                            // so the free is then a safe no-op.
                            let handle_idx = (*start_word + 1) as u32;
                            let handle_ptr = self
                                .builder
                                .build_struct_gep(
                                    layout.llvm_type,
                                    p_arg,
                                    handle_idx,
                                    "drop.map.handle.p",
                                )
                                .unwrap();
                            let handle = self
                                .builder
                                .build_load(ptr_ty, handle_ptr, "drop.map.handle")
                                .unwrap()
                                .into_pointer_value();
                            let (dk, dv) = variant_field_tes
                                .iter()
                                .find(|(n, _)| n == variant_name)
                                .and_then(|(_, tes)| tes.get(fi))
                                .map(|te| self.map_drop_flags(te))
                                .unwrap_or((0, 0));
                            let i32_t = self.context.i32_type();
                            self.builder
                                .build_call(
                                    self.karac_map_free_with_drop_vec_fn,
                                    &[
                                        handle.into(),
                                        i32_t.const_int(dk, false).into(),
                                        i32_t.const_int(dv, false).into(),
                                    ],
                                    "",
                                )
                                .unwrap();
                        }
                    }
                }
            }
            // Reference the vec_ty so the unused-binding lint stays quiet on
            // builds whose variants never enter the VecOrString arm.
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

    /// B-2026-06-14-28 — rc-dec every inline `shared` (RC-pointer) field of a
    /// plain (non-shared) struct stored at `struct_ptr`. Used by the
    /// shared-enum-box RC drop walker for a struct-wrapped recursive payload
    /// (`Add(BinOp)` + `struct BinOp { left: Expr, right: Expr }`, `Expr`
    /// shared — the AST-port operand-wrapper convention). The value-path
    /// `__karac_drop_struct_<S>` frees Vec/String/enum/Map buffers but leaves
    /// shared fields untouched (a local binding's shared fields are dec'd by
    /// its let/param cleanup actions, not its drop fn), so as an enum payload
    /// — which has no such binding — the inline RC children would leak.
    ///
    /// Per shared / `Option[shared T]` field: GEP into the inline struct,
    /// load the RC pointer, null-check, then dec via the field's shared heap
    /// layout. Recurses through nested non-shared struct fields so a deeper
    /// wrapper (`struct Outer { mid: Mid }`, `Mid { e: Expr }`) is reached.
    /// The caller has already established the box is uniquely owned (refcount
    /// hit 0), so each dec is the sole release of the box's one ref.
    /// Force-synthesize the recursive `__karac_rc_drop_<T>` for any shared `T`
    /// reachable from `te` — either `te` itself being a `shared T`, or the inner
    /// of an `Option[shared T]` / `Result[shared T, _]` / `Vec[shared T]`. So a
    /// later `emit_refcount_dec_by_type` dispatches to it (recursing into the
    /// box's children) instead of inline-`free`ing the box and stranding them.
    /// No-op for a non-shared type. Memoized, so safe while the same drop fn is
    /// mid-synthesis.
    fn force_synth_shared_rc_drop_for_type_expr(&mut self, te: &TypeExpr) {
        let TypeKind::Path(p) = &te.kind else {
            return;
        };
        if let Some(head) = p.segments.last() {
            if self.shared_types.contains_key(head.as_str()) {
                self.force_synth_shared_rc_drop_by_name(&head.clone());
                return;
            }
        }
        // Generic inner (`Option[shared]` / `Result[shared, _]` / `Vec[shared]`).
        if let Some(args) = p.generic_args.as_ref() {
            for a in args {
                if let crate::ast::GenericArg::Type(t) = a {
                    if let TypeKind::Path(ip) = &t.kind {
                        if let Some(n) = ip.segments.last() {
                            if self.shared_types.contains_key(n.as_str()) {
                                self.force_synth_shared_rc_drop_by_name(&n.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    fn force_synth_shared_rc_drop_by_name(&mut self, name: &str) {
        if let Some(info) = self.shared_types.get(name).cloned() {
            if info.is_enum {
                let _ = self.emit_shared_enum_rc_drop_fn(name);
            } else {
                let _ = self.emit_shared_struct_rc_drop_fn(name);
            }
        }
    }

    pub(super) fn emit_nested_struct_shared_rc_decs(
        &mut self,
        struct_ptr: PointerValue<'ctx>,
        struct_name: &str,
        drop_fn: FunctionValue<'ctx>,
        owns_buffer_free: bool,
    ) {
        self.emit_nested_struct_shared_rc_decs_ex(
            struct_ptr,
            struct_name,
            drop_fn,
            owns_buffer_free,
            Some(owns_buffer_free),
        );
    }

    /// As [`Self::emit_nested_struct_shared_rc_decs`], but `nested_buffer_free`
    /// controls how a nested non-shared struct FIELD is recursed into:
    ///   * `None` — do NOT recurse (skip the nested struct entirely).
    ///   * `Some(b)` — recurse with `owns_buffer_free = b` (and the same
    ///     `Some(b)` for deeper levels).
    ///
    /// The shared-enum-box BOXED-struct payload drop passes `Some(false)`: a
    /// nested heap struct field of a boxed payload (`IfNode.then_block: Block`)
    /// may be MOVED OUT by the match binding (`let tb = nd.then_block`), whose
    /// own value-drop (`__karac_drop_struct_<S>`) frees that struct's Vec/String
    /// BUFFERS — so re-freeing those through the box's rc-drop walk is a
    /// double-free. But the value-drop does NOT rc-dec the moved struct's
    /// `shared` / `Option[shared]` CHILDREN (those are RC-machinery), so the box
    /// rc-drop must. `Some(false)` recurses to rc-dec exactly those children
    /// while leaving the buffers to the move-out owner: no double-free, and the
    /// moved-out struct's RC children are reclaimed (the leak the boxed-payload
    /// path otherwise stranded). All other callers preserve the prior behavior
    /// (`Some(owns_buffer_free)`): the in-place inline payload is solely
    /// box-owned, no move-out, so buffers and children both flow to this walker.
    pub(super) fn emit_nested_struct_shared_rc_decs_ex(
        &mut self,
        struct_ptr: PointerValue<'ctx>,
        struct_name: &str,
        drop_fn: FunctionValue<'ctx>,
        owns_buffer_free: bool,
        nested_buffer_free: Option<bool>,
    ) {
        // B-2026-06-14-34 — two independent fixes over B-31, both required:
        //
        // (1) `drop_fn` is passed in EXPLICITLY (it used to read
        //     `self.current_fn`), and `self.current_fn` is scoped to it for the
        //     walker body below. `self.current_fn` was the WRONG block-append
        //     target from the VALUE-drop path (`emit_enum_drop_switch`): there
        //     it still points at the OUTER fn that triggered drop synthesis
        //     (e.g. `use_stmt`/`main`), so this walker's `nstr.*` blocks landed
        //     in that outer fn while the surrounding switch's `br exit`
        //     referenced the synthesized drop fn — a cross-function basic-block
        //     reference that failed module verification (the self-host lexer's
        //     memoization order is the trigger; the `--test codegen` cases had
        //     it masked because `compile_to_object` skips verification and the
        //     optimizer DCE'd the orphan blocks). Threading the target through
        //     the arg + scoping `current_fn` only for this leaf walker (not the
        //     whole value-drop body — that version double-freed the value-Vec
        //     cases) fixes it without disturbing the value-drop body's own
        //     emitters (alloca placement / cleanup tracking) that legitimately
        //     read `self.current_fn`.
        //
        // (2) `owns_buffer_free` gates the `Vec[T]` field's `free(data)`. The
        //     B-31 Vec arm unconditionally freed the buffer. That is correct in
        //     the RC-DROP path (the shared-enum box owns the inline Vec buffer;
        //     nothing else frees it), but a DOUBLE-FREE in the VALUE-drop path,
        //     where the struct's own `__karac_drop_struct_<S>` (invoked at the
        //     call site BEFORE this walker) already freed the buffer. The
        //     double-free was latent on B-31 only because the cross-function
        //     blocks above were DCE'd before they could run; fixing (1) made the
        //     free reachable and surfaced it. So the buffer `free(data)` runs
        //     only when this walker owns it.
        //
        //     B-2026-07-10-4 — the per-element rc-dec drain is now ALSO gated on
        //     `owns_buffer_free`. When B-31/B-34 were written, `__karac_drop_struct_<S>`'s
        //     VecOrString arm froze the buffer but did NOT drain elements, so the
        //     walker's drain was the ONLY drain and had to run in the value-drop
        //     companion path too. Since #35 that arm drains elements itself (via
        //     the SAME `vec_elem_agg_drop_for_type_expr` fn), so in the companion
        //     path (`owns_buffer_free == false`) draining here is a redundant
        //     SECOND drain — a double-free of every element's shared/heap
        //     children (`__karac_vec_elem_full_drop_FnDefNode` over `params` /
        //     `attributes` / nested `Block.stmts`). The walker's drain now runs
        //     ONLY in the shared-enum-box path (`owns_buffer_free == true`), where
        //     no `__karac_drop_struct_<S>` runs and the walker is the sole drop.
        let Some(&st) = self.struct_types.get(struct_name) else {
            return;
        };
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return;
        };
        // Force-synthesize the recursive RC drop fn for every shared type
        // referenced by a field (a direct `shared T`, or the inner of an
        // `Option[shared T]` / `Vec[shared T]`) BEFORE emitting any rc-dec
        // below. Without this, an `emit_refcount_dec_by_type` emitted before
        // `__karac_rc_drop_<T>` is registered in `rc_drop_fns` falls to a plain
        // inline `free` of the box that STRANDS its heap children (the box's own
        // String/Vec/shared payload) — the inner `LitNode.name` String of a
        // `Vec[Arg]` element's `value: Expr` leaked under Linux LSan exactly
        // this way (the self-host parser's `Call(CallExpr { args })` shape).
        // This is the identical pre-synth `emit_vec_elem_rc_dec_fn` (B-28) does;
        // it must run for THIS walker too because the walker rc-dec's shared
        // fields directly. Done up front (not inline per arm) so the
        // builder-repositioning the synthesizers do happens before the walker
        // body is emitted. Memoized at fn entry, so re-entrant during the
        // surrounding drop-fn synthesis.
        for fte in &ftes {
            self.force_synth_shared_rc_drop_for_type_expr(fte);
        }
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_t = self.context.i64_type();
        // Scope `current_fn` to `drop_fn` for the DURATION of this walker only:
        // its leaf sub-emitters (`emit_refcount_dec_by_type` →
        // `rc_is_zero`/`rc_free`/`rc_done` blocks, `vec_elem_agg_drop_for_type_expr`,
        // and `create_entry_alloca` for the Vec-drain counter) all append to
        // `self.current_fn`. The block-append targets passed `drop_fn` directly,
        // but these sub-emitters key off `self.current_fn`; without this they'd
        // land in the OUTER fn (the value-drop call site) → cross-function refs.
        // Restored on exit so the surrounding value-drop body's own emitters
        // (which legitimately read `self.current_fn` for alloca placement /
        // cleanup tracking) are untouched — that whole-body version is what
        // double-freed the value-Vec cases.
        let saved_fn = self.current_fn;
        self.current_fn = Some(drop_fn);
        for (idx, fte) in ftes.iter().enumerate() {
            // Direct `shared T` field — the recursive RC edge.
            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(fte) {
                let Ok(field_ptr) =
                    self.builder
                        .build_struct_gep(st, struct_ptr, idx as u32, "nstr.sh.p")
                else {
                    continue;
                };
                let inner = self
                    .builder
                    .build_load(ptr_ty, field_ptr, "nstr.sh.ptr")
                    .unwrap()
                    .into_pointer_value();
                let is_null = self.builder.build_is_null(inner, "nstr.sh.isnull").unwrap();
                let do_bb = self.context.append_basic_block(drop_fn, "nstr.sh.do");
                let skip_bb = self.context.append_basic_block(drop_fn, "nstr.sh.skip");
                self.builder
                    .build_conditional_branch(is_null, skip_bb, do_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                self.emit_refcount_dec_by_type(heap_ty, inner);
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                continue;
            }
            // `Option[shared T]` field — dec the inner when Some. The inline
            // field is the 4-i64 Option enum (tag at w0, payload at w1); a
            // niche-collapsed single-ptr Option does not occur for a struct
            // field stored inline in an enum payload here. Read the tag, and
            // on Some treat w1 as the RC pointer.
            if let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(fte) {
                let Ok(opt_field_ptr) =
                    self.builder
                        .build_struct_gep(st, struct_ptr, idx as u32, "nstr.opt.p")
                else {
                    continue;
                };
                let option_ty = self
                    .enum_layouts
                    .get("Option")
                    .map(|l| l.llvm_type)
                    .unwrap_or(st);
                let Ok(tag_ptr) =
                    self.builder
                        .build_struct_gep(option_ty, opt_field_ptr, 0, "nstr.opt.tag.p")
                else {
                    continue;
                };
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, "nstr.opt.tag")
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
                        "nstr.opt.issome",
                    )
                    .unwrap();
                let do_bb = self.context.append_basic_block(drop_fn, "nstr.opt.do");
                let skip_bb = self.context.append_basic_block(drop_fn, "nstr.opt.skip");
                self.builder
                    .build_conditional_branch(is_some, do_bb, skip_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                if let Ok(w0_ptr) =
                    self.builder
                        .build_struct_gep(option_ty, opt_field_ptr, 1, "nstr.opt.w0.p")
                {
                    let w0 = self
                        .builder
                        .build_load(i64_t, w0_ptr, "nstr.opt.w0")
                        .unwrap()
                        .into_int_value();
                    let inner = self
                        .builder
                        .build_int_to_ptr(w0, ptr_ty, "nstr.opt.inner")
                        .unwrap();
                    let inner_null = self
                        .builder
                        .build_is_null(inner, "nstr.opt.inner.isnull")
                        .unwrap();
                    let ido = self
                        .context
                        .append_basic_block(drop_fn, "nstr.opt.inner.do");
                    let iskip = self
                        .context
                        .append_basic_block(drop_fn, "nstr.opt.inner.skip");
                    self.builder
                        .build_conditional_branch(inner_null, iskip, ido)
                        .unwrap();
                    self.builder.position_at_end(ido);
                    self.emit_refcount_dec_by_type(inner_info.heap_type, inner);
                    self.builder.build_unconditional_branch(iskip).unwrap();
                    self.builder.position_at_end(iskip);
                }
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                continue;
            }
            // B-2026-07-11-39 — an `Option[<inline-heap>]` field whose inner is
            // NOT shared (`Option[String]`, `Option[Vec[T]]`, an inline
            // struct/enum payload). The `Option[shared T]` arm above rejects it
            // (`option_inner_shared_type_for_type_expr` requires a shared inner),
            // and the plain-`String` arm below rejects it too, so before this the
            // field fell through the loop and its `Some` payload leaked when the
            // box dropped (the recursive `shared enum` peer of the standalone
            // struct drop's `FieldDrop::OptionInline` arm, which already handles
            // it in the VALUE-drop path). `karac_drop_Option_<payload>`
            // (`emit_option_drop_fn`) reads the tag and frees the Some payload —
            // the String/Vec overlay or the boxed wide payload. Free here ONLY in
            // the RC-box path (`owns_buffer_free`): in the value-drop path the
            // struct's own `__karac_drop_struct_<S>` already ran it, so freeing
            // again would double-free. The call is unconditional (the drop fn is
            // itself tag-guarded, no-op on `None`); `emit_option_drop_fn` may
            // reposition the builder while synthesizing the payload's drop, so
            // capture and restore the current block before emitting the call.
            if owns_buffer_free
                && self.option_inner_shared_type_for_type_expr(fte).is_none()
                && self.te_owns_option_heap_payload(fte)
            {
                if let Some(payload_te) = Self::option_payload_te(fte) {
                    let cur_bb = self.builder.get_insert_block();
                    let opt_drop = self.emit_option_drop_fn(&payload_te);
                    if let Some(bb) = cur_bb {
                        self.builder.position_at_end(bb);
                    }
                    if let Some(opt_drop) = opt_drop {
                        if let Ok(field_ptr) = self.builder.build_struct_gep(
                            st,
                            struct_ptr,
                            idx as u32,
                            "nstr.optheap.p",
                        ) {
                            self.builder
                                .build_call(opt_drop, &[field_ptr.into()], "")
                                .unwrap();
                        }
                        continue;
                    }
                }
            }
            // A direct `String` field (`{ptr,len,cap}`) — the String peer of
            // the `Vec[T]` arm below. A `shared enum` variant whose plain-struct
            // payload owns a String (`Ident(IdentExpr { name: String, span })`,
            // `Str(StrLit { value: String, span })` — the parser's AST nodes)
            // is laid out INLINE in the box's payload words, so the box owns
            // the String buffer and nothing else frees it; without this arm the
            // name/value String leaked when the box dropped (the recursive peer
            // of the single-level fix — surfaced once a node became a CHILD of
            // a Binary box, freed via the parent box's rc-drop rather than the
            // top-level match path). `vec_inner_type_expr` returns `None` for a
            // String, so the Vec arm never reached it. Free the buffer when
            // this walker owns it (`owns_buffer_free`, the RC-drop path); in the
            // value-drop path the struct's own `__karac_drop_struct_<S>` frees
            // it, so freeing here would double-free.
            if owns_buffer_free && self.is_string_type_expr(fte) {
                let Ok(field_ptr) =
                    self.builder
                        .build_struct_gep(st, struct_ptr, idx as u32, "nstr.str.p")
                else {
                    continue;
                };
                let vec_ty = self.vec_struct_type();
                let cap_p = self
                    .builder
                    .build_struct_gep(vec_ty, field_ptr, 2, "nstr.str.cap.p")
                    .unwrap();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_p, "nstr.str.cap")
                    .unwrap()
                    .into_int_value();
                let is_heap = self
                    .builder
                    .build_int_compare(
                        IntPredicate::UGT,
                        cap,
                        i64_t.const_zero(),
                        "nstr.str.is_heap",
                    )
                    .unwrap();
                let free_bb = self.context.append_basic_block(drop_fn, "nstr.str.free");
                let done_bb = self.context.append_basic_block(drop_fn, "nstr.str.done");
                self.builder
                    .build_conditional_branch(is_heap, free_bb, done_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, field_ptr, 0, "nstr.str.data.pp")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "nstr.str.data")
                    .unwrap()
                    .into_pointer_value();
                // Recycling-aware release; String — cap IS the byte count.
                self.emit_free_buf_call(data, cap, 1);
                self.builder.build_unconditional_branch(done_bb).unwrap();
                self.builder.position_at_end(done_bb);
                continue;
            }
            // B-2026-06-14-31 — a `Vec[T]` field (`struct CallExpr { callee:
            // Expr, args: Vec[Expr] }`, the AST-port `Call(CallExpr)` shape).
            // The struct is laid out INLINE in the enum payload words, so the
            // Vec's `{data, len, cap}` buffer + its per-element boxes are NOT
            // touched by the enum box free — and `emit_nested_struct_shared_rc_decs`
            // previously had no Vec arm at all, leaking the buffer (the
            // 80-byte direct alloc) and every element box. Emit the buffer
            // drain: loop `0..len` calling the element's per-slot drop, then
            // `free(data)` when `cap > 0`. For a `Vec[shared]` element,
            // `vec_elem_agg_drop_for_type_expr` returns the rc-dec helper
            // (`__karac_vec_elem_rc_dec_<T>`), so each element box is rc-dec'd
            // (and recurses into its children); for a Vec of value aggregates
            // it returns the value-drop fn. A `Vec[primitive]` element returns
            // `None` — then only the buffer is freed.
            if let Some(elem_te) = crate::codegen::helpers::vec_inner_type_expr(fte) {
                let Ok(field_ptr) =
                    self.builder
                        .build_struct_gep(st, struct_ptr, idx as u32, "nstr.vec.p")
                else {
                    continue;
                };
                let vec_ty = self.vec_struct_type();
                // Recurse FIRST — the sub-emitter may switch the builder's
                // insert block; capture the per-element drop fn before we open
                // the loop blocks in THIS function.
                let elem_drop = self.vec_elem_agg_drop_for_type_expr(&elem_te);
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, field_ptr, 0, "nstr.vec.data.pp")
                    .unwrap();
                let len_p = self
                    .builder
                    .build_struct_gep(vec_ty, field_ptr, 1, "nstr.vec.len.p")
                    .unwrap();
                let cap_p = self
                    .builder
                    .build_struct_gep(vec_ty, field_ptr, 2, "nstr.vec.cap.p")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, "nstr.vec.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_p, "nstr.vec.len")
                    .unwrap()
                    .into_int_value();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_p, "nstr.vec.cap")
                    .unwrap()
                    .into_int_value();
                // Per-element drain loop — ONLY when this walker OWNS the buffer
                // (the shared-enum-box RC-drop path, `owns_buffer_free == true`,
                // where NO `__karac_drop_struct_<S>` runs and the walker is the
                // sole drop for the inline/boxed payload). In the value-drop
                // COMPANION path (`owns_buffer_free == false`) the struct's own
                // `__karac_drop_struct_<S>` VecOrString arm ALREADY drains every
                // element via the SAME `vec_elem_agg_drop_for_type_expr` fn (#35
                // made that arm element-draining — for `Vec[shared]` it rc-dec's
                // each box, for `Vec[struct/enum-with-shared]` it runs the
                // combined/value element drop). Re-draining here double-frees
                // each element's shared/heap children — B-2026-07-10-4:
                // `__karac_vec_elem_full_drop_FnDefNode` double-dropped
                // `params`/`attributes` and, via the nested-struct recursion into
                // `body: Block`, `Block.stmts`. The buffer free below is gated on
                // the same flag for the same reason (B-2026-06-14-34); the drain
                // and the free are now one disjoint unit owned by pass 1 in the
                // companion path. (`elem_drop` is still resolved above so its
                // synthesis side effect — registering the element drop fn — runs
                // regardless; the memoized fn is reused by pass 1's drain.)
                if let Some(elem_drop) = elem_drop.filter(|_| owns_buffer_free) {
                    let elem_ty = self.llvm_type_for_type_expr(&elem_te);
                    let cond_bb = self.context.append_basic_block(drop_fn, "nstr.vec.cond");
                    let body_bb = self.context.append_basic_block(drop_fn, "nstr.vec.body");
                    let incr_bb = self.context.append_basic_block(drop_fn, "nstr.vec.incr");
                    let after_bb = self.context.append_basic_block(drop_fn, "nstr.vec.after");
                    let counter = self.create_entry_alloca(drop_fn, "nstr.vec.i", i64_t.into());
                    self.builder
                        .build_store(counter, i64_t.const_zero())
                        .unwrap();
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                    self.builder.position_at_end(cond_bb);
                    let cur = self
                        .builder
                        .build_load(i64_t, counter, "nstr.vec.i.cur")
                        .unwrap()
                        .into_int_value();
                    let lt = self
                        .builder
                        .build_int_compare(IntPredicate::ULT, cur, len, "nstr.vec.i.lt")
                        .unwrap();
                    self.builder
                        .build_conditional_branch(lt, body_bb, after_bb)
                        .unwrap();
                    self.builder.position_at_end(body_bb);
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(elem_ty, data, &[cur], "nstr.vec.elem.p")
                            .unwrap()
                    };
                    self.builder
                        .build_call(elem_drop, &[elem_ptr.into()], "")
                        .unwrap();
                    self.builder.build_unconditional_branch(incr_bb).unwrap();
                    self.builder.position_at_end(incr_bb);
                    let next = self
                        .builder
                        .build_int_add(cur, i64_t.const_int(1, false), "nstr.vec.i.next")
                        .unwrap();
                    self.builder.build_store(counter, next).unwrap();
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                    self.builder.position_at_end(after_bb);
                }
                // Free the buffer when heap-allocated (cap > 0; a static /
                // empty Vec has cap == 0 and shares no buffer) — ONLY when this
                // walker owns the buffer free (the RC-drop path). In the
                // value-drop path the struct's own `__karac_drop_struct_<S>`
                // already freed it, so freeing here is a double-free (B-34); we
                // only did the per-element rc-dec drain above.
                if owns_buffer_free {
                    let is_heap = self
                        .builder
                        .build_int_compare(
                            IntPredicate::UGT,
                            cap,
                            i64_t.const_zero(),
                            "nstr.vec.is_heap",
                        )
                        .unwrap();
                    let free_bb = self.context.append_basic_block(drop_fn, "nstr.vec.free");
                    let done_bb = self.context.append_basic_block(drop_fn, "nstr.vec.done");
                    self.builder
                        .build_conditional_branch(is_heap, free_bb, done_bb)
                        .unwrap();
                    self.builder.position_at_end(free_bb);
                    // Recycling-aware release; hint = cap × sizeof(elem)
                    // (phase-10 line 282) so a mid-size `Vec[T]` field parks.
                    let vec_elem_size = self
                        .target_data
                        .as_ref()
                        .map(|td| td.get_abi_size(&self.llvm_type_for_type_expr(&elem_te)))
                        .unwrap_or(1);
                    self.emit_free_buf_call(data, cap, vec_elem_size);
                    self.builder.build_unconditional_branch(done_bb).unwrap();
                    self.builder.position_at_end(done_bb);
                }
                let _ = cap;
                continue;
            }
            // Nested non-shared user struct → recurse into it (unless
            // `nested_buffer_free` is `None`). The recursion's buffer-ownership is
            // the carried `nbf`, not this level's `owns_buffer_free`: the
            // boxed-payload move-out case frees this level's direct buffers but
            // must NOT free a moved-out nested struct's buffers (its move-out owner
            // does) — only rc-dec its RC children. See the `_ex` doc.
            if let Some(nbf) = nested_buffer_free {
                if let TypeKind::Path(p) = &fte.kind {
                    if let Some(head) = p.segments.first() {
                        if self.struct_types.contains_key(head.as_str())
                            && !self.shared_types.contains_key(head.as_str())
                        {
                            if let Ok(field_ptr) = self.builder.build_struct_gep(
                                st,
                                struct_ptr,
                                idx as u32,
                                "nstr.nest.p",
                            ) {
                                let name = head.clone();
                                self.emit_nested_struct_shared_rc_decs_ex(
                                    field_ptr,
                                    &name,
                                    drop_fn,
                                    nbf,
                                    Some(nbf),
                                );
                            }
                        }
                    }
                }
            }
        }
        self.current_fn = saved_fn;
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
    /// True iff a `Vec[elem]` FIELD's `elem` owns heap that the outer buffer
    /// free misses AND `vec_elem_agg_drop_for_type_expr` does not already cover
    /// it — i.e. a *direct* `String`/`str`, a `Map`/`Set`, or a nested
    /// `Vec`/`VecDeque` element. For these the struct-drop field drain must call
    /// `emit_drop_fn_for_type_expr` per element (the top-level `FreeVecBuffer`
    /// drain has bespoke inline branches for exactly this set; the struct-field
    /// drain reuses the recursive drop family instead). The set is EXACTLY the
    /// element shapes `emit_vecstr_defensive_copy`'s element-deep mode can
    /// duplicate, so the by-value-param entry-copy (`param_own.rs`) stays
    /// symmetric with this deeper drop (copy-depth == drop-depth, the invariant
    /// in `param_own.rs`'s module doc). A heap-bearing TUPLE element is
    /// deliberately EXCLUDED — the entry-copy can't deepen it, so deepening its
    /// drop alone would double-free a by-value-param-retained `Vec[(.., String)]`
    /// field; that element stays a tracked outer-only remainder. Named user
    /// struct/enum/shared/Option elements are NOT listed either —
    /// `vec_elem_agg_drop_for_type_expr` already returns their precise (possibly
    /// `None`, for a heapless one) drop. Scalars own no heap and stay `None`.
    pub(super) fn elem_te_needs_direct_recursive_drain(elem_te: &TypeExpr) -> bool {
        match &elem_te.kind {
            TypeKind::Path(p) => matches!(
                p.segments.first().map(String::as_str),
                Some("String" | "str" | "Vec" | "VecDeque" | "Map" | "HashMap" | "Set" | "HashSet")
            ),
            _ => false,
        }
    }

    pub(super) fn emit_struct_drop_synthesis(
        &mut self,
        struct_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        self.emit_struct_drop_synthesis_impl(struct_name, None)
    }

    /// B-2026-07-11-35 (push leg) — per-MONOMORPH struct-drop synthesis. A
    /// generic container `S[T] { items: Vec[T] }` synthesizes ONE drop fn per
    /// struct NAME under `emit_struct_drop_synthesis`, resolving the `Vec[T]`
    /// field's element from the DECLARED bare `T` — so `T`'s heap (a `Vec[String]`
    /// field's char buffers) is never drained and every unconsumed element leaks
    /// (9x under `asan_generic_assoc_fn_vec_field_no_leak` once the push deep-copies).
    /// The drop fn is also shared across `S[String]` / `S[i64]` (same name, same
    /// `{ptr,len,cap}` Vec LLVM type), so a name-shared drop CANNOT free
    /// instantiation-specific heap without corrupting the other (running a
    /// String-element drain over an `i64` Vec would `free` each i64 as a bogus
    /// `{ptr,len,cap}`). This variant threads the concrete `param -> arg` subst
    /// (built from the binding's recorded instantiation `S[String]`), which (a)
    /// mangles a distinct symbol per instantiation (`__karac_drop_struct_S$String`)
    /// and (b) resolves each `Vec[T]` field element to the concrete `String`
    /// before picking its per-element drop. Non-generic structs pass `None` and
    /// are byte-for-byte unchanged (empty subst → bare name, no resolution).
    pub(super) fn emit_struct_drop_synthesis_mono(
        &mut self,
        struct_name: &str,
        subst: &std::collections::HashMap<String, TypeExpr>,
    ) -> Option<FunctionValue<'ctx>> {
        if subst.is_empty() {
            return self.emit_struct_drop_synthesis_impl(struct_name, None);
        }
        self.emit_struct_drop_synthesis_impl(struct_name, Some(subst))
    }

    /// B-2026-07-15-11 — derive a NESTED struct field's own mono subst from its
    /// declared TypeExpr (`Outer { inner: Box[String] }` → `{T: String}` for
    /// `Box`), resolving the parent's active subst FIRST so a generic
    /// `Outer[U] { inner: Box[U] }` propagates `U` → the parent's concrete arg.
    /// Empty when the field type carries no generic args (a non-generic nested
    /// struct) — `emit_struct_drop_synthesis_mono` then falls back to the
    /// name-shared drop, byte-for-byte unchanged. Used to (a) classify and (b)
    /// emit `NestedStruct` fields through the per-monomorph nested drop that
    /// actually frees a bare-T Vec/String field, and (c) match the whole-parent
    /// move-suppression recursion (`zero_struct_move_caps`) so the added nested
    /// drop never double-frees a moved-out parent.
    pub(super) fn nested_struct_field_subst(
        &self,
        struct_name: &str,
        field_idx: usize,
        parent_subst: Option<&std::collections::HashMap<String, TypeExpr>>,
        nested_name: &str,
    ) -> std::collections::HashMap<String, TypeExpr> {
        let Some(fte) = self
            .struct_field_type_exprs
            .get(struct_name)
            .and_then(|v| v.get(field_idx))
        else {
            return std::collections::HashMap::new();
        };
        let resolved = match parent_subst {
            Some(s) => crate::codegen::helpers::subst_type_params_in_type_expr(fte, s),
            None => fte.clone(),
        };
        self.generic_struct_subst_from_inst(nested_name, &resolved)
    }

    /// Recursively mangle a concrete type arg into a drop-fn symbol suffix
    /// component — `String`, `i64`, `Vec_i64`, `Box_String`, `tup_i64_String`.
    /// Unlike `mangled_type_name` (head-only, so `Vec[i64]` and `Vec[String]`
    /// collide) this descends into generic args and tuple elements so two
    /// instantiations that differ only in a nested arg get distinct symbols.
    fn drop_mono_mangle_component(te: &TypeExpr) -> String {
        match &te.kind {
            TypeKind::Path(p) => {
                let head = p
                    .segments
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string());
                match &p.generic_args {
                    Some(args) if !args.is_empty() => {
                        let parts: Vec<String> = args
                            .iter()
                            .map(|a| match a {
                                GenericArg::Type(t) => Self::drop_mono_mangle_component(t),
                                _ => "c".to_string(),
                            })
                            .collect();
                        format!("{head}_{}", parts.join("_"))
                    }
                    _ => head,
                }
            }
            TypeKind::Tuple(elems) => {
                let parts: Vec<String> =
                    elems.iter().map(Self::drop_mono_mangle_component).collect();
                format!("tup_{}", parts.join("_"))
            }
            _ => "unknown".to_string(),
        }
    }

    fn emit_struct_drop_synthesis_impl(
        &mut self,
        struct_name: &str,
        subst: Option<&std::collections::HashMap<String, TypeExpr>>,
    ) -> Option<FunctionValue<'ctx>> {
        // Per-monomorph cache key + symbol suffix: for a generic struct with a
        // non-empty subst, append `$<concrete>` per generic param (in declared
        // order) so `S[String]` / `S[i64]` get distinct cached drop fns and LLVM
        // symbols. Non-generic (or `None`) → bare name, unchanged.
        let mono_suffix: Option<String> = subst.and_then(|subst| {
            let params = self.struct_generic_params.get(struct_name).cloned()?;
            let mut suf = String::new();
            for p in &params {
                if let Some(te) = subst.get(p) {
                    suf.push('$');
                    suf.push_str(&Self::drop_mono_mangle_component(te));
                }
            }
            if suf.is_empty() {
                None
            } else {
                Some(suf)
            }
        });
        let cache_key = match &mono_suffix {
            Some(s) => format!("{struct_name}{s}"),
            None => struct_name.to_string(),
        };
        // The active subst only drives field-element resolution when a real
        // mono suffix was produced (a generic struct with bound params); a
        // non-generic struct treats it as absent.
        let subst = mono_suffix.as_ref().and(subst);
        if let Some(f) = self.struct_drop_fns.get(&cache_key) {
            return Some(*f);
        }
        // Shared structs use the RC machinery; their cleanup is via
        // `track_rc_var` / `emit_refcount_dec`, not a synthesized
        // per-value drop fn.
        if self.shared_types.contains_key(struct_name) {
            return None;
        }
        // B-2026-07-15-24: under a real mono subst, GEP fields with the
        // PER-MONOMORPH layout, not the base erased `struct_types` layout. The
        // base type lowers a bare generic-param field to ONE i64 word; a
        // monomorph that binds that param to a wider heap type (Vec/String = a
        // 3-word `{ptr,len,cap}` triple) widens the field, shifting every
        // following field's offset. So a Map/Set/Vec/String/enum field placed
        // AFTER a bare-T heap field was GEP'd at the wrong (base) offset —
        // double-free / SIGSEGV at scope-exit drop (the read paths already use
        // the mono layout, so reads were correct and only the drop crashed).
        // Rebuilding `st` from each field's subst-resolved TypeExpr makes every
        // downstream field GEP and `get_field_type_at_index` here mono-correct
        // in one place. Falls back to the base type for a non-generic struct or
        // when the mono type can't be built.
        let st = subst
            .and_then(|s| self.mono_struct_type_from_subst(struct_name, s))
            .or_else(|| self.struct_types.get(struct_name).copied())?;
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
            ///
            /// NOTE: superseded for any tuple field carrying a recorded
            /// `TypeExpr` by `NestedTuple` below; survives only as the fallback
            /// when no tuple `TypeExpr` is on record.
            NestedAggregate,
            /// #21 — a nested *anonymous tuple* field whose heap is reachable
            /// through an enum / nested-struct leaf (or a mix with direct
            /// Vec/String). The LLVM-type-driven `NestedAggregate` path above is
            /// enum-blind (an enum's payload is all-i64 words, never the
            /// `vec_struct_type` it looks for), so `(Tok, i64)` with a heap enum
            /// `Tok` reads as no-heap and leaks. This path consults the tuple's
            /// source element `TypeExpr`s (`struct_field_type_exprs`) and routes
            /// each enum leaf through its own `__karac_drop_<E>`, each named
            /// nested struct through `__karac_drop_struct_<S>`, direct Vec/String
            /// through a cap-guarded free, and a nested tuple via recursion.
            /// Paired with `zero_tuple_elem_caps` at every tuple-element move-out
            /// site (the cap-zero dual) and with entry-copy of heap-bearing tuple
            /// params (so a caller-retained shared copy can't double-free) — see
            /// phase-12 #21.
            NestedTuple,
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
            /// ptr (`emit_enum_drop_switch`). `Option`/`Result` take the
            /// dedicated `OptionInline` path (Option only) below instead.
            EnumField,
            /// B-2026-07-03-28 Facet A — a field typed `Option[String]` /
            /// `Option[Vec[..]]` (inline `{ptr,len,cap}` payload). The
            /// let-binding inline-drop machinery (`FreeInlineOptionPayload`)
            /// frees such a payload only when the field is BOUND/MATCHED — a
            /// whole-struct PLAIN drop (a `Vec[A]` element, a scope-exit drop of
            /// an unconsumed value) has no binding, so the `Some` payload leaked.
            /// Free it here, tag-guarded, via the field's own
            /// `karac_drop_Option_<H>` (from `vec_elem_agg_drop_for_type_expr`,
            /// stored in the parallel `option_drops` side-vec). Gated on
            /// `aggregate_param_copy_supported_struct(struct)` = the SAME
            /// predicate that makes the struct CALLEE-OWNED as a by-value param
            /// (param_own entry-copies its Option payload). That gate is what
            /// makes this safe: a copy-supported struct is never a caller-retains
            /// shallow-shared instance, so freeing the Option here can't
            /// double-free a payload the caller also owns. Every destructure /
            /// consume site neutralizes the SOURCE tag (`zero_struct_field_move_cap`'s
            /// Option arm) so a consumed leaf's own free isn't doubled here.
            OptionInline,
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
        // B-2026-07-15-11 — a SINGLE-field generic wrapper `W[T] { f: T }`
        // whose sole field IS a bare type param that THIS monomorph binds to a
        // direct heap type (`String` / `Vec[..]` / `VecDeque[..]`). The
        // name-based classifier above reads the erased declared name `T`
        // (matches no arm) and leaves it `None`, so the field's heap buffer
        // leaked at scope exit (the concrete-field twin `W { f: String }` is
        // clean). Resolve the field's declared TypeExpr through the active mono
        // `subst`; if it is a direct Vec/String monomorph, classify
        // `VecOrString` (the VecOrString emit arm below substitutes the field
        // TypeExpr too, so a `Vec[String]` monomorph also drains its elements).
        //
        // GATED to a single field on PURPOSE: `struct_types[W]` lowers a bare-T
        // field to one erased i64 WORD (declaration-time lowering, no active
        // subst), while the instance is the monomorphized `{ {ptr,len,cap} }`.
        // The VecOrString GEP reinterprets field 0 as a `{ptr,len,cap}` Vec
        // struct, which is offset-correct ONLY for a field at offset 0 — a
        // multi-field wrapper with a mid bare-T heap field would mis-offset
        // every field after it (LLVM-layout erasure). A concrete (non-generic)
        // struct passes `subst == None` and is untouched. The PAIRED move sites
        // (borrowed-receiver field-return deep-clone in runtime.rs, move-cap
        // zeroing) are mono-aware for this shape so the added scope-exit drop
        // does not double-free a moved-out / accessor-returned field.
        // A bare generic-param field this monomorph binds to a direct heap type
        // (`String` / `Vec[..]` / `VecDeque[..]`): the name-based classifier read
        // the erased declared name `T` (matched no arm) and left it `None`, so
        // the field's heap buffer leaked at scope exit (the concrete-field twin
        // `W { f: String }` is clean). Resolve each such field's declared
        // TypeExpr through the active mono `subst`; a direct Vec/String monomorph
        // classifies `VecOrString` (the VecOrString emit arm substitutes the
        // field TypeExpr too, so a `Vec[String]` monomorph also drains its
        // elements).
        //
        // Applies to EVERY field, not just a sole one (B-2026-07-15-24 lifted the
        // original B-2026-07-15-11 `kinds.len() == 1` gate). That gate existed
        // only because the base `struct_types` layout GEP'd a bare-T field at one
        // erased i64 word, mis-offsetting every following field of a multi-field
        // wrapper; now that `st` above is the per-monomorph layout, a mid bare-T
        // heap field GEPs at its true widened offset, so classifying it (and the
        // Map/Vec/String/enum field after it) is offset-correct. A concrete
        // (non-generic) struct passes `subst == None` and is untouched. The
        // PAIRED move sites (move-cap zeroing, borrowed-receiver deep-clone) are
        // mono-aware for this shape so the added scope-exit drop does not
        // double-free a moved-out / accessor-returned field.
        if let Some(subst) = subst {
            let field_tes = self.struct_field_type_exprs.get(struct_name).cloned();
            if let Some(field_tes) = field_tes {
                for (idx, fte) in field_tes.iter().enumerate() {
                    if kinds.get(idx) != Some(&FieldDrop::None) {
                        continue;
                    }
                    let is_bare_param = matches!(
                        &fte.kind,
                        TypeKind::Path(p)
                            if p.segments.len() == 1
                                && p.generic_args.is_none()
                                && subst.contains_key(&p.segments[0])
                    );
                    if !is_bare_param {
                        continue;
                    }
                    let cte = crate::codegen::helpers::subst_type_params_in_type_expr(fte, subst);
                    // A direct heap buffer: `String`/`str` (the typechecker's two
                    // spellings — `is_string_type_expr` unifies them) or a
                    // `Vec`/`VecDeque` head. Both lower to the `{ptr,len,cap}`
                    // Vec-struct layout the `VecOrString` arm frees.
                    let is_vec_head = matches!(
                        &cte.kind,
                        TypeKind::Path(p)
                            if matches!(
                                p.segments.last().map(|s| s.as_str()),
                                Some("Vec") | Some("VecDeque")
                            )
                    );
                    if self.is_string_type_expr(&cte) || is_vec_head {
                        kinds[idx] = FieldDrop::VecOrString;
                    }
                }
            }
        }
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
                // Anonymous tuple (no declared type name). #21 — prefer the
                // source-`TypeExpr`-driven classification so an enum / nested
                // struct leaf is seen; the LLVM-type-driven
                // `aggregate_has_heap_field` is enum-blind. Fall back to the
                // type-driven check only when no tuple `TypeExpr` is recorded.
                let tuple_needs_drop = match self
                    .struct_field_type_exprs
                    .get(struct_name)
                    .and_then(|v| v.get(idx))
                    .map(|te| &te.kind)
                {
                    Some(TypeKind::Tuple(elems)) => {
                        Some(elems.iter().any(|e| self.type_expr_has_drop_heap(e)))
                    }
                    _ => None,
                };
                match tuple_needs_drop {
                    Some(true) => *k = FieldDrop::NestedTuple,
                    Some(false) => {}
                    None => {
                        if self.aggregate_has_heap_field(fst) {
                            *k = FieldDrop::NestedAggregate;
                        }
                    }
                }
            }
            // Phase 2: synthesize each named nested struct's drop fn; mark
            // `NestedStruct` only when one is actually needed (`Some`).
            // B-2026-07-15-11 — synthesize the per-monomorph nested drop
            // (subst derived from the field's declared `Box[String]` instance)
            // so a nested single-field generic wrapper's bare-T Vec/String field
            // is freed; a non-generic nested struct yields an empty subst and
            // the name-shared drop, unchanged.
            for idx in named_struct_fields {
                if let Some(Some(name)) = field_kinds.get(idx).cloned() {
                    let nsub = self.nested_struct_field_subst(struct_name, idx, subst, &name);
                    if self.emit_struct_drop_synthesis_mono(&name, &nsub).is_some() {
                        kinds[idx] = FieldDrop::NestedStruct;
                    }
                }
            }
        }
        // #15 — enum-field detection: a field the passes above left `None`
        // whose declared type name is a heap-bearing, non-shared user enum.
        // Name-based (an enum's LLVM layout — all-i64 words — is invisible to
        // the type-driven nested-aggregate pass). `Option`/`Result` are skipped
        // HERE (the enum drop switch is the wrong machinery for their inline
        // overlay); they take the dedicated `OptionInline` pass below instead.
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
        // B-2026-07-03-28 Facet A — free `Option[String]`/`Option[Vec[..]]`
        // fields, but ONLY when this struct is copy-supported (so it is
        // CALLEE-OWNED as a by-value param, with its Option payload entry-copied
        // by `param_own`). That gate is the soundness condition: a copy-supported
        // struct is never a caller-retains shallow-shared instance, so freeing
        // the Option here can't double-free a payload the caller also owns. Phase
        // 1 collects candidate indices; phase 2 synthesizes each drop fn (needs
        // `&mut self`, can't co-borrow `kinds`). The fn is stashed in
        // `option_drops` (a local enum can't carry a `FunctionValue<'ctx>`).
        let mut option_drops: Vec<Option<FunctionValue<'ctx>>> = vec![None; kinds.len()];
        let struct_callee_owned =
            self.aggregate_param_copy_supported_struct(struct_name, &mut Vec::new());
        if struct_callee_owned {
            let mut option_idxs: Vec<usize> = Vec::new();
            for (idx, k) in kinds.iter().enumerate() {
                if *k == FieldDrop::None
                    && matches!(
                        field_kinds.get(idx).and_then(|o| o.as_deref()),
                        Some("Option") | Some("Result")
                    )
                {
                    option_idxs.push(idx);
                }
            }
            for idx in option_idxs {
                let Some(field_te) = self
                    .struct_field_type_exprs
                    .get(struct_name)
                    .and_then(|v| v.get(idx))
                    .cloned()
                else {
                    continue;
                };
                // B-2026-07-21-15 — a `Result` field in the DIRECT
                // String/Vec-halves class: same registration route as the
                // Option arm (`vec_elem_agg_drop_for_type_expr` hands back
                // the tag-dispatched `karac_drop_Result_<ok>_<err>`),
                // symmetric with `field_copy_supported`'s Result admit and
                // the entry copy's `deep_copy_result_inline_heap_halves_in_
                // place`. Wider Result shapes never get here (the struct is
                // not callee-owned) and stay caller-retains.
                if matches!(
                    field_kinds.get(idx).and_then(|o| o.as_deref()),
                    Some("Result")
                ) {
                    if !self.result_field_direct_vecstr_halves_ok(&field_te) {
                        continue;
                    }
                    if let Some(f) = self.vec_elem_agg_drop_for_type_expr(&field_te) {
                        option_drops[idx] = Some(f);
                        kinds[idx] = FieldDrop::OptionInline;
                    }
                    continue;
                }
                // The payload classes `field_copy_supported` admits and
                // `param_own` entry-copies: the inline-`{ptr,len,cap}` String/Vec
                // overlay, PLUS (B-2026-07-04-7) a non-shared struct/enum payload
                // (boxed or inline) — `param_own`'s
                // `deep_copy_option_struct_enum_payload_in_place` duplicates it, so
                // freeing it here can't double-free the caller's copy. `Option[shared]`
                // is excluded (its drop is the combined struct rc-dec walker, not
                // this `OptionInline` free).
                let payload_droppable = Self::option_payload_te(&field_te)
                    .map(|pt| {
                        self.is_string_type_expr(&pt)
                            || self.extract_vec_elem_type(&pt).is_some()
                            || self.option_payload_struct_or_enum_drop_ok(&pt)
                    })
                    .unwrap_or(false);
                if !payload_droppable {
                    continue;
                }
                if let Some(f) = self.vec_elem_agg_drop_for_type_expr(&field_te) {
                    option_drops[idx] = Some(f);
                    kinds[idx] = FieldDrop::OptionInline;
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

        let fn_name = format!("__karac_drop_struct_{cache_key}");
        if let Some(f) = self.module.get_function(&fn_name) {
            self.struct_drop_fns.insert(cache_key.clone(), f);
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
        self.struct_drop_fns.insert(cache_key.clone(), drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let p_arg = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        // std.secret Zeroize (design.md § Clone / Drop / Zeroize): before the
        // inner value's heap buffer is freed by the field-drop loop below,
        // overwrite it with zeros so the secret's bytes don't linger in freed
        // heap. This is the `impl Drop for Secret[T] where T: Zeroize` contract
        // — `T::zeroize(&mut self.inner)` runs before the field destructor.
        // v1 covers `Secret[String]` (the token / key / HMAC case, matching the
        // shipped `.ct_eq` surface); `Vec[u8]` / `[u8; N]` inners and user
        // `Zeroize` impls are follow-on slices. Scoped to the stdlib `Secret`
        // via `secret_type_is_stdlib`, so a user's own `struct Secret` is
        // untouched. The memset only writes into the buffer (no free), so it
        // cannot double-free with the field-drop loop that frees it next.
        if struct_name == "Secret" && self.secret_type_is_stdlib {
            let inner_is_string = self
                .struct_field_type_exprs
                .get("Secret")
                .and_then(|v| v.first())
                .map(|fte| match subst {
                    Some(s) => crate::codegen::helpers::subst_type_params_in_type_expr(fte, s),
                    None => fte.clone(),
                })
                .map(|fte| self.is_string_type_expr(&fte))
                .unwrap_or(false);
            if inner_is_string {
                if let Ok(field_ptr) = self.builder.build_struct_gep(st, p_arg, 0, "sec.z.inner.p")
                {
                    let cap_p = self
                        .builder
                        .build_struct_gep(vec_ty, field_ptr, 2, "sec.z.cap.p")
                        .unwrap();
                    let cap = self
                        .builder
                        .build_load(i64_t, cap_p, "sec.z.cap")
                        .unwrap()
                        .into_int_value();
                    let is_heap = self
                        .builder
                        .build_int_compare(
                            IntPredicate::UGT,
                            cap,
                            i64_t.const_zero(),
                            "sec.z.is_heap",
                        )
                        .unwrap();
                    let z_bb = self.context.append_basic_block(drop_fn, "sec.z.do");
                    let z_done = self.context.append_basic_block(drop_fn, "sec.z.done");
                    self.builder
                        .build_conditional_branch(is_heap, z_bb, z_done)
                        .unwrap();
                    self.builder.position_at_end(z_bb);
                    let data_pp = self
                        .builder
                        .build_struct_gep(vec_ty, field_ptr, 0, "sec.z.data.pp")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_pp, "sec.z.data")
                        .unwrap()
                        .into_pointer_value();
                    // `cap` IS the String's byte capacity (the whole allocation),
                    // so zeroing `cap` bytes clears the entire backing buffer.
                    let _ = self.builder.build_memset(
                        data,
                        1,
                        self.context.i8_type().const_zero(),
                        cap,
                    );
                    self.builder.build_unconditional_branch(z_done).unwrap();
                    self.builder.position_at_end(z_done);
                }
            }
        }

        for (field_idx, kind) in kinds.iter().enumerate() {
            match kind {
                FieldDrop::None => {}
                FieldDrop::VecOrString => {
                    // #35 — a `Vec[T]` field whose element type itself carries
                    // heap (`Vec[Sp]`, `Sp { tok: Tk }` with a heap enum `Tk`;
                    // the parser's `Parser { tokens: Vec[SpannedToken] }`) must
                    // DRAIN each live element's payload before freeing the
                    // buffer — this arm previously freed only the
                    // `{ptr,len,cap}` buffer, leaking every unconsumed
                    // element's String/enum payload (the Vec-element peer of
                    // the #15 / #18 / #21 struct-drop-ignores-heap-leaf family).
                    // `vec_elem_agg_drop_for_type_expr` returns the per-element
                    // drop fn for a struct / enum / shared / Option element (→
                    // that type's own `__karac_drop_*`). It returns `None` for a
                    // *direct* `String` / `Map` / `Set` element and for a
                    // `Vec[collection]` element — those own heap the outer
                    // buffer-free misses (each element's own char / bucket /
                    // inner buffer), so a `Vec[String]` / `Vec[Map[..]]` /
                    // `Vec[Vec[..]]` FIELD leaked every element's payload
                    // (B-2026-07-03-28 facet: the struct-drop peer of the
                    // top-level `FreeVecBuffer` inline vec-struct / Map / tuple
                    // drain, which handles exactly these element shapes). Fall
                    // back to the unifying `emit_drop_fn_for_type_expr`, which
                    // frees a String's char buffer, a Map/Set's buckets, and a
                    // nested Vec's inner buffer per element. Bare scalars stay
                    // `None` (no drain loop). A bare `String` FIELD (not
                    // `Vec[String]`) has no Vec element type — `vec_inner_type_expr`
                    // returns `None` there — so its single char buffer is still
                    // freed by the buffer-free alone. Resolve it FIRST — the
                    // sub-emitter may synthesize a fn and move the builder's
                    // insert block, so capture it before opening the cap-guard
                    // blocks below (same discipline as the nested-shared Vec
                    // arm in `emit_nested_struct_shared_rc_decs`).
                    // Recycling hint size (phase-10 line 282): the resolved
                    // field type sizes the `karac_free_buf` fast-reject —
                    // `sizeof(T)` for a `Vec[T]` field (so a mid-size grid is
                    // parked), `1` for a String field, `1` if unresolved.
                    let field_hint_elem_size = self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .cloned()
                        .map(|fte| match subst {
                            Some(subst) => {
                                crate::codegen::helpers::subst_type_params_in_type_expr(&fte, subst)
                            }
                            None => fte,
                        })
                        .map(|fte| self.vec_field_free_hint_elem_size(&fte))
                        .unwrap_or(1);
                    let vec_elem_drop = self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .cloned()
                        // B-2026-07-15-11 — resolve the WHOLE field TypeExpr
                        // through the mono subst FIRST, so a bare-T field bound
                        // to `Vec[String]` (`W[T] { f: T }`, T = Vec[String])
                        // reads as a `Vec[String]` and its element drain fires.
                        // For the pre-existing `Vec[T]` field this maps
                        // `Vec[T]` -> `Vec[String]`, and the element-level subst
                        // below is then an identity no-op — behavior-preserving.
                        .map(|fte| match subst {
                            Some(subst) => {
                                crate::codegen::helpers::subst_type_params_in_type_expr(&fte, subst)
                            }
                            None => fte,
                        })
                        .and_then(|fte| crate::codegen::helpers::vec_inner_type_expr(&fte))
                        // B-2026-07-11-35 (push leg) — resolve a generic `Vec[T]`
                        // field's element (`T`) to the concrete monomorph type
                        // (`String`) so its per-element drop is chosen and the
                        // deep-copied element buffers are drained. A no-op (`subst`
                        // absent) for a non-generic struct.
                        .map(|elem_te| match subst {
                            Some(subst) => crate::codegen::helpers::subst_type_params_in_type_expr(
                                &elem_te, subst,
                            ),
                            None => elem_te,
                        })
                        .and_then(|elem_te| {
                            let f = self.vec_elem_agg_drop_for_type_expr(&elem_te).or_else(|| {
                                if Self::elem_te_needs_direct_recursive_drain(&elem_te) {
                                    Some(self.emit_drop_fn_for_type_expr(&elem_te))
                                } else {
                                    None
                                }
                            });
                            f.map(|f| (f, elem_te))
                        });
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
                    // #35 — drain each live element's heap payload BEFORE the
                    // buffer free (loop `0..len`, calling the per-element drop
                    // on `data + i`). `cap > 0` here implies a heap buffer; an
                    // empty Vec (`len == 0`) runs zero iterations. Skipped
                    // entirely for `String` / `Vec[primitive]` fields
                    // (`vec_elem_drop` is `None`).
                    if let Some((elem_drop, elem_te)) = &vec_elem_drop {
                        let elem_ty = self.llvm_type_for_type_expr(elem_te);
                        let len_ptr = self
                            .builder
                            .build_struct_gep(
                                vec_ty,
                                field_ptr,
                                1,
                                &format!("drop.field{field_idx}.len.p"),
                            )
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_ptr, &format!("drop.field{field_idx}.len"))
                            .unwrap()
                            .into_int_value();
                        let cond_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("drop.field{field_idx}.elem.cond"),
                        );
                        let body_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("drop.field{field_idx}.elem.body"),
                        );
                        let incr_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("drop.field{field_idx}.elem.incr"),
                        );
                        let after_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("drop.field{field_idx}.elem.after"),
                        );
                        let counter =
                            self.create_entry_alloca(drop_fn, "drop.elem.i", i64_t.into());
                        self.builder
                            .build_store(counter, i64_t.const_zero())
                            .unwrap();
                        self.builder.build_unconditional_branch(cond_bb).unwrap();
                        self.builder.position_at_end(cond_bb);
                        let cur = self
                            .builder
                            .build_load(i64_t, counter, "drop.elem.i.cur")
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(IntPredicate::ULT, cur, len, "drop.elem.i.lt")
                            .unwrap();
                        self.builder
                            .build_conditional_branch(lt, body_bb, after_bb)
                            .unwrap();
                        self.builder.position_at_end(body_bb);
                        let elem_ptr = unsafe {
                            self.builder
                                .build_gep(elem_ty, data, &[cur], "drop.elem.p")
                                .unwrap()
                        };
                        self.builder
                            .build_call(*elem_drop, &[elem_ptr.into()], "")
                            .unwrap();
                        self.builder.build_unconditional_branch(incr_bb).unwrap();
                        self.builder.position_at_end(incr_bb);
                        let next = self
                            .builder
                            .build_int_add(cur, i64_t.const_int(1, false), "drop.elem.i.next")
                            .unwrap();
                        self.builder.build_store(counter, next).unwrap();
                        self.builder.build_unconditional_branch(cond_bb).unwrap();
                        self.builder.position_at_end(after_bb);
                    }
                    // Recycling-aware release; hint sized by the field type
                    // (phase-10 line 282) so a mid-size `Vec[T]` field is parked.
                    self.emit_free_buf_call(data, cap, field_hint_elem_size);
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
                    // #23 — drop flags must reflect the K/V types: flag a side
                    // only when it lowers to the Vec/String 24-byte struct, so
                    // the runtime frees its per-entry `data` buffer. The old
                    // hardcoded `(1, 1)` made the runtime read offset-16 of an
                    // 8-byte scalar key as a bogus `cap` and free the key VALUE
                    // as a pointer — corruption on any occupied `Map[i64, i64]`
                    // field (B-2026-06-13-19), not the "conservative no-op" the
                    // prior comment assumed.
                    let (dk, dv) = self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .map(|fte| self.map_drop_flags(fte))
                        .unwrap_or((0, 0));
                    self.builder
                        .build_call(
                            self.karac_map_free_with_drop_vec_fn,
                            &[
                                handle.into(),
                                i32_t.const_int(dk, false).into(),
                                i32_t.const_int(dv, false).into(),
                            ],
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
                FieldDrop::NestedTuple => {
                    // #21 — drive tuple element drops off the source `TypeExpr`s
                    // so enum / nested-struct leaves are reached. Clone the
                    // element `TypeExpr`s: the emit walk below takes `&mut self`
                    // (synthesizes nested drop fns), which can't coexist with an
                    // immutable borrow of `struct_field_type_exprs`.
                    let elem_tes: Vec<TypeExpr> = match self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .map(|te| &te.kind)
                    {
                        Some(TypeKind::Tuple(elems)) => elems.clone(),
                        _ => continue,
                    };
                    let fst = match st.get_field_type_at_index(field_idx as u32) {
                        Some(inkwell::types::BasicTypeEnum::StructType(t)) => t,
                        _ => continue,
                    };
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            st,
                            p_arg,
                            field_idx as u32,
                            &format!("drop.field{field_idx}.tuple.p"),
                        )
                        .unwrap();
                    let saved_fn = self.current_fn;
                    self.current_fn = Some(drop_fn);
                    self.emit_tuple_elem_drops(field_ptr, fst, &elem_tes);
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
                    // the call below. B-2026-07-15-11 — same per-monomorph subst
                    // as phase 2, so a nested generic wrapper routes through its
                    // `__karac_drop_struct_Box$str` (which frees the bare-T field)
                    // rather than the name-shared drop that leaks it.
                    let nsub =
                        self.nested_struct_field_subst(struct_name, field_idx, subst, &nested_name);
                    if let Some(nested_drop_fn) =
                        self.emit_struct_drop_synthesis_mono(&nested_name, &nsub)
                    {
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
                FieldDrop::OptionInline => {
                    // B-2026-07-03-28 Facet A — free the field's inline
                    // `Option[String]`/`Option[Vec]` payload via its tag-guarded
                    // `karac_drop_Option_<H>` (stashed at classification time). A
                    // destructured/consumed field's source tag is zeroed at the
                    // move site so the guard skips it here (no double-free).
                    if let Some(opt_drop_fn) = option_drops[field_idx] {
                        let field_ptr = self
                            .builder
                            .build_struct_gep(
                                st,
                                p_arg,
                                field_idx as u32,
                                &format!("drop.field{field_idx}.opt.p"),
                            )
                            .unwrap();
                        self.builder
                            .build_call(opt_drop_fn, &[field_ptr.into()], "")
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

    /// #21 — does a (tuple-element or nested) `TypeExpr` carry heap a synthesized
    /// drop must free? Drives classification of an anonymous tuple struct field:
    /// the LLVM-type-driven `aggregate_has_heap_field` is enum-blind, so a tuple
    /// whose only heap lives inside an enum leaf — `(Tok, i64)` — reads as
    /// no-heap and leaks; consulting the source `TypeExpr` sees the leaf. `&self`,
    /// pure lookup. `Option`/`Result` are no-drop (their inline payloads are
    /// freed by the let-binding machinery); shared (RC) leaves are skipped.
    pub(super) fn type_expr_has_drop_heap(&self, te: &TypeExpr) -> bool {
        match &te.kind {
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.type_expr_has_drop_heap(e)),
            TypeKind::Path(p) => {
                let Some(name) = p.segments.last() else {
                    return false;
                };
                match name.as_str() {
                    // Both String spellings — an INFERRED TypeExpr (e.g. a
                    // tuple element te out of `enum_inst_type_exprs`) renders
                    // `type_to_type_expr(Type::Str)`'s lowercase `str`;
                    // matching only `String` silently classified such tuples
                    // as heapless and their drop synthesis no-op'd (the 3p
                    // spelling-trap lesson, fourth site).
                    "Vec" | "VecDeque" | "String" | "str" | "Map" | "HashMap" | "Set"
                    | "HashSet" => true,
                    "Option" | "Result" => false,
                    _ => {
                        if self.shared_types.contains_key(name) {
                            return false;
                        }
                        if let Some(layout) = self.enum_layouts.get(name) {
                            return !layout.is_shared
                                && layout
                                    .field_drop_kinds
                                    .values()
                                    .any(|ks| ks.iter().any(|dk| *dk != EnumDropKind::None));
                        }
                        if self.struct_types.contains_key(name) {
                            if let Some(fields) = self.struct_field_type_exprs.get(name) {
                                return fields.iter().any(|f| self.type_expr_has_drop_heap(f));
                            }
                        }
                        false
                    }
                }
            }
            _ => false,
        }
    }

    /// Copy-side companion to [`Self::type_expr_has_drop_heap`] for its
    /// deliberate `Option`/`Result` blind spot: true when `te` owns heap
    /// through an `Option[<heap payload>]` leaf (directly, through a tuple
    /// element, or through a non-shared struct field, recursively).
    /// `type_expr_has_drop_heap` hardcodes `Option | Result => false` and is
    /// load-bearing across drop synthesis, so it must not change — but the
    /// DEFENSIVE-COPY gates that use it to mean "a flat memcpy of this
    /// element aliases heap the drop will free" under-fire for an element
    /// struct like `AttrNode { string_value: Option[String] }`: the struct's
    /// drop DOES free the `Some` payload (`karac_drop_Option_String`), so a
    /// shallow element copy double-frees it (B-2026-07-10-4 residual). OR
    /// this predicate into those copy-side gates. `Result` is NOT counted:
    /// the value-clone synthesis (`emit_option_value_clone_fn`) only covers
    /// `Option` today, so counting `Result` would fire the deep-clone leg
    /// against a clone fn that still shallow-copies the payload — the
    /// same-shape `Result` residual stays documented on the ledger instead.
    pub(super) fn te_owns_option_heap_payload(&self, te: &TypeExpr) -> bool {
        match &te.kind {
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.te_owns_option_heap_payload(e)),
            TypeKind::Path(p) => {
                let Some(name) = p.segments.last() else {
                    return false;
                };
                match name.as_str() {
                    "Option" => p
                        .generic_args
                        .as_ref()
                        .and_then(|a| a.first())
                        .is_some_and(|a| match a {
                            crate::ast::GenericArg::Type(t) => {
                                self.is_string_type_expr(t)
                                    || self.extract_vec_elem_type(t).is_some()
                                    || self.type_expr_has_drop_heap(t)
                                    || self.shared_heap_type_for_type_expr(t).is_some()
                                    || self.te_owns_option_heap_payload(t)
                            }
                            _ => false,
                        }),
                    _ => {
                        if self.shared_types.contains_key(name.as_str()) {
                            return false;
                        }
                        if let Some(fields) = self.struct_field_type_exprs.get(name.as_str()) {
                            return fields.iter().any(|f| self.te_owns_option_heap_payload(f));
                        }
                        false
                    }
                }
            }
            _ => false,
        }
    }

    /// #23 — `(drop_key, drop_val)` flags for `karac_map_free_with_drop_vec`,
    /// derived from a `Map[K,V]` / `Set[T]` `TypeExpr`. A side is flagged 1
    /// **only** when its type lowers to the Vec/String 24-byte struct, so the
    /// runtime frees that side's per-entry `data` buffer; scalar sides (i64,
    /// bool, …) and pointer-handle sides (nested Map/Set/shared) get 0.
    ///
    /// The old call sites hardcoded `(1, 1)` on the (now-known-false) assumption
    /// that the runtime no-ops a non-heap side via its `cap == 0` guard. For a
    /// tightly-packed scalar map that is WRONG: with `drop_key = 1` the runtime
    /// reads offset-16 of an 8-byte key (the *next* slot's key, or OOB) as the
    /// "cap"; when that garbage is non-zero it frees offset-0 — the key VALUE —
    /// as a pointer, corrupting the heap on any occupied `Map[i64, i64]` (the
    /// `pointer being freed was not allocated` abort). Computing the flags from
    /// the K/V types is the fix.
    ///
    /// `(0, 0)` when the `TypeExpr` carries no generic args (e.g. a reconstructed
    /// single-segment `Path` from `infer_arg_elem_te`): correct for a scalar map;
    /// a heap-K/V map degrades to a leaked inner buffer, never corruption.
    pub(super) fn map_drop_flags(&self, te: &TypeExpr) -> (u64, u64) {
        if let Some((k_te, v_te)) = super::helpers::map_kv_type_exprs(te) {
            let k = self.llvm_ty_is_vec_struct(self.llvm_type_for_type_expr(&k_te)) as u64;
            let v = self.llvm_ty_is_vec_struct(self.llvm_type_for_type_expr(&v_te)) as u64;
            (k, v)
        } else if let Some(elem_te) = super::helpers::set_inner_type_expr(te) {
            // `Set[T]` lowers to `Map[T, ()]`; the element occupies the key half.
            let k = self.llvm_ty_is_vec_struct(self.llvm_type_for_type_expr(&elem_te)) as u64;
            (k, 0)
        } else {
            (0, 0)
        }
    }

    /// #21 — free the heap reachable through an anonymous tuple struct field,
    /// driven by the tuple's source element `TypeExpr`s: Vec/String → cap-guarded
    /// free; Map/Set → handle free; named non-shared enum → `emit_enum_drop_switch`;
    /// named non-shared struct → `__karac_drop_struct_<S>`; nested tuple →
    /// recurse. Reaches the enum leaves `emit_aggregate_heap_field_frees` misses.
    /// Appends cap-guard blocks to `current_fn` (the caller points it at the drop
    /// fn). Shared / `Option` / `Result` leaves are skipped.
    pub(super) fn emit_tuple_elem_drops(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        tuple_ty: StructType<'ctx>,
        elem_tes: &[TypeExpr],
    ) {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        for (i, te) in elem_tes.iter().enumerate() {
            let idx = i as u32;
            let Some(llvm_field) = tuple_ty.get_field_type_at_index(idx) else {
                continue;
            };
            match &te.kind {
                TypeKind::Tuple(inner) => {
                    if let inkwell::types::BasicTypeEnum::StructType(fst) = llvm_field {
                        let field_ptr = self
                            .builder
                            .build_struct_gep(tuple_ty, base_ptr, idx, "drop.tup.nested.p")
                            .unwrap();
                        self.emit_tuple_elem_drops(field_ptr, fst, inner);
                    }
                }
                TypeKind::Path(p) => {
                    let Some(name) = p.segments.last().cloned() else {
                        continue;
                    };
                    let field_ptr = self
                        .builder
                        .build_struct_gep(tuple_ty, base_ptr, idx, "drop.tup.elem.p")
                        .unwrap();
                    match name.as_str() {
                        // Both String spellings — inferred tuple tes spell
                        // `str` (the 3p trap; this emitter is the drop half,
                        // `zero_tuple_elem_cap_at` the move-suppression dual
                        // — both must agree or a moved tuple double-frees).
                        "Vec" | "VecDeque" | "String" | "str" => {
                            if matches!(llvm_field, inkwell::types::BasicTypeEnum::StructType(st) if st == vec_ty)
                            {
                                let data_pp = self
                                    .builder
                                    .build_struct_gep(vec_ty, field_ptr, 0, "drop.tup.data.pp")
                                    .unwrap();
                                let data = self
                                    .builder
                                    .build_load(ptr_ty, data_pp, "drop.tup.data")
                                    .unwrap()
                                    .into_pointer_value();
                                let cap_pp = self
                                    .builder
                                    .build_struct_gep(vec_ty, field_ptr, 2, "drop.tup.cap.pp")
                                    .unwrap();
                                let cap = self
                                    .builder
                                    .build_load(i64_t, cap_pp, "drop.tup.cap")
                                    .unwrap()
                                    .into_int_value();
                                // Hint sized by the tuple element type
                                // (phase-10 line 282): String/str → 1, Vec[T] →
                                // sizeof(T).
                                let tup_elem_size = self.vec_field_free_hint_elem_size(te);
                                self.emit_free_if_cap_positive(data, cap, tup_elem_size);
                            }
                        }
                        "Map" | "HashMap" | "Set" | "HashSet" => {
                            let handle = self
                                .builder
                                .build_load(ptr_ty, field_ptr, "drop.tup.map.handle")
                                .unwrap()
                                .into_pointer_value();
                            // #23 — flags from the K/V types (see `map_drop_flags`);
                            // the old `(1, 1)` garbage-freed an occupied scalar map.
                            let (dk, dv) = self.map_drop_flags(te);
                            self.builder
                                .build_call(
                                    self.karac_map_free_with_drop_vec_fn,
                                    &[
                                        handle.into(),
                                        i32_t.const_int(dk, false).into(),
                                        i32_t.const_int(dv, false).into(),
                                    ],
                                    "",
                                )
                                .unwrap();
                        }
                        "Option" | "Result" => {}
                        _ => {
                            if self.shared_types.contains_key(&name) {
                                // RC leaf — cleanup is the rc machinery's job.
                            } else if self.enum_layouts.contains_key(&name) {
                                if let Some(enum_drop_fn) = self.emit_enum_drop_switch(&name) {
                                    self.builder
                                        .build_call(enum_drop_fn, &[field_ptr.into()], "")
                                        .unwrap();
                                }
                            } else if self.struct_types.contains_key(&name) {
                                if let Some(nested_drop_fn) = self.emit_struct_drop_synthesis(&name)
                                {
                                    self.builder
                                        .build_call(nested_drop_fn, &[field_ptr.into()], "")
                                        .unwrap();
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// #21 — the cap-zero DUAL of [`Self::emit_tuple_elem_drops`]: zero the
    /// move-suppression caps of every heap leaf reachable through a tuple, so the
    /// owning struct's `NestedTuple` drop no-ops on a moved-out tuple. Called at
    /// every site that moves a whole tuple out of a struct it owns. `&self` —
    /// pure stores, no new blocks.
    pub(super) fn zero_tuple_elem_caps(
        &self,
        base_ptr: PointerValue<'ctx>,
        tuple_ty: StructType<'ctx>,
        elem_tes: &[TypeExpr],
    ) {
        for (i, te) in elem_tes.iter().enumerate() {
            self.zero_tuple_elem_cap_at(base_ptr, tuple_ty, i as u32, te);
        }
    }

    /// Cap-zero a SINGLE tuple element's heap (the per-element core of
    /// [`Self::zero_tuple_elem_caps`]). Used at a single-element move-out —
    /// `let x = t.0` — where only element `idx` is consumed and the tuple's
    /// other elements stay owned by the source.
    pub(super) fn zero_tuple_elem_cap_at(
        &self,
        base_ptr: PointerValue<'ctx>,
        tuple_ty: StructType<'ctx>,
        idx: u32,
        te: &TypeExpr,
    ) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let zero = self.context.i64_type().const_int(0, false);
        let Some(llvm_field) = tuple_ty.get_field_type_at_index(idx) else {
            return;
        };
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(tuple_ty, base_ptr, idx, "ztup.elem.p")
        else {
            return;
        };
        match &te.kind {
            TypeKind::Tuple(inner) => {
                if let inkwell::types::BasicTypeEnum::StructType(fst) = llvm_field {
                    self.zero_tuple_elem_caps(field_ptr, fst, inner);
                }
            }
            TypeKind::Path(p) => {
                let Some(name) = p.segments.last().map(|s| s.as_str()) else {
                    return;
                };
                match name {
                    // Both String spellings — the move-suppression dual of
                    // `emit_tuple_elem_drops`' String arm (see there).
                    "Vec" | "VecDeque" | "String" | "str" => {
                        if matches!(llvm_field, inkwell::types::BasicTypeEnum::StructType(st) if st == vec_ty)
                        {
                            if let Ok(cap_ptr) =
                                self.builder
                                    .build_struct_gep(vec_ty, field_ptr, 2, "ztup.cap.p")
                            {
                                let _ = self.builder.build_store(cap_ptr, zero);
                            }
                        }
                    }
                    "Map" | "HashMap" | "Set" | "HashSet" => {
                        let _ = self.builder.build_store(field_ptr, ptr_ty.const_null());
                    }
                    "Option" | "Result" => {}
                    _ => {
                        if self.shared_types.contains_key(name) {
                            // RC leaf — not freed by the tuple drop walk.
                        } else if let Some(layout) = self.enum_layouts.get(name).cloned() {
                            if !layout.is_shared {
                                self.zero_enum_payload_caps(field_ptr, &layout);
                            }
                        } else if self.struct_types.contains_key(name)
                            && !self.shared_types.contains_key(name)
                        {
                            self.zero_struct_move_caps(field_ptr, name);
                        }
                    }
                }
            }
            _ => {}
        }
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
            // Shared enum: route to the tag-switched enum drop fn, which
            // walks each variant's heap-owning payload (recursive shared
            // children, Option[shared], Vec/String, Map/Set) before the box
            // free. Without it a recursive shared enum (`shared enum Expr {
            // Num(i64), Bin(Expr, Expr) }`) leaked its child boxes — only the
            // outer box was freed (B-2026-06-13-11).
            return self.emit_shared_enum_rc_drop_fn(struct_name);
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
            /// `weak T` field: a single nullable box pointer. Drop is a
            /// `karac_weak_drop` (weak -= 1, freeing the box iff both counts
            /// hit zero) — NEVER the strong recursive dec (a weak ref does not
            /// own the target). `docs/spikes/weak-refs.md` (B-2026-07-19-8).
            WeakField,
            #[allow(dead_code)]
            _Phantom(&'a ()),
        }
        let kinds: Vec<SharedFieldKind<'_, 'ctx>> = field_type_exprs
            .iter()
            .enumerate()
            .map(|(i, te)| {
                let head_name = field_kinds.get(i).and_then(|n| n.as_deref());
                // `weak T` field — a single nullable box pointer, weak-dropped.
                if matches!(te.kind, TypeKind::Weak(_)) {
                    return SharedFieldKind::WeakField;
                }
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
        // A `weak` field needs its `karac_weak_drop` even when the struct has
        // no other heap fields, so it counts toward `any_walkable` above (the
        // `!None` test already includes `WeakField`).
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

        // User field 0's heap base within the full `heap_type` (which this walk
        // always GEPs against, rc word included): 1 for a conventional
        // `{ strong, fields… }` box, 2 for a weak-headered `{ strong, weak,
        // fields… }` box. Computed directly from the weak-header flag rather
        // than via `shared_gep_layout` — that funnel returns a base-0 TWIN
        // struct for the headerless niche, which this `heap_type`-based walk
        // must NOT mix in. (Headerless types never reach this recursive drop
        // synth; a weak-targeted type is force-headed, so base 2 against
        // `heap_type` is exact.) `docs/spikes/weak-refs.md`.
        let field_base: u32 = if info.has_weak_header { 2 } else { 1 };
        for (field_idx, kind) in kinds.iter().enumerate() {
            // Heap layout: control header at idx 0 (strong) [+ 1 (weak) when
            // weak-headered], then user fields. User field `field_idx` lives at
            // heap index `field_idx + field_base`.
            let heap_field_idx = field_idx as u32 + field_base;
            match kind {
                SharedFieldKind::None | SharedFieldKind::_Phantom(_) => {}
                SharedFieldKind::WeakField => {
                    // `weak T` field: load the weak slot and `karac_weak_drop`
                    // it (weak -= 1; frees the target box iff strong == 0 &&
                    // weak == 0). NEVER a strong dec — a weak ref does not own
                    // the target, so it must not trigger the target's payload
                    // drop. Null-safe (a `None` slot is a no-op).
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            heap_type,
                            p_arg,
                            heap_field_idx,
                            &format!("rcdrop.weak{field_idx}.p"),
                        )
                        .unwrap();
                    let slot = self
                        .builder
                        .build_load(ptr_ty, field_ptr, &format!("rcdrop.weak{field_idx}.ptr"))
                        .unwrap()
                        .into_pointer_value();
                    let weak_drop = self.weak_runtime_fn("karac_weak_drop", false);
                    self.builder
                        .build_call(weak_drop, &[slot.into()], "")
                        .unwrap();
                }
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
                    // > 0, DRAIN each live element's heap payload, then free
                    // the buffer. Mirrors `emit_struct_drop_synthesis`'s
                    // VecOrString arm (#35).
                    //
                    // B-2026-07-13-11: a `Vec[shared T]` FIELD (`shared struct
                    // Node { kids: Vec[Node] }`) previously froze only the
                    // `{ptr,len,cap}` buffer here — it never dec'd the shared
                    // ELEMENTS, so every child box pushed into the Vec leaked
                    // one ref (LSan: 40-byte Node boxes accumulating). The
                    // non-shared struct-drop peer already drains via
                    // `vec_elem_agg_drop_for_type_expr` (→ the element's
                    // `emit_vec_elem_rc_dec_fn` for a shared element: load the
                    // slot's RC pointer, null-check, rc-dec). Resolve that
                    // per-element drop FIRST — the sub-emitter may synthesize a
                    // fn and move the builder's insert block (it save/restores,
                    // so the field GEP below still lands in this drop_fn) —
                    // before opening the cap-guard blocks. `None` for a bare
                    // `String` / `Vec[primitive]` field (buffer-free alone
                    // suffices); a self-recursive `Vec[Self]` resolves to this
                    // fn's own in-progress cache entry (RC cycles stay
                    // RC-managed, i.e. leak by design — the acyclic case frees).
                    // Recycling hint size (phase-10 line 282): sizeof(T) for a
                    // `Vec[T]` field, 1 for a String field / if unresolved.
                    let field_hint_elem_size = self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .cloned()
                        .map(|fte| self.vec_field_free_hint_elem_size(&fte))
                        .unwrap_or(1);
                    let vec_elem_drop = self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .cloned()
                        .and_then(|fte| crate::codegen::helpers::vec_inner_type_expr(&fte))
                        .and_then(|elem_te| {
                            // A `Vec[Tensor]` FIELD (`shared struct Tape { grads:
                            // Vec[Tensor[f32, [?]]] }`, tensor-valued autograd)
                            // previously drained only the `{ptr,len,cap}` buffer —
                            // each element `ptr` to a `[rank][dims][data]` block
                            // leaked. Route it to `emit_tensor_drop_fn` (via the
                            // Tensor arm of `emit_drop_fn_for_type_expr`) per
                            // element. Scoped to this SHARED-struct drain, NOT the
                            // shared `elem_te_needs_direct_recursive_drain`
                            // predicate: that predicate also gates the owned-param
                            // deep-COPY (`param_own.rs`), and there is no tensor-
                            // element clone, so widening it would unbalance
                            // copy-depth vs drop-depth (double-free) for a VALUE
                            // struct field. A shared struct is RC-shared, never
                            // deep-copied, so draining here has no copy peer to
                            // unbalance.
                            let elem_is_tensor =
                                self.tensor_var_info_from_type_expr(&elem_te).is_some();
                            let f = self.vec_elem_agg_drop_for_type_expr(&elem_te).or_else(|| {
                                if Self::elem_te_needs_direct_recursive_drain(&elem_te)
                                    || elem_is_tensor
                                {
                                    Some(self.emit_drop_fn_for_type_expr(&elem_te))
                                } else {
                                    None
                                }
                            });
                            f.map(|f| (f, elem_te))
                        });
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
                    // Drain each live element's payload BEFORE the buffer free
                    // (loop `0..len`, calling the per-element drop on `data + i`).
                    // `cap > 0` here implies a heap buffer; `len == 0` runs zero
                    // iterations. Skipped for `String` / `Vec[primitive]`
                    // (`vec_elem_drop` is `None`).
                    if let Some((elem_drop, elem_te)) = &vec_elem_drop {
                        let elem_ty = self.llvm_type_for_type_expr(elem_te);
                        let len_ptr = self
                            .builder
                            .build_struct_gep(
                                vec_ty,
                                field_ptr,
                                1,
                                &format!("rcdrop.vec{field_idx}.len.p"),
                            )
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_ptr, &format!("rcdrop.vec{field_idx}.len"))
                            .unwrap()
                            .into_int_value();
                        let cond_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("rcdrop.vec{field_idx}.elem.cond"),
                        );
                        let body_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("rcdrop.vec{field_idx}.elem.body"),
                        );
                        let incr_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("rcdrop.vec{field_idx}.elem.incr"),
                        );
                        let after_bb = self.context.append_basic_block(
                            drop_fn,
                            &format!("rcdrop.vec{field_idx}.elem.after"),
                        );
                        let counter =
                            self.create_entry_alloca(drop_fn, "rcdrop.elem.i", i64_t.into());
                        self.builder
                            .build_store(counter, i64_t.const_zero())
                            .unwrap();
                        self.builder.build_unconditional_branch(cond_bb).unwrap();
                        self.builder.position_at_end(cond_bb);
                        let cur = self
                            .builder
                            .build_load(i64_t, counter, "rcdrop.elem.i.cur")
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(IntPredicate::ULT, cur, len, "rcdrop.elem.i.lt")
                            .unwrap();
                        self.builder
                            .build_conditional_branch(lt, body_bb, after_bb)
                            .unwrap();
                        self.builder.position_at_end(body_bb);
                        let elem_ptr = unsafe {
                            self.builder
                                .build_gep(elem_ty, data, &[cur], "rcdrop.elem.p")
                                .unwrap()
                        };
                        self.builder
                            .build_call(*elem_drop, &[elem_ptr.into()], "")
                            .unwrap();
                        self.builder.build_unconditional_branch(incr_bb).unwrap();
                        self.builder.position_at_end(incr_bb);
                        let next = self
                            .builder
                            .build_int_add(cur, i64_t.const_int(1, false), "rcdrop.elem.i.next")
                            .unwrap();
                        self.builder.build_store(counter, next).unwrap();
                        self.builder.build_unconditional_branch(cond_bb).unwrap();
                        self.builder.position_at_end(after_bb);
                    }
                    // Recycling-aware release; hint sized by the field type
                    // (phase-10 line 282) so a mid-size shared-struct `Vec[T]`
                    // field parks.
                    self.emit_free_buf_call(data, cap, field_hint_elem_size);
                    self.builder.build_unconditional_branch(skip_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                }
                SharedFieldKind::MapOrSet => {
                    // Map/Set: `karac_map_free_with_drop_vec` with per-side
                    // `(drop_key, drop_val)` flags derived from the field's
                    // declared `Map[K,V]` / `Set[T]` type (see `map_drop_flags`).
                    // The old hardcoded `(1, 1)` assumed the runtime no-ops a
                    // scalar side via a `cap == 0` guard — FALSE: for an occupied
                    // `Map[i64, u64]` the runtime reads offset-16 of an 8-byte key
                    // as a bogus `cap` and frees the key VALUE as a pointer,
                    // corrupting the heap (B-2026-07-08-12 — the sibling of the
                    // non-shared/tuple fixes at `emit_struct_drop_synthesis` /
                    // `emit_tuple_elem_drops`; here the null map handle before
                    // that bug's construction fix masked it, `karac_map_new` in a
                    // shared-struct constructor unmasked it). Mirrors line 1475.
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
                    let field_te = self
                        .struct_field_type_exprs
                        .get(struct_name)
                        .and_then(|v| v.get(field_idx))
                        .cloned();
                    // B-2026-07-13-11 sibling (B-2026-07-13-12): a shared K or V
                    // needs a per-bucket rc-dec BEFORE the runtime free releases
                    // the bucket storage — the type-erased
                    // `karac_map_free_with_drop_vec` can't see the shared layout
                    // (its `map_drop_flags` are 0 for a shared side, so it never
                    // touches the RC handles), so a `shared struct Owner { cache:
                    // Map[i64, Node] }` (Node shared) dropped the bucket storage
                    // without ever dec'ing the shared VALUES — one ref leaked per
                    // live entry. This is exactly what the NON-shared struct-drop
                    // peer already does (`emit_struct_drop_synthesis`'s MapOrSet
                    // arm); the shared arm simply lacked the walk. `current_fn` is
                    // `drop_fn` here, so the walk's blocks attach to this body.
                    if let Some(field_te) = &field_te {
                        if let Some((k_te, v_te)) = super::helpers::map_kv_type_exprs(field_te) {
                            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&v_te) {
                                self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, true);
                            }
                            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&k_te) {
                                self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, false);
                            }
                        } else if let Some(elem_te) = super::helpers::set_inner_type_expr(field_te)
                        {
                            // `Set[T]` lowers to `Map[T, ()]`; the element is the key half.
                            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&elem_te) {
                                self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, false);
                            }
                        }
                    }
                    let (dk, dv) = field_te
                        .as_ref()
                        .map(|fte| self.map_drop_flags(fte))
                        .unwrap_or((0, 0));
                    self.builder
                        .build_call(
                            self.karac_map_free_with_drop_vec_fn,
                            &[
                                handle.into(),
                                i32_t.const_int(dk, false).into(),
                                i32_t.const_int(dv, false).into(),
                            ],
                            "",
                        )
                        .unwrap();
                }
            }
        }

        // Finally, free the heap allocation itself. Weak-aware: a weak-headered
        // box routes through `karac_weak_box_strong_zero_release` so an
        // outstanding weak ref keeps the control header alive (the payload walk
        // above already dropped the owned fields). Conventional boxes `free`.
        self.emit_shared_box_free(heap_type, p_arg);
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        self.current_fn = saved_fn;

        Some(drop_fn)
    }

    /// Load the i64 payload word at heap index `word_idx`, reinterpret it as a
    /// pointer, null-check, and rc-dec it as `child_heap`. The shared-child dec
    /// primitive for shared-enum payloads (the payload word holds the child RC
    /// box pointer reinterpreted as i64 by `coerce_to_payload_words` at
    /// construction). Builder is left positioned at the post-dec join block.
    fn emit_enum_word_shared_dec(
        &mut self,
        enum_heap: StructType<'ctx>,
        p_arg: PointerValue<'ctx>,
        word_idx: usize,
        child_heap: StructType<'ctx>,
        label: &str,
    ) {
        let drop_fn = self.current_fn.unwrap();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let wp = self
            .builder
            .build_struct_gep(enum_heap, p_arg, word_idx as u32, &format!("{label}.wp"))
            .unwrap();
        let w = self
            .builder
            .build_load(i64_t, wp, &format!("{label}.w"))
            .unwrap()
            .into_int_value();
        let child = self
            .builder
            .build_int_to_ptr(w, ptr_ty, &format!("{label}.child"))
            .unwrap();
        let is_null = self
            .builder
            .build_is_null(child, &format!("{label}.isnull"))
            .unwrap();
        let do_bb = self
            .context
            .append_basic_block(drop_fn, &format!("{label}.do"));
        let skip_bb = self
            .context
            .append_basic_block(drop_fn, &format!("{label}.skip"));
        self.builder
            .build_conditional_branch(is_null, skip_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        self.emit_refcount_dec_by_type(child_heap, child);
        self.builder.build_unconditional_branch(skip_bb).unwrap();
        self.builder.position_at_end(skip_bb);
    }

    /// Emit the drop for one heap-owning payload field of a shared-enum variant
    /// whose words begin at heap index `word_idx` (= the field's `start_word` +
    /// 2 for the `{rc, tag}` prefix). Classifies `te` like
    /// `emit_shared_struct_rc_drop_fn`'s field classifier — recursive shared
    /// child / `Option[shared]` (niche single-word or full 4-word) / Vec /
    /// String / Map / Set — and mirrors its per-kind emission against the enum
    /// heap type. Returns true if it emitted a drop (caller tracks walkability).
    fn emit_shared_enum_field_drop(
        &mut self,
        enum_heap: StructType<'ctx>,
        p_arg: PointerValue<'ctx>,
        word_idx: usize,
        num_words: usize,
        te: &TypeExpr,
        label: &str,
    ) -> bool {
        let drop_fn = self.current_fn.unwrap();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();

        // Option[shared T].
        if let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(te) {
            let child_heap = inner_info.heap_type;
            if num_words <= 1 {
                // Niche layout: single nullable ptr word — same as a bare
                // shared child.
                self.emit_enum_word_shared_dec(enum_heap, p_arg, word_idx, child_heap, label);
            } else {
                // Full `Option { tag, w0, … }` packed into consecutive payload
                // words: branch on `tag == Some`, then dec the inner at w0.
                let tag_ptr = self
                    .builder
                    .build_struct_gep(enum_heap, p_arg, word_idx as u32, &format!("{label}.tag.p"))
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, &format!("{label}.tag"))
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
                        &format!("{label}.is_some"),
                    )
                    .unwrap();
                let do_bb = self
                    .context
                    .append_basic_block(drop_fn, &format!("{label}.do"));
                let skip_bb = self
                    .context
                    .append_basic_block(drop_fn, &format!("{label}.skip"));
                self.builder
                    .build_conditional_branch(is_some, do_bb, skip_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                self.emit_enum_word_shared_dec(
                    enum_heap,
                    p_arg,
                    word_idx + 1,
                    child_heap,
                    &format!("{label}.inner"),
                );
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
            }
            return true;
        }

        // Plain shared `T` (including the enum's own type — direct recursion).
        if let TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                if let Some(inner) = self.shared_types.get(seg.as_str()).cloned() {
                    self.emit_enum_word_shared_dec(
                        enum_heap,
                        p_arg,
                        word_idx,
                        inner.heap_type,
                        label,
                    );
                    return true;
                }
            }
        }

        // B-2026-06-14-28 — a plain (non-shared) user struct payload that
        // transitively owns `shared` fields: the AST-port operand-wrapper
        // shape `Add(BinOp)` + `struct BinOp { left: Expr, right: Expr }`
        // (`Expr` shared). The struct is laid out INLINE in the enum payload
        // words; its `shared` fields are RC pointers that must be dec'd or
        // they leak. `field_is_walkable` flags it (it must, or this variant
        // gets no drop block); here GEP to the inline word region as the
        // struct's LLVM type and rc-dec each shared field via
        // `emit_nested_struct_shared_rc_decs`. (Its OWN Vec/String/enum
        // buffers are freed by `__karac_drop_struct_<S>`, invoked separately
        // by the value-drop path; here we only release the inline RC
        // children the value path leaves untouched.)
        if let TypeKind::Path(p) = &te.kind {
            if let Some(seg) = p.segments.last() {
                let sname = seg.clone();
                // `struct_owns_shared_field` OR `type_expr_has_drop_heap`: the
                // walker (`emit_nested_struct_shared_rc_decs`, owns_buffer_free)
                // both rc-decs inline shared children AND frees the payload's
                // own Vec/String buffers. A struct that owns ONLY a String/Vec
                // (no shared edge) — `IdentExpr { name: String }`, the parser's
                // `Ident`/`Str` node — must still be walked, else its String
                // leaks when the box drops (it has no top-level match path to
                // free it when reached as a nested child of a Binary box).
                if self.struct_types.contains_key(&sname)
                    && !self.shared_types.contains_key(&sname)
                    && (self.struct_owns_shared_field(&sname, &mut Vec::new())
                        || self.type_expr_has_drop_heap(te)
                        // B-2026-07-11-39 — mirror `field_is_walkable`'s union
                        // (they must agree, per the comment above): a payload
                        // struct owning only an `Option[<inline-heap>]` field is
                        // heap-owning for drop purposes despite
                        // `type_expr_has_drop_heap`'s Option blind spot.
                        || self.te_owns_option_heap_payload(te))
                {
                    // Inline vs heap-BOXED, recomputed from the SAME predicate
                    // `coerce_to_payload_words` boxes on (`llvm_type_word_count(T)
                    // > area`): a struct payload wider than its allotted payload
                    // words was malloc'd at construction and only its box pointer
                    // lives in word 0 (the `Block` of `struct Block { tail:
                    // Option[Expr] }` used as `Expr.Blk(Block)` — the Option field
                    // hits the enum-in-enum carve-out, undersizing the area to 1
                    // word while `Block` is 4). Before this, the struct arm ALWAYS
                    // read the payload inline, so for a boxed payload it walked the
                    // box-POINTER word as if it were the struct's first field (a
                    // garbage tag → the recursion was skipped) and never freed the
                    // box — leaking the box AND every heap child reachable through
                    // it (B-2026-06-20: the self-host render leak). Box: deref word
                    // 0 to the heap struct, walk it (frees its buffers + rc-decs its
                    // shared/Option children), then free the box. Inline: walk in
                    // place. Pack/unpack (`coerce_to_payload_words` /
                    // `reconstruct_payload_value`) and now drop all agree on the
                    // predicate, so the three stay coherent.
                    let boxed = self
                        .struct_types
                        .get(&sname)
                        .map(|st| Self::llvm_type_word_count((*st).into()) > num_words)
                        .unwrap_or(false);
                    if boxed {
                        let wp = self
                            .builder
                            .build_struct_gep(
                                enum_heap,
                                p_arg,
                                word_idx as u32,
                                &format!("{label}.nstr.box.wp"),
                            )
                            .unwrap();
                        let w = self
                            .builder
                            .build_load(i64_t, wp, &format!("{label}.nstr.box.w"))
                            .unwrap()
                            .into_int_value();
                        let box_ptr = self
                            .builder
                            .build_int_to_ptr(w, ptr_ty, &format!("{label}.nstr.box.p"))
                            .unwrap();
                        let is_null = self
                            .builder
                            .build_is_null(box_ptr, &format!("{label}.nstr.box.isnull"))
                            .unwrap();
                        let do_bb = self
                            .context
                            .append_basic_block(drop_fn, &format!("{label}.nstr.box.do"));
                        let skip_bb = self
                            .context
                            .append_basic_block(drop_fn, &format!("{label}.nstr.box.skip"));
                        self.builder
                            .build_conditional_branch(is_null, skip_bb, do_bb)
                            .unwrap();
                        self.builder.position_at_end(do_bb);
                        // The box owns the struct's heap children (no
                        // `__karac_drop_struct_<S>` runs here), so the walker frees
                        // the boxed struct's DIRECT Vec/String buffers
                        // (`owns_buffer_free=true`) and rc-decs its shared /
                        // `Option[shared]` children; then free the heap box itself.
                        // `nested_buffer_free = Some(false)`: a nested heap STRUCT
                        // field of the boxed payload (`IfNode.then_block: Block`)
                        // may be MOVED OUT by the match binding (`let tb =
                        // nd.then_block`), whose own value-drop frees that struct's
                        // BUFFERS — re-freeing them here double-frees (the
                        // regression that surfaced
                        // `test_e2e_shared_enum_payload_with_nested_heap_struct_field`).
                        // So recurse to rc-dec the nested struct's RC children (the
                        // value-drop does NOT) while leaving its buffers to the
                        // move-out owner: no double-free, no stranded RC child.
                        self.emit_nested_struct_shared_rc_decs_ex(
                            box_ptr,
                            &sname,
                            drop_fn,
                            true,
                            Some(false),
                        );
                        self.builder
                            .build_call(self.free_fn, &[box_ptr.into()], "")
                            .unwrap();
                        self.builder.build_unconditional_branch(skip_bb).unwrap();
                        self.builder.position_at_end(skip_bb);
                    } else if let Ok(field_ptr) = self.builder.build_struct_gep(
                        enum_heap,
                        p_arg,
                        word_idx as u32,
                        &format!("{label}.nstr.p"),
                    ) {
                        // RC-drop path (shared-enum box): the box owns the inline
                        // struct payload's Vec/String buffers — no
                        // `__karac_drop_struct_<S>` runs here, so the walker frees
                        // them (`owns_buffer_free=true`), in addition to rc-dec'ing
                        // the inline shared children.
                        self.emit_nested_struct_shared_rc_decs(field_ptr, &sname, drop_fn, true);
                    }
                    return true;
                }
            }
        }

        // Vec / String (3 payload words `{data, len, cap}`): free data when
        // cap > 0. The payload words reinterpret as the `{ptr,i64,i64}` vec
        // struct (ptr and i64 are both 8 bytes), as the struct drop does.
        let head = if let TypeKind::Path(p) = &te.kind {
            p.segments.last().map(|s| s.as_str())
        } else {
            None
        };
        match head {
            Some("Vec") | Some("VecDeque") | Some("String") => {
                // B-2026-07-13-13 (the shared-enum sibling of B-2026-07-13-11):
                // a `Vec[shared T]` PAYLOAD (`shared enum Tree { Branch(Vec[Node])
                // }`) must DRAIN each live element's RC box before freeing the
                // buffer — this arm previously froze only the `{ptr,len,cap}`
                // buffer, leaking every element's box. Resolve the per-element
                // drop FIRST (may synthesize a fn / move the builder, which it
                // save/restores) — `vec_elem_agg_drop_for_type_expr` returns the
                // shared element's `emit_vec_elem_rc_dec_fn`; `None` for a bare
                // `String` payload or a `Vec[primitive]` (buffer-free alone).
                let vec_elem_drop =
                    crate::codegen::helpers::vec_inner_type_expr(te).and_then(|elem_te| {
                        let f = self.vec_elem_agg_drop_for_type_expr(&elem_te).or_else(|| {
                            if Self::elem_te_needs_direct_recursive_drain(&elem_te) {
                                Some(self.emit_drop_fn_for_type_expr(&elem_te))
                            } else {
                                None
                            }
                        });
                        f.map(|f| (f, elem_te))
                    });
                let field_ptr = self
                    .builder
                    .build_struct_gep(enum_heap, p_arg, word_idx as u32, &format!("{label}.vec.p"))
                    .unwrap();
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, field_ptr, 2, &format!("{label}.cap.p"))
                    .unwrap();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, &format!("{label}.cap"))
                    .unwrap()
                    .into_int_value();
                let is_heap = self
                    .builder
                    .build_int_compare(
                        IntPredicate::UGT,
                        cap,
                        i64_t.const_zero(),
                        &format!("{label}.is_heap"),
                    )
                    .unwrap();
                let free_bb = self
                    .context
                    .append_basic_block(drop_fn, &format!("{label}.free"));
                let skip_bb = self
                    .context
                    .append_basic_block(drop_fn, &format!("{label}.skip"));
                self.builder
                    .build_conditional_branch(is_heap, free_bb, skip_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                let data_pp = self
                    .builder
                    .build_struct_gep(vec_ty, field_ptr, 0, &format!("{label}.data.p"))
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_pp, &format!("{label}.data"))
                    .unwrap()
                    .into_pointer_value();
                // Drain each live element BEFORE the buffer free (loop `0..len`,
                // per-element drop on `data + i`). Skipped when `vec_elem_drop`
                // is `None` (String / Vec[primitive]).
                if let Some((elem_drop, elem_te)) = &vec_elem_drop {
                    let elem_ty = self.llvm_type_for_type_expr(elem_te);
                    let len_ptr = self
                        .builder
                        .build_struct_gep(vec_ty, field_ptr, 1, &format!("{label}.len.p"))
                        .unwrap();
                    let len = self
                        .builder
                        .build_load(i64_t, len_ptr, &format!("{label}.len"))
                        .unwrap()
                        .into_int_value();
                    let cond_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{label}.elem.cond"));
                    let body_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{label}.elem.body"));
                    let incr_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{label}.elem.incr"));
                    let after_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{label}.elem.after"));
                    let counter = self.create_entry_alloca(drop_fn, "enumvec.elem.i", i64_t.into());
                    self.builder
                        .build_store(counter, i64_t.const_zero())
                        .unwrap();
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                    self.builder.position_at_end(cond_bb);
                    let cur = self
                        .builder
                        .build_load(i64_t, counter, "enumvec.elem.i.cur")
                        .unwrap()
                        .into_int_value();
                    let lt = self
                        .builder
                        .build_int_compare(IntPredicate::ULT, cur, len, "enumvec.elem.i.lt")
                        .unwrap();
                    self.builder
                        .build_conditional_branch(lt, body_bb, after_bb)
                        .unwrap();
                    self.builder.position_at_end(body_bb);
                    let elem_ptr = unsafe {
                        self.builder
                            .build_gep(elem_ty, data, &[cur], "enumvec.elem.p")
                            .unwrap()
                    };
                    self.builder
                        .build_call(*elem_drop, &[elem_ptr.into()], "")
                        .unwrap();
                    self.builder.build_unconditional_branch(incr_bb).unwrap();
                    self.builder.position_at_end(incr_bb);
                    let next = self
                        .builder
                        .build_int_add(cur, i64_t.const_int(1, false), "enumvec.elem.i.next")
                        .unwrap();
                    self.builder.build_store(counter, next).unwrap();
                    self.builder.build_unconditional_branch(cond_bb).unwrap();
                    self.builder.position_at_end(after_bb);
                }
                // Recycling-aware release; hint sized by the payload field type
                // (phase-10 line 282): String → 1, Vec[T] → sizeof(T).
                let enum_field_size = self.vec_field_free_hint_elem_size(te);
                self.emit_free_buf_call(data, cap, enum_field_size);
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                true
            }
            Some("Map") | Some("HashMap") | Some("Set") | Some("HashSet") => {
                let wp = self
                    .builder
                    .build_struct_gep(enum_heap, p_arg, word_idx as u32, &format!("{label}.map.p"))
                    .unwrap();
                let w = self
                    .builder
                    .build_load(i64_t, wp, &format!("{label}.map.w"))
                    .unwrap()
                    .into_int_value();
                let handle = self
                    .builder
                    .build_int_to_ptr(w, ptr_ty, &format!("{label}.map.handle"))
                    .unwrap();
                // B-2026-07-13-15 (the shared-enum sibling of B-2026-07-13-12): a
                // shared K or V needs a per-bucket rc-dec BEFORE the runtime free
                // releases the bucket storage — `karac_map_free_with_drop_vec` is
                // type-erased (its flags are 0 for a shared side, so it never
                // touches the RC handles), so a `shared enum Store { Full(Map[i64,
                // Node]) }` (Node shared) dropped the bucket storage without ever
                // dec'ing the shared VALUES → one ref leaked per live entry.
                // Mirrors the struct-drop MapOrSet arm. `current_fn` is the enum
                // drop fn here, so the walk's blocks attach to this body.
                if let Some((k_te, v_te)) = super::helpers::map_kv_type_exprs(te) {
                    if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&v_te) {
                        self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, true);
                    }
                    if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&k_te) {
                        self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, false);
                    }
                } else if let Some(elem_te) = super::helpers::set_inner_type_expr(te) {
                    // `Set[T]` lowers to `Map[T, ()]`; the element is the key half.
                    if let Some(heap_ty) = self.shared_heap_type_for_type_expr(&elem_te) {
                        self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, false);
                    }
                }
                // Per-side flags from the payload's `Map[K,V]` / `Set[T]` type,
                // not the old hardcoded `(1, 1)` — a scalar-keyed map payload
                // (`Map[i64, u64]`) would otherwise free the key VALUE as a
                // pointer on drop (B-2026-07-08-12, shared-enum sibling of the
                // struct/tuple fixes; `map_drop_flags` returns `(0, 0)` here).
                let i32_t = self.context.i32_type();
                let (dk, dv) = self.map_drop_flags(te);
                self.builder
                    .build_call(
                        self.karac_map_free_with_drop_vec_fn,
                        &[
                            handle.into(),
                            i32_t.const_int(dk, false).into(),
                            i32_t.const_int(dv, false).into(),
                        ],
                        "",
                    )
                    .unwrap();
                true
            }
            _ => false,
        }
    }

    /// `__karac_rc_drop_<Enum>(ptr)` for a **shared enum** — the tag-switched
    /// sibling of `emit_shared_struct_rc_drop_fn`. Loads the tag (heap index 1),
    /// switches to a per-variant block that walks that variant's heap-owning
    /// payload fields (`emit_shared_enum_field_drop`), then frees the box.
    /// Without it `emit_rc_dec` plain-`free`d a shared enum box and stranded its
    /// recursive children / heap payload (B-2026-06-13-11). Memoized in
    /// `rc_drop_fns`; the cache insert precedes the body so direct recursion
    /// (`shared enum Expr { Bin(Expr, Expr) }`) resolves to the in-progress fn.
    /// Returns `None` (and caches it) when no variant owns heap and there's no
    /// user `impl Drop` — `emit_rc_dec` then uses plain `free`.
    pub(super) fn emit_shared_enum_rc_drop_fn(
        &mut self,
        enum_name: &str,
    ) -> Option<FunctionValue<'ctx>> {
        if let Some(slot) = self.rc_drop_fns.get(enum_name) {
            return *slot;
        }
        let info = self.shared_types.get(enum_name)?.clone();
        let heap_type = info.heap_type;
        let layout = self.enum_layouts.get(enum_name)?.clone();

        // Per-variant field TypeExprs from the source AST. Each entry pairs the
        // variant name with its field TypeExprs in declaration order.
        let prog = self.program_snapshot.clone()?;
        let variants: Vec<(String, Vec<TypeExpr>)> = prog
            .items
            .iter()
            .find_map(|it| match it {
                Item::EnumDef(e) if e.name == enum_name => Some(&e.variants),
                _ => None,
            })?
            .iter()
            .map(|v| {
                let tys: Vec<TypeExpr> = match &v.kind {
                    VariantKind::Unit => Vec::new(),
                    VariantKind::Tuple(tys) => tys.clone(),
                    VariantKind::Struct(fields) => fields.iter().map(|f| f.ty.clone()).collect(),
                };
                (v.name.clone(), tys)
            })
            .collect();

        // Does any field of any variant own heap (so the box free needs a
        // pre-walk)? A field is walkable iff it is a shared child, an
        // `Option[shared]`, or a Vec/String/Map/Set.
        let field_is_walkable = |slf: &Self, te: &TypeExpr| -> bool {
            if slf.option_inner_shared_type_for_type_expr(te).is_some() {
                return true;
            }
            if let TypeKind::Path(p) = &te.kind {
                if let Some(seg) = p.segments.last() {
                    if slf.shared_types.contains_key(seg.as_str()) {
                        return true;
                    }
                    if matches!(
                        seg.as_str(),
                        // Both String spellings (inferred tes spell `str`).
                        "Vec"
                            | "VecDeque"
                            | "String"
                            | "str"
                            | "Map"
                            | "HashMap"
                            | "Set"
                            | "HashSet"
                    ) {
                        return true;
                    }
                    // B-2026-06-14-28 — a plain struct payload that owns
                    // shared fields (`Add(BinOp)`, `BinOp { left: Expr }`):
                    // its inline RC children need dec'ing at box drop. Also a
                    // struct that owns a String/Vec/heap field with NO shared
                    // edge (`Ident(IdentExpr { name: String })`): the box owns
                    // that buffer and must free it (`type_expr_has_drop_heap`),
                    // or the payload's String leaks when the box drops. The
                    // walk site (`emit_shared_enum_field_drop`'s struct branch)
                    // mirrors this same union.
                    if slf.struct_types.contains_key(seg.as_str())
                        && !slf.shared_types.contains_key(seg.as_str())
                        && (slf.struct_owns_shared_field(seg.as_str(), &mut Vec::new())
                            || slf.type_expr_has_drop_heap(te)
                            // B-2026-07-11-39 — `type_expr_has_drop_heap` has a
                            // DELIBERATE `Option | Result => false` blind spot
                            // (its copy-side callers rely on it), so a payload
                            // struct whose only heap is an `Option[String]` /
                            // `Option[Vec[T]]` field read as heapless here: the
                            // whole variant was judged non-walkable, got no drop
                            // block, and its boxed payload + Some payload leaked
                            // on rc-drop. `te_owns_option_heap_payload` is the
                            // Option-aware companion; OR it in so the variant is
                            // walked and the walker's new Option arm frees it.
                            || slf.te_owns_option_heap_payload(te))
                    {
                        return true;
                    }
                }
            }
            false
        };
        let any_walkable = variants
            .iter()
            .any(|(_, tys)| tys.iter().any(|te| field_is_walkable(self, te)));
        let has_user_drop = self
            .program_snapshot
            .as_ref()
            .is_some_and(|p| p.drop_method_keys.contains_key(enum_name));
        if !any_walkable && !has_user_drop {
            self.rc_drop_fns.insert(enum_name.to_string(), None);
            return None;
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let void_ty = self.context.void_type();

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let fn_name = format!("__karac_rc_drop_{enum_name}");
        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));
        // Cache BEFORE the body so a self-recursive payload (`Bin(Expr, Expr)`)
        // resolves through `emit_rc_dec`'s `rc_drop_fns` lookup to this fn.
        self.rc_drop_fns
            .insert(enum_name.to_string(), Some(drop_fn));
        self.current_fn = Some(drop_fn);

        // Pre-pass: ensure sibling shared types referenced in payloads have
        // their drop fns synthesized (self-reference is already cached above).
        let sibling_names: Vec<String> = variants
            .iter()
            .flat_map(|(_, tys)| tys.iter())
            .filter_map(|te| {
                self.option_inner_shared_type_for_type_expr(te)
                    .map(|(_, i)| i.heap_type)
                    .or_else(|| {
                        if let TypeKind::Path(p) = &te.kind {
                            p.segments
                                .last()
                                .and_then(|s| self.shared_types.get(s.as_str()))
                                .map(|i| i.heap_type)
                        } else {
                            None
                        }
                    })
            })
            .filter_map(|ht| self.struct_name_for_heap_type(ht))
            .filter(|n| n != enum_name && !self.rc_drop_fns.contains_key(n))
            .collect();
        for n in sibling_names {
            let is_enum = self
                .shared_types
                .get(&n)
                .map(|i| i.is_enum)
                .unwrap_or(false);
            if is_enum {
                let _ = self.emit_shared_enum_rc_drop_fn(&n);
            } else {
                let _ = self.emit_shared_struct_rc_drop_fn(&n);
            }
        }

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let p_arg = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        // User `impl Drop` body runs first (refcount already 0 here), mirroring
        // the struct path's self-slot ABI.
        if has_user_drop {
            if let Some(user_drop_fn) = self.module.get_function(&format!("{enum_name}.drop")) {
                let self_slot =
                    self.create_entry_alloca(drop_fn, "rcedrop.self.slot", ptr_ty.into());
                self.builder.build_store(self_slot, p_arg).unwrap();
                self.builder
                    .build_call(user_drop_fn, &[self_slot.into()], "")
                    .unwrap();
            }
        }

        let free_bb = self.context.append_basic_block(drop_fn, "rcedrop.free");

        // Variants with no walkable payload fall to the switch default (the
        // free block); each walkable variant gets a dedicated block.
        let tag_ptr = self
            .builder
            .build_struct_gep(heap_type, p_arg, 1, "rcedrop.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "rcedrop.tag")
            .unwrap()
            .into_int_value();
        let mut cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        for (vname, tys) in &variants {
            if !tys.iter().any(|te| field_is_walkable(self, te)) {
                continue;
            }
            let Some(&tagv) = layout.tags.get(vname) else {
                continue;
            };
            let offsets = layout
                .field_word_offsets
                .get(vname)
                .cloned()
                .unwrap_or_default();
            let vbb = self
                .context
                .append_basic_block(drop_fn, &format!("rcedrop.v.{vname}"));
            self.builder.position_at_end(vbb);
            for (i, te) in tys.iter().enumerate() {
                let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
                // +2: skip the `{rc, tag}` prefix in the heap box.
                self.emit_shared_enum_field_drop(
                    heap_type,
                    p_arg,
                    start_word + 2,
                    num_words,
                    te,
                    &format!("rcedrop.{vname}.f{i}"),
                );
            }
            self.builder.build_unconditional_branch(free_bb).unwrap();
            cases.push((i64_t.const_int(tagv, false), vbb));
        }

        // The tag load is the last instruction in `entry_bb` (no terminator
        // yet); append the switch there as its terminator.
        self.builder.position_at_end(entry_bb);
        self.builder.build_switch(tag, free_bb, &cases).unwrap();

        self.builder.position_at_end(free_bb);
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

        // `Semaphore.drop` / `RateLimiter.drop` — same single-owner
        // handle-free shape as `BoundedChannel.drop` (load `self.handle_id`,
        // `inttoptr`, call the runtime `_drop`). The stdlib declares
        // `impl Drop` for both, so `drop_method_keys` carries their names when
        // the prelude is in scope. (`src/codegen/backpressure.rs`.)
        if program.drop_method_keys.contains_key("Semaphore") {
            self.emit_handle_drop_body("Semaphore", "karac_runtime_semaphore_drop");
        }
        if program.drop_method_keys.contains_key("RateLimiter") {
            self.emit_handle_drop_body("RateLimiter", "karac_runtime_rate_limiter_drop");
        }

        // `Pool.drop` — single-owner handle-free (same shape as Semaphore).
        // `PooledConnection.drop` — the drop-releases-automatically contract:
        // return the checked-out slot to its source pool at scope exit. Both
        // land when `std.Pool`'s prelude `impl Drop`s are in scope
        // (`src/codegen/pool.rs`).
        if program.drop_method_keys.contains_key("Pool") {
            self.emit_handle_drop_body("Pool", "karac_runtime_pool_drop");
        }
        if program.drop_method_keys.contains_key("PooledConnection") {
            self.emit_pooled_connection_drop_body();
        }

        // `CriticalSectionGuard.drop` (design.md § Critical sections). Loads
        // `self.restore_token` (`{i64}` field 0) and calls
        // `karac_critical_section_release(token)` to re-enable interrupts to
        // their prior mask state. The stdlib declares `impl Drop for
        // CriticalSectionGuard` so `drop_method_keys` contains the type when
        // the prelude is in scope.
        if program
            .drop_method_keys
            .contains_key("CriticalSectionGuard")
        {
            self.emit_critical_section_drop_body();
        }
    }

    /// Hand-roll `@CriticalSectionGuard.drop(ptr) -> void`. Body:
    ///
    /// ```text
    /// %tok_ptr = getelementptr {i64}, ptr %self, i32 0, i32 0
    /// %tok     = load i64, ptr %tok_ptr
    /// call void @karac_critical_section_release(i64 %tok)
    /// ret void
    /// ```
    ///
    /// Mirrors `emit_tcp_drop_body_for` (single-`i64`-field struct → load the
    /// word, hand it to a runtime call), but calls `release` rather than a
    /// close.
    fn emit_critical_section_drop_body(&mut self) {
        let fn_name = "CriticalSectionGuard.drop";
        if self.module.get_function(fn_name).is_some() {
            return;
        }
        let release_fn = match self.module.get_function("karac_critical_section_release") {
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
        let tok_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 0, "restore_token_ptr")
            .unwrap();
        let token = self
            .builder
            .build_load(i64_ty, tok_ptr, "restore_token")
            .unwrap()
            .into_int_value();
        self.builder
            .build_call(release_fn, &[token.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
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

    /// Hand-roll `@<TypeName>.drop(ptr) -> void` for a single-owner
    /// `{ handle_id: i64 }` handle type: load field 0, `inttoptr`, call
    /// `free_fn`. Generalizes `emit_bounded_channel_drop_body`; used by the
    /// `Semaphore` / `RateLimiter` backpressure primitives.
    fn emit_handle_drop_body(&mut self, type_name: &str, free_fn_name: &str) {
        let fn_name = format!("{type_name}.drop");
        if self.module.get_function(&fn_name).is_some() {
            return;
        }
        let free_fn = match self.module.get_function(free_fn_name) {
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
            .add_function(&fn_name, drop_fn_ty, Some(Linkage::Internal));

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
        let obj_ptr = self
            .builder
            .build_int_to_ptr(handle, ptr_ty, "obj_ptr")
            .unwrap();
        self.builder
            .build_call(free_fn, &[obj_ptr.into()], "")
            .unwrap();
        self.builder.build_return(None).unwrap();

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Hand-roll `@PooledConnection.drop(ptr self) -> void` — the
    /// drop-releases-automatically contract. `self` points to a
    /// `PooledConnection { i64 pool_handle_id, i64 conn_id, T val }`; the drop
    /// loads the pool handle (field 0) + `conn_id` (field 1) and calls
    /// `karac_runtime_pool_release(pool, conn_id, &val)`. The runtime already
    /// knows `elem_size`, so the drop needs only the POINTER to `val` — it never
    /// touches `T`'s layout, so ONE body serves every monomorph. The `val`
    /// pointer is field 2, at byte offset 16 (after two i64s), aligned for any
    /// POD `T` (v1 scope). Release is idempotent on `conn_id`, so an explicit
    /// `pool.release(conn)` followed by this scope-exit drop hands the slot back
    /// exactly once. A null pool handle (a hand-rolled `PooledConnection`) is a
    /// runtime no-op.
    fn emit_pooled_connection_drop_body(&mut self) {
        let fn_name = "PooledConnection.drop";
        if self.module.get_function(fn_name).is_some() {
            return;
        }
        let release_fn = match self.module.get_function("karac_runtime_pool_release") {
            Some(f) => f,
            None => return,
        };

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let void_ty = self.context.void_type();
        // Prefix type `{i64 pool_handle_id, i64 conn_id}` for the two scalar
        // header fields; `val` follows at byte offset 16.
        let hdr_ty = self
            .context
            .struct_type(&[i64_ty.into(), i64_ty.into()], false);

        let saved_bb = self.builder.get_insert_block();
        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self
            .module
            .add_function(fn_name, drop_fn_ty, Some(Linkage::Internal));
        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);

        let self_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let h_ptr = self
            .builder
            .build_struct_gep(hdr_ty, self_ptr, 0, "pc.h.ptr")
            .unwrap();
        let pool_handle = self
            .builder
            .build_load(i64_ty, h_ptr, "pc.h")
            .unwrap()
            .into_int_value();
        let c_ptr = self
            .builder
            .build_struct_gep(hdr_ty, self_ptr, 1, "pc.c.ptr")
            .unwrap();
        let conn_id = self
            .builder
            .build_load(i64_ty, c_ptr, "pc.c")
            .unwrap()
            .into_int_value();
        let pool_ptr = self
            .builder
            .build_int_to_ptr(pool_handle, ptr_ty, "pc.pool")
            .unwrap();
        // val pointer = self + 16 bytes (T-agnostic; runtime knows elem_size).
        let val_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_ty,
                    self_ptr,
                    &[i64_ty.const_int(16, false)],
                    "pc.val.ptr",
                )
                .unwrap()
        };
        self.builder
            .build_call(
                release_fn,
                &[pool_ptr.into(), conn_id.into(), val_ptr.into()],
                "",
            )
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
        // i64 fd ABI: the socket struct's fd field is i64; load it as i64
        // and hand it to the now-i64 `karac_runtime_tcp_close`.
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();
        let struct_ty = self.context.struct_type(&[i64_ty.into()], false);

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
            .build_load(i64_ty, fd_ptr, "fd")
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
        // i64 fd ABI: load the i64 fd field and pass it to the now-i64
        // `karac_runtime_tls_close`.
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
        let fd_ptr = self
            .builder
            .build_struct_gep(struct_ty, self_ptr, 0, "fd_ptr")
            .unwrap();
        let fd = self
            .builder
            .build_load(i64_ty, fd_ptr, "fd")
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
        // i64 fd ABI: `{ i64 fd, ptr config }`.
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();
        let struct_ty = self
            .context
            .struct_type(&[i64_ty.into(), ptr_ty.into()], false);

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
            .build_load(i64_ty, fd_field_ptr, "fd")
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
