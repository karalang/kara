//! Map-collection codegen: construction, indexing, method dispatch.
//!
//! Houses the `compile_map_*` cluster: `compile_map_new_stmt`,
//! `compile_set_new_stmt`, `compile_map_literal_stmt`,
//! `compile_map_index_store`, `compile_map_index`,
//! `compile_map_keys_values_entries`, and `compile_map_method`.
//!
//! Maps lower to the `karac_map_*` runtime in `runtime/src/map.rs`;
//! these methods marshal arguments / extract results across the FFI
//! boundary and emit the per-K/V hash + eq fns via `synth.rs`.

use crate::ast::*;

use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::AddressSpace;

use super::state::VarSlot;

impl<'ctx> super::Codegen<'ctx> {
    /// Construct an empty Map/Set runtime handle for `var_name` and
    /// return the opaque `ptr`. Computes the key/val element sizes and
    /// emits (or reuses) the per-key-type hash/eq fns from the side
    /// tables `register_var_from_type_expr` / the local-binding paths
    /// seed under `var_name`, then calls `karac_map_new`. `is_set`
    /// selects the Set shape (`val_size = 0`, element type read from
    /// `set_elem_*` rather than `map_key_*` / `map_val_*`).
    ///
    /// Pure handle construction — no alloca, no `self.variables`
    /// registration, no scope-exit cleanup tracking. Callers layer
    /// those on: `compile_map_new_stmt` / `compile_set_new_stmt` for a
    /// local `let`, and `finalize_module_binding_static_init` for a
    /// module-scope binding (which stores the handle into a global and
    /// never frees it — the binding lives for the whole process).
    pub(super) fn build_map_new_handle(
        &mut self,
        var_name: &str,
        is_set: bool,
    ) -> PointerValue<'ctx> {
        let i64_t = self.context.i64_type();

        // Element type + (key_size, val_size). Set is the `Map[T, ()]`
        // bucket with the value side stripped (`val_size = 0`).
        let (key_ty, val_size) = if is_set {
            let elem_ty = self
                .set_elem_types
                .get(var_name)
                .copied()
                .unwrap_or(i64_t.into());
            (elem_ty, i64_t.const_int(0, false))
        } else {
            let key_ty = self
                .map_key_types
                .get(var_name)
                .copied()
                .unwrap_or(i64_t.into());
            let val_ty = self
                .map_val_types
                .get(var_name)
                .copied()
                .unwrap_or(i64_t.into());
            let val_size = val_ty
                .size_of()
                .unwrap_or_else(|| i64_t.const_int(8, false));
            (key_ty, val_size)
        };
        let key_size = key_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));

        // Emit (or reuse) hash/eq functions for the concrete key type.
        // Prefer the TypeExpr-aware path so compound key shapes (tuples, …)
        // compose correctly via per-field recursion. The plain
        // `emit_hash_fn_for_type` path is the fallback for code paths that
        // never registered a `TypeExpr` for the variable. Set elements are
        // the key of the underlying bucket, so they consult the `set_elem_*`
        // tables.
        let key_te = if is_set {
            self.set_elem_type_exprs.get(var_name).cloned()
        } else {
            self.map_key_type_exprs.get(var_name).cloned()
        };
        let (hash_fn, eq_fn) = if let Some(key_te) = key_te {
            (
                self.emit_hash_fn_for_type_expr(&key_te),
                self.emit_eq_fn_for_type_expr(&key_te),
            )
        } else {
            let type_name = if is_set {
                self.set_elem_type_names.get(var_name).cloned()
            } else {
                self.map_key_type_names.get(var_name).cloned()
            }
            .unwrap_or_else(|| "i64".to_string());
            (
                self.emit_hash_fn_for_type(&type_name, key_ty),
                self.emit_eq_fn_for_type(&type_name, key_ty),
            )
        };
        let hash_fn_ptr = hash_fn.as_global_value().as_pointer_value();
        let eq_fn_ptr = eq_fn.as_global_value().as_pointer_value();

        self.builder
            .build_call(
                self.karac_map_new_fn,
                &[
                    key_size.into(),
                    val_size.into(),
                    hash_fn_ptr.into(),
                    eq_fn_ptr.into(),
                ],
                if is_set { "set.new" } else { "map.new" },
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value()
    }

    /// Build a fresh `Map`/`Set` handle from a `Map[K, V]` / `Set[T]`
    /// `TypeExpr` directly — no per-variable `map_key_types` registration
    /// required. The name-keyed `build_map_new_handle` above serves `let`
    /// bindings (which register K/V under the binding name); this serves
    /// contexts where the K/V types come from a declared TYPE instead, e.g. a
    /// `Map`-typed struct field initialized with `Map.new()` in a constructor
    /// (`Cache { index: Map.new() }`). Without it, `Map.new()` in field position
    /// falls through `compile_expr` to the `i64 0` default, which builds an
    /// `insertvalue i64 0` into the pointer-typed field slot — invalid IR
    /// (B-2026-07-08-12). Returns `None` if `te` is not a `Map`/`Set` path.
    /// The caller owns the returned handle's lifetime (a struct field's handle
    /// is freed by the struct's generated Drop, so no scope-exit action here).
    pub(super) fn build_map_new_handle_from_type_expr(
        &mut self,
        te: &TypeExpr,
    ) -> Option<PointerValue<'ctx>> {
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        let last = p.segments.last().map(|s| s.as_str())?;
        let is_set = matches!(last, "Set" | "SortedSet");
        let is_map = matches!(last, "Map" | "SortedMap");
        if !is_set && !is_map {
            return None;
        }
        let i64_t = self.context.i64_type();
        let args = p.generic_args.as_ref();
        let key_te = args.and_then(|a| a.first()).and_then(|g| match g {
            crate::ast::GenericArg::Type(t) => Some(t.clone()),
            _ => None,
        });
        let key_ty = key_te
            .as_ref()
            .map(|t| self.llvm_type_for_type_expr(t))
            .unwrap_or(i64_t.into());
        let key_size = key_ty
            .size_of()
            .unwrap_or_else(|| i64_t.const_int(8, false));
        let val_size = if is_set {
            i64_t.const_int(0, false)
        } else {
            let val_te = args.and_then(|a| a.get(1)).and_then(|g| match g {
                crate::ast::GenericArg::Type(t) => Some(t.clone()),
                _ => None,
            });
            let val_ty = val_te
                .as_ref()
                .map(|t| self.llvm_type_for_type_expr(t))
                .unwrap_or(i64_t.into());
            val_ty
                .size_of()
                .unwrap_or_else(|| i64_t.const_int(8, false))
        };
        let (hash_fn, eq_fn) = if let Some(kte) = key_te {
            (
                self.emit_hash_fn_for_type_expr(&kte),
                self.emit_eq_fn_for_type_expr(&kte),
            )
        } else {
            (
                self.emit_hash_fn_for_type("i64", key_ty),
                self.emit_eq_fn_for_type("i64", key_ty),
            )
        };
        let hash_fn_ptr = hash_fn.as_global_value().as_pointer_value();
        let eq_fn_ptr = eq_fn.as_global_value().as_pointer_value();
        Some(
            self.builder
                .build_call(
                    self.karac_map_new_fn,
                    &[
                        key_size.into(),
                        val_size.into(),
                        hash_fn_ptr.into(),
                        eq_fn_ptr.into(),
                    ],
                    if is_set { "set.new.te" } else { "map.new.te" },
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value(),
        )
    }

    /// Emit `karac_map_new`, alloca a ptr slot to hold the opaque handle, and
    /// register a scope-exit `karac_map_free` cleanup action.
    /// Called from `compile_stmt` when the RHS is `Map.new()`.
    pub(super) fn compile_map_new_stmt(&mut self, var_name: &str) -> Result<(), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let key_ty = self
            .map_key_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());
        let val_ty = self
            .map_val_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        let map_handle = self.build_map_new_handle(var_name, /* is_set = */ false);

        let fn_val = self.current_fn.unwrap();
        let slot_ptr = self.create_entry_alloca(fn_val, &format!("{var_name}.slot"), ptr_ty.into());
        self.builder.build_store(slot_ptr, map_handle).unwrap();
        self.variables.insert(
            var_name.to_string(),
            VarSlot {
                ptr: slot_ptr,
                ty: ptr_ty.into(),
            },
        );
        let key_is_vec = self.llvm_ty_is_vec_struct(key_ty);
        let val_is_vec = self.llvm_ty_is_vec_struct(val_ty);
        let val_shared_heap = self.map_val_shared_heap_type_for(var_name);
        let key_shared_heap = self.map_key_shared_heap_type_for(var_name);
        // Slice 3r (deferred gap (d)): a value that owns heap beyond the
        // one-level `{ptr,len,cap}` overlay gets a synthesized per-value
        // drop fn; it owns the whole value side, so the flag/shared halves
        // are forced off (the selector returns None for shared V and for
        // values the flag free handles exactly).
        let val_drop_fn = self
            .var_elem_type_exprs
            .get(var_name)
            .cloned()
            .and_then(|te| self.map_val_drop_fn_for_type_expr(&te));
        let (val_is_vec, val_shared_heap) = if val_drop_fn.is_some() {
            (false, None)
        } else {
            (val_is_vec, val_shared_heap)
        };
        // Slice c-repl.B.5.3b: skip the scope-exit FreeMapHandle when
        // the let binding is destined for the cross-cell snapshot
        // global — the snapshot owns the handle's lifetime (until the
        // runner dies via `:reset` / shadow / panic), and freeing it
        // at end of the let's scope would leave the global pointing
        // at reclaimed memory. The slot still gets the handle so
        // same-cell ops (`m.insert(...)`, `m.get(...)`) work via
        // direct slot reads — unlike Vec/String capture, we don't
        // need a "null the slot" suppression because Map's cleanup
        // is queue-driven (skip the queue push, no further action
        // needed at scope exit).
        if !self.snapshot_capture.contains_key(var_name) {
            self.track_map_var_with_val_drop(
                slot_ptr,
                key_is_vec,
                val_is_vec,
                val_shared_heap,
                key_shared_heap,
                val_drop_fn,
            );
        }
        // Record the binding's surface type name, mirroring the place-source
        // path (`let mm = s.m` records "Map" via `record_var_type_name`). Without
        // it, `type_name_of` returns None for a `Map.new()`-created var, so a
        // bare tuple over it (`let t = (d, i)`) can't infer the Map leaf's
        // `TypeExpr` (`infer_arg_elem_te` → empty path) and the tuple's Part A
        // drop never registers — the handle leaked (Linux LSan; silent on
        // macOS). #23 sibling.
        self.record_var_type_name(var_name.to_string(), "Map".to_string());
        Ok(())
    }

    /// Compile `let s: Set[T] = Set.new()` — emit `karac_map_new(elem_size,
    /// 0, hash_fn, eq_fn)` (val_size = 0 → key-only table), alloca a slot
    /// for the opaque handle, register the scope-exit `karac_map_free`
    /// cleanup. Mirrors `compile_map_new_stmt` with the value side stripped.
    pub(super) fn compile_set_new_stmt(&mut self, var_name: &str) -> Result<(), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        let elem_ty = self
            .set_elem_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        let set_handle = self.build_map_new_handle(var_name, /* is_set = */ true);

        let fn_val = self.current_fn.unwrap();
        let slot_ptr = self.create_entry_alloca(fn_val, &format!("{var_name}.slot"), ptr_ty.into());
        self.builder.build_store(slot_ptr, set_handle).unwrap();
        self.variables.insert(
            var_name.to_string(),
            VarSlot {
                ptr: slot_ptr,
                ty: ptr_ty.into(),
            },
        );
        // Set handles use the same `karac_map_free` cleanup as Map handles —
        // the runtime is the same; only the type-system identity differs.
        // Sets have no value slot (val_size = 0 in the bucket layout), so the
        // value-side recursive drop never applies (`val_is_vec = false`).
        // The KEY side, however, is the element type — `Set[Vec[T]]` /
        // `Set[String]` need `key_is_vec = true` so the runtime helper
        // walks live entries and frees each key's data buffer before
        // deallocating the bucket storage (slice α of the recursive-drop
        // work, 2026-05-14). Primitive-element sets (`Set[i64]`) keep
        // `key_is_vec = false` and stay on plain `karac_map_free`.
        let key_is_vec = self.llvm_ty_is_vec_struct(elem_ty);
        // Sets have no value slot, so `Set[shared T]`'s shared-val
        // half is unreachable — pass `None` for the val-shared heap
        // type. The element T is the *key* of the underlying
        // `Map[T, ()]` bucket; `Set[shared T]` therefore lights up
        // the key-side walk via `map_key_shared_heap_type_for`
        // (which consults `set_elem_type_exprs` for Set bindings).
        let key_shared_heap = self.map_key_shared_heap_type_for(var_name);
        // Slice c-repl.B.5.3c: skip the scope-exit FreeMapHandle when
        // the let binding is destined for the cross-cell snapshot
        // global — same rationale as the Map.new() arm in
        // `compile_map_new_stmt`. The snapshot owns the handle's
        // lifetime; freeing at scope exit would leave the global
        // pointing at reclaimed memory. The slot still keeps the
        // handle so same-cell `s.insert(...)` / `s.contains(...)`
        // observe the live Set via direct slot reads.
        if !self.snapshot_capture.contains_key(var_name) {
            self.track_map_var(slot_ptr, key_is_vec, false, None, key_shared_heap);
        }
        // Record the surface type name so a bare tuple over a `Set.new()` var
        // (`let t = (d, i)`) can infer the Set leaf and register its Part A
        // drop — the Set sibling of the `compile_map_new_stmt` recording above.
        self.record_var_type_name(var_name.to_string(), "Set".to_string());
        Ok(())
    }

    /// Compile `let m: Map[K, V] = ["k1": v1, "k2": v2, ...]` (bare or prefix
    /// `Map[k1: v1, ...]` form — both lower to `ExprKind::MapLiteral`). Calls
    /// `compile_map_new_stmt` first to build the empty map + register the
    /// binding + cleanup tracking, then inserts each entry via
    /// `karac_map_insert_old` (discarding the previous-value out-slot since
    /// every key is fresh on construction).
    pub(super) fn compile_map_literal_stmt(
        &mut self,
        var_name: &str,
        entries: &[(Expr, Expr)],
    ) -> Result<(), String> {
        // Build the empty map first (registers slot + cleanup).
        self.compile_map_new_stmt(var_name)?;

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        let slot = self
            .variables
            .get(var_name)
            .copied()
            .ok_or_else(|| format!("compile_map_literal_stmt: '{var_name}' not registered"))?;
        let key_ty = self
            .map_key_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());
        let val_ty = self
            .map_val_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        // Reuse a single set of allocas across all inserts in the literal —
        // the storage is overwritten per iteration.
        let key_slot = self.create_entry_alloca(fn_val, "map.lit.key", key_ty);
        let val_slot = self.create_entry_alloca(fn_val, "map.lit.val", val_ty);
        let old_slot = self.create_entry_alloca(fn_val, "map.lit.old", val_ty);

        for (k_expr, v_expr) in entries {
            let map_handle = self
                .builder
                .build_load(ptr_ty, slot.ptr, "map.lit.handle")
                .unwrap()
                .into_pointer_value();
            let k_val = self.compile_expr(k_expr)?;
            let v_val = self.compile_expr(v_expr)?;
            // Move semantics for tracked Vec/String keys and values —
            // see `Map.insert` arm below for the rationale. Key
            // suppression added alongside the recursive key-drop path
            // (slice α/β, 2026-05-14).
            self.suppress_source_vec_cleanup_for_arg(k_expr);
            self.suppress_source_vec_cleanup_for_arg(v_expr);
            self.builder.build_store(key_slot, k_val).unwrap();
            self.builder.build_store(val_slot, v_val).unwrap();
            self.builder
                .build_call(
                    self.karac_map_insert_old_fn,
                    &[
                        map_handle.into(),
                        key_slot.into(),
                        val_slot.into(),
                        old_slot.into(),
                    ],
                    "map.lit.insert",
                )
                .unwrap();
        }

        Ok(())
    }

    /// Compile `m[k] = v` index-store on a `Map[K, V]` variable. Lowers to
    /// `karac_map_insert_old` and discards the previous-value out-slot. The
    /// write path is uniform regardless of whether the key already exists —
    /// `karac_map_insert_old` overwrites or fresh-inserts as appropriate.
    pub(super) fn compile_map_index_store(
        &mut self,
        name: &str,
        index: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // No `self.variables` precheck: `get_data_ptr` below gates
        // existence and resolves module-binding globals too (a
        // module-scope `Map.new()` handle lives in a global, not
        // `self.variables`).
        // Use `get_data_ptr` so `mut ref Map[K,V]` params unwrap one
        // ref-level before the handle load. Owned bindings yield
        // `slot.ptr` directly.
        let handle_ptr = self
            .get_data_ptr(name)
            .ok_or_else(|| format!("unknown map variable '{name}' in index-store"))?;
        let map_handle = self
            .builder
            .build_load(ptr_ty, handle_ptr, "map.idxst.handle")
            .unwrap()
            .into_pointer_value();

        let key_ty = self
            .map_key_types
            .get(name)
            .copied()
            .unwrap_or(i64_t.into());
        let val_ty = self
            .map_val_types
            .get(name)
            .copied()
            .unwrap_or(i64_t.into());

        let key_val = self.compile_expr(index)?;
        let fn_val = self.current_fn.unwrap();
        let key_slot = self.create_entry_alloca(fn_val, "map.idxst.key", key_ty);
        let val_slot = self.create_entry_alloca(fn_val, "map.idxst.val", val_ty);
        let old_slot = self.create_entry_alloca(fn_val, "map.idxst.old", val_ty);
        self.builder.build_store(key_slot, key_val).unwrap();
        self.builder.build_store(val_slot, val).unwrap();

        self.builder
            .build_call(
                self.karac_map_insert_old_fn,
                &[
                    map_handle.into(),
                    key_slot.into(),
                    val_slot.into(),
                    old_slot.into(),
                ],
                "map.idxst.existed",
            )
            .unwrap();

        Ok(())
    }

    /// Compile `m[k]` indexing on a `Map[K, V]` variable. Panics at runtime if
    /// the key is missing — matches the spec's `fn index(ref self, key: ref K)
    /// -> ref V` semantics. The returned value is a bit-copy of the bucket's V,
    /// not a borrow into the bucket; this matches the existing `Map.get`
    /// codegen behavior. Proper `ref V` return semantics is a follow-up that
    /// applies uniformly to both `[]` and `Map.get`.
    pub(super) fn compile_map_index(
        &mut self,
        name: &str,
        index: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // No `self.variables` precheck: `get_data_ptr` below gates
        // existence and resolves module-binding globals too.
        // Use `get_data_ptr` so `mut ref Map[K,V]` params unwrap one
        // ref-level before the handle load. Owned bindings yield
        // `slot.ptr` directly.
        let handle_ptr = self
            .get_data_ptr(name)
            .ok_or_else(|| format!("unknown map variable '{name}' in index expression"))?;
        let map_handle = self
            .builder
            .build_load(ptr_ty, handle_ptr, "map.idx.handle")
            .unwrap()
            .into_pointer_value();

        let key_ty = self
            .map_key_types
            .get(name)
            .copied()
            .unwrap_or(i64_t.into());
        let val_ty = self
            .map_val_types
            .get(name)
            .copied()
            .unwrap_or(i64_t.into());

        let key_val = self.compile_expr(index)?;
        let fn_val = self.current_fn.unwrap();
        let key_slot = self.create_entry_alloca(fn_val, "map.idx.key", key_ty);
        let val_slot = self.create_entry_alloca(fn_val, "map.idx.val", val_ty);
        self.builder.build_store(key_slot, key_val).unwrap();

        let found = self
            .builder
            .build_call(
                self.karac_map_get_fn,
                &[map_handle.into(), key_slot.into(), val_slot.into()],
                "map.idx.found",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        let found_bb = self.context.append_basic_block(fn_val, "map.idx.found");
        let notfound_bb = self.context.append_basic_block(fn_val, "map.idx.notfound");

        self.builder
            .build_conditional_branch(found, found_bb, notfound_bb)
            .unwrap();

        self.builder.position_at_end(notfound_bb);
        self.emit_panic("Map index: key not present");
        self.builder.build_unreachable().unwrap();

        self.builder.position_at_end(found_bb);
        let elem_val = self
            .builder
            .build_load(val_ty, val_slot, "map.idx.val")
            .unwrap();
        Ok(elem_val)
    }

    /// Compile `Map.keys()`, `Map.values()`, or `Map.entries()` — each
    /// materializes a fresh Vec by iterating the map. Pre-allocates the result
    /// buffer at `karac_map_len` capacity (matches Rust's reserve-then-fill
    /// pattern for known-size collections), then writes elements at index `i`
    /// via the iterator. Returns the resulting Vec struct value `{data, len,
    /// cap}` directly; the receiving binding's let-statement registers it for
    /// scope cleanup via the existing `vec_elem_types` machinery (the type
    /// annotation `let v: Vec[K] = m.keys()` drives that path).
    ///
    /// Iteration order is unspecified — matches the spec at design.md
    /// "Iteration order is unspecified" (line 1588).
    pub(super) fn compile_map_keys_values_entries(
        &mut self,
        var_name: &str,
        method: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let fn_val = self.current_fn.unwrap();

        // No `self.variables` precheck: `get_data_ptr` below gates
        // existence and resolves module-binding globals too.
        // Use `get_data_ptr` so `mut ref Map[K,V]` params unwrap one
        // ref-level before the handle load. Owned bindings yield
        // `slot.ptr` directly.
        let handle_ptr = self
            .get_data_ptr(var_name)
            .ok_or_else(|| format!("unknown map variable '{var_name}'"))?;
        let map_handle = self
            .builder
            .build_load(ptr_ty, handle_ptr, "kvg.map.handle")
            .unwrap()
            .into_pointer_value();

        let key_ty = self
            .map_key_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());
        let val_ty = self
            .map_val_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        // Resulting Vec's element type depends on which method we're emitting.
        // For `entries`, the element is the {K, V} tuple struct — same shape
        // as `extract_vec_elem_type` produces for `Vec[(K, V)]`.
        let elem_ty: BasicTypeEnum<'ctx> = match method {
            "keys" => key_ty,
            "values" => val_ty,
            "entries" => self.context.struct_type(&[key_ty, val_ty], false).into(),
            _ => {
                return Err(format!(
                    "compile_map_keys_values_entries: unexpected method '{method}'"
                ))
            }
        };

        let elem_size = elem_ty.size_of().unwrap();

        // `SortedMap.keys()`/`values()`/`entries()` must emit in ASCENDING key
        // order. Walk the sorted-key buffer and look each value up by key
        // (`karac_map_get`), instead of the hash-order iterator below. `values`
        // in particular cannot be post-sorted (it carries no key), so ordering
        // has to happen at build time.
        if self.sorted_collection_vars.contains(var_name) {
            return self.compile_sorted_map_kvg(
                var_name, method, map_handle, key_ty, val_ty, elem_ty, elem_size,
            );
        }

        // len = karac_map_len(map)
        let len = self
            .builder
            .build_call(self.karac_map_len_fn, &[map_handle.into()], "kvg.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();

        // Allocate buffer: malloc(len * elem_size). On len == 0 this calls
        // malloc(0) — implementation-defined; the resulting Vec carries cap=0
        // so scope cleanup never frees it (the bytes leak only on empty maps,
        // a pre-existing pattern shared with empty Vec literals).
        let alloc_bytes = self
            .builder
            .build_int_mul(len, elem_size, "kvg.alloc.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "kvg.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Map iterator + per-iteration out-slots.
        let iter_ptr = self
            .builder
            .build_call(self.karac_map_iter_new_fn, &[map_handle.into()], "kvg.iter")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let out_key = self.create_entry_alloca(fn_val, "kvg.out.k", key_ty);
        let out_val = self.create_entry_alloca(fn_val, "kvg.out.v", val_ty);
        let i_slot = self.create_entry_alloca(fn_val, "kvg.i", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_int(0, false))
            .unwrap();

        let loop_bb = self.context.append_basic_block(fn_val, "kvg.loop");
        let body_bb = self.context.append_basic_block(fn_val, "kvg.body");
        let exit_bb = self.context.append_basic_block(fn_val, "kvg.exit");

        self.builder.build_unconditional_branch(loop_bb).unwrap();

        // loop_bb: advance iterator; branch on result.
        self.builder.position_at_end(loop_bb);
        let has_next = self
            .builder
            .build_call(
                self.karac_map_iter_next_fn,
                &[iter_ptr.into(), out_key.into(), out_val.into()],
                "kvg.next",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder
            .build_conditional_branch(has_next, body_bb, exit_bb)
            .unwrap();

        // body_bb: deep-clone each key/val out-slot into buf[i], increment i.
        //
        // keys()/values()/entries() return an OWNED `Vec[K]` / `Vec[V]` /
        // `Vec[(K,V)]`, so a heap (String/Vec/struct) half must be DEEP-CLONED
        // into the result buffer: a shallow `{ptr,len,cap}` load+store would
        // alias the map's stored buffer, and the result Vec's scope-exit drop
        // would then free the same allocation as the map's drop — a double-free
        // that crashed `Map[String,_].keys()` (B-2026-06-20-10) and mirrors the
        // `get_or` owned-copy contract. `emit_clone_fn_for_type_expr` deep-
        // clones String/Vec/struct, memcpys scalars, and pointer-copies shared
        // (RC); when a half's K/V TypeExpr side-table entry is absent (an
        // inferred map with no recorded TypeExpr) we fall back to the shallow
        // load+store, the prior behavior — correct for scalars, the only
        // regression-free option without the type.
        self.builder.position_at_end(body_bb);
        let key_te = self.map_key_type_exprs.get(var_name).cloned();
        let val_te = self.var_elem_type_exprs.get(var_name).cloned();
        // Emit (cached) clone fns first — `emit_clone_fn_for_type_expr` may move
        // the builder into the synthesized fn, so re-assert `body_bb` after.
        let key_clone = key_te
            .as_ref()
            .map(|te| self.emit_clone_fn_for_type_expr(te));
        let val_clone = val_te
            .as_ref()
            .map(|te| self.emit_clone_fn_for_type_expr(te));
        self.builder.position_at_end(body_bb);
        let i_val = self
            .builder
            .build_load(i64_t, i_slot, "kvg.i.cur")
            .unwrap()
            .into_int_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, buf, &[i_val], "kvg.elem.ptr")
                .unwrap()
        };
        match method {
            "keys" => self.kvg_emit_half(key_clone, key_ty, out_key, elem_ptr, "kvg.k"),
            "values" => self.kvg_emit_half(val_clone, val_ty, out_val, elem_ptr, "kvg.v"),
            "entries" => {
                let kv_struct_ty = self.context.struct_type(&[key_ty, val_ty], false);
                let k_dst = self
                    .builder
                    .build_struct_gep(kv_struct_ty, elem_ptr, 0, "kvg.kv.k")
                    .unwrap();
                let v_dst = self
                    .builder
                    .build_struct_gep(kv_struct_ty, elem_ptr, 1, "kvg.kv.v")
                    .unwrap();
                self.kvg_emit_half(key_clone, key_ty, out_key, k_dst, "kvg.kv.k");
                self.kvg_emit_half(val_clone, val_ty, out_val, v_dst, "kvg.kv.v");
            }
            _ => unreachable!(),
        }
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "kvg.i.next")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        // exit_bb: free iterator, build Vec struct {data, len, cap=len}.
        self.builder.position_at_end(exit_bb);
        self.builder
            .build_call(self.karac_map_iter_free_fn, &[iter_ptr.into()], "")
            .unwrap();

        let mut vec_val = vec_ty.get_undef();
        vec_val = self
            .builder
            .build_insert_value(vec_val, buf, 0, "vec.data")
            .unwrap()
            .into_struct_value();
        vec_val = self
            .builder
            .build_insert_value(vec_val, len, 1, "vec.len")
            .unwrap()
            .into_struct_value();
        vec_val = self
            .builder
            .build_insert_value(vec_val, len, 2, "vec.cap")
            .unwrap()
            .into_struct_value();

        Ok(vec_val.into())
    }

    /// Ordered `SortedMap.keys()` / `values()` / `entries()` — the sorted
    /// sibling of the hash-order path in `compile_map_keys_values_entries`.
    /// Materializes the ascending-sorted keys (`karac_map_sorted_keys`) and
    /// walks them by index; `values` / `entries` look each value up by its key
    /// (`karac_map_get`, which always hits since the key came from the map).
    /// Every half is DEEP-CLONED into the result buffer (the same owned-`Vec`
    /// contract as the hash path, so a `String`/`Vec` half never aliases the
    /// map's stored buffer). The sorted-key scratch buffer is freed at exit.
    #[allow(clippy::too_many_arguments)]
    fn compile_sorted_map_kvg(
        &mut self,
        var_name: &str,
        method: &str,
        map_handle: PointerValue<'ctx>,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_size: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();
        let fn_val = self.current_fn.unwrap();

        let key_te = self
            .map_key_type_exprs
            .get(var_name)
            .cloned()
            .ok_or_else(|| format!("SortedMap.{method}: unknown key type for '{var_name}'"))?;
        let val_te = self.var_elem_type_exprs.get(var_name).cloned();

        // Emit (cached) clone fns before creating blocks — the emitter may move
        // the builder into the synthesized fn.
        let key_clone = Some(self.emit_clone_fn_for_type_expr(&key_te));
        let val_clone = val_te
            .as_ref()
            .map(|te| self.emit_clone_fn_for_type_expr(te));

        let (kbuf, len) = self.emit_sorted_keys_buf(map_handle, &key_te)?;
        let alloc_bytes = self
            .builder
            .build_int_mul(len, elem_size, "smkvg.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "smkvg.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let out_val = self.create_entry_alloca(fn_val, "smkvg.outv", val_ty);
        let i_slot = self.create_entry_alloca(fn_val, "smkvg.i", i64_t.into());
        self.builder
            .build_store(i_slot, i64_t.const_zero())
            .unwrap();

        let loop_bb = self.context.append_basic_block(fn_val, "smkvg.loop");
        let body_bb = self.context.append_basic_block(fn_val, "smkvg.body");
        let exit_bb = self.context.append_basic_block(fn_val, "smkvg.exit");
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.builder.position_at_end(loop_bb);
        let i_val = self
            .builder
            .build_load(i64_t, i_slot, "smkvg.i.cur")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i_val, len, "smkvg.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let kptr = unsafe {
            self.builder
                .build_gep(key_ty, kbuf, &[i_val], "smkvg.kptr")
                .unwrap()
        };
        let elem_ptr = unsafe {
            self.builder
                .build_gep(elem_ty, buf, &[i_val], "smkvg.eptr")
                .unwrap()
        };
        match method {
            "keys" => self.kvg_emit_half(key_clone, key_ty, kptr, elem_ptr, "smkvg.k"),
            "values" => {
                self.builder
                    .build_call(
                        self.karac_map_get_fn,
                        &[map_handle.into(), kptr.into(), out_val.into()],
                        "smkvg.get",
                    )
                    .unwrap();
                self.kvg_emit_half(val_clone, val_ty, out_val, elem_ptr, "smkvg.v");
            }
            "entries" => {
                let kv_struct_ty = self.context.struct_type(&[key_ty, val_ty], false);
                let k_dst = self
                    .builder
                    .build_struct_gep(kv_struct_ty, elem_ptr, 0, "smkvg.kv.k")
                    .unwrap();
                let v_dst = self
                    .builder
                    .build_struct_gep(kv_struct_ty, elem_ptr, 1, "smkvg.kv.v")
                    .unwrap();
                self.kvg_emit_half(key_clone, key_ty, kptr, k_dst, "smkvg.kv.kk");
                self.builder
                    .build_call(
                        self.karac_map_get_fn,
                        &[map_handle.into(), kptr.into(), out_val.into()],
                        "smkvg.kv.get",
                    )
                    .unwrap();
                self.kvg_emit_half(val_clone, val_ty, out_val, v_dst, "smkvg.kv.vv");
            }
            _ => {
                return Err(format!(
                    "compile_sorted_map_kvg: unexpected method '{method}'"
                ))
            }
        }
        let i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "smkvg.i.next")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        // `free(NULL)` is a no-op (empty map → null buffer).
        self.builder
            .build_call(self.free_fn, &[kbuf.into()], "")
            .unwrap();
        let mut vec_val = vec_ty.get_undef();
        vec_val = self
            .builder
            .build_insert_value(vec_val, buf, 0, "smkvg.vec.data")
            .unwrap()
            .into_struct_value();
        vec_val = self
            .builder
            .build_insert_value(vec_val, len, 1, "smkvg.vec.len")
            .unwrap()
            .into_struct_value();
        vec_val = self
            .builder
            .build_insert_value(vec_val, len, 2, "smkvg.vec.cap")
            .unwrap()
            .into_struct_value();
        Ok(vec_val.into())
    }

    /// `keys`/`values`/`entries` body helper: write one key or value half from
    /// the iterator out-slot `src` into the result slot `dst`. Deep-clones via
    /// `clone_fn` when the half's TypeExpr was available (the owned-Vec
    /// contract — the result never aliases the map's stored buffer); otherwise
    /// a shallow load+store (correct for scalars; the no-TypeExpr fallback).
    fn kvg_emit_half(
        &mut self,
        clone_fn: Option<FunctionValue<'ctx>>,
        ty: BasicTypeEnum<'ctx>,
        src: PointerValue<'ctx>,
        dst: PointerValue<'ctx>,
        name: &str,
    ) {
        if let Some(cf) = clone_fn {
            self.builder
                .build_call(cf, &[src.into(), dst.into()], name)
                .unwrap();
        } else {
            let v = self.builder.build_load(ty, src, name).unwrap();
            self.builder.build_store(dst, v).unwrap();
        }
    }

    /// `SortedMap.min()` / `max()` / `floor(k)` / `ceiling(k)` — the ordered
    /// single-entry lookups, returning `Option[(K,V)]` (B-2026-07-18-1). Built
    /// on the sorted-keys buffer (`emit_sorted_keys_buf`): pick the target index
    /// (0 for `min`, len-1 for `max`, a comparator scan for `floor`/`ceiling`),
    /// then deep-clone that key + its looked-up value into a `(K,V)` tuple
    /// wrapped in `Some`, or `None` when the map is empty / no key satisfies the
    /// bound. `min`/`max` need no argument; `floor`/`ceiling` take the pivot key.
    /// Only integer/String keys sort under codegen (via `emit_sorted_key_cmp_fn`,
    /// which rejects other `Ord` key types with an actionable message).
    #[allow(clippy::too_many_arguments)]
    fn compile_sorted_map_option_lookup(
        &mut self,
        var_name: &str,
        method: &str,
        map_handle: PointerValue<'ctx>,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let fn_val = self.current_fn.unwrap();

        let key_te = self
            .map_key_type_exprs
            .get(var_name)
            .cloned()
            .ok_or_else(|| format!("SortedMap.{method}: unknown key type for '{var_name}'"))?;
        let val_te = self.var_elem_type_exprs.get(var_name).cloned();
        let key_clone = self.emit_clone_fn_for_type_expr(&key_te);
        let val_clone = val_te
            .as_ref()
            .map(|te| self.emit_clone_fn_for_type_expr(te));
        // Emits (and validates support for) the ascending key comparator; used
        // by floor/ceiling and required to have run before the sorted-keys call.
        let cmp_fn = self.emit_sorted_key_cmp_fn(&key_te)?;

        // floor/ceiling take a pivot key argument; store it in an alloca so the
        // comparator (which takes key POINTERS) can read it.
        let needs_arg = matches!(method, "floor" | "ceiling");
        let arg_pivot = if needs_arg {
            let arg = args
                .first()
                .ok_or_else(|| format!("SortedMap.{method} expects 1 argument"))?;
            let av = self.compile_expr(&arg.value)?;
            let av = self.coerce_scalar_to_type(av, key_ty);
            let slot = self.create_entry_alloca(fn_val, "smol.argk", key_ty);
            self.builder.build_store(slot, av).unwrap();
            Some((slot, &arg.value, av))
        } else {
            None
        };

        let (kbuf, len) = self.emit_sorted_keys_buf(map_handle, &key_te)?;

        // Compute (found, idx) into allocas. min/max are direct; floor/ceiling
        // scan the ascending buffer.
        let found_slot = self.create_entry_alloca(fn_val, "smol.found", bool_t.into());
        let idx_slot = self.create_entry_alloca(fn_val, "smol.idx", i64_t.into());
        let zero = i64_t.const_zero();
        let one = i64_t.const_int(1, false);
        let len_pos = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGT, len, zero, "smol.nonempty")
            .unwrap();
        match method {
            "min" => {
                self.builder.build_store(found_slot, len_pos).unwrap();
                self.builder.build_store(idx_slot, zero).unwrap();
            }
            "max" => {
                self.builder.build_store(found_slot, len_pos).unwrap();
                let last = self.builder.build_int_sub(len, one, "smol.last").unwrap();
                self.builder.build_store(idx_slot, last).unwrap();
            }
            "floor" | "ceiling" => {
                // Scan i = 0..len comparing kbuf[i] to the pivot. floor keeps the
                // LAST key <= pivot (largest, since ascending); ceiling keeps the
                // FIRST key >= pivot (smallest). Conditional stores via select.
                self.builder
                    .build_store(found_slot, bool_t.const_zero())
                    .unwrap();
                self.builder.build_store(idx_slot, zero).unwrap();
                let (pivot_slot, _, _) = arg_pivot.as_ref().unwrap();
                let is_floor = method == "floor";
                let i_slot = self.create_entry_alloca(fn_val, "smol.i", i64_t.into());
                self.builder.build_store(i_slot, zero).unwrap();
                let loop_bb = self.context.append_basic_block(fn_val, "smol.loop");
                let body_bb = self.context.append_basic_block(fn_val, "smol.body");
                let cont_bb = self.context.append_basic_block(fn_val, "smol.cont");
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(loop_bb);
                let i_cur = self
                    .builder
                    .build_load(i64_t, i_slot, "smol.i.cur")
                    .unwrap()
                    .into_int_value();
                let more = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::SLT, i_cur, len, "smol.more")
                    .unwrap();
                self.builder
                    .build_conditional_branch(more, body_bb, cont_bb)
                    .unwrap();
                self.builder.position_at_end(body_bb);
                let kptr = unsafe {
                    self.builder
                        .build_gep(key_ty, kbuf, &[i_cur], "smol.kptr")
                        .unwrap()
                };
                let c = self
                    .builder
                    .build_call(cmp_fn, &[kptr.into(), (*pivot_slot).into()], "smol.cmp")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let i32z = self.context.i32_type().const_zero();
                let cur_found = self
                    .builder
                    .build_load(bool_t, found_slot, "smol.f.cur")
                    .unwrap()
                    .into_int_value();
                // in-range: floor → cmp <= 0; ceiling → cmp >= 0.
                let pred = if is_floor {
                    inkwell::IntPredicate::SLE
                } else {
                    inkwell::IntPredicate::SGE
                };
                let in_range = self
                    .builder
                    .build_int_compare(pred, c, i32z, "smol.inrange")
                    .unwrap();
                // floor: take on every in-range i (keeps the last/largest).
                // ceiling: take only the FIRST in-range i (found still false).
                let take = if is_floor {
                    in_range
                } else {
                    let not_found = self.builder.build_not(cur_found, "smol.notfound").unwrap();
                    self.builder
                        .build_and(in_range, not_found, "smol.take")
                        .unwrap()
                };
                let new_found = self
                    .builder
                    .build_or(cur_found, take, "smol.f.new")
                    .unwrap();
                self.builder.build_store(found_slot, new_found).unwrap();
                let cur_idx = self
                    .builder
                    .build_load(i64_t, idx_slot, "smol.idx.cur")
                    .unwrap()
                    .into_int_value();
                let new_idx = self
                    .builder
                    .build_select(take, i_cur, cur_idx, "smol.idx.new")
                    .unwrap()
                    .into_int_value();
                self.builder.build_store(idx_slot, new_idx).unwrap();
                let i_next = self
                    .builder
                    .build_int_add(i_cur, one, "smol.i.next")
                    .unwrap();
                self.builder.build_store(i_slot, i_next).unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(cont_bb);
            }
            _ => {
                return Err(format!(
                    "compile_sorted_map_option_lookup: bad method '{method}'"
                ))
            }
        }

        // Free the fresh-owned pivot arg (floor/ceiling) — a lookup arg, never
        // stored (no-op for a borrowed/literal/scalar key).
        if let Some((_, arg_expr, av)) = arg_pivot {
            self.free_fresh_owned_str_arg(arg_expr, av);
        }

        let found = self
            .builder
            .build_load(bool_t, found_slot, "smol.found.v")
            .unwrap()
            .into_int_value();
        let some_bb = self.context.append_basic_block(fn_val, "smol.some");
        let none_bb = self.context.append_basic_block(fn_val, "smol.none");
        let merge_bb = self.context.append_basic_block(fn_val, "smol.merge");
        self.builder
            .build_conditional_branch(found, some_bb, none_bb)
            .unwrap();

        // Some: clone kbuf[idx] + its value into a fresh (K,V) tuple.
        self.builder.position_at_end(some_bb);
        let idx = self
            .builder
            .build_load(i64_t, idx_slot, "smol.idx.f")
            .unwrap()
            .into_int_value();
        let kptr = unsafe {
            self.builder
                .build_gep(key_ty, kbuf, &[idx], "smol.f.kptr")
                .unwrap()
        };
        let k_slot = self.create_entry_alloca(fn_val, "smol.k", key_ty);
        self.kvg_emit_half(Some(key_clone), key_ty, kptr, k_slot, "smol.k.clone");
        let raw_val = self.create_entry_alloca(fn_val, "smol.rawv", val_ty);
        self.builder
            .build_call(
                self.karac_map_get_fn,
                &[map_handle.into(), kptr.into(), raw_val.into()],
                "smol.get",
            )
            .unwrap();
        let v_slot = self.create_entry_alloca(fn_val, "smol.v", val_ty);
        self.kvg_emit_half(val_clone, val_ty, raw_val, v_slot, "smol.v.clone");
        let kv_struct_ty = self.context.struct_type(&[key_ty, val_ty], false);
        let k_v = self.builder.build_load(key_ty, k_slot, "smol.k.v").unwrap();
        let v_v = self.builder.build_load(val_ty, v_slot, "smol.v.v").unwrap();
        let mut tuple = kv_struct_ty.get_undef();
        tuple = self
            .builder
            .build_insert_value(tuple, k_v, 0, "smol.kv.k")
            .unwrap()
            .into_struct_value();
        tuple = self
            .builder
            .build_insert_value(tuple, v_v, 1, "smol.kv.v")
            .unwrap()
            .into_struct_value();
        let words = self.coerce_to_payload_words(tuple.into(), 3)?;
        let some_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(none_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
        let opt = self.build_option_some_via_phis(&words, some_end_bb, none_bb, "smol.opt");
        // `free(NULL)` is a no-op (empty map → null buffer).
        self.builder
            .build_call(self.free_fn, &[kbuf.into()], "")
            .unwrap();
        Ok(opt)
    }

    /// `SortedMap.range(lo, hi)` — the INCLUSIVE `[lo, hi]` sub-range as a fresh
    /// `Vec[(K,V)]` in ascending key order (B-2026-07-18-1). Scans the sorted
    /// keys, keeps those with `lo <= key <= hi` (via the ascending comparator),
    /// and deep-clones each matching key + its looked-up value into the result.
    /// Over-allocates to `len` entries (upper bound) and reports the matched
    /// `count` as the Vec's len/cap. Integer/String keys only (as `min`/etc.).
    fn compile_sorted_map_range(
        &mut self,
        var_name: &str,
        map_handle: PointerValue<'ctx>,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let vec_ty = self.vec_struct_type();
        let fn_val = self.current_fn.unwrap();

        let key_te = self
            .map_key_type_exprs
            .get(var_name)
            .cloned()
            .ok_or_else(|| format!("SortedMap.range: unknown key type for '{var_name}'"))?;
        let val_te = self.var_elem_type_exprs.get(var_name).cloned();
        let key_clone = self.emit_clone_fn_for_type_expr(&key_te);
        let val_clone = val_te
            .as_ref()
            .map(|te| self.emit_clone_fn_for_type_expr(te));
        let cmp_fn = self.emit_sorted_key_cmp_fn(&key_te)?;

        // Compile lo/hi pivots into allocas (comparator takes key pointers).
        let lo_arg = args
            .first()
            .ok_or_else(|| "SortedMap.range expects 2 arguments".to_string())?;
        let hi_arg = args
            .get(1)
            .ok_or_else(|| "SortedMap.range expects 2 arguments".to_string())?;
        let lo_v = self.compile_expr(&lo_arg.value)?;
        let lo_v = self.coerce_scalar_to_type(lo_v, key_ty);
        let lo_slot = self.create_entry_alloca(fn_val, "smr.lo", key_ty);
        self.builder.build_store(lo_slot, lo_v).unwrap();
        let hi_v = self.compile_expr(&hi_arg.value)?;
        let hi_v = self.coerce_scalar_to_type(hi_v, key_ty);
        let hi_slot = self.create_entry_alloca(fn_val, "smr.hi", key_ty);
        self.builder.build_store(hi_slot, hi_v).unwrap();

        let kv_struct_ty = self.context.struct_type(&[key_ty, val_ty], false);
        let elem_size = kv_struct_ty.size_of().unwrap();
        let (kbuf, len) = self.emit_sorted_keys_buf(map_handle, &key_te)?;
        let alloc_bytes = self
            .builder
            .build_int_mul(len, elem_size, "smr.bytes")
            .unwrap();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "smr.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let raw_val = self.create_entry_alloca(fn_val, "smr.rawv", val_ty);
        let count_slot = self.create_entry_alloca(fn_val, "smr.count", i64_t.into());
        let i_slot = self.create_entry_alloca(fn_val, "smr.i", i64_t.into());
        let zero = i64_t.const_zero();
        let one = i64_t.const_int(1, false);
        let i32z = self.context.i32_type().const_zero();
        self.builder.build_store(count_slot, zero).unwrap();
        self.builder.build_store(i_slot, zero).unwrap();

        let loop_bb = self.context.append_basic_block(fn_val, "smr.loop");
        let body_bb = self.context.append_basic_block(fn_val, "smr.body");
        let keep_bb = self.context.append_basic_block(fn_val, "smr.keep");
        let next_bb = self.context.append_basic_block(fn_val, "smr.next");
        let exit_bb = self.context.append_basic_block(fn_val, "smr.exit");
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.builder.position_at_end(loop_bb);
        let i_cur = self
            .builder
            .build_load(i64_t, i_slot, "smr.i.cur")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLT, i_cur, len, "smr.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let kptr = unsafe {
            self.builder
                .build_gep(key_ty, kbuf, &[i_cur], "smr.kptr")
                .unwrap()
        };
        let c_lo = self
            .builder
            .build_call(cmp_fn, &[kptr.into(), lo_slot.into()], "smr.clo")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let c_hi = self
            .builder
            .build_call(cmp_fn, &[kptr.into(), hi_slot.into()], "smr.chi")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let ge_lo = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SGE, c_lo, i32z, "smr.gelo")
            .unwrap();
        let le_hi = self
            .builder
            .build_int_compare(inkwell::IntPredicate::SLE, c_hi, i32z, "smr.lehi")
            .unwrap();
        let in_range = self.builder.build_and(ge_lo, le_hi, "smr.inr").unwrap();
        self.builder
            .build_conditional_branch(in_range, keep_bb, next_bb)
            .unwrap();

        // keep_bb: clone (key, value) into buf[count]; count++.
        self.builder.position_at_end(keep_bb);
        let count_cur = self
            .builder
            .build_load(i64_t, count_slot, "smr.count.cur")
            .unwrap()
            .into_int_value();
        let elem_ptr = unsafe {
            self.builder
                .build_gep(kv_struct_ty, buf, &[count_cur], "smr.eptr")
                .unwrap()
        };
        let k_dst = self
            .builder
            .build_struct_gep(kv_struct_ty, elem_ptr, 0, "smr.kv.k")
            .unwrap();
        let v_dst = self
            .builder
            .build_struct_gep(kv_struct_ty, elem_ptr, 1, "smr.kv.v")
            .unwrap();
        self.kvg_emit_half(Some(key_clone), key_ty, kptr, k_dst, "smr.k.clone");
        self.builder
            .build_call(
                self.karac_map_get_fn,
                &[map_handle.into(), kptr.into(), raw_val.into()],
                "smr.get",
            )
            .unwrap();
        self.kvg_emit_half(val_clone, val_ty, raw_val, v_dst, "smr.v.clone");
        let count_next = self
            .builder
            .build_int_add(count_cur, one, "smr.count.next")
            .unwrap();
        self.builder.build_store(count_slot, count_next).unwrap();
        self.builder.build_unconditional_branch(next_bb).unwrap();

        self.builder.position_at_end(next_bb);
        let i_next = self
            .builder
            .build_int_add(i_cur, one, "smr.i.next")
            .unwrap();
        self.builder.build_store(i_slot, i_next).unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        self.builder
            .build_call(self.free_fn, &[kbuf.into()], "")
            .unwrap();
        // Free fresh-owned lo/hi pivots (lookup args, never stored).
        self.free_fresh_owned_str_arg(&lo_arg.value, lo_v);
        self.free_fresh_owned_str_arg(&hi_arg.value, hi_v);
        let count = self
            .builder
            .build_load(i64_t, count_slot, "smr.count.f")
            .unwrap()
            .into_int_value();
        let mut vec_val = vec_ty.get_undef();
        vec_val = self
            .builder
            .build_insert_value(vec_val, buf, 0, "smr.vec.data")
            .unwrap()
            .into_struct_value();
        vec_val = self
            .builder
            .build_insert_value(vec_val, count, 1, "smr.vec.len")
            .unwrap()
            .into_struct_value();
        vec_val = self
            .builder
            .build_insert_value(vec_val, count, 2, "smr.vec.cap")
            .unwrap()
            .into_struct_value();
        Ok(vec_val.into())
    }

    /// Compile a method call on a `Map[K,V]` variable.
    pub(super) fn compile_map_method(
        &mut self,
        var_name: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // No `self.variables` precheck: `get_data_ptr` below gates
        // existence and resolves module-binding globals too.

        // Load the opaque map handle. `get_data_ptr` returns the alloca
        // for owned Map params/locals (alloca holds the handle), or the
        // caller's alloca address for `ref Map` / `mut ref Map` params
        // (alloca holds a `*Map`). The single subsequent load yields the
        // opaque handle in both cases — owned reads through one level,
        // ref reads through two levels, with the first level already
        // performed inside `get_data_ptr`.
        let handle_ptr = self
            .get_data_ptr(var_name)
            .ok_or_else(|| format!("unknown map variable '{var_name}'"))?;
        let map_handle = self
            .builder
            .build_load(ptr_ty, handle_ptr, "map.handle")
            .unwrap()
            .into_pointer_value();

        let key_ty = self
            .map_key_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());
        let val_ty = self
            .map_val_types
            .get(var_name)
            .copied()
            .unwrap_or(i64_t.into());

        match method {
            "len" => {
                // Slice 1a: route Map[i64, i64].len through the
                // monomorphized symbol family; every other K/V tuple
                // stays on the erased fallback per § 3.6 coexist.
                let len_fn = if self.should_use_mono_map_for(key_ty, val_ty) {
                    self.get_or_emit_map_mono_methods(key_ty, val_ty).len_fn
                } else {
                    self.karac_map_len_fn
                };
                let len = self
                    .builder
                    .build_call(len_fn, &[map_handle.into()], "map.len")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic();
                Ok(len)
            }
            "is_empty" => {
                let len = self
                    .builder
                    .build_call(self.karac_map_len_fn, &[map_handle.into()], "map.len")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let zero = i64_t.const_int(0, false);
                let cmp = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::EQ, len, zero, "map.is_empty")
                    .unwrap();
                Ok(cmp.into())
            }
            "try_insert" => {
                // Fallible-allocation companion (phase-8-stdlib-floor item 8):
                // `Map.try_insert(k, v) -> Result[Option[V], AllocError]`. Uses
                // the generic pointer runtime path (`karac_map_try_insert`) for
                // all maps — mono and borrowed-key are perf fast paths
                // orthogonal to fallibility, so they are intentionally not
                // mirrored here. Success reuses the panicking `insert` arm's
                // `Option[V]` construction and wraps it in `Result.Ok`; an OOM
                // during growth (status 2) becomes `Result.Err(AllocError.
                // OutOfMemory{bytes})` with the map left unchanged.
                if args.len() < 2 {
                    return Err("Map.try_insert requires key and value arguments".to_string());
                }
                let fn_val = self.current_fn.unwrap();
                // Owned-path key/value compilation with the SAME move semantics
                // as `insert` (defensive-copy owned params, disarm f-string
                // accumulators + source scope-exit frees so the map is the
                // unique owner of any adopted buffer).
                let key_val = self.compile_expr(&args[0].value)?;
                self.suppress_fstr_acc_if_moved_out(&args[0].value);
                let key_val = self.maybe_defensive_copy_param_arg(&args[0].value, key_val);
                let val_val = self.compile_expr(&args[1].value)?;
                self.suppress_fstr_acc_if_moved_out(&args[1].value);
                let val_val = self.maybe_defensive_copy_param_arg(&args[1].value, val_val);
                self.suppress_source_vec_cleanup_for_arg(&args[0].value);
                self.suppress_source_vec_cleanup_for_arg(&args[1].value);
                self.suppress_boxed_enum_payload_cleanup_for_moved_arg(&args[1].value);
                self.suppress_inline_option_payload_cleanup_for_moved_arg(&args[1].value);
                self.suppress_inline_result_payload_cleanup_for_moved_arg(&args[1].value);

                let key_slot = self.create_entry_alloca(fn_val, "map.try.key", key_ty);
                let val_slot = self.create_entry_alloca(fn_val, "map.try.val", val_ty);
                let old_slot = self.create_entry_alloca(fn_val, "map.try.old", val_ty);
                let bytes_slot = self.create_entry_alloca(fn_val, "map.try.bytes", i64_t.into());
                self.builder.build_store(key_slot, key_val).unwrap();
                self.builder.build_store(val_slot, val_val).unwrap();

                let i32_t = self.context.i32_type();
                let status = self
                    .builder
                    .build_call(
                        self.karac_map_try_insert_fn,
                        &[
                            map_handle.into(),
                            key_slot.into(),
                            val_slot.into(),
                            old_slot.into(),
                            bytes_slot.into(),
                        ],
                        "map.try.status",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();

                let oom_bb = self.context.append_basic_block(fn_val, "map.try.oom");
                let ok_bb = self.context.append_basic_block(fn_val, "map.try.ok");
                let done_bb = self.context.append_basic_block(fn_val, "map.try.done");
                let is_oom = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        status,
                        i32_t.const_int(2, false),
                        "map.try.is_oom",
                    )
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_oom, oom_bb, ok_bb)
                    .unwrap();

                // OOM: Result.Err(AllocError.OutOfMemory{bytes}). The map is
                // unchanged, so any incoming owned key/value buffers we
                // deep-copied are orphaned — free them here (the same
                // duplicate-key leak the `insert` arm guards, extended to the
                // value since nothing was stored).
                self.builder.position_at_end(oom_bb);
                let bytes = self
                    .builder
                    .build_load(i64_t, bytes_slot, "map.try.bytes.v")
                    .unwrap()
                    .into_int_value();
                self.free_str_vec_buffer_if_heap(key_val);
                self.free_str_vec_buffer_if_heap(val_val);
                let err_result = self.build_alloc_oom_result(bytes)?;
                let oom_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(done_bb).unwrap();

                // Success: build Option[V] (Some(old) if updated, None if
                // fresh) and wrap in Result.Ok.
                self.builder.position_at_end(ok_bb);
                let existed = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::EQ,
                        status,
                        i32_t.const_int(1, false),
                        "map.try.existed",
                    )
                    .unwrap();
                let some_bb = self.context.append_basic_block(fn_val, "map.try.some");
                let none_bb = self.context.append_basic_block(fn_val, "map.try.none");
                let opt_merge_bb = self.context.append_basic_block(fn_val, "map.try.optmerge");
                self.builder
                    .build_conditional_branch(existed, some_bb, none_bb)
                    .unwrap();
                // Updated an existing key: the map kept its stored key and did
                // NOT adopt the incoming one, so free the deep-copied key buffer
                // (duplicate-key leak, B-2026-06-20-9 sibling).
                self.builder.position_at_end(some_bb);
                self.free_str_vec_buffer_if_heap(key_val);
                let old_val = self
                    .builder
                    .build_load(val_ty, old_slot, "map.try.oldv")
                    .unwrap();
                let some_payload_words = self.coerce_to_payload_words(old_val, 3)?;
                let some_end_bb = self.builder.get_insert_block().unwrap();
                self.builder
                    .build_unconditional_branch(opt_merge_bb)
                    .unwrap();
                self.builder.position_at_end(none_bb);
                self.builder
                    .build_unconditional_branch(opt_merge_bb)
                    .unwrap();
                self.builder.position_at_end(opt_merge_bb);
                let opt_agg = self.build_option_some_via_phis(
                    &some_payload_words,
                    some_end_bb,
                    none_bb,
                    "map.try.opt",
                );
                let ok_result = self.build_nonshared_enum_value("Result", "Ok", &[opt_agg])?;
                let ok_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(done_bb).unwrap();

                self.builder.position_at_end(done_bb);
                let phi = self
                    .builder
                    .build_phi(err_result.get_type(), "map.try.result")
                    .unwrap();
                phi.add_incoming(&[(&err_result, oom_end_bb), (&ok_result, ok_end_bb)]);
                Ok(phi.as_basic_value())
            }
            "insert" => {
                if args.len() < 2 {
                    return Err("Map.insert requires key and value arguments".to_string());
                }
                // Allocation-free String-slice key fast path: when the key is
                // a slice expression (`m.insert(s[a..b], v)`), pass a borrowed
                // `{ptr,len,cap=0}` view and let the runtime deep-copy it only
                // on a *fresh* insertion — existing keys cost zero allocation
                // (the common case in counter/window maps). Sound because the
                // map owns its copy; the borrowed pointer is never stored. The
                // slice is a temporary, so there is no key source to suppress.
                let borrowed_key = self.try_compile_borrowed_string_slice(&args[0].value)?;
                // Preserve key-before-val compile order on the owned path
                // (the borrowed path already compiled the sliced object above).
                let key_val = if borrowed_key.is_none() {
                    let kv = self.compile_expr(&args[0].value)?;
                    // Consume-site ownership pair, identical to `Vec.push`:
                    // an f-string key (`m.insert(f"…", v)`) moves its buffer
                    // in — disarm the staged accumulator's scope-exit free;
                    // an owned String/Vec PARAM key deep-copies — the Map
                    // takes ownership of a private copy while the caller
                    // retains the original buffer's free under the by-value
                    // header ABI (kata-22 owned-param UAF family). Applied
                    // immediately after compiling the key so a later
                    // f-string VALUE arg can't clobber the key's accumulator.
                    self.suppress_fstr_acc_if_moved_out(&args[0].value);
                    Some(self.maybe_defensive_copy_param_arg(&args[0].value, kv))
                } else {
                    None
                };
                let val_val = self.compile_expr(&args[1].value)?;
                // Same consume-site pair for the value argument.
                self.suppress_fstr_acc_if_moved_out(&args[1].value);
                let val_val = self.maybe_defensive_copy_param_arg(&args[1].value, val_val);
                // Move semantics — same shape as `Vec.push`. When the
                // key OR value argument is a tracked Vec/String binding,
                // the bucket bit-copies its `{ptr, len, cap}` and the
                // `karac_map_free_with_drop_vec` cleanup would
                // double-free the buffer against the source binding's
                // own scope-exit `FreeVecBuffer`. Suppress the source's
                // cleanup so the Map becomes the unique owner. (Skip the
                // key side on the borrowed path — nothing is moved in.)
                if borrowed_key.is_none() {
                    self.suppress_source_vec_cleanup_for_arg(&args[0].value);
                }
                self.suppress_source_vec_cleanup_for_arg(&args[1].value);
                // Slice 3u: a moved boxed-payload Option/Result binding
                // (`m.insert(k, o)` on a Map[K, Option[Wide]]) — null the
                // source's box word so its BoxedEnumDrop skips; the map's
                // per-value drop owns the box now.
                self.suppress_boxed_enum_payload_cleanup_for_moved_arg(&args[1].value);
                // Inline-payload siblings (the 3p/3q push-family pair, here
                // for the map VALUE argument).
                self.suppress_inline_option_payload_cleanup_for_moved_arg(&args[1].value);
                self.suppress_inline_result_payload_cleanup_for_moved_arg(&args[1].value);
                let fn_val = self.current_fn.unwrap();
                let old_slot = self.create_entry_alloca(fn_val, "map.insert.old", val_ty);
                let existed = if let Some(view) = borrowed_key {
                    let key_slot = self.create_entry_alloca(fn_val, "map.insert.bkey", key_ty);
                    let val_slot = self.create_entry_alloca(fn_val, "map.insert.bval", val_ty);
                    self.builder.build_store(key_slot, view).unwrap();
                    self.builder.build_store(val_slot, val_val).unwrap();
                    self.builder
                        .build_call(
                            self.karac_map_insert_borrowed_str_old_fn,
                            &[
                                map_handle.into(),
                                key_slot.into(),
                                val_slot.into(),
                                old_slot.into(),
                            ],
                            "map.insert.existed",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value()
                } else {
                    let key_val = key_val.unwrap();
                    // Slice 1b.2a — Map[i64, i64] inserts route through
                    // the mono `karac_map_i64_i64_insert_old` symbol (value
                    // calling convention: i64 key + i64 val rather than
                    // pointer args). Gate the *compiled value types* against
                    // the side-table-derived `key_ty` / `val_ty`; mono's
                    // value-pass convention is an LLVM verifier error on a
                    // mismatch, while the erased pointer path tolerates shape
                    // drift.
                    let key_val_matches = key_val.get_type() == key_ty;
                    let val_val_matches = val_val.get_type() == val_ty;
                    if self.should_use_mono_map_for(key_ty, val_ty)
                        && key_val_matches
                        && val_val_matches
                    {
                        let mono = self.get_or_emit_map_mono_methods(key_ty, val_ty);
                        self.builder
                            .build_call(
                                mono.insert_old_fn,
                                &[
                                    map_handle.into(),
                                    key_val.into(),
                                    val_val.into(),
                                    old_slot.into(),
                                ],
                                "map.insert.existed",
                            )
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value()
                    } else {
                        let key_slot = self.create_entry_alloca(fn_val, "map.insert.key", key_ty);
                        let val_slot = self.create_entry_alloca(fn_val, "map.insert.val", val_ty);
                        self.builder.build_store(key_slot, key_val).unwrap();
                        self.builder.build_store(val_slot, val_val).unwrap();
                        self.builder
                            .build_call(
                                self.karac_map_insert_old_fn,
                                &[
                                    map_handle.into(),
                                    key_slot.into(),
                                    val_slot.into(),
                                    old_slot.into(),
                                ],
                                "map.insert.existed",
                            )
                            .unwrap()
                            .try_as_basic_value()
                            .unwrap_basic()
                            .into_int_value()
                    }
                };
                // Build Option[V]: Some(old) if existed, None if fresh insert.
                let some_bb = self.context.append_basic_block(fn_val, "map.ins.some");
                let none_bb = self.context.append_basic_block(fn_val, "map.ins.none");
                let merge_bb = self.context.append_basic_block(fn_val, "map.ins.merge");
                self.builder
                    .build_conditional_branch(existed, some_bb, none_bb)
                    .unwrap();
                self.builder.position_at_end(some_bb);
                // Existed (no-adopt) path: `karac_map_insert_old` kept the
                // bucket's existing key and did NOT adopt the incoming one,
                // while the owned path above already suppressed the source's
                // scope-exit free — so the incoming key buffer is orphaned and
                // leaks (B-2026-06-20-9; LSan-only, one buffer per duplicate
                // key). Free it once here. `key_val` is `Some` only on the
                // owned path (the borrowed slice path never adopts and leaves
                // the caller's source intact); cap>0 / vec-struct guards no-op
                // on a rodata literal or scalar key.
                if let Some(kv) = key_val {
                    self.free_str_vec_buffer_if_heap(kv);
                }
                let old_val = self
                    .builder
                    .build_load(val_ty, old_slot, "map.ins.old")
                    .unwrap();
                // Shared-V overwrite-leak fix: when the caller discards
                // the `Option[V]` result (`let _ = m.insert(...)` or a
                // bare `m.insert(...)` statement) AND V is a shared
                // struct / enum, the displaced bucket value's +1 is
                // transferred to the synthesized `Some(old)` payload
                // that no one will hold — so dec it here before the
                // payload is materialized. The flag is set by
                // `compile_stmt`'s discard detection and cleared
                // unconditionally at the next statement; only consume
                // (read + clear) here so a no-op for the discard path
                // doesn't poison the bound-result path. When V isn't
                // shared, `map_val_shared_heap_type_for` returns None
                // and we skip — Vec/String/primitive V's don't have a
                // refcount to dec.
                if self.pending_map_insert_old_dec {
                    self.pending_map_insert_old_dec = false;
                    if let Some(heap_type) = self.map_val_shared_heap_type_for(var_name) {
                        if old_val.is_pointer_value() {
                            self.emit_refcount_dec(
                                var_name,
                                heap_type,
                                old_val.into_pointer_value(),
                            );
                        }
                    } else if self.map_val_owned_heap_str_vec_for(var_name) {
                        // B-2026-07-22-12: owned-heap V (`String` / `Vec`). The
                        // displaced old value was loaded into `old_val` and is
                        // about to be wrapped in a `Some(old)` payload nobody
                        // holds (the result is discarded) — reclaim it now. Deep
                        // per-value drop for `Vec[String]` (inner Strings + outer
                        // buffer); shallow `{ptr,len,cap}` free for `String` /
                        // `Vec[primitive]`, self-guarded on cap>0 so a rodata
                        // literal no-ops. (The fresh-insert path never reaches
                        // here.)
                        self.reclaim_displaced_owned_map_value(var_name, old_val, val_ty);
                    }
                }
                // Multi-word payload via `coerce_to_payload_words` — see
                // `Vec.first`/`Vec.last` arm for the rationale.
                let some_payload_words = self.coerce_to_payload_words(old_val, 3)?;
                let some_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                self.builder.position_at_end(none_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                self.builder.position_at_end(merge_bb);
                let agg = self.build_option_some_via_phis(
                    &some_payload_words,
                    some_end_bb,
                    none_bb,
                    "ins.opt",
                );
                Ok(agg)
            }
            "get" => {
                if args.is_empty() {
                    return Err("Map.get requires a key argument".to_string());
                }
                // Allocation-free String-slice key: a borrowed `{ptr,len,cap=0}`
                // view into the source. `get` only hashes/compares and never
                // retains the key, so the borrow is sound.
                let key_val = match self.try_compile_borrowed_string_slice(&args[0].value)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                let fn_val = self.current_fn.unwrap();
                // Slice 1b.3 — Map[i64, i64].get routes through the
                // mono `karac_map_i64_i64_get` symbol (value calling
                // convention: i64 key). Gate on the compiled value
                // type rather than the side-table `key_ty` for the
                // same reason as the insert arm — see the comment
                // there. Allocate the key/val slots BEFORE the gate
                // so the alloca order in the entry block remains
                // identical between mono and erased paths
                // (`map.get.key` before `map.get.val`); the
                // failing-char-map test passes under that layout.
                let key_slot = self.create_entry_alloca(fn_val, "map.get.key", key_ty);
                let val_slot = self.create_entry_alloca(fn_val, "map.get.val", val_ty);
                self.builder.build_store(key_slot, key_val).unwrap();
                let key_val_matches = key_val.get_type() == key_ty;
                let found = if self.should_use_mono_map_for(key_ty, val_ty) && key_val_matches {
                    let mono = self.get_or_emit_map_mono_methods(key_ty, val_ty);
                    self.builder
                        .build_call(
                            mono.get_fn,
                            &[map_handle.into(), key_val.into(), val_slot.into()],
                            "map.get.found",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value()
                } else {
                    self.builder
                        .build_call(
                            self.karac_map_get_fn,
                            &[map_handle.into(), key_slot.into(), val_slot.into()],
                            "map.get.found",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value()
                };

                // The lookup never stores the key, so a fresh-owned-temp key
                // (`m.get(w.to_string())`) must be freed — no-ops on a borrowed
                // view (cap == 0) or a moved binding / literal key (the binding
                // source's own scope-exit free covers those). Mirrors `get_or`;
                // without it `get` leaked one key buffer per call (LSan).
                self.free_fresh_owned_str_arg(&args[0].value, key_val);

                let found_bb = self.context.append_basic_block(fn_val, "map.get.found.bb");
                let notfound_bb = self
                    .context
                    .append_basic_block(fn_val, "map.get.notfound.bb");
                let merge_bb = self.context.append_basic_block(fn_val, "map.get.merge");

                self.builder
                    .build_conditional_branch(found, found_bb, notfound_bb)
                    .unwrap();

                // Found — load value and split into payload words.
                self.builder.position_at_end(found_bb);
                let elem_val = self
                    .builder
                    .build_load(val_ty, val_slot, "map.get.val")
                    .unwrap();
                // Shared-struct V: the runtime `karac_map_get` /
                // mono `karac_map_*_get` byte-copies the bucket's
                // stored pointer into `val_slot` without touching its
                // refcount. The caller's let-site treats Call/MethodCall
                // RHS as "fresh +1 ref" (`rhs_yields_fresh_ref` →
                // skip receive-side rc_inc) under the assumption every
                // shared-returning callee hands the caller a freshly-
                // owned ref. Map.get violates that assumption: the
                // returned ptr is an alias to the bucket's stored ref.
                // Pre-2bd2dba, the let-binding's queued scope-exit
                // dec only fired at function tail (function-scope
                // frame), so the alias-then-discard pattern didn't
                // expose the missing inc. Post-2bd2dba, the per-iter
                // cleanup of body-local lets fires on every loop
                // iteration; without the inc here, the per-iter dec
                // brings the bucket's ref to zero, freeing the Node
                // while the Map still holds a dangling pointer.
                // Subsequent allocations reuse the freed chunk and
                // every bucket whose value was the freed chunk now
                // aliases the new occupant — observed in kata 133
                // (clone_graph BFS over 2000-node ring graph) as a
                // ~100× per-clone slowdown from malloc-freelist
                // thrashing. Emit the inc here so Map.get matches the
                // calling convention; the caller's per-iter dec
                // brings the count back to the construction-time
                // value, leaving the Map's bucket reference intact.
                if let Some(te) = self.var_elem_type_exprs.get(var_name).cloned() {
                    if let TypeKind::Path(p) = &te.kind {
                        if let Some(seg) = p.segments.last() {
                            if let Some(info) = self.shared_types.get(seg.as_str()).cloned() {
                                let ptr = elem_val.into_pointer_value();
                                self.emit_refcount_inc("map.get", info.heap_type, ptr);
                            }
                        }
                    }
                }
                // Multi-word payload via `coerce_to_payload_words` — see
                // `Vec.first`/`Vec.last` arm for the rationale.
                let some_payload_words = self.coerce_to_payload_words(elem_val, 3)?;
                let found_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Not found.
                self.builder.position_at_end(notfound_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge — phi and build Option struct.
                self.builder.position_at_end(merge_bb);
                let agg = self.build_option_some_via_phis(
                    &some_payload_words,
                    found_end_bb,
                    notfound_bb,
                    "opt",
                );
                Ok(agg)
            }
            "get_or" => {
                if args.len() < 2 {
                    return Err("Map.get_or requires a key and a default argument".to_string());
                }
                // Borrowed String-slice key (lookup-only, no retain — sound),
                // mirroring `get`.
                let key_val = match self.try_compile_borrowed_string_slice(&args[0].value)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                let fn_val = self.current_fn.unwrap();
                let key_slot = self.create_entry_alloca(fn_val, "map.getor.key", key_ty);
                let val_slot = self.create_entry_alloca(fn_val, "map.getor.val", val_ty);
                self.builder.build_store(key_slot, key_val).unwrap();
                let key_val_matches = key_val.get_type() == key_ty;
                let found = if self.should_use_mono_map_for(key_ty, val_ty) && key_val_matches {
                    let mono = self.get_or_emit_map_mono_methods(key_ty, val_ty);
                    self.builder
                        .build_call(
                            mono.get_fn,
                            &[map_handle.into(), key_val.into(), val_slot.into()],
                            "map.getor.found",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value()
                } else {
                    self.builder
                        .build_call(
                            self.karac_map_get_fn,
                            &[map_handle.into(), key_slot.into(), val_slot.into()],
                            "map.getor.found",
                        )
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_int_value()
                };

                // The lookup never stores the key, so a fresh-owned-temp key
                // (`m.get_or(w.to_string(), d)`) must be freed — no-ops on a
                // borrowed view (cap == 0) or a binding/literal key.
                self.free_fresh_owned_str_arg(&args[0].value, key_val);

                let found_bb = self
                    .context
                    .append_basic_block(fn_val, "map.getor.found.bb");
                let default_bb = self
                    .context
                    .append_basic_block(fn_val, "map.getor.default.bb");
                let merge_bb = self.context.append_basic_block(fn_val, "map.getor.merge");
                self.builder
                    .build_conditional_branch(found, found_bb, default_bb)
                    .unwrap();

                // Found — produce an OWNED copy of the stored value. `get_or`
                // returns `V` (not a borrow), so a non-shared heap V (String /
                // Vec / struct) is deep-cloned: returning an alias to the
                // bucket's buffer would double-free with the map's drop at the
                // caller's scope exit. A shared (RC) V clones shallowly (pointer
                // copy) so it gets an rc_inc to own a balanced reference (same
                // rationale as `get`). A scalar V's clone fn is a plain
                // load+store. Mirrors the interpreter's `v.clone()`.
                self.builder.position_at_end(found_bb);
                let found_val = if let Some(v_te) = self.var_elem_type_exprs.get(var_name).cloned()
                {
                    let clone_fn = self.emit_clone_fn_for_type_expr(&v_te);
                    let dst = self.create_entry_alloca(fn_val, "map.getor.clone", val_ty);
                    // `emit_clone_fn_*` / `create_entry_alloca` may move the
                    // builder; re-assert the found block before emitting here.
                    self.builder.position_at_end(found_bb);
                    self.builder
                        .build_call(clone_fn, &[val_slot.into(), dst.into()], "map.getor.clone")
                        .unwrap();
                    let fv = self
                        .builder
                        .build_load(val_ty, dst, "map.getor.hit")
                        .unwrap();
                    if let TypeKind::Path(p) = &v_te.kind {
                        if let Some(seg) = p.segments.last() {
                            if let Some(info) = self.shared_types.get(seg.as_str()).cloned() {
                                self.emit_refcount_inc(
                                    "map.getor",
                                    info.heap_type,
                                    fv.into_pointer_value(),
                                );
                            }
                        }
                    }
                    fv
                } else {
                    self.builder
                        .build_load(val_ty, val_slot, "map.getor.hit")
                        .unwrap()
                };
                let found_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Not found — evaluate the default expression.
                self.builder.position_at_end(default_bb);
                let default_val = self.compile_expr(&args[1].value)?;
                let default_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();

                // Merge — phi between the hit value and the default.
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(val_ty, "map.getor.result").unwrap();
                phi.add_incoming(&[(&found_val, found_end_bb), (&default_val, default_end_bb)]);
                Ok(phi.as_basic_value())
            }
            "remove" => {
                if args.is_empty() {
                    return Err("Map.remove requires a key argument".to_string());
                }
                // Borrowed String-slice key (lookup-only, no retain — sound).
                let key_val = match self.try_compile_borrowed_string_slice(&args[0].value)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                let fn_val = self.current_fn.unwrap();
                let key_slot = self.create_entry_alloca(fn_val, "map.remove.key", key_ty);
                let old_slot = self.create_entry_alloca(fn_val, "map.remove.old", val_ty);
                self.builder.build_store(key_slot, key_val).unwrap();
                // `drop_key` releases the bucket's STORED key buffer (the
                // tombstone would orphan it) when the key is a heap
                // `{ptr,len,cap}`. The value is moved out into the returned
                // `Some(old)`, so the runtime never frees it.
                let drop_key = self
                    .context
                    .i32_type()
                    .const_int(u64::from(self.llvm_ty_is_vec_struct(key_ty)), false);
                let found = self
                    .builder
                    .build_call(
                        self.karac_map_remove_old_fn,
                        &[
                            map_handle.into(),
                            key_slot.into(),
                            old_slot.into(),
                            drop_key.into(),
                        ],
                        "map.remove.found",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                // `remove` looks the key up and tombstones the bucket; it never
                // stores the incoming key, so a fresh-owned-temp key must be
                // freed (no-ops on a borrowed view / moved binding / literal).
                self.free_fresh_owned_str_arg(&args[0].value, key_val);
                // Build Option[V]: Some(old) if found, None otherwise.
                let found_bb = self.context.append_basic_block(fn_val, "map.rm.found");
                let notfound_bb = self.context.append_basic_block(fn_val, "map.rm.notfound");
                let merge_bb = self.context.append_basic_block(fn_val, "map.rm.merge");
                self.builder
                    .build_conditional_branch(found, found_bb, notfound_bb)
                    .unwrap();
                self.builder.position_at_end(found_bb);
                let old_val = self
                    .builder
                    .build_load(val_ty, old_slot, "map.rm.old")
                    .unwrap();
                // B-2026-07-22-12 (remove sibling of the insert overwrite leak):
                // when the `Option[V]` result is discarded (`let _ =
                // m.remove(k)`), the moved-out value is wrapped in a `Some(old)`
                // nobody holds. Reclaim it — dec a shared/RC value, free an
                // owned-heap `String`/`Vec` buffer. Same wildcard-let gating as
                // insert (a bare `m.remove(k);` already drops its Option temp).
                if self.pending_map_insert_old_dec {
                    self.pending_map_insert_old_dec = false;
                    if let Some(heap_type) = self.map_val_shared_heap_type_for(var_name) {
                        if old_val.is_pointer_value() {
                            self.emit_refcount_dec(
                                var_name,
                                heap_type,
                                old_val.into_pointer_value(),
                            );
                        }
                    } else if self.map_val_owned_heap_str_vec_for(var_name) {
                        // Deep drop for Vec[String]; shallow free for String /
                        // Vec[primitive] (B-2026-07-22-12 remove sibling).
                        self.reclaim_displaced_owned_map_value(var_name, old_val, val_ty);
                    }
                }
                // Multi-word payload via `coerce_to_payload_words` — see
                // `Vec.first`/`Vec.last` arm for the rationale.
                let some_payload_words = self.coerce_to_payload_words(old_val, 3)?;
                let found_end_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                self.builder.position_at_end(notfound_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                self.builder.position_at_end(merge_bb);
                let agg = self.build_option_some_via_phis(
                    &some_payload_words,
                    found_end_bb,
                    notfound_bb,
                    "rm.opt",
                );
                Ok(agg)
            }
            "contains_key" => {
                if args.is_empty() {
                    return Err("Map.contains_key requires a key argument".to_string());
                }
                // Borrowed String-slice key (lookup-only, no retain — sound).
                let key_val = match self.try_compile_borrowed_string_slice(&args[0].value)? {
                    Some(v) => v,
                    None => self.compile_expr(&args[0].value)?,
                };
                let fn_val = self.current_fn.unwrap();
                let key_slot = self.create_entry_alloca(fn_val, "map.contains.key", key_ty);
                self.builder.build_store(key_slot, key_val).unwrap();
                let found = self
                    .builder
                    .build_call(
                        self.karac_map_contains_fn,
                        &[map_handle.into(), key_slot.into()],
                        "map.contains",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                // Lookup-only; never stores the key, so free a fresh-owned-temp
                // key (no-ops on a borrowed view / moved binding / literal).
                self.free_fresh_owned_str_arg(&args[0].value, key_val);
                Ok(found.into())
            }
            "clear" => {
                // Free heap key/value buffers before resetting: plain
                // `karac_map_clear` only zeroes the status bytes, leaking any
                // `{ptr,len,cap}` String/Vec keys or values (the eventual
                // map-free frees only occupied slots, and a clear leaves
                // none). Shared-typed halves get a refcount-dec walk first,
                // mirroring the scope-exit `FreeMapHandle` cleanup.
                let key_is_vec = self.llvm_ty_is_vec_struct(key_ty);
                let val_is_vec = self.llvm_ty_is_vec_struct(val_ty);
                if let Some(heap_ty) = self.map_val_shared_heap_type_for(var_name) {
                    self.emit_map_shared_half_rc_dec_walk(map_handle, heap_ty, true);
                }
                if let Some(heap_ty) = self.map_key_shared_heap_type_for(var_name) {
                    self.emit_map_shared_half_rc_dec_walk(map_handle, heap_ty, false);
                }
                // Slice 3r (deferred gap (d)): a value beyond the one-level
                // `{ptr,len,cap}` overlay clears through the per-value drop
                // fn (the clear sibling of the scope-exit routing).
                let val_drop_fn = self
                    .var_elem_type_exprs
                    .get(var_name)
                    .cloned()
                    .and_then(|vte| self.map_val_drop_fn_for_type_expr(&vte));
                if let Some(val_fn) = val_drop_fn {
                    let i32_t = self.context.i32_type();
                    let key_flag = i32_t.const_int(if key_is_vec { 1 } else { 0 }, false);
                    let fn_ptr = val_fn.as_global_value().as_pointer_value();
                    self.builder
                        .build_call(
                            self.karac_map_clear_with_val_drop_fn_fn,
                            &[map_handle.into(), key_flag.into(), fn_ptr.into()],
                            "",
                        )
                        .unwrap();
                } else if key_is_vec || val_is_vec {
                    let i32_t = self.context.i32_type();
                    let key_flag = i32_t.const_int(if key_is_vec { 1 } else { 0 }, false);
                    let val_flag = i32_t.const_int(if val_is_vec { 1 } else { 0 }, false);
                    self.builder
                        .build_call(
                            self.karac_map_clear_with_drop_vec_fn,
                            &[map_handle.into(), key_flag.into(), val_flag.into()],
                            "",
                        )
                        .unwrap();
                } else {
                    self.builder
                        .build_call(self.karac_map_clear_fn, &[map_handle.into()], "")
                        .unwrap();
                }
                // Map.clear returns Unit — codegen represents Unit as i64 0.
                Ok(i64_t.const_int(0, false).into())
            }
            "keys" | "values" | "entries" => self.compile_map_keys_values_entries(var_name, method),
            // SortedMap ordered-only methods (B-2026-07-18-1). Gated to a sorted
            // receiver — a plain (hash) Map never typechecks these — so the
            // sorted-keys backbone (`emit_sorted_keys_buf`) is always valid here.
            "min" | "max" | "floor" | "ceiling"
                if self.sorted_collection_vars.contains(var_name) =>
            {
                self.compile_sorted_map_option_lookup(
                    var_name, method, map_handle, key_ty, val_ty, args,
                )
            }
            "range" if self.sorted_collection_vars.contains(var_name) => {
                self.compile_sorted_map_range(var_name, map_handle, key_ty, val_ty, args)
            }
            _ => Err(format!("codegen: Map.{method} not yet implemented")),
        }
    }
}
