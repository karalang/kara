//! Provider / ambient-resource state.
//!
//! Third slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! state behind the provider (ambient resource) surface: the per-resource
//! ids and trait names, the trait method tables, the emitted per-(resource,
//! trait) vtable globals, and the LLVM struct type of a provider frame.
//!
//! The *code* that consumes this state lives in the sibling `provider.rs`
//! (`with_provider[R]` lowering and `R.method(..)` dispatch); this module
//! holds only the data, which is why it is named `provider_state`.
//!
//! Nearly a private module already — the spike measured it as 7 fields
//! across 3 files, the tightest cluster in the struct. One of those 7,
//! `provider_lookup_result_ty`, was **dead**: stored by the constructor and
//! never read back through `self`. It is deleted rather than moved (the
//! constructor still builds the local, which the lookup fn's `fn_type`
//! needs).
//!
//! Accessed as `self.provider_state.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::HashMap;

use inkwell::types::StructType;
use inkwell::values::GlobalValue;

/// Provider (ambient-resource) tables and emitted vtables.
pub(crate) struct ProviderState<'ctx> {
    // ── Theme 6: `with_provider[R]` trait-method dispatch ──────────
    /// Resource name → stable u32 ID assigned at codegen init from the
    /// declaration order of `Item::EffectResource` items. The same
    /// integer flows through to runtime calls (`karac_provider_push`,
    /// `karac_provider_lookup`); the runtime is name-agnostic.
    pub(crate) provider_resource_ids: HashMap<String, u32>,
    /// Resource name → the declared provider trait bounds, in source order,
    /// for resources declared as `effect resource R: T` (one bound) or
    /// `effect resource R: A + B` (design.md:7216, B-2026-08-19-3). Used to
    /// (1) drive vtable emission for the impls of each bound and (2) resolve
    /// method indices at `R.method(...)` call sites.
    ///
    /// A resource with NO bound is absent from the map entirely, not
    /// present-with-an-empty-list: trait-*absence* is the discriminator that
    /// routes a resource to the ambient runtime-stack path, and several
    /// readers spell that as `contains_key`.
    pub(crate) provider_resource_traits: HashMap<String, Vec<String>>,
    /// Trait name → ordered method-name list (source-declaration order
    /// from the `trait T { ... }` block). Vtables for `impl T for U`
    /// store fn ptrs in this same order; method dispatch resolves the
    /// vtable index by `position()` against this list.
    pub(crate) provider_trait_methods: HashMap<String, Vec<String>>,
    /// Trait-less *user* effect resource (`effect resource R;`, no `: T`)
    /// → ordered method-name list, derived from the override type's
    /// inherent-impl method order during the eager ambient-vtable pre-pass
    /// (`emit_ambient_provider_vtables`). A trait-less resource has no trait
    /// to pin a canonical method order, so it is keyed by *resource* (the
    /// call site `R.method(...)` knows R but not the override type U) and
    /// plays the same role `provider_trait_methods` plays for trait-ful
    /// resources: vtable layout + dispatch index. Distinct from
    /// `prelude::AMBIENT_RESOURCE_METHODS` (prelude resources like `Clock`
    /// keep their hardcoded order + FFI default); membership here is the
    /// discriminator that routes a trait-less resource through the
    /// always-override runtime dispatch (no FFI default) in
    /// `try_compile_provider_dispatch`.
    pub(crate) user_ambient_resource_methods: HashMap<String, Vec<String>>,
    /// (impl-target type name, trait name) → emitted vtable global.
    /// Populated after impl method declarations run in `compile_program`.
    pub(crate) provider_vtables: HashMap<(String, String), GlobalValue<'ctx>>,
    /// LLVM struct type for `ProviderFrame { prev, resource_id, data, vtable }`
    /// — `#[repr(C)]` matches `runtime/src/lib.rs::ProviderFrame`. Consumed
    /// at `with_provider[R]` lowering sites for the alloca'd frame storage
    /// (sub-step 3); declared here so the type is established alongside
    /// the runtime extern declarations.
    pub(crate) provider_frame_ty: StructType<'ctx>,
}
