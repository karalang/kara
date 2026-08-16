//! RC-elision hint tables, consumed from the ownership pass.
//!
//! The fn-keyed products of `src/ownership/elision.rs` (phase A single-owner
//! elision, phase B1 cluster walks, phase B2 build-side roles) plus the
//! per-fn borrowed-param skip sets and the weak-edge target types: which
//! shared bindings may skip refcount traffic entirely, which cluster roots
//! swap their cleanup for a walk, and which types carry a weak back-edge.
//! Plain analysis data, no `'ctx`. Extracted from `Codegen` as a cluster-15
//! sub-slice of the state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use std::collections::{HashMap, HashSet};

use super::state;

pub(crate) struct RcElision {
    /// RC elision phase A (`src/ownership/elision.rs`; design record in
    /// phase-7-codegen.md): per-function sets of shared bindings whose
    /// refcount provably never exceeds 1. The let-site queues a
    /// `FreeSharedElided` cleanup (unconditional null-guarded free)
    /// instead of `RcDec` for these. Keyed by fn key (bare name /
    /// `Type.method`), matching `current_fn_name`.
    pub(crate) elided_bindings: HashMap<String, HashSet<String>>,
    /// Phase B1 cluster roots: fn key → root binding → (member struct
    /// name, link user-field index). The let-site swaps the root's
    /// cleanup for `FreeClusterWalk`. Cursors and fresh nodes keep
    /// their standard cleanups (drop-side-only consumption).
    pub(crate) elided_cluster_roots:
        HashMap<String, HashMap<String, (String, usize, crate::ownership::ReturnedChain)>>,
    /// Phase B2 build-side elision: fn key → cluster binding →
    /// role/cluster record. Populated only for clusters whose analysis
    /// `b2` flag is set (displacement-free canonical shapes). Consulted
    /// by the let-site shared/option arms, both Assign arms, and the
    /// dedicated link-store fast path.
    pub(crate) elided_b2_bindings: HashMap<String, HashMap<String, state::B2Binding>>,
    /// Phase C1c caller adoption: fn key → adopted root binding →
    /// (member type, link user-field index), for clusters whose
    /// analysis `adopted` flag is set. The root is an `Option[shared
    /// T]` binding born from a fresh-return builder call; its let-site
    /// queues a `FreeClusterWalkOption` cleanup instead of the
    /// `RcDecOption` dec-walk (and skips `var_option_shared_heap`
    /// registration — adopted roots are never reassigned, the analysis
    /// poisons that). Kept separate from `elided_cluster_roots` so the
    /// literal-cluster let-site/transfer paths never see adopted roots.
    pub(crate) adopted_cluster_roots: HashMap<String, HashMap<String, (String, usize)>>,
    /// Whole-program set of shared types that are the target of any `weak T`
    /// field. Computed in `build_struct_types` by scanning every struct field
    /// for `TypeKind::Weak(inner)`. Members are force-headed (excluded from
    /// `headerless_types` at reconcile) and get the two-word `{ strong, weak,
    /// fields… }` control box; `shared_gep_layout` returns base 2 for them and
    /// the box free routes through `karac_weak_box_strong_zero_release`. Empty
    /// for all code today (`weak` fields are declaration-only until the codegen
    /// store/read slices), so this whole layout path is inert. See
    /// `docs/spikes/weak-refs.md` (B-2026-07-19-8).
    pub(crate) weak_targeted_types: HashSet<String>,
    /// Phase C2b: adopted families that used the sanctioned-arg channel
    /// — active ONLY when their member type is in `headerless_types`
    /// (otherwise the binding falls back to full RC and the ordinary
    /// arg-inc / exit-dec balance applies).
    pub(crate) conditional_adopted_roots: HashMap<String, HashMap<String, (String, usize)>>,
    /// Phase C2b: borrowed-param records per fn — (param name, position,
    /// member type). Drives the callee-side exit-dec skip (by name, in
    /// `compile_function`) and the call-site arg-inc skip (by position,
    /// in the direct-call arg loop) — both gated on `headerless_types`.
    pub(crate) borrowed_param_skips: HashMap<String, Vec<(String, usize, String)>>,
}
