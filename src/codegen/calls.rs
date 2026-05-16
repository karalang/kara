//! Function-call and method-call compilation.
//!
//! Houses `compile_assoc_call` (associated/free fn dispatch),
//! `compile_method_call` (object method dispatch including the big
//! per-receiver-type dispatch table), `compile_indexed_receiver_method`
//! (slice/vec indexed-receiver methods), `compile_for_indexed_iter`
//! (for-loop iteration over indexed sources), `compile_nested_index_read`
//! (`a[i][j]`-style chained index reads), `compile_entry_chain_receiver_method`
//! (map `entry().or_insert(...)` chains), the `lower_indexed_elem_ptr_*`
//! helpers (`vec`, `slice`, `array`), and `inferred_receiver_type` for
//! method-call receiver type recovery.

use crate::ast::*;

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    /// Slice MR helper: lower an indexed-receiver method call
    /// `obj[i].method(args)`. Computes the element pointer through the outer
    /// container's index machinery, synthesizes an identifier name pointing
    /// into the outer storage with the element's type registries populated,
    /// recursively dispatches the method through the existing identifier
    /// path, and cleans up the synth registrations on return.
    ///
    /// Locked design choices (MR1–MR5, see `phase-7-codegen.md`):
    /// - MR1 receiver-shape early dispatch at the top of `compile_method_call`.
    /// - MR2 routes by container shape (Vec/Slice/Array), not method name.
    /// - MR3 read-only and mutating methods both flow through the same path
    ///   — the elem pointer aliases the outer storage so writes propagate.
    /// - MR4 synthesized name `__indexed_elem_<n>` + per-call-site temporary
    ///   registry injection + post-call cleanup.
    /// - MR5 chained `a[i][j].method()` is rejected (single-level only in v1).
    pub(super) fn compile_indexed_receiver_method(
        &mut self,
        inner: &Expr,
        index: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // MR5: reject chained indexed receivers up front. The user must bind
        // the inner element to a temporary first.
        if matches!(inner.kind, ExprKind::Index { .. }) {
            return Err(format!(
                "codegen: chained indexed receivers (`a[i][j].{}(...)`) are deferred to v1.x; \
                 bind the inner element to a temporary first",
                method
            ));
        }

        // Container must be an identifier in v1 — `get_grid()[i].push(x)` is
        // out of scope. The error mirrors the existing fall-through diagnostic.
        let outer_name = if let ExprKind::Identifier(name) = &inner.kind {
            name.clone()
        } else {
            return Err(format!(
                "codegen: indexed-receiver method '{}' requires the indexed container to be a \
                 named variable in v1 (got non-identifier inner expression)",
                method
            ));
        };

        // Determine the element TypeExpr from the outer's recorded element
        // type. Without this we can't populate the synth's side tables, so
        // the recursive dispatch would fall through to the silent-`0` arm.
        let elem_te = self
            .var_elem_type_exprs
            .get(outer_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: indexed-receiver method '{}' on '{}' — element TypeExpr unknown \
                     (outer is not a tracked Vec/Slice/Array variable)",
                    method, outer_name
                )
            })?;

        // Lower the index access to an element pointer through the outer's
        // container-shape-specific path. Bounds check goes through
        // `emit_panic` on OOB; the OK BB leaves the builder positioned for
        // the post-elem-ptr work.
        let (elem_ptr, elem_ll_ty) = if self.vec_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_vec(&outer_name, index)?
        } else if self.slice_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_slice(&outer_name, index)?
        } else {
            // Array shape via slot.ty inspection. v1 supports fixed-size
            // arrays only when the slot's LLVM type is ArrayType.
            let slot = self
                .variables
                .get(outer_name.as_str())
                .copied()
                .ok_or_else(|| {
                    format!(
                        "codegen: indexed-receiver method '{}' — outer '{}' has no slot",
                        method, outer_name
                    )
                })?;
            if let BasicTypeEnum::ArrayType(_) = slot.ty {
                self.lower_indexed_elem_ptr_array(slot, index)?
            } else {
                return Err(format!(
                    "codegen: indexed-receiver method '{}' on '{}' — outer is not a Vec/Slice/Array",
                    method, outer_name
                ));
            }
        };

        // Mint a fresh synth name and register it so the recursive dispatch
        // sees a regular identifier-receiver flow.
        let synth = format!("__indexed_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: elem_ptr,
                ty: elem_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &elem_te);
        // User-struct receiver: also populate `var_type_names` so the
        // impl-block dispatch path resolves `Type.method`.
        if let TypeKind::Path(path) = &elem_te.kind {
            if let Some(seg) = path.segments.first() {
                if self.struct_types.contains_key(seg.as_str()) {
                    self.var_type_names.insert(synth.clone(), seg.clone());
                }
            }
        }

        // Build a fresh Identifier expr at the original call site's span and
        // recursively dispatch. The recursive call will skip this arm
        // (Identifier, not Index) and fall into the regular flow.
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span);

        // Clean up synth registrations.  The LLVM IR is already emitted; this
        // is bookkeeping cleanup so subsequent compilations in the same
        // function don't see stale entries.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);

        result
    }

    /// Slice FR (2026-05-16): field-receiver method dispatch. Sibling to
    /// `compile_indexed_receiver_method` (MR slice) for the
    /// `outer.field.method(...)` shape. The outer must be a named
    /// variable bound to a struct (shared or plain) so we can recover
    /// the struct name from `var_type_names` and the per-field LLVM /
    /// `TypeExpr` info from the declaration registries. Returns
    /// `Ok(None)` when the shape isn't a known struct field — caller
    /// falls through to the regular dispatch.
    ///
    /// Locked design choices (FR1–FR4, sibling to MR1–MR5):
    /// - FR1 receiver-shape early dispatch at the top of
    ///   `compile_method_call`.
    /// - FR2 routes by struct kind (shared via heap-GEP, plain via
    ///   slot-GEP), not by method name.
    /// - FR3 synth identifier `__field_elem_<n>` bound to the field
    ///   pointer with the field's TypeExpr-derived registries
    ///   populated through `register_var_from_type_expr`; both
    ///   read-only and mutating methods flow through the same path
    ///   because the field pointer aliases the parent storage.
    /// - FR4 chained `outer.a.b.method()` is rejected with a clear
    ///   diagnostic — bind the inner field to a temporary first.
    pub(super) fn try_compile_field_receiver_method(
        &mut self,
        inner: &Expr,
        field: &str,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // FR4: reject chained field receivers up front.
        if matches!(inner.kind, ExprKind::FieldAccess { .. }) {
            return Err(format!(
                "codegen: chained field receivers (`a.b.c.{}(...)`) are deferred to v1.x; \
                 bind the inner field to a temporary first",
                method
            ));
        }
        // Outer must be a named variable so we can look up its struct
        // type. Anything else (a call return, an index, …) falls through
        // to the regular dispatch; the existing fall-through diagnostic
        // already says the right thing.
        let outer_name = match &inner.kind {
            ExprKind::Identifier(n) => n.clone(),
            _ => return Ok(None),
        };
        let type_name = match self.var_type_names.get(outer_name.as_str()).cloned() {
            Some(t) => t,
            None => return Ok(None),
        };
        // Look up the field's declaration-order index and full TypeExpr.
        let field_idx = match self
            .struct_field_names
            .get(&type_name)
            .and_then(|names| names.iter().position(|n| n == field))
        {
            Some(i) => i,
            None => return Ok(None),
        };
        let field_te = match self
            .struct_field_type_exprs
            .get(&type_name)
            .and_then(|tes| tes.get(field_idx).cloned())
        {
            Some(te) => te,
            None => return Ok(None),
        };

        // GEP the field pointer. Shared: load the handle, GEP at
        // (idx + 1) past the refcount slot. Plain: GEP directly into
        // the slot at idx.
        let (field_ptr, field_ll_ty) =
            if let Some(info) = self.shared_types.get(&type_name).cloned() {
                if info.is_enum {
                    return Ok(None);
                }
                // Load the handle pointer from the outer var slot.
                let slot = self
                    .variables
                    .get(outer_name.as_str())
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "codegen: field-receiver method '{}' — outer '{}' has no slot",
                            method, outer_name
                        )
                    })?;
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let handle = self
                    .builder
                    .build_load(ptr_ty, slot.ptr, "fr.shared.handle")
                    .unwrap()
                    .into_pointer_value();
                let fp = self
                    .builder
                    .build_struct_gep(
                        info.heap_type,
                        handle,
                        (field_idx + 1) as u32,
                        &format!("fr_sh_{}", field),
                    )
                    .unwrap();
                let fty = info
                    .heap_type
                    .get_field_type_at_index((field_idx + 1) as u32)
                    .ok_or_else(|| {
                        format!(
                        "codegen: field-receiver method '{}' on '{}.{}' — field LLVM type missing",
                        method, type_name, field
                    )
                    })?;
                (fp, fty)
            } else if let Some(st) = self.struct_types.get(&type_name).copied() {
                // Plain struct: outer's slot stores the struct by value, so
                // GEP into the slot directly.
                let slot = self
                    .variables
                    .get(outer_name.as_str())
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "codegen: field-receiver method '{}' — outer '{}' has no slot",
                            method, outer_name
                        )
                    })?;
                let fp = self
                    .builder
                    .build_struct_gep(st, slot.ptr, field_idx as u32, &format!("fr_pl_{}", field))
                    .unwrap();
                let fty = st
                    .get_field_type_at_index(field_idx as u32)
                    .ok_or_else(|| {
                        format!(
                    "codegen: field-receiver method '{}' on '{}.{}' — field LLVM type missing",
                    method, type_name, field
                )
                    })?;
                (fp, fty)
            } else {
                // Not a tracked struct shape — fall through.
                return Ok(None);
            };

        // Mint a fresh synth identifier and populate its registries so
        // the recursive dispatch sees a regular Identifier-receiver flow.
        let synth = format!("__field_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: field_ptr,
                ty: field_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &field_te);
        // User-struct field: also populate `var_type_names` so the
        // impl-block dispatch path resolves `Type.method`.
        if let TypeKind::Path(path) = &field_te.kind {
            if let Some(seg) = path.segments.first() {
                if self.struct_types.contains_key(seg.as_str())
                    || self.shared_types.contains_key(seg.as_str())
                {
                    self.var_type_names.insert(synth.clone(), seg.clone());
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span);

        // Clean up synth registrations.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);

        result.map(Some)
    }

    /// Nested indexed read codegen (`a[i][j]`) — sibling to
    /// `compile_indexed_receiver_method` (MR slice). The outer
    /// container `a` must be a named variable in v1; chained
    /// `a[i][j][k]` rejected with a clear diagnostic. The inner index
    /// lowers to an element pointer via the same per-container
    /// machinery (`lower_indexed_elem_ptr_vec` / `_slice` / `_array`),
    /// a synth identifier is minted with the right side-table
    /// registrations, then `compile_index` is re-invoked with the
    /// synth as the outer object so the existing identifier-keyed
    /// dispatch (`compile_vec_index` / `compile_slice_index` /
    /// generic Array path) handles the second index correctly.
    /// Drive `for x in coll[i].iter()` codegen by synthesizing a
    /// temp identifier for the indexed receiver, registering it in
    /// the appropriate elem-type tables, and recursing into
    /// `compile_for` with the synth as the iterable. Mirrors
    /// `compile_nested_index_read` for the read-only side.
    pub(super) fn compile_for_indexed_iter(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        outer: &Expr,
        idx: &Expr,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if matches!(outer.kind, ExprKind::Index { .. }) {
            return Err(
                "codegen: `for x in a[i][j].iter()` (chained indexed receiver) \
                 is deferred — bind the intermediate element first"
                    .to_string(),
            );
        }
        let outer_name = if let ExprKind::Identifier(name) = &outer.kind {
            name.clone()
        } else {
            return Err(
                "codegen: indexed-receiver `.iter()` requires the outer container \
                 to be a named variable in v1"
                    .to_string(),
            );
        };
        let elem_te = self
            .var_elem_type_exprs
            .get(outer_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: `for x in {}[i].iter()` — outer element TypeExpr unknown",
                    outer_name
                )
            })?;
        let (elem_ptr, elem_ll_ty) = if self.vec_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_vec(&outer_name, idx)?
        } else if self.slice_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_slice(&outer_name, idx)?
        } else {
            let slot = self
                .variables
                .get(outer_name.as_str())
                .copied()
                .ok_or_else(|| {
                    format!(
                        "codegen: `for x in {}[i].iter()` — outer has no slot",
                        outer_name
                    )
                })?;
            if let BasicTypeEnum::ArrayType(_) = slot.ty {
                self.lower_indexed_elem_ptr_array(slot, idx)?
            } else {
                return Err(format!(
                    "codegen: `for x in {}[i].iter()` — outer is not a Vec/Slice/Array",
                    outer_name
                ));
            }
        };
        let synth = format!("__indexed_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: elem_ptr,
                ty: elem_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &elem_te);
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: outer.span.clone(),
        };
        let result = self.compile_for(label, pattern, &synth_expr, body);
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        result
    }

    pub(super) fn compile_nested_index_read(
        &mut self,
        inner_object: &Expr,
        inner_idx: &Expr,
        outer_idx: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // MR5 symmetric guard: chained `a[i][j][k]` not supported.
        if matches!(inner_object.kind, ExprKind::Index { .. }) {
            return Err(
                "codegen: chained indexed reads (`a[i][j][k]`) are deferred to v1.x; \
                 bind the intermediate element to a temporary first"
                    .to_string(),
            );
        }
        let outer_name = if let ExprKind::Identifier(name) = &inner_object.kind {
            name.clone()
        } else {
            return Err(
                "codegen: nested indexed read requires the outer container to be a \
                 named variable in v1 (got non-identifier inner expression)"
                    .to_string(),
            );
        };
        // Recover the element TypeExpr — needed to populate the synth
        // identifier's vec_elem_types / slice_elem_types registrations.
        let elem_te = self
            .var_elem_type_exprs
            .get(outer_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: nested indexed read on '{}' — element TypeExpr unknown \
                     (outer is not a tracked Vec/Slice/Array variable)",
                    outer_name
                )
            })?;
        // Lower the inner `outer[i]` to an element pointer + LLVM type.
        let (elem_ptr, elem_ll_ty) = if self.vec_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_vec(&outer_name, inner_idx)?
        } else if self.slice_elem_types.contains_key(outer_name.as_str()) {
            self.lower_indexed_elem_ptr_slice(&outer_name, inner_idx)?
        } else {
            let slot = self
                .variables
                .get(outer_name.as_str())
                .copied()
                .ok_or_else(|| {
                    format!(
                        "codegen: nested indexed read — outer '{}' has no slot",
                        outer_name
                    )
                })?;
            if let BasicTypeEnum::ArrayType(_) = slot.ty {
                self.lower_indexed_elem_ptr_array(slot, inner_idx)?
            } else {
                return Err(format!(
                    "codegen: nested indexed read on '{}' — outer is not a Vec/Slice/Array",
                    outer_name
                ));
            }
        };
        // Mint a synth identifier so the recursive call sees the
        // inner element as a regular Vec/Slice/Array variable.
        let synth = format!("__indexed_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: elem_ptr,
                ty: elem_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &elem_te);
        // Rebuild the outer Index expression against the synth
        // identifier and dispatch.
        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner_object.span.clone(),
        };
        let result = self.compile_index(&synth_expr, outer_idx);
        // Tear down the per-call synth registrations so subsequent
        // dispatch sites don't see a stale entry.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        result
    }

    /// Trailing-method dispatch on an entry-chain receiver. When the call
    /// is
    /// `<m.entry(k){.and_modify(f)}*.{or_insert|or_insert_with}(d)>.method(args)`,
    /// the inner chain produces a slot pointer (`*mut V`, the LLVM
    /// realisation of `mut ref V` per `design.md § Entry[K, V]`).
    /// Mirrors `compile_indexed_receiver_method`: mint a synth identifier
    /// bound to the slot pointer with V's side-tables populated, recurse
    /// into `compile_method_call` with the synth as receiver, tear down on
    /// exit. Closes the LeetCode 3629 kata's canonical
    /// `bucket.entry(p).or_insert(Vec.new()).push(j)` shape.
    ///
    /// Returns `Ok(None)` when the receiver isn't a recognised
    /// or_insert / or_insert_with chain, so the caller falls through to
    /// the regular dispatch (which surfaces its own diagnostic for
    /// unrecognised non-identifier receivers).
    pub(super) fn compile_entry_chain_receiver_method(
        &mut self,
        inner_object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Inner receiver must itself be a method call ending in
        // or_insert / or_insert_with. and_modify-terminal returns the
        // Entry struct, not a slot pointer, so we don't peel that here.
        let ExprKind::MethodCall {
            object: chain_recv,
            method: inner_method,
            args: inner_args,
            ..
        } = &inner_object.kind
        else {
            return Ok(None);
        };
        if !matches!(inner_method.as_str(), "or_insert" | "or_insert_with") {
            return Ok(None);
        }

        // Walk chain_recv (peeling and_modify wrappers) to find the map
        // identifier. Mirrors the loop in `try_compile_entry_chain`.
        let map_name = {
            let mut current: &Expr = chain_recv;
            loop {
                let ExprKind::MethodCall {
                    object: inner_obj,
                    method: m,
                    args: inner_args2,
                    ..
                } = &current.kind
                else {
                    return Ok(None);
                };
                if m == "entry" && inner_args2.len() == 1 {
                    let ExprKind::Identifier(name) = &inner_obj.kind else {
                        return Ok(None);
                    };
                    break name.clone();
                } else if m == "and_modify" && inner_args2.len() == 1 {
                    current = inner_obj;
                } else {
                    return Ok(None);
                }
            }
        };

        // Receiver must be a tracked Map variable; without map_val_types
        // we can't size the synth slot.
        if !self.map_key_types.contains_key(map_name.as_str()) {
            return Ok(None);
        }
        let val_te = self
            .var_elem_type_exprs
            .get(map_name.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "codegen: entry-chain trailing-method '{}' on map '{}' \
                     — value TypeExpr unknown",
                    method, map_name
                )
            })?;
        let val_ty = *self.map_val_types.get(map_name.as_str()).ok_or_else(|| {
            format!(
                "codegen: entry-chain trailing-method '{}' on map '{}' \
                     — value LLVM type unknown",
                method, map_name
            )
        })?;

        // Compile the inner chain — returns the slot pointer (`*mut V`).
        let slot_value = self
            .try_compile_entry_chain(chain_recv, inner_method, inner_args)?
            .ok_or_else(|| {
                format!(
                    "codegen: entry-chain trailing-method '{}' — inner chain \
                     '{}' unexpectedly didn't compile as an entry chain",
                    method, inner_method
                )
            })?;
        let slot_ptr = slot_value.into_pointer_value();

        // Mint the synth identifier. Same teardown contract as
        // compile_indexed_receiver_method — entries are bookkeeping for
        // the recursive dispatch only; synth owns no allocation.
        let synth = format!("__entry_slot_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: slot_ptr,
                ty: val_ty,
            },
        );
        self.register_var_from_type_expr(&synth, &val_te);
        if let TypeKind::Path(path) = &val_te.kind {
            if let Some(seg) = path.segments.first() {
                if self.struct_types.contains_key(seg.as_str()) {
                    self.var_type_names.insert(synth.clone(), seg.clone());
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner_object.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span);

        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);

        Ok(Some(result?))
    }

    /// Slice MR: lower `outer[i]` for an outer Vec[T] receiver into an
    /// element pointer + element LLVM type. Bounds-checks against `len`
    /// (not `cap`). Mirrors `compile_vec_index`'s machinery.
    pub(super) fn lower_indexed_elem_ptr_vec(
        &mut self,
        outer_name: &str,
        index: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let elem_ty = self.vec_elem_type_for_var(outer_name);
        let vec_ptr = self.get_data_ptr(outer_name).ok_or_else(|| {
            format!(
                "Undefined Vec variable '{}' in indexed-receiver lowering",
                outer_name
            )
        })?;
        let idx_val = self.compile_expr(index)?.into_int_value();

        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 1, "v.mr.len.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "v.mr.len")
            .unwrap()
            .into_int_value();
        let data_pp = self
            .builder
            .build_struct_gep(vec_ty, vec_ptr, 0, "v.mr.data.ptr")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "v.mr.data")
            .unwrap()
            .into_pointer_value();

        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "v.mr.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "v.mr.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx_val, len, "bounds")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("vec index out of bounds");
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(ok_bb);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "v.mr.elem.ptr")
                .unwrap()
        };
        Ok((elem_ptr, elem_ty))
    }

    /// Slice MR: lower `outer[i]` for an outer Slice[T] receiver.
    pub(super) fn lower_indexed_elem_ptr_slice(
        &mut self,
        outer_name: &str,
        index: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let slice_ty = self.slice_struct_type();
        let elem_ty = *self.slice_elem_types.get(outer_name).ok_or_else(|| {
            format!(
                "Undefined Slice variable '{}' in indexed-receiver lowering",
                outer_name
            )
        })?;
        let slice_ptr = self.get_data_ptr(outer_name).ok_or_else(|| {
            format!(
                "Undefined Slice variable '{}' in indexed-receiver lowering",
                outer_name
            )
        })?;
        let idx_val = self.compile_expr(index)?.into_int_value();

        let data_pp = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 0, "s.mr.data.pp")
            .unwrap();
        let len_p = self
            .builder
            .build_struct_gep(slice_ty, slice_ptr, 1, "s.mr.len.p")
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "s.mr.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_p, "s.mr.len")
            .unwrap()
            .into_int_value();

        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "s.mr.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "s.mr.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx_val, len, "bounds")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("slice index out of bounds");
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(ok_bb);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[idx_val], "s.mr.elem.ptr")
                .unwrap()
        };
        Ok((elem_ptr, elem_ty))
    }

    /// Slice MR: lower `outer[i]` for a fixed-size Array[T, N] receiver.
    pub(super) fn lower_indexed_elem_ptr_array(
        &mut self,
        slot: VarSlot<'ctx>,
        index: &Expr,
    ) -> Result<(PointerValue<'ctx>, BasicTypeEnum<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let arr_ty = match slot.ty {
            BasicTypeEnum::ArrayType(at) => at,
            _ => return Err("Array shape required for Array indexed-receiver lowering".to_string()),
        };
        let elem_ty = arr_ty.get_element_type();
        let idx_val = self.compile_expr(index)?.into_int_value();
        let len = i64_t.const_int(arr_ty.len() as u64, false);

        let fn_val = self.current_fn.unwrap();
        let oob_bb = self.context.append_basic_block(fn_val, "a.mr.oob");
        let ok_bb = self.context.append_basic_block(fn_val, "a.mr.ok");
        let cmp = self
            .builder
            .build_int_compare(IntPredicate::UGE, idx_val, len, "bounds")
            .unwrap();
        self.builder
            .build_conditional_branch(cmp, oob_bb, ok_bb)
            .unwrap();
        self.builder.position_at_end(oob_bb);
        self.emit_panic("array index out of bounds");
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(ok_bb);
        let zero = i64_t.const_int(0, false);
        let elem_ptr = unsafe {
            self.builder
                .build_gep(arr_ty, slot.ptr, &[zero, idx_val], "a.mr.elem.ptr")
                .unwrap()
        };
        Ok((elem_ptr, elem_ty))
    }

    /// Infer the declared struct/enum type name of a method-call receiver,
    /// or `None` if we can't — in which case the caller falls back to its
    /// built-in/primitive handling. Keys off `var_type_names`, which the
    /// existing struct-literal and struct-param paths populate.
    pub(super) fn inferred_receiver_type(&self, object: &Expr) -> Option<String> {
        if let ExprKind::Identifier(name) = &object.kind {
            return self.var_type_names.get(name.as_str()).cloned();
        }
        None
    }
}
