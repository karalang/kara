//! Pattern-binding lowering (the non-condition arm of match): destructure
//! a scrutinee value into named bindings.
//!
//! Houses `bind_pattern_values` (the structural-pattern walk that
//! materializes per-leaf-binding `VarSlot`s from a scrutinee value),
//! `emit_ref_leaf_binding_at_ptr` (the borrow-mode shim that
//! synthesizes a ref-shim alloca for a leaf binding whose pattern-
//! site mode is `Ref` / `MutRef`), and `bind_pattern_values_via_ptr`
//! (the variant of `bind_pattern_values` that takes a pointer-to-
//! scrutinee instead of a loaded value — used by Slice / `mut Slice`
//! / array slice-pattern bindings that need element-pointer access
//! to avoid round-tripping through a load).

use crate::ast::*;

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::AddressSpace;

use super::state::{EnumLayout, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn bind_pattern_values(
        &mut self,
        pattern: &Pattern,
        scrut: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                // Skip binding if this is a unit enum variant pattern.
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                if self.enum_tag_for_variant(variant_name).is_some() {
                    return Ok(());
                }
                let fn_val = self.current_fn.unwrap();

                // Shared-struct payload reconstitution. `Option[Shared(N)]`
                // (and every other enum carrying a shared-struct payload)
                // lowers the heap pointer to an `i64` payload word; the
                // non-shared-enum extraction path above (line ~18415)
                // hands us that word as `IntValue`. Without re-typing the
                // binding's alloca as a pointer, `compile_field_access`
                // calls `.into_pointer_value()` on the loaded `i64` and
                // panics — or its shared-enum branch silently misses
                // because `compile_expr(Identifier("node"))` returns
                // `IntValue` instead of `PointerValue`. Restore the
                // pointer shape here so downstream `.field` / method-
                // call dispatch on a pattern-bound shared handle finds
                // the heap struct.
                let key = (pattern.span.offset, pattern.span.length);
                if let Some(type_name) = self.pattern_binding_types.get(&key).cloned() {
                    if let Some(info) = self.shared_types.get(&type_name).cloned() {
                        let ptr_ty = self.context.ptr_type(AddressSpace::default());
                        let ptr_val = match scrut {
                            BasicValueEnum::IntValue(iv) => self
                                .builder
                                .build_int_to_ptr(iv, ptr_ty, &format!("{}.ptr", name))
                                .unwrap()
                                .into(),
                            BasicValueEnum::PointerValue(_) => scrut,
                            _ => scrut,
                        };
                        let alloca = self.create_entry_alloca(fn_val, name, ptr_ty.into());
                        self.builder.build_store(alloca, ptr_val).unwrap();
                        self.variables.insert(
                            name.clone(),
                            VarSlot {
                                ptr: alloca,
                                ty: ptr_ty.into(),
                            },
                        );
                        self.record_var_type_name(name.clone(), type_name.clone());
                        // Alias acquire — pattern-binding sibling of the
                        // let-path receive-inc (`stmts.rs` shared_info arm)
                        // and the kata-21 field-let acquire. The binding
                        // aliases a payload OWNED BY ITS SOURCE (an enum
                        // payload word, typically an `Option[shared]`
                        // field); without its own +1, any store that
                        // displaces the source's ref while the binding is
                        // live frees the node under it — kata #24's
                        // pair-swap (`if let Some(second) = first.next {
                        // first.next = second.next; ... }`) reads `second`
                        // through freed memory. The scope-exit `RcDec`
                        // (`track_rc_var`, drained at the binding scope's
                        // end — if-let arm / match arm / while-let body
                        // frame) balances the inc; the entry-block null
                        // init keeps the early-exit drains' reload-by-name
                        // guard sound on paths where the bind never ran.
                        //
                        // Skipped for b2 count-free roles (fresh nodes /
                        // cursors / C2a borrowed-family walk bindings —
                        // nothing is freed mid-scope in those families, so
                        // count-free aliases never dangle) and for
                        // headerless (phase-D) members, which have no rc
                        // word to touch — an inc would corrupt the first
                        // user field. b2 is a structural precondition of
                        // headerless, so the role check alone covers most
                        // of it; the explicit layout check keeps the two
                        // gates independent.
                        if !self.b2_skips_counts(name) && !self.headerless_here(&type_name) {
                            let ptr_v = ptr_val.into_pointer_value();
                            self.emit_refcount_inc(name, info.heap_type, ptr_v);
                            self.track_rc_var(name, alloca, info.heap_type);
                            self.null_init_slot_in_entry_block(alloca);
                        }
                        return Ok(());
                    }
                    // Phase 8 `File` handle slice F4: same int→ptr
                    // re-typing as the shared-struct path above. When
                    // the user destructures `Ok(f)` against a
                    // `Result[File, IoError]`, the Result enum-payload
                    // word arrives as i64 (the Result lowering's
                    // payload-word ABI); without converting back to
                    // `ptr` here, downstream `f.read(...)` /
                    // `f.write(...)` dispatch would call
                    // `compile_expr(Identifier("f"))` and receive an
                    // IntValue where the dispatch arms expect a
                    // PointerValue. Stand-alone arm (not folded into
                    // the shared-types check) because `File` isn't a
                    // `shared struct` — it's an opaque handle the F3
                    // lowering routes to `ptr`.
                    if type_name == "File" {
                        let ptr_ty = self.context.ptr_type(AddressSpace::default());
                        let ptr_val = match scrut {
                            BasicValueEnum::IntValue(iv) => self
                                .builder
                                .build_int_to_ptr(iv, ptr_ty, &format!("{}.fileptr", name))
                                .unwrap()
                                .into(),
                            BasicValueEnum::PointerValue(_) => scrut,
                            _ => scrut,
                        };
                        let alloca = self.create_entry_alloca(fn_val, name, ptr_ty.into());
                        self.builder.build_store(alloca, ptr_val).unwrap();
                        self.variables.insert(
                            name.clone(),
                            VarSlot {
                                ptr: alloca,
                                ty: ptr_ty.into(),
                            },
                        );
                        self.record_var_type_name(name.clone(), type_name);
                        // F4b — register the File-typed binding for
                        // scope-exit close. The drain emits
                        // `karac_runtime_file_close(load(file_alloca))`
                        // when this scope frame unwinds. Gated on
                        // `!pattern_binding_is_borrow` exactly like the
                        // Vec/String `track_vec_var` site below: under a
                        // borrow-returning scrutinee (`Map.get`) or a
                        // `ref x @ Ok(f)` by_ref bind, the fd is owned by
                        // the source and closed there — registering a
                        // second close here double-closes the same fd
                        // (`karac_runtime_file_close` fired twice).
                        if !self.pattern_binding_is_borrow {
                            self.track_file_var(alloca);
                        }
                        return Ok(());
                    }
                }

                // Struct-payload reconstruction: when the typechecker
                // recorded a struct surface type for this binding, the
                // enum-payload codegen has handed us the i64 word that
                // held the (single-field) struct. Wrap it back into the
                // struct shape so subsequent `.field` access dispatches
                // through the right LLVM struct type. Limited to the
                // single-i64-field case for now — wider error wrappers
                // can't survive the i64-payload-word lowering anyway, so
                // there's nothing to reconstitute beyond this shape.
                let key = (pattern.span.offset, pattern.span.length);
                if let Some(type_name) = self.pattern_binding_types.get(&key).cloned() {
                    if let Some(&st) = self.struct_types.get(&type_name) {
                        if let BasicValueEnum::IntValue(iv) = scrut {
                            if st.count_fields() == 1
                                && matches!(
                                    st.get_field_type_at_index(0),
                                    Some(BasicTypeEnum::IntType(t))
                                        if t.get_bit_width() == iv.get_type().get_bit_width()
                                )
                            {
                                let undef = st.get_undef();
                                let struct_val = self
                                    .builder
                                    .build_insert_value(undef, iv, 0, "pat.struct")
                                    .unwrap()
                                    .into_struct_value();
                                let alloca = self.create_entry_alloca(fn_val, name, st.into());
                                self.builder.build_store(alloca, struct_val).unwrap();
                                self.variables.insert(
                                    name.clone(),
                                    VarSlot {
                                        ptr: alloca,
                                        ty: st.into(),
                                    },
                                );
                                self.record_var_type_name(name.clone(), type_name);
                                return Ok(());
                            }
                        }
                    }
                }

                let alloca = self.create_entry_alloca(fn_val, name, scrut.get_type());
                self.builder.build_store(alloca, scrut).unwrap();
                self.variables.insert(
                    name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: scrut.get_type(),
                    },
                );
                // Compound-payload enum codegen (CP4): when the
                // typechecker recorded a surface name for this binding
                // (set in `check_pattern_against`), propagate it to
                // `var_type_names` so subsequent method-dispatch (e.g.
                // `xs.len()` on a `Vec[T]` payload) routes to the right
                // collection type. The Vec/String/Slice families are
                // looked up by name in `compile_method_call` via this
                // table; user struct types use it for `.field` access.
                let key = (pattern.span.offset, pattern.span.length);
                let mut bound_vec_elem: Option<BasicTypeEnum<'ctx>> = None;
                if let Some(type_name) = self.pattern_binding_types.get(&key).cloned() {
                    // PB sibling slice (2026-05-09): when the binding's
                    // surface type is `Vec[T]` / `Slice[T]`, look up the
                    // inner element TypeExpr in the sibling table and
                    // register the LLVM element type under the binding's
                    // variable name. This lights up direct method dispatch
                    // (`xs.len()` / `xs[0]` / `xs.push(...)`) on a
                    // pattern-bound collection payload — without it, the
                    // dispatch falls through to a generic path that
                    // doesn't know the element type and either produces
                    // wrong codegen or fails with cryptic diagnostics.
                    // String / user-struct surface types don't populate
                    // any elem-type registry — they're sufficient via
                    // the existing String-name table.
                    if let Some(inner_te) = self.pattern_binding_inner_types.get(&key).cloned() {
                        let elem_llvm = self.llvm_type_for_type_expr(&inner_te);
                        match type_name.as_str() {
                            // `VecDeque[T]` shares `Vec[T]`'s `{ptr, len, cap}`
                            // storage + method dispatch, so a `match … { Ok(v)
                            // => v.len() }` over a `VecDeque` payload must
                            // register `vec_elem_types` too — without this it
                            // fell through method dispatch ("no handler for
                            // method 'len'"), the VecDeque half of
                            // B-2026-06-10-3.
                            "Vec" | "VecDeque" => {
                                self.vec_elem_types.insert(name.clone(), elem_llvm);
                                bound_vec_elem = Some(elem_llvm);
                            }
                            "Slice" => {
                                self.slice_elem_types.insert(name.clone(), elem_llvm);
                            }
                            _ => {}
                        }
                    }
                    // String binding via enum payload — the layout matches
                    // `Vec[u8]` (`{ptr, len, cap}` shape) so the same
                    // buffer-free cleanup applies. Element type is `u8`.
                    if type_name == "String" {
                        let u8_ty: BasicTypeEnum<'ctx> = self.context.i8_type().into();
                        self.vec_elem_types.insert(name.clone(), u8_ty);
                        bound_vec_elem = Some(u8_ty);
                    }
                    // Map[K,V] / Set[T] payload binding — register the
                    // collection dispatch side-tables (map_key_types /
                    // map_val_types / set_elem_types) off the full collection
                    // `TypeExpr` the typechecker stored in
                    // `pattern_binding_inner_types`, so `m.len()` /
                    // `s.contains(x)` on a match-arm-bound Map/Set dispatches
                    // like a let-bound one. Mirrors the Vec/Slice arm above,
                    // but routes through the shared `register_var_from_type_expr`
                    // helper (which extracts K/V/elem).
                    //
                    // AND register the handle's scope-exit free
                    // (`track_map_var`) so the binding OWNS and frees the moved-
                    // out Map/Set at end-of-arm — closes the deferred match-bound-
                    // Map leak (B-2026-06-12-6 cluster 4): `match make() {
                    // Some(m) => println(m.len()) }` over an `Option[Map]` leaked
                    // the whole handle (the source's `FreeInlineOptionMapPayload`
                    // is suppressed → tag set to `None` → on the consuming arm by
                    // `suppress_inline_option_map_payload_cleanup`, and a fresh-
                    // temp scrutinee was never tracked at all). The bound name now
                    // takes over the free; the source suppression (named source)
                    // or the absence of source tracking (fresh temp) prevents a
                    // double-free. Gated on `!pattern_binding_is_borrow` exactly
                    // like the Vec arm below — a borrow-returning scrutinee
                    // (`Map.get` → `Option[ref V]`) aliases the container's
                    // storage, which frees it itself. The `match opt { Some(m) =>
                    // m }` return-the-map shape is balanced by the arm-tail
                    // `suppress_map_cleanup_for_tail_identifier` in `compile_match`
                    // (the Map sibling of the Vec tail-move suppression).
                    if matches!(type_name.as_str(), "Map" | "Set") {
                        if let Some(full_te) = self.pattern_binding_inner_types.get(&key).cloned() {
                            self.register_var_from_type_expr(name, &full_te);
                            if !self.pattern_binding_is_borrow {
                                let (key_is_vec, val_is_vec, key_shared, val_shared) =
                                    self.map_temp_cleanup_parts(&full_te);
                                self.track_map_var(
                                    alloca, key_is_vec, val_is_vec, val_shared, key_shared,
                                );
                            }
                        }
                    }
                    self.record_var_type_name(name.clone(), type_name);
                }
                // Register scope-exit cleanup for the heap-owning binding.
                // The cleanup fires at end-of-match-arm via the per-arm
                // scope frame pushed by `compile_match` — so a Vec
                // extracted from a Map / Option / Result via `match` is
                // freed when the arm body completes, not at function-end.
                // Without the per-arm frame, alloca reuse across loop
                // iterations defeated the cleanup (only the last bound
                // value's data buffer got freed; the other N-1
                // generations leaked). The move-aware suppression in
                // `compile_match` zeros the cap before drain when the
                // arm's tail expression returns the bound value via
                // identity (e.g. `match opt { Some(v) => v }`), so
                // double-free is prevented for the canonical
                // Option-unwrap shape. Closes the 2026-05-13 bfs_sieve
                // residual leak — match-arm pattern-bound Vec/String
                // values were registered for method dispatch but never
                // for cleanup.
                // `pattern_binding_is_borrow` is set by `compile_match` when
                // the match scrutinee is a borrow-returning call (`Map.get`,
                // `Vec.first`, ...). In that case the Vec/String payload in
                // the Option/Result aliases the container's storage; the
                // container's own cleanup will free that buffer at scope
                // exit. Tracking the pattern-bound name as a tracked Vec
                // would queue a second `FreeVecBuffer` against the same
                // pointer → macOS `mfm_free.cold.4` spin on the resulting
                // double-free. Suppress the track in that case; the leak
                // mode the original tracking guarded against doesn't apply
                // since the container retains ownership.
                if let Some(elem_ty) = bound_vec_elem {
                    if !self.pattern_binding_is_borrow {
                        self.track_vec_var(alloca, Some(elem_ty));
                    }
                }
                // Phase-8 line 39 follow-up — register scope-exit Drop for a
                // pattern-bound HTTP `Response` / `HttpError`. The eager
                // client path binds these via `Ok(resp)` / `Err(e)`
                // destructure (pattern bindings), which don't flow through
                // the let-binding `track_struct_var` site in `stmts.rs`, so
                // wire them here: `Response` frees its `body` String + the
                // `headers` side-table handle; `HttpError` frees its
                // runtime-malloc'd `message` String. `track_struct_var`
                // no-ops when the type has no synthesized drop fn, and
                // move-suppression across every exit path comes from
                // `suppress_source_vec_cleanup_for_arg` (which zeros the
                // String caps + the headers handle on move). Targeted to the
                // seeded HTTP structs by name so unrelated struct
                // destructures keep their current (untracked) behavior.
                // Gated on `!pattern_binding_is_borrow` for the same
                // reason as the File and Vec/String sites: a borrow-mode
                // bind (`Map.get` scrutinee / `ref x @` by_ref) aliases a
                // `Response`/`HttpError` owned by the source, so its drop
                // fires there — a second `track_struct_var` here would
                // double-free the body String + headers handle.
                let bound_type = self.var_type_names.get(name.as_str()).cloned();
                if !self.pattern_binding_is_borrow
                    && matches!(bound_type.as_deref(), Some("Response" | "HttpError"))
                {
                    self.track_struct_var(bound_type.as_deref().unwrap(), alloca);
                }
                // Slice 3a (ref-scrutinee leaf binding ABI parity):
                // when the typechecker tagged this binding with a borrow
                // mode (i.e., the enclosing match scrutinee is `ref T` /
                // `mut ref T`), wrap the value alloca in a ref-shim — an
                // extra `ptr` alloca holding the value alloca's address,
                // registered in `ref_params`. Subsequent identifier
                // lookups go through `load_variable`'s auto-deref path,
                // and call sites that pass the binding to a `ref T` /
                // `mut ref T` parameter receive the shim alloca's
                // contents (a pointer) rather than the raw value —
                // closes the latent miscompile where well-typed
                // `match val { Foo { name } => use_str(name) }` under
                // `val: ref Foo` produced `name` as a struct value but
                // passed it where a pointer was expected.
                //
                // Mutation-propagation caveat: the shim aliases a
                // _copy_ of the scrutinee data, not the scrutinee
                // itself. A mutation through `mut ref` on a leaf
                // binding does not flow back to the original — same
                // limitation the interpreter sub-item documents
                // (phase-5-diagnostics.md slice 3 sub-item 2). The
                // pull-signal trigger for true GEP-based aliasing
                // remains a real user program that depends on
                // write-through under `mut ref` scrutinees.
                if self
                    .pattern_binding_borrow_modes
                    .contains_key(&(pattern.span.offset, pattern.span.length))
                {
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let shim_alloca = self.create_entry_alloca(
                        fn_val,
                        &format!("{}.refshim", name),
                        ptr_ty.into(),
                    );
                    self.builder.build_store(shim_alloca, alloca).unwrap();
                    let inner_ty = scrut.get_type();
                    self.variables.insert(
                        name.clone(),
                        VarSlot {
                            ptr: shim_alloca,
                            ty: ptr_ty.into(),
                        },
                    );
                    self.ref_params.insert(name.clone(), inner_ty);
                }
                Ok(())
            }
            PatternKind::TupleVariant { path, patterns } => {
                let variant_name = path.last().map(|s| s.as_str()).unwrap_or("");
                // Compound-payload enum codegen (CP4 destructure side):
                // resolve the variant's per-field word ranges from the
                // enum layout. When multiple enums share a variant name
                // (e.g., the built-in `Option.Some` and a user-defined
                // `MyOption.Some`), prefer the layout whose LLVM struct
                // type matches the scrutinee's type — `enum_layouts`
                // HashMap iteration order is non-deterministic, so a
                // bare `.values().find(...)` would mis-pick. Falls back
                // to "one word per field at sequential offsets" if no
                // layout matches (legacy IR-snippet compatibility).
                let scrut_struct_ty = match scrut {
                    BasicValueEnum::StructValue(sv) => Some(sv.get_type()),
                    _ => None,
                };
                let offsets: Vec<(usize, usize)> = self
                    .enum_layouts
                    .iter()
                    .find(|(_, l)| {
                        l.tags.contains_key(variant_name)
                            && scrut_struct_ty
                                .as_ref()
                                .map(|t| &l.llvm_type == t)
                                .unwrap_or(true)
                    })
                    .map(|(_, l)| l)
                    .or_else(|| {
                        // Type-match miss — fall back to variant-name
                        // lookup, but prefer user-declared enums over
                        // seeded built-ins (Option/Result/Json/TcpError)
                        // when the name collides. Without this, HashMap
                        // iteration order picks a seeded layout
                        // non-deterministically.
                        let mut user_hit: Option<&EnumLayout<'ctx>> = None;
                        let mut seed_hit: Option<&EnumLayout<'ctx>> = None;
                        for (en, l) in &self.enum_layouts {
                            if l.tags.contains_key(variant_name) {
                                if self.seeded_enum_names.contains(en) {
                                    seed_hit.get_or_insert(l);
                                } else {
                                    user_hit.get_or_insert(l);
                                }
                            }
                        }
                        user_hit.or(seed_hit)
                    })
                    .and_then(|l| l.field_word_offsets.get(variant_name).cloned())
                    .unwrap_or_else(|| (0..patterns.len()).map(|i| (i, 1)).collect());

                // Shared enum: extract payload via GEP (words at heap index 2+).
                if let BasicValueEnum::PointerValue(ptr) = scrut {
                    for (enum_name, layout) in &self.enum_layouts.clone() {
                        if layout.tags.contains_key(variant_name) {
                            if let Some(info) = self.shared_types.get(enum_name).cloned() {
                                for (i, sub_pat) in patterns.iter().enumerate() {
                                    let (start_word, num_words) =
                                        offsets.get(i).copied().unwrap_or((i, 1));
                                    let mut field_words: Vec<inkwell::values::IntValue<'ctx>> =
                                        Vec::with_capacity(num_words);
                                    for j in 0..num_words {
                                        let word_ptr = self
                                            .builder
                                            .build_struct_gep(
                                                info.heap_type,
                                                ptr,
                                                (start_word + j + 2) as u32,
                                                "sh_payload",
                                            )
                                            .unwrap();
                                        let w = self
                                            .builder
                                            .build_load(
                                                self.context.i64_type(),
                                                word_ptr,
                                                "payload",
                                            )
                                            .unwrap()
                                            .into_int_value();
                                        field_words.push(w);
                                    }
                                    let bound =
                                        self.reconstruct_payload_value(sub_pat, &field_words)?;
                                    self.bind_pattern_values(sub_pat, bound)?;
                                }
                                return Ok(());
                            }
                        }
                    }
                }
                // Non-shared enum: extract payload words from the struct value.
                if let BasicValueEnum::StructValue(sv) = scrut {
                    for (i, sub_pat) in patterns.iter().enumerate() {
                        let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
                        let mut field_words: Vec<inkwell::values::IntValue<'ctx>> =
                            Vec::with_capacity(num_words);
                        for j in 0..num_words {
                            let w = self
                                .builder
                                .build_extract_value(
                                    sv,
                                    (start_word + j + 1) as u32, // +1 for tag
                                    "payload",
                                )
                                .unwrap()
                                .into_int_value();
                            field_words.push(w);
                        }
                        let bound = self.reconstruct_payload_value(sub_pat, &field_words)?;
                        self.bind_pattern_values(sub_pat, bound)?;
                    }
                }
                Ok(())
            }
            PatternKind::Or(pats) => {
                // Bind variables from first sub-pattern (all alternatives must bind same names)
                if let Some(first) = pats.first() {
                    self.bind_pattern_values(first, scrut)?;
                }
                Ok(())
            }
            // Compound-payload tuple-payload destructure (CP follow-up):
            // mirrors the let-pattern Tuple arm in `bind_pattern`. The
            // scrutinee here is the tuple-shaped struct value produced by
            // `reconstruct_payload_value`'s Tuple branch; per-element
            // extracts dispatch back through `bind_pattern_values` so
            // nested tuples / leaf bindings / wildcards compose
            // uniformly.
            PatternKind::Tuple(pats) => {
                if let BasicValueEnum::StructValue(sv) = scrut {
                    for (idx, pat) in pats.iter().enumerate() {
                        let elem = self
                            .builder
                            .build_extract_value(sv, idx as u32, "tup.elem")
                            .unwrap();
                        self.bind_pattern_values(pat, elem)?;
                    }
                }
                Ok(())
            }
            // Plain struct destructure in a match arm: `match p { Foo
            // { x, y } => … }`. Mirrors the let-binding `bind_pattern`
            // Struct arm but resolves field index by name (the user can
            // omit / reorder fields) instead of positionally. Shorthand
            // fields synthesize a fresh `PatternKind::Binding` so the
            // ordinary leaf-binding path runs (alloca + variable
            // registration + the typechecker's `pattern_binding_types`
            // surface-name plumbing). Without this arm, struct match
            // destructure errored at body compile with
            // `Undefined variable 'x'` — the bind path was missing
            // entirely, the `_ => Ok(())` fall-through silently no-op'd.
            PatternKind::Struct {
                path,
                fields,
                has_rest: _,
            } => {
                // Enum struct-variant pattern `Enum.Variant { field, ... }`:
                // the qualifier names an enum whose `Variant` is struct-shaped.
                // Extract each named field's payload words by mapping the field
                // name to its declared position, then to the variant's
                // `field_word_offsets` slot — the named-field twin of the
                // TupleVariant arm above. (Without this, the struct-name lookup
                // below misses and the fields stay unbound → "Undefined
                // variable".)
                let variant_name = path.last().cloned().unwrap_or_default();
                if path.len() >= 2 {
                    let enum_name = path[path.len() - 2].clone();
                    if let Some(decl_field_names) =
                        self.enum_variant_struct_field_names(&enum_name, &variant_name)
                    {
                        if let BasicValueEnum::StructValue(sv) = scrut {
                            let offsets: Vec<(usize, usize)> = self
                                .enum_layouts
                                .get(&enum_name)
                                .and_then(|l| l.field_word_offsets.get(&variant_name).cloned())
                                .unwrap_or_default();
                            for field_pat in fields {
                                let Some(pos) =
                                    decl_field_names.iter().position(|n| n == &field_pat.name)
                                else {
                                    continue;
                                };
                                let (start_word, num_words) =
                                    offsets.get(pos).copied().unwrap_or((pos, 1));
                                let mut field_words: Vec<inkwell::values::IntValue<'ctx>> =
                                    Vec::with_capacity(num_words);
                                for j in 0..num_words {
                                    let w = self
                                        .builder
                                        .build_extract_value(
                                            sv,
                                            (start_word + j + 1) as u32, // +1 for tag
                                            "payload",
                                        )
                                        .unwrap()
                                        .into_int_value();
                                    field_words.push(w);
                                }
                                if let Some(sub_pat) = &field_pat.pattern {
                                    let bound =
                                        self.reconstruct_payload_value(sub_pat, &field_words)?;
                                    self.bind_pattern_values(sub_pat, bound)?;
                                } else {
                                    let synthetic = Pattern {
                                        kind: PatternKind::Binding(field_pat.name.clone()),
                                        span: field_pat.span.clone(),
                                    };
                                    let bound =
                                        self.reconstruct_payload_value(&synthetic, &field_words)?;
                                    self.bind_pattern_values(&synthetic, bound)?;
                                }
                            }
                        }
                        return Ok(());
                    }
                }
                let struct_name = path.last().cloned().unwrap_or_default();
                let field_names = self.struct_field_names.get(&struct_name).cloned();
                if let (BasicValueEnum::StructValue(sv), Some(field_names)) = (scrut, field_names) {
                    for field_pat in fields {
                        let Some(idx) = field_names.iter().position(|n| n == &field_pat.name)
                        else {
                            continue;
                        };
                        let field_val = self
                            .builder
                            .build_extract_value(sv, idx as u32, "field")
                            .unwrap();
                        if let Some(sub_pat) = &field_pat.pattern {
                            self.bind_pattern_values(sub_pat, field_val)?;
                        } else {
                            let synthetic = Pattern {
                                kind: PatternKind::Binding(field_pat.name.clone()),
                                span: field_pat.span.clone(),
                            };
                            self.bind_pattern_values(&synthetic, field_val)?;
                        }
                    }
                }
                Ok(())
            }
            // `name @ subpattern` — bind the outer alias to the whole
            // scrutinee value, then recurse into the sub-pattern so any
            // nested bindings (`whole @ Some(x)` → `x`) also materialize.
            // The alias reuses the leaf-`Binding` machinery (alloca +
            // surface-type plumbing) via a synthetic `Binding` at the
            // AtBinding's own span — the span the typechecker recorded
            // the alias against (`check_pattern_against`'s AtBinding arm).
            // Without this arm, `@` bindings fell through to `_ => Ok(())`
            // and were never bound in compiled code (the match-condition
            // side had the same gap — see `compile_pattern_condition`).
            PatternKind::AtBinding {
                name,
                pattern: inner,
                by_ref,
            } => {
                // `ref name @ PATTERN` (design.md § @ Bindings): the whole
                // subtree borrows — the scrutinee's owner keeps drop
                // responsibility (`pattern_consumes_field` returns false,
                // so no source-cap zeroing happens), and the bindings here
                // must not register their own heap cleanup against the
                // same buffers. Reuse the `pattern_binding_is_borrow`
                // suppression (the borrow-returning-scrutinee mechanism)
                // for the duration of the subtree bind; the typechecker
                // recorded `Ref` borrow modes for every binding span in
                // the subtree, so each leaf also gets the ref-shim ABI.
                // The bindings are copy-aliases (slice-3a semantics) —
                // correct for immutable `ref`; write-through is not a
                // requirement (`mut ref name @` does not exist).
                let saved_borrow_flag = self.pattern_binding_is_borrow;
                if *by_ref {
                    self.pattern_binding_is_borrow = true;
                }
                let synthetic = Pattern {
                    kind: PatternKind::Binding(name.clone()),
                    span: pattern.span.clone(),
                };
                let bind_result = self
                    .bind_pattern_values(&synthetic, scrut)
                    .and_then(|()| self.bind_pattern_values(inner, scrut));
                self.pattern_binding_is_borrow = saved_borrow_flag;
                bind_result?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Slice 3b: emit a leaf binding whose shim alloca points **into**
    /// the scrutinee's storage (via GEP) rather than into a local copy
    /// of the field value. This is what makes mutation through a
    /// `mut ref`-typed match-arm binding flow back to the original
    /// scrutinee — `set_to(name, v)` where `name: mut ref T` writes
    /// through the GEP'd pointer, mutating the source `foo.name`.
    ///
    /// The shim mechanic itself is identical to slice 3a's
    /// `pattern_binding_borrow_modes` path: a `ptr` alloca registered
    /// in `variables` + `ref_params`, so subsequent `load_variable`
    /// auto-derefs back to the value and `compile_call`'s `is_ref`
    /// arg path uses `get_data_ptr` to pass the stored pointer. The
    /// only thing that changes between 3a and 3b is **what** pointer
    /// the shim stores: 3a stores the address of a local value-alloca;
    /// 3b stores a GEP into the scrutinee.
    pub(super) fn emit_ref_leaf_binding_at_ptr(
        &mut self,
        name: &str,
        field_ptr: PointerValue<'ctx>,
        inner_ty: BasicTypeEnum<'ctx>,
        debug_label: &str,
    ) {
        let fn_val = self.current_fn.unwrap();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let shim_alloca =
            self.create_entry_alloca(fn_val, &format!("{}.{}", name, debug_label), ptr_ty.into());
        self.builder.build_store(shim_alloca, field_ptr).unwrap();
        self.variables.insert(
            name.to_string(),
            VarSlot {
                ptr: shim_alloca,
                ty: ptr_ty.into(),
            },
        );
        self.ref_params.insert(name.to_string(), inner_ty);
    }

    /// Slice 3b pointer-source twin of `bind_pattern_values`. Walks the
    /// pattern and, for each leaf binding, emits a GEP into the
    /// scrutinee pointer rather than reconstructing the field value
    /// and shimming a local copy. Returns `Some(())` when the pattern
    /// shape was handled by this path; `None` falls back to the
    /// value-source `bind_pattern_values` (which slice 3a's shim still
    /// adapts to the ref-binding ABI shape).
    ///
    /// Coverage at slice 3b landing:
    /// - `PatternKind::Struct` on a plain (non-enum) struct: GEP into
    ///   each field by index; recurse on sub-pattern OR emit leaf
    ///   binding directly for shorthand.
    /// - `PatternKind::TupleVariant` on a non-shared enum: GEP into
    ///   the layout's payload word for each positional sub-pattern;
    ///   emit leaf binding directly (nested destructure under enum
    ///   payload not supported at this slice — return `None` to defer
    ///   to value-source path for those cases).
    /// - `PatternKind::Wildcard`: bind nothing.
    /// - `PatternKind::Binding`: the scrutinee pointer IS what we want
    ///   to alias; register a leaf binding at the pointer with the
    ///   pointee type.
    /// - Anything else (slice patterns, range patterns, or-patterns,
    ///   at-bindings, literals): return `None`, defer to the existing
    ///   value-source pipeline.
    pub(super) fn bind_pattern_values_via_ptr(
        &mut self,
        pattern: &Pattern,
        scrut_ptr: PointerValue<'ctx>,
        pointee_ty: StructType<'ctx>,
    ) -> Result<Option<()>, String> {
        match &pattern.kind {
            PatternKind::Wildcard => Ok(Some(())),
            PatternKind::Binding(name) => {
                // Unit-variant guard mirrors the value-source path.
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                if self.enum_tag_for_variant(variant_name).is_some() {
                    return Ok(Some(()));
                }
                // The scrutinee pointer IS the data we want to alias.
                // Inner type is the full pointee struct.
                self.emit_ref_leaf_binding_at_ptr(
                    name,
                    scrut_ptr,
                    pointee_ty.into(),
                    "refshim.ptr",
                );
                Ok(Some(()))
            }
            PatternKind::Struct {
                path,
                fields,
                has_rest: _,
            } => {
                let struct_name = path.last().cloned().unwrap_or_default();
                // Plain user struct: GEP into each field by name-resolved
                // index. Enum struct-variants would also reach here, but
                // those require tag-aware payload routing — defer to the
                // value-source path until slice 3c.
                let Some(field_names) = self.struct_field_names.get(&struct_name).cloned() else {
                    return Ok(None);
                };
                let Some(&struct_ty) = self.struct_types.get(&struct_name) else {
                    return Ok(None);
                };
                if struct_ty != pointee_ty {
                    return Ok(None);
                }
                for field_pat in fields {
                    let Some(idx) = field_names.iter().position(|n| n == &field_pat.name) else {
                        continue;
                    };
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            struct_ty,
                            scrut_ptr,
                            idx as u32,
                            &format!("{}.fld.{}.ptr", struct_name, field_pat.name),
                        )
                        .unwrap();
                    let field_ty =
                        struct_ty
                            .get_field_type_at_index(idx as u32)
                            .ok_or_else(|| {
                                format!("field index {} out of range for {}", idx, struct_name)
                            })?;
                    if let Some(sub_pat) = &field_pat.pattern {
                        // For nested destructure under a struct field,
                        // recurse only when the sub-pattern is a Binding
                        // / Wildcard (the GEP semantics compose). For
                        // deeper structural sub-patterns under a ref
                        // scrutinee, fall back to value-source: it
                        // preserves correctness at the cost of the
                        // copy-shim semantic for that sub-tree.
                        match (&sub_pat.kind, field_ty) {
                            (PatternKind::Wildcard, _) => {}
                            (PatternKind::Binding(sub_name), _) => {
                                // Same unit-variant guard.
                                let variant_name = sub_name.rsplit('.').next().unwrap_or(sub_name);
                                if self.enum_tag_for_variant(variant_name).is_some() {
                                    continue;
                                }
                                self.emit_ref_leaf_binding_at_ptr(
                                    sub_name,
                                    field_ptr,
                                    field_ty,
                                    "fld.refshim",
                                );
                            }
                            (_, BasicTypeEnum::StructType(field_struct_ty)) => {
                                if let Some(()) = self.bind_pattern_values_via_ptr(
                                    sub_pat,
                                    field_ptr,
                                    field_struct_ty,
                                )? {
                                    // ok
                                } else {
                                    return Ok(None);
                                }
                            }
                            _ => return Ok(None),
                        }
                    } else {
                        // Shorthand: bind field name as a ref leaf.
                        self.emit_ref_leaf_binding_at_ptr(
                            &field_pat.name,
                            field_ptr,
                            field_ty,
                            "fld.refshim",
                        );
                    }
                }
                Ok(Some(()))
            }
            PatternKind::TupleVariant { path, patterns } => {
                let variant_name = path.last().map(|s| s.as_str()).unwrap_or("");
                // Locate the enum layout whose llvm_type matches the
                // pointee (variant-name collisions across enums are
                // disambiguated by struct identity, mirroring the
                // value-source `TupleVariant` arm).
                let Some((_enum_name, layout)) = self
                    .enum_layouts
                    .iter()
                    .find(|(_, l)| l.tags.contains_key(variant_name) && l.llvm_type == pointee_ty)
                    .or_else(|| {
                        self.enum_layouts
                            .iter()
                            .find(|(_, l)| l.tags.contains_key(variant_name))
                    })
                    .map(|(n, l)| (n.clone(), l.clone()))
                else {
                    return Ok(None);
                };
                // Shared (heap) enums use a different pointee shape (the
                // refcount header sits at index 0, tag at 1, payload at
                // 2+). The current call site routes shared enums
                // through the value-source path's heap-pointer branch;
                // mirror that by falling back here.
                if layout.is_shared {
                    return Ok(None);
                }
                let offsets: Vec<(usize, usize)> = layout
                    .field_word_offsets
                    .get(variant_name)
                    .cloned()
                    .unwrap_or_else(|| (0..patterns.len()).map(|i| (i, 1)).collect());
                for (i, sub_pat) in patterns.iter().enumerate() {
                    let (start_word, num_words) = offsets.get(i).copied().unwrap_or((i, 1));
                    let word_idx = (start_word + 1) as u32; // +1 for the tag word
                    let field_ty = layout.llvm_type.get_field_type_at_index(word_idx).unwrap();
                    // Multi-word source fields (aggregate payloads like
                    // String / Vec / user struct that span >1 word) need
                    // word-stream reconstruction — defer to the value-
                    // source path for those. Single-word source fields
                    // (primitive payloads — i64, bool, char) GEP cleanly
                    // to the first payload word. Note: `Option`'s seeded
                    // layout sets `num_words` to `option_payload_words`
                    // (the cross-variant max, often 3), but the actual
                    // payload of `Option[i64]` is one i64 — the layout's
                    // wider `num_words` is an over-estimate for the
                    // single-word use. Recognize this shape by checking
                    // the sub-pattern (a leaf `Binding` / `Wildcard`)
                    // and the first payload word's LLVM type (primitive
                    // int / bool / float).
                    let sub_is_leaf = matches!(
                        sub_pat.kind,
                        PatternKind::Binding(_) | PatternKind::Wildcard
                    );
                    let first_word_is_primitive = matches!(
                        field_ty,
                        BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)
                    );
                    let ok_single_word = num_words == 1;
                    let ok_padded_primitive =
                        num_words > 1 && sub_is_leaf && first_word_is_primitive;
                    if !ok_single_word && !ok_padded_primitive {
                        return Ok(None);
                    }
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            layout.llvm_type,
                            scrut_ptr,
                            word_idx,
                            &format!("{}.pl.{}.ptr", variant_name, i),
                        )
                        .unwrap();
                    match &sub_pat.kind {
                        PatternKind::Wildcard => {}
                        PatternKind::Binding(sub_name) => {
                            let vn = sub_name.rsplit('.').next().unwrap_or(sub_name);
                            if self.enum_tag_for_variant(vn).is_some() {
                                continue;
                            }
                            self.emit_ref_leaf_binding_at_ptr(
                                sub_name,
                                field_ptr,
                                field_ty,
                                "pl.refshim",
                            );
                        }
                        _ => return Ok(None),
                    }
                }
                Ok(Some(()))
            }
            // `name @ subpattern` under a ref scrutinee. Mirrors the
            // value-source `AtBinding` arm (alias the outer `name` to the
            // whole matched value, then recurse into the sub-pattern) but
            // aliases the scrutinee *pointer* rather than copying the
            // value — so `name: mut ref T` and any nested mut-ref leaf
            // both write through to the scrutinee storage. The outer
            // `name` aliases the full pointee; the sub-pattern recurses
            // through the same pointer (e.g. `whole @ Bag { n }` → `n`
            // GEPs into the field while `whole` aliases the struct).
            //
            // If the sub-pattern shape isn't via_ptr-handleable, the
            // recursion returns `None` and the caller re-runs the
            // value-source path on the whole `AtBinding` (re-binding both
            // `name` and the sub-pattern via the slice-3a copy-shim —
            // correct, just not write-through). The outer shim emitted
            // here is then harmlessly overwritten in `variables`,
            // mirroring the `Struct` arm's mid-emit `Ok(None)` fallback.
            PatternKind::AtBinding {
                name,
                pattern: inner,
                // `by_ref` is a no-op here: the pointer-source path
                // already binds borrows into the scrutinee storage —
                // exactly what `ref name @` asks for.
                by_ref: _,
            } => {
                self.emit_ref_leaf_binding_at_ptr(name, scrut_ptr, pointee_ty.into(), "at.refshim");
                self.bind_pattern_values_via_ptr(inner, scrut_ptr, pointee_ty)
            }
            // Or-patterns, slice patterns, range patterns, and literals
            // fall back to the value-source path. The value-source path
            // under a ref scrutinee still produces the correct
            // match-condition + copy-semantic binding (per slice 3a);
            // slice 3b's pull-signal trigger names the shapes that need
            // write-through, and the remaining shapes above don't appear
            // in those triggers yet.
            _ => Ok(None),
        }
    }
}
