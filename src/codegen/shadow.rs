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

use inkwell::types::{BasicTypeEnum, FunctionType, StructType};

use crate::ast::TypeExpr;

use super::state::{ColumnVarInfo, TensorVarInfo};

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
