//! Ownership-derived scoped-alias metadata for slice parameters
//! (alias-metadata slice 4 — the loop-kernel `restrict` win).
//!
//! A `mut Slice[T]` is an *exclusive* borrow of a contiguous region: while it
//! is live, no other accessible reference — shared or mutable — overlaps that
//! region (the same guarantee the shipped `mut ref T` → `noalias` rests on,
//! enforced by the borrow checker + RC-fallback pass). A shared `Slice[T]` may
//! alias *another shared* slice, but never an exclusive one. Those facts are
//! exactly the C99-`restrict` / Fortran no-alias property the autovectorizer
//! wants and O2 can never prove from a `{ptr,len}` fat struct on its own
//! (design.md § 8878, Tier 0 — backend alias facts).
//!
//! Unlike `mut ref T`, a slice's buffer pointer is a *field* of the by-value
//! fat struct, not the parameter, so a param-level `noalias` cannot reach it.
//! Instead we lower the disjointness onto the element loads/stores as
//! `!alias.scope` / `!noalias` metadata (LLVM's scoped-alias mechanism):
//!
//!   - one distinct alias scope `Sᵢ` per EXCLUSIVE (`mut Slice`) param;
//!   - an exclusive param's element accesses get `!alias.scope !{Sᵢ}` and
//!     `!noalias !{Sⱼ : other exclusive j}`;
//!   - a shared (`Slice`) param's element accesses get `!noalias !{all Sᵢ}`
//!     (no scope of their own — shared-vs-shared may legitimately alias).
//!
//! For any pair (exclusive i, other j≠i), one side carries `!alias.scope Sᵢ`
//! and the other carries `!noalias Sᵢ`, which is all LLVM needs to prove the
//! two do not alias. Emitted only when the function has ≥1 exclusive slice
//! param AND ≥2 slice params total (nothing to disambiguate otherwise).
//!
//! **Soundness rests entirely on the borrow checker** preventing overlapping
//! borrows: a program that aliased an exclusive slice with anything else is
//! *unrepresentable* in valid Kāra, so the `!noalias` claim can never be
//! violated by a well-typed program. The shadow hazard — a local slice
//! re-binding a slice-param name — is closed in `register_var_from_type_expr`,
//! which drops the name from the scope map on any re-registration (fail
//! closed: a shadowed name simply loses the metadata).

use crate::ast::{Function, TypeKind};
use inkwell::values::{AsValueRef, InstructionValue};
use llvm_sys::core::{
    LLVMGetMDKindIDInContext, LLVMMDNodeInContext2, LLVMMetadataAsValue, LLVMSetMetadata,
};
use llvm_sys::debuginfo::{LLVMMetadataReplaceAllUsesWith, LLVMTemporaryMDNode};
use llvm_sys::prelude::{LLVMContextRef, LLVMMetadataRef, LLVMValueRef};
use std::os::raw::{c_char, c_uint};

/// The `!alias.scope` / `!noalias` metadata-as-value nodes to attach to a slice
/// param's element load/store, prebuilt at function entry. Raw LLVM value refs
/// (metadata wrapped as a value) — they live in the module for its whole life,
/// so caching the raw ref across the function body is sound. `Copy` so the
/// attach site can `.get().copied()` without holding a borrow of `self`.
#[derive(Clone, Copy)]
pub(crate) struct SliceAliasMd {
    pub(crate) alias_scope: Option<LLVMValueRef>,
    pub(crate) noalias: Option<LLVMValueRef>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// Rebuild [`Codegen::slice_alias_md`] for `func` (see module docs). Must
    /// run AFTER the parameters are registered via
    /// `register_var_from_type_expr` — that path removes names from the map, so
    /// building first would let param registration clear its own entries.
    pub(super) fn build_slice_alias_scopes(&mut self, func: &Function) {
        self.slice_alias_md.clear();
        // (name, is_exclusive) for each slice parameter.
        let mut params: Vec<(String, bool)> = Vec::new();
        for p in &func.params {
            let excl = matches!(&p.ty.kind, TypeKind::MutSlice(_));
            let shared = matches!(
                &p.ty.kind,
                TypeKind::Path(path) if path.segments.first().map(String::as_str) == Some("Slice")
            );
            if excl || shared {
                if let Some(name) = p.name() {
                    params.push((name.to_string(), excl));
                }
            }
        }
        let n_excl = params.iter().filter(|(_, e)| *e).count();
        // Nothing to disambiguate without ≥1 exclusive borrow and ≥2 slices.
        if n_excl == 0 || params.len() < 2 {
            return;
        }
        let ctx = self.context.raw();
        unsafe {
            let domain = make_distinct_self_node(ctx, &[]);
            // A scope per EXCLUSIVE param; shared params get `None` (they only
            // declare `!noalias` against the exclusive scopes).
            let scopes: Vec<Option<LLVMMetadataRef>> = params
                .iter()
                .map(|(_, excl)| excl.then(|| make_distinct_self_node(ctx, &[domain])))
                .collect();
            let excl_scopes: Vec<LLVMMetadataRef> = scopes.iter().filter_map(|s| *s).collect();
            for (i, (name, excl)) in params.iter().enumerate() {
                let (alias_scope, noalias) = if *excl {
                    let own = scopes[i].expect("exclusive param has a scope");
                    let as_val = md_list_value(ctx, &[own]);
                    // noalias = every OTHER exclusive scope.
                    let others: Vec<LLVMMetadataRef> = scopes
                        .iter()
                        .enumerate()
                        .filter_map(|(j, s)| (j != i).then_some(*s).flatten())
                        .collect();
                    let na_val = (!others.is_empty()).then(|| md_list_value(ctx, &others));
                    (Some(as_val), na_val)
                } else {
                    // Shared: noalias = every exclusive scope; no alias.scope.
                    let na_val =
                        (!excl_scopes.is_empty()).then(|| md_list_value(ctx, &excl_scopes));
                    (None, na_val)
                };
                if alias_scope.is_some() || noalias.is_some() {
                    self.slice_alias_md.insert(
                        name.clone(),
                        SliceAliasMd {
                            alias_scope,
                            noalias,
                        },
                    );
                }
            }
        }
    }

    /// Attach the prebuilt `!alias.scope` / `!noalias` metadata for slice param
    /// `var_name` to `inst` (an element load or store). No-op for any name not
    /// in the scope map — local slices and non-slice bindings included.
    pub(super) fn attach_slice_alias_md(&self, inst: InstructionValue<'ctx>, var_name: &str) {
        let Some(md) = self.slice_alias_md.get(var_name).copied() else {
            return;
        };
        let ctx = self.context.raw();
        unsafe {
            let inst_val = inst.as_value_ref();
            if let Some(v) = md.alias_scope {
                LLVMSetMetadata(inst_val, md_kind(ctx, b"alias.scope"), v);
            }
            if let Some(v) = md.noalias {
                LLVMSetMetadata(inst_val, md_kind(ctx, b"noalias"), v);
            }
        }
    }
}

unsafe fn md_kind(ctx: LLVMContextRef, name: &[u8]) -> c_uint {
    LLVMGetMDKindIDInContext(ctx, name.as_ptr() as *const c_char, name.len() as c_uint)
}

/// `!{ self }` (extra=`[]`) or `!{ self, extra… }` — a self-referential, hence
/// *distinct*, metadata node (the same temp + RAUW recipe as the `llvm.loop`
/// id in control_flow_bce.rs). Scoped-alias analysis keys on node identity, and
/// a self-referential node cannot be uniqued against any other, giving each
/// domain / scope the distinct identity it needs.
unsafe fn make_distinct_self_node(
    ctx: LLVMContextRef,
    extra: &[LLVMMetadataRef],
) -> LLVMMetadataRef {
    let temp = LLVMTemporaryMDNode(ctx, std::ptr::null_mut(), 0);
    let mut ops: Vec<LLVMMetadataRef> = Vec::with_capacity(1 + extra.len());
    ops.push(temp);
    ops.extend_from_slice(extra);
    let node = LLVMMDNodeInContext2(ctx, ops.as_mut_ptr(), ops.len());
    LLVMMetadataReplaceAllUsesWith(temp, node);
    node
}

/// A uniqued `!{ scopes… }` list node wrapped as a value for `LLVMSetMetadata`
/// (the operand form `!alias.scope` / `!noalias` expect — a plain list of scope
/// nodes, no self-reference).
unsafe fn md_list_value(ctx: LLVMContextRef, scopes: &[LLVMMetadataRef]) -> LLVMValueRef {
    let mut ops = scopes.to_vec();
    let node = LLVMMDNodeInContext2(ctx, ops.as_mut_ptr(), ops.len());
    LLVMMetadataAsValue(ctx, node)
}
