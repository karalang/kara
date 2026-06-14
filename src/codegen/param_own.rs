//! #14 — Callee-ownership for by-value aggregate (`struct` / `enum`)
//! parameters.
//!
//! ## The bug
//!
//! Codegen passes a by-value aggregate argument as a SHALLOW copy (the
//! struct/enum words, including any heap `ptr`) under a *caller-retains*
//! model: the caller's source binding frees the buffers at its scope exit,
//! and the callee's param frees nothing. That model is sound only when the
//! callee *consumes-and-frees* (destructures) or *ignores* the value. It
//! double-frees when the callee **transfers the value OUT** — moves the param
//! into its return value (directly, or wrapped into a returned struct/enum
//! literal). Then the caller's source binding AND the returned value alias the
//! same buffer, and BOTH free it (`exit 133`).
//!
//! ## Why not move-by-default
//!
//! The "proper" fix — have the caller MOVE the arg (suppress its source drop)
//! and the callee OWN the param — is unsound here because Kāra's move-checker
//! does NOT reject double-consume / use-after-move: `take(x); take(x)` and
//! `take(x); println(x.f)` both compile and run correctly TODAY under
//! caller-retains. Caller-side move would turn those into use-after-frees.
//!
//! ## The fix: entry deep-copy + callee-owned drop
//!
//! At function entry, deep-copy the owned aggregate param's heap-field buffers
//! so the callee owns buffers INDEPENDENT of the caller's retained originals,
//! then register the param's scope-exit drop. The param now behaves exactly
//! like a `let`-bound local owned binding, so ALL existing local
//! move-suppression (tail return, struct/enum-literal consume, match
//! destructure, pass-as-arg) applies to it for free. Result: the caller frees
//! its original once; the callee frees its copy once (or suppresses that drop
//! when the copy is transferred out, leaving the destination the sole owner).
//! No caller-side change, hence no move-checker dependency — `take(x); take(x)`
//! keeps working (each call copies at entry).
//!
//! ## Depth discipline
//!
//! The entry copy MIRRORS the registered drop's depth EXACTLY. Both the struct
//! drop (`emit_struct_drop_synthesis`) and the enum drop
//! (`emit_enum_drop_switch`) free OUTER Vec/String buffers only — a nested
//! `Vec[heap_T]`'s elements are a bounded leak on both sides, never corruption
//! — recursing into nested structs/tuples. So the copy is an outer-buffer
//! copy (`emit_vecstr_defensive_copy` with `elem_te = None`, no element
//! recursion) per Vec/String field/payload, recursing into nested
//! structs/tuples.
//!
//! ## Bail conditions (left on caller-retains — never a regression)
//!
//! Any aggregate whose drop frees buffers this routine can't soundly duplicate
//! is left untouched (returns `false`): Map/Set handles, HTTP side-table
//! handles (`Response`/`RequestBuilder`), shared (RC) types, and `Option` /
//! `Result` fields (type-erased payloads with no static `VecOrString` field
//! kind, so `deep_copy_enum_heap_payload_in_place` can't duplicate them — and
//! the struct drop deliberately ignores them, so there's no double-free to
//! guard). A non-shared user-ENUM field IS now supported (#19, 2026-06-12): the
//! struct drop frees its live-variant `VecOrString` payload (post-#15/#18) and
//! `deep_copy_one_aggregate_field` duplicates exactly that via
//! `deep_copy_enum_heap_payload_in_place`, keeping copy and drop symmetric.
//! Bailing on the rest preserves today's exact behavior for those shapes.

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::PointerValue;
use inkwell::AddressSpace;
use std::collections::HashMap;

use crate::ast::{Expr, ExprKind, TypeExpr, TypeKind};

use super::state::{EnumDropKind, EnumLayout};

impl<'ctx> super::Codegen<'ctx> {
    /// Make an owned by-value aggregate parameter callee-owned: emit the entry
    /// deep-copy of its heap fields and register its scope-exit drop. Returns
    /// `true` if ownership was taken; `false` if the param was left on the
    /// caller-retains model (no copy, no drop — status quo). See the module
    /// doc for the full rationale.
    pub(super) fn make_aggregate_param_callee_owned(
        &mut self,
        type_name: &str,
        slot: PointerValue<'ctx>,
    ) -> bool {
        // #17 — the seeded std.tracing builder value types (`LogEvent` / `Span`
        // / `SpanField`) used to be name-excluded here. Their chained builder
        // methods (`info(..).with_field(..).with_field(..).in_span(..)`) move
        // individual `self` fields into returned literals, and engaging
        // entry-copy on top of the caller-retains `owned_struct_params` field-move
        // band-aid double-copied / emptied the chained fields. That redundancy is
        // now resolved generally: (gap 1) `compile_function` retires the
        // `owned_struct_params` band-aid for a callee-owned param, and (gap 2)
        // `compile_struct_init` cap-zeros a slot-sourced Vec/String/enum field
        // moved into a returned literal. With both in place these types are
        // callee-owned like any other aggregate — no name exclusion needed.
        // Non-shared user STRUCT.
        if self.struct_types.contains_key(type_name) && !self.shared_types.contains_key(type_name) {
            if !self.aggregate_param_copy_supported_struct(type_name, &mut Vec::new()) {
                return false;
            }
            self.deep_copy_struct_heap_fields_in_place(slot, type_name);
            self.track_struct_var(type_name, slot);
            return true;
        }
        // Non-shared user ENUM (NOT the type-erased Option/Result, whose
        // payloads are handled by their own dedicated machinery).
        if let Some(layout) = self.enum_layouts.get(type_name).cloned() {
            if layout.is_shared || type_name == "Option" || type_name == "Result" {
                return false;
            }
            // Only meaningful when some variant carries a heap payload —
            // otherwise the drop is a no-op and there's nothing to copy.
            let any_heap = layout
                .field_drop_kinds
                .values()
                .any(|ks| ks.iter().any(|k| k.is_heap_bearing()));
            if !any_heap {
                return false;
            }
            self.deep_copy_enum_heap_payload_in_place(type_name, slot, &layout);
            self.track_enum_var(type_name, slot);
            return true;
        }
        false
    }

    /// Recursively decide whether a struct's heap content can be soundly
    /// outer-buffer-copied to mirror its drop. `stack` guards against
    /// self-referential owned structs (which would recurse forever — bail).
    pub(super) fn aggregate_param_copy_supported_struct(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
    ) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        if self.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return false;
        };
        stack.push(struct_name.to_string());
        let ok = ftes.iter().all(|fte| self.field_copy_supported(fte, stack));
        stack.pop();
        ok
    }

    fn field_copy_supported(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems.iter().all(|e| self.field_copy_supported(e, stack)),
            // Borrows carry no owned heap — the struct drop never frees them.
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_) => true,
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                match head {
                    "String" | "Vec" | "VecDeque" => true,
                    "Slice" => true,
                    // Heap the outer-buffer copy can't duplicate → bail.
                    "Map" | "HashMap" | "Set" | "HashSet" | "SortedSet" | "BTreeMap"
                    | "BTreeSet" => false,
                    // HTTP side-table handle structs (see emit_struct_drop_synthesis).
                    "Response" | "RequestBuilder" => false,
                    // Type-erased Option/Result: their payloads carry no static
                    // VecOrString field kind, so `deep_copy_enum_heap_payload_in_place`
                    // can't duplicate them — and the struct drop deliberately does
                    // NOT free them (#15 excludes Option/Result), so there's no
                    // double-free to guard. Bail to status quo (their own inline
                    // machinery owns the payload).
                    "Option" | "Result" => false,
                    _ if is_primitive_type_name(head) => true,
                    _ if self.shared_types.contains_key(head) => false,
                    _ if self.struct_types.contains_key(head) => {
                        self.aggregate_param_copy_supported_struct(head, stack)
                    }
                    // User enum field (#19 FIXED 2026-06-12). Without entry-copy,
                    // a by-value transfer of an enum-field struct (`let b =
                    // wrap(a)`, `wrap(s: Span) -> Span { s }`) leaves `b` shallow-
                    // aliasing the source's enum buffer; post-#15 BOTH struct drops
                    // free it → double-free (#19). `EnumDropKind` only ever frees a
                    // `VecOrString` payload — exactly what
                    // `deep_copy_enum_heap_payload_in_place` duplicates (wired into
                    // `deep_copy_one_aggregate_field`) — so entry-copy is symmetric
                    // with the struct drop's enum-field free: whatever the drop
                    // frees, the copy copies; carved-out nested-aggregate payloads
                    // are `EnumDropKind::None`, freed by neither. Shared enums bail
                    // at the `shared_types` arm above; Option/Result bail above too,
                    // so any enum reaching here is a non-shared user enum.
                    _ if self.enum_layouts.contains_key(head) => !self.enum_layouts[head].is_shared,
                    // Generic type param / unknown → conservative bail.
                    _ => false,
                }
            }
            // Array[T, N] of heap, fn-ptr types, etc. → conservative bail.
            _ => false,
        }
    }

    /// Deep-copy every Vec/String heap field of the struct value at `base_ptr`,
    /// recursing into nested structs/tuples. Mirrors
    /// `emit_struct_drop_synthesis`'s field walk.
    fn deep_copy_struct_heap_fields_in_place(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
    ) {
        let Some(&st) = self.struct_types.get(struct_name) else {
            return;
        };
        let Some(ftes) = self.struct_field_type_exprs.get(struct_name).cloned() else {
            return;
        };
        for (i, fte) in ftes.iter().enumerate() {
            self.deep_copy_one_aggregate_field(base_ptr, st, i as u32, fte);
        }
    }

    /// Copy one aggregate field in place per its TypeExpr. String/Vec → outer
    /// buffer copy; nested struct → recurse; tuple → recurse per element;
    /// everything else (primitive, borrow, ignored kinds) → no-op.
    fn deep_copy_one_aggregate_field(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        agg_ty: StructType<'ctx>,
        idx: u32,
        fte: &TypeExpr,
    ) {
        let vec_ty = self.vec_struct_type();
        // String / Vec field → copy the OUTER buffer in place (`elem_te = None`),
        // mirroring the struct drop's outer-only free (nested Vec elements are a
        // bounded leak on both sides, never corruption).
        let elem_ty: Option<BasicTypeEnum<'ctx>> = if self.is_string_type_expr(fte) {
            Some(self.context.i8_type().into())
        } else {
            self.extract_vec_elem_type(fte)
        };
        if let Some(elem_ty) = elem_ty {
            if let Ok(field_ptr) = self
                .builder
                .build_struct_gep(agg_ty, base_ptr, idx, "p14.f")
            {
                if let Ok(val) = self.builder.build_load(vec_ty, field_ptr, "p14.v") {
                    let copied = self.emit_vecstr_defensive_copy(val, elem_ty, None);
                    let _ = self.builder.build_store(field_ptr, copied);
                }
            }
            return;
        }
        // Nested non-shared user struct → recurse into it in place.
        if let TypeKind::Path(p) = &fte.kind {
            if let Some(head) = p.segments.first() {
                if self.struct_types.contains_key(head.as_str())
                    && !self.shared_types.contains_key(head.as_str())
                {
                    if let Ok(field_ptr) = self
                        .builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.nf")
                    {
                        let name = head.clone();
                        self.deep_copy_struct_heap_fields_in_place(field_ptr, &name);
                    }
                    return;
                }
            }
        }
        // Nested user-ENUM field (#19 FIXED) → deep-copy its live-variant
        // Vec/String payload in place, mirroring the struct drop's per-field enum
        // free (`emit_struct_drop_synthesis`'s `EnumField` arm → `__karac_drop_<E>`).
        // `deep_copy_enum_heap_payload_in_place` duplicates exactly the
        // `VecOrString` payloads `EnumDropKind` frees, so the entry-copy stays
        // symmetric with the drop. Shared enums / Option / Result never reach here
        // — `field_copy_supported` bails on them, so the struct is caller-retains.
        if let TypeKind::Path(p) = &fte.kind {
            if let Some(head) = p.segments.first() {
                if let Some(layout) = self.enum_layouts.get(head.as_str()).cloned() {
                    if !layout.is_shared && head != "Option" && head != "Result" {
                        if let Ok(field_ptr) = self
                            .builder
                            .build_struct_gep(agg_ty, base_ptr, idx, "p14.ef")
                        {
                            let name = head.clone();
                            self.deep_copy_enum_heap_payload_in_place(&name, field_ptr, &layout);
                        }
                        return;
                    }
                }
            }
        }
        // Tuple field → recurse into each element.
        if let TypeKind::Tuple(elems) = &fte.kind {
            if !elems.is_empty() {
                if let (Ok(field_ptr), Some(BasicTypeEnum::StructType(tup_ty))) = (
                    self.builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.tf"),
                    agg_ty.get_field_type_at_index(idx),
                ) {
                    for (j, ete) in elems.iter().enumerate() {
                        self.deep_copy_one_aggregate_field(field_ptr, tup_ty, j as u32, ete);
                    }
                }
            }
        }
        // Primitive / borrow / ignored kind → nothing to copy.
    }

    /// Deep-copy (outer buffers only) the live variant's Vec/String payload of
    /// the enum value at `base_ptr`. Emits a tag switch mirroring
    /// `emit_enum_drop_switch`; only variants with a VecOrString payload get a
    /// case. The enum's payload words are stored as raw i64s (data = ptrtoint,
    /// then len, then cap), so the copy reconstructs a `{ptr,len,cap}` value,
    /// runs `emit_vecstr_defensive_copy`, and writes the copied words back.
    fn deep_copy_enum_heap_payload_in_place(
        &mut self,
        enum_name: &str,
        base_ptr: PointerValue<'ctx>,
        layout: &EnumLayout<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let enum_ty = layout.llvm_type;
        let fn_val = self.current_fn.unwrap();

        // Per-variant payload element TypeExprs (for buffer-element sizing).
        let variant_tes: HashMap<String, Vec<TypeExpr>> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .map(|(_tag, name, tes)| (name, tes))
            .collect();

        let tag_ptr = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, 0, "p14e.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14e.tag")
            .unwrap()
            .into_int_value();

        let mut tag_entries: Vec<(String, u64)> =
            layout.tags.iter().map(|(n, t)| (n.clone(), *t)).collect();
        tag_entries.sort_by_key(|(_, t)| *t);

        let merge_bb = self.context.append_basic_block(fn_val, "p14e.merge");
        let mut cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut case_bbs: Vec<(String, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        for (name, tag_v) in &tag_entries {
            let has_heap = layout
                .field_drop_kinds
                .get(name)
                .map(|ks| ks.iter().any(|k| k.is_heap_bearing()))
                .unwrap_or(false);
            if !has_heap {
                continue;
            }
            let bb = self
                .context
                .append_basic_block(fn_val, &format!("p14e.{name}"));
            cases.push((i64_t.const_int(*tag_v, false), bb));
            case_bbs.push((name.clone(), bb));
        }

        self.builder.build_switch(tag, merge_bb, &cases).unwrap();

        for (name, bb) in &case_bbs {
            self.builder.position_at_end(*bb);
            if let (Some(kinds), Some(offsets)) = (
                layout.field_drop_kinds.get(name),
                layout.field_word_offsets.get(name),
            ) {
                for (fi, (kind, (start_word, _num_words))) in
                    kinds.iter().zip(offsets.iter()).enumerate()
                {
                    // B-2026-06-13-13: a nested-struct payload is deep-copied by
                    // recursing into the struct's own heap fields in place — the
                    // symmetric peer of the enum drop's `NestedStruct` arm, so the
                    // callee copy and caller temp own independent buffers (no
                    // double-free). The struct's words start at `start_word + 1`.
                    if *kind == EnumDropKind::NestedStruct {
                        let sname =
                            variant_tes
                                .get(name)
                                .and_then(|tes| tes.get(fi))
                                .and_then(|te| match &te.kind {
                                    TypeKind::Path(p) => p.segments.first().cloned(),
                                    _ => None,
                                });
                        if let Some(sname) = sname {
                            if let Ok(field_ptr) = self.builder.build_struct_gep(
                                enum_ty,
                                base_ptr,
                                (*start_word + 1) as u32,
                                "p14e.nstruct.p",
                            ) {
                                self.deep_copy_struct_heap_fields_in_place(field_ptr, &sname);
                            }
                        }
                        continue;
                    }
                    if *kind != EnumDropKind::VecOrString {
                        continue;
                    }
                    let data_idx = (*start_word + 1) as u32;
                    let len_idx = (*start_word + 2) as u32;
                    let cap_idx = (*start_word + 3) as u32;

                    let data_w = self.load_enum_word(enum_ty, base_ptr, data_idx, "p14e.data");
                    let len_w = self.load_enum_word(enum_ty, base_ptr, len_idx, "p14e.len");
                    let cap_w = self.load_enum_word(enum_ty, base_ptr, cap_idx, "p14e.cap");
                    let data_p = self
                        .builder
                        .build_int_to_ptr(data_w, ptr_ty, "p14e.data.p")
                        .unwrap();

                    // Reconstruct the {ptr,len,cap} value the defensive copy expects.
                    let mut sv = vec_ty.get_undef();
                    sv = self
                        .builder
                        .build_insert_value(sv, data_p, 0, "p14e.sv.d")
                        .unwrap()
                        .into_struct_value();
                    sv = self
                        .builder
                        .build_insert_value(sv, len_w, 1, "p14e.sv.l")
                        .unwrap()
                        .into_struct_value();
                    sv = self
                        .builder
                        .build_insert_value(sv, cap_w, 2, "p14e.sv.c")
                        .unwrap()
                        .into_struct_value();

                    let elem_ty: BasicTypeEnum<'ctx> = variant_tes
                        .get(name)
                        .and_then(|tes| tes.get(fi))
                        .map(|te| {
                            if self.is_string_type_expr(te) {
                                self.context.i8_type().into()
                            } else {
                                self.extract_vec_elem_type(te)
                                    .unwrap_or_else(|| self.context.i8_type().into())
                            }
                        })
                        .unwrap_or_else(|| self.context.i8_type().into());

                    // Outer-buffer copy (`elem_te = None`), mirroring the enum
                    // drop's outer-only payload free.
                    let copied = self
                        .emit_vecstr_defensive_copy(sv.into(), elem_ty, None)
                        .into_struct_value();
                    let cd = self
                        .builder
                        .build_extract_value(copied, 0, "p14e.cd")
                        .unwrap()
                        .into_pointer_value();
                    let cl = self
                        .builder
                        .build_extract_value(copied, 1, "p14e.cl")
                        .unwrap()
                        .into_int_value();
                    let cc = self
                        .builder
                        .build_extract_value(copied, 2, "p14e.cc")
                        .unwrap()
                        .into_int_value();
                    let cd_w = self
                        .builder
                        .build_ptr_to_int(cd, i64_t, "p14e.cd.w")
                        .unwrap();

                    self.store_enum_word(enum_ty, base_ptr, data_idx, cd_w.into());
                    self.store_enum_word(enum_ty, base_ptr, len_idx, cl.into());
                    self.store_enum_word(enum_ty, base_ptr, cap_idx, cc.into());
                }
            }
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);
    }

    /// #14 — at a struct-literal field init `S { f: obj.field }` whose value is
    /// a heap FIELD moved out of a tracked struct binding `obj` (a callee-owned
    /// by-value aggregate param, or a local), cap-zero that field's buffer in
    /// `obj`'s slot so `obj`'s `StructDrop` skips it — the new struct literal is
    /// now the sole owner. This is the field-access peer of the whole-Identifier
    /// `suppress_source_vec_cleanup_for_arg` (which the literal path already
    /// calls), and the analog of its TupleIndex arm.
    ///
    /// SCOPED to struct-literal field inits, where the value is genuinely MOVED
    /// into the new owner — NOT folded into the general suppression funnel,
    /// which also fires at by-value-arg sites where the callee may not take
    /// ownership (cap-zeroing there would leak). Without this, a builder method
    /// that moves `self`'s fields into a returned literal
    /// (`LogEvent { level: self.level, message: self.message, … }`) double-frees
    /// once `self` is a callee-owned by-value aggregate param — the source field
    /// AND the returned literal both free the same buffer (std.tracing's
    /// `with_field`).
    pub(super) fn suppress_struct_field_move_into_literal(&self, value: &Expr) {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        // The source root is either a named binding (`obj.field`) or the method
        // receiver (`self.field`) — `self` is bound as an ordinary local named
        // "self" by `compile_function`. The std.tracing builder bodies move
        // `self.fields` / `self.message` out, so SelfValue must resolve here or
        // the move-out suppression never fires (#17 gap 2).
        let s: &str = match &object.kind {
            ExprKind::Identifier(s) => s.as_str(),
            ExprKind::SelfValue => "self",
            _ => return,
        };
        let Some(slot) = self.variables.get(s).copied() else {
            return;
        };
        let BasicTypeEnum::StructType(agg_ty) = slot.ty else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        if agg_ty == vec_ty {
            return;
        }
        let Some(sname) = self.var_type_names.get(s).cloned() else {
            return;
        };
        let Some(idx) = self
            .struct_field_names
            .get(sname.as_str())
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        let field_llvm = agg_ty.get_field_type_at_index(idx as u32);
        let Ok(field_ptr) =
            self.builder
                .build_struct_gep(agg_ty, slot.ptr, idx as u32, "p14.fldmv.p")
        else {
            return;
        };
        match field_llvm {
            // Direct Vec/String field → zero its cap (drop's `cap > 0` skips).
            Some(BasicTypeEnum::StructType(fst)) if fst == vec_ty => {
                if let Ok(cap_ptr) =
                    self.builder
                        .build_struct_gep(vec_ty, field_ptr, 2, "p14.fldmv.cap")
                {
                    let _ = self
                        .builder
                        .build_store(cap_ptr, self.context.i64_type().const_int(0, false));
                }
            }
            // Nested aggregate field → recursively zero its Vec/String caps.
            Some(BasicTypeEnum::StructType(fst)) if self.aggregate_has_heap_field(fst) => {
                self.zero_aggregate_field_caps(field_ptr, fst);
            }
            // Enum field (#19) → cap-zero its `VecOrString` payload words so the
            // owning struct's drop skips the buffer the moved-out binding now owns
            // (`let tk = t.token` of an entry-copied SpannedToken — the bootstrap
            // lexer's `render()` shape). The enum's LLVM type is all-i64 words, so
            // it matches neither the Vec arm (`== vec_ty`) nor
            // `aggregate_has_heap_field` (no `vec_struct` field) — it would
            // otherwise fall through unsuppressed. Resolve the enum by the field's
            // declared type; shared enums carry RC (no `VecOrString` kind) and
            // self-skip, Option/Result have no static kind and `zero_enum_payload_caps`
            // no-ops for them.
            Some(BasicTypeEnum::StructType(_)) => {
                if let Some(ename) = self
                    .struct_field_type_exprs
                    .get(sname.as_str())
                    .and_then(|ftes| ftes.get(idx))
                    .and_then(|fte| match &fte.kind {
                        TypeKind::Path(p) => p.segments.first().cloned(),
                        _ => None,
                    })
                {
                    if let Some(layout) = self.enum_layouts.get(ename.as_str()) {
                        if !layout.is_shared {
                            let layout = layout.clone();
                            self.zero_enum_payload_caps(field_ptr, &layout);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn load_enum_word(
        &self,
        enum_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let p = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, idx, name)
            .unwrap();
        self.builder
            .build_load(i64_t, p, name)
            .unwrap()
            .into_int_value()
    }

    fn store_enum_word(
        &self,
        enum_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        val: inkwell::values::BasicValueEnum<'ctx>,
    ) {
        if let Ok(p) = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, idx, "p14e.store.p")
        {
            let _ = self.builder.build_store(p, val);
        }
    }
}

fn is_primitive_type_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}
