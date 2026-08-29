//! Per-variable Option/Result/enum PAYLOAD tracking and ownership aliases.
//!
//! The tables that answer "where does this binding's payload live, and who
//! owns it": the inline Option/Result payload-var sets and their map/agg
//! variants, the boxed payload families (enum, struct, nested,
//! struct-field), the view-var sets (shared-enum and boxed-optres), the
//! deboxed box-pointer map, the passthrough-owner alias chain (plain,
//! boxed, nested-boxed), the plain-alias base/generic-param tables, and
//! `param_view_locals`. Extracted from `Codegen` as a cluster-15 sub-slice
//! of the state-decomposition spike; see
//! `docs/spikes/state-decomposition-codegen-methodcall.md`.

use std::collections::{HashMap, HashSet};

pub(crate) struct PayloadVars<'ctx> {
    /// Names of `Option[T]` bindings that registered a
    /// `CleanupAction::FreeInlineOptionPayload` (T is an inline heap
    /// `String`/`Vec`). A `match`/`if let` arm that binds the `Some`
    /// payload out of such a variable must zero the variable's `cap` word
    /// (option field 3) so the scope-exit free skips — the bound payload's
    /// own cleanup frees it once. Without this gate the suppression can't
    /// tell `Option[String]` (cap at w2, must suppress) from `Option[i64]`
    /// (no heap payload, nothing to suppress): the `Option` layout is
    /// type-erased. See B-2026-06-10-6.
    /// B-2026-08-06-27 — result-binding → source-binding, for a `let` whose RHS
    /// hands one of its armed arguments straight back
    /// (`call_passes_armed_inline_binding_through`). The result registers no
    /// cleanup there (the source stays sole owner), so a later disarm keyed on
    /// the RESULT's name — a consuming match arm's
    /// `suppress_inline_option_payload_cleanup` — must forward to the SOURCE,
    /// or the source's free runs over a payload the arm binding already owns.
    ///
    /// Measured, not assumed: with the registration skipped but no forwarding,
    /// a non-binding `Some(_)` arm is clean and a payload-BINDING arm still
    /// double-frees. That pair is what this map exists for.
    pub(crate) passthrough_owner_alias: std::collections::HashMap<String, String>,
    /// B-2026-08-06-9 leg A — the BOX-channel sibling of
    /// `passthrough_owner_alias`, kept SEPARATE on purpose.
    ///
    /// Same relation (passthrough result → the binding that owns the value),
    /// but consulted by exactly one site: the user-function argument disarm,
    /// which retracts the source's box with the WORD-scoped
    /// `suppress_boxed_enum_payload_cleanup_for_moved_arg`. Folding these
    /// entries into the map above was tried and breaks two shapes, because
    /// every consumer of that map disarms the WHOLE slot or a different
    /// channel: a `Result[Wide, String]` passthrough sent its Err-side inline
    /// disarm to the source and left the result's own armed (double free), and
    /// an `Option[Wide]` passthrough consumed by a callee had its source's box
    /// zeroed by a callee that does not own struct payloads (leak). Both are
    /// regression fixtures.
    ///
    /// Recorded only for `Option` with a boxed NON-STRUCT payload — the exact
    /// population whose box the callee now owns (`functions.rs`), so a disarm
    /// keyed here always has a taker.
    pub(crate) boxed_passthrough_owner_alias: std::collections::HashMap<String, String>,
    pub(crate) inline_option_payload_vars: std::collections::HashSet<String>,
    /// `Result[T, E]` sibling of `inline_option_payload_vars` — names of
    /// `Result` bindings that registered a `FreeInlineResultPayload` (the Ok
    /// and/or Err half is an inline heap `String`/`Vec`). A `match`/`if let`
    /// arm binding the `Ok`/`Err` payload out zeros the variable's `cap`
    /// word so the scope-exit free skips (the bound payload frees it once).
    /// See B-2026-06-10-6's Result follow-on.
    pub(crate) inline_result_payload_vars: std::collections::HashSet<String>,
    /// `Option[Map]`/`Option[Set]` sibling — names of `Option` bindings that
    /// registered a `FreeInlineOptionMapPayload`. A `match`/`if let` arm
    /// binding the `Some` payload out sets the source tag to `None` (no `cap`
    /// word to zero, unlike the Vec case) so the scope-exit free skips. See
    /// B-2026-06-10-6's `Option[Map]` follow-on.
    pub(crate) inline_option_map_payload_vars: std::collections::HashSet<String>,
    /// `Option[<user struct/enum>]` sibling — names of `Option` bindings whose
    /// `Some` payload is a NON-shared user struct/enum the recursive drop family
    /// frees (inline in the payload words, or heap-boxed when wider). Registered
    /// a `CleanupAction::EnumDrop` running `karac_drop_Option_<payload>` (the
    /// same tag-guarded fn the `Vec[Option[..]]` element path uses) on the slot.
    /// The generic `Option` drop switch is a no-op (type-erased) and the
    /// String/Vec-overlay `FreeInlineOptionPayload` doesn't cover a struct/enum
    /// payload, so a destructured-into-a-local `Option[Val]` leaked its payload
    /// (B-2026-07-03-27). A `match`/`if let` arm binding the `Some` payload out
    /// sets the source tag to `None` (like the `Option[Map]` case — no `cap`
    /// word) so the scope-exit drop skips and the bound payload frees it once.
    pub(crate) inline_option_agg_payload_vars: std::collections::HashSet<String>,
    /// Names of `Option`/`Result` bindings whose wide payload was heap-BOXED
    /// (`track_boxed_enum_var` registered a `CleanupAction::BoxedEnumDrop` —
    /// `Option[Block]` and other `Option[Wide]` / `Result[Wide,_]`). The
    /// boxed sibling of `inline_option_payload_vars`: when such a binding is
    /// moved WHOLE into a struct-literal / enum-variant field, the field now
    /// owns the box, so the source slot must be zeroed (`BoxedEnumDrop` guards
    /// on `tag == Some` at word 0) — otherwise the source frees the box the
    /// destination still references downstream → UAF (selfhost slice 3c-iv:
    /// `TraitMethodNode { body, .. }` for `let mut body = Some(parse_block())`).
    pub(crate) boxed_enum_payload_vars: std::collections::HashSet<String>,
    /// B-2026-08-28-66 — the payload STRUCT name recorded alongside each
    /// `boxed_enum_payload_vars` entry.
    ///
    /// A destructuring arm (`Some(Holder { name, id })`) carries the struct's
    /// name in the pattern, so the field-disarm walk reads it from there. A
    /// WHOLE-payload binding (`Some(r)`) does not, and that is exactly the arm
    /// this row is about — so the name has to come from the registration
    /// instead. `track_boxed_enum_var` already receives it as
    /// `inner_struct_name`.
    pub(crate) boxed_enum_payload_struct: std::collections::HashMap<String, String>,
    pub(crate) boxed_struct_payload_vars: std::collections::HashSet<String>,
    /// B-2026-08-06-32 — bindings carrying a `NestedBoxedEnumDrop`, i.e. a box
    /// living inside the binding's INLINE payload area
    /// (`Result[Option[Wide], E]`).
    ///
    /// Kept SEPARATE from `boxed_enum_payload_vars` above rather than folded
    /// into it, and the separation is the safety property. That set's members
    /// are subject to move-suppression: a whole-value move zeroes the source so
    /// the destination can become the box's owner. No destination takes a
    /// NESTED box over — a struct-literal move, a `Vec.push` and a by-value
    /// call were each measured leaking it — so a nested binding must NOT be
    /// suppressed on a move, and joining that set would disarm the only owner
    /// there is. This set exists purely so the passthrough rule below can ask
    /// "is this source armed?" without granting membership in the move rules.
    pub(crate) nested_boxed_payload_vars: std::collections::HashSet<String>,
    /// B-2026-08-12-15 — the SUBSET of `nested_boxed_payload_vars` above whose
    /// box lives inside a FIELD of an inline user-STRUCT payload
    /// (`Result[W, i64]` over `struct W { o: Option[Option[i64]] }`) rather
    /// than inside an inline `Option`/`Result` payload.
    ///
    /// Exists for one question, at one site: whether a by-value call should
    /// RETRACT the caller's registration. For the parent population it must —
    /// the callee's owned non-escaping param registers its own
    /// `NestedBoxedEnumDrop`, so leaving the caller armed makes two owners.
    /// For this subset it must not: the callee deliberately registers nothing
    /// (see the `functions.rs` loop's note — the arm that binds the struct out
    /// runs `__karac_drop_struct_<T>` and is already the callee-side owner), so
    /// retracting hands the caller's box to nobody and it leaks, which is the
    /// bug this row is.
    ///
    /// Exactly the role `boxed_struct_payload_vars` plays for the DIRECT-box
    /// family one line up in that same arg loop, and for the same underlying
    /// asymmetry: a STRUCT payload has a callee-side move-out mirror and a bare
    /// enum payload does not.
    pub(crate) struct_field_boxed_payload_vars: std::collections::HashSet<String>,
    /// B-2026-08-06-32 — result binding of a passthrough call → the binding
    /// that actually owns its nested box (`let back = id(b)` ⇒ `back → b`).
    ///
    /// The result registers nothing (the source stays sole owner, as in
    /// B-2026-08-06-21), but a CHAIN would then see an unarmed argument and
    /// register a second owner for the same box — a double free, measured at
    /// `-O0` on `let r1 = id(b); let r2 = id(r1);`. Recording resolves one hop
    /// so every stored value is a genuinely armed owner, which keeps the lookup
    /// a single hop and needs no walk over a possibly-cyclic map. Same shape as
    /// `boxed_passthrough_owner_alias`, and separate for the same reason as the
    /// set above.
    pub(crate) nested_boxed_passthrough_owner_alias: std::collections::HashMap<String, String>,
    /// PLAIN type alias name → its base `TypeExpr` (`type Name = String;` →
    /// the `String` type expr). The `where`-free sibling of
    /// `refinement_bases`, populated from the same `Item::TypeAlias`es and
    /// consulted at exactly the same layout / dispatch sites — a plain alias
    /// is *more* transparent than a refinement (no nominal identity at all,
    /// no predicate, no `try_from`), so every place that peels a refinement
    /// to its base must peel a plain alias too.
    ///
    /// B-2026-07-30-7: without this map a plain alias hit the `i64`
    /// unknown-name fall-through in `llvm_type_for_name`, so `type Plain =
    /// Vec[i64]; fn total(xs: Plain)` passed `karac check` and `karac run
    /// --interp` (the typechecker lowers a plain alias transparently, see
    /// `env_build::env_add_type_alias`) and then failed LLVM module
    /// verification under `karac build` — a `{ptr, i64, i64}` argument
    /// against an `i64` parameter. The paradox that localized it: adding a
    /// `where` clause *fixed* the program, because only the refinement arm
    /// had a base map. Integer-shaped aliases (`type Count = i64`) stayed
    /// invisible for the same reason the fall-through exists — `i64` happens
    /// to be the right layout.
    pub(crate) plain_alias_bases: HashMap<String, crate::ast::TypeExpr>,
    /// Plain type alias name → the ordered names of its generic parameters
    /// (`type Plain[T] = Vec[T];` → `["T"]`). The `where`-free sibling of
    /// `refinement_generic_params`; see `resolve_type_alias_te`, which zips
    /// these against the use-site generic args so `Plain[i64]` resolves to
    /// `Vec[i64]` (correct element type) rather than `Vec[T]`. Empty for a
    /// non-generic alias.
    pub(crate) plain_alias_generic_params: HashMap<String, Vec<String>>,
    /// B-2026-07-09-12 clone-on-extract — names of struct-typed bindings that are
    /// a by-value VIEW of a shared-enum RC box's inline payload (`match e { Call(c)
    /// => … }`, `c` aliasing the box's `CallNode`). Mapped to the payload struct
    /// type. Unlike a callee-owned struct (which carries its own `StructDrop`), a
    /// view is UNTRACKED — the box's rc-drop is the sole owner. When such a view is
    /// destructured (`let CallNode { callee, args } = c`) the extracted leaves
    /// alias the box's heap; `finish_owned_struct_destructure` consults this map to
    /// DUPLICATE each moved-out heap child (deep-copy a buffer, rc-inc a shared
    /// handle) so the leaf owns it independently and the box's drop does not
    /// double-free. Populated in `pattern_binding.rs` at the view bind; cleared
    /// per-function alongside `owned_struct_params`.
    pub(crate) shared_enum_payload_view_vars: std::collections::HashMap<String, String>,
    /// B-2026-08-04-2 — whole-payload match bindings that are VIEWS of a
    /// heap-BOXED `Option`/`Result` payload: `name -> scrutinee slot`.
    ///
    /// The binding is an unboxed COPY of the box's `{ptr,len,cap}` words and
    /// registers no memory drop of its own — by design, since the box drop's
    /// inner walk owns the interior (that is what keeps `if let Some(r) =
    /// v.pop()` from double-freeing). But when the binding then MOVES — into a
    /// struct literal, out as the match's tail value, into `let x = r`, into a
    /// container — the destination takes ownership of exactly those buffers and
    /// frees them too. Recording the view lets each move site neutralize the
    /// box's inner walk, leaving the destination the sole owner.
    ///
    /// Keyed by binding name and snapshotted with the rest of the per-arm var
    /// environment, so an arm's view cannot leak into a sibling arm.
    pub(crate) boxed_optres_payload_view_vars: HashMap<String, inkwell::values::PointerValue<'ctx>>,
    /// B-2026-08-06-10 — match-arm payload bindings that were DEBOXED out of an
    /// enum payload box: `binding slot -> box pointer`.
    ///
    /// A payload wider than the variant's payload area is heap-boxed by
    /// `coerce_to_payload_words`, and `reconstruct_payload_value` reads it back
    /// by LOADING the whole box into the binding's own slot. The binding is
    /// therefore a private COPY, and that is the whole problem: a move-out of
    /// one of its fields zeroes a `cap` in the copy, where the box's owner
    /// cannot see it.
    ///
    /// Within one frame that is fine — the owner is a `BoxedEnumDrop` action and
    /// `suppress_boxed_payload_view_move` retracts its inner walk. ACROSS A CALL
    /// it is not: the owner is the caller's synthesized drop fn reading the box's
    /// DATA, and a retraction in the callee's cleanup queue is invisible to it.
    /// A data write is the only channel, so the move-out neutralizers mirror
    /// their zero through this pointer.
    ///
    /// Keyed by SLOT rather than name deliberately. The pointer is an
    /// `inttoptr` emitted in the arm's own block, so it must never be reached
    /// from a later, unrelated binding of the same name — a fresh binding has a
    /// fresh alloca, which makes a stale entry structurally unreachable instead
    /// of merely unlikely. Cleared per function.
    pub(crate) deboxed_payload_box_ptrs:
        HashMap<inkwell::values::PointerValue<'ctx>, inkwell::values::PointerValue<'ctx>>,
    /// B-2026-08-18-4 — the DEFERRED sibling of `deboxed_payload_box_ptrs`,
    /// for the one shape that map deliberately refuses: a payload box that a
    /// user `Drop` BODIES walk still has to read.
    ///
    /// Writing the move's neutralizing zero through the box at the MOVE SITE
    /// is what `deboxed_payload_box_ptrs` does, and it is wrong here — the
    /// re-homed `__karac_dropelems_opt_*` walk fires LATER (at the binding's
    /// death) and would read the zeroed field, which is exactly the measured
    /// regression B-2026-08-06-10's comment records: a double free traded for
    /// a user Drop body printing an empty string. Retraction cannot serve
    /// either, because it is whole-action and a single moved FIELD leaves the
    /// box's drop responsible for every field the move did not take.
    ///
    /// So the zero is neither written early nor skipped: it is QUEUED here and
    /// emitted between the two readers — after the bodies walk, before the
    /// box's memory drop. Both readers then see what they need, which is the
    /// per-field analogue of B-2026-08-04-2's whole-action retraction.
    ///
    /// Keyed by binding NAME rather than slot, because the drain site is the
    /// `CleanupAction::UserDrop` arm and the name is what that action carries.
    /// Cleared per function.
    pub(crate) deferred_payload_box_ptrs: HashMap<String, inkwell::values::PointerValue<'ctx>>,
    /// The per-field neutralizations queued against [`Self::deferred_payload_box_ptrs`],
    /// drained immediately after the binding's payload-BODIES walk. Cleared per
    /// function.
    pub(crate) pending_box_field_zeroes: HashMap<String, Vec<PendingBoxFieldZero<'ctx>>>,
    /// B-2026-08-01-15 — locals that are whole-move REBINDS of an owned
    /// param (`let h2 = h;`), transitively. A destructure or match on one
    /// is a param-view bind exactly like the direct param case
    /// (`scrutinee_is_owned_param_binding` consults this), and its own
    /// let-site registration is memory-only. Cleared per-function.
    pub(crate) param_view_locals: HashSet<String>,
}

/// One queued per-field neutralization against a payload box whose user `Drop`
/// bodies walk has not run yet. See
/// [`PayloadVars::pending_box_field_zeroes`].
///
/// The struct LLVM type is carried rather than re-resolved at the drain site:
/// it is a type, not an SSA value, so it stays valid anywhere in the function,
/// and re-resolving would consult the ACTIVE monomorph subst — which at the
/// drain point is whatever the surrounding cleanup happens to be under, not the
/// move site's. That is the same "hand the slot's own layout down" discipline
/// B-2026-08-06-2 established for `zero_struct_field_move_cap_in`.
pub(crate) struct PendingBoxFieldZero<'ctx> {
    pub(crate) box_ptr: inkwell::values::PointerValue<'ctx>,
    pub(crate) struct_name: String,
    pub(crate) field: String,
    pub(crate) st: Option<inkwell::types::StructType<'ctx>>,
    pub(crate) inst: Option<crate::ast::TypeExpr>,
}
