//! Match-expression codegen + pattern-condition lowering.
//!
//! Houses `compile_match` (the entry: lowers `match scrutinee { arms... }`
//! to a chain of conditional branches via `compile_pattern_condition`)
//! plus the supporting machinery:
//!
//! - `scrutinee_is_borrow_call` — receiver-borrow recognizer
//! - `compile_pattern_condition` — per-arm pattern→bool lowering
//! - `extract_enum_tag` — load the discriminant from a tagged-enum value
//! - `enum_tag_for_variant` / `enum_type_for_variant` — variant
//!   metadata lookups
//! - `pattern_payload_word_count` / `pattern_payload_llvm_type` —
//!   per-pattern payload shape
//! - `reconstruct_payload_value` — rebuild the variant payload tuple
//!   from a pre-decomposed bit-cast
//!
//! Lives in a sibling `impl<'ctx> super::Codegen<'ctx>` block.

use crate::ast::*;

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

impl<'ctx> super::Codegen<'ctx> {
    // ── Match ─────────────────────────────────────────────────────

    pub(super) fn compile_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Slice 3b: when the scrutinee is a ref-typed identifier
        // (function parameter `f: ref T` / `mut ref T`), obtain the raw
        // scrutinee pointer in addition to the auto-derefed value.
        // Pattern conditions still run against the value (tag/field
        // checks are identical); leaf bindings under recognized
        // pattern shapes can then route through
        // `bind_pattern_values_via_ptr` to emit GEP-based shims that
        // alias the scrutinee storage rather than a local copy — which
        // is what makes `mut ref` write-through propagate back to the
        // caller's storage.
        let scrut_ref_ptr: Option<(PointerValue<'ctx>, StructType<'ctx>)> =
            if let ExprKind::Identifier(name) = &scrutinee.kind {
                if self.ref_params.contains_key(name) {
                    let pointee = *self.ref_params.get(name).unwrap();
                    if let BasicTypeEnum::StructType(st) = pointee {
                        self.get_data_ptr(name).map(|p| (p, st))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
        let scrut = self.compile_expr(scrutinee)?;
        // Detect borrow-returning scrutinees so pattern bindings don't
        // register a `FreeVecBuffer` against a buffer the container still
        // owns. `Map.get` is the canonical case (the returned `Option[V]`
        // aliases the bucket entry's value words); a duplicate cleanup
        // would double-free against the `karac_map_free_with_val_drop_vec`
        // path at function exit.
        let saved_borrow_flag = self.pattern_binding_is_borrow;
        self.pattern_binding_is_borrow = Self::scrutinee_is_borrow_call(scrutinee);
        let fn_val = self.current_fn.unwrap();
        let merge_bb = self.context.append_basic_block(fn_val, "match.merge");

        let mut arm_results: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            Vec::new();

        let mut next_bb = self.context.append_basic_block(fn_val, "match.arm0");
        self.builder.build_unconditional_branch(next_bb).unwrap();

        for (i, arm) in arms.iter().enumerate() {
            let arm_bb = next_bb;
            // Always create a fresh fail_bb — never reuse merge_bb directly.
            // If the last pattern condition is false (non-exhaustive match or
            // missed case), we emit `unreachable` to satisfy LLVM's requirement
            // that every basic block has a terminator and every phi predecessor
            // is accounted for.
            let is_last = i + 1 == arms.len();
            let fail_bb = if !is_last {
                self.context
                    .append_basic_block(fn_val, &format!("match.arm{}", i + 1))
            } else {
                self.context.append_basic_block(fn_val, "match.nofall")
            };
            next_bb = fail_bb;

            self.builder.position_at_end(arm_bb);

            // Slice arms route through the SliceSource-driven helper —
            // the generic `compile_pattern_condition` Slice fall-through
            // would always-match and clobber length dispatch.
            let cond = if let PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } = &arm.pattern.kind
            {
                let src = self.resolve_slice_source(scrutinee).ok_or_else(|| {
                    "slice pattern requires an identifier scrutinee resolvable to Array/Vec/Slice"
                        .to_string()
                })?;
                self.compile_slice_pattern_condition(prefix, rest, suffix, &src)?
            } else {
                self.compile_pattern_condition(&arm.pattern, scrut)?
            };

            let body_bb = self
                .context
                .append_basic_block(fn_val, &format!("match.body{}", i));

            self.builder
                .build_conditional_branch(cond.into_int_value(), body_bb, fail_bb)
                .unwrap();

            self.builder.position_at_end(body_bb);

            // Per-arm scope frame: cleanups registered during this arm's
            // pattern binding + body compilation fire at end-of-arm rather
            // than end-of-function. Closes the 2026-05-13 alloca-reuse leak
            // for loop-driven match arms (e.g. `while ... { match bucket
            // .remove(k) { Some(indices) => ... } }` — `indices`'s alloca
            // is hoisted to entry and reused N times, but only the last
            // value's cleanup fired at fn-end; the other N-1 leaked).
            // Frame is popped either by `drain_top_frame_with_emit` (the
            // fall-through-to-merge path below) or `scope_cleanup_actions
            // .pop()` (the early-return path, where the return's own
            // `emit_scope_cleanup` already walked the full stack including
            // this frame and emitted cleanup for its actions).
            self.scope_cleanup_actions.push(Vec::new());

            // Bind pattern variables
            if let PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } = &arm.pattern.kind
            {
                let src = self.resolve_slice_source(scrutinee).ok_or_else(|| {
                    "slice pattern requires an identifier scrutinee resolvable to Array/Vec/Slice"
                        .to_string()
                })?;
                self.bind_slice_pattern(prefix, rest, suffix, &src, true)?;
            } else {
                // Slice 3b: try the pointer-source binding path first
                // when we have a ref-scrutinee. If the pattern shape
                // isn't recognized by `bind_pattern_values_via_ptr`
                // (e.g., or-patterns, at-bindings, slice patterns,
                // multi-word payloads), fall back to slice 3a's
                // value-source + ref-shim path which still produces
                // correct (though copy-aliased) bindings.
                let handled_via_ptr = if let Some((scrut_ptr, pointee_ty)) = scrut_ref_ptr {
                    self.bind_pattern_values_via_ptr(&arm.pattern, scrut_ptr, pointee_ty)?
                        .is_some()
                } else {
                    false
                };
                if !handled_via_ptr {
                    self.bind_pattern_values(&arm.pattern, scrut)?;
                }
            }

            let arm_val = self.compile_expr(&arm.body)?;
            let arm_body_end = self.builder.get_insert_block().unwrap();
            if arm_body_end.get_terminator().is_none() {
                // Move-aware: if the arm's tail expression is an
                // Identifier for a tracked Vec / String, the value is
                // being moved into the match's result (caller now owns
                // the buffer). Zero the source's `cap` so the per-arm
                // cleanup's `cap > 0` guard skips, preventing double-free
                // (analogous to `suppress_cleanup_for_tail_return` for
                // function-level Vec returns). Identifier match-arm
                // tail-return is the canonical Option-unwrap shape
                // `match opt { Some(v) => v, None => default() }`.
                self.suppress_source_vec_cleanup_for_arg(&arm.body);
                self.drain_top_frame_with_emit();
                // Re-read the current bb AFTER drain — the cleanup IR
                // may have appended new basic blocks (e.g. `cleanup.free`
                // / `cleanup.skip` for FreeVecBuffer's `cap > 0` guard),
                // so the merge-predecessor is the drain's exit bb, NOT
                // `arm_body_end`. The PHI at `merge_bb` must list the
                // ACTUAL predecessor bb where the unconditional branch
                // to merge originates from, or LLVM module verification
                // fails with "PHI node entries do not match predecessors".
                let merge_pred = self.builder.get_insert_block().unwrap();
                arm_results.push((arm_val, merge_pred));
                self.builder.build_unconditional_branch(merge_bb).unwrap();
            } else {
                // Early-return / terminator inside arm body: the return
                // path's own `emit_scope_cleanup` walked the entire stack
                // including this per-arm frame and emitted cleanup for
                // its actions before the return. Pop the now-spent frame
                // so it doesn't shadow subsequent arms' bindings.
                self.scope_cleanup_actions.pop();
            }
        }

        // Terminate the last fail_bb (match.nofall) — exhaustive matches never
        // reach here; emit `unreachable` so LLVM doesn't require a phi entry.
        self.builder.position_at_end(next_bb);
        if next_bb.get_terminator().is_none() {
            self.builder.build_unreachable().unwrap();
        }

        self.builder.position_at_end(merge_bb);
        self.pattern_binding_is_borrow = saved_borrow_flag;

        // Build phi if all arms produce a value of the same type
        if !arm_results.is_empty() {
            let first_ty = arm_results[0].0.get_type();
            if arm_results.iter().all(|(v, _)| v.get_type() == first_ty) {
                let phi = self.builder.build_phi(first_ty, "matchval").unwrap();
                for (val, bb) in &arm_results {
                    phi.add_incoming(&[(val, *bb)]);
                }
                return Ok(phi.as_basic_value());
            }
        }

        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// True when a match scrutinee expression's value aliases a container
    /// the surrounding scope still owns — and so the cleanup actions
    /// attached to that container will free any heap-bearing payload words
    /// embedded in the scrutinee's value. In those cases, a pattern
    /// binding extracted from the scrutinee must NOT itself register a
    /// cleanup, or the buffer will be freed twice.
    ///
    /// Current closed list (returns by value, container retains
    /// ownership): `Map.get`. Other shape candidates (`Vec.first`,
    /// `Vec.last`, `Slice.get`, ...) are followups — they return one-word
    /// scalar payloads in the v1 stdlib, not heap-bearing Vec/String, so
    /// their match-arm bindings don't trigger the duplicate cleanup yet.
    /// `Map.remove` truly transfers ownership (the entry is deleted) and
    /// is intentionally NOT on this list — its `Some(v)` bindings still
    /// own the Vec they receive.
    pub(super) fn scrutinee_is_borrow_call(scrutinee: &Expr) -> bool {
        if let ExprKind::MethodCall { method, .. } = &scrutinee.kind {
            return method == "get";
        }
        false
    }

    /// Returns an i1 (bool) value: 1 if the scrutinee matches the pattern.
    pub(super) fn compile_pattern_condition(
        &mut self,
        pattern: &Pattern,
        scrut: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let tru = self.context.bool_type().const_int(1, false);
        match &pattern.kind {
            PatternKind::Wildcard => Ok(tru.into()),
            PatternKind::Binding(name) => {
                // Check if this binding name is actually a unit enum variant.
                // The parser produces Binding("Color.Red") or Binding("Red") for
                // unit variants in match arms; detect and compare tags.
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                if let Some(tag) = self.enum_tag_for_variant(variant_name) {
                    let actual_tag = self.extract_enum_tag(scrut, variant_name)?;
                    let expected_tag = self.context.i64_type().const_int(tag, false);
                    return Ok(self
                        .builder
                        .build_int_compare(IntPredicate::EQ, actual_tag, expected_tag, "tag_eq")
                        .unwrap()
                        .into());
                }
                // Not a variant — true binding, always matches.
                Ok(tru.into())
            }
            PatternKind::Literal(lit) => {
                let lit_val = match lit {
                    LiteralPattern::Integer(n, sfx) => self.const_int_for_suffix(*n, *sfx).into(),
                    LiteralPattern::Bool(b) => self
                        .context
                        .bool_type()
                        .const_int(u64::from(*b), false)
                        .into(),
                    LiteralPattern::Float(f, sfx) => self.const_float_for_suffix(*f, *sfx).into(),
                    LiteralPattern::Char(c) => {
                        self.context.i32_type().const_int(*c as u64, false).into()
                    }
                    LiteralPattern::String(s) => self
                        .builder
                        .build_global_string_ptr(s, "spat")
                        .unwrap()
                        .as_pointer_value()
                        .into(),
                };
                self.compile_binop(&BinOp::Eq, scrut, lit_val)
            }
            PatternKind::Or(pats) => {
                let mut result: BasicValueEnum<'ctx> =
                    self.context.bool_type().const_int(0, false).into();
                for p in pats {
                    let cond = self.compile_pattern_condition(p, scrut)?;
                    result = self
                        .builder
                        .build_or(result.into_int_value(), cond.into_int_value(), "orcond")
                        .unwrap()
                        .into();
                }
                Ok(result)
            }
            // Tuple enum variant: check tag matches
            PatternKind::TupleVariant { path, .. } => {
                let variant_name = path.last().map(|s| s.as_str()).unwrap_or("");
                if let Some(tag) = self.enum_tag_for_variant(variant_name) {
                    let actual_tag = self.extract_enum_tag(scrut, variant_name)?;
                    let expected_tag = self.context.i64_type().const_int(tag, false);
                    return Ok(self
                        .builder
                        .build_int_compare(IntPredicate::EQ, actual_tag, expected_tag, "tag_eq")
                        .unwrap()
                        .into());
                }
                Ok(tru.into())
            }
            // Struct enum variant: check tag matches
            PatternKind::Struct { path, .. }
                if path.len() > 1
                    || self
                        .enum_tag_for_variant(path.last().map(|s| s.as_str()).unwrap_or(""))
                        .is_some() =>
            {
                let variant_name = path.last().map(|s| s.as_str()).unwrap_or("");
                if let Some(tag) = self.enum_tag_for_variant(variant_name) {
                    let actual_tag = self.extract_enum_tag(scrut, variant_name)?;
                    let expected_tag = self.context.i64_type().const_int(tag, false);
                    return Ok(self
                        .builder
                        .build_int_compare(IntPredicate::EQ, actual_tag, expected_tag, "tag_eq")
                        .unwrap()
                        .into());
                }
                Ok(tru.into())
            }
            // Plain struct pattern or anything else — always matches
            _ => Ok(tru.into()),
        }
    }

    /// Extract the tag integer from an enum scrutinee.
    /// Handles both shared enums (pointer — GEP to tag at index 1) and
    /// non-shared enums (struct value — extractvalue at index 0).
    pub(super) fn extract_enum_tag(
        &self,
        scrut: BasicValueEnum<'ctx>,
        variant_name: &str,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        // Check if this variant belongs to a shared enum.
        if let BasicValueEnum::PointerValue(ptr) = scrut {
            for (enum_name, layout) in &self.enum_layouts {
                if layout.tags.contains_key(variant_name) {
                    if let Some(info) = self.shared_types.get(enum_name) {
                        // Shared enum: tag is at heap index 1.
                        let tag_ptr = self
                            .builder
                            .build_struct_gep(info.heap_type, ptr, 1, "sh_tag_ptr")
                            .unwrap();
                        let tag = self
                            .builder
                            .build_load(i64_t, tag_ptr, "actual_tag")
                            .unwrap()
                            .into_int_value();
                        return Ok(tag);
                    }
                }
            }
        }
        // Non-shared enum: extractvalue at index 0.
        if let BasicValueEnum::StructValue(sv) = scrut {
            let tag = self
                .builder
                .build_extract_value(sv, 0, "actual_tag")
                .unwrap()
                .into_int_value();
            return Ok(tag);
        }
        Ok(i64_t.const_int(0, false))
    }

    /// Find the discriminant tag for a variant name across all registered enums.
    pub(super) fn enum_tag_for_variant(&self, variant_name: &str) -> Option<u64> {
        for layout in self.enum_layouts.values() {
            if let Some(&tag) = layout.tags.get(variant_name) {
                return Some(tag);
            }
        }
        None
    }

    /// Find the LLVM struct type for the enum containing a given variant.
    #[allow(dead_code)]
    pub(super) fn enum_type_for_variant(&self, variant_name: &str) -> Option<StructType<'ctx>> {
        for layout in self.enum_layouts.values() {
            if layout.tags.contains_key(variant_name) {
                return Some(layout.llvm_type);
            }
        }
        None
    }

    /// Compound-payload enum codegen (tuple-destructure helper) —
    /// per-element word count for a destructure sub-pattern. Mirrors
    /// the construction-side `payload_word_count_for_type_expr` shape
    /// but reads typechecker-recorded surface names (`pattern_binding_types`)
    /// off the pattern instead of source-level `TypeExpr`. Used by the
    /// Tuple arm in `reconstruct_payload_value` to slice the variant's
    /// flat payload-word vector into per-element ranges.
    ///
    /// - Vec / String → 3 words (vec struct shape)
    /// - Slice → 2 words (slice struct shape)
    /// - Registered user struct → its LLVM word count
    /// - Tuple sub-pattern → recursive sum
    /// - Primitive binding / wildcard / unknown → 1 word
    pub(super) fn pattern_payload_word_count(&self, pat: &Pattern) -> usize {
        match &pat.kind {
            PatternKind::Tuple(elems) => elems
                .iter()
                .map(|p| self.pattern_payload_word_count(p))
                .sum(),
            PatternKind::Binding(_) => {
                let key = (pat.span.offset, pat.span.length);
                // Tuple-typed bindings (e.g. `Some(node)` where node is
                // `(i64, i64)`) — sum element widths from the recorded
                // tuple `TypeExpr` so multi-word payloads reconstitute
                // as the right-shaped tuple struct.
                if matches!(
                    self.pattern_binding_types.get(&key).map(|s| s.as_str()),
                    Some("Tuple")
                ) {
                    if let Some(te) = self.pattern_binding_inner_types.get(&key) {
                        if let TypeKind::Tuple(elems) = &te.kind {
                            return elems
                                .iter()
                                .map(|el| {
                                    Self::llvm_type_word_count(self.llvm_type_for_type_expr(el))
                                })
                                .sum::<usize>()
                                .max(1);
                        }
                    }
                }
                match self.pattern_binding_types.get(&key).map(|s| s.as_str()) {
                    Some("Vec") | Some("String") => 3,
                    Some("Slice") => 2,
                    Some(name) => self
                        .struct_types
                        .get(name)
                        .map(|st| Self::llvm_type_word_count((*st).into()))
                        .unwrap_or(1),
                    None => 1,
                }
            }
            _ => 1,
        }
    }

    /// Compound-payload enum codegen (tuple-destructure helper) —
    /// LLVM type for a destructure sub-pattern's reconstructed value.
    /// Used by the Tuple arm in `reconstruct_payload_value` to build
    /// the surrounding tuple struct type whose fields hold each
    /// element's reconstructed aggregate.
    pub(super) fn pattern_payload_llvm_type(&self, pat: &Pattern) -> BasicTypeEnum<'ctx> {
        match &pat.kind {
            PatternKind::Tuple(elems) => {
                let elem_tys: Vec<BasicTypeEnum<'ctx>> = elems
                    .iter()
                    .map(|p| self.pattern_payload_llvm_type(p))
                    .collect();
                self.context.struct_type(&elem_tys, false).into()
            }
            PatternKind::Binding(_) => {
                let key = (pat.span.offset, pat.span.length);
                // Tuple-typed binding: lower the recorded tuple
                // `TypeExpr` to its LLVM struct type so the
                // reconstruction builds a value with the right shape
                // for downstream `let (a, b) = node` destructure.
                if matches!(
                    self.pattern_binding_types.get(&key).map(|s| s.as_str()),
                    Some("Tuple")
                ) {
                    if let Some(te) = self.pattern_binding_inner_types.get(&key) {
                        if matches!(te.kind, TypeKind::Tuple(_)) {
                            return self.llvm_type_for_type_expr(te);
                        }
                    }
                }
                match self.pattern_binding_types.get(&key).map(|s| s.as_str()) {
                    Some("Vec") | Some("String") => self.vec_struct_type().into(),
                    Some("Slice") => self.slice_struct_type().into(),
                    Some(name) => self
                        .struct_types
                        .get(name)
                        .map(|st| (*st).into())
                        .unwrap_or_else(|| self.context.i64_type().into()),
                    None => self.context.i64_type().into(),
                }
            }
            _ => self.context.i64_type().into(),
        }
    }

    /// Compound-payload enum codegen (CP4 destructure side helper) —
    /// reconstruct an aggregate `BasicValueEnum` from a sequence of i64
    /// payload words loaded from a variant's payload area. Single-word
    /// fields short-circuit to the legacy single-i64 binding (the
    /// pattern's `Binding` arm already handles struct-payload
    /// reconstitution). Multi-word fields look up the binding's
    /// recorded type via `pattern_binding_types` (set by the
    /// typechecker's `check_pattern_against`) and use the matching LLVM
    /// type to reassemble: 3-word `String` / `Vec[T]` rebuild as
    /// `vec_struct_type` (`{ ptr, i64, i64 }`); 2-word `Slice[T]`
    /// rebuild as `slice_struct_type`; user struct fields rebuild as
    /// the registered LLVM struct type. Tuple sub-patterns dispatch
    /// through a per-element walk that uses `pattern_payload_word_count`
    /// to slice `field_words` and recurses for nested tuples.
    pub(super) fn reconstruct_payload_value(
        &self,
        sub_pat: &Pattern,
        field_words: &[inkwell::values::IntValue<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Tuple sub-pattern: walk per-element, reconstruct each into its
        // own LLVM aggregate (or single word for primitive elements),
        // then pack into a tuple struct value. The element word counts
        // come from `pattern_payload_word_count` which mirrors the
        // construction-side `payload_word_count_for_type_expr` logic on
        // pattern shape (Vec/String=3, Slice=2, struct=struct-fields,
        // primitive/wildcard=1; tuple=sum). Recursive on nested tuples.
        if let PatternKind::Tuple(elems) = &sub_pat.kind {
            let elem_tys: Vec<BasicTypeEnum<'ctx>> = elems
                .iter()
                .map(|p| self.pattern_payload_llvm_type(p))
                .collect();
            let tuple_ty = self.context.struct_type(&elem_tys, false);
            let mut agg = tuple_ty.get_undef();
            let mut cursor = 0usize;
            for (i, sub) in elems.iter().enumerate() {
                let n = self.pattern_payload_word_count(sub);
                let end = (cursor + n).min(field_words.len());
                let slice = &field_words[cursor..end];
                let elem_val = self.reconstruct_payload_value(sub, slice)?;
                agg = self
                    .builder
                    .build_insert_value(agg, elem_val, i as u32, "tup.iv")
                    .unwrap()
                    .into_struct_value();
                cursor = end;
            }
            return Ok(agg.into());
        }
        // Single-word: keep legacy single-i64 binding shape. The
        // PatternKind::Binding arm handles single-field struct
        // reconstitution downstream via `pattern_binding_types`.
        // Gate on the BINDING's natural width (not the slice length)
        // so widened variant payloads (e.g. the seeded `Option[T]`
        // bumped to 3 i64 payload words to fit tuple/Vec/String
        // payloads from `Vec.pop` / `VecDeque.pop_*`) don't force
        // primitive bindings through the multi-word reconstruction
        // path. The slice may legitimately carry more words than the
        // binding consumes — trailing words are undef.
        let want_words = self.pattern_payload_word_count(sub_pat);
        if want_words <= 1 || field_words.len() <= 1 {
            let w = field_words
                .first()
                .copied()
                .unwrap_or_else(|| i64_t.const_int(0, false));
            return Ok(w.into());
        }
        // Tuple-typed binding (e.g. `Some(node)` where node: (i64, i64)):
        // walk per-element from the recorded tuple `TypeExpr` and pack
        // into the tuple struct value. Mirrors the Tuple sub-pattern
        // branch above but reads element types from the typechecker
        // side-table instead of sub-pattern shapes.
        let key = (sub_pat.span.offset, sub_pat.span.length);
        if matches!(
            self.pattern_binding_types.get(&key).map(|s| s.as_str()),
            Some("Tuple")
        ) {
            if let Some(te) = self.pattern_binding_inner_types.get(&key) {
                if let TypeKind::Tuple(elem_tes) = &te.kind {
                    let elem_llvm_tys: Vec<BasicTypeEnum<'ctx>> = elem_tes
                        .iter()
                        .map(|et| self.llvm_type_for_type_expr(et))
                        .collect();
                    let tuple_ty = self.context.struct_type(&elem_llvm_tys, false);
                    let mut agg = tuple_ty.get_undef();
                    let mut cursor = 0usize;
                    for (i, elem_ty) in elem_llvm_tys.iter().enumerate() {
                        let n = Self::llvm_type_word_count(*elem_ty).max(1);
                        let end = (cursor + n).min(field_words.len());
                        let slice = &field_words[cursor..end];
                        // Primitive single-word elements coerce the
                        // word back to the declared LLVM type (int/bool
                        // bit-cast); multi-word elements aren't expected
                        // here but fall back to the first word as a
                        // safety net.
                        let raw = slice
                            .first()
                            .copied()
                            .unwrap_or_else(|| i64_t.const_int(0, false));
                        let elem_val: BasicValueEnum<'ctx> = match *elem_ty {
                            BasicTypeEnum::IntType(it) if it.get_bit_width() != 64 => self
                                .builder
                                .build_int_truncate(raw, it, "tup.elem.tr")
                                .unwrap()
                                .into(),
                            BasicTypeEnum::IntType(_) => raw.into(),
                            _ => raw.into(),
                        };
                        agg = self
                            .builder
                            .build_insert_value(agg, elem_val, i as u32, "tup.bind.iv")
                            .unwrap()
                            .into_struct_value();
                        cursor = end;
                    }
                    return Ok(agg.into());
                }
            }
        }
        // Multi-word: resolve the binding's surface type to choose the
        // target LLVM aggregate type.
        let type_name = self.pattern_binding_types.get(&key).cloned();
        let target_ty: Option<BasicTypeEnum<'ctx>> =
            type_name.as_ref().and_then(|n| match n.as_str() {
                "String" | "str" | "Vec" => Some(self.vec_struct_type().into()),
                "Slice" => Some(self.slice_struct_type().into()),
                _ => self.struct_types.get(n.as_str()).map(|st| (*st).into()),
            });
        // Heuristic fallback when the typechecker didn't record a name:
        // 3 words → vec/string shape; 2 words → slice shape.
        let target_ty: BasicTypeEnum<'ctx> = target_ty.unwrap_or_else(|| match field_words.len() {
            3 => self.vec_struct_type().into(),
            2 => self.slice_struct_type().into(),
            _ => self.vec_struct_type().into(),
        });
        let st = match target_ty {
            BasicTypeEnum::StructType(s) => s,
            _ => self.vec_struct_type(),
        };
        let mut agg = st.get_undef();
        // Reconstruct field-by-field. Each LLVM field of the target
        // struct corresponds to one i64 word in source-declaration order
        // (matches `coerce_to_payload_words`'s decomposition shape).
        let n_fields = st.count_fields() as usize;
        for i in 0..n_fields {
            if i >= field_words.len() {
                break;
            }
            let word = field_words[i];
            let field_ty = st
                .get_field_type_at_index(i as u32)
                .ok_or_else(|| format!("field type at index {} missing", i))?;
            let field_val: BasicValueEnum<'ctx> = match field_ty {
                BasicTypeEnum::IntType(it) => {
                    if it.get_bit_width() == 64 {
                        word.into()
                    } else if it.get_bit_width() < 64 {
                        self.builder
                            .build_int_truncate(word, it, "pl.tr")
                            .unwrap()
                            .into()
                    } else {
                        self.builder
                            .build_int_z_extend(word, it, "pl.zx")
                            .unwrap()
                            .into()
                    }
                }
                BasicTypeEnum::FloatType(ft) => {
                    self.builder.build_bit_cast(word, ft, "pl.fc").unwrap()
                }
                BasicTypeEnum::PointerType(_) => self
                    .builder
                    .build_int_to_ptr(word, ptr_ty, "pl.itop")
                    .unwrap()
                    .into(),
                _ => word.into(),
            };
            agg = self
                .builder
                .build_insert_value(agg, field_val, i as u32, "pl.iv")
                .unwrap()
                .into_struct_value();
        }
        Ok(agg.into())
    }
}
