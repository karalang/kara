//! Target-ABI and layout-shape state.
//!
//! Second slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! state that answers "what shape does this target want?":
//!
//! - the three target predicates (`target_is_aarch64`, `target_is_x86_64`
//!   System V, `target_is_windows_x86_64`), computed once at construction
//!   from the triple;
//! - the per-function `#[repr(C)]` parameter and return adaptations —
//!   AArch64 register coercion, indirect (byval) params, `sret` returns,
//!   and the exported names whose signatures those adaptations changed
//!   (B-2026-07-09-2);
//! - the niche-ABI record per function, and the C-ABI auto-boxed export
//!   names;
//! - the headerless-layout analysis (candidates, the final program-wide
//!   headerless set, per-function cluster density, and the reshaper
//!   dummies).
//!
//! Deliberately excluded: `current_fn_arm64_return_coercion`,
//! `current_fn_sret_param` and `current_fn_boxes_return`. Those read like
//! ABI state but are *per-function frame* values set at body entry — they
//! belong to cluster 14 (`FnCtx`), and pulling them here would split the
//! frame across two owners.
//!
//! Accessed as `self.target_abi.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::{HashMap, HashSet};

use inkwell::types::BasicTypeEnum;

use super::state;

/// Target-shape predicates and the ABI/layout adaptations derived from them.
pub(crate) struct TargetAbi<'ctx> {
    /// Niche-ABI record per function (wip-shared-struct-codegen-followups
    /// Slice 1). A function whose signature mentions `Option[shared T]`
    /// in return and/or parameter position is declared with a single
    /// nullable `ptr` (null = None, non-null = Some) at those positions
    /// instead of the conventional 4-i64 Option enum struct — closing
    /// the field-niche/call-ABI asymmetry and skipping the sret
    /// round-trip on every call. The function *body* still works on the
    /// conventional 4-word shape: `compile_function` unpacks niche
    /// params at entry, the return sites pack at `ret`, and
    /// `compile_call` packs args / unpacks the result, so every other
    /// codegen path (refcounting, pattern matching, RC-fallback
    /// analysis) is shape-blind to the ABI. Keyed by LLVM symbol name;
    /// names absent from the map (impl methods, closures, generic
    /// monos, coroutine ramps, extern decls) keep the conventional ABI.
    /// Eligibility is decided once in `declare_function`.
    pub(crate) fn_niche_abi: HashMap<String, state::NicheAbi>,
    /// Per-function record of which `bf16` signature positions are carried as
    /// `i16` on wasm (B-2026-08-30-42). See [`state::Bf16Abi`]. Empty on every
    /// non-wasm target, so the pack/unpack sites are inert there.
    pub(crate) fn_wasm_bf16_abi: HashMap<String, state::Bf16Abi>,
    /// Names of `pub extern "C" fn`s whose aggregate return is C-ABI
    /// auto-boxed (additive-interop Slice 4 Path B). Their LLVM signature
    /// returns a `ptr` (the heap box), not the `{data,len,cap}` value a
    /// Kāra caller's typecheck expects — so a call to one *from Kāra code*
    /// would read a garbage Vec. Such a boxed export is a C-facing surface;
    /// `compile_call` rejects an internal call with an actionable error
    /// (extract the body into a non-exported helper and call that).
    pub(crate) boxed_export_names: std::collections::HashSet<String>,
    /// Phase D headerless cluster density: fn key → member type name →
    /// link user-field index, for clusters whose analysis `headerless`
    /// flag is set (b2 + dual type-purity gate — see
    /// `ElidedCluster::headerless`). Within such a fn, every value of
    /// the member type is provably a cluster member, so the heap
    /// layout is keyed per `(fn, type)`: allocation drops the 8-byte
    /// rc header (`emit_headerless_alloc`), and every member-field GEP
    /// routes through `shared_gep_layout` to pick the headerless twin
    /// struct type at field base 0 instead of `heap_type` at base one.
    /// The link index rides along for the lazy niche-shape check in
    /// `headerless_here` (a non-niche link would make the free-walk's
    /// RcDec fallback reachable — structurally excluded by demoting).
    pub(crate) headerless_fns: HashMap<String, HashMap<String, usize>>,
    /// Phase C2b: ANALYSIS-side headerless-T candidates — member type →
    /// (link index, touching fn keys). Reconciled into
    /// `headerless_types` in `compile_program` once coroutine keys and
    /// struct layouts exist (a coro toucher or a non-niche link drops
    /// the type; every consumer keys on the reconciled set, so a drop
    /// deactivates the whole composition coherently).
    pub(crate) headerless_type_candidates: HashMap<String, (usize, Vec<String>)>,
    /// Headerless reshaper fns (bare name / `Type.method`) → the `dummy`
    /// sentinel binding name. At such a fn's scope exit codegen frees
    /// `dummy` as a single headerless node (`emit_headerless_reshaper_dummy_free`)
    /// — it is uniquely owned and NOT part of the returned chain
    /// (`dummy.<link>`), so it has no other cleanup and cannot double-free
    /// with the caller's free-walk. EXPERIMENTAL, populated only under
    /// `KARAC_HEADERLESS_RESHAPER`. See `elision::reshaper_dummy_binding`.
    pub(crate) headerless_reshaper_dummies: HashMap<String, String>,
    /// Phase C2b: the FINAL program-wide headerless set. A member type
    /// in here has no rc word anywhere — `headerless_here` answers true
    /// in every fn, builders allocate via `emit_headerless_alloc`, the
    /// borrowed-param exit decs and call-site arg incs are skipped, and
    /// the arg-sanctioned adopted families activate.
    pub(crate) headerless_types: HashSet<String>,
    /// Whether the build target is AArch64 — computed once at construction
    /// from the native triple (or `KARAC_FORCE_TARGET_ARCH`). Gates the AArch64
    /// `#[repr(C)]` struct-by-value ABI: HFA / ≤ 16 B register coercion, and the
    /// larger-than-16 B indirect/`sret` cases (B-2026-07-09-2).
    pub(crate) target_is_aarch64: bool,
    /// Whether the build target is x86-64 **System V** (Linux / macOS / BSD)
    /// — computed once at construction. SysV matches the raw-struct lowering
    /// for `#[repr(C)]` structs ≤ 16 B (eightbyte register classification, by
    /// luck), so those need no adaptation. A struct larger than 16 B is MEMORY
    /// class, which the raw lowering does NOT match — it gets a `byval` param
    /// / `sret` return (B-2026-07-09-2 Slice 3c). **Windows x64 is a distinct
    /// gate** (`target_is_windows_x86_64`); this flag is `false` there.
    pub(crate) target_is_x86_64: bool,
    /// Whether the build target is **Windows x64** (Microsoft x64) — computed
    /// once at construction. Distinct from `target_is_x86_64` (SysV): the
    /// Microsoft x64 aggregate ABI passes 1/2/4/8-byte aggregates in a single
    /// integer register (coerced to `iN`) and passes everything else by
    /// reference (plain `ptr`, caller-owned copy) with `sret` for non-POT
    /// returns — no eightbyte splitting, no HFA, no `byval` (B-2026-07-09-8).
    /// `false` outside Windows x64.
    pub(crate) target_is_windows_x86_64: bool,
    /// Per-function record of `#[repr(C)]` struct params coerced to an AAPCS
    /// register type on AArch64 (B-2026-07-09-2): fn name → `[(param_index,
    /// struct_name)]`. The declared LLVM param at `param_index` is the coerced
    /// type (`[N x i64]` / `[N x fp]` / `i64`); the body prologue reconstructs
    /// the original struct value from it. Empty on x86-64 (no coercion).
    pub(crate) arm64_coerced_struct_params: HashMap<String, Vec<(usize, String)>>,
    /// Per-function record of `#[repr(C)]` struct params passed **indirectly**
    /// (B-2026-07-09-2 Slice 3a/3c): fn name → `[(param_index, struct_name)]`.
    /// A struct larger than 16 B crosses the C boundary by pointer on both
    /// AArch64 (a plain `ptr` to a caller-owned copy) and x86-64 SysV (a `ptr
    /// byval(%Struct)`), so the declared LLVM param at `param_index` is `ptr`;
    /// the body prologue loads the struct value back through it. The `byval`
    /// attribute (x86-64 only) is attached after `add_function`. Distinct from
    /// `arm64_coerced_struct_params` (register coercion for ≤ 16 B, AArch64).
    pub(crate) indirect_struct_params: HashMap<String, Vec<(usize, String)>>,
    /// Exported fn names whose signature adapts a `#[repr(C)]` struct param or
    /// return to the target C ABI (register coercion, indirect `byval`, or
    /// `sret`). An internal Kāra call to one would need matching arg/return
    /// adaptation (not implemented), so `compile_call` rejects it with an
    /// actionable message — mirroring the boxed-export rejection.
    pub(crate) abi_adapted_export_names: std::collections::HashSet<String>,
    /// Per-function AArch64-coerced `#[repr(C)]` struct **return** type
    /// (B-2026-07-09-2 Slice 2): fn name → the coerced LLVM return type
    /// (`i64` / `[2 x i64]`). The declared return type is coerced; each return
    /// site reinterprets the struct value into it. HFA returns are absent (they
    /// return the raw struct). Empty on x86-64.
    pub(crate) arm64_coerced_struct_returns: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Per-function `sret` return (B-2026-07-09-2 Slice 3b/3c): fn name → the
    /// returned `#[repr(C)]` struct's LLVM type. A struct larger than 16 B is
    /// returned via `sret` on both AArch64 (x8) and x86-64 SysV (rdi): the LLVM
    /// signature drops the struct return (becomes `void`) and gains a leading
    /// `ptr sret(%Struct)` param; each return site stores the struct value
    /// through that pointer and `ret void`s. Empty for register/HFA returns.
    pub(crate) sret_struct_returns: HashMap<String, inkwell::types::StructType<'ctx>>,
}
