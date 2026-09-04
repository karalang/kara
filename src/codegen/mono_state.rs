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

/// The four TYPE-substitution axes of one monomorphization, bundled so an
/// entry point can hand them to [`Codegen::ensure_mono_generated`] as a unit.
///
/// They are exactly the four `MonoState` fields a mono body reads to resolve a
/// bare `T` — `type_subst` (the LLVM type), `type_subst_names` (the head name),
/// `type_subst_type_exprs` (the element-aware `TypeExpr`) and
/// `type_subst_call_te` (the typechecker's exact per-arg `TypeExpr`). Grouping
/// them keeps the entry point's signature honest: a caller that populates one
/// axis and forgets another gets a body that resolves `T` inconsistently
/// depending on which channel a given lowering site asks.
///
/// `default()` is the empty substitution, which is what a per-LAYOUT monomorph
/// of a non-generic function wants — it has no type params at all, and every
/// axis must be CLEARED so a stale outer substitution cannot leak in.
///
/// [`Codegen::ensure_mono_generated`]: super::Codegen::ensure_mono_generated
#[derive(Default)]
pub(crate) struct MonoTypeAxes<'ctx> {
    pub(crate) subst: HashMap<String, BasicTypeEnum<'ctx>>,
    pub(crate) subst_names: HashMap<String, String>,
    pub(crate) subst_type_exprs: HashMap<String, TypeExpr>,
    pub(crate) subst_call_te: HashMap<String, TypeExpr>,
}

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
    /// The typechecker's EXACT per-call type-arg `TypeExpr` for the active
    /// monomorph (`span_tables.call_type_subs_te` at the call span, flattened
    /// through the caller's own substitution). B-2026-08-31-39.
    ///
    /// Sits BETWEEN the two maps above in `subst_monomorph_type_params`'s
    /// precedence, and that position is the whole design. It OVERRIDES
    /// `type_subst_names`, which is head-only and therefore wrong rather than
    /// merely incomplete for a nested generic (`T = Vec[i64]` resolves a bare
    /// `T` to `Vec`, an elementless container no program can write). It YIELDS
    /// to `type_subst_type_exprs`, which is equally exact and is written by
    /// resolvers reading the caller's LIVE var side-tables — those see
    /// instantiations no per-call record can, notably a nested generic call
    /// whose binding `record_call_type_subs` drops as self-referential.
    ///
    /// Deliberately NOT consulted by the mangle. The two axes
    /// (`append_collection_type_param_mangle`,
    /// `append_structural_type_param_mangle`) read `type_subst_type_exprs`
    /// directly, and the typechecker already feeds the mangle its own exact
    /// channel (`call_type_subs_mangle`, widened to the nameless aggregates by
    /// B-2026-08-31-48). Routing this map into those axes would change existing
    /// symbols for a fix that is entirely about resolving types INSIDE a body.
    ///
    /// Saved/restored around `compile_mono_function` exactly like
    /// `type_subst_type_exprs`.
    pub(crate) type_subst_call_te: HashMap<String, crate::ast::TypeExpr>,
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

    /// B-2026-08-31-39 — the DISPLAY twin of `mono_payload_binding_type_exprs`,
    /// keyed by BINDING NAME rather than span.
    ///
    /// The renderer answers "what shape is this operand?" from span-keyed
    /// tables (`display_array_types` / `display_slice_types` / …) that
    /// `lowering.rs` fills off the typechecker's `expr_types`. Inside a generic
    /// fn the typechecker sees `t: T`, so every USE of a bare-`T` payload
    /// binding misses those tables and falls to the value-kind arms — which
    /// printed an `Array`'s first element and a `Slice`'s data pointer, and
    /// refused a slice outright at depth 0. The span-keyed sibling cannot help:
    /// it is keyed by the PATTERN's span, and the tables want the span of each
    /// interpolation hole, which the bind site never sees.
    ///
    /// So the concrete payload type is recorded under the binding's NAME here
    /// and the display entry points seed the span-keyed tables lazily, at the
    /// first use they cannot otherwise resolve. Retracted on any
    /// re-registration of the name (`register_var_from_type_expr`), the same
    /// shadow rule `slice_alias_md` documents — a shadowing local must not
    /// inherit the payload binding's shape.
    pub(crate) mono_payload_binding_display_types: HashMap<String, TypeExpr>,
}
