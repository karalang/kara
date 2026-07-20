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
    /// Tear down the per-call-site registry entries a hoisted synthetic
    /// container / element binding installed (`variables` + every collection
    /// side-table `register_var_from_type_expr` may have populated). The LLVM
    /// IR is already emitted by the time this runs; this is bookkeeping cleanup
    /// so later compilations in the same function don't see stale synth names.
    /// Shared by the MR indexed-receiver path and the `lower_field_access_ptr`
    /// field-container / indexed-container hoists.
    pub(super) fn unregister_synth_container(&mut self, name: &str) {
        self.variables.remove(name);
        self.vec_elem_types.remove(name);
        self.slice_elem_types.remove(name);
        self.var_elem_type_exprs.remove(name);
        self.var_type_names.remove(name);
        self.map_key_types.remove(name);
        self.map_val_types.remove(name);
        self.map_key_type_names.remove(name);
        self.map_key_type_exprs.remove(name);
        self.set_elem_types.remove(name);
        self.set_elem_type_names.remove(name);
        self.set_elem_type_exprs.remove(name);
    }

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

        // B-2026-07-09-1: hoist a FieldAccess container — `self.names[i].m()` —
        // to a synth Vec/Slice identifier so the identifier-keyed lowering
        // below applies unchanged. Surfaced by std.protobuf `#[derive(Message)]`
        // on repeated-`Vec[String]` / Map fields, whose generated encode loop
        // emits `self.<field>[i].bytes()`; without the hoist the
        // container-must-be-a-named-variable guard rejects it (the interpreter
        // accepts it, so it was a run-vs-build divergence). We resolve the field
        // to its storage pointer + declared TypeExpr, register a synth binding
        // for it (so `vec_elem_types` / `var_elem_type_exprs` / slot are all in
        // place), and rewrite `inner` to that synth identifier. Cleaned up at
        // the end alongside the element synth.
        let mut hoisted_container: Option<String> = None;
        let inner_synth: Option<Expr> = if let ExprKind::FieldAccess { object, field } = &inner.kind
        {
            // `self` parses as `SelfValue`; normalise to the "self" binding the
            // per-var registries key on (mirrors the field-receiver method path
            // in `compile_method_call`).
            let self_ident;
            let obj: &Expr = if matches!(object.kind, ExprKind::SelfValue) {
                self_ident = Expr {
                    kind: ExprKind::Identifier("self".to_string()),
                    span: object.span.clone(),
                };
                &self_ident
            } else {
                object
            };
            match self.lower_field_access_ptr(
                obj,
                field,
                &format!("indexed-receiver method '{method}'"),
            )? {
                Some((field_ptr, field_ll_ty, field_te)) => {
                    let synth = format!("__field_container_{}", self.indexed_elem_counter);
                    self.indexed_elem_counter += 1;
                    self.variables.insert(
                        synth.clone(),
                        VarSlot {
                            ptr: field_ptr,
                            ty: field_ll_ty,
                        },
                    );
                    self.register_var_from_type_expr(&synth, &field_te);
                    let expr = Expr {
                        kind: ExprKind::Identifier(synth.clone()),
                        span: inner.span.clone(),
                    };
                    hoisted_container = Some(synth);
                    Some(expr)
                }
                // Not a known struct field — leave `inner` as-is so the
                // existing non-identifier diagnostic fires below.
                None => None,
            }
        } else if let ExprKind::TupleIndex {
            object: tup,
            index: tidx,
        } = &inner.kind
        {
            // Hoist a tuple-element Vec container — `t.0[i].method()` — to a
            // synth Vec identifier, the `TupleIndex` sibling of the
            // `FieldAccess` arm above. GEP into the tuple element's storage
            // (structural, so inferred + annotated bindings both work); the
            // element `TypeExpr` comes from the typechecker's
            // `temp_recv_elem_types` (recorded for the `Index` receiver `t.0`).
            // Without this the container-must-be-a-named-variable guard rejected
            // it while the interpreter accepted it (B-2026-07-20-4). `Vec`/
            // `VecDeque` elements only; any gap leaves `inner` as-is so the
            // existing diagnostic fires.
            let key = (inner.span.offset, inner.span.length);
            let hoisted = self
                .temp_recv_elem_types
                .get(&key)
                .cloned()
                .and_then(|elem_te| {
                    let vec_te = super::Codegen::vec_type_expr_from_element(&elem_te);
                    let elem_ptr = self.field_chain_place_ptr(inner)?;
                    let tuple_ty = self.place_chain_aggregate_llvm_type(tup)?;
                    let elem_ll_ty = tuple_ty.get_field_type_at_index(*tidx as u32)?;
                    let synth = format!("__tup_container_{}", self.indexed_elem_counter);
                    self.indexed_elem_counter += 1;
                    self.variables.insert(
                        synth.clone(),
                        VarSlot {
                            ptr: elem_ptr,
                            ty: elem_ll_ty,
                        },
                    );
                    self.register_var_from_type_expr(&synth, &vec_te);
                    let expr = Expr {
                        kind: ExprKind::Identifier(synth.clone()),
                        span: inner.span.clone(),
                    };
                    hoisted_container = Some(synth);
                    Some(expr)
                });
            hoisted
        } else if let Some(vec_te) = self.map_get_unwrap_vec_value_te(inner) {
            // B-2026-07-15-27: hoist a `<map>.get(k).unwrap()` Vec-value BORROW
            // container — `m.get(k).unwrap()[i].method()` — to a synth Vec
            // identifier so the identifier-keyed indexed-receiver lowering
            // below applies unchanged. Unlike the FieldAccess arm (which points
            // the synth at live field storage), the borrow is a VALUE, so we
            // materialize the `{ptr,len,cap=0}` view (B-2026-07-15-26 zeroed the
            // cap) into a synth Vec local. No teardown drop: the map owns the
            // buffer AND its elements, and the element pointer the recursion
            // GEPs aliases the map's live bucket (MR3), so a read method reads
            // it in place — matching the interpreter's `ref`-borrow semantics.
            let vec_val = self.compile_expr(inner)?;
            if self.llvm_ty_is_vec_struct(vec_val.get_type()) {
                let fn_val = self.current_fn.unwrap();
                let synth = format!("__mapget_container_{}", self.indexed_elem_counter);
                self.indexed_elem_counter += 1;
                let slot = self.create_entry_alloca(fn_val, "mapget.vec.tmp", vec_val.get_type());
                self.builder.build_store(slot, vec_val).unwrap();
                self.variables.insert(
                    synth.clone(),
                    VarSlot {
                        ptr: slot,
                        ty: vec_val.get_type(),
                    },
                );
                self.register_var_from_type_expr(&synth, &vec_te);
                let expr = Expr {
                    kind: ExprKind::Identifier(synth.clone()),
                    span: inner.span.clone(),
                };
                hoisted_container = Some(synth);
                Some(expr)
            } else {
                // Not actually a Vec struct — leave for the diagnostic below.
                None
            }
        } else {
            None
        };
        let inner: &Expr = inner_synth.as_ref().unwrap_or(inner);

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
                    self.record_var_type_name(synth.clone(), seg.clone());
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
        let result = self.compile_method_call(&synth_expr, method, args, call_span, call_span);

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

        // B-2026-07-09-1: tear down the hoisted FieldAccess-container synth
        // (same registry set as the element synth above), if one was minted.
        if let Some(c) = hoisted_container {
            self.variables.remove(&c);
            self.vec_elem_types.remove(&c);
            self.slice_elem_types.remove(&c);
            self.var_elem_type_exprs.remove(&c);
            self.var_type_names.remove(&c);
            self.map_key_types.remove(&c);
            self.map_val_types.remove(&c);
            self.map_key_type_names.remove(&c);
            self.map_key_type_exprs.remove(&c);
            self.set_elem_types.remove(&c);
            self.set_elem_type_names.remove(&c);
            self.set_elem_type_exprs.remove(&c);
        }

        result
    }

    /// Resolve `inner.field` to the field's storage pointer + LLVM type +
    /// declared `TypeExpr`, for FieldAccess-rooted receivers. Shared by
    /// `try_compile_field_receiver_method` (the `obj.field.method(...)` FR
    /// slice) and `compile_index`'s FieldAccess arm (`obj.field[i]`), so the
    /// struct-kind routing (shared heap-GEP incl. phase-D headerless layout
    /// vs plain slot-GEP vs `ref` param deref) and the `outer[i].field`
    /// indexed-inner shape live in exactly one place. `ctx` is the
    /// diagnostic label naming the consuming construct (e.g. `method 'push'`
    /// or `index expression`). Returns `Ok(None)` when the shape isn't a
    /// known struct field — callers fall through to their regular dispatch.
    pub(super) fn lower_field_access_ptr(
        &mut self,
        inner: &Expr,
        field: &str,
        ctx: &str,
    ) -> Result<Option<(PointerValue<'ctx>, BasicTypeEnum<'ctx>, TypeExpr)>, String> {
        // FR4: reject chained field receivers up front.
        if matches!(inner.kind, ExprKind::FieldAccess { .. }) {
            return Err(format!(
                "codegen: chained field receivers (`a.b.c…`) are deferred to v1.x; \
                 bind the inner field to a temporary first ({ctx})"
            ));
        }
        // Recover the struct type name + the receiver-pointer the field
        // GEP should hang off. Two recognised shapes:
        //   1. `outer.field.method(...)` — Identifier inner. Receiver-
        //      pointer is the variable's slot or (for `shared struct`s)
        //      the loaded handle pointer.
        //   2. `outer[i].field.method(...)` — Index inner. Receiver-
        //      pointer is the element-pointer returned by the per-
        //      container indexed-elem helper. Closes the kata-133 inner
        //      loop's `nodes[i as u64].neighbors.push(nodes[j as u64])`.
        // Any other shape falls through to the regular dispatch with the
        // existing fall-through diagnostic.
        let (type_name, receiver_ptr, is_shared_handle) = match &inner.kind {
            ExprKind::Identifier(outer_name) => {
                let type_name = match self.var_type_names.get(outer_name.as_str()).cloned() {
                    Some(t) => t,
                    None => return Ok(None),
                };
                let slot = self
                    .variables
                    .get(outer_name.as_str())
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "codegen: field-receiver {} — outer '{}' has no slot",
                            ctx, outer_name
                        )
                    })?;
                // For shared structs, the slot stores the heap-pointer
                // handle; load it to get the receiver-pointer the field
                // GEP indexes into. For plain structs, the slot itself
                // IS the receiver pointer — UNLESS the binding is a
                // `ref T` parameter, in which case the slot holds a
                // pointer-to-struct (the caller's struct) and we need
                // to dereference once to get to the struct, same shape
                // as the shared-struct case. Without the deref, the
                // GEP indexes into the alloca slot's first 8 bytes
                // (which hold the pointer value) and reads junk past
                // it — surfaces as a silent runtime segfault when the
                // field's read kernel touches the resulting garbage
                // (e.g. `karac_clone_String` dereferencing a bad
                // `{ptr, len, cap}` triple). Bug from the helper-fn
                // Json kata gap surfaced 2026-05-22.
                let is_shared = self.shared_types.contains_key(&type_name);
                let is_ref_param = self.ref_params.contains_key(outer_name.as_str());
                let recv_ptr = if is_shared {
                    // Shared receiver: the heap pointer is whatever
                    // `load_variable` yields — a single load for an owned
                    // binding (slot holds the handle), a *double* load for a
                    // `ref self` / `ref` binding (slot holds a pointer to the
                    // handle slot, so one load yields `&self`, not the heap
                    // struct). `compile_expr` walks that exact chain via
                    // `load_variable`'s `ref_params` deref, so it returns the
                    // heap struct pointer in both cases. A bare single
                    // `build_load` here lands one indirection short for a
                    // `ref self` shared receiver — the field GEP then reads a
                    // garbage `{ptr,len,cap}` (indexed access traps / OOBs).
                    // Mirrors `compile_field_store`'s shared branch, which
                    // resolves `self.field = v` the same way.
                    self.compile_expr(inner)?.into_pointer_value()
                } else if is_ref_param {
                    // Plain (non-shared) `ref T` receiver: slot holds a
                    // pointer-to-struct (the caller's struct); a single deref
                    // yields the struct address. Without it the GEP indexes
                    // into the alloca's first 8 bytes (the pointer value) and
                    // reads junk past it — a silent segfault when the field's
                    // read kernel touches the resulting garbage. (Helper-fn
                    // Json kata gap, 2026-05-22.)
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    self.builder
                        .build_load(ptr_ty, slot.ptr, "fr.ref.deref")
                        .unwrap()
                        .into_pointer_value()
                } else {
                    slot.ptr
                };
                (type_name, recv_ptr, is_shared)
            }
            ExprKind::Index {
                object: container,
                index,
            } => {
                // FR5: chained `a[i][j].field.method()` rejected — bind
                // the intermediate element first. Mirrors MR5.
                if matches!(container.kind, ExprKind::Index { .. }) {
                    return Err(format!(
                        "codegen: chained indexed field receivers \
                         (`a[i][j].field…`) are deferred to v1.x; \
                         bind the intermediate element first ({ctx})"
                    ));
                }
                // The indexed container is normally a named Vec/Slice/Array
                // variable. A FieldAccess container (`o.inners[i].xs.method()` /
                // `self.rows[i].cols…`) is hoisted to a synth Vec identifier
                // first — resolve the field to its storage pointer + declared
                // element registries, register a synth, and index that. This is
                // the field-of-a-ref-struct nested-receiver shape B-2026-07-11-11
                // (`o.inners[0].xs.push(v)`); mirrors the sibling B-2026-07-09-1
                // hoist in `compile_indexed_receiver_method`. `self` normalises
                // to the "self" binding the per-var registries key on.
                let mut hoisted_idx_container: Option<String> = None;
                let outer_name = match &container.kind {
                    ExprKind::Identifier(n) => n.clone(),
                    ExprKind::FieldAccess {
                        object: fobj,
                        field: ffield,
                    } => {
                        let self_ident;
                        let fobj: &Expr = if matches!(fobj.kind, ExprKind::SelfValue) {
                            self_ident = Expr {
                                kind: ExprKind::Identifier("self".to_string()),
                                span: fobj.span.clone(),
                            };
                            &self_ident
                        } else {
                            fobj
                        };
                        match self.lower_field_access_ptr(fobj, ffield, ctx)? {
                            Some((field_ptr, field_ll_ty, field_te)) => {
                                let synth =
                                    format!("__idx_field_container_{}", self.indexed_elem_counter);
                                self.indexed_elem_counter += 1;
                                self.variables.insert(
                                    synth.clone(),
                                    VarSlot {
                                        ptr: field_ptr,
                                        ty: field_ll_ty,
                                    },
                                );
                                self.register_var_from_type_expr(&synth, &field_te);
                                hoisted_idx_container = Some(synth.clone());
                                synth
                            }
                            None => return Ok(None),
                        }
                    }
                    _ => return Ok(None),
                };
                // Recover the element TypeExpr to learn the struct type
                // name. The container must be a tracked Vec/Slice/Array;
                // its element-TypeExpr was populated at binding time.
                let elem_te = match self.var_elem_type_exprs.get(outer_name.as_str()).cloned() {
                    Some(te) => te,
                    None => {
                        if let Some(c) = &hoisted_idx_container {
                            self.unregister_synth_container(c);
                        }
                        return Ok(None);
                    }
                };
                let elem_type_name = match &elem_te.kind {
                    TypeKind::Path(p) => match p.segments.first() {
                        Some(s) => s.clone(),
                        None => {
                            if let Some(c) = &hoisted_idx_container {
                                self.unregister_synth_container(c);
                            }
                            return Ok(None);
                        }
                    },
                    _ => {
                        if let Some(c) = &hoisted_idx_container {
                            self.unregister_synth_container(c);
                        }
                        return Ok(None);
                    }
                };
                // Lower the inner `container[index]` to an element pointer
                // via the same per-container helper the MR-slice
                // indexed-receiver arm uses. Bounds-check goes through
                // `emit_panic` on OOB and leaves the builder on the OK BB.
                let (elem_ptr, _elem_ll_ty) =
                    if self.vec_elem_types.contains_key(outer_name.as_str()) {
                        self.lower_indexed_elem_ptr_vec(&outer_name, index)?
                    } else if self.slice_elem_types.contains_key(outer_name.as_str()) {
                        self.lower_indexed_elem_ptr_slice(&outer_name, index)?
                    } else {
                        let slot = self
                            .variables
                            .get(outer_name.as_str())
                            .copied()
                            .ok_or_else(|| {
                                format!(
                                    "codegen: indexed-field-receiver {} — outer '{}' has no slot",
                                    ctx, outer_name
                                )
                            })?;
                        if let BasicTypeEnum::ArrayType(_) = slot.ty {
                            self.lower_indexed_elem_ptr_array(slot, index)?
                        } else {
                            if let Some(c) = &hoisted_idx_container {
                                self.unregister_synth_container(c);
                            }
                            return Ok(None);
                        }
                    };
                // For shared-struct elements, the element slot stores the
                // heap-pointer handle; load it to get the receiver-pointer
                // the field GEP indexes into. For plain-struct elements,
                // the element pointer itself IS the receiver pointer.
                let is_shared = self.shared_types.contains_key(&elem_type_name);
                let recv_ptr = if is_shared {
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    self.builder
                        .build_load(ptr_ty, elem_ptr, "fr.idx.shared.handle")
                        .unwrap()
                        .into_pointer_value()
                } else {
                    elem_ptr
                };
                // The synth container is no longer referenced past the
                // element-pointer computation; tear it down before the field
                // GEP so no stale registry entry survives (the GEP'd pointers
                // are already-emitted IR and stay valid).
                if let Some(c) = &hoisted_idx_container {
                    self.unregister_synth_container(c);
                }
                (elem_type_name, recv_ptr, is_shared)
            }
            _ => return Ok(None),
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
        // B-2026-07-11-35: a GENERIC struct's field carries the bare type param
        // (`xs: Vec[T]`); resolve it to the container's concrete instantiation so
        // a `Heap[String]` `self.xs[i]` / `h.xs[i]` (and the field-receiver
        // method path — `self.xs.push(x)`) registers the synth element as
        // `String`, not the i64 unknown-name default (which read an 8-byte scalar
        // off a 24-byte {ptr,len,cap} → garbage). Sources the concrete arg from
        // the receiver's recorded instantiation (`h: H[String]`, or `self` in a
        // monomorph) with the active monomorph subst as fallback; a no-op for a
        // non-generic struct.
        let field_te = self.resolve_generic_field_te(inner, &type_name, &field_te);

        // GEP the field pointer. Shared: GEP at (idx + 1) past the
        // refcount slot using the heap_type. Plain: GEP directly into
        // the receiver-pointer at idx using the value struct_type.
        let (field_ptr, field_ll_ty) = if is_shared_handle {
            let info = match self.shared_types.get(&type_name).cloned() {
                Some(i) if !i.is_enum => i,
                _ => return Ok(None),
            };
            // Phase-D layout: headerless members GEP the twin at the
            // un-shifted user index (see `shared_gep_layout`).
            let (gep_ty, base) = self.shared_gep_layout(&type_name, info.heap_type);
            let fp = self
                .builder
                .build_struct_gep(
                    gep_ty,
                    receiver_ptr,
                    field_idx as u32 + base,
                    &format!("fr_sh_{}", field),
                )
                .unwrap();
            let fty = gep_ty
                .get_field_type_at_index(field_idx as u32 + base)
                .ok_or_else(|| {
                    format!(
                        "codegen: field-receiver {} on '{}.{}' — field LLVM type missing",
                        ctx, type_name, field
                    )
                })?;
            (fp, fty)
        } else if let Some(base_st) = self.struct_types.get(&type_name).copied() {
            // B-2026-07-15-17: for a GENERIC struct instantiation the field GEP
            // must use the per-monomorph struct type — the base `struct_types`
            // entry erases every generic-param field to i64 (1 word), so a
            // `Pair[Vec, Vec]` field-1 GEP would land at byte 8 (word 1 of the
            // FIRST field's `{ptr,len,cap}`) instead of after field 0's full
            // width. Read back through that misaligned pointer, `p.second.len()`
            // silently returned the first field's length (a wrong value; the
            // scalar-field print path uses the loaded mono VALUE and was
            // correct, which is why single-wide-field cases passed). Prefer the
            // mono type from the receiver's recorded instantiation; fall back to
            // the base type for a non-generic struct.
            let st = self
                .receiver_struct_inst(inner)
                .and_then(|te| match &te.kind {
                    TypeKind::Path(p) => {
                        let args = p.generic_args.as_ref()?;
                        self.mono_struct_type(p.segments.last()?, args)
                    }
                    _ => None,
                })
                .unwrap_or(base_st);
            let fp = self
                .builder
                .build_struct_gep(
                    st,
                    receiver_ptr,
                    field_idx as u32,
                    &format!("fr_pl_{}", field),
                )
                .unwrap();
            let fty = st
                .get_field_type_at_index(field_idx as u32)
                .ok_or_else(|| {
                    format!(
                        "codegen: field-receiver {} on '{}.{}' — field LLVM type missing",
                        ctx, type_name, field
                    )
                })?;
            (fp, fty)
        } else {
            return Ok(None);
        };

        Ok(Some((field_ptr, field_ll_ty, field_te)))
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
        // A borrowed (`ref T`) field method receiver (`p.source.len()` where
        // `source: ref String`): the field slot holds a borrow POINTER, not
        // an inline value, so the synth-field-pointer path below (built for
        // inline struct / Vec / Atomic fields) doesn't apply. Fall through to
        // the value-receiver path — `compile_field_access` deref's the borrow
        // to the `T` value, which the read-only `len`/`is_empty` extract
        // services. Non-`len`/`is_empty` methods on a borrowed field are a
        // follow-on (same scope as non-`len` methods on borrow-locals).
        // B-2026-06-07-5.
        if self.field_access_ref_inner(inner, field).is_some() {
            return Ok(None);
        }
        let Some((field_ptr, field_ll_ty, field_te)) =
            self.lower_field_access_ptr(inner, field, &format!("method '{method}'"))?
        else {
            return Ok(None);
        };
        self.compile_method_via_synth_elem_ptr(
            field_ptr,
            field_ll_ty,
            &field_te,
            method,
            args,
            call_span,
            &inner.span,
        )
        .map(Some)
    }

    /// `h.m.0.len()` — a method on a Map/Set tuple ELEMENT (`#26`,
    /// `B-2026-06-14-6`). Vec/String/scalar/struct tuple elements already
    /// dispatch via the value-extraction fall-through (`compile_tuple_index`
    /// → `build_extract_value`), but a `Map`/`Set` lowers to an opaque `ptr`
    /// handle whose runtime methods (`karac_map_*`) need a NAMED handle slot
    /// (`compile_map_method` → `get_data_ptr`), which only an identifier
    /// receiver provides — so a tuple-index Map receiver fell through to a
    /// generic path and read a garbage handle. The `FieldAccess` peer
    /// (`s.m.len()`) already works via [`Self::try_compile_field_receiver_method`];
    /// this is the `TupleIndex` sibling. GEP to the element handle slot
    /// (`field_chain_place_ptr`, which walks a tuple-index hop) and re-dispatch
    /// through a synth identifier. Returns `None` (fall through) for a non-Map/Set
    /// element or any shape that doesn't resolve, so the working Vec/scalar/struct
    /// paths are untouched.
    pub(super) fn try_compile_tuple_index_receiver_method(
        &mut self,
        recv: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let ExprKind::TupleIndex { object, index } = &recv.kind else {
            return Ok(None);
        };
        let idx = *index as usize;
        let Some(elem_tes) = self.place_chain_tuple_tes(object) else {
            return Ok(None);
        };
        let Some(elem_te) = elem_tes.get(idx).cloned() else {
            return Ok(None);
        };
        // Map/Set (opaque ptr-handle) AND String elements need the synth-
        // pointer dispatch; scalar/Vec/struct elements already work via value
        // extraction. String was assumed to work via value extraction, but its
        // place-taking methods (`.bytes()`, dispatched through the named-
        // receiver String-method path) fell through on a tuple element — the
        // shape a `#[derive(Message)]` map-field encode loop generates
        // (`e0.0.bytes()` over `self.<field>.entries()`), so a Map Message
        // failed codegen while the interpreter accepted it (B-2026-07-09-1).
        // Routing String tuple elements through the same synth-elem-ptr path as
        // Map/Set (a read-only alias of the tuple storage — no drop registered)
        // closes that gap.
        if self.extract_map_kv_types(&elem_te).is_none()
            && self.extract_set_elem_type(&elem_te).is_none()
            && !self.is_string_type_expr(&elem_te)
        {
            return Ok(None);
        }
        let Some(elem_ptr) = self.field_chain_place_ptr(recv) else {
            return Ok(None);
        };
        let Some(tuple_ty) = self.place_chain_aggregate_llvm_type(object) else {
            return Ok(None);
        };
        let Some(elem_ll_ty) = tuple_ty.get_field_type_at_index(idx as u32) else {
            return Ok(None);
        };
        self.compile_method_via_synth_elem_ptr(
            elem_ptr, elem_ll_ty, &elem_te, method, args, call_span, &recv.span,
        )
        .map(Some)
    }

    /// Shared core of the field-/tuple-index-receiver method dispatch: given a
    /// POINTER to an inline collection/struct element, its LLVM type, and its
    /// `TypeExpr`, mint a synthetic identifier bound to that pointer with the
    /// element's per-binding side tables populated, then re-dispatch the method
    /// through the regular identifier-keyed flow (so `compile_map_method` etc.
    /// resolve the handle via `get_data_ptr`). Cleans up the synth registrations
    /// before returning.
    #[allow(clippy::too_many_arguments)]
    fn compile_method_via_synth_elem_ptr(
        &mut self,
        elem_ptr: PointerValue<'ctx>,
        elem_ll_ty: BasicTypeEnum<'ctx>,
        elem_te: &TypeExpr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
        span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Mint a fresh synth identifier and populate its registries so
        // the recursive dispatch sees a regular Identifier-receiver flow.
        let synth = format!("__field_elem_{}", self.indexed_elem_counter);
        self.indexed_elem_counter += 1;
        self.variables.insert(
            synth.clone(),
            VarSlot {
                ptr: elem_ptr,
                ty: elem_ll_ty,
            },
        );
        self.register_var_from_type_expr(&synth, elem_te);
        // User-struct field: also populate `var_type_names` so the
        // impl-block dispatch path resolves `Type.method`.
        if let TypeKind::Path(path) = &elem_te.kind {
            if let Some(seg) = path.segments.first() {
                if self.struct_types.contains_key(seg.as_str())
                    || self.shared_types.contains_key(seg.as_str())
                {
                    self.record_var_type_name(synth.clone(), seg.clone());
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span, call_span);

        // Clean up synth registrations.
        self.variables.remove(&synth);
        self.vec_elem_types.remove(&synth);
        self.slice_elem_types.remove(&synth);
        self.var_elem_type_exprs.remove(&synth);
        self.var_type_names.remove(&synth);
        self.atomic_var_inner_is_bool.remove(&synth);
        self.map_key_types.remove(&synth);
        self.map_val_types.remove(&synth);
        self.map_key_type_names.remove(&synth);
        self.map_key_type_exprs.remove(&synth);
        self.set_elem_types.remove(&synth);
        self.set_elem_type_names.remove(&synth);
        self.set_elem_type_exprs.remove(&synth);

        result
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
                    self.record_var_type_name(synth.clone(), seg.clone());
                }
            }
        }

        let synth_expr = Expr {
            kind: ExprKind::Identifier(synth.clone()),
            span: inner_object.span.clone(),
        };
        let result = self.compile_method_call(&synth_expr, method, args, call_span, call_span);

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
        let idx_raw = self.compile_expr(index)?;
        let idx_val = self.coerce_to_i64(idx_raw)?;

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
        let idx_raw = self.compile_expr(index)?;
        let idx_val = self.coerce_to_i64(idx_raw)?;

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
        let idx_raw = self.compile_expr(index)?;
        let idx_val = self.coerce_to_i64(idx_raw)?;
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
        match &object.kind {
            ExprKind::Identifier(name) => self.var_type_names.get(name.as_str()).cloned(),
            // `self.method()` inside an impl body: the receiver parses as
            // `SelfValue`, not `Identifier("self")`. `make_impl_method_function`
            // prepends a regular `self` param whose declared type registers in
            // `var_type_names["self"]`, so the qualified `Type.method` lookup
            // resolves the same way an identifier receiver would.
            ExprKind::SelfValue => self.var_type_names.get("self").cloned(),
            _ => None,
        }
    }

    /// Slice OR (2026-05-16): Option/Result `unwrap` / `expect` / `is_some`
    /// / `is_none` / `is_ok` / `is_err` dispatch, receiver-shape-agnostic.
    ///
    /// Lowers `recv.unwrap()` (and friends) where `recv` is any expression
    /// of type `Option[T]` or `Result[T, E]`. The receiver is compiled to
    /// an SSA value (the
    /// `{ i64 tag, i64 w0, i64 w1, i64 w2 }` aggregate the prelude enum
    /// layouts produce) and we operate on the value directly — no synth
    /// identifier / no temporary alloca / no per-receiver-shape gymnastics.
    /// This is the cleanest path because the existing Index / FieldAccess
    /// synth arms mint a name tied to *receiver storage*, which doesn't
    /// exist for method-chain receivers (`m.get(k).unwrap()`).
    ///
    /// Returns `Ok(Some(value))` on a recognised Option/Result dispatch.
    /// Returns `Ok(None)` when the typechecker didn't record an inner type
    /// (the receiver wasn't Option/Result-shaped after all) — the caller
    /// falls through to the regular dispatch in `compile_method_call`,
    /// which will surface its own diagnostic if no arm applies.
    ///
    /// Tag semantics (mirroring `compile_question`):
    ///   Option: None=0, Some=1
    ///   Result: Err=0,  Ok=1
    /// Both share the same "tag != 0 ⇒ payload-bearing" shape, so a
    /// single value-extraction path covers both.
    /// `true` when `expr` syntactically produces a heap `String` — a string
    /// literal / f-string, or a String→String builtin method call
    /// (`trim`/case/`to_string`/`repeat`/`replace`), recursing through a block
    /// tail. Gates an un-annotated `.map()` mapper whose heap return the
    /// typechecker can't infer without a param annotation (B-2026-07-12-10).
    fn closure_body_produces_heap_string(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::InterpolatedStringLit(_) => true,
            ExprKind::MethodCall { method, .. } => matches!(
                method.as_str(),
                "trim"
                    | "trim_start"
                    | "trim_end"
                    | "to_lowercase"
                    | "to_uppercase"
                    | "to_string"
                    | "repeat"
                    | "replace"
                    | "replacen"
            ),
            ExprKind::Block(b) | ExprKind::Seq(b) => b
                .final_expr
                .as_ref()
                .is_some_and(|e| Self::closure_body_produces_heap_string(e)),
            _ => false,
        }
    }

    /// Seed `pattern_binding_types` + `pattern_binding_inner_types` for a
    /// codegen-SYNTHESIZED pattern binding (which the typechecker never saw),
    /// mirroring what those tables hold for the same binding in a hand-written
    /// match. `pattern_binding_types` gets the payload's canonical head name
    /// (`Vec` / `String` / struct name / `Tuple`); `pattern_binding_inner_types`
    /// gets the element type for a `Vec`/`Slice` payload, else the whole
    /// payload TypeExpr. `type_to_type_expr` lowers `Type::Str` to the head
    /// `"str"`, but the pattern tables canonicalize String to `"String"`, so
    /// normalize that. Used by the `.map()`-over-heap match synthesis.
    fn seed_synthetic_pattern_binding_type(&mut self, span: &crate::token::Span, te: &TypeExpr) {
        let key = (span.offset, span.length);
        match &te.kind {
            TypeKind::Path(p) => {
                if let Some(head) = p.segments.last() {
                    let canonical = if head == "str" {
                        "String"
                    } else {
                        head.as_str()
                    };
                    self.pattern_binding_types
                        .insert(key, canonical.to_string());
                }
            }
            TypeKind::Tuple(_) => {
                self.pattern_binding_types.insert(key, "Tuple".to_string());
            }
            _ => {}
        }
        let inner = super::helpers::vec_inner_type_expr(te)
            .or_else(|| super::helpers::slice_inner_type_expr(te));
        match inner {
            Some(el) => {
                self.pattern_binding_inner_types.insert(key, el);
            }
            None => {
                self.pattern_binding_inner_types.insert(key, te.clone());
            }
        }
    }

    /// Lower `opt.map(f)` / `res.map(f)` over a HEAP payload by synthesizing the
    /// equivalent `match` and compiling it, so the match codegen's heap-payload
    /// ownership machinery (move-out, arm-body drop / move suppression,
    /// receiver-payload suppression) handles the buffer. The synthesized
    /// pattern-binding types are seeded from `inner_te` / `err_te` (keyed on the
    /// MAPPER's span — unique per closure — to avoid colliding with the outer
    /// call in a chain); the mapper closure keeps its real span so its
    /// typechecker-recorded `Fn(T) -> R` drives inference. B-2026-07-12-11.
    fn compile_map_via_match_synthesis(
        &mut self,
        object: &Expr,
        mapper: &Expr,
        inner_te: &TypeExpr,
        err_te: Option<&TypeExpr>,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let is_result = self.type_name_of_expr(object).as_deref() == Some("Result");
        let x_name = "__karac_map_x";
        let e_name = "__karac_map_e";
        // Key the synthesized bindings on the MAPPER's span (unique per
        // closure), not `call_span`: the parser gives a method call the same
        // span as its receiver, so a chained `opt.map(f).unwrap_or(d)` would
        // collide on `call_span`. Ok/Err bindings need distinct keys, so nudge
        // the length.
        let mut x_span = mapper.span.clone();
        x_span.length += 2;
        let mut e_span = mapper.span.clone();
        e_span.length += 3;
        self.seed_synthetic_pattern_binding_type(&x_span, inner_te);
        if let Some(ete) = err_te {
            self.seed_synthetic_pattern_binding_type(&e_span, ete);
        }
        let mk = |kind: ExprKind| Expr {
            kind,
            span: call_span.clone(),
        };
        let arg = |value: Expr| CallArg {
            label: None,
            mut_marker: false,
            span: value.span.clone(),
            value,
        };
        let bind_pat_spanned = |name: &str, span: crate::token::Span| Pattern {
            kind: PatternKind::Binding(name.to_string()),
            span,
        };
        let bind_pat = |name: &str| Pattern {
            kind: PatternKind::Binding(name.to_string()),
            span: call_span.clone(),
        };
        let call_f = mk(ExprKind::Call {
            callee: Box::new(mapper.clone()),
            args: vec![arg(mk(ExprKind::Identifier(x_name.to_string())))],
        });
        let present_ctor = if is_result { "Ok" } else { "Some" };
        let present_body = mk(ExprKind::Call {
            callee: Box::new(mk(ExprKind::Identifier(present_ctor.to_string()))),
            args: vec![arg(call_f)],
        });
        let present_arm = MatchArm {
            pattern: Pattern {
                kind: PatternKind::TupleVariant {
                    path: vec![present_ctor.to_string()],
                    patterns: vec![bind_pat_spanned(x_name, x_span.clone())],
                },
                span: call_span.clone(),
            },
            guard: None,
            body: present_body,
            span: call_span.clone(),
        };
        let absent_arm = if is_result {
            let err_body = mk(ExprKind::Call {
                callee: Box::new(mk(ExprKind::Identifier("Err".to_string()))),
                args: vec![arg(mk(ExprKind::Identifier(e_name.to_string())))],
            });
            MatchArm {
                pattern: Pattern {
                    kind: PatternKind::TupleVariant {
                        path: vec!["Err".to_string()],
                        patterns: vec![bind_pat_spanned(e_name, e_span.clone())],
                    },
                    span: call_span.clone(),
                },
                guard: None,
                body: err_body,
                span: call_span.clone(),
            }
        } else {
            MatchArm {
                pattern: bind_pat("None"),
                guard: None,
                body: mk(ExprKind::Identifier("None".to_string())),
                span: call_span.clone(),
            }
        };
        let match_expr = mk(ExprKind::Match {
            scrutinee: Box::new(object.clone()),
            arms: vec![present_arm, absent_arm],
        });
        self.compile_expr(&match_expr)
    }

    pub(super) fn try_compile_option_result_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
        args_close_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        // Pull the inner `T` from the typechecker-populated side-table.
        // Without it we don't know how to shape the payload reconstruction
        // for unwrap/expect; fall through so the caller's diagnostic
        // (which already names `compile_method_call` as the fix point)
        // surfaces. `is_*` could technically work without the inner type,
        // but routing them all through the same gate keeps the contract
        // uniform — the typechecker writes the entry for every variant
        // we care about.
        // Key on the closing-paren span (falling back to the receiver span for
        // synthetic callers) so a CHAINED `opt.map(f).unwrap_or(d)` — where map
        // and unwrap_or share `call_span` — doesn't read the outer call's
        // `method_unwrap_*` entry. Matches the typechecker's
        // `SpanKey::for_method_call` insert. Span-collision fix, Slice 1.
        let key = crate::token::method_call_key(call_span, args_close_span);
        let inner_te = match self.method_unwrap_inner_types.get(&key).cloned() {
            Some(te) => te,
            None => return Ok(None),
        };
        // Normalize a `ref T` payload to `T`. Borrow-returning producers
        // (`OnceLock[T].get() -> Option[ref T]`, and the `Vec.get`/`Map.get`
        // family) physically PACK THE POINTEE VALUE into the Option payload
        // words — `once.rs`'s `compile_once_get` loads `T` through the borrow
        // before splitting, so the payload is identical to a plain
        // `Option[T]`. But the typechecker is inconsistent about the recorded
        // inner type: `Vec.get` records `T`, `OnceLock.get` records `ref T`.
        // Left as `ref T`, `rebuild_value_from_payload_words` lowers it to a
        // pointer and re-interprets the value word via `inttoptr` — the
        // `unwrap_or` merge phi then mixes `ptr` (present) with a value
        // default (`unwrap_or(0)`), which the LLVM verifier rejects outright,
        // and `unwrap()` silently builds a bogus pointer that faults when
        // dereferenced. Stripping the outer borrow here makes every
        // Option/Result method reconstruct the value directly, matching both
        // the physical payload and the interpreter's auto-deref (and it is a
        // no-op for the already-`T` `Vec.get` path). A wide `T` is still
        // deboxed by the `word_count > area` branch, exactly as before.
        let inner_te = match &inner_te.kind {
            crate::ast::TypeKind::Ref(inner) | crate::ast::TypeKind::MutRef(inner) => {
                (**inner).clone()
            }
            _ => inner_te,
        };

        let i64_t = self.context.i64_type();

        // Compile the receiver. The Option/Result enum lowering produces a
        // 4-word struct `{ i64 tag, i64 w0, i64 w1, i64 w2 }` regardless of
        // the inner `T`'s natural word count (Slice 1c.2 widen, see
        // `seed_builtin_enum_layouts`).
        let recv_val = self.compile_expr(object)?;
        let recv_struct = match recv_val {
            BasicValueEnum::StructValue(sv) => sv,
            _ => {
                return Err(format!(
                    "codegen: Option/Result method '{}' expected struct receiver, got {:?}",
                    method, recv_val
                ));
            }
        };

        let tag = self
            .builder
            .build_extract_value(recv_struct, 0, "or.tag")
            .map_err(|e| format!("codegen: extract Option/Result tag: {:?}", e))?
            .into_int_value();

        // is_*: pure boolean reductions on the tag. No payload extraction.
        match method {
            "is_some" | "is_ok" => {
                let one = i64_t.const_int(1, false);
                let b = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, tag, one, "or.is_present")
                    .unwrap();
                return Ok(Some(b.into()));
            }
            "is_none" | "is_err" => {
                let zero = i64_t.const_int(0, false);
                let b = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, tag, zero, "or.is_absent")
                    .unwrap();
                return Ok(Some(b.into()));
            }
            _ => {}
        }

        // `Option[T].map(f)` / `Result[T, E].map(f)`: apply `f` to the present
        // payload and re-wrap in the present variant (`Some`/`Ok`); an absent
        // receiver (`None`/`Err`) passes through unchanged. The typechecker
        // recorded the SOURCE `T` in `inner_te`; the RESULT `R` is read off the
        // mapper's compiled value (the Some/Ok ctor infers its payload words
        // from that value). We reconstruct the payload, bind it to a synthetic
        // local, then compile `Some(f(__x))` / `Ok(f(__x))` so the existing
        // call + enum-ctor codegen (closure/fn-ref dispatch, payload packing)
        // does the work rather than re-implementing it. Scoped to a trivially-
        // copyable payload: a heap `T` needs ownership care (the reconstructed
        // payload aliases the receiver's buffer and the mapper may consume it),
        // so it defers loudly to the interpreter. B-2026-07-12-11.
        if method == "map" && args.len() == 1 {
            if !super::vec_method::is_trivially_copyable_te(&inner_te) {
                // Heap payload (String / Vec / heap struct): the hand-rolled
                // trivially-copyable path below only shallow-copies the payload
                // (unsound for a mapper that MOVES or RETURNS heap). Delegate to
                // the match codegen — `opt.map(f)` is exactly `match opt {
                // Some(x) => Some(f(x)), None => None }` (Ok/Err for Result) —
                // which owns the correct heap-payload move-out / drop /
                // receiver-suppression machinery. The typechecker records the
                // mapper's solved `Fn(T) -> R`, so codegen types the closure.
                // Chained `map(f).unwrap_or(d)` now works because the span-
                // collision fix (Slice 1) keys `method_unwrap_*` on
                // `args_close_span`. B-2026-07-12-11 heap half.
                //
                // Residual gate: an UN-ANNOTATED closure with a heap-String-
                // producing body (`|s| s.to_uppercase()`). The typechecker
                // can't infer its return from the payload `T` without a param
                // annotation (B-2026-07-12-10), and codegen can't recover the
                // surface type from the shared String/Vec LLVM type — so it
                // would silently miscompile. Detect it syntactically and gate
                // cleanly (annotate the param, or use --interp). Scalar / move /
                // annotated / named-fn mappers resolve concretely and proceed.
                if let ExprKind::Closure { params, body, .. } = &args[0].value.kind {
                    let all_unannotated = params.iter().all(|p| p.ty.is_none());
                    if all_unannotated && Self::closure_body_produces_heap_string(body) {
                        return Err(
                            "codegen: Option/Result.map with an un-annotated closure that \
                             returns a String/Vec is not yet supported under `karac build`; \
                             annotate the closure parameter (`|x: T| ...`) or run with \
                             `--interp` (or `KARAC_RUN_JIT=0`)"
                                .to_string(),
                        );
                    }
                }
                let err_te = self.method_unwrap_err_types.get(&key).cloned();
                return self
                    .compile_map_via_match_synthesis(
                        object,
                        &args[0].value,
                        &inner_te,
                        err_te.as_ref(),
                        call_span,
                    )
                    .map(Some);
            }
            let is_result = self.type_name_of_expr(object).as_deref() == Some("Result");
            let present_variant = if is_result { "Ok" } else { "Some" };
            let inner_ll = self.llvm_type_for_type_expr(&inner_te);
            let fn_val = self.current_fn.unwrap();
            let present_bb = self.context.append_basic_block(fn_val, "map.present");
            let absent_bb = self.context.append_basic_block(fn_val, "map.absent");
            let merge_bb = self.context.append_basic_block(fn_val, "map.merge");
            let one = i64_t.const_int(1, false);
            let is_present = self
                .builder
                .build_int_compare(IntPredicate::EQ, tag, one, "map.is_present")
                .unwrap();
            self.builder
                .build_conditional_branch(is_present, present_bb, absent_bb)
                .unwrap();

            // Present: reconstruct the payload, bind it to a synthetic local,
            // and compile `Some(f(__x))` / `Ok(f(__x))`.
            self.builder.position_at_end(present_bb);
            let w0 = self
                .builder
                .build_extract_value(recv_struct, 1, "map.w0")
                .map_err(|e| format!("codegen: map payload w0: {:?}", e))?
                .into_int_value();
            let w1 = self
                .builder
                .build_extract_value(recv_struct, 2, "map.w1")
                .map_err(|e| format!("codegen: map payload w1: {:?}", e))?
                .into_int_value();
            let w2 = self
                .builder
                .build_extract_value(recv_struct, 3, "map.w2")
                .map_err(|e| format!("codegen: map payload w2: {:?}", e))?
                .into_int_value();
            let payload = self.rebuild_value_from_payload_words(inner_ll, w0, w1, w2)?;
            let x_name = "__karac_map_x";
            let alloca = self.create_entry_alloca(fn_val, x_name, payload.get_type());
            // `create_entry_alloca` moves the builder to the entry block; re-anchor.
            self.builder.position_at_end(present_bb);
            self.builder.build_store(alloca, payload).unwrap();
            let saved = self.variables.insert(
                x_name.to_string(),
                VarSlot {
                    ptr: alloca,
                    ty: payload.get_type(),
                },
            );
            let mk = |kind: ExprKind| Expr {
                kind,
                span: call_span.clone(),
            };
            let arg = |value: Expr| CallArg {
                label: None,
                mut_marker: false,
                span: value.span.clone(),
                value,
            };
            let x_ident = mk(ExprKind::Identifier(x_name.to_string()));
            let call_f = mk(ExprKind::Call {
                callee: Box::new(args[0].value.clone()),
                args: vec![arg(x_ident)],
            });
            let ctor = mk(ExprKind::Call {
                callee: Box::new(mk(ExprKind::Identifier(present_variant.to_string()))),
                args: vec![arg(call_f)],
            });
            let present_result = self.compile_expr(&ctor)?;
            match saved {
                Some(s) => {
                    self.variables.insert(x_name.to_string(), s);
                }
                None => {
                    self.variables.remove(x_name);
                }
            }
            let present_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Absent: the receiver (`None`/`Err`) is already the correct
            // type-erased shape (`Option[R]` / `Result[R, E]` share the layout).
            self.builder.position_at_end(absent_bb);
            let absent_result: BasicValueEnum<'ctx> = recv_struct.into();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(present_result.get_type(), "map.result")
                .map_err(|e| format!("codegen: map phi: {:?}", e))?;
            phi.add_incoming(&[(&present_result, present_end), (&absent_result, absent_bb)]);
            return Ok(Some(phi.as_basic_value()));
        }

        // ── Option/Result combinators, CLOSURE batch (B-2026-07-14-6) ──
        // `map_or`/`and_then`/`filter` use ONLY the present payload `T` (which
        // codegen has via `method_unwrap_inner_types`) and are lowered below,
        // following `Option/Result.map`'s reconstruct-payload → invoke-closure →
        // branch/phi shape. `map_err` maps over `E` (recorded in the
        // present-payload slot for it).
        //
        // `unwrap_or_else`/`map_or_else`/`or_else`: the OPTION forms take a
        // NO-ARG absent-branch closure (`|| …`); the RESULT forms pass the
        // `Err` value `e` to it, reconstructed at the `E` recorded in the
        // sibling `method_unwrap_err_types` table.
        if matches!(method, "unwrap_or_else" | "map_or_else" | "or_else") {
            let is_result = self.type_name_of_expr(object).as_deref() == Some("Result");
            // For a Result receiver the absent closure takes `e: E` — pull `E`
            // from the sibling table (recorded by the typechecker for exactly
            // these three methods on a Result).
            let err_te = if is_result {
                match self.method_unwrap_err_types.get(&key).cloned() {
                    Some(te) => Some(te),
                    None => {
                        return Err(format!(
                            "codegen: Result.{method} is missing its recorded Err type; \
                             re-run with `--interp` (or `KARAC_RUN_JIT=0`)"
                        ));
                    }
                }
            } else {
                None
            };
            if !super::vec_method::is_trivially_copyable_te(&inner_te)
                || err_te
                    .as_ref()
                    .is_some_and(|te| !super::vec_method::is_trivially_copyable_te(te))
            {
                return Err(format!(
                    "codegen: Option/Result.{method} over a non-trivially-copyable \
                     payload (String / Vec / struct) is not yet supported under \
                     `karac build`; re-run with `--interp` (or `KARAC_RUN_JIT=0`)"
                ));
            }
            let inner_ll = self.llvm_type_for_type_expr(&inner_te);
            let fn_val = self.current_fn.unwrap();
            let one = i64_t.const_int(1, false);
            let some_bb = self.context.append_basic_block(fn_val, "optelse.some");
            let none_bb = self.context.append_basic_block(fn_val, "optelse.none");
            let merge_bb = self.context.append_basic_block(fn_val, "optelse.merge");
            let is_some = self
                .builder
                .build_int_compare(IntPredicate::EQ, tag, one, "optelse.some?")
                .unwrap();
            self.builder
                .build_conditional_branch(is_some, some_bb, none_bb)
                .unwrap();

            // Present (`Some(x)`/`Ok(x)`): `unwrap_or_else` → x; `map_or_else`
            // → f(x); `or_else` → the receiver itself.
            self.builder.position_at_end(some_bb);
            let some_result: BasicValueEnum<'ctx> = match method {
                "or_else" => recv_struct.into(),
                _ => {
                    let w0 = self
                        .builder
                        .build_extract_value(recv_struct, 1, "optelse.w0")
                        .map_err(|e| format!("codegen: optelse w0: {:?}", e))?
                        .into_int_value();
                    let w1 = self
                        .builder
                        .build_extract_value(recv_struct, 2, "optelse.w1")
                        .map_err(|e| format!("codegen: optelse w1: {:?}", e))?
                        .into_int_value();
                    let w2 = self
                        .builder
                        .build_extract_value(recv_struct, 3, "optelse.w2")
                        .map_err(|e| format!("codegen: optelse w2: {:?}", e))?
                        .into_int_value();
                    let payload = self.rebuild_value_from_payload_words(inner_ll, w0, w1, w2)?;
                    if method == "map_or_else" {
                        // `map_or_else(default_fn, f)` — the mapper is arg 1.
                        let mapper = &args
                            .get(1)
                            .ok_or_else(|| {
                                "codegen: Option.map_or_else missing mapper".to_string()
                            })?
                            .value
                            .clone();
                        self.compile_optres_closure_on(mapper, payload, call_span)?
                    } else {
                        // `unwrap_or_else` — the present payload is the result.
                        payload
                    }
                }
            };
            let some_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Absent (`None`/`Err(e)`): invoke the absent closure (arg 0) —
            // no-arg for Option, `f(e)` for Result (reconstruct `e` at `E`).
            self.builder.position_at_end(none_bb);
            let none_closure = &args
                .first()
                .ok_or_else(|| format!("codegen: Option/Result.{method} missing closure arg"))?
                .value
                .clone();
            let none_result = if let Some(err_te) = &err_te {
                let err_ll = self.llvm_type_for_type_expr(err_te);
                let ew0 = self
                    .builder
                    .build_extract_value(recv_struct, 1, "optelse.ew0")
                    .map_err(|e| format!("codegen: optelse ew0: {:?}", e))?
                    .into_int_value();
                let ew1 = self
                    .builder
                    .build_extract_value(recv_struct, 2, "optelse.ew1")
                    .map_err(|e| format!("codegen: optelse ew1: {:?}", e))?
                    .into_int_value();
                let ew2 = self
                    .builder
                    .build_extract_value(recv_struct, 3, "optelse.ew2")
                    .map_err(|e| format!("codegen: optelse ew2: {:?}", e))?
                    .into_int_value();
                let e_val = self.rebuild_value_from_payload_words(err_ll, ew0, ew1, ew2)?;
                self.compile_optres_closure_on(none_closure, e_val, call_span)?
            } else {
                self.compile_optres_closure_noarg(none_closure, call_span)?
            };
            let none_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(some_result.get_type(), "optelse.result")
                .map_err(|e| format!("codegen: optelse phi: {:?}", e))?;
            phi.add_incoming(&[(&some_result, some_end), (&none_result, none_end)]);
            return Ok(Some(phi.as_basic_value()));
        }
        // `Result[T,E].map_err(f)` — `Ok(x)` passes through; `Err(e)` →
        // `Err(f(e))`. The closure fires on the ABSENT (Err) branch and its
        // result is re-packed as `Err`. `inner_te` is `E` here (the typechecker
        // records the Err payload for `map_err`, not `T`). Scalar `E`.
        if method == "map_err" {
            if !super::vec_method::is_trivially_copyable_te(&inner_te) {
                return Err(format!(
                    "codegen: Option/Result.{method} over a non-trivially-copyable \
                     payload (String / Vec / struct) is not yet supported under \
                     `karac build`; re-run with `--interp` (or `KARAC_RUN_JIT=0`)"
                ));
            }
            let inner_ll = self.llvm_type_for_type_expr(&inner_te);
            let fn_val = self.current_fn.unwrap();
            let one = i64_t.const_int(1, false);
            let ok_bb = self.context.append_basic_block(fn_val, "maperr.ok");
            let err_bb = self.context.append_basic_block(fn_val, "maperr.err");
            let merge_bb = self.context.append_basic_block(fn_val, "maperr.merge");
            let is_ok = self
                .builder
                .build_int_compare(IntPredicate::EQ, tag, one, "maperr.ok?")
                .unwrap();
            self.builder
                .build_conditional_branch(is_ok, ok_bb, err_bb)
                .unwrap();

            // Ok(x): pass the receiver through unchanged.
            self.builder.position_at_end(ok_bb);
            let ok_result: BasicValueEnum<'ctx> = recv_struct.into();
            let ok_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Err(e): reconstruct e, apply f, re-pack as Err(f(e)).
            self.builder.position_at_end(err_bb);
            let w0 = self
                .builder
                .build_extract_value(recv_struct, 1, "maperr.w0")
                .map_err(|e| format!("codegen: map_err w0: {:?}", e))?
                .into_int_value();
            let w1 = self
                .builder
                .build_extract_value(recv_struct, 2, "maperr.w1")
                .map_err(|e| format!("codegen: map_err w1: {:?}", e))?
                .into_int_value();
            let w2 = self
                .builder
                .build_extract_value(recv_struct, 3, "maperr.w2")
                .map_err(|e| format!("codegen: map_err w2: {:?}", e))?
                .into_int_value();
            let e_val = self.rebuild_value_from_payload_words(inner_ll, w0, w1, w2)?;
            let closure = &args
                .first()
                .ok_or_else(|| "codegen: Result.map_err missing closure arg".to_string())?
                .value
                .clone();
            let mapped = self.compile_optres_closure_on(closure, e_val, call_span)?;
            let words = self.coerce_to_payload_words(mapped, 3)?;
            let mut err_agg = recv_struct.get_type().get_undef();
            err_agg = self
                .builder
                .build_insert_value(err_agg, i64_t.const_zero(), 0, "maperr.tag")
                .unwrap()
                .into_struct_value();
            for (i, w) in words.iter().enumerate() {
                err_agg = self
                    .builder
                    .build_insert_value(err_agg, *w, (i + 1) as u32, "maperr.w")
                    .unwrap()
                    .into_struct_value();
            }
            let err_result: BasicValueEnum<'ctx> = err_agg.into();
            let err_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(ok_result.get_type(), "maperr.result")
                .map_err(|e| format!("codegen: map_err phi: {:?}", e))?;
            phi.add_incoming(&[(&ok_result, ok_end), (&err_result, err_end)]);
            return Ok(Some(phi.as_basic_value()));
        }
        if matches!(method, "map_or" | "and_then" | "filter") {
            if !super::vec_method::is_trivially_copyable_te(&inner_te) {
                return Err(format!(
                    "codegen: Option/Result.{method} over a non-trivially-copyable \
                     payload (String / Vec / struct) is not yet supported under \
                     `karac build`; re-run with `--interp` (or `KARAC_RUN_JIT=0`)"
                ));
            }
            let inner_ll = self.llvm_type_for_type_expr(&inner_te);
            let fn_val = self.current_fn.unwrap();
            let one = i64_t.const_int(1, false);
            let present_bb = self.context.append_basic_block(fn_val, "combi.present");
            let absent_bb = self.context.append_basic_block(fn_val, "combi.absent");
            let merge_bb = self.context.append_basic_block(fn_val, "combi.merge");
            let is_present = self
                .builder
                .build_int_compare(IntPredicate::EQ, tag, one, "combi.present?")
                .unwrap();
            self.builder
                .build_conditional_branch(is_present, present_bb, absent_bb)
                .unwrap();

            // Present branch: reconstruct the scalar payload and invoke the
            // closure on it.
            self.builder.position_at_end(present_bb);
            let w0 = self
                .builder
                .build_extract_value(recv_struct, 1, "combi.w0")
                .map_err(|e| format!("codegen: combinator w0: {:?}", e))?
                .into_int_value();
            let w1 = self
                .builder
                .build_extract_value(recv_struct, 2, "combi.w1")
                .map_err(|e| format!("codegen: combinator w1: {:?}", e))?
                .into_int_value();
            let w2 = self
                .builder
                .build_extract_value(recv_struct, 3, "combi.w2")
                .map_err(|e| format!("codegen: combinator w2: {:?}", e))?
                .into_int_value();
            let payload = self.rebuild_value_from_payload_words(inner_ll, w0, w1, w2)?;
            // The closure arg: `map_or(default, f)` has it at index 1, the
            // others at index 0.
            let closure_idx = if method == "map_or" { 1 } else { 0 };
            let closure = &args
                .get(closure_idx)
                .ok_or_else(|| format!("codegen: Option/Result.{method} missing closure arg"))?
                .value
                .clone();
            let f_result = self.compile_optres_closure_on(closure, payload, call_span)?;
            let present_result: BasicValueEnum<'ctx> = match method {
                // `f(payload)` is the whole result (a scalar `U` for `map_or`,
                // an Option/Result struct for `and_then`).
                "map_or" | "and_then" => f_result,
                // `filter`: keep `Some(x)` when the predicate holds, else `None`.
                "filter" => {
                    let keep = f_result.into_int_value();
                    let mut none_agg = recv_struct.get_type().get_undef();
                    none_agg = self
                        .builder
                        .build_insert_value(none_agg, i64_t.const_zero(), 0, "filt.none.tag")
                        .unwrap()
                        .into_struct_value();
                    let some_bv: BasicValueEnum<'ctx> = recv_struct.into();
                    let none_bv: BasicValueEnum<'ctx> = none_agg.into();
                    self.builder
                        .build_select(keep, some_bv, none_bv, "filt.sel")
                        .unwrap()
                }
                _ => unreachable!(),
            };
            let present_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Absent branch: `map_or` yields the eager default; `and_then` /
            // `filter` pass the absent receiver (`None`/`Err`) through.
            self.builder.position_at_end(absent_bb);
            let absent_result: BasicValueEnum<'ctx> = if method == "map_or" {
                let default_arg = args.first().ok_or_else(|| {
                    "codegen: Option/Result.map_or missing default arg".to_string()
                })?;
                self.compile_expr(&default_arg.value)?
            } else {
                recv_struct.into()
            };
            let absent_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(present_result.get_type(), "combi.result")
                .map_err(|e| format!("codegen: combinator phi: {:?}", e))?;
            phi.add_incoming(&[(&present_result, present_end), (&absent_result, absent_end)]);
            return Ok(Some(phi.as_basic_value()));
        }

        // ── Option/Result combinators, non-closure batch (B-2026-07-14-6) ──
        // Option and Result share the type-erased 4-word `{tag, w0, w1, w2}`
        // layout with tag 1 = present (`Some`/`Ok`) and tag 0 = absent
        // (`None`/`Err`), so these are tag manipulations / selects on the shared
        // struct. Gated to a trivially-copyable (scalar) payload — like
        // `Option/Result.map` above — so no heap payload's ownership (which the
        // consuming combinator would need to thread) can be mishandled under
        // `karac build`; the interpreter covers heap payloads and is the oracle.
        if matches!(
            method,
            "ok" | "err" | "or" | "and" | "ok_or" | "flatten" | "take" | "get_or_insert"
        ) {
            // Heap-safety gate (see the block comment). For every method except
            // `flatten`, `inner_te` IS the scalar payload to check. For
            // `flatten`, `inner_te` is the inner `Option[T]`, so check its `T`.
            let payload_te: &TypeExpr = if method == "flatten" {
                match &inner_te.kind {
                    TypeKind::Path(p)
                        if p.segments.last().map(|s| s.as_str()) == Some("Option") =>
                    {
                        match p.generic_args.as_ref().and_then(|a| a.first()) {
                            Some(GenericArg::Type(t)) => t,
                            _ => &inner_te,
                        }
                    }
                    _ => &inner_te,
                }
            } else {
                &inner_te
            };
            if !super::vec_method::is_trivially_copyable_te(payload_te) {
                return Err(format!(
                    "codegen: Option/Result.{method} over a non-trivially-copyable \
                     payload (String / Vec / struct) is not yet supported under \
                     `karac build`; re-run with `--interp` (or `KARAC_RUN_JIT=0`)"
                ));
            }
            let one = i64_t.const_int(1, false);
            let zero = i64_t.const_int(0, false);
            let is_present = self
                .builder
                .build_int_compare(IntPredicate::EQ, tag, one, "combi.present")
                .unwrap();
            let struct_ty = recv_struct.get_type();
            match method {
                "ok" => {
                    // `Ok(x)`[tag1,payload] → `Some(x)`[tag1,payload]; `Err(_)`
                    // [tag0] → `None`[tag0]. Identical layout + tag semantics, so
                    // the receiver struct already IS the Option value.
                    return Ok(Some(recv_struct.into()));
                }
                "err" => {
                    // `Err(e)`[tag0] → `Some(e)`[tag1]; `Ok(_)`[tag1] →
                    // `None`[tag0]. Flip the tag (1 - tag); keep the payload
                    // words (Err's payload becomes Some's; Ok's is dead under
                    // tag0).
                    let flipped = self.builder.build_int_sub(one, tag, "err.tag").unwrap();
                    let agg = self
                        .builder
                        .build_insert_value(recv_struct, flipped, 0, "err.set")
                        .unwrap()
                        .into_struct_value();
                    return Ok(Some(agg.into()));
                }
                "or" | "and" => {
                    // Eager arg (matches the interpreter + Rust's `or`/`and`).
                    // present(tag1): `or`→receiver, `and`→arg.
                    // absent(tag0):  `or`→arg,      `and`→receiver.
                    let arg = args.first().ok_or_else(|| {
                        format!("codegen: Option/Result.{method} expects 1 argument, found 0")
                    })?;
                    let arg_val = self.compile_expr(&arg.value)?;
                    let recv_bv: BasicValueEnum<'ctx> = recv_struct.into();
                    let (present_val, absent_val) = if method == "or" {
                        (recv_bv, arg_val)
                    } else {
                        (arg_val, recv_bv)
                    };
                    let sel = self
                        .builder
                        .build_select(is_present, present_val, absent_val, "combi.sel")
                        .unwrap();
                    return Ok(Some(sel));
                }
                "ok_or" => {
                    // `Some(x)` → `Ok(x)` (tag 1, x payload); `None` → `Err(e)`
                    // (tag 0, packed e). B-2026-07-15-15: the result value MUST
                    // be built in the RESULT layout `{tag, w0..w4}` (6 fields),
                    // NOT the Option receiver's `{tag, w0..w2}` (4 fields). A
                    // downstream `match r { Ok(v)/Err(e) }` reads Result's
                    // field_word_offsets — `(0, 5)` for both arms — and extracts
                    // 5 payload words; against a 4-field Option-shaped value that
                    // `build_extract_value(sv, 5)` is ExtractOutOfRange → a
                    // compiler ICE (pattern_binding.rs). Pre-fix `ok_or` handed
                    // back the Option struct verbatim, so it crashed the backend
                    // for EVERY payload type (i64 and String alike). Copy the
                    // receiver's Some payload words into the Result's Ok slots and
                    // pack `e` into the Err slots.
                    let err_arg = args.first().ok_or_else(|| {
                        "codegen: Option.ok_or expects 1 argument, found 0".to_string()
                    })?;
                    let e_val = self.compile_expr(&err_arg.value)?;
                    let result_ty = self
                        .enum_layouts
                        .get("Result")
                        .map(|l| l.llvm_type)
                        .ok_or_else(|| {
                            "codegen: Result layout unavailable for Option.ok_or".to_string()
                        })?;
                    let result_payload_words =
                        (result_ty.count_fields() as usize).saturating_sub(1);
                    // Err(e): tag 0, `e` packed into the payload area.
                    let err_words = self.coerce_to_payload_words(e_val, result_payload_words)?;
                    let mut err_agg = result_ty.get_undef();
                    err_agg = self
                        .builder
                        .build_insert_value(err_agg, zero, 0, "okor.errtag")
                        .unwrap()
                        .into_struct_value();
                    for (i, w) in err_words.iter().enumerate() {
                        err_agg = self
                            .builder
                            .build_insert_value(err_agg, *w, (i + 1) as u32, "okor.errw")
                            .unwrap()
                            .into_struct_value();
                    }
                    // Ok(x): tag 1, copy the receiver's Some payload words (Option
                    // words 1..=option_payload) into the Result's Ok words. The Ok
                    // binding reads only its natural width downstream, so the
                    // trailing Result slots may stay undef.
                    let option_payload = (struct_ty.count_fields() as usize).saturating_sub(1);
                    let copy_words = option_payload.min(result_payload_words);
                    let mut ok_agg = result_ty.get_undef();
                    ok_agg = self
                        .builder
                        .build_insert_value(ok_agg, one, 0, "okor.oktag")
                        .unwrap()
                        .into_struct_value();
                    for w in 1..=copy_words {
                        let pw = self
                            .builder
                            .build_extract_value(recv_struct, w as u32, "okor.okw")
                            .unwrap();
                        ok_agg = self
                            .builder
                            .build_insert_value(ok_agg, pw, w as u32, "okor.okw.set")
                            .unwrap()
                            .into_struct_value();
                    }
                    let ok_bv: BasicValueEnum<'ctx> = ok_agg.into();
                    let err_bv: BasicValueEnum<'ctx> = err_agg.into();
                    let sel = self
                        .builder
                        .build_select(is_present, ok_bv, err_bv, "okor.sel")
                        .unwrap();
                    return Ok(Some(sel));
                }
                "flatten" => {
                    // `Some(inner)` → the inner `Option` value; `None` → `None`
                    // (receiver). The outer payload boxes the 4-word inner
                    // Option (4 > 3 payload words), so w0 is the box pointer —
                    // unbox and load. `inner_te` is the inner `Option[T]`.
                    let w0 = self
                        .builder
                        .build_extract_value(recv_struct, 1, "flat.w0")
                        .map_err(|e| format!("codegen: flatten w0: {:?}", e))?
                        .into_int_value();
                    // The inner `Option` shares the outer's type-erased 4-word
                    // `{tag, w0, w1, w2}` layout (`struct_ty`), and the outer
                    // `Some` payload BOXES it (4 words > 3 payload slots), so w0
                    // is the box pointer — unbox and load a 4-word struct.
                    let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let box_ptr = self
                        .builder
                        .build_int_to_ptr(w0, ptr_ty, "flat.box")
                        .unwrap();
                    let inner_val = self
                        .builder
                        .build_load(struct_ty, box_ptr, "flat.inner")
                        .unwrap();
                    let none_bv: BasicValueEnum<'ctx> = recv_struct.into();
                    let sel = self
                        .builder
                        .build_select(is_present, inner_val, none_bv, "flat.sel")
                        .unwrap();
                    return Ok(Some(sel));
                }
                "take" => {
                    // MUTATING: yield the receiver's current value and store a
                    // zeroed `None` (tag 0) back into the receiver's slot. The
                    // loaded `recv_struct` IS the taken value. Identifier
                    // receiver only — the shape a mutating take makes sense on;
                    // a fresh-temp receiver has no slot to null and gets a loud
                    // error rather than a silent no-op mutation.
                    let ExprKind::Identifier(recv_name) = &object.kind else {
                        return Err("codegen: Option.take requires a named Option binding \
                             as its receiver (a temporary has no slot to clear)"
                            .to_string());
                    };
                    let slot = self
                        .variables
                        .get(recv_name.as_str())
                        .map(|s| s.ptr)
                        .ok_or_else(|| {
                            format!("codegen: Option.take receiver '{recv_name}' has no slot")
                        })?;
                    let none_val = struct_ty.const_zero();
                    self.builder.build_store(slot, none_val).unwrap();
                    return Ok(Some(recv_struct.into()));
                }
                "get_or_insert" => {
                    // MUTATING: `Some(x)` yields `x`; `None` stores `Some(v)`
                    // into the receiver's slot and yields `v`. Result is the
                    // payload BY VALUE (matches the typechecker's modeling).
                    // Identifier receiver only, like `take`.
                    let ExprKind::Identifier(recv_name) = &object.kind else {
                        return Err("codegen: Option.get_or_insert requires a named Option \
                             binding as its receiver"
                            .to_string());
                    };
                    let slot = self
                        .variables
                        .get(recv_name.as_str())
                        .map(|s| s.ptr)
                        .ok_or_else(|| {
                            format!(
                                "codegen: Option.get_or_insert receiver '{recv_name}' \
                                 has no slot"
                            )
                        })?;
                    let inner_ll = self.llvm_type_for_type_expr(&inner_te);
                    let fn_val = self.current_fn.unwrap();
                    let some_bb = self.context.append_basic_block(fn_val, "goi.some");
                    let none_bb = self.context.append_basic_block(fn_val, "goi.none");
                    let merge_bb = self.context.append_basic_block(fn_val, "goi.merge");
                    self.builder
                        .build_conditional_branch(is_present, some_bb, none_bb)
                        .unwrap();

                    // Some(x): reconstruct and yield the payload.
                    self.builder.position_at_end(some_bb);
                    let w0 = self
                        .builder
                        .build_extract_value(recv_struct, 1, "goi.w0")
                        .map_err(|e| format!("codegen: get_or_insert w0: {:?}", e))?
                        .into_int_value();
                    let w1 = self
                        .builder
                        .build_extract_value(recv_struct, 2, "goi.w1")
                        .map_err(|e| format!("codegen: get_or_insert w1: {:?}", e))?
                        .into_int_value();
                    let w2 = self
                        .builder
                        .build_extract_value(recv_struct, 3, "goi.w2")
                        .map_err(|e| format!("codegen: get_or_insert w2: {:?}", e))?
                        .into_int_value();
                    let existing = self.rebuild_value_from_payload_words(inner_ll, w0, w1, w2)?;
                    let some_end = self.builder.get_insert_block().unwrap();
                    self.builder.build_unconditional_branch(merge_bb).unwrap();

                    // None: compile v, store Some(v) into the slot, yield v.
                    self.builder.position_at_end(none_bb);
                    let v_arg = args.first().ok_or_else(|| {
                        "codegen: Option.get_or_insert expects 1 argument".to_string()
                    })?;
                    let mut v_val = self.compile_expr(&v_arg.value)?;
                    // Width-coerce an int literal default to T (the unwrap_or
                    // precedent) so `get_or_insert(0)` feeds a narrow T cleanly.
                    if let (BasicValueEnum::IntValue(dv), BasicTypeEnum::IntType(it)) =
                        (v_val, inner_ll)
                    {
                        let dw = dv.get_type().get_bit_width();
                        let tw = it.get_bit_width();
                        if dw != tw {
                            v_val = if dw > tw {
                                self.builder
                                    .build_int_truncate(dv, it, "goi.v.tr")
                                    .unwrap()
                                    .into()
                            } else {
                                self.builder
                                    .build_int_z_extend(dv, it, "goi.v.zx")
                                    .unwrap()
                                    .into()
                            };
                        }
                    }
                    let words = self.coerce_to_payload_words(v_val, 3)?;
                    let mut some_agg = struct_ty.get_undef();
                    some_agg = self
                        .builder
                        .build_insert_value(some_agg, one, 0, "goi.tag")
                        .unwrap()
                        .into_struct_value();
                    for (i, w) in words.iter().enumerate() {
                        some_agg = self
                            .builder
                            .build_insert_value(some_agg, *w, (i + 1) as u32, "goi.w")
                            .unwrap()
                            .into_struct_value();
                    }
                    self.builder.build_store(slot, some_agg).unwrap();
                    let none_end = self.builder.get_insert_block().unwrap();
                    self.builder.build_unconditional_branch(merge_bb).unwrap();

                    self.builder.position_at_end(merge_bb);
                    let phi = self
                        .builder
                        .build_phi(existing.get_type(), "goi.result")
                        .map_err(|e| format!("codegen: get_or_insert phi: {:?}", e))?;
                    phi.add_incoming(&[(&existing, some_end), (&v_val, none_end)]);
                    return Ok(Some(phi.as_basic_value()));
                }
                _ => {}
            }
        }

        // unwrap_or(default): eager fallback, NO panic. Compile the default
        // once (matching Rust's eager `unwrap_or`, unlike `unwrap_or_else`),
        // then branch on the tag — present (tag == 1) reconstitutes the
        // payload from the words, absent yields the default — and phi the two.
        // An int default is width-coerced to `T`'s LLVM shape so a literal
        // `0` (i64) feeds a narrower `T` (e.g. `Option[i32]`) cleanly; the
        // typechecker types this call as `T`, so non-int defaults already
        // match the reconstituted shape.
        if method == "unwrap_or" {
            let default_arg = args.first().ok_or_else(|| {
                "codegen: Option/Result.unwrap_or expects 1 argument, found 0".to_string()
            })?;
            let inner_ll = self.llvm_type_for_type_expr(&inner_te);
            // Snapshot the innermost cleanup frame BEFORE compiling the
            // default, so an f-string default's `acc` cleanup (armed by
            // `track_vec_var` during compile) can be located and suppressed
            // below (B-2026-07-16-23 leg 3).
            let cleanup_snap = self
                .scope_cleanup_actions
                .last()
                .map(|f| f.len())
                .unwrap_or(0);
            let mut default_val = self.compile_expr(&default_arg.value)?;
            if let (BasicValueEnum::IntValue(dv), BasicTypeEnum::IntType(it)) =
                (default_val, inner_ll)
            {
                let dw = dv.get_type().get_bit_width();
                let tw = it.get_bit_width();
                if dw != tw {
                    default_val = if dw > tw {
                        self.builder
                            .build_int_truncate(dv, it, "uo.def.tr")
                            .unwrap()
                            .into()
                    } else {
                        self.builder
                            .build_int_z_extend(dv, it, "uo.def.zx")
                            .unwrap()
                            .into()
                    };
                }
            }

            // A moved owned Vec/String binding default (`let d = "x".to_string();
            // opt(i).unwrap_or(d)`) is CONSUMED by unwrap_or — the ownership
            // checker treats it as a move (reusing `d` after is a move error).
            // But `default_val` is a shallow copy of the binding's
            // {ptr,len,cap}; without suppression the absent path binds that copy
            // to the result (freed at scope) AND the binding `d` is freed at ITS
            // scope → double-free (B-2026-07-16-23 leg 1). Suppress the binding's
            // scope-exit free (zero its `cap`) HERE — before the branch, so BOTH
            // paths see cap==0 — while the already-loaded `default_val` keeps the
            // original cap>0 and is freed exactly once: by the present-path free
            // below (Some/Ok) or via the result binding at scope (None/Err).
            // Gated to a binding whose slot holds the Vec/String INLINE (owned):
            // a `ref` binding's slot is a pointer, and a borrow cannot be moved
            // into unwrap_or anyway, so it never reaches here — but the inline
            // guard makes the present-path free provably safe (never frees a
            // borrowed buffer).
            let default_is_moved_heap_ident = matches!(
                &default_arg.value.kind,
                ExprKind::Identifier(n)
                    if self.vec_elem_types.contains_key(n.as_str())
                        && self.variables.get(n.as_str()).is_some_and(|s| matches!(
                            s.ty,
                            BasicTypeEnum::StructType(held) if held == self.vec_struct_type()
                        ))
            );
            if default_is_moved_heap_ident {
                self.suppress_source_vec_cleanup_for_arg(&default_arg.value);
            }

            // f-string default (`unwrap_or(f"…")`): the InterpolatedStringLit
            // materializes into an `acc` alloca that `track_vec_var` armed for
            // scope-exit free (exprs.rs). On the ABSENT path that same buffer
            // becomes the unwrap_or result, so the result binding frees it AND
            // the acc cleanup frees it → double-free (B-2026-07-16-23 leg 3;
            // `free(): double free detected`). Remove the acc's FreeVecBuffer
            // (added during the default's compile, so it sits in the frame tail
            // past `cleanup_snap`) and free `default_val` on the present path
            // instead — the temp analogue of leg 1's moved-binding cap-zeroing.
            // Result: freed exactly once per branch — present → the present-path
            // free below; absent → the result binding at scope.
            let default_is_fstring =
                matches!(&default_arg.value.kind, ExprKind::InterpolatedStringLit(_));
            if default_is_fstring {
                if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                    let mut i = frame.len();
                    while i > cleanup_snap {
                        i -= 1;
                        if matches!(
                            frame[i],
                            crate::codegen::state::CleanupAction::FreeVecBuffer { .. }
                        ) {
                            frame.remove(i);
                        }
                    }
                }
            }

            let fn_val = self.current_fn.unwrap();
            let present_bb = self.context.append_basic_block(fn_val, "uo.present");
            let absent_bb = self.context.append_basic_block(fn_val, "uo.absent");
            let merge_bb = self.context.append_basic_block(fn_val, "uo.merge");
            let one = i64_t.const_int(1, false);
            let is_present = self
                .builder
                .build_int_compare(IntPredicate::EQ, tag, one, "uo.is_present")
                .unwrap();
            self.builder
                .build_conditional_branch(is_present, present_bb, absent_bb)
                .unwrap();

            // Present: reconstitute the inner value from the payload words
            // (same unbox-or-words logic as the unwrap path below).
            self.builder.position_at_end(present_bb);
            let w0 = self
                .builder
                .build_extract_value(recv_struct, 1, "uo.w0")
                .map_err(|e| format!("codegen: extract unwrap_or payload w0: {:?}", e))?
                .into_int_value();
            let w1 = self
                .builder
                .build_extract_value(recv_struct, 2, "uo.w1")
                .map_err(|e| format!("codegen: extract unwrap_or payload w1: {:?}", e))?
                .into_int_value();
            let w2 = self
                .builder
                .build_extract_value(recv_struct, 3, "uo.w2")
                .map_err(|e| format!("codegen: extract unwrap_or payload w2: {:?}", e))?
                .into_int_value();
            let area = (recv_struct.get_type().count_fields() as usize).saturating_sub(1);
            let present_val = if Self::llvm_type_word_count(inner_ll) > area {
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let box_ptr = self
                    .builder
                    .build_int_to_ptr(w0, ptr_ty, "uo.box.p")
                    .unwrap();
                self.builder
                    .build_load(inner_ll, box_ptr, "uo.box.ld")
                    .unwrap()
            } else {
                self.rebuild_value_from_payload_words(inner_ll, w0, w1, w2)?
            };
            // `unwrap_or`'s default is evaluated EAGERLY (before the branch), so
            // in the present (Some/Ok) path it is DISCARDED and its heap buffer
            // leaks once per call — unbounded in a loop (the pure-constant-Some
            // case elides the default entirely, so the leak only shows on a
            // data-dependent receiver). Free it here for the two proven-safe
            // fresh-owned shapes: a Call/MethodCall temp
            // (`unwrap_or("d".to_string())`) and a `String[a..b]` slice, each a
            // fresh `cap>0` buffer with no other owner (the absent path binds it
            // to the result, freed once at scope; the present path frees it here,
            // once — no double-free). `free_str_vec_buffer_if_heap`'s cap>0 guard
            // additionally no-ops on a scalar / borrowed (cap==0) default. Other
            // default shapes are EXCLUDED, left for the follow-up
            // B-2026-07-16-23: a place-expr (identifier / field / index) default
            // is either borrowed (must not be freed) or a moved owned binding
            // that needs scope-exit move-suppression (a pre-existing
            // double-free); an f-string / collection-literal default is already
            // temp-tracked by the statement machinery, so freeing it here would
            // double-free. B-2026-07-16-22.
            if self.expr_yields_fresh_owned_temp(&default_arg.value)
                || self.expr_is_fresh_owned_string_slice(&default_arg.value)
                || default_is_moved_heap_ident
                || default_is_fstring
                || matches!(
                    &default_arg.value.kind,
                    ExprKind::ArrayLiteral(_)
                        | ExprKind::PrefixCollectionLiteral { .. }
                        | ExprKind::RepeatLiteral { .. }
                )
            {
                self.free_str_vec_buffer_if_heap(default_val);
            }
            let present_end = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Absent: fall through to the default.
            self.builder.position_at_end(absent_bb);
            self.builder.build_unconditional_branch(merge_bb).unwrap();

            // Merge: select present vs default.
            self.builder.position_at_end(merge_bb);
            let phi = self
                .builder
                .build_phi(inner_ll, "uo.val")
                .map_err(|e| format!("codegen: unwrap_or phi: {:?}", e))?;
            phi.add_incoming(&[(&present_val, present_end), (&default_val, absent_bb)]);
            // B-2026-07-17-4: like `unwrap`/`expect` (B-2026-07-10-2, below),
            // `unwrap_or` CONSUMES the receiver — the present branch's
            // reconstituted payload is a SHALLOW alias of the receiver's
            // inline heap buffer, and the consumer now owns it (a chained
            // `.len()` frees it as an owned temp). A LET-BOUND identifier
            // receiver's scope-exit `FreeInlineOptionPayload` would free the
            // same buffer again → double-free (`let a =
            // r.unwrap_or("x").len();` aborted; the absent path is
            // unaffected — zeroing a payload-less slot is a no-op). Same
            // no-op cases as the unwrap site: fresh-temp receiver, non-heap
            // payload.
            self.suppress_inline_option_result_binding_move(object);
            return Ok(Some(phi.as_basic_value()));
        }

        // unwrap / expect / unwrap_err / expect_err: the extract-or-panic
        // family. `unwrap`/`expect` extract the Some/Ok payload (tag != 0) and
        // panic on tag == 0 (None/Err); the `_err` mirrors extract the Err
        // payload of a `Result` (tag == 0) and panic on tag == 1 (Ok). Both
        // extract the SAME payload words w0..w2 — a tagged union overlays the
        // Ok/Err payloads — and the reconstituted inner type differs (T vs E),
        // supplied by the typechecker via `method_unwrap_inner_types` (E for the
        // `_err` variants). `expect`/`expect_err` accept a single string-message
        // arg, compiled for side-effects / typecheck completeness; the panic text
        // is fixed at the call site for v1.
        let is_err_variant = method == "unwrap_err" || method == "expect_err";
        let is_expect = method == "expect" || method == "expect_err";
        if is_expect {
            for a in args {
                let _ = self.compile_expr(&a.value)?;
            }
        } else if !args.is_empty() {
            return Err(format!(
                "codegen: Option/Result.{} takes no arguments, found {}",
                method,
                args.len()
            ));
        }

        let fn_val = self.current_fn.unwrap();
        let fail_bb = self.context.append_basic_block(fn_val, "or.unwrap.fail");
        let ok_bb = self.context.append_basic_block(fn_val, "or.unwrap.ok");
        // Panic tag: `_err` variants fail on Ok (tag == 1); the normal variants
        // fail on None/Err (tag == 0).
        let fail_tag = i64_t.const_int(if is_err_variant { 1 } else { 0 }, false);
        let should_fail = self
            .builder
            .build_int_compare(IntPredicate::EQ, tag, fail_tag, "or.should_fail")
            .unwrap();
        self.builder
            .build_conditional_branch(should_fail, fail_bb, ok_bb)
            .unwrap();

        // Fail block: panic with a concise message naming the operation.
        self.builder.position_at_end(fail_bb);
        let msg = match method {
            "expect" => "expect() called on None/Err",
            "expect_err" => "expect_err() called on Ok",
            "unwrap_err" => "unwrap_err() called on Ok",
            _ => "unwrap() called on None/Err",
        };
        self.emit_panic(msg);
        self.builder.build_unreachable().unwrap();

        // OK block: reconstitute the inner value. Extract w0..w2 once so
        // any of the downstream LLVM shapes can pick the words it needs
        // without re-extracting (and so the IR is uniform regardless of T).
        self.builder.position_at_end(ok_bb);
        let w0 = self
            .builder
            .build_extract_value(recv_struct, 1, "or.w0")
            .map_err(|e| format!("codegen: extract Option payload w0: {:?}", e))?
            .into_int_value();
        let w1 = self
            .builder
            .build_extract_value(recv_struct, 2, "or.w1")
            .map_err(|e| format!("codegen: extract Option payload w1: {:?}", e))?
            .into_int_value();
        let w2 = self
            .builder
            .build_extract_value(recv_struct, 3, "or.w2")
            .map_err(|e| format!("codegen: extract Option payload w2: {:?}", e))?
            .into_int_value();

        // Reconstruct based on the inner type's LLVM shape.
        // B-2026-07-14-16 (scalar crash leg) + B-2026-07-14-11 (Vec/String leg):
        // `v.get(i)` / `.first()` / `.last()` return `Option[ref T]` but PACK the
        // element VALUE into the payload words (`get.valid` loads `*elem` and
        // coerces its words). The `ref` is a lifetime marker, NOT a stored
        // address — yet `ref T` lowers to `ptr` (types_lowering.rs), so the
        // default reconstruction `inttoptr`s w0: a scalar `10` becomes pointer
        // `0xA` (crashes with an `Invalid read` on use), and a `Vec`/`String`'s
        // `{ptr,len,cap}` collapses to just its data pointer (len/cap lost, so
        // `row.len()` reads garbage). Peel the leading `ref`/`mut ref` so the
        // rebuild uses the pointee's VALUE shape.
        //
        // Producer-aware gate: peeling is correct ONLY because these builtin
        // accessors pack the VALUE. A GENUINE address-packing `Option[ref T]`
        // (a user `Some(some_ref)`) stores the pointer in w0 and MUST keep the
        // `inttoptr` reconstruction — peeling it would misread 3 words off a
        // single pointer. So the aggregate legs peel ONLY when the `.unwrap()`
        // receiver is one of the value-packing borrow accessors
        // (`get`/`first`/`last`, whose `Option[ref _]` return shape is what put
        // a `ref` in `inner_te` here). Under that gate, peel any aggregate
        // pointee that lowers to a StructType — the 3-word `{ptr,len,cap}`
        // Vec/String/VecDeque shape (B-2026-07-14-11) AND user structs
        // (B-2026-07-14-16 struct leg): `Vec.get` packs the element VALUE
        // (`coerce_to_payload_words` on the loaded element) for every element
        // type, so rebuilding at the pointee's value shape is always right for
        // these accessors. The SCALAR peel stays unconditional (no producer ever
        // packs a scalar's address). A genuine address-packing `Option[ref T]`
        // (a user `Some(some_ref)`) is NOT a get/first/last receiver, so it
        // keeps its `inttoptr`.
        let recv_is_value_packing_borrow_accessor = matches!(
            &object.kind,
            ExprKind::MethodCall { method: m, .. }
                if matches!(m.as_str(), "get" | "first" | "last")
        );
        let recon_te = match &inner_te.kind {
            TypeKind::Ref(inner) | TypeKind::MutRef(inner)
                if matches!(
                    self.llvm_type_for_type_expr(inner),
                    BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_)
                ) || (recv_is_value_packing_borrow_accessor
                    && matches!(
                        self.llvm_type_for_type_expr(inner),
                        BasicTypeEnum::StructType(_)
                    )) =>
            {
                inner.as_ref()
            }
            _ => &inner_te,
        };
        let inner_ll = self.llvm_type_for_type_expr(recon_te);
        // Oversized boxed payload (see `coerce_to_payload_words`): if `T`'s
        // LLVM word count exceeds this enum's payload area, the pack side
        // heap-boxed it and w0 holds the box pointer. The area is the
        // receiver's field count minus the tag (Option → 3, Result → 5),
        // so a `Result` payload that legitimately fits in 5 words is not
        // mistaken for boxed. Mirror of `reconstruct_payload_value`'s unbox.
        let area = (recv_struct.get_type().count_fields() as usize).saturating_sub(1);
        let value = if Self::llvm_type_word_count(inner_ll) > area {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let box_ptr = self
                .builder
                .build_int_to_ptr(w0, ptr_ty, "enumbox.uw.p")
                .unwrap();
            self.builder
                .build_load(inner_ll, box_ptr, "enumbox.uw.ld")
                .unwrap()
        } else {
            self.rebuild_value_from_payload_words(inner_ll, w0, w1, w2)?
        };
        // B-2026-07-15-26: `map.get(k).unwrap()` on a NON-shared heap value
        // (`Map[K, String]` / `Map[K, Vec[..]]`) packs a BORROW of the bucket's
        // `{ptr,len,cap}` — the buffer belongs to the map. Consumed INLINE (a
        // println arg, a method receiver, a call arg) the temporary's own
        // `cap > 0` free-guard frees that buffer, which the map's scope-exit
        // per-entry drop ALSO frees → double-free. Zero the borrow view's `cap`
        // so every consumer's free-guard skips it (the map stays the sole owner);
        // reads use ptr+len and are unaffected. Harmless for the let-bound case
        // (borrow-elided, reads only) and closes that binding's latent move-out
        // double-free too. Only fires for the `<map>.get(k)` receiver shape, so a
        // genuinely-owned unwrap payload keeps its real `cap`.
        let value = match value {
            BasicValueEnum::StructValue(sv)
                if sv.get_type() == self.vec_struct_type()
                    && self.unwrap_receiver_is_nonshared_heap_value_map_get(object) =>
            {
                self.builder
                    .build_insert_value(sv, i64_t.const_zero(), 2, "map.get.borrow.cap0")
                    .unwrap()
                    .into_struct_value()
                    .into()
            }
            _ => value,
        };
        // B-2026-07-10-2 — the extracted payload is a SHALLOW alias of the
        // receiver's inline/boxed heap buffer. `unwrap`/`expect`/`unwrap_err`/
        // `expect_err` CONSUME the receiver, so a LET-BOUND receiver's scope-exit
        // drop must be disarmed or it frees the buffer the returned value now
        // solely owns (double-free). This zeros a tracked inline/boxed
        // `Option`/`Result` identifier receiver's slot; it no-ops for a
        // fresh-temp receiver (not an identifier — already single-owned) and for
        // a non-heap payload (not in the tracked sets).
        self.suppress_inline_option_result_binding_move(object);
        Ok(Some(value))
    }

    /// Bind `payload` to a synthetic local and compile `closure(x)` at the
    /// builder's current insert block, restoring the synthetic binding after.
    /// Used by the `map_or`/`and_then`/`filter` codegen (B-2026-07-14-6) to
    /// invoke a combinator's closure on the reconstructed present payload —
    /// the same mechanism `Option/Result.map` uses inline.
    fn compile_optres_closure_on(
        &mut self,
        closure: &Expr,
        payload: BasicValueEnum<'ctx>,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.expect("closure call inside a function");
        let cur = self.builder.get_insert_block().unwrap();
        let x_name = "__karac_optres_x";
        let alloca = self.create_entry_alloca(fn_val, x_name, payload.get_type());
        // `create_entry_alloca` repositions the builder to the entry block.
        self.builder.position_at_end(cur);
        self.builder.build_store(alloca, payload).unwrap();
        let saved = self.variables.insert(
            x_name.to_string(),
            VarSlot {
                ptr: alloca,
                ty: payload.get_type(),
            },
        );
        let mk = |kind: ExprKind| Expr {
            kind,
            span: call_span.clone(),
        };
        let arg = CallArg {
            label: None,
            mut_marker: false,
            span: call_span.clone(),
            value: mk(ExprKind::Identifier(x_name.to_string())),
        };
        let call = mk(ExprKind::Call {
            callee: Box::new(closure.clone()),
            args: vec![arg],
        });
        let result = self.compile_expr(&call);
        match saved {
            Some(s) => {
                self.variables.insert(x_name.to_string(), s);
            }
            None => {
                self.variables.remove(x_name);
            }
        }
        result
    }

    /// Compile a NO-ARG closure call `closure()` at the builder's current
    /// insert block — the absent-branch thunk of the OPTION `unwrap_or_else` /
    /// `map_or_else` / `or_else` combinators (B-2026-07-14-6).
    fn compile_optres_closure_noarg(
        &mut self,
        closure: &Expr,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let call = Expr {
            kind: ExprKind::Call {
                callee: Box::new(closure.clone()),
                args: vec![],
            },
            span: call_span.clone(),
        };
        self.compile_expr(&call)
    }

    /// Slice OR helper: reconstitute a value of the requested LLVM type
    /// from the 3 payload words of an Option/Result aggregate. The packing
    /// side is `coerce_to_payload_words` (see `call_dispatch.rs`); this is
    /// the symmetric unpack. Coverage matches the kata workloads through
    /// v1.x:
    /// - Integer types (i8/i16/i32/i64 + unsigned) and bool/char: trunc
    ///   from i64 w0 to the requested width.
    /// - Float types (f32/f64): bitcast w0 to the float type.
    /// - Pointer types (Shared structs, Map/Set handles, Request, slice
    ///   data pointer, ref/mut ref): inttoptr w0.
    /// - 3-word Vec/String shape: insertvalue w0/w1/w2 into the
    ///   {ptr, i64 len, i64 cap} struct, with w0 reinterpreted as a
    ///   pointer.
    /// - 2-word Slice shape: insertvalue w0/w1 into the {ptr, i64 len}
    ///   struct, with w0 reinterpreted as a pointer.
    /// - User struct: rebuild field-by-field from sequential words. Each
    ///   field consumes one word for primitive types and is recursively
    ///   reconstructed for aggregate fields (per the symmetric packing
    ///   contract of `coerce_to_payload_words`).
    pub(super) fn rebuild_value_from_payload_words(
        &self,
        target_ty: BasicTypeEnum<'ctx>,
        w0: inkwell::values::IntValue<'ctx>,
        w1: inkwell::values::IntValue<'ctx>,
        w2: inkwell::values::IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let slice_ty = self.slice_struct_type();
        match target_ty {
            BasicTypeEnum::IntType(it) => {
                if it.get_bit_width() == 64 {
                    Ok(w0.into())
                } else if it.get_bit_width() < 64 {
                    Ok(self
                        .builder
                        .build_int_truncate(w0, it, "or.pl.tr")
                        .unwrap()
                        .into())
                } else {
                    Ok(self
                        .builder
                        .build_int_z_extend(w0, it, "or.pl.zx")
                        .unwrap()
                        .into())
                }
            }
            BasicTypeEnum::FloatType(ft) => {
                // f64 bitcasts directly; f32's pattern sits in the low 32
                // bits (see `coerce_to_i64`'s pack side), so truncate then
                // bitcast — a direct i64→f32 bitcast is invalid IR
                // (B-2026-07-20-11).
                if ft == self.context.f64_type() {
                    Ok(self.builder.build_bit_cast(w0, ft, "or.pl.fc").unwrap())
                } else {
                    let lo = self
                        .builder
                        .build_int_truncate(w0, self.context.i32_type(), "or.pl.f32.tr")
                        .unwrap();
                    Ok(self.builder.build_bit_cast(lo, ft, "or.pl.f32.bc").unwrap())
                }
            }
            BasicTypeEnum::PointerType(_) => Ok(self
                .builder
                .build_int_to_ptr(w0, ptr_ty, "or.pl.itop")
                .unwrap()
                .into()),
            BasicTypeEnum::StructType(st) => {
                // Vec/String shape: 3 fields, first is ptr.
                if st == vec_ty {
                    let p = self
                        .builder
                        .build_int_to_ptr(w0, ptr_ty, "or.pl.vec.ptr")
                        .unwrap();
                    let mut agg = vec_ty.get_undef();
                    agg = self
                        .builder
                        .build_insert_value(agg, p, 0, "or.pl.vec.f0")
                        .unwrap()
                        .into_struct_value();
                    agg = self
                        .builder
                        .build_insert_value(agg, w1, 1, "or.pl.vec.f1")
                        .unwrap()
                        .into_struct_value();
                    agg = self
                        .builder
                        .build_insert_value(agg, w2, 2, "or.pl.vec.f2")
                        .unwrap()
                        .into_struct_value();
                    return Ok(agg.into());
                }
                if st == slice_ty {
                    let p = self
                        .builder
                        .build_int_to_ptr(w0, ptr_ty, "or.pl.slice.ptr")
                        .unwrap();
                    let mut agg = slice_ty.get_undef();
                    agg = self
                        .builder
                        .build_insert_value(agg, p, 0, "or.pl.slice.f0")
                        .unwrap()
                        .into_struct_value();
                    agg = self
                        .builder
                        .build_insert_value(agg, w1, 1, "or.pl.slice.f1")
                        .unwrap()
                        .into_struct_value();
                    return Ok(agg.into());
                }
                // Generic struct: field-by-field reconstruction from
                // sequential words. Each field consumes as many words as its
                // LLVM width demands (1 for a scalar, 2 for a Slice, 3 for a
                // String/Vec, recursively for nested structs), so a payload
                // like `AppError { msg: String }` correctly claims all three
                // words for its single String field. Covers the v1.x kata
                // workloads that fit inside the ≤3-word Option/Result payload
                // budget; larger payloads stay on the deferred path until the
                // layout widens further.
                let n_fields = st.count_fields() as usize;
                let words = [w0, w1, w2];
                let zero = i64_t.const_zero();
                let mut agg = st.get_undef();
                let mut cursor = 0usize;
                for i in 0..n_fields {
                    let field_ty = st
                        .get_field_type_at_index(i as u32)
                        .ok_or_else(|| format!("or.pl.struct: field {} type missing", i))?;
                    let need = self.payload_words_for_type(field_ty);
                    if cursor >= words.len() {
                        break;
                    }
                    // Feed this field its own window of words (padding with
                    // zero when the field straddles the end of the budget).
                    let fw0 = words[cursor];
                    let fw1 = if cursor + 1 < words.len() {
                        words[cursor + 1]
                    } else {
                        zero
                    };
                    let fw2 = if cursor + 2 < words.len() {
                        words[cursor + 2]
                    } else {
                        zero
                    };
                    let field_val =
                        self.rebuild_value_from_payload_words(field_ty, fw0, fw1, fw2)?;
                    agg = self
                        .builder
                        .build_insert_value(agg, field_val, i as u32, "or.pl.s.iv")
                        .unwrap()
                        .into_struct_value();
                    cursor += need.max(1);
                }
                Ok(agg.into())
            }
            BasicTypeEnum::ArrayType(_) => {
                // Fixed-size arrays as Option payloads aren't expected in
                // v1.x kata workloads; conservatively return w0 in i64
                // form so downstream code at least compiles. Surfaces a
                // bug-shaped artifact rather than a hard error if reached.
                Ok(i64_t.const_zero().into())
            }
            _ => Ok(w0.into()),
        }
    }

    /// Number of i64 payload words an LLVM type occupies when packed into an
    /// Option/Result payload by `coerce_to_payload_words`. Scalars are one
    /// word; a Slice is two ({ptr,len}); a String/Vec is three
    /// ({ptr,len,cap}); a nested struct is the sum of its fields. Used by
    /// `rebuild_value_from_payload_words` to advance its word cursor
    /// field-by-field so multi-word fields claim their full span.
    fn payload_words_for_type(&self, ty: BasicTypeEnum<'ctx>) -> usize {
        match ty {
            BasicTypeEnum::StructType(st) => {
                if st == self.vec_struct_type() {
                    3
                } else if st == self.slice_struct_type() {
                    2
                } else {
                    (0..st.count_fields())
                        .filter_map(|i| st.get_field_type_at_index(i))
                        .map(|ft| self.payload_words_for_type(ft))
                        .sum::<usize>()
                        .max(1)
                }
            }
            _ => 1,
        }
    }
}
