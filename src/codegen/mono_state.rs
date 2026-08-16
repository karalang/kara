//! Monomorphization state — generic fn bodies and the active substitution.
//!
//! Ninth slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). Groups the
//! state that drives generic instantiation:
//!
//! - `generic_fns` — the un-monomorphized ASTs, keyed by name;
//! - `generated_monos` — which instantiations have already been emitted,
//!   so each is compiled once;
//! - the **active substitution** for the instantiation being compiled:
//!   type params to LLVM types (`type_subst`), to type names
//!   (`type_subst_names`) and to `TypeExpr`s (`type_subst_type_exprs`),
//!   const params (`const_subst`), and layout params (`layout_subst`);
//! - the per-mono handle param infos and payload-binding type exprs.
//!
//! The substitution maps are swapped in and out around each instantiation
//! rather than being write-once, which is why several sibling modules
//! assign to them; grouping them makes that save/restore explicit at the
//! type level for whoever tackles a proper scoped-substitution API later.
//!
//! Named `mono_state` to avoid the sibling `mono.rs`, which holds the
//! monomorphization *behaviour* this data feeds.
//!
//! Accessed as `self.mono_state.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::{HashMap, HashSet};

use inkwell::types::BasicTypeEnum;

use super::state;
use super::state::LayoutId;
use crate::ast::{Function, TypeExpr};

/// Generic-instantiation state and the active substitution.
pub(crate) struct MonoState<'ctx> {
    // ── Generic monomorphization ──────────────────────────────────
    /// Generic function AST nodes keyed by name. Not compiled until instantiated.
    pub(crate) generic_fns: HashMap<String, Function>,
    /// Already-generated monomorphizations (mangled name → done). Prevents duplicate codegen.
    pub(crate) generated_monos: HashSet<String>,
    /// Active type-parameter substitution during a monomorphization pass.
    /// Maps generic param name (e.g. `"T"`) → concrete LLVM type.
    pub(crate) type_subst: HashMap<String, BasicTypeEnum<'ctx>>,
    /// Active const-parameter substitution during a monomorphization
    /// pass (const generics slice 4). Maps const-generic param name
    /// (e.g. `"N"`) → its bound `ConstValue`. Used by
    /// `compile_expr ExprKind::Identifier` to lower const-param
    /// references in generic bodies to LLVM constants of the matching
    /// width via `compile_primitive_const`, and by `Array[T, N]`
    /// element-size extraction sites to recover the size from a
    /// const-param reference. Slice 1b populates this map during
    /// `compile_generic_call`'s mango-key mango step; slice 4
    /// extends the save/restore around `compile_mono_function` so the
    /// body lowering sees the same bindings.
    pub(crate) const_subst: HashMap<String, crate::prelude::ConstValue>,
    /// Active type-parameter substitution during a monomorphization pass,
    /// as concrete type *names* (e.g. `"T"` → `"C"`) — the name-level twin of
    /// `type_subst` (which holds LLVM types). Populated in `compile_generic_call`
    /// from the typechecker's per-call `call_type_subs` frame (resolved through
    /// the caller's active name-subst so a nested generic call flattens the
    /// outer param), saved/restored around `compile_mono_function` exactly like
    /// `type_subst`. Consulted by the mono param prologue so a bare-type-param
    /// param (`x: X`) registers its receiver type as the concrete impl target
    /// (`var_type_names["x"] = "C"`), which is what `inferred_receiver_type`
    /// needs to dispatch a trait method called through the generic bound
    /// (`x.tag()` → `C.tag`; B-2026-07-03-11). LLVM types can't be reverse-mapped
    /// to a name safely — same-shape structs collide — so this is a distinct map.
    pub(crate) type_subst_names: HashMap<String, String>,
    /// Element-aware twin of `type_subst_names` (B-2026-07-13-2/-3): a generic
    /// param name → its FULL concrete `TypeExpr` at the active monomorphization
    /// (`"T"` → `Vec[i64]`, `Vec[String]`, …). `type_subst_names` is head-ONLY
    /// (`"Vec"`), which suffices for a scalar/String param (String carries no
    /// element) but DROPS a `Vec`/`VecDeque` element, so a bare-`T` param bound
    /// to a whole collection lost its element in the mono body: the param
    /// prologue's `register_var_from_type_expr(x, subst_monomorph_type_params(T))`
    /// reconstructed a bare `Vec` (no `[E]`) and never populated `vec_elem_types`
    /// → `owned_vecstr_params` missed `x` → the owned-param return deep-copy was
    /// skipped (double-free), and a generic-enum payload bind sized the payload
    /// at the erased scalar width (match-arm-type mismatch → invalid IR).
    /// Populated at the mono call site from the argument's registered element
    /// type, saved/restored around `compile_mono_function` exactly like
    /// `type_subst_names`, and consulted FIRST by `subst_monomorph_type_params`.
    /// Empty entry ⇒ fall back to the `type_subst_names` head-name path
    /// (unchanged for every non-collection param).
    pub(crate) type_subst_type_exprs: HashMap<String, crate::ast::TypeExpr>,
    /// Per-layout-monomorphization axis: callee param NAME → the `LayoutId`
    /// of the caller's argument at the active call site
    /// (`docs/spikes/per-layout-monomorphization.md`). Saved/restored around
    /// `compile_mono_function` exactly like `type_subst` / `const_subst`, fed
    /// to `mangle_mono_name` so each layout variant is a distinct LLVM symbol,
    /// and read by `active_layout_id` / `active_param_soa_layout` to lower a
    /// monomorph's SoA `Vec[E]` params and their access paths (slice 2).
    pub(crate) layout_subst: HashMap<String, LayoutId>,
    /// Handle-backed builtin (Column/Tensor) bindings for bare
    /// type-param params of generic monos, keyed by MANGLED mono name →
    /// `[(param_name, info)]`. Written by `compile_generic_call` (from
    /// the arg spans' `column_typed_exprs` / `tensor_typed_exprs`
    /// records), read by `compile_mono_function`'s prologue to register
    /// `column_var_infos` / `tensor_var_infos` for the param — see
    /// `state::MonoHandleArgInfo`. Module-lifetime (mangled keys are
    /// globally unique), so no per-mono save/restore.
    pub(crate) mono_handle_param_infos: HashMap<String, Vec<(String, state::MonoHandleArgInfo)>>,
    /// B-2026-07-13-3: monomorph-resolved concrete payload `TypeExpr` for a
    /// GENERIC enum's bare-type-param variant payload binding (`enum Opt[T] {
    /// Yes(T) }`, matched as `Opt.Yes(v)` at `T = String`). The typechecker
    /// records NOTHING for a `Type::TypeParam` binding (it never sees the
    /// concrete arg), so `pattern_binding_types` / `pattern_binding_inner_types`
    /// are both empty at the binding span — codegen would then size the payload
    /// at the erased 1-word default and load only the box pointer. Populated at
    /// the match-bind site (`bind_pattern_values`, TupleVariant arm) by
    /// substituting the enum's declared payload `TypeExpr` through the active
    /// monomorph substitution (`subst_monomorph_type_params`), and consulted —
    /// ONLY when the typechecker recorded no concrete surface type — by
    /// `pattern_payload_word_count` / `pattern_payload_llvm_type` (to trigger and
    /// size the debox unpack) and the Binding metadata path (to register the
    /// heap-owning binding's scope-exit free). Keyed by the sub-pattern's
    /// `(span.offset, span.length)`; refreshed on every monomorph's body compile
    /// and cleared per function.
    pub(crate) mono_payload_binding_type_exprs: HashMap<(usize, usize), TypeExpr>,
}
