//! Type-changing shadow support for the AOT backend.
//!
//! A `let` that re-binds a name already in scope with a *different
//! type/class* (`let s = "hi"; let s = s.len();`) is legal in Kāra — the
//! resolver and interpreter support it (design.md § Variables > Shadowing).
//! The AOT backend tracks each binding's class through a constellation of
//! **per-variable sidecar maps** keyed by variable name (`string_vars`,
//! `vec_elem_types`, the `map_*` / `set_*` tables, `tensor_var_infos`, …).
//! Those maps are only ever *added* to at bind time, never cleared, so an
//! old binding's class metadata used to survive a rebind and mis-dispatch a
//! later use of the new binding — which traps at runtime. A conservative
//! guard in `bind_pattern` previously rejected such shadows with a clean
//! compile error.
//!
//! This module replaces that guard with a precise metadata lifecycle:
//!
//! * [`Codegen::forget_var_metadata`] purges every per-variable entry for a
//!   name. `bind_pattern` calls it on a rebind so for-loop / match-arm /
//!   destructure bindings (which re-register their metadata *after*
//!   `bind_pattern`) start from a clean slate.
//! * [`Codegen::take_var_metadata`] / [`Codegen::restore_var_metadata`]
//!   power the `StmtKind::Let` arm's three-step dance, which is the whole
//!   difficulty: the RHS may reference the old binding (`let s = s.len()`),
//!   so the OLD class tags must live until *after* value compilation, but be
//!   gone before any use of the NEW binding.
//!
//! **Drop safety (LeakSanitizer):** purging these maps cannot drop a still-
//! needed cleanup. Scope-exit drops are queued as [`CleanupAction`]s keyed by
//! the binding's *alloca* (captured eagerly at bind time in
//! `scope_cleanup_actions`), not re-derived from these name-keyed maps at
//! drain time. The old binding's value therefore still drops at scope exit
//! even after its name-metadata is forgotten.
//!
//! [`CleanupAction`]: super::state

use std::collections::{HashMap, HashSet};

use inkwell::types::{BasicTypeEnum, FunctionType, StructType};

use crate::ast::TypeExpr;

use super::state::{ColumnVarInfo, TensorVarInfo, VarSlot};

/// A complete snapshot of every per-variable sidecar-metadata entry for one
/// binding name. Produced by [`Codegen::take_var_metadata`] (which removes
/// the entries) and consumed by [`Codegen::restore_var_metadata`] (which
/// re-installs the present ones). The field set is the audited, exhaustive
/// list of `Codegen` maps/sets keyed by a local binding name — see the
/// phase-5-diagnostics "codegen type-changing-shadow" entry. Adding a new
/// per-variable map to `Codegen` REQUIRES adding it here too, or a shadow
/// will leak its stale tag.
#[derive(Default)]
pub(super) struct VarMetadataSnapshot<'ctx> {
    var_type_names: Option<String>,
    tuple_var_elem_type_names: Option<Vec<Option<String>>>,
    atomic_var_inner_is_bool: bool,
    closure_fn_types: Option<FunctionType<'ctx>>,
    len_alias: Option<String>,
    vec_elem_types: Option<BasicTypeEnum<'ctx>>,
    slice_elem_types: Option<BasicTypeEnum<'ctx>>,
    ref_params: Option<BasicTypeEnum<'ctx>>,
    var_option_shared_heap: Option<StructType<'ctx>>,
    tensor_var_infos: Option<TensorVarInfo<'ctx>>,
    column_var_infos: Option<ColumnVarInfo<'ctx>>,
    enum_inst_var_types: Option<TypeExpr>,
    map_key_types: Option<BasicTypeEnum<'ctx>>,
    map_val_types: Option<BasicTypeEnum<'ctx>>,
    map_key_type_names: Option<String>,
    var_elem_type_exprs: Option<TypeExpr>,
    map_key_type_exprs: Option<TypeExpr>,
    set_elem_types: Option<BasicTypeEnum<'ctx>>,
    set_elem_type_names: Option<String>,
    set_elem_type_exprs: Option<TypeExpr>,
    string_vars: bool,
    cstr_vars: bool,
    inline_option_payload_vars: bool,
    inline_result_payload_vars: bool,
    inline_option_map_payload_vars: bool,
    inline_option_agg_payload_vars: bool,
    boxed_enum_payload_vars: bool,
    rc_fallback_heap_types: Option<StructType<'ctx>>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Remove and return every per-variable sidecar-metadata entry for
    /// `name`, leaving the maps with no trace of the binding. The returned
    /// snapshot can be reinstated with [`Self::restore_var_metadata`].
    pub(super) fn take_var_metadata(&mut self, name: &str) -> VarMetadataSnapshot<'ctx> {
        VarMetadataSnapshot {
            var_type_names: self.var_type_names.remove(name),
            tuple_var_elem_type_names: self.tuple_var_elem_type_names.remove(name),
            atomic_var_inner_is_bool: self.atomic_var_inner_is_bool.remove(name),
            closure_fn_types: self.closure_fn_types.remove(name),
            len_alias: self.len_alias.remove(name),
            vec_elem_types: self.vec_elem_types.remove(name),
            slice_elem_types: self.slice_elem_types.remove(name),
            ref_params: self.ref_params.remove(name),
            var_option_shared_heap: self.var_option_shared_heap.remove(name),
            tensor_var_infos: self.tensor_var_infos.remove(name),
            column_var_infos: self.column_var_infos.remove(name),
            enum_inst_var_types: self.enum_inst_var_types.remove(name),
            map_key_types: self.map_key_types.remove(name),
            map_val_types: self.map_val_types.remove(name),
            map_key_type_names: self.map_key_type_names.remove(name),
            var_elem_type_exprs: self.var_elem_type_exprs.remove(name),
            map_key_type_exprs: self.map_key_type_exprs.remove(name),
            set_elem_types: self.set_elem_types.remove(name),
            set_elem_type_names: self.set_elem_type_names.remove(name),
            set_elem_type_exprs: self.set_elem_type_exprs.remove(name),
            string_vars: self.string_vars.remove(name),
            cstr_vars: self.cstr_vars.remove(name),
            inline_option_payload_vars: self.inline_option_payload_vars.remove(name),
            inline_result_payload_vars: self.inline_result_payload_vars.remove(name),
            inline_option_map_payload_vars: self.inline_option_map_payload_vars.remove(name),
            inline_option_agg_payload_vars: self.inline_option_agg_payload_vars.remove(name),
            boxed_enum_payload_vars: self.boxed_enum_payload_vars.remove(name),
            rc_fallback_heap_types: self.rc_fallback_heap_types.remove(name),
        }
    }

    /// Re-install the present entries of `snap` under `name`. Any entry that
    /// was absent (e.g. the old binding carried no Vec element type) is left
    /// absent — `restore` is the exact inverse of the `take` that produced
    /// `snap`, modulo entries written in between.
    pub(super) fn restore_var_metadata(&mut self, name: &str, snap: VarMetadataSnapshot<'ctx>) {
        let key = name.to_string();
        if let Some(v) = snap.var_type_names {
            self.var_type_names.insert(key.clone(), v);
        }
        if let Some(v) = snap.tuple_var_elem_type_names {
            self.tuple_var_elem_type_names.insert(key.clone(), v);
        }
        if snap.atomic_var_inner_is_bool {
            self.atomic_var_inner_is_bool.insert(key.clone());
        }
        if let Some(v) = snap.closure_fn_types {
            self.closure_fn_types.insert(key.clone(), v);
        }
        if let Some(v) = snap.len_alias {
            self.len_alias.insert(key.clone(), v);
        }
        if let Some(v) = snap.vec_elem_types {
            self.vec_elem_types.insert(key.clone(), v);
        }
        if let Some(v) = snap.slice_elem_types {
            self.slice_elem_types.insert(key.clone(), v);
        }
        if let Some(v) = snap.ref_params {
            self.ref_params.insert(key.clone(), v);
        }
        if let Some(v) = snap.var_option_shared_heap {
            self.var_option_shared_heap.insert(key.clone(), v);
        }
        if let Some(v) = snap.tensor_var_infos {
            self.tensor_var_infos.insert(key.clone(), v);
        }
        if let Some(v) = snap.column_var_infos {
            self.column_var_infos.insert(key.clone(), v);
        }
        if let Some(v) = snap.enum_inst_var_types {
            self.enum_inst_var_types.insert(key.clone(), v);
        }
        if let Some(v) = snap.map_key_types {
            self.map_key_types.insert(key.clone(), v);
        }
        if let Some(v) = snap.map_val_types {
            self.map_val_types.insert(key.clone(), v);
        }
        if let Some(v) = snap.map_key_type_names {
            self.map_key_type_names.insert(key.clone(), v);
        }
        if let Some(v) = snap.var_elem_type_exprs {
            self.var_elem_type_exprs.insert(key.clone(), v);
        }
        if let Some(v) = snap.map_key_type_exprs {
            self.map_key_type_exprs.insert(key.clone(), v);
        }
        if let Some(v) = snap.set_elem_types {
            self.set_elem_types.insert(key.clone(), v);
        }
        if let Some(v) = snap.set_elem_type_names {
            self.set_elem_type_names.insert(key.clone(), v);
        }
        if let Some(v) = snap.set_elem_type_exprs {
            self.set_elem_type_exprs.insert(key.clone(), v);
        }
        if snap.string_vars {
            self.string_vars.insert(key.clone());
        }
        if snap.cstr_vars {
            self.cstr_vars.insert(key.clone());
        }
        if snap.inline_option_payload_vars {
            self.inline_option_payload_vars.insert(key.clone());
        }
        if snap.inline_result_payload_vars {
            self.inline_result_payload_vars.insert(key.clone());
        }
        if snap.inline_option_map_payload_vars {
            self.inline_option_map_payload_vars.insert(key.clone());
        }
        if snap.inline_option_agg_payload_vars {
            self.inline_option_agg_payload_vars.insert(key.clone());
        }
        if snap.boxed_enum_payload_vars {
            self.boxed_enum_payload_vars.insert(key.clone());
        }
        if let Some(v) = snap.rc_fallback_heap_types {
            self.rc_fallback_heap_types.insert(key, v);
        }
    }

    /// Purge every per-variable sidecar-metadata entry for `name`. Equivalent
    /// to dropping the result of [`Self::take_var_metadata`]. Called from
    /// `bind_pattern` on a rebind so the new binding does not inherit the old
    /// one's class tags.
    pub(super) fn forget_var_metadata(&mut self, name: &str) {
        let _ = self.take_var_metadata(name);
    }
}

/// A whole-environment snapshot of every name-keyed variable map (the primary
/// `variables` slot table PLUS every per-variable sidecar map/set — the same
/// audited field set as [`VarMetadataSnapshot`], the two extra binding-classed
/// sets `owned_vecstr_params` / `for_loop_borrow_vars`, and `variables`
/// itself). Produced by [`Codegen::snapshot_var_env`] at NESTED-SCOPE entry and
/// re-installed by [`Codegen::restore_var_env`] at scope exit — the lexical
/// scoping that `self.variables` (a flat map with no scope stack) otherwise
/// lacks. Without it a nested `let x` that SHADOWS an outer `x` overwrote the
/// outer slot + metadata and never restored them, so reads after the scope saw
/// the inner value (B-2026-07-13-6: silent wrong result on the everyday
/// nested-scope shadow — a build/run divergence, the interpreter scopes
/// correctly). Restoring the WHOLE per-variable env at scope exit is correct by
/// construction regardless of HOW a name was bound (`let`, `for`-loop var,
/// `match`/`if let` pattern) and keeps `variables` and its sidecar metadata
/// CONSISTENT (a variables-only restore would leave the outer slot paired with
/// the inner shadow's stale class tags — a worse, dispatch-corrupting state).
///
/// **Cleanup safety:** scope-exit heap drops are queued as `CleanupAction`s
/// keyed by the binding's *alloca* (captured eagerly in `scope_cleanup_actions`
/// at bind time), NOT re-derived from these name maps at drain time (see the
/// module header). So reverting the name maps here neither drops a needed
/// cleanup nor resurrects a freed one — it is a pure name-resolution restore.
/// MUST run AFTER the scope's cleanup frame drains (the drain reads the inner
/// bindings' allocas) and only on NORMAL scope exit, never during an
/// `emit_scope_cleanup` return/error walk (which does not pop the scope).
///
/// Adding a new per-variable map to `Codegen` REQUIRES adding it here too
/// (same contract as `VarMetadataSnapshot`), or a shadow will leak its stale
/// entry past the scope.
pub(super) struct VarEnvSnapshot<'ctx> {
    variables: HashMap<String, VarSlot<'ctx>>,
    owned_vecstr_params: HashSet<String>,
    for_loop_borrow_vars: HashSet<String>,
    var_type_names: HashMap<String, String>,
    tuple_var_elem_type_names: HashMap<String, Vec<Option<String>>>,
    atomic_var_inner_is_bool: HashSet<String>,
    closure_fn_types: HashMap<String, FunctionType<'ctx>>,
    len_alias: HashMap<String, String>,
    vec_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    slice_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    ref_params: HashMap<String, BasicTypeEnum<'ctx>>,
    var_option_shared_heap: HashMap<String, StructType<'ctx>>,
    tensor_var_infos: HashMap<String, TensorVarInfo<'ctx>>,
    column_var_infos: HashMap<String, ColumnVarInfo<'ctx>>,
    enum_inst_var_types: HashMap<String, TypeExpr>,
    map_key_types: HashMap<String, BasicTypeEnum<'ctx>>,
    map_val_types: HashMap<String, BasicTypeEnum<'ctx>>,
    map_key_type_names: HashMap<String, String>,
    var_elem_type_exprs: HashMap<String, TypeExpr>,
    map_key_type_exprs: HashMap<String, TypeExpr>,
    set_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    set_elem_type_names: HashMap<String, String>,
    set_elem_type_exprs: HashMap<String, TypeExpr>,
    string_vars: HashSet<String>,
    cstr_vars: HashSet<String>,
    inline_option_payload_vars: HashSet<String>,
    inline_result_payload_vars: HashSet<String>,
    inline_option_map_payload_vars: HashSet<String>,
    inline_option_agg_payload_vars: HashSet<String>,
    boxed_enum_payload_vars: HashSet<String>,
    rc_fallback_heap_types: HashMap<String, StructType<'ctx>>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Clone the whole per-variable name environment for a lexical-scope
    /// checkpoint. See [`VarEnvSnapshot`]. Cheap relative to codegen: the
    /// name-keyed maps hold at most a function's live-binding count.
    pub(super) fn snapshot_var_env(&self) -> VarEnvSnapshot<'ctx> {
        VarEnvSnapshot {
            variables: self.variables.clone(),
            owned_vecstr_params: self.owned_vecstr_params.clone(),
            for_loop_borrow_vars: self.for_loop_borrow_vars.clone(),
            var_type_names: self.var_type_names.clone(),
            tuple_var_elem_type_names: self.tuple_var_elem_type_names.clone(),
            atomic_var_inner_is_bool: self.atomic_var_inner_is_bool.clone(),
            closure_fn_types: self.closure_fn_types.clone(),
            len_alias: self.len_alias.clone(),
            vec_elem_types: self.vec_elem_types.clone(),
            slice_elem_types: self.slice_elem_types.clone(),
            ref_params: self.ref_params.clone(),
            var_option_shared_heap: self.var_option_shared_heap.clone(),
            tensor_var_infos: self.tensor_var_infos.clone(),
            column_var_infos: self.column_var_infos.clone(),
            enum_inst_var_types: self.enum_inst_var_types.clone(),
            map_key_types: self.map_key_types.clone(),
            map_val_types: self.map_val_types.clone(),
            map_key_type_names: self.map_key_type_names.clone(),
            var_elem_type_exprs: self.var_elem_type_exprs.clone(),
            map_key_type_exprs: self.map_key_type_exprs.clone(),
            set_elem_types: self.set_elem_types.clone(),
            set_elem_type_names: self.set_elem_type_names.clone(),
            set_elem_type_exprs: self.set_elem_type_exprs.clone(),
            string_vars: self.string_vars.clone(),
            cstr_vars: self.cstr_vars.clone(),
            inline_option_payload_vars: self.inline_option_payload_vars.clone(),
            inline_result_payload_vars: self.inline_result_payload_vars.clone(),
            inline_option_map_payload_vars: self.inline_option_map_payload_vars.clone(),
            inline_option_agg_payload_vars: self.inline_option_agg_payload_vars.clone(),
            boxed_enum_payload_vars: self.boxed_enum_payload_vars.clone(),
            rc_fallback_heap_types: self.rc_fallback_heap_types.clone(),
        }
    }

    /// Re-install a [`VarEnvSnapshot`], reverting every name-keyed variable map
    /// to its pre-scope state. Nested-scope bindings (new names) vanish and
    /// shadowed outer bindings (name + metadata together) return. See
    /// [`VarEnvSnapshot`] for the cleanup-safety and ordering contract.
    pub(super) fn restore_var_env(&mut self, snap: VarEnvSnapshot<'ctx>) {
        self.variables = snap.variables;
        self.owned_vecstr_params = snap.owned_vecstr_params;
        self.for_loop_borrow_vars = snap.for_loop_borrow_vars;
        self.var_type_names = snap.var_type_names;
        self.tuple_var_elem_type_names = snap.tuple_var_elem_type_names;
        self.atomic_var_inner_is_bool = snap.atomic_var_inner_is_bool;
        self.closure_fn_types = snap.closure_fn_types;
        self.len_alias = snap.len_alias;
        self.vec_elem_types = snap.vec_elem_types;
        self.slice_elem_types = snap.slice_elem_types;
        self.ref_params = snap.ref_params;
        self.var_option_shared_heap = snap.var_option_shared_heap;
        self.tensor_var_infos = snap.tensor_var_infos;
        self.column_var_infos = snap.column_var_infos;
        self.enum_inst_var_types = snap.enum_inst_var_types;
        self.map_key_types = snap.map_key_types;
        self.map_val_types = snap.map_val_types;
        self.map_key_type_names = snap.map_key_type_names;
        self.var_elem_type_exprs = snap.var_elem_type_exprs;
        self.map_key_type_exprs = snap.map_key_type_exprs;
        self.set_elem_types = snap.set_elem_types;
        self.set_elem_type_names = snap.set_elem_type_names;
        self.set_elem_type_exprs = snap.set_elem_type_exprs;
        self.string_vars = snap.string_vars;
        self.cstr_vars = snap.cstr_vars;
        self.inline_option_payload_vars = snap.inline_option_payload_vars;
        self.inline_result_payload_vars = snap.inline_result_payload_vars;
        self.inline_option_map_payload_vars = snap.inline_option_map_payload_vars;
        self.inline_option_agg_payload_vars = snap.inline_option_agg_payload_vars;
        self.boxed_enum_payload_vars = snap.boxed_enum_payload_vars;
        self.rc_fallback_heap_types = snap.rc_fallback_heap_types;
    }
}
