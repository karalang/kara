//! Expression-operator codegen: tuples, field access/store, casts,
//! binops, unaryops, and slice coercion.
//!
//! Houses tuple construction (`compile_tuple`, `compile_tuple_index`),
//! field access/store (`compile_field_access`, `compile_field_store`,
//! `field_index_for`, `type_name_of_expr`, `type_name_of`),
//! `compile_cast`, short-circuit / binary / struct-eq / string-binop /
//! float-binop emission (`compile_short_circuit`, `compile_binop`,
//! `compile_struct_eq`, `compile_string_binop`, `compile_float_binop`,
//! `to_float`), `compile_unaryop`, and slice coercion + range slicing
//! (`coerce_to_slice`, `build_slice_header`, `compile_range_slice`).

use crate::ast::*;

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValue, BasicValueEnum, PointerValue, VectorValue};
use inkwell::AddressSpace;
use inkwell::{FloatPredicate, IntPredicate};

impl<'ctx> super::Codegen<'ctx> {
    pub(super) fn compile_tuple(&mut self, elems: &[Expr]) -> Result<BasicValueEnum<'ctx>, String> {
        let vals: Vec<BasicValueEnum<'ctx>> = elems
            .iter()
            .map(|e| self.compile_expr(e))
            .collect::<Result<_, _>>()?;
        // Move-aware: tuple construction takes ownership of each
        // element. For Vec / String / shared-RC / Vec-bearing-struct
        // elements that arrive as identifiers, suppress the source
        // binding's scope-exit cleanup the same way function-call
        // arg passing does (`suppress_source_vec_cleanup_for_arg`,
        // `call_dispatch.rs:1033`). Without this, a Vec passed into a
        // tuple keeps its cap word non-zero — the original binding's
        // `CleanupAction::FreeVecBuffer` fires at scope exit and
        // free()s the same buffer that the *consumer* of the tuple
        // (enum payload, destructure binding, helper function) now
        // owns. Surfaces under LLJIT as `_BUG_IN_CLIENT_OF_LIBMALLOC_
        // POINTER_BEING_FREED_WAS_NOT_ALLOCATED` (bug 2 of N from
        // phase-7 L560 W3.3 routing); AOT masks via post-codegen O2
        // passes that elide the redundant free, but the bug is real
        // and a future codegen perturbation would re-expose it.
        for elem_expr in elems {
            self.suppress_source_vec_cleanup_for_arg(elem_expr);
        }
        let types: Vec<BasicTypeEnum<'ctx>> = vals.iter().map(|v| v.get_type()).collect();
        let st = self.context.struct_type(&types, false);
        let mut agg = st.get_undef();
        for (idx, val) in vals.iter().enumerate() {
            agg = self
                .builder
                .build_insert_value(agg, *val, idx as u32, "elem")
                .unwrap()
                .into_struct_value();
        }
        Ok(agg.into())
    }

    /// Lower a borrow-return expression to the ADDRESS of its source,
    /// for functions whose declared return type is `ref T` / `mut ref T`
    /// (`current_fn_returns_ref`). The LLVM return type is a thin `ptr`;
    /// the normal value-materializing path produces the borrowed value
    /// instead, which fails module verification (`ret {ptr,i64,i64} / ptr`,
    /// `ret i64 / ptr`) — see `B-2026-06-07-5`.
    ///
    /// Handles the source-pinned shapes the front-end admits as valid
    /// borrow returns (a borrow that traces to a `ref` parameter):
    ///   - `fn f(s: ref T) -> ref T { s }` — forward the borrow itself:
    ///     `get_data_ptr` yields the pointer the ref param holds (the
    ///     caller's storage).
    ///   - `fn f(u: ref U) -> ref F { u.field }` — GEP into the caller's
    ///     struct through the ref param. Mirrors the proven ref-param
    ///     field-STORE path in `compile_field_store`.
    ///
    /// Returns `None` for any other shape (owned local / temporary /
    /// `if`/`match` / call chain), so callers fall back to the existing
    /// return path. Dangling shapes (returning a local / owned value) are
    /// rejected earlier by the ownership source-pinning check; non-dangling
    /// shapes not yet handled here (`longer`-style `if` of two ref params,
    /// `first_word`-style method chains) are tracked follow-ons.
    pub(super) fn compile_ref_return_ptr(&mut self, expr: &Expr) -> Option<PointerValue<'ctx>> {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                // Only a borrow that is itself a `ref` parameter can be
                // forwarded — `get_data_ptr` returns the held caller
                // pointer for a ref param (and the local alloca for an
                // owned binding, which would dangle: excluded here).
                if self.ref_params.contains_key(name) {
                    self.get_data_ptr(name)
                } else {
                    None
                }
            }
            // `ref self` returned directly (`fn this(ref self) -> ref Self`):
            // forward the receiver borrow, same as a ref parameter.
            ExprKind::SelfValue => {
                if self.ref_params.contains_key("self") {
                    self.get_data_ptr("self")
                } else {
                    None
                }
            }
            // Field reached through a `ref` parameter or `ref self` receiver
            // (`u.name` / `self.name`): GEP into the borrowed struct.
            ExprKind::FieldAccess { object, field } => {
                let base = match &object.kind {
                    ExprKind::Identifier(b) => Some(b.as_str()),
                    ExprKind::SelfValue => Some("self"),
                    _ => None,
                }?;
                if let Some(&BasicTypeEnum::StructType(struct_ty)) = self.ref_params.get(base) {
                    let idx = self.field_index_for(object, field)?;
                    let base_ptr = self.get_data_ptr(base)?;
                    return self
                        .builder
                        .build_struct_gep(
                            struct_ty,
                            base_ptr,
                            idx,
                            &format!("ret_borrow_{}", field),
                        )
                        .ok();
                }
                None
            }
            // Borrow returned from a conditional: compile each branch to a
            // borrow pointer and phi them at the merge. Covers `longer`-style
            // `if a.len() > b.len() { a } else { b }` (Tier 2 — multi-source
            // overapproximation, design.md § Feature 4 Part 3). A value `if`
            // requires an `else`; both branches must themselves be
            // borrowable, else the whole `if` is unsupported (`None`).
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                let else_e = else_branch.as_deref()?;
                let fn_val = self.current_fn?;
                let cond = self.compile_expr(condition).ok()?.into_int_value();
                let then_bb = self.context.append_basic_block(fn_val, "refret.then");
                let else_bb = self.context.append_basic_block(fn_val, "refret.else");
                let merge_bb = self.context.append_basic_block(fn_val, "refret.merge");
                self.builder
                    .build_conditional_branch(cond, then_bb, else_bb)
                    .ok()?;
                self.builder.position_at_end(then_bb);
                let then_ptr = self.ref_return_ptr_of_block(then_block)?;
                let then_end = self.builder.get_insert_block()?;
                self.builder.build_unconditional_branch(merge_bb).ok()?;
                self.builder.position_at_end(else_bb);
                let else_ptr = self.compile_ref_return_ptr(else_e)?;
                let else_end = self.builder.get_insert_block()?;
                self.builder.build_unconditional_branch(merge_bb).ok()?;
                self.builder.position_at_end(merge_bb);
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let phi = self.builder.build_phi(ptr_ty, "refret.phi").ok()?;
                phi.add_incoming(&[
                    (&then_ptr as &dyn BasicValue, then_end),
                    (&else_ptr as &dyn BasicValue, else_end),
                ]);
                Some(phi.as_basic_value().into_pointer_value())
            }
            // Borrow returned from a `match` (sibling of the `if` arm):
            // lower the scalar selector, branch per arm, compile each arm
            // body to a borrow pointer, and phi them at the merge. Covers
            // `match which { 0 => a, 1 => b, _ => c }` selecting among `ref`
            // params/fields. Gated — in lockstep with `classify_borrow_return`
            // (src/ownership/ref_return.rs) — to guard-free arms whose
            // patterns are integer/char/bool literals or `_`, over a scalar
            // (int) scrutinee. That keeps the scrutinee free of heap/drop
            // obligations (no String/enum destructure), so no per-arm scope
            // frame is needed. Any shape this rejects (returns `None` before
            // emitting IR) is reported upstream as `UnsupportedForm` — never
            // miscompiled. Destructuring patterns, guards, and non-scalar
            // scrutinees are tracked follow-ons (B-2026-06-07-5).
            ExprKind::Match { scrutinee, arms } => {
                if arms.is_empty() || !arms.iter().all(Self::ref_return_match_arm_ok) {
                    return None;
                }
                let fn_val = self.current_fn?;
                let scrut = self.compile_expr(scrutinee).ok()?;
                if !scrut.is_int_value() {
                    return None;
                }
                let merge_bb = self
                    .context
                    .append_basic_block(fn_val, "refret.match.merge");
                let mut incoming: Vec<(
                    PointerValue<'ctx>,
                    inkwell::basic_block::BasicBlock<'ctx>,
                )> = Vec::new();
                let mut next_bb = self.context.append_basic_block(fn_val, "refret.match.arm0");
                self.builder.build_unconditional_branch(next_bb).ok()?;
                for (i, arm) in arms.iter().enumerate() {
                    let arm_bb = next_bb;
                    let is_last = i + 1 == arms.len();
                    let fail_bb = if is_last {
                        self.context
                            .append_basic_block(fn_val, "refret.match.nofall")
                    } else {
                        self.context
                            .append_basic_block(fn_val, &format!("refret.match.arm{}", i + 1))
                    };
                    next_bb = fail_bb;
                    self.builder.position_at_end(arm_bb);
                    let cond = self.compile_pattern_condition(&arm.pattern, scrut).ok()?;
                    let body_bb = self
                        .context
                        .append_basic_block(fn_val, &format!("refret.match.body{}", i));
                    self.builder
                        .build_conditional_branch(cond.into_int_value(), body_bb, fail_bb)
                        .ok()?;
                    self.builder.position_at_end(body_bb);
                    let ptr = self.compile_ref_return_ptr(&arm.body)?;
                    let end = self.builder.get_insert_block()?;
                    self.builder.build_unconditional_branch(merge_bb).ok()?;
                    incoming.push((ptr, end));
                }
                // The trailing fail block is unreachable for an exhaustive
                // match (the wildcard / final arm always matches); give it a
                // terminator so the function verifies.
                self.builder.position_at_end(next_bb);
                self.builder.build_unreachable().ok()?;
                self.builder.position_at_end(merge_bb);
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let phi = self.builder.build_phi(ptr_ty, "refret.match.phi").ok()?;
                let incoming_dyn: Vec<(&dyn BasicValue, inkwell::basic_block::BasicBlock<'ctx>)> =
                    incoming
                        .iter()
                        .map(|(p, b)| (p as &dyn BasicValue, *b))
                        .collect();
                phi.add_incoming(&incoming_dyn);
                Some(phi.as_basic_value().into_pointer_value())
            }
            // A block in borrow-return position (an `if`/`else` arm, or a
            // bare `{ ... }`): only a statement-free borrow tail is
            // supported today.
            ExprKind::Block(b) => self.ref_return_ptr_of_block(b),
            _ => None,
        }
    }

    /// Borrow pointer produced by a block's tail expression, for blocks
    /// sitting in borrow-return position. Tier-2 scope: statement-free
    /// blocks only — a block with preceding statements returns `None`, so
    /// the source-pinning check reports it as a not-yet-supported form
    /// rather than miscompiling.
    fn ref_return_ptr_of_block(&mut self, b: &Block) -> Option<PointerValue<'ctx>> {
        if !b.stmts.is_empty() {
            return None;
        }
        let tail = b.final_expr.as_deref()?;
        self.compile_ref_return_ptr(tail)
    }

    /// A `match` arm is lowerable in borrow-return position only when it has
    /// no guard and its pattern is a scalar literal (`Integer`/`Char`/`Bool`)
    /// or `_`. Kept identical to the classify-side gate in
    /// `src/ownership/ref_return.rs::match_arm_borrowable_shape` so the two
    /// stay in lockstep (codegen accepting more than classify would
    /// miscompile; classify accepting more would dangle).
    fn ref_return_match_arm_ok(arm: &MatchArm) -> bool {
        arm.guard.is_none()
            && matches!(
                arm.pattern.kind,
                PatternKind::Wildcard
                    | PatternKind::Literal(
                        LiteralPattern::Integer(..)
                            | LiteralPattern::Char(..)
                            | LiteralPattern::Bool(..)
                    )
            )
    }

    pub(super) fn compile_field_access(
        &mut self,
        object: &Expr,
        field: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Primitive-type associated constants — `i64.MAX` /
        // `f64.INFINITY` / `usize.MAX` etc. parse as
        // `FieldAccess(Identifier("i64"), "MAX")`. Intercept before the
        // normal field-access path so the bare primitive identifier
        // doesn't fall through to a generic compile_expr that would
        // either panic or produce wrong codegen. Mirrors the typechecker
        // and interpreter early-intercepts for the same expression
        // shape.
        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(cv) = crate::prelude::lookup_primitive_const(name, field) {
                return Ok(self.compile_primitive_const(cv));
            }
            // Unit-variant enum access via `EnumName.Variant` (e.g.
            // `Json.Null`, `Ordering.Equal`). Parser turns this into a
            // FieldAccess with the enum name as the object — without an
            // explicit arm here, the access falls through to the generic
            // struct-field path which returns the `i64 0` placeholder.
            // Mirrors the `Identifier(name)`-bare path that
            // `try_unit_enum_variant` handles elsewhere, but scoped to
            // the `EnumName.Variant` form.
            if let Some(layout) = self.enum_layouts.get(name) {
                if layout.tags.contains_key(field)
                    && layout.field_counts.get(field).copied().unwrap_or(0) == 0
                {
                    if let Some(ev) = self.try_unit_enum_variant(field) {
                        return Ok(ev);
                    }
                }
            }
        }
        // FFI union field read (phase 5 line 569 slice 4). Detect a
        // union-typed receiver — by-binding via `var_type_names` →
        // `union_types`, or `self` field access from within an impl
        // method on the union (the latter is not v1-surface today but
        // the lookup is symmetric and cheap) — and lower as a typed
        // load through the storage alloca. The typechecker's slice 2a
        // `unsafe { }` gate already approves the read at this site; we
        // simply emit the load using the field's declared LLVM type
        // (recovered from `union_field_types`) rather than the storage
        // struct's first-member type, so a union with primary slot
        // `{ i64 }` accessed via `u.u32val` reads exactly 4 bytes.
        if let Some(loaded) = self.try_compile_union_field_read(object, field) {
            return Ok(loaded);
        }
        // Indexed-shared-struct receiver: `nodes[i].field` where
        // `nodes: Vec[Shared(N)]`. Mirror of `compile_field_store`'s
        // Index branch — load the heap pointer at `nodes[i]`, GEP into
        // the heap struct's field, return the typed load. Without this,
        // the access falls through to the generic Struct-value extract
        // path which returns `i64 0` for any shared-struct receiver.
        if let ExprKind::Index {
            object: inner,
            index,
        } = &object.kind
        {
            if let ExprKind::Identifier(outer_name) = &inner.kind {
                if let Some(elem_te) = self.var_elem_type_exprs.get(outer_name.as_str()).cloned() {
                    if let TypeKind::Path(path) = &elem_te.kind {
                        if let Some(seg) = path.segments.first() {
                            if let Some(info) = self.shared_types.get(seg.as_str()).cloned() {
                                if !info.is_enum {
                                    let outer_name = outer_name.clone();
                                    let (elem_ptr, _) =
                                        if self.vec_elem_types.contains_key(outer_name.as_str()) {
                                            self.lower_indexed_elem_ptr_vec(&outer_name, index)?
                                        } else if self
                                            .slice_elem_types
                                            .contains_key(outer_name.as_str())
                                        {
                                            self.lower_indexed_elem_ptr_slice(&outer_name, index)?
                                        } else {
                                            let zero = self.context.i64_type().const_zero();
                                            return Ok(zero.into());
                                        };
                                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                                    let heap_ptr = self
                                        .builder
                                        .build_load(ptr_ty, elem_ptr, "idx.shared.read")
                                        .unwrap()
                                        .into_pointer_value();
                                    if let Some(names) = self.struct_field_names.get(seg) {
                                        if let Some(idx) = names.iter().position(|n| n == field) {
                                            let field_ptr = self
                                                .builder
                                                .build_struct_gep(
                                                    info.heap_type,
                                                    heap_ptr,
                                                    (idx + 1) as u32,
                                                    &format!("sh_idx_{}", field),
                                                )
                                                .unwrap();
                                            if self.niche_field_inner_heap_type(seg, idx).is_some()
                                            {
                                                return Ok(
                                                    self.niche_load_option_field(field_ptr, field)
                                                );
                                            }
                                            let field_ty = info
                                                .heap_type
                                                .get_field_type_at_index((idx + 1) as u32)
                                                .unwrap();
                                            return Ok(self
                                                .builder
                                                .build_load(field_ty, field_ptr, field)
                                                .unwrap());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // SoA-laid-out Vec element field read: `entities[i].field`. The
        // generic struct-field path returns the `i64 0` placeholder for an
        // Index receiver (it never recurses into `compile_index`), so the
        // headline SoA access shape is handled explicitly: materialize the
        // element via `compile_soa_index_read`, then extract the requested
        // field by its position in the element struct. The direct group-
        // indexed load that skips materialization entirely is the deferred
        // optimization sub-slice; this materialize-then-extract form is
        // semantically identical, just not the cache-optimal lowering.
        if let ExprKind::Index {
            object: inner,
            index,
        } = &object.kind
        {
            if let ExprKind::Identifier(soa_var) = &inner.kind {
                if let Some(soa) = self.soa_layouts.get(soa_var.as_str()).cloned() {
                    if let Some(names) = self.struct_field_names.get(&soa.struct_name) {
                        if let Some(fidx) = names.iter().position(|n| n == field) {
                            let soa_var = soa_var.clone();
                            let elem = self
                                .compile_soa_index_read(&soa_var, index)?
                                .into_struct_value();
                            return Ok(self
                                .builder
                                .build_extract_value(elem, fidx as u32, field)
                                .unwrap());
                        }
                    }
                }
            }
        }
        // Plain owned-struct Vec element field read: `entities[i].field`
        // where `entities: Vec[Entity]` and `Entity` is a non-shared,
        // non-SoA user struct. The shared-struct branch above covers
        // `Vec[Shared(T)]`; the SoA branch covers layout-annotated
        // Vec[T]. Without this arm, plain `Vec[Struct][i].field` falls
        // through to the generic struct-field path which returns the
        // `i64 0` placeholder, so the access silently produces 0 for
        // any field — the gap surfaced (but unfixed) during the SoA
        // work on 2026-05-29 (`tests/codegen.rs::test_e2e_soa_whole_element_matches_aos`
        // used whole-element binding instead of direct field access for
        // exactly this reason). Materializes the element via
        // `compile_vec_index`, then extracts the field by its position
        // in the element struct. Primitive (non-heap) fields only:
        // for heap-bearing fields the materialized copy aliases the
        // outer Vec's buffer exactly as `let e = entities[i]` already
        // does, so this arm matches the latent alias hazard of the
        // existing whole-element-binding path — not a new exposure.
        if let ExprKind::Index {
            object: inner,
            index,
        } = &object.kind
        {
            if let ExprKind::Identifier(outer_name) = &inner.kind {
                if self.vec_elem_types.contains_key(outer_name.as_str()) {
                    if let Some(elem_te) =
                        self.var_elem_type_exprs.get(outer_name.as_str()).cloned()
                    {
                        if let TypeKind::Path(path) = &elem_te.kind {
                            if let Some(seg) = path.segments.first() {
                                if self.struct_types.contains_key(seg.as_str())
                                    && !self.shared_types.contains_key(seg.as_str())
                                {
                                    if let Some(names) = self.struct_field_names.get(seg).cloned() {
                                        if let Some(fidx) = names.iter().position(|n| n == field) {
                                            let outer_name = outer_name.clone();
                                            let elem = self
                                                .compile_vec_index(&outer_name, index)?
                                                .into_struct_value();
                                            return Ok(self
                                                .builder
                                                .build_extract_value(elem, fidx as u32, field)
                                                .unwrap());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Call-chain field access on a shared-struct return — bug #8.
        // `helper().val` where `helper() -> SharedT` lowers the call to
        // a pointer to the heap object; we GEP into the field, load, then
        // rc_dec the temp so the returned RC=1 doesn't leak. The bug #7
        // fix (move-out rc_inc in the callee) ensures the returned ptr is
        // RC≥1, so the caller owns one ref that must be released after
        // the field load. Without this branch the access falls through to
        // the generic `StructValue` path which sees a `PointerValue` and
        // silently returns `i64 0`. Covers `Call`, `MethodCall`, and
        // `Path`-based associated calls (`T.builder()`) — any shape whose
        // static return type names a known shared struct.
        if let Some((type_name, info)) = self.shared_type_for_call_like(object) {
            if !info.is_enum {
                if let Some(names) = self.struct_field_names.get(&type_name).cloned() {
                    if let Some(idx) = names.iter().position(|n| n == field) {
                        let ptr = self.compile_expr(object)?.into_pointer_value();
                        // Fields start at heap index 1 (index 0 is refcount).
                        let field_ptr = self
                            .builder
                            .build_struct_gep(
                                info.heap_type,
                                ptr,
                                (idx + 1) as u32,
                                &format!("sh_call_{}", field),
                            )
                            .unwrap();
                        let loaded = if self.niche_field_inner_heap_type(&type_name, idx).is_some()
                        {
                            self.niche_load_option_field(field_ptr, field)
                        } else {
                            let field_ty = info
                                .heap_type
                                .get_field_type_at_index((idx + 1) as u32)
                                .unwrap();
                            self.builder.build_load(field_ty, field_ptr, field).unwrap()
                        };
                        // `Option[shared T]` field: the upcoming
                        // `emit_rc_dec(ptr)` runs the outer Node's
                        // recursive drop fn, which walks the
                        // `next: Option[shared T]` field and dec's
                        // its inner ptr. The loaded SSA register
                        // would then alias freed memory — a
                        // use-after-free for any subsequent access
                        // through the returned value
                        // (`match v { Some(n) => n.val }`, `v.next.val`,
                        // etc.). Bump the inner ptr's RC here so the
                        // recursive drop's dec brings it back to the
                        // original count; the caller (let-stmt
                        // path's `shared_option_info` detection, the
                        // match-scrutinee binding, or the next
                        // FieldAccess hop in a chain) takes ownership
                        // of the +1 with its own RcDecOption.
                        // Mirrors how the let-stmt's
                        // `shared_option_info` arm doesn't inc for
                        // an aliasing source — the +1 emitted here
                        // *is* the alias source's transfer of one
                        // owned ref into the field-access result.
                        if let Some(field_te) = self
                            .struct_field_type_exprs
                            .get(&type_name)
                            .and_then(|v| v.get(idx))
                            .cloned()
                        {
                            if let Some((_, inner_info)) =
                                self.option_inner_shared_type_for_type_expr(&field_te)
                            {
                                self.emit_option_inner_rc_inc_for_loaded(
                                    loaded,
                                    inner_info.heap_type,
                                );
                            }
                        }
                        // The call-result temp owns one ref (RC=1 from the
                        // callee's move-out inc + scope-exit dec under bug
                        // #7). Release it now — the field value has been
                        // read into a register and no longer depends on
                        // the heap object.
                        self.emit_refcount_dec_by_type(info.heap_type, ptr);
                        return Ok(loaded);
                    }
                }
            }
        }

        // Shared type: object compiles to a pointer; field access via GEP.
        // `compile_expr(object)` yields the heap pointer for every receiver
        // shape — `load_variable` applies the right number of loads (one for an
        // owned local / the constructor binding, two for a `ref self` param via
        // its ref-param deref). The fix for `self.field` in a shared method was
        // purely making `shared_type_for_expr` resolve `SelfValue` (so this
        // shared GEP path is taken at all); before that, `self.field` fell to
        // the const-0 fallback below and returned 0 for every field.
        if let Some((type_name, info)) = self.shared_type_for_expr(object) {
            if !info.is_enum {
                let ptr = self.compile_expr(object)?.into_pointer_value();
                if let Some(names) = self.struct_field_names.get(&type_name) {
                    if let Some(idx) = names.iter().position(|n| n == field) {
                        // Fields start at heap index `base` — 1 past
                        // the refcount, or 0 for a phase-D headerless
                        // member (see `shared_gep_layout`).
                        let (gep_ty, base) = self.shared_gep_layout(&type_name, info.heap_type);
                        let field_ptr = self
                            .builder
                            .build_struct_gep(
                                gep_ty,
                                ptr,
                                idx as u32 + base,
                                &format!("sh_{}", field),
                            )
                            .unwrap();
                        if self.niche_field_inner_heap_type(&type_name, idx).is_some() {
                            return Ok(self.niche_load_option_field(field_ptr, field));
                        }
                        let field_ty = gep_ty.get_field_type_at_index(idx as u32 + base).unwrap();
                        return Ok(self.builder.build_load(field_ty, field_ptr, field).unwrap());
                    }
                }
            }
        }

        let obj_val = self.compile_expr(object)?;
        if let BasicValueEnum::StructValue(sv) = obj_val {
            // Look up field index from struct type name in object's identifier
            let field_idx = self.field_index_for(object, field);
            if let Some(idx) = field_idx {
                let extracted = self.builder.build_extract_value(sv, idx, field).unwrap();
                // Borrowed (`ref`) field: the extract yields the stored borrow
                // POINTER. In this generic value-read position (`println(p.f)`,
                // an operand, a by-value method receiver), deref-on-use so the
                // consumer sees the borrowed `T` — mirroring how a `ref` param
                // identifier deref's when read. The pointer-forwarding
                // positions (`let x = p.f` ref-local bind, a `ref`-param
                // argument) intercept earlier via `compile_ref_field_access_ptr`
                // and never reach here. design.md Feature 4 Part 3.
                if let Some(inner_te) = self.field_access_ref_inner(object, field) {
                    let inner_ty = self.llvm_type_for_type_expr(&inner_te);
                    return Ok(self
                        .builder
                        .build_load(
                            inner_ty,
                            extracted.into_pointer_value(),
                            &format!("{}.ref.deref", field),
                        )
                        .unwrap());
                }
                return Ok(extracted);
            }
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// FFI union field read helper — phase 5 line 569 slice 4. Returns
    /// `Some(loaded_value)` when `object` resolves to a known union
    /// binding (by identifier or by `self` in an impl method on the
    /// union) and `field` is a registered union member; returns `None`
    /// otherwise so the caller falls through to the struct / shared /
    /// generic field-access paths. The helper does NOT enforce the
    /// `unsafe { }` gate — the typechecker's slice 2a diagnostic
    /// (`E_UNION_READ_REQUIRES_UNSAFE`) is what holds users to that
    /// rule; codegen executes the load unconditionally because the
    /// AST is already gate-cleared by the time we get here.
    fn try_compile_union_field_read(
        &mut self,
        object: &Expr,
        field: &str,
    ) -> Option<BasicValueEnum<'ctx>> {
        let (var_name, type_name, field_ty, storage_ptr) =
            self.resolve_union_field_access(object, field)?;
        let load_name = format!("union.{}.{}", type_name, field);
        let _ = var_name; // reserved for future tracing / diag hooks
        let loaded = self
            .builder
            .build_load(field_ty, storage_ptr, &load_name)
            .ok()?;
        Some(loaded)
    }

    /// Counterpart to `try_compile_union_field_read` for the LHS of an
    /// assignment (`u.field = expr`). The typechecker permits this
    /// without an `unsafe { }` block (slice 2a's `assigning_lhs` flag
    /// — only reads trip the gate); codegen mirrors the read shape but
    /// stores `new_val` at the union's base address.
    fn try_compile_union_field_store(
        &mut self,
        object: &Expr,
        field: &str,
        new_val: BasicValueEnum<'ctx>,
    ) -> bool {
        let Some((_, _, _field_ty, storage_ptr)) = self.resolve_union_field_access(object, field)
        else {
            return false;
        };
        self.builder.build_store(storage_ptr, new_val).unwrap();
        true
    }

    /// Common resolver for the union read + write paths. Walks `object`
    /// to its union storage pointer + field LLVM type. Returns
    /// `(binding_name, union_type_name, field_llvm_type, storage_ptr)`
    /// on a hit; `None` for anything outside the recognised receiver
    /// shapes (identifier-bound or `self`-bound union local). The
    /// returned pointer is the alloca of the storage struct — opaque
    /// pointers under LLVM 15+ make a per-field bitcast unnecessary,
    /// so callers can `build_load` / `build_store` through it directly
    /// using the field's LLVM type for the read width.
    fn resolve_union_field_access(
        &self,
        object: &Expr,
        field: &str,
    ) -> Option<(String, String, BasicTypeEnum<'ctx>, PointerValue<'ctx>)> {
        let var_name: String = match &object.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return None,
        };
        let type_name = self.var_type_names.get(&var_name)?.clone();
        let storage_ty = self.union_types.get(&type_name).copied()?;
        let field_ty = self
            .union_field_types
            .get(&type_name)
            .and_then(|fs| fs.iter().find(|(n, _)| n == field))
            .map(|(_, ty)| *ty)?;
        let slot = self.variables.get(&var_name)?;
        // For ref-bound union locals (`ref u: Foo`) the alloca holds a
        // pointer to the caller's storage rather than the storage
        // itself — load it through the same shape `get_data_ptr` uses
        // for ref-struct receivers. Storage type is needed only to
        // anchor the through-ptr alignment when SSE/ARM hosts care; we
        // suppress the warning here without using it directly.
        let _ = storage_ty;
        let storage_ptr = if self.ref_params.contains_key(&var_name) {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            self.builder
                .build_load(ptr_ty, slot.ptr, &format!("{}.ref.ptr", var_name))
                .ok()?
                .into_pointer_value()
        } else {
            slot.ptr
        };
        Some((var_name, type_name, field_ty, storage_ptr))
    }

    pub(super) fn compile_field_store(
        &mut self,
        object: &Expr,
        field: &str,
        new_val: BasicValueEnum<'ctx>,
        rhs_is_fresh: bool,
    ) -> Result<(), String> {
        // FFI union field store (phase 5 line 569 slice 4). Detect a
        // union-typed receiver and lower as an untyped store at the
        // storage base address. Slice 2a's typechecker makes
        // `u.field = x` unconditionally safe (no `unsafe { }` required
        // — only reads trip the gate), so the LHS path is reached
        // outside `unsafe` blocks too. Runs ahead of the
        // shared-struct / owned-struct branches so a future
        // struct/union name collision (today blocked at the resolver)
        // would not silently misroute the store.
        if self.try_compile_union_field_store(object, field, new_val) {
            return Ok(());
        }
        // Indexed-shared-struct receiver: `nodes[i].field = X` where
        // `nodes: Vec[Shared(N)]`. Load the heap pointer at `nodes[i]`
        // (the element slot stores the RC pointer cast to its LLVM
        // type), then GEP into the heap struct and store. Without this
        // branch the assignment silently falls through to the no-op
        // `Ok(())` exit at the function tail — the field write compiles
        // clean but does not persist, so a subsequent `nodes[i].field`
        // read returns the stale value.
        if let ExprKind::Index {
            object: inner,
            index,
        } = &object.kind
        {
            if let ExprKind::Identifier(outer_name) = &inner.kind {
                if let Some(elem_te) = self.var_elem_type_exprs.get(outer_name.as_str()).cloned() {
                    if let TypeKind::Path(path) = &elem_te.kind {
                        if let Some(seg) = path.segments.first() {
                            if let Some(info) = self.shared_types.get(seg.as_str()).cloned() {
                                if !info.is_enum {
                                    let outer_name = outer_name.clone();
                                    let (elem_ptr, _) =
                                        if self.vec_elem_types.contains_key(outer_name.as_str()) {
                                            self.lower_indexed_elem_ptr_vec(&outer_name, index)?
                                        } else if self
                                            .slice_elem_types
                                            .contains_key(outer_name.as_str())
                                        {
                                            self.lower_indexed_elem_ptr_slice(&outer_name, index)?
                                        } else {
                                            return Ok(());
                                        };
                                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                                    let heap_ptr = self
                                        .builder
                                        .build_load(ptr_ty, elem_ptr, "idx.shared.ptr")
                                        .unwrap()
                                        .into_pointer_value();
                                    if let Some(names) = self.struct_field_names.get(seg) {
                                        if let Some(idx) = names.iter().position(|n| n == field) {
                                            let field_ptr = self
                                                .builder
                                                .build_struct_gep(
                                                    info.heap_type,
                                                    heap_ptr,
                                                    (idx + 1) as u32,
                                                    &format!("sh_idx_{}_ptr", field),
                                                )
                                                .unwrap();
                                            // Field-store width coercion —
                                            // see `coerce_to_struct_field_ty`.
                                            let new_val = self.coerce_to_struct_field_ty(
                                                info.heap_type,
                                                (idx + 1) as u32,
                                                new_val,
                                            );
                                            self.builder.build_store(field_ptr, new_val).unwrap();
                                        }
                                    }
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }

        // `self.field = …` parses as `FieldAccess { object: SelfValue, … }`,
        // and `self` is bound as a regular local named "self" — same lookup
        // path as a plain Identifier. Treat both shapes uniformly so
        // ref-self method bodies can mutate through the receiver.
        let var_name_owned: Option<String> = match &object.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            ExprKind::SelfValue => Some("self".to_string()),
            _ => None,
        };
        if let Some(var_name) = var_name_owned.as_deref() {
            // Shared type: store directly into the heap object via GEP.
            if let Some(type_name) = self.var_type_names.get(var_name).cloned() {
                if let Some(info) = self.shared_types.get(&type_name).cloned() {
                    if !info.is_enum {
                        if self.variables.contains_key(var_name) {
                            // Heap pointer via `compile_expr` so a `ref self`
                            // receiver gets its ref-param double-load — a single
                            // load yields `&self` (a pointer to the heap-pointer
                            // slot), not the heap pointer itself, so the GEP+store
                            // would miss the heap object and the field write
                            // wouldn't persist. Matches the read path; an owned
                            // local / constructor binding still loads once.
                            let ptr = self.compile_expr(object)?.into_pointer_value();
                            if let Some(names) = self.struct_field_names.get(&type_name) {
                                if let Some(idx) = names.iter().position(|n| n == field) {
                                    // Phase-D layout: headerless members
                                    // GEP the twin at base 0. Only
                                    // primitive-field stores can reach
                                    // here for a headerless type — link
                                    // stores are intercepted by the b2
                                    // fast path before the generic
                                    // Assign compile.
                                    let (gep_ty, base) =
                                        self.shared_gep_layout(&type_name, info.heap_type);
                                    let field_ptr = self
                                        .builder
                                        .build_struct_gep(
                                            gep_ty,
                                            ptr,
                                            idx as u32 + base,
                                            &format!("sh_{}_ptr", field),
                                        )
                                        .unwrap();
                                    // `Option[shared T]` field-store: dec
                                    // the OLD inner ref (if Some) before
                                    // clobbering the slot, then store the
                                    // new value, then inc the NEW inner
                                    // ref (if Some and RHS isn't fresh).
                                    // Mirrors the Assign-arm's
                                    // `var_option_shared_heap` dispatch
                                    // for plain Option[shared T] bindings,
                                    // scaled up to a struct-field slot:
                                    // `field_ptr` is the per-field GEP
                                    // into the heap struct rather than a
                                    // per-binding alloca. Without this,
                                    // `tail.next = Some(node);` followed
                                    // by `tail.next = Some(new_node);`
                                    // strands the first node's whole
                                    // chain — the leak shape the 79a7db8
                                    // follow-up notes called out.
                                    //
                                    // Returns Ok(()) after the
                                    // refcount-aware path completes so
                                    // the plain `build_store` below
                                    // doesn't re-fire. No-op for
                                    // non-Option-shared fields, which
                                    // fall through to the plain store.
                                    if let Some(field_te) = self
                                        .struct_field_type_exprs
                                        .get(&type_name)
                                        .and_then(|v| v.get(idx))
                                        .cloned()
                                    {
                                        if let Some((_, inner_info)) =
                                            self.option_inner_shared_type_for_type_expr(&field_te)
                                        {
                                            // Route niche-optimized fields
                                            // through the pointer-slot
                                            // variant; conventional fields
                                            // through the 4-i64 path.
                                            if self
                                                .niche_field_inner_heap_type(&type_name, idx)
                                                .is_some()
                                            {
                                                self.emit_niche_option_shared_field_store(
                                                    field_ptr,
                                                    new_val,
                                                    inner_info.heap_type,
                                                    rhs_is_fresh,
                                                    field,
                                                );
                                            } else {
                                                self.emit_option_shared_field_store(
                                                    field_ptr,
                                                    new_val,
                                                    inner_info.heap_type,
                                                    rhs_is_fresh,
                                                    field,
                                                );
                                            }
                                            return Ok(());
                                        }
                                    }
                                    // Field-store width coercion — see
                                    // `coerce_to_struct_field_ty`.
                                    let new_val = self.coerce_to_struct_field_ty(
                                        gep_ty,
                                        idx as u32 + base,
                                        new_val,
                                    );
                                    self.builder.build_store(field_ptr, new_val).unwrap();
                                }
                            }
                        }
                        return Ok(());
                    }
                }
            }

            // Ref / mut-ref struct param: write through the pointer so the
            // caller's storage observes the update. The owned-param path
            // below would mutate a local copy of the struct value, so the
            // caller never sees the change — the `mut ref self` mutation
            // bug fixed in this slice. `get_data_ptr` returns the alloca
            // for owned bindings and the dereferenced pointer for ref
            // params, so we use it uniformly when GEP'ing into a struct.
            if let Some(&BasicTypeEnum::StructType(struct_ty)) = self.ref_params.get(var_name) {
                if let Some(idx) = self.field_index_for(object, field) {
                    if let Some(ptr) = self.get_data_ptr(var_name) {
                        let field_ptr = self
                            .builder
                            .build_struct_gep(struct_ty, ptr, idx, &format!("ref_{}_ptr", field))
                            .unwrap();
                        // Field-store width coercion — see
                        // `coerce_to_struct_field_ty`.
                        let new_val = self.coerce_to_struct_field_ty(struct_ty, idx, new_val);
                        self.builder.build_store(field_ptr, new_val).unwrap();
                        return Ok(());
                    }
                }
            }

            if let Some(slot) = self.variables.get(var_name).copied() {
                let obj_val = self
                    .builder
                    .build_load(slot.ty, slot.ptr, var_name)
                    .unwrap();
                if let BasicValueEnum::StructValue(sv) = obj_val {
                    let field_idx = self.field_index_for(object, field);
                    if let Some(idx) = field_idx {
                        // Field-store width coercion — see
                        // `coerce_to_struct_field_ty`.
                        let new_val = self.coerce_to_struct_field_ty(sv.get_type(), idx, new_val);
                        let updated = self
                            .builder
                            .build_insert_value(sv, new_val, idx, field)
                            .unwrap();
                        self.builder.build_store(slot.ptr, updated).unwrap();
                    }
                }
            }
        }
        Ok(())
    }

    /// Inc the inner shared-T ref of a just-loaded `Option[shared T]`
    /// SSA value, when its tag is Some and its inner ptr is non-null.
    /// Used by `compile_field_access`'s call-chain branch to balance
    /// the upcoming `emit_rc_dec` on the outer call temp — the outer
    /// temp's recursive drop walks the Option field and dec's the
    /// inner ptr, which would free the chain before the caller can
    /// read it. The inc emitted here brings the net effect to "the
    /// caller owns the inner ref"; the caller's let-stmt / match-arm
    /// binding registration takes ownership of the +1.
    ///
    /// Operates on a struct-valued SSA register rather than a slot
    /// pointer — the parent's `RcDecOption`-style cleanup paths read
    /// from a slot, but here the field value lives only in a register
    /// at the time the temp's RC is released, so the tag / w0 come
    /// from `build_extract_value`. Same Some-tag + null-guard
    /// structure as `emit_option_shared_field_store`'s old-side
    /// path, just consuming a register rather than a GEP slot.
    pub(super) fn emit_option_inner_rc_inc_for_loaded(
        &self,
        loaded: BasicValueEnum<'ctx>,
        inner_heap_type: StructType<'ctx>,
    ) {
        let sv = match loaded {
            BasicValueEnum::StructValue(sv) => sv,
            _ => return,
        };
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        let some_tag_const = i64_t.const_int(some_tag, false);
        let Some(fn_val) = self.current_fn else {
            return;
        };
        // Tag is field 0 of the Option layout; w0 (inner ptr as i64)
        // is field 1. Anything else (w1 / w2) is unused for the
        // shared-ref payload — Option[shared T]'s payload is the
        // single ptr-as-i64 in w0 per `coerce_to_payload_words`'s
        // primitive fast path.
        let tag = self
            .builder
            .build_extract_value(sv, 0, "opt.chain.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(IntPredicate::EQ, tag, some_tag_const, "opt.chain.is_some")
            .unwrap();
        let do_bb = self.context.append_basic_block(fn_val, "opt.chain.inc.do");
        let skip_bb = self
            .context
            .append_basic_block(fn_val, "opt.chain.inc.skip");
        self.builder
            .build_conditional_branch(is_some, do_bb, skip_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        let w0 = self
            .builder
            .build_extract_value(sv, 1, "opt.chain.w0")
            .unwrap()
            .into_int_value();
        let inner = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "opt.chain.inner")
            .unwrap();
        let inner_is_null = self
            .builder
            .build_is_null(inner, "opt.chain.inner.is_null")
            .unwrap();
        let real_do_bb = self
            .context
            .append_basic_block(fn_val, "opt.chain.inc.real");
        self.builder
            .build_conditional_branch(inner_is_null, skip_bb, real_do_bb)
            .unwrap();
        self.builder.position_at_end(real_do_bb);
        self.emit_refcount_inc_by_type(inner_heap_type, inner);
        self.builder.build_unconditional_branch(skip_bb).unwrap();
        self.builder.position_at_end(skip_bb);
    }

    /// Refcount-aware store into an `Option[shared T]` struct field.
    /// `field_ptr` is the GEP'd address of the Option slot inside the
    /// heap struct; `new_val` is the just-compiled RHS Option struct
    /// SSA; `inner_heap_type` is the heap layout of `T`; `rhs_is_fresh`
    /// matches the parent commit's `rhs_yields_fresh_ref` semantics.
    /// Mirrors the Assign-arm's `var_option_shared_heap` dispatch in
    /// `compile_stmt`:
    ///
    ///   1. Load the old slot's tag; if Some, dec the old inner ptr.
    ///   2. Store the new Option value.
    ///   3. If RHS is not fresh, branch on the new tag; if Some, inc
    ///      the new inner ptr.
    ///
    /// Without this, `obj.next = some_other_opt;` strands the old
    /// inner ref; `obj.next = Some(node);` over the same field
    /// across iterations leaks one chain per overwrite. The kata's
    /// `tail.next = Some(node); tail = node;` shape doesn't surface
    /// the leak (each iter creates a fresh `tail` so each field
    /// store sees a None old value), but mutations against a
    /// long-lived holder (`head.next = Some(...);` on a persistent
    /// `head`) hit it.
    pub(super) fn emit_option_shared_field_store(
        &self,
        field_ptr: PointerValue<'ctx>,
        new_val: BasicValueEnum<'ctx>,
        inner_heap_type: StructType<'ctx>,
        rhs_is_fresh: bool,
        field_name: &str,
    ) {
        let option_ty = self.enum_layouts["Option"].llvm_type;
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        let some_tag_const = i64_t.const_int(some_tag, false);
        let Some(fn_val) = self.current_fn else {
            // Defensive: outside a function body, fall back to a plain
            // store. The let-stmt / Assign sites are inside fn bodies
            // by construction, so this branch is unreachable today.
            let _ = self.builder.build_store(field_ptr, new_val);
            return;
        };
        // ── Step 1: load the old slot (tag + inner ptr) up front, before
        //           the store clobbers it. The dec itself happens last, so
        //           the new value can be retained first (Step 2/3 order). ──
        let old_tag_ptr = self
            .builder
            .build_struct_gep(
                option_ty,
                field_ptr,
                0,
                &format!("opt.fld.{field_name}.old.tag.p"),
            )
            .unwrap();
        let old_tag = self
            .builder
            .build_load(i64_t, old_tag_ptr, &format!("opt.fld.{field_name}.old.tag"))
            .unwrap()
            .into_int_value();
        let old_is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                old_tag,
                some_tag_const,
                &format!("opt.fld.{field_name}.old.is_some"),
            )
            .unwrap();
        let old_w0_ptr = self
            .builder
            .build_struct_gep(
                option_ty,
                field_ptr,
                1,
                &format!("opt.fld.{field_name}.old.w0.p"),
            )
            .unwrap();
        let old_w0 = self
            .builder
            .build_load(i64_t, old_w0_ptr, &format!("opt.fld.{field_name}.old.w0"))
            .unwrap()
            .into_int_value();
        // `old_inner` is only dereferenced inside the `old_is_some` +
        // non-null guard below, so materializing it eagerly (even for a
        // None old slot, where w0 is undef) is harmless.
        let old_inner = self
            .builder
            .build_int_to_ptr(old_w0, ptr_ty, &format!("opt.fld.{field_name}.old.inner"))
            .unwrap();
        // ── Step 2: retain the new inner BEFORE releasing the old. ──
        // Read the new inner from the `new_val` SSA (not the slot, which is
        // not stored yet). If the RHS aliases *through* the old value — the
        // canonical `slow.next = slow.next.next` splice, where the new node's
        // only live reference is the old node's `next` field — releasing old
        // first runs its drop, which recursively dec_refs the new node to
        // zero and frees it out from under us (use-after-free). Retain-
        // before-release is the ARC setter rule; it also makes the
        // self-assignment case (`x.next = x.next`) a no-op. A fresh RHS can't
        // alias old, so the inc is skipped.
        if !rhs_is_fresh {
            let new_tag = self
                .builder
                .build_extract_value(
                    new_val.into_struct_value(),
                    0,
                    &format!("opt.fld.{field_name}.new.tag"),
                )
                .unwrap()
                .into_int_value();
            let new_is_some = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    new_tag,
                    some_tag_const,
                    &format!("opt.fld.{field_name}.new.is_some"),
                )
                .unwrap();
            let new_do_bb = self
                .context
                .append_basic_block(fn_val, &format!("opt.fld.{field_name}.new.do"));
            let new_skip_bb = self
                .context
                .append_basic_block(fn_val, &format!("opt.fld.{field_name}.new.skip"));
            self.builder
                .build_conditional_branch(new_is_some, new_do_bb, new_skip_bb)
                .unwrap();
            self.builder.position_at_end(new_do_bb);
            let new_w0 = self
                .builder
                .build_extract_value(
                    new_val.into_struct_value(),
                    1,
                    &format!("opt.fld.{field_name}.new.w0"),
                )
                .unwrap()
                .into_int_value();
            let new_inner = self
                .builder
                .build_int_to_ptr(new_w0, ptr_ty, &format!("opt.fld.{field_name}.new.inner"))
                .unwrap();
            let new_is_null = self
                .builder
                .build_is_null(
                    new_inner,
                    &format!("opt.fld.{field_name}.new.inner.is_null"),
                )
                .unwrap();
            let new_real_do_bb = self
                .context
                .append_basic_block(fn_val, &format!("opt.fld.{field_name}.new.real_do"));
            self.builder
                .build_conditional_branch(new_is_null, new_skip_bb, new_real_do_bb)
                .unwrap();
            self.builder.position_at_end(new_real_do_bb);
            self.emit_refcount_inc_by_type(inner_heap_type, new_inner);
            self.builder
                .build_unconditional_branch(new_skip_bb)
                .unwrap();
            self.builder.position_at_end(new_skip_bb);
        }
        // ── Step 3: store the new Option value now that it is retained. ──
        self.builder.build_store(field_ptr, new_val).unwrap();
        // ── Step 4: release the old inner if old was Some and non-null. ──
        let old_do_bb = self
            .context
            .append_basic_block(fn_val, &format!("opt.fld.{field_name}.old.do"));
        let old_skip_bb = self
            .context
            .append_basic_block(fn_val, &format!("opt.fld.{field_name}.old.skip"));
        self.builder
            .build_conditional_branch(old_is_some, old_do_bb, old_skip_bb)
            .unwrap();
        self.builder.position_at_end(old_do_bb);
        let old_is_null = self
            .builder
            .build_is_null(
                old_inner,
                &format!("opt.fld.{field_name}.old.inner.is_null"),
            )
            .unwrap();
        let old_real_do_bb = self
            .context
            .append_basic_block(fn_val, &format!("opt.fld.{field_name}.old.real_do"));
        self.builder
            .build_conditional_branch(old_is_null, old_skip_bb, old_real_do_bb)
            .unwrap();
        self.builder.position_at_end(old_real_do_bb);
        self.emit_refcount_dec_by_type(inner_heap_type, old_inner);
        self.builder
            .build_unconditional_branch(old_skip_bb)
            .unwrap();
        self.builder.position_at_end(old_skip_bb);
    }

    /// Niche-opt variant of `emit_option_shared_field_store` — the field
    /// slot is a single `ptr` (null = None, non-null = Some), not the
    /// 4-i64 Option enum. Same three-step refcount discipline:
    ///   1. Load the old ptr; if non-null, dec_ref it (null-check
    ///      subsumes the old tag-check because null *is* None here).
    ///   2. Extract w0 from the incoming Option SSA value and store it
    ///      as the new ptr (None's `const_zero` carries w0=0 which
    ///      stores as null, so no separate tag branch is needed for
    ///      the store itself).
    ///   3. If RHS isn't fresh, inc_ref the new ptr (after null-check).
    pub(super) fn emit_niche_option_shared_field_store(
        &self,
        field_ptr: PointerValue<'ctx>,
        new_val: BasicValueEnum<'ctx>,
        inner_heap_type: StructType<'ctx>,
        rhs_is_fresh: bool,
        field_name: &str,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let Some(fn_val) = self.current_fn else {
            self.niche_store_option_field(field_ptr, new_val);
            return;
        };
        // ── Step 1: compute the new inner ptr from `new_val`. ──
        //           Tag-aware: when tag == None the payload words are LLVM
        //           `undef` (see `try_compile_enum_variant`'s None build),
        //           so a bare `w0 as ptr` would materialize garbage. Select
        //           against null on the None branch so the niche slot always
        //           reads back correctly. Done up front so the new value can
        //           be retained *before* the old one is released (Step 2).
        let sv = new_val.into_struct_value();
        let new_tag = self
            .builder
            .build_extract_value(sv, 0, &format!("niche.fld.{field_name}.new.tag"))
            .unwrap()
            .into_int_value();
        let new_w0 = self
            .builder
            .build_extract_value(sv, 1, &format!("niche.fld.{field_name}.new.w0"))
            .unwrap()
            .into_int_value();
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        let is_some_for_store = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                new_tag,
                i64_t.const_int(some_tag, false),
                &format!("niche.fld.{field_name}.new.is_some.store"),
            )
            .unwrap();
        let ptr_from_w0 = self
            .builder
            .build_int_to_ptr(
                new_w0,
                ptr_ty,
                &format!("niche.fld.{field_name}.new.ptr_some"),
            )
            .unwrap();
        let new_inner = self
            .builder
            .build_select(
                is_some_for_store,
                ptr_from_w0,
                ptr_ty.const_null(),
                &format!("niche.fld.{field_name}.new.ptr"),
            )
            .unwrap()
            .into_pointer_value();
        // ── Step 2: retain new, store, then release old. ──
        // Load the old inner ptr (the value being overwritten) before the
        // store clobbers the slot.
        let old_inner = self
            .builder
            .build_load(
                ptr_ty,
                field_ptr,
                &format!("niche.fld.{field_name}.old.ptr"),
            )
            .unwrap()
            .into_pointer_value();
        // Retain the new inner BEFORE releasing the old. If the RHS aliases
        // *through* the old value — the canonical `slow.next = slow.next.next`
        // splice, where the new node's only live reference is the old node's
        // `next` field — releasing old first runs its drop, which recursively
        // dec_refs the new node to zero and frees it out from under us
        // (use-after-free: a trap, or a garbage store). Retain-before-release
        // is the ARC setter rule; it also makes the self-assignment case
        // (`x.next = x.next`) a no-op. A fresh RHS can't alias old, so skip.
        let _ = i64_t;
        if !rhs_is_fresh {
            let new_is_null = self
                .builder
                .build_is_null(new_inner, &format!("niche.fld.{field_name}.new.is_null"))
                .unwrap();
            let new_do_bb = self
                .context
                .append_basic_block(fn_val, &format!("niche.fld.{field_name}.new.do"));
            let new_skip_bb = self
                .context
                .append_basic_block(fn_val, &format!("niche.fld.{field_name}.new.skip"));
            self.builder
                .build_conditional_branch(new_is_null, new_skip_bb, new_do_bb)
                .unwrap();
            self.builder.position_at_end(new_do_bb);
            self.emit_refcount_inc_by_type(inner_heap_type, new_inner);
            self.builder
                .build_unconditional_branch(new_skip_bb)
                .unwrap();
            self.builder.position_at_end(new_skip_bb);
        }
        // Store the new inner ptr now that it is retained.
        self.builder.build_store(field_ptr, new_inner).unwrap();
        // Release the old inner if non-null (after new is retained + stored).
        let old_is_null = self
            .builder
            .build_is_null(old_inner, &format!("niche.fld.{field_name}.old.is_null"))
            .unwrap();
        let old_do_bb = self
            .context
            .append_basic_block(fn_val, &format!("niche.fld.{field_name}.old.do"));
        let old_skip_bb = self
            .context
            .append_basic_block(fn_val, &format!("niche.fld.{field_name}.old.skip"));
        self.builder
            .build_conditional_branch(old_is_null, old_skip_bb, old_do_bb)
            .unwrap();
        self.builder.position_at_end(old_do_bb);
        self.emit_refcount_dec_by_type(inner_heap_type, old_inner);
        self.builder
            .build_unconditional_branch(old_skip_bb)
            .unwrap();
        self.builder.position_at_end(old_skip_bb);
    }

    /// Refcount-balance the capture of an `Option[shared T]` value into a
    /// freshly-constructed `shared struct` literal field (`ListNode { val:
    /// 0, next: head }` over a non-fresh `head`). The new heap object's
    /// field becomes an independent reference to that chain and must inc the
    /// inner pointer. A fresh field value (`Some(node)` over a just-built
    /// node, a call's move-out) already owns its ref and must not — the
    /// caller gates on `!rhs_yields_fresh_ref`. Tag/null-guarded.
    ///
    /// This is the `Option[shared T]` analogue of `suppress_source_vec_cleanup_for_arg`'s
    /// shared-struct transfer-inc, which only fires for a bare `shared
    /// struct` field value (its `var_type_names`/`shared_types` lookup
    /// misses an `Option[shared]` binding like a param `head`). Without it,
    /// `let dummy = ListNode { next: head };` over an `Option[shared]`
    /// `head` leaves the field uncounted, so returning `dummy.next` hands
    /// the caller an under-counted chain → over-dec / double-free (kata #19
    /// `remove_nth_from_end`, masked at O2).
    pub(super) fn emit_rc_inc_for_captured_option(
        &self,
        val: BasicValueEnum<'ctx>,
        inner_heap_type: StructType<'ctx>,
    ) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let sv = val.into_struct_value();
        let tag = self
            .builder
            .build_extract_value(sv, 0, "capt.opt.tag")
            .unwrap()
            .into_int_value();
        let w0 = self
            .builder
            .build_extract_value(sv, 1, "capt.opt.w0")
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
                "capt.opt.is_some",
            )
            .unwrap();
        let do_bb = self.context.append_basic_block(fn_val, "capt.opt.inc.do");
        let skip_bb = self.context.append_basic_block(fn_val, "capt.opt.inc.skip");
        self.builder
            .build_conditional_branch(is_some, do_bb, skip_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        let inner = self
            .builder
            .build_int_to_ptr(w0, ptr_ty, "capt.opt.inner")
            .unwrap();
        let is_null = self
            .builder
            .build_is_null(inner, "capt.opt.is_null")
            .unwrap();
        let real_bb = self.context.append_basic_block(fn_val, "capt.opt.inc.real");
        self.builder
            .build_conditional_branch(is_null, skip_bb, real_bb)
            .unwrap();
        self.builder.position_at_end(real_bb);
        self.emit_refcount_inc_by_type(inner_heap_type, inner);
        self.builder.build_unconditional_branch(skip_bb).unwrap();
        self.builder.position_at_end(skip_bb);
    }

    pub(super) fn compile_tuple_index(
        &mut self,
        object: &Expr,
        index: usize,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let obj_val = self.compile_expr(object)?;
        if let BasicValueEnum::StructValue(sv) = obj_val {
            return Ok(self
                .builder
                .build_extract_value(sv, index as u32, "tidx")
                .unwrap());
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// If `object.field` reads a borrowed (`ref T` / `mut ref T`) struct
    /// field, return the inner `T` type expression; otherwise `None`. The
    /// field slot lowers to `ptr` and holds the borrow pointer
    /// (`compile_struct_init`); callers use this to decide deref-on-use
    /// (value position) vs. forward-the-pointer (let-bind as ref-local,
    /// ref-param argument). design.md Feature 4 Part 3; B-2026-06-07-5.
    pub(super) fn field_access_ref_inner(&self, object: &Expr, field: &str) -> Option<TypeExpr> {
        let struct_name = self.type_name_of_expr(object)?;
        let names = self.struct_field_names.get(struct_name.as_str())?;
        let idx = names.iter().position(|n| n == field)?;
        let te = self
            .struct_field_type_exprs
            .get(struct_name.as_str())?
            .get(idx)?;
        match &te.kind {
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => Some((**inner).clone()),
            _ => None,
        }
    }

    pub(super) fn field_index_for(&self, object: &Expr, field: &str) -> Option<u32> {
        // Try to resolve by walking the object expression to its
        // user-type name, then looking up `field` in that struct's
        // field registry. Chained `o.inner.name` requires walking the
        // inner FieldAccess to recover `o.inner`'s declared type from
        // `struct_field_type_names`. See `type_name_of_expr`.
        if let Some(type_name) = self.type_name_of_expr(object) {
            if let Some(names) = self.struct_field_names.get(type_name.as_str()) {
                if let Some(idx) = names.iter().position(|n| n == field) {
                    return Some(idx as u32);
                }
            }
        }
        // Fall back: numeric index for tuple fields like `.0`, `.1`
        field.parse::<u32>().ok()
    }

    /// Resolve the user-type name of an arbitrary expression by walking
    /// `Identifier` / `SelfValue` / `FieldAccess` chains. Returns
    /// `None` for primitive-typed expressions, calls whose return type
    /// isn't a known struct, or any shape outside this trio. Companion
    /// to `type_name_of` (which only handles direct identifiers and
    /// struct literals).
    pub(super) fn type_name_of_expr(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Identifier(n) => self.var_type_names.get(n.as_str()).cloned(),
            ExprKind::SelfValue => self.var_type_names.get("self").cloned(),
            ExprKind::StructLiteral { path, .. } => path.last().cloned(),
            ExprKind::FieldAccess { object, field } => {
                let obj_ty = self.type_name_of_expr(object)?;
                let field_names = self.struct_field_names.get(obj_ty.as_str())?;
                let idx = field_names.iter().position(|n| n == field)?;
                let field_ty_names = self.struct_field_type_names.get(obj_ty.as_str())?;
                field_ty_names.get(idx).and_then(|n| n.clone())
            }
            _ => None,
        }
    }

    /// Return the Kāra type name for a compiled expression, if known.
    pub(super) fn type_name_of(&self, expr: &Expr) -> Option<String> {
        match &expr.kind {
            ExprKind::Identifier(n) => self.var_type_names.get(n.as_str()).cloned(),
            ExprKind::StructLiteral { path, .. } => path.last().cloned(),
            // `let x = vec_of_chars[i]` — recover the element type's name
            // from `var_elem_type_exprs` so downstream consumers (notably
            // `expr_is_char` for the print/f-string glyph rendering)
            // pick up `x: char`. Generalises to any single-path element
            // type, e.g. `let p = points[0]` → "Point" — same shape the
            // shared-struct field-access machinery already relies on.
            ExprKind::Index { object, .. } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(te) = self.var_elem_type_exprs.get(n.as_str()) {
                        if let TypeKind::Path(p) = &te.kind {
                            return p.segments.last().cloned();
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// True when `expr` has source-level type `char` per the codegen-side
    /// type tracking. Drives the print and f-string char-arms: a true
    /// result routes the value through `emit_codepoint_to_utf8` so the
    /// glyph is rendered rather than the integer codepoint.
    pub(super) fn expr_is_char(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::CharLit(_) => true,
            ExprKind::Identifier(n) => self
                .var_type_names
                .get(n.as_str())
                .map(|s| s == "char")
                .unwrap_or(false),
            // `vec_of_chars[i]` / `array_of_chars[i]` — check the
            // collection's element TypeExpr is a bare `char` path.
            ExprKind::Index { object, .. } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(te) = self.var_elem_type_exprs.get(n.as_str()) {
                        if let TypeKind::Path(p) = &te.kind {
                            return p.segments.last().map(|s| s == "char").unwrap_or(false);
                        }
                    }
                }
                false
            }
            // `<char>.clone()` returns a char — recurse on the receiver so the
            // result is still rendered as a glyph by print / f-string lowering
            // (clone is scalar identity for primitives; see compile_method_call).
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } if method == "clone" && args.is_empty() => self.expr_is_char(object),
            // `someStruct.charField` — a char-typed struct field. Needed so the
            // synthetic-f-string struct Display (see synth_display.rs) renders
            // char fields as glyphs, matching the interpreter.
            ExprKind::FieldAccess { object, field } => {
                let Some(outer) = self.expr_user_struct_name(object) else {
                    return false;
                };
                let (Some(names), Some(tes)) = (
                    self.struct_field_names.get(&outer),
                    self.struct_field_type_exprs.get(&outer),
                ) else {
                    return false;
                };
                names
                    .iter()
                    .position(|f| f == field)
                    .and_then(|idx| tes.get(idx))
                    .map(|te| {
                        matches!(&te.kind, TypeKind::Path(p)
                            if p.segments.last().map(|s| s == "char").unwrap_or(false))
                    })
                    .unwrap_or(false)
            }
            _ => false,
        }
    }

    /// True when `expr` is known to have an unsigned integer source type
    /// (`u8` / `u16` / `u32` / `u64` / `u128` / `usize`). Drives the
    /// print path's choice between `sext + %lld` (signed) and
    /// `zext + %llu` (unsigned); the default (`false`) is signed, which
    /// is the right call for unknown-typed expressions because `i64` is
    /// the language's default integer width and unsigned printing of a
    /// signed value with the high bit set already worked by accident
    /// pre-fix (zero-pad happens to match the unsigned value), whereas
    /// signed printing of a negative narrow signed int via raw `%lld`
    /// produces the unsigned representation in the high bits. Mirror
    /// of [`Self::expr_is_char`] for the int signedness signal.
    pub(super) fn expr_is_unsigned_int(&self, expr: &Expr) -> bool {
        fn is_uint_name(s: &str) -> bool {
            matches!(s, "u8" | "u16" | "u32" | "u64" | "u128" | "usize")
        }
        match &expr.kind {
            // Suffixed literal — the suffix is authoritative.
            ExprKind::Integer(_, Some(suf)) => matches!(
                suf,
                crate::token::IntSuffix::U8
                    | crate::token::IntSuffix::U16
                    | crate::token::IntSuffix::U32
                    | crate::token::IntSuffix::U64
                    | crate::token::IntSuffix::U128
            ),
            ExprKind::Identifier(n) => self
                .var_type_names
                .get(n.as_str())
                .map(|s| is_uint_name(s.as_str()))
                .unwrap_or(false),
            // Call result — the callee's declared return-type name
            // (registered in `fn_return_type_names` during the function
            // walk) carries the signedness. Without this arm,
            // `println(u8_fn())` sign-extends the narrow result on the
            // print path (200 printed as -56 — surfaced by the
            // sub-64-bit boundary-coercion fix's E2E, 2026-06-06).
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Identifier(n) = &callee.kind {
                    return self
                        .fn_return_type_names
                        .get(n.as_str())
                        .map(|s| is_uint_name(s.as_str()))
                        .unwrap_or(false);
                }
                false
            }
            // Method-call result — impl methods register their return
            // type under the synth `Type.method` key (see
            // `make_impl_method_function`); resolve the receiver's
            // type name and look the qualified key up. Same -56 print
            // symptom as the free-fn arm, method shape (2026-06-06).
            ExprKind::MethodCall { object, method, .. } => {
                // Float→int conversion methods with an unsigned target return a
                // bare `u*` (phase-8 cast slice 4): `saturating_to_u8` /
                // `wrapping_to_u16` / `trunc_to_u32` … so the print / coercion
                // path must zero-extend (else 255u8 prints as -1). `checked_to_*`
                // is excluded — it returns `Option[u*]`, not a bare integer (the
                // `Some(v)` binding's signedness is handled at the pattern site).
                if let Some((family, _, _, signed)) =
                    crate::numeric_conv::parse_float_to_int(method)
                {
                    return !signed
                        && !matches!(family, crate::numeric_conv::FloatToIntFamily::Checked);
                }
                // Element-typed vector reductions / lane folds on an
                // unsigned-element `Vector[T, N]`: the result carries the
                // receiver's element signedness. The receiver rides
                // `unsigned_vector_exprs` at the method-call span (call
                // and receiver share a span; the receiver-position
                // entries come from `vector_method_receivers` — see
                // lowering.rs). Without this arm `println(v.reduce_max())`
                // on `Vector[u8, N]` sign-extends 255 to -1 (surfaced
                // 2026-06-07 with the lane boundary-coercion fix; lanes
                // previously lowered i64-wide and printed by accident).
                if matches!(
                    method.as_str(),
                    "reduce_min"
                        | "reduce_max"
                        | "reduce_sum"
                        | "reduce_product"
                        | "reduce_and"
                        | "reduce_or"
                        | "reduce_xor"
                        | "dot"
                ) && self
                    .unsigned_vector_exprs
                    .contains(&(expr.span.offset, expr.span.length))
                {
                    return true;
                }
                let recv_ty = match &object.kind {
                    ExprKind::Identifier(n) => self.var_type_names.get(n.as_str()),
                    ExprKind::SelfValue => self.var_type_names.get("self"),
                    _ => None,
                };
                if let Some(ty) = recv_ty {
                    return self
                        .fn_return_type_names
                        .get(&format!("{ty}.{method}"))
                        .map(|s| is_uint_name(s.as_str()))
                        .unwrap_or(false);
                }
                false
            }
            // `<expr> as uN` in value position (`println(x as u8)`): the result
            // carries the TARGET type's signedness, not the source's. Without
            // this arm a direct unsigned as-cast in a print/coerce position
            // sign-extends (255u8 → -1); a `let v: u8 = …` binding already works
            // via `var_type_names`, so this closes the cast-expression case.
            ExprKind::Cast { ty, .. } => matches!(&ty.kind, TypeKind::Path(p)
                if p
                    .segments
                    .last()
                    .map(|s| is_uint_name(s.as_str()))
                    .unwrap_or(false)),
            // Struct-field read — the declared field TypeExpr carries
            // the signedness (`println(s.b)` for a `u8` field).
            ExprKind::FieldAccess { object, field } => {
                let recv_ty = match &object.kind {
                    ExprKind::Identifier(n) => self.var_type_names.get(n.as_str()),
                    ExprKind::SelfValue => self.var_type_names.get("self"),
                    _ => None,
                };
                if let Some(ty) = recv_ty {
                    if let (Some(names), Some(tes)) = (
                        self.struct_field_names.get(ty),
                        self.struct_field_type_exprs.get(ty),
                    ) {
                        if let Some(idx) = names.iter().position(|n| n == field) {
                            if let Some(TypeKind::Path(p)) = tes.get(idx).map(|te| &te.kind) {
                                return p
                                    .segments
                                    .last()
                                    .map(|s| is_uint_name(s.as_str()))
                                    .unwrap_or(false);
                            }
                        }
                    }
                }
                false
            }
            // `vec_of_u32s[i]` / `array_of_u32s[i]` — check element TypeExpr.
            ExprKind::Index { object, .. } => {
                if let ExprKind::Identifier(n) = &object.kind {
                    if let Some(te) = self.var_elem_type_exprs.get(n.as_str()) {
                        if let TypeKind::Path(p) = &te.kind {
                            return p
                                .segments
                                .last()
                                .map(|s| is_uint_name(s.as_str()))
                                .unwrap_or(false);
                        }
                    }
                }
                // `v[i]` lane read on an unsigned-element `Vector[T, N]` —
                // same signedness story as the reduction arm above. The
                // typechecker records the receiver at the Index node's
                // span (`vector_method_receivers` → `unsigned_vector_exprs`
                // via lowering.rs); the object span is checked too for the
                // collided-span shape.
                self.unsigned_vector_exprs
                    .contains(&(expr.span.offset, expr.span.length))
                    || self
                        .unsigned_vector_exprs
                        .contains(&(object.span.offset, object.span.length))
            }
            _ => false,
        }
    }

    // ── Cast ──────────────────────────────────────────────────────

    /// Lower a Kāra `expr as Target` cast.
    ///
    /// `source_is_unsigned` drives the integer-widening lane: when the source
    /// expression has an unsigned Kāra type (`u8` / `u16` / `u32` / `u64` /
    /// `usize` / `bool` / `char`), widening lowers to `zext`; for signed
    /// sources (default), widening lowers to `sext`. The caller is responsible
    /// for computing the flag via `expr_is_unsigned_int` on the inner
    /// expression — `compile_cast` itself has only the LLVM `IntValue`, which
    /// doesn't carry Kāra-level signedness (`i8` and `u8` both lower to LLVM
    /// `i8`).
    ///
    /// Pre-fix this used `build_int_cast`, which always sign-extends on
    /// widening. The kata-91 hot-loop measurement (`(bytes[i] as i32) -
    /// (zero as i32)` for `bytes: Vec[u8]`) showed `ldrsb`+`sxtb`+`and #0xff`
    /// — a sign-extending load plus a redundant zero-mask — where rust
    /// emitted a single `ldrb`. Two extra instructions per inner iter; same
    /// missed optimization applies to every `(u_typed_indexed_value) as
    /// wider_int` pattern in the language.
    ///
    /// `target_is_unsigned` selects `fptoui.sat` over `fptosi.sat` for the
    /// float→int lane (the LLVM `IntType` target doesn't carry Kāra signedness);
    /// the caller computes it from the target type name. It is ignored by the
    /// other lanes.
    pub(super) fn compile_cast(
        &self,
        val: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
        source_is_unsigned: bool,
        target_is_unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match (val, target) {
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(tt)) => {
                let src_w = iv.get_type().get_bit_width();
                let dst_w = tt.get_bit_width();
                let result = if dst_w > src_w {
                    if source_is_unsigned {
                        self.builder.build_int_z_extend(iv, tt, "cast.zx").unwrap()
                    } else {
                        self.builder.build_int_s_extend(iv, tt, "cast.sx").unwrap()
                    }
                } else if dst_w < src_w {
                    self.builder.build_int_truncate(iv, tt, "cast.tr").unwrap()
                } else {
                    iv
                };
                Ok(result.into())
            }
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::FloatType(ft)) => {
                // Signed/unsigned int-to-float branch on the source-type hint:
                // `255u8 as f32` should yield 255.0, not -1.0 via sitofp on the
                // bit-pattern. Symmetric to the int-widening path above.
                let result = if source_is_unsigned {
                    self.builder
                        .build_unsigned_int_to_float(iv, ft, "cast")
                        .unwrap()
                } else {
                    self.builder
                        .build_signed_int_to_float(iv, ft, "cast")
                        .unwrap()
                };
                Ok(result.into())
            }
            (BasicValueEnum::FloatValue(fv), BasicTypeEnum::IntType(it)) => {
                // `f as iN` is the **saturating** float→int cast (design.md
                // § Numeric Semantics > as-cast semantics; phase-8 cast slice 4):
                // out-of-range clamps to the target MIN/MAX and NaN → 0, both
                // guaranteed by `llvm.fptosi.sat` / `llvm.fptoui.sat` — a single
                // target-independent instruction. (Pre-fix this was raw
                // `fptosi`, which is poison on out-of-range.) Identical lowering
                // to `f.saturating_to_iN()`.
                let result = self.emit_float_to_int_sat(fv, it, target_is_unsigned)?;
                Ok(result.into())
            }
            (BasicValueEnum::FloatValue(fv), BasicTypeEnum::FloatType(ft)) => {
                let result = self.builder.build_float_cast(fv, ft, "cast").unwrap();
                Ok(result.into())
            }
            _ => Ok(val),
        }
    }

    /// Bit width (32 / 64) of an LLVM float type, for intrinsic name mangling.
    fn float_bit_width(&self, ft: inkwell::types::FloatType<'ctx>) -> u32 {
        if ft == self.context.f32_type() {
            32
        } else {
            64
        }
    }

    /// LLVM integer type of the given bit width — the standard types for the
    /// 8/16/32/64/128 widths, `custom_width_int_type` for anything else (the
    /// 256-bit round-trip width used by `emit_float_to_int_rangecheck`).
    pub(super) fn int_type_for_bits(&self, bits: u32) -> inkwell::types::IntType<'ctx> {
        match bits {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            64 => self.context.i64_type(),
            128 => self.context.i128_type(),
            other => self
                .context
                .custom_width_int_type(std::num::NonZeroU32::new(other).expect("nonzero width"))
                .expect("custom int width"),
        }
    }

    /// Saturating float→int conversion to `int_ty` via the `llvm.fptosi.sat` /
    /// `llvm.fptoui.sat` intrinsic (out-of-range clamps to MIN/MAX, NaN → 0).
    /// Backs both `f as iN` and `f.saturating_to_iN()` (phase-8 cast slice 4),
    /// and — invoked with a wider `int_ty` — the round-trip range check used by
    /// the `checked`/`trunc`/`wrapping` families.
    pub(super) fn emit_float_to_int_sat(
        &self,
        fv: inkwell::values::FloatValue<'ctx>,
        int_ty: inkwell::types::IntType<'ctx>,
        target_unsigned: bool,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let base = if target_unsigned {
            "llvm.fptoui.sat"
        } else {
            "llvm.fptosi.sat"
        };
        let intrinsic = inkwell::intrinsics::Intrinsic::find(base)
            .ok_or_else(|| format!("{base} intrinsic must exist in LLVM"))?;
        // `fptosi.sat`/`fptoui.sat` are overloaded on BOTH the result int type
        // and the operand float type (`llvm.fptosi.sat.iN.fM`).
        let decl = intrinsic
            .get_declaration(&self.module, &[int_ty.into(), fv.get_type().into()])
            .ok_or_else(|| {
                format!(
                    "{base} has no declaration for i{}.f{}",
                    int_ty.get_bit_width(),
                    self.float_bit_width(fv.get_type())
                )
            })?;
        Ok(self
            .builder
            .build_call(decl, &[fv.into()], "f2i.sat")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value())
    }

    /// Round-trip range check for the `checked`/`trunc` float→int families.
    /// Returns `(in_range_i1, narrowed_iN)` where `narrowed` is the
    /// truncate-toward-zero result (also the `wrapping` value for in-range
    /// inputs) and `in_range` is true iff `f` is non-NaN AND exactly
    /// representable in the target type. Works in exact integer arithmetic via
    /// a wider saturating cast (`iW`, `W = 128` for ≤64-bit targets, `256` for
    /// 128-bit) then narrow + re-extend + compare — sidestepping float-bound
    /// rounding entirely. Mirrors the slice-2 interpreter oracle.
    fn emit_float_to_int_rangecheck(
        &self,
        fv: inkwell::values::FloatValue<'ctx>,
        int_ty: inkwell::types::IntType<'ctx>,
        target_unsigned: bool,
    ) -> Result<
        (
            inkwell::values::IntValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
        ),
        String,
    > {
        let w = int_ty.get_bit_width();
        let wide_w = if w < 128 { 128 } else { 256 };
        let wide_ty = self.int_type_for_bits(wide_w);
        // Signed wide saturation preserves the sign, so a negative value into an
        // unsigned target fails the round-trip below (rather than silently
        // clamping to 0 the way `fptoui.sat` would).
        let wide = self.emit_float_to_int_sat(fv, wide_ty, false)?;
        let narrowed = self
            .builder
            .build_int_truncate(wide, int_ty, "f2i.narrow")
            .unwrap();
        let re = if target_unsigned {
            self.builder
                .build_int_z_extend(narrowed, wide_ty, "f2i.re")
                .unwrap()
        } else {
            self.builder
                .build_int_s_extend(narrowed, wide_ty, "f2i.re")
                .unwrap()
        };
        let fits = self
            .builder
            .build_int_compare(IntPredicate::EQ, re, wide, "f2i.fits")
            .unwrap();
        // `f == f` is false exactly when `f` is NaN (saturating cast maps NaN to
        // 0, which would otherwise pass the round-trip).
        let not_nan = self
            .builder
            .build_float_compare(FloatPredicate::OEQ, fv, fv, "f2i.notnan")
            .unwrap();
        let in_range = self
            .builder
            .build_and(fits, not_nan, "f2i.inrange")
            .unwrap();
        Ok((in_range, narrowed))
    }

    /// Build an `Option[iN]` from a `checked_to_iN` result: `Some(narrowed)`
    /// when `in_range`, else `None`. Branch-free (two `select`s into the seeded
    /// `Option` aggregate). The payload coerces to one i64 word — exact for
    /// targets ≤ 64 bits; `i128`/`u128` payloads beyond i64 truncate (the same
    /// wide-int limitation the interpreter has).
    fn build_checked_to_int_option(
        &self,
        in_range: inkwell::values::IntValue<'ctx>,
        narrowed: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let zero = i64_t.const_zero();
        let one = i64_t.const_int(1, false);
        let payload = self.coerce_to_i64(narrowed.into())?;
        let tag = self
            .builder
            .build_select(in_range, one, zero, "chk.tag")
            .unwrap()
            .into_int_value();
        let w0 = self
            .builder
            .build_select(in_range, payload, zero, "chk.w0")
            .unwrap()
            .into_int_value();
        let option_ty = self
            .enum_layouts
            .get("Option")
            .ok_or("codegen: Option enum layout not seeded for checked_to_iN")?
            .llvm_type;
        let nfields = option_ty.count_fields();
        let mut agg = option_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, tag, 0, "chk.tag.f")
            .unwrap()
            .into_struct_value();
        // Field 1 = payload word 0; remaining payload words zeroed.
        for i in 1..nfields {
            let v = if i == 1 { w0 } else { zero };
            agg = self
                .builder
                .build_insert_value(agg, v, i, "chk.wf")
                .unwrap()
                .into_struct_value();
        }
        Ok(agg.into())
    }

    /// Lower one of the four float→int conversion method families (phase-8 cast
    /// slice 4). `Saturating` is identical to the `f as iN` cast; `Wrapping` is
    /// modular truncation; `Checked` yields `Option[iN]`; `Trunc` traps with a
    /// `panics` "float-to-int out of range" on NaN / out-of-range. Semantics
    /// match `crate::numeric_conv` (the slice-2 interpreter oracle).
    pub(super) fn emit_float_to_int_conv(
        &self,
        fv: inkwell::values::FloatValue<'ctx>,
        family: crate::numeric_conv::FloatToIntFamily,
        int_ty: inkwell::types::IntType<'ctx>,
        target_unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        use crate::numeric_conv::FloatToIntFamily as F;
        match family {
            F::Saturating => Ok(self
                .emit_float_to_int_sat(fv, int_ty, target_unsigned)?
                .into()),
            F::Wrapping => {
                // Modular truncation = low `bits` of the toward-zero integer.
                // For ≤64-bit targets, truncate the i128 saturating cast (exact,
                // matches the interpreter); for the 128-bit targets the i128 cast
                // is already the result.
                if int_ty.get_bit_width() >= 128 {
                    Ok(self
                        .emit_float_to_int_sat(fv, int_ty, target_unsigned)?
                        .into())
                } else {
                    let wide_ty = self.context.i128_type();
                    let wide = self.emit_float_to_int_sat(fv, wide_ty, false)?;
                    let narrowed = self
                        .builder
                        .build_int_truncate(wide, int_ty, "f2i.wrap")
                        .unwrap();
                    Ok(narrowed.into())
                }
            }
            F::Checked => {
                let (in_range, narrowed) =
                    self.emit_float_to_int_rangecheck(fv, int_ty, target_unsigned)?;
                self.build_checked_to_int_option(in_range, narrowed)
            }
            F::Trunc => {
                let (in_range, narrowed) =
                    self.emit_float_to_int_rangecheck(fv, int_ty, target_unsigned)?;
                let fn_val = self
                    .current_fn
                    .ok_or("codegen: trunc_to_iN outside a function")?;
                let panic_bb = self.context.append_basic_block(fn_val, "f2i.trap");
                let cont_bb = self.context.append_basic_block(fn_val, "f2i.ok");
                self.builder
                    .build_conditional_branch(in_range, cont_bb, panic_bb)
                    .unwrap();
                self.builder.position_at_end(panic_bb);
                self.emit_panic("float-to-int out of range");
                self.builder.build_unreachable().unwrap();
                self.builder.position_at_end(cont_bb);
                Ok(narrowed.into())
            }
        }
    }

    // ── Binary / unary operators ──────────────────────────────────

    /// Emit short-circuit `and` / `or` per documented design intent
    /// (roadmap.md:425, 429): the RHS is only compiled into a basic
    /// block reachable when the LHS doesn't already determine the
    /// result. Without this, the RHS would emit unconditionally and
    /// its side-effects (panicking index, dropped fn call) would fire
    /// even when short-circuited — same shape as the interpreter's
    /// eager-eval bug fixed in lockstep.
    pub(super) fn compile_short_circuit(
        &mut self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let lhs_val = self.compile_expr(left)?.into_int_value();
        let lhs_end_bb = self.builder.get_insert_block().unwrap();

        let rhs_bb = self.context.append_basic_block(fn_val, "sc.rhs");
        let merge_bb = self.context.append_basic_block(fn_val, "sc.merge");

        // `and`: lhs true → eval rhs; lhs false → short-circuit to false.
        // `or`:  lhs true → short-circuit to true; lhs false → eval rhs.
        let (true_dest, false_dest) = match op {
            BinOp::And => (rhs_bb, merge_bb),
            BinOp::Or => (merge_bb, rhs_bb),
            _ => unreachable!("compile_short_circuit only handles And/Or"),
        };
        self.builder
            .build_conditional_branch(lhs_val, true_dest, false_dest)
            .unwrap();

        // Bounds-check-elision propagation: when the RHS of `lhs and rhs`
        // fires, we've branch-proved that lhs holds. Any index-safety fact
        // asserted by lhs is in scope for rhs's compilation. This is how
        // the kata's `while lo >= 0 and hi < n and chars[lo] == chars[hi]`
        // pattern lets the indexing in the third conjunct skip its
        // bounds check — `lo >= 0` and `hi < n` are conjuncts evaluated
        // first under short-circuit, so by the time chars[lo] / chars[hi]
        // lower (in compile_vec_index), the facts are on the stack.
        let pushed = if matches!(op, BinOp::And) {
            let facts = self.collect_asserted_bounds_from_guard(left);
            let n = facts.len();
            self.asserted_index_bounds.extend(facts);
            n
        } else {
            0
        };

        self.builder.position_at_end(rhs_bb);
        let rhs_val = self.compile_expr(right)?.into_int_value();
        let rhs_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        // Pop the temporarily-asserted facts so the merge / surrounding
        // scope sees only its own bounds. Compile_while's body-entry push
        // re-establishes them for body code on the long-lived path.
        for _ in 0..pushed {
            self.asserted_index_bounds.pop();
        }

        self.builder.position_at_end(merge_bb);
        let bool_ty = self.context.bool_type();
        let short_const = match op {
            BinOp::And => bool_ty.const_int(0, false),
            BinOp::Or => bool_ty.const_int(1, false),
            _ => unreachable!(),
        };
        let phi = self.builder.build_phi(bool_ty, "sc.result").unwrap();
        phi.add_incoming(&[(&short_const, lhs_end_bb), (&rhs_val, rhs_end_bb)]);
        Ok(phi.as_basic_value())
    }

    pub(super) fn compile_binop(
        &mut self,
        op: &BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.compile_binop_typed(op, lhs, rhs, false)
    }

    /// Element-wise SIMD arithmetic on two `<N x T>` vectors (design.md
    /// § Portable SIMD, slice 1). The element type selects the integer vs
    /// float instruction family; `is_unsigned` switches `Div`/`Mod` to the
    /// unsigned integer forms. LLVM legalizes the `<N x T>` op to native SIMD
    /// where the target supports it and scalarizes otherwise (the auto-fallback
    /// rule, handled by the backend). Only `+ - * / %` reach here — the
    /// typechecker rejects every other operator on vectors.
    fn compile_vector_binop(
        &mut self,
        op: &BinOp,
        lv: VectorValue<'ctx>,
        rv: VectorValue<'ctx>,
        is_unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let is_float = lv.get_type().get_element_type().is_float_type();
        let result: BasicValueEnum<'ctx> = if is_float {
            match op {
                BinOp::Add => self.builder.build_float_add(lv, rv, "vadd").unwrap().into(),
                BinOp::Sub => self.builder.build_float_sub(lv, rv, "vsub").unwrap().into(),
                BinOp::Mul => self.builder.build_float_mul(lv, rv, "vmul").unwrap().into(),
                BinOp::Div => self.builder.build_float_div(lv, rv, "vdiv").unwrap().into(),
                BinOp::Mod => self.builder.build_float_rem(lv, rv, "vrem").unwrap().into(),
                // Comparisons → per-lane mask `<N x i1>`. Ordered float
                // predicates match the scalar float compares.
                BinOp::Eq => self
                    .builder
                    .build_float_compare(FloatPredicate::OEQ, lv, rv, "veq")
                    .unwrap()
                    .into(),
                BinOp::NotEq => self
                    .builder
                    .build_float_compare(FloatPredicate::ONE, lv, rv, "vne")
                    .unwrap()
                    .into(),
                BinOp::Lt => self
                    .builder
                    .build_float_compare(FloatPredicate::OLT, lv, rv, "vlt")
                    .unwrap()
                    .into(),
                BinOp::LtEq => self
                    .builder
                    .build_float_compare(FloatPredicate::OLE, lv, rv, "vle")
                    .unwrap()
                    .into(),
                BinOp::Gt => self
                    .builder
                    .build_float_compare(FloatPredicate::OGT, lv, rv, "vgt")
                    .unwrap()
                    .into(),
                BinOp::GtEq => self
                    .builder
                    .build_float_compare(FloatPredicate::OGE, lv, rv, "vge")
                    .unwrap()
                    .into(),
                _ => return Err(format!("unsupported vector float op {op:?}")),
            }
        } else {
            match op {
                BinOp::Add => self
                    .builder
                    .build_int_nsw_add(lv, rv, "vadd")
                    .unwrap()
                    .into(),
                BinOp::Sub => self
                    .builder
                    .build_int_nsw_sub(lv, rv, "vsub")
                    .unwrap()
                    .into(),
                BinOp::Mul => self
                    .builder
                    .build_int_nsw_mul(lv, rv, "vmul")
                    .unwrap()
                    .into(),
                BinOp::Div => {
                    if is_unsigned {
                        self.builder
                            .build_int_unsigned_div(lv, rv, "vdiv")
                            .unwrap()
                            .into()
                    } else {
                        self.builder
                            .build_int_signed_div(lv, rv, "vdiv")
                            .unwrap()
                            .into()
                    }
                }
                BinOp::Mod => {
                    if is_unsigned {
                        self.builder
                            .build_int_unsigned_rem(lv, rv, "vrem")
                            .unwrap()
                            .into()
                    } else {
                        self.builder
                            .build_int_signed_rem(lv, rv, "vrem")
                            .unwrap()
                            .into()
                    }
                }
                // Bitwise `& | ^` — integer lanes only (typechecker-enforced),
                // sign-agnostic so `is_unsigned` is irrelevant.
                BinOp::BitAnd => self.builder.build_and(lv, rv, "vand").unwrap().into(),
                BinOp::BitOr => self.builder.build_or(lv, rv, "vor").unwrap().into(),
                BinOp::BitXor => self.builder.build_xor(lv, rv, "vxor").unwrap().into(),
                // Comparisons → per-lane mask `<N x i1>`. `is_unsigned` (from the
                // operand's `unsigned_vector_exprs` span) picks `ult`/`ugt` over
                // `slt`/`sgt`, matching the scalar integer compares.
                BinOp::Eq => self
                    .builder
                    .build_int_compare(IntPredicate::EQ, lv, rv, "veq")
                    .unwrap()
                    .into(),
                BinOp::NotEq => self
                    .builder
                    .build_int_compare(IntPredicate::NE, lv, rv, "vne")
                    .unwrap()
                    .into(),
                BinOp::Lt => self
                    .builder
                    .build_int_compare(
                        if is_unsigned {
                            IntPredicate::ULT
                        } else {
                            IntPredicate::SLT
                        },
                        lv,
                        rv,
                        "vlt",
                    )
                    .unwrap()
                    .into(),
                BinOp::LtEq => self
                    .builder
                    .build_int_compare(
                        if is_unsigned {
                            IntPredicate::ULE
                        } else {
                            IntPredicate::SLE
                        },
                        lv,
                        rv,
                        "vle",
                    )
                    .unwrap()
                    .into(),
                BinOp::Gt => self
                    .builder
                    .build_int_compare(
                        if is_unsigned {
                            IntPredicate::UGT
                        } else {
                            IntPredicate::SGT
                        },
                        lv,
                        rv,
                        "vgt",
                    )
                    .unwrap()
                    .into(),
                BinOp::GtEq => self
                    .builder
                    .build_int_compare(
                        if is_unsigned {
                            IntPredicate::UGE
                        } else {
                            IntPredicate::SGE
                        },
                        lv,
                        rv,
                        "vge",
                    )
                    .unwrap()
                    .into(),
                _ => return Err(format!("unsupported vector int op {op:?}")),
            }
        };
        Ok(result)
    }

    /// Type-aware sibling to `compile_binop`. `is_unsigned == true` switches
    /// signedness-sensitive integer ops to their unsigned forms:
    /// `Div`/`Mod` → `build_int_unsigned_{div,rem}`, comparison predicates
    /// `Lt/LtEq/Gt/GtEq` → `ULT/ULE/UGT/UGE`, and `Shr` → logical (zero-fill)
    /// instead of arithmetic. Lowered primitive trait-method calls
    /// (`assoc_call.rs`) feed this with the type-name's signedness so e.g.
    /// `usize.lt(a, b)` lowers to `icmp ult` rather than `icmp slt` — without
    /// which LLVM emits the signed-aware `subs + cinc + asr` mid-point
    /// computation for `(lo + hi) / 2` shapes even on `u`-typed sources.
    pub(super) fn compile_binop_typed(
        &mut self,
        op: &BinOp,
        lhs: BasicValueEnum<'ctx>,
        rhs: BasicValueEnum<'ctx>,
        is_unsigned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // SIMD vector path: element-wise arithmetic on `<N x T>` operands
        // (design.md § Portable SIMD). Checked before the scalar paths since a
        // VectorValue would panic in `into_int_value()` / `to_float`. The
        // typechecker (`infer_vector_binary`) has already verified both sides
        // are the same `Vector[T, N]` and the op is one of `+ - * / %`.
        if lhs.is_vector_value() && rhs.is_vector_value() {
            return self.compile_vector_binop(
                op,
                lhs.into_vector_value(),
                rhs.into_vector_value(),
                is_unsigned,
            );
        }

        // Struct path: strings or user-defined structs.
        if lhs.is_struct_value() && rhs.is_struct_value() {
            let ls = lhs.into_struct_value();
            let rs = rhs.into_struct_value();
            let field_count = ls.get_type().count_fields();
            let vec_fields = self.vec_struct_type().count_fields();
            // String/Vec layout ({ ptr, i64, i64 }) — 3 fields.
            if field_count == vec_fields {
                return self.compile_string_binop(op, ls, rs);
            }
            // User struct equality: field-by-field comparison.
            if matches!(op, BinOp::Eq | BinOp::NotEq) {
                return self.compile_struct_eq(op, ls, rs);
            }
            return Err(format!("Unsupported struct binary op: {:?}", op));
        }

        // Float path
        if lhs.is_float_value() || rhs.is_float_value() {
            let lf = self.to_float(lhs)?;
            let rf = self.to_float(rhs)?;
            return self.compile_float_binop(op, lf, rf);
        }

        // Heterogeneous-shape guard. The struct path above only fires
        // when BOTH operands are struct values; the float path covers
        // the mixed-float case. Everything else must arrive as an
        // i1/i64-shaped IntValue. Pre-guard, a struct on one side and
        // an int on the other tripped `into_int_value()`'s panic
        // ("Found StructValue ... but expected the IntValue variant")
        // — e.g. `assert_eq(opt_int, 1)` where the typechecker doesn't
        // yet enforce `assert_eq[T]`'s same-type rule and `Option[i64]`
        // flowed through to here. Return a typed-mismatch error so the
        // surrounding caller (`compile_assert_eq`, `compile_assign`,
        // user `==` lowering) emits a structured diagnostic instead of
        // crashing codegen.
        if !lhs.is_int_value() {
            return Err(format!(
                "Binary op {op:?}: left operand has non-comparable type {:?} \
                 (likely a typechecker gap — `assert_eq` and `==` should reject \
                 mismatched operand types before reaching codegen)",
                lhs.get_type()
            ));
        }
        if !rhs.is_int_value() {
            return Err(format!(
                "Binary op {op:?}: right operand has non-comparable type {:?} \
                 (likely a typechecker gap — `assert_eq` and `==` should reject \
                 mismatched operand types before reaching codegen)",
                rhs.get_type()
            ));
        }
        let lv = lhs.into_int_value();
        let rv = rhs.into_int_value();
        // Width harmonization: a legal mixed-width int pair is always
        // "narrow-typed operand × default-i64 literal" (two
        // differently-typed int VARS are a type error; the Q4 rule
        // makes the typechecker type the op at the NARROW side) — so
        // truncate the wide side down to match source semantics.
        // Without this, `x + 1` on an `i8` param emits
        // `add nsw i8 %x, i64 1`, which fails module verification.
        // Mirror rationale in `compile_float_binop`'s harmonization.
        let (lv, rv) = {
            let lw = lv.get_type().get_bit_width();
            let rw = rv.get_type().get_bit_width();
            if lw > rw {
                (
                    self.builder
                        .build_int_truncate(lv, rv.get_type(), "iop.l.tr")
                        .unwrap(),
                    rv,
                )
            } else if rw > lw {
                (
                    lv,
                    self.builder
                        .build_int_truncate(rv, lv.get_type(), "iop.r.tr")
                        .unwrap(),
                )
            } else {
                (lv, rv)
            }
        };
        let result = match op {
            // Checked arithmetic — design.md § Arithmetic Overflow (trap on
            // app/lib profiles). These arms previously emitted bare `nsw`
            // ops, which declared overflow UB at the IR level while the
            // interpreter trapped; the divergence record lives in
            // `phase-7-codegen.md` § "AOT integer-overflow trapping". The
            // `with.overflow` intrinsics hand LLVM the same no-wrap fact on
            // the continue path that `nsw` asserted — but as a checked
            // runtime property. Panic messages match `eval_ops.rs` exactly.
            BinOp::Add => self.emit_checked_int_arith("add", lv, rv, is_unsigned)?,
            BinOp::Sub => self.emit_checked_int_arith("sub", lv, rv, is_unsigned)?,
            BinOp::Mul => self.emit_checked_int_arith("mul", lv, rv, is_unsigned)?,
            BinOp::Div => {
                self.emit_int_div_guards(lv, rv, is_unsigned);
                if is_unsigned {
                    self.builder.build_int_unsigned_div(lv, rv, "div").unwrap()
                } else {
                    self.builder.build_int_signed_div(lv, rv, "div").unwrap()
                }
            }
            BinOp::Mod => {
                self.emit_int_div_guards(lv, rv, is_unsigned);
                if is_unsigned {
                    self.builder.build_int_unsigned_rem(lv, rv, "mod").unwrap()
                } else {
                    self.builder.build_int_signed_rem(lv, rv, "mod").unwrap()
                }
            }
            BinOp::Eq => self
                .builder
                .build_int_compare(IntPredicate::EQ, lv, rv, "eq")
                .unwrap(),
            BinOp::NotEq => self
                .builder
                .build_int_compare(IntPredicate::NE, lv, rv, "ne")
                .unwrap(),
            BinOp::Lt => self
                .builder
                .build_int_compare(
                    if is_unsigned {
                        IntPredicate::ULT
                    } else {
                        IntPredicate::SLT
                    },
                    lv,
                    rv,
                    "lt",
                )
                .unwrap(),
            BinOp::LtEq => self
                .builder
                .build_int_compare(
                    if is_unsigned {
                        IntPredicate::ULE
                    } else {
                        IntPredicate::SLE
                    },
                    lv,
                    rv,
                    "le",
                )
                .unwrap(),
            BinOp::Gt => self
                .builder
                .build_int_compare(
                    if is_unsigned {
                        IntPredicate::UGT
                    } else {
                        IntPredicate::SGT
                    },
                    lv,
                    rv,
                    "gt",
                )
                .unwrap(),
            BinOp::GtEq => self
                .builder
                .build_int_compare(
                    if is_unsigned {
                        IntPredicate::UGE
                    } else {
                        IntPredicate::SGE
                    },
                    lv,
                    rv,
                    "ge",
                )
                .unwrap(),
            BinOp::And => self.builder.build_and(lv, rv, "and").unwrap(),
            BinOp::Or => self.builder.build_or(lv, rv, "or").unwrap(),
            BinOp::BitAnd => self.builder.build_and(lv, rv, "bitand").unwrap(),
            BinOp::BitOr => self.builder.build_or(lv, rv, "bitor").unwrap(),
            BinOp::BitXor => self.builder.build_xor(lv, rv, "bitxor").unwrap(),
            BinOp::Shl => self.builder.build_left_shift(lv, rv, "shl").unwrap(),
            BinOp::Shr => self
                .builder
                .build_right_shift(lv, rv, !is_unsigned, "shr")
                .unwrap(),
            _ => return Err(format!("Unsupported binary op: {:?}", op)),
        };
        Ok(result.into())
    }

    /// Checked integer `+` / `-` / `*` via the
    /// `llvm.{s,u}{add,sub,mul}.with.overflow.iN` intrinsic family,
    /// branching to an outlined panic site on the overflow flag.
    /// Implements design.md § Arithmetic Overflow (trap by default on
    /// `app`/`lib`) at the AOT surface, matching the interpreter's
    /// `checked_*` arms in `src/interpreter/eval_ops.rs` — message
    /// `integer overflow`. The `embedded`-profile wrapping default is
    /// future profile plumbing (see the phase-7 tracker entry); both
    /// shipped backends currently expose only app/lib semantics.
    ///
    /// `op_name` ∈ {"add", "sub", "mul"} — used for both the intrinsic
    /// name and IR labels (`add.chk` / `add.ovf.trap` / `add.ovf.ok`),
    /// which IR tests pin.
    fn emit_checked_int_arith(
        &mut self,
        op_name: &str,
        lv: inkwell::values::IntValue<'ctx>,
        rv: inkwell::values::IntValue<'ctx>,
        is_unsigned: bool,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let sign = if is_unsigned { 'u' } else { 's' };
        let name = format!("llvm.{sign}{op_name}.with.overflow");
        let intrinsic = inkwell::intrinsics::Intrinsic::find(&name)
            .ok_or_else(|| format!("{name} intrinsic must exist in LLVM"))?;
        let decl = intrinsic
            .get_declaration(&self.module, &[lv.get_type().into()])
            .ok_or_else(|| {
                format!(
                    "{name} has no declaration for width {}",
                    lv.get_type().get_bit_width()
                )
            })?;
        let pair = self
            .builder
            .build_call(decl, &[lv.into(), rv.into()], &format!("{op_name}.chk"))
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        let value = self
            .builder
            .build_extract_value(pair, 0, &format!("{op_name}.val"))
            .unwrap()
            .into_int_value();
        let overflowed = self
            .builder
            .build_extract_value(pair, 1, &format!("{op_name}.ovf"))
            .unwrap()
            .into_int_value();
        let fn_val = self.current_fn.unwrap();
        let trap_bb = self
            .context
            .append_basic_block(fn_val, &format!("{op_name}.ovf.trap"));
        let ok_bb = self
            .context
            .append_basic_block(fn_val, &format!("{op_name}.ovf.ok"));
        self.builder
            .build_conditional_branch(overflowed, trap_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(trap_bb);
        self.emit_panic("integer overflow");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);
        Ok(value)
    }

    /// Division/remainder guards — design.md § Arithmetic Overflow:
    /// `x / 0` and `x % 0` trap `division by zero`; signed
    /// `iN::MIN / -1` and `iN::MIN % -1` trap `integer overflow` (the
    /// mathematical result doesn't fit; LLVM `sdiv`/`srem` make it UB).
    /// Mirrors the interpreter's Div/Mod arms (`checked_div`/`checked_rem`
    /// after an explicit zero test). Unsigned types only need the zero
    /// guard. Without these, AOT div-by-zero was full IR-level UB —
    /// measured printing garbage where `karac run` traps (2026-06-07,
    /// `docs/investigations/bce_monotonic_assume.md` filing session).
    fn emit_int_div_guards(
        &mut self,
        lv: inkwell::values::IntValue<'ctx>,
        rv: inkwell::values::IntValue<'ctx>,
        is_unsigned: bool,
    ) {
        let ty = rv.get_type();
        let fn_val = self.current_fn.unwrap();

        let zero_trap = self.context.append_basic_block(fn_val, "div.zero.trap");
        let zero_ok = self.context.append_basic_block(fn_val, "div.zero.ok");
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, rv, ty.const_zero(), "div.is_zero")
            .unwrap();
        self.builder
            .build_conditional_branch(is_zero, zero_trap, zero_ok)
            .unwrap();
        self.builder.position_at_end(zero_trap);
        self.emit_panic("division by zero");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(zero_ok);

        if is_unsigned {
            return;
        }
        // Signed MIN / -1: build MIN as `1 << (w - 1)` via const ops so the
        // shape is width-generic (covers i128 without u64 literal overflow).
        let w = ty.get_bit_width();
        let min = ty
            .const_int(1, false)
            .const_shl(ty.const_int(u64::from(w) - 1, false));
        let ovf_trap = self.context.append_basic_block(fn_val, "div.ovf.trap");
        let ovf_ok = self.context.append_basic_block(fn_val, "div.ovf.ok");
        let lhs_is_min = self
            .builder
            .build_int_compare(IntPredicate::EQ, lv, min, "div.lhs_min")
            .unwrap();
        let rhs_is_m1 = self
            .builder
            .build_int_compare(IntPredicate::EQ, rv, ty.const_all_ones(), "div.rhs_m1")
            .unwrap();
        let both = self
            .builder
            .build_and(lhs_is_min, rhs_is_m1, "div.min_ovf")
            .unwrap();
        self.builder
            .build_conditional_branch(both, ovf_trap, ovf_ok)
            .unwrap();
        self.builder.position_at_end(ovf_trap);
        self.emit_panic("integer overflow");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ovf_ok);
    }

    pub(super) fn compile_struct_eq(
        &mut self,
        op: &BinOp,
        lhs: inkwell::values::StructValue<'ctx>,
        rhs: inkwell::values::StructValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let field_count = lhs.get_type().count_fields();
        let bool_t = self.context.bool_type();
        let mut result = bool_t.const_int(1, false); // start true (all equal)

        for i in 0..field_count {
            let l_field = self
                .builder
                .build_extract_value(lhs, i, &format!("l.f{}", i))
                .unwrap();
            let r_field = self
                .builder
                .build_extract_value(rhs, i, &format!("r.f{}", i))
                .unwrap();
            // Recursively compare the field.
            let field_eq = self.compile_binop(&BinOp::Eq, l_field, r_field)?;
            result = self
                .builder
                .build_and(result, field_eq.into_int_value(), &format!("eq.f{}", i))
                .unwrap();
        }

        if matches!(op, BinOp::NotEq) {
            Ok(self.builder.build_not(result, "struct_ne").unwrap().into())
        } else {
            Ok(result.into())
        }
    }

    pub(super) fn compile_string_binop(
        &self,
        op: &BinOp,
        lhs: inkwell::values::StructValue<'ctx>,
        rhs: inkwell::values::StructValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i32_t = self.context.i32_type();

        // Extract ptr and len from each string struct.
        let l_ptr = self
            .builder
            .build_extract_value(lhs, 0, "l.ptr")
            .unwrap()
            .into_pointer_value();
        let l_len = self
            .builder
            .build_extract_value(lhs, 1, "l.len")
            .unwrap()
            .into_int_value();
        let r_ptr = self
            .builder
            .build_extract_value(rhs, 0, "r.ptr")
            .unwrap()
            .into_pointer_value();
        let r_len = self
            .builder
            .build_extract_value(rhs, 1, "r.len")
            .unwrap()
            .into_int_value();

        match op {
            BinOp::Eq | BinOp::NotEq => {
                // Fast reject: if lengths differ, strings are not equal.
                let len_eq = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, l_len, r_len, "len_eq")
                    .unwrap();
                // memcmp the data.
                let cmp_result = self
                    .builder
                    .build_call(
                        self.memcmp_fn,
                        &[l_ptr.into(), r_ptr.into(), l_len.into()],
                        "memcmp",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let data_eq = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        cmp_result,
                        i32_t.const_int(0, false),
                        "data_eq",
                    )
                    .unwrap();
                let is_eq = self.builder.build_and(len_eq, data_eq, "str_eq").unwrap();
                if matches!(op, BinOp::NotEq) {
                    Ok(self.builder.build_not(is_eq, "str_ne").unwrap().into())
                } else {
                    Ok(is_eq.into())
                }
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                // Lexicographic comparison: memcmp on min(l_len, r_len), then compare lengths.
                let cmp_lens = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, l_len, r_len, "l_shorter")
                    .unwrap();
                let min_len = self
                    .builder
                    .build_select(cmp_lens, l_len, r_len, "min_len")
                    .unwrap()
                    .into_int_value();
                let cmp_result = self
                    .builder
                    .build_call(
                        self.memcmp_fn,
                        &[l_ptr.into(), r_ptr.into(), min_len.into()],
                        "memcmp",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let zero = i32_t.const_int(0, false);
                // If memcmp != 0, use its sign. If memcmp == 0, shorter string is "less".
                let cmp_is_zero = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, cmp_result, zero, "cmp_zero")
                    .unwrap();
                // When cmp == 0, compare lengths as signed i64 difference.
                let len_diff = self
                    .builder
                    .build_int_sub(l_len, r_len, "len_diff")
                    .unwrap();
                let len_diff_i32 = self
                    .builder
                    .build_int_truncate(len_diff, i32_t, "len_diff32")
                    .unwrap();
                let effective_cmp = self
                    .builder
                    .build_select(cmp_is_zero, len_diff_i32, cmp_result, "eff_cmp")
                    .unwrap()
                    .into_int_value();
                let pred = match op {
                    BinOp::Lt => IntPredicate::SLT,
                    BinOp::LtEq => IntPredicate::SLE,
                    BinOp::Gt => IntPredicate::SGT,
                    BinOp::GtEq => IntPredicate::SGE,
                    _ => unreachable!(),
                };
                let result = self
                    .builder
                    .build_int_compare(pred, effective_cmp, zero, "str_cmp")
                    .unwrap();
                Ok(result.into())
            }
            BinOp::Add => {
                // String concatenation: allocate new buffer, copy both, return new string.
                let new_len = self.builder.build_int_add(l_len, r_len, "cat_len").unwrap();
                let new_buf = self
                    .builder
                    .build_call(self.malloc_fn, &[new_len.into()], "cat_buf")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                // Copy left.
                self.builder
                    .build_memcpy(new_buf, 1, l_ptr, 1, l_len)
                    .unwrap();
                // Copy right after left.
                let i8_ty = self.context.i8_type();
                let dest2 = unsafe {
                    self.builder
                        .build_gep(i8_ty, new_buf, &[l_len], "cat_dest2")
                        .unwrap()
                };
                self.builder
                    .build_memcpy(dest2, 1, r_ptr, 1, r_len)
                    .unwrap();
                // Build result string struct.
                let str_ty = self.vec_struct_type();
                let mut agg = str_ty.get_undef();
                agg = self
                    .builder
                    .build_insert_value(agg, new_buf, 0, "cat.ptr")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, new_len, 1, "cat.len")
                    .unwrap()
                    .into_struct_value();
                agg = self
                    .builder
                    .build_insert_value(agg, new_len, 2, "cat.cap")
                    .unwrap()
                    .into_struct_value();
                Ok(agg.into())
            }
            _ => Err(format!("Unsupported string binary op: {:?}", op)),
        }
    }

    /// Scalar width coercion at a typed ABI boundary (a `ret` whose
    /// function declares a sub-64-bit type, a call arg landing in a
    /// narrower-declared param). Kāra codegen's internal convention is
    /// default-width scalars — unsuffixed int literals and annotated
    /// `let` slots are i64, float literals f64 — while function
    /// signatures lower at their declared width, so a legal program
    /// reaches the boundary with a wider value than the slot
    /// (`ret i64 0` vs `i32`, `call i8 @f(i64 5)`). Truncate down /
    /// extend up to the declared type; same-class same-width and every
    /// non-scalar shape pass through untouched, so this is safe to
    /// apply unconditionally at the boundary (it never converts across
    /// classes — the verifier still catches genuinely-wrong IR).
    /// Widening uses sext (Kāra's default int literal type is signed;
    /// legal programs only widen via explicit `as`, which carries its
    /// own signedness — see `compile_cast`).
    pub(super) fn coerce_scalar_to_type(
        &self,
        val: BasicValueEnum<'ctx>,
        target: BasicTypeEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match (val, target) {
            (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(tt)) => {
                let src_w = iv.get_type().get_bit_width();
                let dst_w = tt.get_bit_width();
                if dst_w < src_w {
                    self.builder
                        .build_int_truncate(iv, tt, "bnd.tr")
                        .unwrap()
                        .into()
                } else if dst_w > src_w {
                    self.builder
                        .build_int_s_extend(iv, tt, "bnd.sx")
                        .unwrap()
                        .into()
                } else {
                    val
                }
            }
            (BasicValueEnum::FloatValue(fv), BasicTypeEnum::FloatType(ft))
                if fv.get_type() != ft =>
            {
                self.builder
                    .build_float_cast(fv, ft, "bnd.fcast")
                    .unwrap()
                    .into()
            }
            _ => val,
        }
    }

    /// Boundary coercion for a struct-field store/insert: coerce a
    /// scalar value to the aggregate's declared field type. Without
    /// this, `S { b: 200 }` against `struct S { b: u8 }` built the
    /// aggregate with the raw i64 literal in an i8 member — a
    /// malformed constant that slips past verification under opaque
    /// pointers and reads back as garbage (the `s.b` → 0 repro,
    /// 2026-06-06) — and the shared-struct heap path stored 8 bytes
    /// over a 1-byte field, corrupting neighbors. No-op when the
    /// index is out of range or either side is non-scalar.
    pub(super) fn coerce_to_struct_field_ty(
        &self,
        agg_ty: inkwell::types::StructType<'ctx>,
        field_index: u32,
        val: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match agg_ty.get_field_type_at_index(field_index) {
            Some(ft) => self.coerce_scalar_to_type(val, ft),
            None => val,
        }
    }

    /// Boundary coercion for the current function's `ret`: coerce a
    /// scalar return value to the declared LLVM return type. No-op for
    /// void fns and non-scalar returns.
    pub(super) fn coerce_to_current_ret_type(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match self.current_fn.and_then(|f| f.get_type().get_return_type()) {
            Some(ret_ty) => self.coerce_scalar_to_type(val, ret_ty),
            None => val,
        }
    }

    /// Boundary coercion for call args: coerce each scalar arg to the
    /// callee's declared param type (`call i8 @f(i64 5)` →
    /// `call i8 @f(i8 5)`). Walks only the zip of declared params ×
    /// supplied args, so variadic tails and arity mismatches (the
    /// verifier's problem, not ours) pass through.
    pub(super) fn coerce_args_to_fn_params(
        &self,
        func: inkwell::values::FunctionValue<'ctx>,
        args: &mut [inkwell::values::BasicMetadataValueEnum<'ctx>],
    ) {
        for (param_ty, arg) in func
            .get_type()
            .get_param_types()
            .iter()
            .zip(args.iter_mut())
        {
            let val: BasicValueEnum<'ctx> = match *arg {
                inkwell::values::BasicMetadataValueEnum::IntValue(iv) => iv.into(),
                inkwell::values::BasicMetadataValueEnum::FloatValue(fv) => fv.into(),
                _ => continue,
            };
            let target: BasicTypeEnum<'ctx> = match *param_ty {
                inkwell::types::BasicMetadataTypeEnum::IntType(t) => t.into(),
                inkwell::types::BasicMetadataTypeEnum::FloatType(t) => t.into(),
                _ => continue,
            };
            let coerced = self.coerce_scalar_to_type(val, target);
            *arg = inkwell::values::BasicMetadataValueEnum::from(coerced);
        }
    }

    pub(super) fn compile_float_binop(
        &self,
        op: &BinOp,
        lf: inkwell::values::FloatValue<'ctx>,
        rf: inkwell::values::FloatValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Width harmonization: a legal mixed-width pair is always "real
        // f32 operand × default-f64 float literal" (two differently-
        // typed float VARS are a type error), and the typechecker typed
        // the op at the narrower f32 — so cast the wide side DOWN. The
        // literal-valued side converts exactly for any value the
        // narrow type represents; computing in f32 matches source
        // semantics (vs. widening, which would double-round on the way
        // back down at the ret/arg boundary).
        let f32_t = self.context.f32_type();
        let f64_t = self.context.f64_type();
        let (lf, rf) = if lf.get_type() == f64_t && rf.get_type() == f32_t {
            (
                self.builder
                    .build_float_cast(lf, f32_t, "fop.l.tr")
                    .unwrap(),
                rf,
            )
        } else if lf.get_type() == f32_t && rf.get_type() == f64_t {
            (
                lf,
                self.builder
                    .build_float_cast(rf, f32_t, "fop.r.tr")
                    .unwrap(),
            )
        } else {
            (lf, rf)
        };
        match op {
            BinOp::Add => Ok(self.builder.build_float_add(lf, rf, "fadd").unwrap().into()),
            BinOp::Sub => Ok(self.builder.build_float_sub(lf, rf, "fsub").unwrap().into()),
            BinOp::Mul => Ok(self.builder.build_float_mul(lf, rf, "fmul").unwrap().into()),
            BinOp::Div => Ok(self.builder.build_float_div(lf, rf, "fdiv").unwrap().into()),
            BinOp::Mod => Ok(self.builder.build_float_rem(lf, rf, "frem").unwrap().into()),
            BinOp::Eq => Ok(self
                .builder
                .build_float_compare(FloatPredicate::OEQ, lf, rf, "feq")
                .unwrap()
                .into()),
            BinOp::NotEq => Ok(self
                .builder
                .build_float_compare(FloatPredicate::ONE, lf, rf, "fne")
                .unwrap()
                .into()),
            BinOp::Lt => Ok(self
                .builder
                .build_float_compare(FloatPredicate::OLT, lf, rf, "flt")
                .unwrap()
                .into()),
            BinOp::LtEq => Ok(self
                .builder
                .build_float_compare(FloatPredicate::OLE, lf, rf, "fle")
                .unwrap()
                .into()),
            BinOp::Gt => Ok(self
                .builder
                .build_float_compare(FloatPredicate::OGT, lf, rf, "fgt")
                .unwrap()
                .into()),
            BinOp::GtEq => Ok(self
                .builder
                .build_float_compare(FloatPredicate::OGE, lf, rf, "fge")
                .unwrap()
                .into()),
            _ => Err(format!("Unsupported float binary op: {:?}", op)),
        }
    }

    pub(super) fn to_float(
        &self,
        val: BasicValueEnum<'ctx>,
    ) -> Result<inkwell::values::FloatValue<'ctx>, String> {
        match val {
            BasicValueEnum::FloatValue(f) => Ok(f),
            BasicValueEnum::IntValue(i) => Ok(self
                .builder
                .build_signed_int_to_float(i, self.context.f64_type(), "itof")
                .unwrap()),
            _ => Err(format!("Cannot convert {:?} to float", val.get_type())),
        }
    }

    pub(super) fn compile_unaryop(
        &mut self,
        op: &UnaryOp,
        val: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match op {
            UnaryOp::Neg => {
                if val.is_float_value() {
                    Ok(self
                        .builder
                        .build_float_neg(val.into_float_value(), "fneg")
                        .unwrap()
                        .into())
                } else {
                    // Checked negate: `-iN::MIN` doesn't fit and traps as
                    // `integer overflow`, matching the interpreter's
                    // `checked_neg` arm (`eval_ops.rs`). Lowered as checked
                    // `0 - v` through the same `ssub.with.overflow` path the
                    // binary ops use.
                    let v = val.into_int_value();
                    let zero = v.get_type().const_zero();
                    Ok(self.emit_checked_int_arith("sub", zero, v, false)?.into())
                }
            }
            UnaryOp::Not | UnaryOp::BitNot => {
                // Integer-lane `Vector[T, N]` complement (`~v`): `build_not`
                // is generic over `IntMathValue`, so it lowers a `<N x iX>`
                // operand directly. Logical `!` only ever sees `bool`.
                if val.is_vector_value() {
                    Ok(self
                        .builder
                        .build_not(val.into_vector_value(), "vnot")
                        .unwrap()
                        .into())
                } else {
                    Ok(self
                        .builder
                        .build_not(val.into_int_value(), "not")
                        .unwrap()
                        .into())
                }
            }
            // Deref is handled in compile_expr before reaching here.
            UnaryOp::Deref => Err("unreachable: Deref handled in compile_expr".into()),
        }
    }

    // ── Slice coercion ────────────────────────────────────────────

    /// Synthesize a `{ptr, i64}` slice header at a call site when the
    /// argument is an Array, Vec, or Slice value and the callee parameter
    /// expects `Slice[T]` / `mut Slice[T]`.
    ///
    /// Returns `Ok(None)` when the argument is not a recognized
    /// sequence source, signalling the caller to fall back to the
    /// default argument-passing path.
    pub(super) fn coerce_to_slice(
        &mut self,
        arg: &Expr,
        elem_ty: BasicTypeEnum<'ctx>,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let slice_ty = self.slice_struct_type();

        // Fast path: the argument is a named local variable whose
        // representation we already understand.
        if let ExprKind::Identifier(var_name) = &arg.kind {
            if let Some(slot) = self.variables.get(var_name.as_str()).copied() {
                // Owned Array[T, N]: point at the alloca, length is N.
                if let BasicTypeEnum::ArrayType(at) = slot.ty {
                    let len = i64_t.const_int(at.len() as u64, false);
                    return Ok(Some(self.build_slice_header(slice_ty, slot.ptr, len)));
                }
                // Already a slice: load and pass through.
                if self.slice_elem_types.contains_key(var_name.as_str()) {
                    let loaded = self
                        .builder
                        .build_load(slice_ty, slot.ptr, "slice.arg")
                        .unwrap();
                    return Ok(Some(loaded));
                }
                // Owned Vec[T]: the alloca holds the 3-field struct; load
                // its data-ptr and len fields, rebuild as a 2-field slice.
                if self.vec_elem_types.contains_key(var_name.as_str()) {
                    let vec_ty = self.vec_struct_type();
                    let data_ptr_ptr = self
                        .builder
                        .build_struct_gep(vec_ty, slot.ptr, 0, "coerce.v.data.ptr")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_ptr_ptr, "coerce.v.data")
                        .unwrap()
                        .into_pointer_value();
                    let len_ptr = self
                        .builder
                        .build_struct_gep(vec_ty, slot.ptr, 1, "coerce.v.len.ptr")
                        .unwrap();
                    let len = self
                        .builder
                        .build_load(i64_t, len_ptr, "coerce.v.len")
                        .unwrap()
                        .into_int_value();
                    return Ok(Some(self.build_slice_header(slice_ty, data, len)));
                }
            }
            // Ref parameter: pointer-to-data is in ref_params.
            if let Some(&BasicTypeEnum::ArrayType(at)) = self.ref_params.get(var_name.as_str()) {
                let data = self.get_data_ptr(var_name).unwrap();
                let len = i64_t.const_int(at.len() as u64, false);
                return Ok(Some(self.build_slice_header(slice_ty, data, len)));
            }
        }

        // Range-indexing at a call boundary — e.g. `sum(a[1..4])`. Produce
        // a slice header with pointer-into-source and length `end - start`.
        if let ExprKind::Index { object, index } = &arg.kind {
            if let ExprKind::Range {
                start,
                end,
                inclusive,
            } = &index.kind
            {
                return self
                    .compile_range_slice(object, start, end, *inclusive, elem_ty)
                    .map(Some);
            }
        }

        // Anonymous collection literal at a call boundary — `f([1, 2, 3])`.
        // Named arrays / Vecs hit the Identifier fast path above; a bare
        // literal has no alloca, so materialize it and build a slice header.
        // Without this the literal reaches the call as a raw aggregate and
        // fails LLVM verification (param-type mismatch); the interpreter
        // accepts the same literal -> Slice coercion, so this brings codegen
        // in line.
        if let Some((data, len)) = self.collection_literal_slice_parts(arg)? {
            return Ok(Some(self.build_slice_header(slice_ty, data, len)));
        }

        let _ = elem_ty;
        Ok(None)
    }

    /// For an anonymous collection literal — a bare `[..]` (which lowering
    /// keeps as `ArrayLiteral` when typed `Array[T, N]`, or rewrites to a
    /// `Vec` `PrefixCollectionLiteral` otherwise) — compile it and return
    /// `(data_ptr, len)` suitable for a slice header or range-slice base.
    /// An array value is materialized to a temp alloca (`&arr == &arr[0]`,
    /// so a one-index GEP from the result lands at `&arr[i]`); a Vec value
    /// is unpacked to its `{data, len}` fields. Returns `None` if `expr`
    /// isn't a collection literal, so the caller's compile is never
    /// double-emitted.
    pub(super) fn collection_literal_slice_parts(
        &mut self,
        expr: &Expr,
    ) -> Result<Option<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>)>, String> {
        let is_lit = matches!(&expr.kind, ExprKind::ArrayLiteral(_))
            || matches!(&expr.kind, ExprKind::PrefixCollectionLiteral { type_name, .. }
                if type_name == "Vec");
        if !is_lit {
            return Ok(None);
        }
        let i64_t = self.context.i64_type();
        let compiled = self.compile_expr(expr)?;
        match compiled.get_type() {
            BasicTypeEnum::ArrayType(at) => {
                let fn_val = self.current_fn.unwrap();
                let tmp = self.create_entry_alloca(fn_val, "lit.slice.tmp", at.into());
                self.builder.build_store(tmp, compiled).unwrap();
                Ok(Some((tmp, i64_t.const_int(at.len() as u64, false))))
            }
            BasicTypeEnum::StructType(_) => {
                // Vec value `{ptr,len,cap}`: pull out the data ptr + len.
                let sv = compiled.into_struct_value();
                let data = self
                    .builder
                    .build_extract_value(sv, 0, "lit.data")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(sv, 1, "lit.len")
                    .unwrap()
                    .into_int_value();
                Ok(Some((data, len)))
            }
            _ => Err("collection literal compiled to neither array nor Vec value".into()),
        }
    }

    /// Assemble a two-field slice struct value from a data pointer and an
    /// i64 length.
    pub(super) fn build_slice_header(
        &self,
        slice_ty: StructType<'ctx>,
        data_ptr: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let mut agg = slice_ty.get_undef();
        agg = self
            .builder
            .build_insert_value(agg, data_ptr, 0, "slice.ptr")
            .unwrap()
            .into_struct_value();
        agg = self
            .builder
            .build_insert_value(agg, len, 1, "slice.len")
            .unwrap()
            .into_struct_value();
        agg.into()
    }

    /// Construct a slice from a `collection[start..end]` expression —
    /// emits a bounds check and produces a `{ptr + start*stride, end - start}`
    /// slice header.
    pub(super) fn compile_range_slice(
        &mut self,
        object: &Expr,
        start: &Option<Box<Expr>>,
        end: &Option<Box<Expr>>,
        inclusive: bool,
        elem_ty: BasicTypeEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let slice_ty = self.slice_struct_type();

        let start_val = if let Some(s) = start {
            self.compile_expr(s)?.into_int_value()
        } else {
            i64_t.const_int(0, false)
        };
        // Compile end expression now if present; defer to src_len for open-end
        // forms (`a..` and `..`). Inclusive adjustment applied after src_len
        // is resolved.
        let compiled_end = if let Some(e) = end {
            Some(self.compile_expr(e)?.into_int_value())
        } else {
            None
        };

        // Resolve the object to (base_ptr, length).
        let (base_ptr, src_len) = if let ExprKind::Identifier(name) = &object.kind {
            if let Some(slot) = self.variables.get(name.as_str()).copied() {
                if let BasicTypeEnum::ArrayType(at) = slot.ty {
                    (slot.ptr, i64_t.const_int(at.len() as u64, false))
                } else if self.slice_elem_types.contains_key(name.as_str()) {
                    let data_pp = self
                        .builder
                        .build_struct_gep(slice_ty, slot.ptr, 0, "rs.s.data.pp")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_pp, "rs.s.data")
                        .unwrap()
                        .into_pointer_value();
                    let len_p = self
                        .builder
                        .build_struct_gep(slice_ty, slot.ptr, 1, "rs.s.len.p")
                        .unwrap();
                    let len = self
                        .builder
                        .build_load(i64_t, len_p, "rs.s.len")
                        .unwrap()
                        .into_int_value();
                    (data, len)
                } else if self.vec_elem_types.contains_key(name.as_str()) {
                    let vec_ty = self.vec_struct_type();
                    let data_pp = self
                        .builder
                        .build_struct_gep(vec_ty, slot.ptr, 0, "rs.v.data.pp")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_pp, "rs.v.data")
                        .unwrap()
                        .into_pointer_value();
                    let len_p = self
                        .builder
                        .build_struct_gep(vec_ty, slot.ptr, 1, "rs.v.len.p")
                        .unwrap();
                    let len = self
                        .builder
                        .build_load(i64_t, len_p, "rs.v.len")
                        .unwrap()
                        .into_int_value();
                    (data, len)
                } else {
                    return Err(format!(
                        "range-slice requires Array, Vec, or Slice source; variable '{}' is neither",
                        name
                    ));
                }
            } else if self.ref_params.contains_key(name.as_str()) {
                // Ref-parameter path: pointer to inner data.
                let inner = *self.ref_params.get(name.as_str()).unwrap();
                if let BasicTypeEnum::ArrayType(at) = inner {
                    let data = self.get_data_ptr(name).unwrap();
                    (data, i64_t.const_int(at.len() as u64, false))
                } else {
                    return Err("range-slice on ref parameter requires ref Array".into());
                }
            } else {
                return Err(format!("Undefined variable '{}' in range-slice", name));
            }
        } else if let Some((data, len)) = self.collection_literal_slice_parts(object)? {
            // Anonymous collection-literal source — `[1, 2, 3][a..b]`. Named
            // arrays hit the Identifier path above; a bare literal has no
            // alloca and (depending on its inferred type) lowers to either an
            // `[N x T]` array value or a Vec `{ptr,len,cap}` value. The helper
            // materializes either into a `(base_ptr, len)` whose base is the
            // address of element 0, so the `source_is_array == false`
            // one-index GEP below lands at `&elem[start]` correctly. The
            // interpreter accepts the same form (`[7, 8][0..2]`).
            (data, len)
        } else {
            return Err("range-slice requires a named source variable".into());
        };

        // Resolve end: open-end (`a..`, `..`) uses src_len; inclusive adds 1.
        let mut end_val = compiled_end.unwrap_or(src_len);
        if inclusive {
            end_val = self
                .builder
                .build_int_add(end_val, i64_t.const_int(1, false), "end.incl")
                .unwrap();
        }

        // Bounds check: start <= end && end <= len.
        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "slice.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "slice.ok");
        let se_bad = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, start_val, end_val, "s.le.e")
            .unwrap();
        let el_bad = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, end_val, src_len, "e.le.len")
            .unwrap();
        let any_bad = self.builder.build_or(se_bad, el_bad, "slice.bad").unwrap();
        self.builder
            .build_conditional_branch(any_bad, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("slice range out of bounds");
        self.builder.build_unreachable().unwrap();
        self.builder.position_at_end(ok_bb);

        // For an Array source, `base_ptr` is the alloca of `[N x T]` —
        // compute the element pointer via two-index GEP. For a Vec / Slice
        // source, `base_ptr` is already an element pointer, so we use a
        // one-index GEP. We distinguish by asking whether the source var is
        // an array alloca (known type) or a loaded data pointer.
        let source_is_array = if let ExprKind::Identifier(name) = &object.kind {
            if let Some(slot) = self.variables.get(name.as_str()) {
                matches!(slot.ty, BasicTypeEnum::ArrayType(_))
            } else if let Some(&inner) = self.ref_params.get(name.as_str()) {
                matches!(inner, BasicTypeEnum::ArrayType(_))
            } else {
                false
            }
        } else {
            false
        };

        let elem_ptr = if source_is_array {
            // GEP into `[N x T]*` using [0, start].
            let arr_ty = if let ExprKind::Identifier(name) = &object.kind {
                if let Some(slot) = self.variables.get(name.as_str()).copied() {
                    slot.ty
                } else if let Some(&inner) = self.ref_params.get(name.as_str()) {
                    inner
                } else {
                    return Err("range-slice: lost array type".into());
                }
            } else {
                return Err("range-slice: non-identifier array source".into());
            };
            let zero = i64_t.const_int(0, false);
            unsafe {
                self.builder
                    .build_gep(arr_ty, base_ptr, &[zero, start_val], "slice.elem.ptr")
                    .unwrap()
            }
        } else {
            // GEP into `T*` using [start].
            unsafe {
                self.builder
                    .build_gep(elem_ty, base_ptr, &[start_val], "slice.elem.ptr")
                    .unwrap()
            }
        };

        let new_len = self
            .builder
            .build_int_sub(end_val, start_val, "slice.new.len")
            .unwrap();
        Ok(self.build_slice_header(slice_ty, elem_ptr, new_len))
    }
}
