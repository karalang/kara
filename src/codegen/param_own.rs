//! #14 — Callee-ownership for by-value aggregate (`struct` / `enum`)
//! parameters.
//!
//! ## The bug
//!
//! Codegen passes a by-value aggregate argument as a SHALLOW copy (the
//! struct/enum words, including any heap `ptr`) under a *caller-retains*
//! model: the caller's source binding frees the buffers at its scope exit,
//! and the callee's param frees nothing. That model is sound only when the
//! callee *consumes-and-frees* (destructures) or *ignores* the value. It
//! double-frees when the callee **transfers the value OUT** — moves the param
//! into its return value (directly, or wrapped into a returned struct/enum
//! literal). Then the caller's source binding AND the returned value alias the
//! same buffer, and BOTH free it (`exit 133`).
//!
//! ## Why not move-by-default
//!
//! The "proper" fix — have the caller MOVE the arg (suppress its source drop)
//! and the callee OWN the param — is unsound here because Kāra's move-checker
//! does NOT reject double-consume / use-after-move: `take(x); take(x)` and
//! `take(x); println(x.f)` both compile and run correctly TODAY under
//! caller-retains. Caller-side move would turn those into use-after-frees.
//!
//! ## The fix: entry deep-copy + callee-owned drop
//!
//! At function entry, deep-copy the owned aggregate param's heap-field buffers
//! so the callee owns buffers INDEPENDENT of the caller's retained originals,
//! then register the param's scope-exit drop. The param now behaves exactly
//! like a `let`-bound local owned binding, so ALL existing local
//! move-suppression (tail return, struct/enum-literal consume, match
//! destructure, pass-as-arg) applies to it for free. Result: the caller frees
//! its original once; the callee frees its copy once (or suppresses that drop
//! when the copy is transferred out, leaving the destination the sole owner).
//! No caller-side change, hence no move-checker dependency — `take(x); take(x)`
//! keeps working (each call copies at entry).
//!
//! ## Depth discipline
//!
//! The entry copy MIRRORS the registered drop's depth EXACTLY. Both the struct
//! drop (`emit_struct_drop_synthesis`) and the enum drop
//! (`emit_enum_drop_switch`) free OUTER Vec/String buffers only — a nested
//! `Vec[heap_T]`'s elements are a bounded leak on both sides, never corruption
//! — recursing into nested structs/tuples. So the copy is an outer-buffer
//! copy (`emit_vecstr_defensive_copy` with `elem_te = None`, no element
//! recursion) per Vec/String field/payload, recursing into nested
//! structs/tuples.
//!
//! ## Bail conditions (left on caller-retains — never a regression)
//!
//! Any aggregate whose drop frees buffers this routine can't soundly duplicate
//! is left untouched (returns `false`): Map/Set handles, HTTP side-table
//! handles (`Response`/`RequestBuilder`), shared (RC) types, and `Result`
//! fields plus the `Option` payloads this routine can't yet duplicate
//! (boxed-wide, struct/enum-inline, plain-enum = B-27). A non-shared user-ENUM
//! field IS supported (#19, 2026-06-12): the struct drop frees its live-variant
//! `VecOrString` payload (post-#15/#18) and `deep_copy_one_aggregate_field`
//! duplicates exactly that via `deep_copy_enum_heap_payload_in_place`, keeping
//! copy and drop symmetric. An `Option[String]` / `Option[Vec[..]]` field (an
//! inline `{ptr,len,cap}` payload) IS supported too (B-2026-07-03-28 Facet A,
//! 2026-07-03): `deep_copy_option_inline_payload_in_place` duplicates the `Some`
//! buffer type-aware off the field `TypeExpr`, symmetric with the struct drop's
//! `OptionInline` free (which is gated on this very copy-supported predicate).
//! Bailing on the rest preserves today's exact behavior for those shapes.

use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::PointerValue;
use inkwell::{AddressSpace, IntPredicate};
use std::collections::HashMap;

use crate::ast::{
    Block, Expr, ExprKind, GenericArg, ImplItem, Item, PatternKind, SelfParam, TraitItem, TypeExpr,
    TypeKind,
};

/// Whole-body wrapper over [`crate::deque_head::expr_may_take_struct_field`]:
/// could this function body take ownership of `binding`'s `field`? Statements
/// are reached through the shared statement walker so a move buried in a `let`
/// initializer, an assignment RHS, or a nested block is not missed.
/// B-2026-08-08-6.
fn body_may_take_field(body: &Block, binding: &str, field: &str) -> bool {
    let mut found = false;
    for s in &body.stmts {
        crate::rc_elide::walk_stmt_children_pub(s, &mut |e| {
            found = found || crate::deque_head::expr_may_take_struct_field(e, binding, field);
        });
    }
    if let Some(e) = &body.final_expr {
        found = found || crate::deque_head::expr_may_take_struct_field(e, binding, field);
    }
    found
}

use super::state::{EnumDropKind, EnumLayout};

impl<'ctx> super::Codegen<'ctx> {
    /// Make an owned by-value aggregate parameter callee-owned: emit the entry
    /// deep-copy of its heap fields and register its scope-exit drop. Returns
    /// `true` if ownership was taken; `false` if the param was left on the
    /// caller-retains model (no copy, no drop — status quo). See the module
    /// doc for the full rationale.
    ///
    /// Takes the param's declared
    /// generic INSTANTIATION (`Box[String]` for `fn sink(b: Box[String])`),
    /// recorded per-binding in `enum_inst_var_types` at the param-registration
    /// site. It is threaded to the own-by-transfer arm below, whose drop is
    /// otherwise synthesized against the ERASED declaration and emits nothing
    /// (B-2026-08-05-33 predicate (a)).
    ///
    /// `inst` reaches ONLY that arm's drop, never an arm-selection predicate —
    /// every one of those is name-keyed. So passing it corrects a drop's
    /// LAYOUT without moving the ownership decision, which is what makes it
    /// safe to thread from a call site that previously passed `None`.
    ///
    /// A MONOMORPH BODY NEEDS IT TOO, and the belief that it did not was
    /// B-2026-08-07-18. The active subst is keyed by the enclosing FN's
    /// type-param names, so it resolves the struct's fields only when the two
    /// happen to agree — `fn f[T](x: Mix[T])` yes, `fn f[U](x: Mix[U])` no.
    /// On the diverging spelling the entry-copy arm declines, own-by-transfer
    /// takes it, and with `None` its drop reads the base layout: for
    /// `Mix[T] { v: T, s: String }` that is field 0's LENGTH word, freed as a
    /// pointer. `inst` binds the struct's params POSITIONALLY instead, so the
    /// name coincidence stops mattering. `None` remains correct only where
    /// there is no generic instantiation to resolve.
    pub(super) fn make_aggregate_param_callee_owned_inst(
        &mut self,
        type_name: &str,
        slot: PointerValue<'ctx>,
        inst: Option<TypeExpr>,
        param_name: &str,
    ) -> bool {
        self.make_aggregate_param_callee_owned_transfer(type_name, slot, inst, None, param_name)
    }

    /// [`Self::make_aggregate_param_callee_owned_inst`] plus the B-2026-08-29-63
    /// transfer decision.
    ///
    /// `transfer_param` is `Some(param name)` when the whole-program prepass
    /// ([`super::param_transfer`]) proved that EVERY call site hands this
    /// parameter a binding the caller owns and never reads again, so there is no
    /// original left to protect and the entry copy is pure cost — measured at
    /// the full heap content of the argument, and 1.96x wall-clock on a hot
    /// by-value loop. The NAME rides along because a transferred param that
    /// carries a user `Drop` registers its wrapper under that name, and the
    /// existing move-out retractions (`suppress_user_drop_for_var`) are
    /// name-keyed.
    ///
    /// It reaches ONLY the copy-supported struct arm. The other arms are
    /// unaffected on purpose: own-by-transfer is already what the
    /// copy-UNSUPPORTED arm does (B-2026-08-05-33), and the enum arm keeps its
    /// payload copy because its caller-side predicates
    /// (`arg_is_entry_copied_heap_enum` and the Option/Result family) still
    /// reason from "the callee's copy is independent" and are not part of this
    /// slice.
    pub(super) fn make_aggregate_param_callee_owned_transfer(
        &mut self,
        type_name: &str,
        slot: PointerValue<'ctx>,
        inst: Option<TypeExpr>,
        transfer_param: Option<&str>,
        param_name: &str,
    ) -> bool {
        // The prepass's permission is about call-site SHAPES; this is the TYPE
        // half, and both must hold. Keeping them in one predicate is what lets
        // the caller's retraction and this prologue agree without re-deriving
        // the conditions separately.
        let transfer = transfer_param.is_some() && self.struct_param_transfer_eligible(type_name);
        let transfer_param = if transfer { transfer_param } else { None };
        // #17 — the seeded std.tracing builder value types (`LogEvent` / `Span`
        // / `SpanField`) used to be name-excluded here. Their chained builder
        // methods (`info(..).with_field(..).with_field(..).in_span(..)`) move
        // individual `self` fields into returned literals, and engaging
        // entry-copy on top of the caller-retains `owned_struct_params` field-move
        // band-aid double-copied / emptied the chained fields. That redundancy is
        // now resolved generally: (gap 1) `compile_function` retires the
        // `owned_struct_params` band-aid for a callee-owned param, and (gap 2)
        // `compile_struct_init` cap-zeros a slot-sourced Vec/String/enum field
        // moved into a returned literal. With both in place these types are
        // callee-owned like any other aggregate — no name exclusion needed.
        // Non-shared user STRUCT.
        if self.type_decls.struct_types.contains_key(type_name)
            && !self.type_decls.shared_types.contains_key(type_name)
        {
            if !self.aggregate_param_copy_supported_struct(type_name, &mut Vec::new()) {
                // B-2026-07-18-31/-32 — a GENERIC struct param whose fields are
                // bare type params (`Pair[T] { a: T, b: T }`) fails the base
                // copy-support check: `field_copy_supported` sees the erased `T`
                // and bails at its conservative `_ => false` arm, so the param
                // stays caller-retains. But the callee still MOVES those fields
                // out into a returned literal (`Pair { a: p.b, b: p.a }`) —
                // aliasing the caller's buffers, which both the caller's own
                // drop and the returned value then free (double-free, masked at
                // -O2 but live under `karac run`/-O0). When an active monomorph
                // subst resolves the params to copy-supported heap types AND the
                // slot is the concrete mono layout, entry-copy the mono heap
                // fields so the param is callee-owned exactly like its concrete
                // twin (`struct PairS { a: String, b: String }`), which never
                // had this bug. The mono entry-copy GEPs the CONCRETE layout, so
                // it is offset-correct for any field count — unlike the base
                // bare-`T`-reinterpret drop path (B-2026-07-15-11), which stays
                // single-field-gated.
                if self.try_make_generic_struct_param_callee_owned(type_name, slot) {
                    return true;
                }
                // B-2026-08-05-33 — OWN BY TRANSFER when no copy is possible.
                //
                // The previous attempt at this row tried to keep the CALLER's
                // drop and left the callee alone; that double-freed as soon as
                // the callee moved a field out (`let s = b.v`), because
                // caller-retains says how the param ARRIVES, not what the body
                // does with it. This takes the other side: the caller already
                // retracts its drop for exactly this shape
                // (`move_declined_copy_struct_arg`), i.e. the value was MOVED
                // in, so the callee can simply take the original buffers. A
                // copy was never the point — `deep_copy_…` exists to leave the
                // caller's original intact, and there is no original to
                // protect once ownership transferred.
                //
                // Registering the drop WITHOUT the copy is what
                // copy-supported params already do one line apart; this is that
                // pair minus the copy. A field the body moves out is excluded
                // by the same cap-zeroing move-suppression the copy-supported
                // path relies on, which is why the `let s = b.v` shape balances
                // here where keeping the caller's drop did not.
                //
                // Held in LOCKSTEP with the caller's retraction, which is the
                // whole safety argument. Excluded: a shared-owning struct,
                // where B-2026-08-05-32 made the CALLER keep its drop — owning
                // here too would rc-dec twice. Self-referential structs are
                // excluded as well: there the callee may store the alias into
                // an owning container (B-2026-07-28-3), so a param drop could
                // free what the container now owns. That leak stays, unchanged.
                //
                // The drop is registered at the param's DECLARED instantiation
                // (`inst`), not by bare name. A generic wrapper taken at a
                // concrete arg — `fn sink(b: Box[String])`, predicate (a) —
                // sits in a CONCRETE fn, so there is no active monomorph subst
                // for the synthesizer to resolve `T` through; keyed by name
                // alone it classifies the erased field as no-heap, emits no
                // `__karac_drop_struct_Box` at all, and `track_struct_var`
                // silently registers nothing. The declared param type carries
                // exactly the missing binding.
                if !self.struct_owns_shared_field(type_name, &mut Vec::new())
                    && !self.struct_is_self_referential(type_name)
                {
                    // B-2026-09-04-4 — the same split the `transfer_param` arm
                    // above makes, for the one type class that could not reach
                    // it: a GENERIC struct with an `impl[T] Drop for S[T]`.
                    //
                    // Its bare `karac_drop_<T>` wrapper does not exist and never
                    // will (the mono pipeline instantiates from call sites and
                    // `drop` has none), so `user_drop_wrapper_fns` has no
                    // bare-name key and the arm above cannot fire. The
                    // PER-MONOMORPH wrapper can be built from the param's
                    // declared instantiation, and it calls this same field walk
                    // internally — so it REPLACES the registration below rather
                    // than adding to it, exactly as the transfer arm says.
                    //
                    // Gated on the bare wrapper being ABSENT, which is precisely
                    // the class this fix created: a non-generic `Drop` type has
                    // its bare wrapper and is left on whatever path it takes
                    // today, byte-for-byte.
                    if !self.drop_rc.user_drop_wrapper_fns.contains_key(type_name)
                        && self.track_user_drop_var_inst(type_name, param_name, slot, inst.as_ref())
                    {
                        return true;
                    }
                    self.track_struct_var_inst(type_name, slot, inst);
                    return true;
                }
                return false;
            }
            // B-2026-07-10-4: rc-inc buried bare-shared during entry-copy so it stays
            // symmetric with the combined drop's per-element rc-dec (a copy-supported
            // struct can carry a shared handle buried in a `Vec[struct]` element /
            // nested struct — `FnDefNode.params[].ty`, `FnDefNode.body`).
            //
            // B-2026-08-29-63 — SKIP the copy entirely when the prepass proved
            // every call site transfers. The rc-inc above is skipped with it,
            // and that stays balanced for the same reason the copy does: the
            // caller retracted its own drop, so its rc-dec is gone and the drop
            // registered below is the handle's only release. This is exactly
            // the copy-unsupported arm's own-by-transfer bargain, applied to a
            // type that merely *could* have been copied.
            if !transfer {
                let saved = self.drop_rc.deep_copy_rc_inc_bare_shared;
                self.drop_rc.deep_copy_rc_inc_bare_shared = true;
                self.deep_copy_struct_heap_fields_in_place(slot, type_name);
                self.drop_rc.deep_copy_rc_inc_bare_shared = saved;
            }
            // B-2026-08-25-14 — register the drop at the param's declared
            // INSTANTIATION, exactly as the copy-unsupported branch above
            // already does. This branch discarded `inst` and keyed the drop by
            // NAME alone, so an owned `self: Heap[T]` got the name-shared
            // `__karac_drop_struct_Heap`, which resolves the `xs: Vec[T]` field
            // from the erased `T`, classifies it as outer-only, and never walks
            // the elements. The entry copy directly above is element-deep inside
            // a monomorph (B-2026-08-25-10), so every element buffer it
            // allocated was left with no owner: a method that takes `self` and
            // transfers nothing out (`fn take(self) -> i64 { self.xs.len() }`)
            // leaked the whole container's elements.
            //
            // `track_struct_var` IS `track_struct_var_inst(.., None)`, and that
            // function's own doc names this failure mode. The sibling shapes hid
            // it: `let mut h = self` hands ownership to `h`, whose drop is
            // selected from the binding's recorded instantiation, and `self`'s
            // drop is then cap/len-zeroed by the move-suppression — so the
            // erased drop was reached only when nothing moved out of `self`.
            // B-2026-08-29-63 — under TRANSFER the callee owns the value
            // OUTRIGHT: body, fields and memory. `track_struct_var_inst`
            // registers the field walk alone (`__karac_drop_struct_<T>`), which
            // is the right half while the CALLER still holds the value's own
            // `karac_drop_<T>` wrapper and runs the body — the split every
            // non-transfer call relies on. Transfer retracts the caller's half,
            // so registering only the field walk here loses the body entirely:
            // measured as `impl Drop for Res` printing nothing at all on a
            // `fn eatf(r: Res) -> i64` that had printed `drop 41` before.
            //
            // Register the wrapper INSTEAD, never as well — it calls
            // `__karac_drop_struct_<T>` internally, so both would double-walk
            // the fields. `track_user_drop_var` no-ops for a type with no
            // validated wrapper, which is exactly the types that want the plain
            // field walk.
            if let Some(pname) = transfer_param {
                if self.drop_rc.user_drop_wrapper_fns.contains_key(type_name) {
                    self.track_user_drop_var(type_name, pname, slot);
                    return true;
                }
            }
            self.track_struct_var_inst(type_name, slot, inst);
            return true;
        }
        // Non-shared user ENUM (NOT the type-erased Option/Result, whose
        // payloads are handled by their own dedicated machinery).
        if let Some(layout) = self.type_decls.enum_layouts.get(type_name).cloned() {
            if layout.is_shared || type_name == "Option" || type_name == "Result" {
                return false;
            }
            // Only meaningful when some variant carries a heap payload —
            // otherwise the drop is a no-op and there's nothing to copy.
            let any_heap = layout
                .field_drop_kinds
                .values()
                .any(|ks| ks.iter().any(|k| k.is_heap_bearing()));
            if !any_heap {
                return false;
            }
            self.deep_copy_enum_heap_payload_in_place(type_name, slot, &layout);
            self.track_enum_var(type_name, slot);
            return true;
        }
        false
    }

    /// #21 — the tuple-param analog of [`Self::make_aggregate_param_callee_owned`].
    /// A bare (non-ref) by-value TUPLE param with an enum / nested-struct heap
    /// leaf (`fn f(p: (Tok, i64))`) is, without this, a shallow copy SHARING the
    /// caller's heap pointer. When the callee consumes a leaf internally
    /// (`match p.0`) while the caller's owning struct drop (`NestedTuple`) also
    /// frees that buffer, both free it → double-free (#21 P5/P6, which cross the
    /// call boundary so no caller-side suppression resolves them). Deep-copy the
    /// tuple's heap leaves at entry (`deep_copy_one_aggregate_field`, which
    /// already recurses through tuple / enum / nested-struct elements) and
    /// register a `TypeExpr`-driven scope-exit drop (`synthesize_tuple_drop_fn_te`)
    /// so the param owns an INDEPENDENT copy — caller and callee free distinct
    /// buffers. Bails (caller-retains status quo) when any leaf is not
    /// copy-supported (`Map` / shared / `Option` / `Result`), matching the
    /// struct-param policy. Returns whether entry-copy engaged.
    pub(super) fn make_tuple_param_callee_owned(
        &mut self,
        elems: &[TypeExpr],
        agg_ty: StructType<'ctx>,
        slot: PointerValue<'ctx>,
    ) -> bool {
        if !elems.iter().any(|e| self.type_expr_has_drop_heap(e)) {
            return false;
        }
        let mut stack = Vec::new();
        if !elems
            .iter()
            .all(|e| self.field_copy_supported(e, &mut stack))
        {
            return false;
        }
        for (j, ete) in elems.iter().enumerate() {
            self.deep_copy_one_aggregate_field(slot, agg_ty, j as u32, ete);
        }
        match self.synthesize_tuple_drop_fn_te(agg_ty, elems) {
            Some(drop_fn) => {
                if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::StructDrop {
                        struct_alloca: slot,
                        drop_fn,
                    });
                }
                true
            }
            None => false,
        }
    }

    /// The fixed-array sibling of [`Self::make_tuple_param_callee_owned`]
    /// (B-2026-08-22-18 follow-up): give an owned by-value `Array[T, N]` param /
    /// `self` whose element `T` owns heap a scope-exit element drop, so its `N`
    /// buffers are freed exactly once.
    ///
    /// Unlike the tuple/struct owned-aggregate paths this does NOT deep-copy at
    /// entry: a fixed array is passed by value transferring ownership (the caller
    /// does not free the value it hands over — a bound-array source's own drop is
    /// suppressed at the call site by [`Self::suppress_array_binding_move_arg`],
    /// and a temporary has no separate owner), so the callee is the sole owner
    /// and a copy would create a second one. The DROP goes through
    /// [`Self::synthesize_array_drop_fn_te`], which GEPs the real `[N x T]` with
    /// the `[i]` stride and is cap-guarded, so a moved-out element cap-zeroed by
    /// [`Self::suppress_array_elem_move_source`] is skipped.
    ///
    /// The slot is recorded in `owned_array_params` so the move-out disarm and
    /// the call-site source suppression know this root carries a drop. Returns
    /// `false` (no drop) when `N == 0` or the element owns no drop-bearing heap.
    pub(super) fn make_array_param_callee_owned(
        &mut self,
        param_name: &str,
        elem_te: &TypeExpr,
        n: u32,
        elem_ty: inkwell::types::BasicTypeEnum<'ctx>,
        slot: PointerValue<'ctx>,
    ) -> bool {
        if n == 0 || !self.type_expr_has_drop_heap(elem_te) {
            return false;
        }
        match self.synthesize_array_drop_fn_te(elem_ty, elem_te, n) {
            Some(drop_fn) => {
                if let Some(frame) = self.drop_rc.scope_cleanup_actions.last_mut() {
                    frame.push(super::state::CleanupAction::StructDrop {
                        struct_alloca: slot,
                        drop_fn,
                    });
                }
                self.borrow_vars
                    .owned_array_params
                    .insert(param_name.to_string(), (elem_te.clone(), n));
                true
            }
            None => false,
        }
    }

    /// Recursively decide whether a struct's heap content can be soundly
    /// outer-buffer-copied to mirror its drop. `stack` guards against
    /// self-referential owned structs (which would recurse forever — bail).
    /// B-2026-06-14-28 — does this struct transitively own a `shared`
    /// (RC-pointer) field? Used to classify a plain struct carried inline as
    /// a shared-enum-variant payload (`Add(BinOp)`, `BinOp { left: Expr,
    /// right: Expr }`) so the enum-box RC drop walker rc-dec's its inline RC
    /// children. Walks direct shared fields, `Option[shared T]` fields, and
    /// recurses through nested non-shared struct / tuple fields; `stack`
    /// guards self-reference. Conservative on collections/enums (they don't
    /// hold a *direct* shared edge this walk needs to dec — their own drop
    /// machinery handles inner shared values).
    pub(super) fn struct_owns_shared_field(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
    ) -> bool {
        self.struct_owns_shared_field_subst(struct_name, stack, None)
    }

    /// [`Self::struct_owns_shared_field`] with the owner's generic subst applied
    /// to each declared field type before the shared test (B-2026-08-06-8).
    ///
    /// This predicate is the GATE that decides whether a struct local gets the
    /// COMBINED drop (value drop + shared-field rc-dec walker) or the value drop
    /// alone. It reads declared field types, so `Box[T] { v: T }` at `T = Node`
    /// answered `false` — the local took the plain value drop, nothing ever
    /// rc-dec'd the box, and it leaked. The concrete `Holder { v: Node }`
    /// answered `true` and was always clean; that asymmetry IS the bug.
    ///
    /// `None` (or an empty subst) is the name-only behavior, byte-for-byte.
    pub(super) fn struct_owns_shared_field_subst(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
        subst: Option<&std::collections::HashMap<String, TypeExpr>>,
    ) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return false;
        };
        stack.push(struct_name.to_string());
        let owns = ftes.iter().any(|fte| match subst {
            Some(s) if !s.is_empty() => {
                let cte = crate::codegen::helpers::subst_type_params_in_type_expr(fte, s);
                self.field_owns_shared(&cte, stack)
            }
            _ => self.field_owns_shared(fte, stack),
        });
        stack.pop();
        owns
    }

    /// Is `struct_name` (transitively) SELF-REFERENTIAL — does walking its
    /// field types reach the struct itself?
    ///
    /// B-2026-08-05-33. `aggregate_param_copy_supported_struct` declines for
    /// two unrelated reasons and only one of them means the callee may ALIAS:
    /// self-reference (`struct N { edges: Vec[N] }`) declines via the `stack`
    /// cycle guard because the unrolled copy has no finite emission, and the
    /// callee can then store the alias into an owning container. Every other
    /// decline — a Map-bearing field, a generic-erased `T`, a direct `shared`
    /// field — just means this routine cannot duplicate the field.
    ///
    /// The walk descends Vec/VecDeque elements because the copy-support walk's
    /// Vec arm does (`deep_copy_vec_aggregate_elements_in_place` inlines a
    /// per-element struct copy), and generic args because a cycle can run
    /// through one. The cycle guard trips iff the struct reaches itself, so
    /// asking that directly is equivalent to "the decline was the
    /// self-referential one" while staying independent of how the
    /// copy-support walk is refactored later.
    pub(super) fn struct_is_self_referential(&self, struct_name: &str) -> bool {
        let mut stack = Vec::new();
        self.struct_reaches(struct_name, struct_name, &mut stack)
    }

    /// B-2026-09-02-5 — arrange for the flag-`false` edge of a param-view
    /// assignment target to still run the MEMORY half of its drop.
    ///
    /// `h2 = h` moves an owned by-value struct param's ENTRY COPY into `h2`.
    /// The copy is the callee's (`make_aggregate_param_callee_owned`), so this
    /// frame owes its buffers a free; only the `Drop` BODY belongs to the
    /// caller, which is what B-2026-08-01-16's retraction — now
    /// B-2026-08-30-53's per-path flag — exists to withhold. For a type with an
    /// `impl Drop` those two halves are ONE registered action (the
    /// `karac_drop_<T>` wrapper runs the body and then the field walk, and is
    /// mutually exclusive with `StructDrop`), so withholding the body withheld
    /// the free with it. Record the field walk to run instead on that edge.
    ///
    /// ADMITTING A SOURCE IS THE WHOLE SAFETY ARGUMENT, and it is INDUCTIVE
    /// rather than a predicate over the type. "Does the callee own this
    /// param's buffers" turned out not to be answerable from the type alone —
    /// measured twice, both times as a double free rather than a leak, which is
    /// the direction that trades a small bug for a bigger one:
    ///
    ///   * an RC-PROMOTED param bypasses
    ///     [`Self::make_aggregate_param_callee_owned_transfer`] entirely, so the
    ///     callee holds a HANDLE onto the caller's box and the caller keeps its
    ///     drop, while every type-level test still says "copy-supported";
    ///   * and a source reached THROUGH another binding (`let a = p; out = a;`)
    ///     inherits whatever `p` was, which the type cannot say either.
    ///
    /// So the base case is a genuine owned by-value PARAM that is not `ref` and
    /// not RC-promoted — the shape the prologue entry-copies — and the step is a
    /// local that THIS function already registered, which by induction carries
    /// callee-owned memory. Anything else declines, and declining is exactly the
    /// pre-fix behaviour (the leak), never a new free.
    ///
    /// The type-level disjunction is still applied on top of that, for the
    /// caller-retains shapes the prologue itself declines: a shared-owning or
    /// self-referential struct gets no entry copy and no callee drop. It also
    /// declines for a type whose field walk is `None` (nothing to free), which
    /// keeps every heap-free `Drop` type emitting exactly what it did.
    pub(super) fn register_param_view_mem_drop(
        &mut self,
        binding_name: &str,
        source_name: &str,
        type_name: &str,
        slot: PointerValue<'ctx>,
    ) {
        if self.type_decls.shared_types.contains_key(type_name)
            || !self.type_decls.struct_types.contains_key(type_name)
        {
            return;
        }
        // THE TARGET's own slot must be a `T` to walk. An RC-promoted binding's
        // alloca is an 8-byte `ptr` handle instead — the confusion
        // B-2026-09-02-20/-21 fixed for the cap-zeroing on this same statement.
        if self
            .drop_rc
            .rc_fallback_heap_types
            .contains_key(binding_name)
        {
            return;
        }
        // THE SOURCE: see [`Self::source_carries_callee_owned_param_memory`] for
        // why this is an induction rather than a question about the type.
        if !self.source_carries_callee_owned_param_memory(source_name) {
            return;
        }
        let callee_owns = self.aggregate_param_copy_supported_struct(type_name, &mut Vec::new())
            || (!self.struct_owns_shared_field(type_name, &mut Vec::new())
                && !self.struct_is_self_referential(type_name));
        if !callee_owns {
            return;
        }
        // Same instantiation threading as the move-suppression on this path
        // (`suppress_source_vec_cleanup_for_arg`): a generic wrapper's field
        // walk must be the per-monomorph one, or it resolves the field from the
        // erased `T` and frees nothing (B-2026-07-15-11).
        let subst = self
            .type_decls
            .enum_inst_var_types
            .get(binding_name)
            .cloned()
            .map(|i| self.generic_struct_subst_from_inst(type_name, &i))
            .unwrap_or_default();
        let Some(mem_fn) = self.emit_struct_drop_synthesis_mono(type_name, &subst) else {
            return;
        };
        self.drop_rc
            .param_view_mem_drops
            .insert((binding_name.to_string(), slot), mem_fn);
        // The induction's step: the target now carries the callee-owned memory,
        // so a further hand-off out of it (`b = a`) is admissible in turn.
        self.drop_rc
            .param_view_callee_owned
            .insert(binding_name.to_string());
    }

    /// B-2026-09-02-5 — does `source_name` hold heap this FRAME owns, as opposed
    /// to a view onto the caller's?
    ///
    /// BASE CASE: an owned by-value param, which
    /// [`Self::make_aggregate_param_callee_owned_transfer`] entry-copies in the
    /// prologue. `ref` params are excluded (nothing was copied) and so are
    /// RC-PROMOTED ones, which never reach that prologue at all — the ownership
    /// pass gives them a heap `{ i64 rc, T }` box and the callee holds a HANDLE
    /// onto the CALLER's, so the caller keeps its drop. Measured: without that
    /// exclusion `test_rc_promoted_param_move_suppression_is_sound_at_o0_and_on_the_jit`
    /// aborts with `free(): double free detected in tcache 2` on the JIT lane and
    /// at `KARAC_OPT_LEVEL=0`, and valgrind names the block as one `main`
    /// allocated and frees again. The shape that causes the promotion is a
    /// conditional param-view assignment to a LOOP-DECLARED local.
    ///
    /// STEP: a local that took a qualifying view and registered its own
    /// memory ownership — through the assignment path (`a = h`) or through the
    /// `let` rebind (`let a = h`, whose site registers the memory-only
    /// `track_struct_var_inst` on the same premise, B-2026-08-01-15).
    ///
    /// NOT `param_view_locals`, which is the near-miss worth naming: that set
    /// records who runs the BODY, and `let a = p; out = a;` over an RC-promoted
    /// `p` puts `a` in it while nothing was ever copied — admitting it
    /// double-freed a program the pre-fix compiler ran cleanly.
    pub(super) fn source_carries_callee_owned_param_memory(&self, source_name: &str) -> bool {
        let is_entry_copied_param = self.fn_ctx.current_fn_param_names.contains(source_name)
            && !self.borrow_vars.ref_params.contains_key(source_name)
            && !self
                .drop_rc
                .rc_fallback_heap_types
                .contains_key(source_name);
        is_entry_copied_param || self.drop_rc.param_view_callee_owned.contains(source_name)
    }

    /// B-2026-08-25-2 — does unrolling the emitter's per-element struct copy
    /// for `Vec[elem]` have a FINITE emission?
    ///
    /// B-2026-07-28-3 asked a narrower question here: "is `elem` already on the
    /// walk stack". That catches a DIRECT self-reference — `struct GraphNode {
    /// edges: Vec[GraphNode] }`, where `elem` IS the struct being walked — but
    /// it answers "terminates" for every cycle of length two or more and then
    /// STOPS THE WALK, because the arm returns a verdict instead of recursing.
    /// `std.cli` is exactly that shape: `Parser.subcommands: Vec[Subcommand]`
    /// and `Subcommand.parser: Parser`. Walking `Parser` sees element
    /// `Subcommand`, which is not on the stack, so the analysis called `Parser`
    /// copy-supported and never looked at `Subcommand`'s fields. The emitter
    /// has no such stopping rule — `deep_copy_one_aggregate_field` descends a
    /// `Vec[struct]`'s elements and then that struct's own fields — so it took
    /// the `Subcommand.parser` edge straight back to `Parser` and recursed
    /// until the compiler's stack was gone (13 000 frames, on `hello world`,
    /// as soon as cli.kara's bodies were registered for compilation).
    ///
    /// Asking about REACHABILITY instead of membership makes the analysis walk
    /// exactly as deep as the emitter, which is what B-2026-07-28-3 intended.
    /// The three ways the unrolled copy fails to terminate:
    ///   - `elem` is an ancestor (the original direct-cycle case),
    ///   - `elem` can REACH an ancestor (the mutual cycle this row is about),
    ///   - `elem` reaches itself (a cycle wholly below the current walk).
    ///
    /// Deliberately narrow: an ACYCLIC element type still answers "terminates"
    /// and keeps the arm's unconditional `true`, so programs that compile today
    /// emit byte-identical IR. Only a genuine cycle newly declines, and
    /// declining just means the param falls back to caller-retains — the same
    /// conservative treatment every other non-copyable field shape gets.
    fn vec_elem_copy_emission_terminates(&self, ehead: &str, stack: &[String]) -> bool {
        if stack.iter().any(|s| s == ehead) {
            return false;
        }
        if self.struct_is_self_referential(ehead) {
            return false;
        }
        !stack
            .iter()
            .any(|a| self.struct_reaches(a, ehead, &mut Vec::new()))
    }

    fn struct_reaches(&self, root: &str, cur: &str, stack: &mut Vec<String>) -> bool {
        if stack.iter().any(|s| s == cur) {
            return false;
        }
        let Some(ftes) = self.type_decls.struct_field_type_exprs.get(cur).cloned() else {
            return false;
        };
        stack.push(cur.to_string());
        let found = ftes
            .iter()
            .any(|fte| self.type_expr_reaches(root, fte, stack));
        stack.pop();
        found
    }

    fn type_expr_reaches(&self, root: &str, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.type_expr_reaches(root, e, stack)),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                if head == root {
                    return true;
                }
                if matches!(head, "Vec" | "VecDeque") {
                    if let Some(elem) = crate::codegen::helpers::vec_inner_type_expr(fte) {
                        if self.type_expr_reaches(root, &elem, stack) {
                            return true;
                        }
                    }
                }
                if let Some(args) = p.generic_args.as_ref() {
                    for a in args {
                        if let crate::ast::GenericArg::Type(t) = a {
                            if self.type_expr_reaches(root, t, stack) {
                                return true;
                            }
                        }
                    }
                }
                if self.type_decls.struct_types.contains_key(head)
                    && !self.type_decls.shared_types.contains_key(head)
                {
                    return self.struct_reaches(root, head, stack);
                }
                false
            }
            _ => false,
        }
    }

    /// Name-set companion to `option_inner_shared_type_for_type_expr`: does
    /// `Option[T]` / `Result[T, _]` have a shared `T`, judged by the early
    /// `shared_type_decl_names` set (before `shared_types` layouts exist)?
    fn option_inner_decl_shared(&self, fte: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &fte.kind else {
            return false;
        };
        let Some(args) = p.generic_args.as_ref() else {
            return false;
        };
        args.iter().any(|a| {
            if let crate::ast::GenericArg::Type(t) = a {
                if let TypeKind::Path(ip) = &t.kind {
                    if let Some(name) = ip.segments.last() {
                        return self
                            .type_decls
                            .shared_type_decl_names
                            .contains(name.as_str());
                    }
                }
            }
            false
        })
    }

    fn field_owns_shared(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.field_owns_shared(e, stack)),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                // A direct shared field (the `Expr` edge) — the one we dec.
                // Use the NAME set (`shared_type_decl_names`), not
                // `shared_types`: this classifier runs inside `declare_enums`,
                // before `shared_types` is populated for `Expr` (B-2026-06-14-28).
                if self.type_decls.shared_type_decl_names.contains(head) {
                    return true;
                }
                // `Option[shared T]` — the inner shared edge is reachable.
                if (head == "Option" || head == "Result") && self.option_inner_decl_shared(fte) {
                    return true;
                }
                // B-2026-06-14-31 — a `Vec[shared T]` field also owns a shared
                // edge: its element boxes are RC pointers that must be dec'd
                // when the owning struct drops (the `Call(CallExpr { args:
                // Vec[Expr] })` shape). Without this, a struct whose ONLY
                // shared content is a `Vec[shared]` would be classified as
                // non-walkable and the shared-enum box drop would skip its
                // payload entirely, leaking the buffer + every element box.
                // Judged by the NAME set (same reason as the direct-field
                // case): this runs before `shared_types` is populated.
                if (head == "Vec" || head == "VecDeque") && self.option_inner_decl_shared(fte) {
                    return true;
                }
                // Recurse through a nested non-shared user struct.
                if self.type_decls.struct_field_type_exprs.contains_key(head)
                    && !self.type_decls.shared_type_decl_names.contains(head)
                {
                    return self.struct_owns_shared_field(head, stack);
                }
                false
            }
            _ => false,
        }
    }

    /// B-2026-08-29-63 — the single eligibility predicate for owning a by-value
    /// struct param by TRANSFER instead of by ENTRY COPY.
    ///
    /// Consulted by all three sites that must agree — the callee prologue
    /// ([`Self::make_aggregate_param_callee_owned_transfer`]), the caller's drop
    /// retraction (`move_transferred_struct_arg`) and the caller's fresh-temp
    /// registrar gate — so that "who owns this buffer" cannot be answered two
    /// ways for one call. A whole-program permission from
    /// [`super::param_transfer`] is necessary but NOT sufficient: it answers a
    /// question about call-site SHAPES, and this answers the one about the TYPE.
    ///
    /// A USER `Drop` ANYWHERE IN THE TYPE DECLINES, and that exclusion is about
    /// observable ORDER, not about memory. Transfer moves the value's death from
    /// the caller's frame into the callee's, so a `Drop` body that used to run
    /// after the call returns now runs before it. That is invisible for a type
    /// whose drop only frees, and visible for one whose drop PRINTS:
    /// `println(f"eat={eat(a)}")` over a `Res` with an `impl Drop` printed
    /// `eat=11` then `drop 10` under the entry copy — and the interpreter agrees
    /// with the entry copy — while transfer printed them the other way round.
    /// Both orderings are defensible; only one of them is what `karac run`
    /// does, and a compiled backend that reorders a user-visible side effect
    /// away from the interpreter is a run-vs-build divergence, which this repo
    /// treats as a compiler bug rather than a trade.
    ///
    /// So the win is taken only where it is unobservable. That costs nothing
    /// measured: the cost this row is about is the field-buffer memcpy, and a
    /// `Drop`-free struct pays it identically (the row measured the same
    /// `N * 8` delta "with or without a `Drop` impl"). Making the ordering itself
    /// safe to move is a separate question about where a moved-from value's body
    /// belongs, and it needs the interpreter to move with it.
    ///
    /// The `Drop` test is TRANSITIVE and conservative in the declining
    /// direction: an unresolvable field type answers "has a drop", because the
    /// cost of a wrong `false` is a reordered side effect and the cost of a
    /// wrong `true` is only a missed optimisation.
    pub(super) fn struct_param_transfer_eligible(&self, struct_name: &str) -> bool {
        if !self.type_decls.struct_types.contains_key(struct_name)
            || self.type_decls.shared_types.contains_key(struct_name)
        {
            return false;
        }
        if !self.aggregate_param_copy_supported_struct(struct_name, &mut Vec::new()) {
            return false;
        }
        if self.struct_owns_shared_field(struct_name, &mut Vec::new())
            || self.struct_is_self_referential(struct_name)
        {
            return false;
        }
        !self.struct_reaches_user_drop(struct_name, &mut Vec::new())
    }

    /// Does `struct_name`, or any type reachable through its declared fields,
    /// carry a user `impl Drop`? See
    /// [`Self::struct_param_transfer_eligible`] for why this declines rather
    /// than approximates.
    fn struct_reaches_user_drop(&self, struct_name: &str, stack: &mut Vec<String>) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        let has_own = self
            .program_snapshot
            .as_deref()
            .map(|p| p.drop_method_keys.contains_key(struct_name))
            // No snapshot means no way to tell — decline.
            .unwrap_or(true);
        if has_own {
            return true;
        }
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return false;
        };
        stack.push(struct_name.to_string());
        let reaches = ftes
            .iter()
            .any(|fte| self.field_reaches_user_drop(fte, stack));
        stack.pop();
        reaches
    }

    /// Field-level half of [`Self::struct_reaches_user_drop`]. Walks tuples and
    /// every generic argument, so a `Vec[Guard]` / `Option[Guard]` /
    /// `Map[K, Guard]` field is caught as readily as a bare one.
    fn field_reaches_user_drop(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.field_reaches_user_drop(e, stack)),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                let head_drops = self
                    .program_snapshot
                    .as_deref()
                    .map(|pr| pr.drop_method_keys.contains_key(head))
                    .unwrap_or(true);
                if head_drops {
                    return true;
                }
                if self.type_decls.struct_field_type_exprs.contains_key(head)
                    && self.struct_reaches_user_drop(head, stack)
                {
                    return true;
                }
                p.generic_args.iter().flatten().any(|ga| match ga {
                    crate::ast::GenericArg::Type(t) => self.field_reaches_user_drop(t, stack),
                    _ => false,
                })
            }
            _ => false,
        }
    }

    pub(super) fn aggregate_param_copy_supported_struct(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
    ) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        if self.type_decls.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return false;
        };
        stack.push(struct_name.to_string());
        let ok = ftes.iter().all(|fte| self.field_copy_supported(fte, stack));
        stack.pop();
        ok
    }

    /// B-2026-08-12-2 — is every field of `struct_name` either outer-buffer
    /// COPY-supported or a direct `Map`/`Set` HANDLE?
    ///
    /// The two properties come apart, and the gap is the bug. A `Map`/`Set`
    /// field is not copy-supported — `field_copy_supported` bails on it because
    /// the entry-copy cannot duplicate a side-table handle — but the synthesized
    /// struct drop DOES free it, which is why a plain `let s = mk();` over a
    /// `Map`-bearing struct is already leak-clean. So a payload bound out of a
    /// `match` arm can be given its `track_struct_var` even though it could
    /// never have been entry-copied: the arm needs the FREE, not the copy.
    ///
    /// Deliberately one level deep on the handle arm. A nested struct field
    /// still has to be fully copy-supported, so this widens the admitted set by
    /// exactly the direct-handle case that was measured, and a `Vec[Map[..]]` or
    /// a `Map`-bearing nested struct keeps today's behaviour rather than riding
    /// in on an unmeasured generalisation.
    pub(super) fn struct_heap_copyable_or_handle(&self, struct_name: &str) -> bool {
        if self.type_decls.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return false;
        };
        ftes.iter().all(|fte| {
            if self.field_copy_supported(fte, &mut Vec::new()) {
                return true;
            }
            let TypeKind::Path(p) = &fte.kind else {
                return false;
            };
            matches!(
                p.segments.first().map(String::as_str),
                Some("Map")
                    | Some("HashMap")
                    | Some("SortedMap")
                    | Some("BTreeMap")
                    | Some("Set")
                    | Some("HashSet")
                    | Some("SortedSet")
                    | Some("BTreeSet")
            )
        })
    }

    /// B-2026-08-12-1 — is a by-value `Option`/`Result` PARAM of this declared
    /// type entry-COPIED by the callee, rather than owned by transfer? Both
    /// frames consult this ONE predicate (`callee_optres_param_entry_copied`,
    /// call_dispatch.rs, suppresses the arg-site whole-slot zero on the same
    /// answer), so they cannot drift into a double free.
    ///
    /// Delegates to [`Self::field_copy_supported`]'s `Option`/`Result` arms:
    /// that predicate already decides copy == drop for these two payload
    /// families as struct FIELDS, and it vets BOTH halves of a `Result` — which
    /// matters here because the registered payload drop frees whichever half is
    /// live, so admitting a copy that skipped the `Err` half would double-free
    /// every program that takes an error path while every `Ok`-only test passed.
    pub(super) fn optres_param_entry_copied_te(&self, te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &te.kind else {
            return false;
        };
        let head = p.segments.first().map(String::as_str);
        if !matches!(head, Some("Option") | Some("Result")) {
            return false;
        }
        // SHARED and BOXED payloads are excluded, even though
        // `field_copy_supported` admits both. As a struct FIELD their "copy" is
        // an rc-INC of the box / a fresh envelope, and the enclosing struct's
        // drop is the matching release — copy == drop, one owner. At a PARAM
        // boundary the same two shapes already have owners of their own: the
        // rc machinery for a shared handle, and `boxed_enum_payload_vars` /
        // `boxed_struct_payload_vars` for a box, each with its own caller-side
        // retraction rules that this entry copy would be a second, unsynchronised
        // answer to. Measured: admitting them fails 20 memory_sanitizer fixtures
        // across the shared-Option, boxed-Option and boxed-enum-chain families
        // — the copy is not what those shapes are missing.
        let boxing_words = if head == Some("Result") { 5 } else { 3 };
        let halves = p.generic_args.as_deref().unwrap_or(&[]);
        for a in halves.iter().take(2) {
            let GenericArg::Type(half) = a else {
                return false;
            };
            let hhead = match &half.kind {
                TypeKind::Path(hp) => hp.segments.first().map(String::as_str).unwrap_or(""),
                _ => "",
            };
            if self.type_decls.shared_types.contains_key(hhead)
                || self.option_inner_shared_type_for_type_expr(te).is_some()
            {
                return false;
            }
            if Self::llvm_type_word_count(self.llvm_type_for_type_expr(half)) > boxing_words {
                return false;
            }
        }
        self.field_copy_supported(te, &mut Vec::new())
    }

    /// B-2026-08-12-1 — emit the entry copy for a by-value `Option`/`Result`
    /// param slot admitted by [`Self::optres_param_entry_copied_te`]. Dispatch
    /// mirrors `deep_copy_one_aggregate_field`'s arms exactly (the two admitted
    /// `Result` classes are structurally disjoint, so precisely one applies),
    /// which is what keeps a param's copy depth equal to the same type's field
    /// copy depth, and therefore equal to the drop that frees it.
    pub(super) fn deep_copy_optres_param_in_place(
        &mut self,
        slot: PointerValue<'ctx>,
        te: &TypeExpr,
    ) {
        let TypeKind::Path(p) = &te.kind else {
            return;
        };
        match p.segments.first().map(String::as_str) {
            Some("Option") => self.deep_copy_option_inline_payload_in_place(slot, te),
            Some("Result") => {
                if self.result_field_struct_enum_payload_ok(te) {
                    self.deep_copy_result_struct_enum_payload_in_place(slot, te);
                } else {
                    self.deep_copy_result_inline_heap_halves_in_place(slot, te);
                }
            }
            _ => {}
        }
    }

    pub(super) fn field_copy_supported(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems.iter().all(|e| self.field_copy_supported(e, stack)),
            // Borrows carry no owned heap — the struct drop never frees them.
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_) => true,
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                match head {
                    // "str" is the typechecker-internal spelling of `String` (see
                    // `is_string_type_expr` / `type_expr_has_drop_heap`, which
                    // already treat the two as synonyms). It appears here when a
                    // generic field's bare `T` is resolved through a MONOMORPH
                    // subst whose value is `str` — e.g. a METHOD monomorph
                    // (`impl[T] Box[T] { fn get(self) -> T { self.v } }` at
                    // T=String records `type_subst_names["T"] = "str"`, whereas the
                    // free-fn twin records `String`). Omitting it made
                    // `aggregate_param_copy_supported_struct_mono` bail, so the
                    // generic method's owned `self` was NOT entry-copied (stayed a
                    // caller-retains alias) and returning `self.v` double-freed the
                    // aliased buffer (B-2026-07-18-44; the free-fn twin already
                    // worked via the `String` spelling). `deep_copy_one_aggregate_
                    // field` copies it correctly (it keys on `is_string_type_expr`,
                    // which matches both spellings).
                    "String" | "str" => true,
                    // B-2026-07-28-3 — a `Vec`/`VecDeque` field is copyable, but
                    // only after the SELF-REFERENCE check below. This arm used to
                    // return `true` unconditionally, which stopped the walk at the
                    // `Vec` and so never re-tested the element type against
                    // `stack`. The emitter does NOT stop there: since
                    // B-2026-07-04-9(a), `deep_copy_one_aggregate_field` descends
                    // into a `Vec[struct]`'s elements via
                    // `deep_copy_vec_aggregate_elements_in_place`, which inlines a
                    // per-element `deep_copy_struct_heap_fields_in_place`. For a
                    // directly or mutually self-referential type — `struct
                    // GraphNode { mut edges: Vec[GraphNode] }`, i.e. every
                    // adjacency-list graph — analysis said "copyable", the param
                    // became callee-owned, and the emitter then recursed
                    // struct → Vec-element → struct forever, overflowing the
                    // compiler's stack. The element copy is UNROLLED at emission,
                    // so its depth would have to equal the runtime data depth;
                    // no finite emission exists. Consulting `stack` here makes the
                    // analysis walk exactly as deep as the emitter, so the
                    // existing cycle guard in
                    // `aggregate_param_copy_supported_struct` trips and the param
                    // falls back to caller-retains — the same conservative
                    // treatment every other non-copyable field shape already gets.
                    "Vec" | "VecDeque" => {
                        let Some(elem) = crate::codegen::helpers::vec_inner_type_expr(fte) else {
                            return true;
                        };
                        let TypeKind::Path(ep) = &elem.kind else {
                            return true;
                        };
                        let ehead = ep.segments.first().map(String::as_str).unwrap_or("");
                        // Only a user struct is descended into by the emitter's
                        // `ElemCopy::Struct` plan; anything else keeps the old
                        // unconditional `true` so currently-working programs emit
                        // byte-identical IR.
                        !(self.type_decls.struct_types.contains_key(ehead)
                            && !self.type_decls.shared_types.contains_key(ehead)
                            && !self.vec_elem_copy_emission_terminates(ehead, stack))
                    }
                    "Slice" => true,
                    // Heap the outer-buffer copy can't duplicate → bail.
                    "Map" | "HashMap" | "Set" | "HashSet" | "SortedSet" | "SortedMap"
                    | "BTreeMap" | "BTreeSet" => false,
                    // HTTP side-table handle structs (see emit_struct_drop_synthesis).
                    "Response" | "RequestBuilder" => false,
                    // B-2026-07-03-28 Facet A — an `Option[String]`/`Option[Vec[..]]`
                    // field with an inline `{ptr,len,cap}` payload IS copyable:
                    // `deep_copy_option_inline_payload_in_place` duplicates the
                    // `Some` buffer type-aware off the field TypeExpr, and the
                    // struct drop's `OptionInline` arm (gated on this same
                    // copy-supported predicate) frees it — copy == drop, so a
                    // callee-owned copy and the caller's retained original own
                    // independent buffers. An `Option[shared]` field is ALSO
                    // copyable (B-2026-07-03-28 shared leg): its inline payload is
                    // a single RC box pointer (word 1, ptrtoint), so the "copy" is
                    // an rc-INC of the box when Some
                    // (`deep_copy_option_inline_payload_in_place`'s shared branch),
                    // symmetric with the Vec-element / destructure-leaf drop's
                    // `Option[shared]` rc-DEC (`emit_nested_struct_shared_rc_decs_ex`
                    // / `RcDecOption`). Other `Option` payloads (boxed-wide,
                    // struct/enum-inline, plain-enum = B-27) stay caller-retains
                    // (this routine can't duplicate them, and the drop
                    // correspondingly leaves them excluded). `Result` fields in
                    // the DIRECT String/Vec-halves class are copyable since
                    // B-2026-07-21-15 (arm below); every other Result shape
                    // stays caller-retains the same way.
                    // B-2026-07-18-2: under for-loop strict-shared mode an
                    // `Option` field is UNSUPPORTED — a shared-bearing struct's
                    // drain (synthesized as non-copy-supported) skips Option
                    // fields, so a registered element's aliased Option leaf
                    // would lose its leaf-cleanup and leak.
                    // B-2026-08-07-20 — the ban above is lifted for exactly the
                    // structs whose drain DOES free the field. Its premise ("a
                    // shared-bearing struct's drain skips Option fields") is what
                    // `shared_owning_struct_sole_field_owner` changes, so the two
                    // move in lockstep off ONE predicate: admit only when the
                    // enclosing struct passes it AND the payload is in the class
                    // `emit_struct_drop_synthesis_impl` promotes. Admitting
                    // without the drain (or draining without admitting) is the
                    // leak / double-free pair this row measured.
                    "Option"
                        if self.copy_support_for_loop_shared_mode
                            && stack.last().is_some_and(|s| {
                                self.shared_owning_struct_sole_field_owner_core(s)
                            }) =>
                    {
                        Self::option_payload_te(fte)
                            .map(|pt| {
                                self.is_string_type_expr(&pt)
                                    || self.extract_vec_elem_type(&pt).is_some()
                            })
                            .unwrap_or(false)
                            || self.option_inner_shared_type_for_type_expr(fte).is_some()
                    }
                    "Option" if self.copy_support_for_loop_shared_mode => false,
                    "Option" => {
                        Self::option_payload_te(fte)
                            .map(|pt| {
                                self.is_string_type_expr(&pt)
                                    || self.extract_vec_elem_type(&pt).is_some()
                            })
                            .unwrap_or(false)
                            || self.option_inner_shared_type_for_type_expr(fte).is_some()
                            // B-2026-07-04-7 — an `Option[<non-shared struct/enum>]`
                            // field is ALSO copyable: its `Some` payload is either
                            // BOXED (wider than the 3-word inline area) or inline in
                            // words 1..3, and `deep_copy_option_struct_enum_payload_in_place`
                            // duplicates it (allocating a fresh box, deep-copying the
                            // payload's heap) — the copy peer of `emit_option_drop_fn`'s
                            // boxed/inline free (`option_payload_struct_or_enum_drop_ok`).
                            // Symmetric copy == drop, so a callee-owned copy and the
                            // caller's retained original own independent heap.
                            || Self::option_payload_te(fte)
                                .map(|pt| self.option_payload_struct_or_enum_copyable(&pt, stack))
                                .unwrap_or(false)
                            // B-2026-08-07-2 shape 3 — the same admission for a
                            // BOXED payload that owns no heap of its own. The
                            // disjunct above routes through
                            // `option_payload_struct_or_enum_drop_ok`, which
                            // requires the payload to be droppable, so
                            // `Option[Option[i64]]` fails it and the whole struct
                            // reads as caller-retains — which in turn switches off
                            // `emit_struct_drop_synthesis`'s entire `OptionInline`
                            // pass (gated on this predicate) and orphans the
                            // envelope, 320 B / 10 at -O0 for a bare `let w: W`
                            // whose field is never read.
                            //
                            // Copy == drop still holds, which is the only reason
                            // this is admissible: the entry copy allocates a fresh
                            // box and duplicates the payload value into it, and the
                            // drop side frees exactly that box (nothing inside it to
                            // free). Both sides consult the SAME predicate so they
                            // cannot drift apart.
                            || Self::option_payload_te(fte)
                                .map(|pt| self.option_payload_boxed_envelope_only(&pt))
                                .unwrap_or(false)
                    }
                    // B-2026-07-21-15 — a `Result` field in the DIRECT
                    // String/Vec-halves class IS copyable: the entry copy
                    // duplicates the live half's `{ptr,len,cap}` overlay
                    // (`deep_copy_result_inline_heap_halves_in_place`, built
                    // for the -14 clone leg) and the struct drop's Result
                    // overlay free (the OptionInline classifier's Result
                    // extension) frees it — copy == drop. Under for-loop
                    // strict-shared mode it stays unsupported, matching the
                    // Option arm's rationale. Every other Result shape
                    // (shared / wrapper / nested halves) stays caller-retains.
                    "Result" if self.copy_support_for_loop_shared_mode => false,
                    // B-2026-08-03-3 leg B — the disjoint struct/enum-payload
                    // class is copyable too, via
                    // `deep_copy_result_struct_enum_payload_in_place` (the
                    // `Result` twin of the Option struct/enum copy, boxing at
                    // the 5-word Result payload area rather than Option's 3).
                    "Result" => {
                        self.result_field_direct_vecstr_halves_ok(fte)
                            || self.result_payload_struct_enum_copyable(fte, stack)
                    }
                    _ if is_primitive_type_name(head) => true,
                    // B-2026-07-18-2: a DIRECT `shared` handle field is copyable
                    // in for-loop strict-shared mode — the "copy" is an rc-INC of
                    // the box (`deep_copy_rc_inc_bare_shared` arm), symmetric with
                    // the drop's rc-DEC. Hard bail outside that mode (entry-copy
                    // / clone / drop-synthesis gates keep their meaning).
                    _ if self.type_decls.shared_types.contains_key(head) => {
                        self.copy_support_for_loop_shared_mode
                    }
                    _ if self.type_decls.struct_types.contains_key(head) => {
                        self.aggregate_param_copy_supported_struct(head, stack)
                    }
                    // User enum field (#19 FIXED 2026-06-12). Without entry-copy,
                    // a by-value transfer of an enum-field struct (`let b =
                    // wrap(a)`, `wrap(s: Span) -> Span { s }`) leaves `b` shallow-
                    // aliasing the source's enum buffer; post-#15 BOTH struct drops
                    // free it → double-free (#19). `EnumDropKind` only ever frees a
                    // `VecOrString` payload — exactly what
                    // `deep_copy_enum_heap_payload_in_place` duplicates (wired into
                    // `deep_copy_one_aggregate_field`) — so entry-copy is symmetric
                    // with the struct drop's enum-field free: whatever the drop
                    // frees, the copy copies; carved-out nested-aggregate payloads
                    // are `EnumDropKind::None`, freed by neither. Shared enums bail
                    // at the `shared_types` arm above; Option/Result bail above too,
                    // so any enum reaching here is a non-shared user enum.
                    _ if self.type_decls.enum_layouts.contains_key(head) => {
                        !self.type_decls.enum_layouts[head].is_shared
                    }
                    // Generic type param / unknown → conservative bail.
                    _ => false,
                }
            }
            // Array[T, N] of heap, fn-ptr types, etc. → conservative bail.
            _ => false,
        }
    }

    /// Does `emit_struct_clone_fn` produce a fully INDEPENDENT deep copy of this
    /// struct — every heap field duplicated into its own allocation with no
    /// aliasing back to the source? This is STRICTLY narrower than
    /// `aggregate_param_copy_supported_struct`, which describes what the
    /// deep-copy-ON-ENTRY path (`deep_copy_*_in_place`) can duplicate. That path
    /// handles `Vec[shared]` / `Option[shared]` (rc-inc'ing the shared elements);
    /// the CLONE path (`emit_clone_fn_for_type_expr`) does NOT — it shallow-copies
    /// a shared handle with no refcount bump, so cloning a struct that transitively
    /// owns a shared element aliases it and later double-frees / SEGVs. This
    /// predicate therefore admits ONLY String, primitive, `Vec`/`VecDeque` of a
    /// clone-duplicable element, and nested clone-duplicable structs; it bails on
    /// any shared type, `Option`/`Result`/enum, `Map`/`Set`, or `Slice` field. It
    /// gates the B-2026-07-09-12 deep-clone-on-bind so only the shapes the clone
    /// infra reproduces exactly are upgraded from view to owned copy.
    pub(super) fn struct_clone_fully_duplicates(
        &self,
        struct_name: &str,
        stack: &mut Vec<String>,
    ) -> bool {
        if stack.iter().any(|s| s == struct_name) {
            return false;
        }
        if self.type_decls.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return false;
        };
        stack.push(struct_name.to_string());
        let ok = ftes
            .iter()
            .all(|fte| self.clone_field_fully_duplicates(fte, stack));
        stack.pop();
        ok
    }

    fn clone_field_fully_duplicates(&self, fte: &TypeExpr, stack: &mut Vec<String>) -> bool {
        match &fte.kind {
            TypeKind::Tuple(elems) => elems
                .iter()
                .all(|e| self.clone_field_fully_duplicates(e, stack)),
            // Borrows carry no owned heap — the clone leaves them as shared views
            // and the struct drop never frees them.
            TypeKind::Ref(_) | TypeKind::MutRef(_) => true,
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                match head {
                    "String" => true,
                    // Vec/VecDeque clone deep-copies the buffer AND clones each
                    // element — sound only when the element itself fully
                    // duplicates (so a `Vec[shared]` bails here: its element clone
                    // would shallow-copy the handle).
                    "Vec" | "VecDeque" => match p.generic_args.as_ref().and_then(|a| a.first()) {
                        Some(crate::ast::GenericArg::Type(elem_te)) => {
                            self.clone_field_fully_duplicates(elem_te, stack)
                        }
                        _ => false,
                    },
                    _ if is_primitive_type_name(head) => true,
                    // B-2026-07-18-2: a DIRECT `shared` handle field is copyable
                    // in for-loop strict-shared mode — the "copy" is an rc-INC of
                    // the box (`deep_copy_rc_inc_bare_shared` arm), symmetric with
                    // the drop's rc-DEC. Hard bail outside that mode (entry-copy
                    // / clone / drop-synthesis gates keep their meaning).
                    _ if self.type_decls.shared_types.contains_key(head) => {
                        self.copy_support_for_loop_shared_mode
                    }
                    _ if self.type_decls.struct_types.contains_key(head) => {
                        self.struct_clone_fully_duplicates(head, stack)
                    }
                    // Option / Result / user enum (the clone infra's niche +
                    // shared-payload gaps), Map / Set, Slice, and unknowns → bail.
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// B-2026-07-04-7 — is an `Option[P]` payload `P` (a non-shared user
    /// struct/enum) deep-COPYABLE, so `field_copy_supported`'s `Option` arm can
    /// admit it (making the owning struct callee-owned and its `OptionInline`
    /// drop safe)? The drop side (`emit_option_drop_fn`, gated on
    /// `option_payload_struct_or_enum_drop_ok`) already frees such a payload; the
    /// copy peer is `deep_copy_option_struct_enum_payload_in_place`, which for a
    /// STRUCT recurses via `deep_copy_struct_heap_fields_in_place` (so require the
    /// struct be recursively copy-supported — copy-depth == drop-depth) and for a
    /// non-shared ENUM via `deep_copy_enum_heap_payload_in_place` (the SAME
    /// machinery a DIRECT non-shared enum field already trusts in
    /// `field_copy_supported`'s enum arm, so admit any non-shared enum here too).
    fn option_payload_struct_or_enum_copyable(
        &self,
        payload_te: &TypeExpr,
        stack: &mut Vec<String>,
    ) -> bool {
        if !self.option_payload_struct_or_enum_drop_ok(payload_te) {
            return false;
        }
        let TypeKind::Path(p) = &payload_te.kind else {
            return false;
        };
        let head = p.segments.first().map(String::as_str).unwrap_or("");
        if self.type_decls.shared_types.contains_key(head) {
            return false;
        }
        if self.type_decls.struct_types.contains_key(head) {
            return self.aggregate_param_copy_supported_struct(head, stack);
        }
        self.type_decls
            .enum_layouts
            .get(head)
            .map(|l| !l.is_shared)
            .unwrap_or(false)
    }

    /// B-2026-08-03-3 leg B — the `Result` twin of
    /// [`Self::option_payload_struct_or_enum_copyable`]: is every heap-owning
    /// half of a `result_field_struct_enum_payload_ok` field deep-COPYABLE, so
    /// `field_copy_supported`'s `Result` arm can admit it (making the owning
    /// struct callee-owned and its `OptionInline` drop safe)? The shape gate
    /// already restricted each heap half to a non-shared struct/enum or (since
    /// B-2026-08-03-11) a direct String/Vec; this adds the copy-depth ==
    /// drop-depth requirement — a struct half must be recursively
    /// copy-supported (it recurses via `deep_copy_struct_heap_fields_in_place`),
    /// an enum half rides the same `deep_copy_enum_heap_payload_in_place`
    /// machinery a DIRECT non-shared enum field already trusts, and a direct
    /// String/Vec half is copied by the same overlay dance the all-direct class
    /// uses, whose own gate already vetted it.
    fn result_payload_struct_enum_copyable(
        &self,
        field_te: &TypeExpr,
        stack: &mut Vec<String>,
    ) -> bool {
        if !self.result_field_struct_enum_payload_ok(field_te) {
            return false;
        }
        let TypeKind::Path(p) = &field_te.kind else {
            return false;
        };
        let Some(args) = p.generic_args.as_ref() else {
            return false;
        };
        for a in args.iter().take(2) {
            let GenericArg::Type(half) = a else {
                return false;
            };
            if !self.te_owns_heap_below_buffer(half) {
                continue;
            }
            let TypeKind::Path(hp) = &half.kind else {
                return false;
            };
            let head = hp.segments.first().map(String::as_str).unwrap_or("");
            if self.type_decls.shared_types.contains_key(head) {
                return false;
            }
            let ok = if self.type_decls.struct_types.contains_key(head) {
                self.aggregate_param_copy_supported_struct(head, stack)
            } else if self.type_decls.enum_layouts.contains_key(head) {
                !self.type_decls.enum_layouts[head].is_shared
            } else {
                // The direct String/Vec half the shape gate admits alongside a
                // struct/enum one — the overlay copy handles it (B-2026-08-03-11).
                true
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Deep-copy every Vec/String heap field of the struct value at `base_ptr`,
    /// recursing into nested structs/tuples. Mirrors
    /// `emit_struct_drop_synthesis`'s field walk.
    pub(super) fn deep_copy_struct_heap_fields_in_place(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
    ) {
        let Some(&st) = self.type_decls.struct_types.get(struct_name) else {
            return;
        };
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return;
        };
        for (i, fte) in ftes.iter().enumerate() {
            self.deep_copy_one_aggregate_field(base_ptr, st, i as u32, fte);
        }
    }

    /// B-2026-07-18-31/-32 — make a GENERIC by-value struct param callee-owned
    /// when its fields are bare type params erased by the base layout but bound
    /// to copy-supported heap types by the active monomorph subst. Returns
    /// `true` (ownership taken: mono entry-copy + mono drop registered) or
    /// `false` (left caller-retains, unchanged). Only engages when (a) an active
    /// subst exists, (b) the concrete mono struct layout is buildable, and (c)
    /// every field, resolved through the subst, is copy-supported. Outside a
    /// monomorph — or for a struct whose fields don't all resolve to
    /// copy-supported types — it is a no-op, so non-generic params and the
    /// existing base path are untouched.
    fn try_make_generic_struct_param_callee_owned(
        &mut self,
        type_name: &str,
        slot: PointerValue<'ctx>,
    ) -> bool {
        // The mono layout drives the field GEPs; absent it (no active subst /
        // non-generic struct) there is nothing to widen, so bail to
        // caller-retains exactly as before.
        let Some(mono_st) = self.mono_struct_type_from_active_subst(type_name) else {
            return false;
        };
        if !self.aggregate_param_copy_supported_struct_mono(type_name) {
            return false;
        }
        let saved = self.drop_rc.deep_copy_rc_inc_bare_shared;
        self.drop_rc.deep_copy_rc_inc_bare_shared = true;
        self.deep_copy_struct_heap_fields_in_place_mono(slot, type_name, mono_st);
        self.drop_rc.deep_copy_rc_inc_bare_shared = saved;
        // Register the PER-MONOMORPH drop (`__karac_drop_struct_Pair$str`) so a
        // field NOT moved out (e.g. `fn peek[T](p: Pair[T]) -> T { p.a.clone()
        // }`) still frees its entry-copied buffer; the base drop skips the
        // erased bare-`T` fields and would leak it. Fields that ARE moved out
        // have their caps zeroed by `suppress_struct_field_move_into_literal`,
        // so the mono drop no-ops on them.
        match self.active_subst_struct_inst(type_name) {
            Some(inst) => self.track_struct_var_inst(type_name, slot, Some(inst)),
            None => self.track_struct_var(type_name, slot),
        }
        true
    }

    /// Mono twin of [`aggregate_param_copy_supported_struct`]: resolve each
    /// declared field `TypeExpr` through the ACTIVE monomorph subst before
    /// classifying, so a generic struct whose fields are bare type params
    /// (`Pair[T] { a: T, b: T }`) is judged on its CONCRETE instantiation
    /// (`T = String` → `String` is copyable) instead of bailing at
    /// `field_copy_supported`'s bare-`T` `_ => false` arm. A field that resolves
    /// to a NESTED generic struct (`Inner[T]` → `Inner[String]`) still recurses
    /// through the base `aggregate_param_copy_supported_struct`, which reads
    /// `Inner`'s own erased bare-`T` fields and bails — so a nested-generic
    /// field keeps the caller-retains behavior (a documented residual, same
    /// class as B-2026-07-15-11's single-field gate).
    /// Will a by-value aggregate param of `struct_name` be ENTRY-COPIED by the
    /// monomorph — i.e. take [`Self::try_make_generic_struct_param_callee_owned`]'s
    /// arm rather than the own-by-transfer one below it? Asked by the CALLER
    /// (`compile_generic_call`) with the callee's substitution installed, so the
    /// two cannot disagree: an entry-copied param leaves the caller's original
    /// buffer orphaned and the caller must free it (B-2026-08-06-2 defect (B)),
    /// while an own-by-transfer param means the callee TOOK the buffer and a
    /// caller-side drop would be a double free.
    ///
    /// Restricted to the not-base-copy-supported case, which is the only one
    /// with a caller-side gap: a base-copy-supported struct is entry-copied on
    /// the non-generic arm and its caller drop already resolves by name.
    pub(super) fn mono_entry_copies_aggregate_param(&self, struct_name: &str) -> bool {
        if !self.type_decls.struct_types.contains_key(struct_name)
            || self.type_decls.shared_types.contains_key(struct_name)
        {
            return false;
        }
        if self.aggregate_param_copy_supported_struct(struct_name, &mut Vec::new()) {
            return false;
        }
        self.mono_struct_type_from_active_subst(struct_name)
            .is_some()
            && self.aggregate_param_copy_supported_struct_mono(struct_name)
    }

    /// B-2026-08-07-20 — is `struct_name` the SOLE owner of its own field heap
    /// at every death of the type?
    ///
    /// The gap this answers: the promotion gate in
    /// `emit_struct_drop_synthesis_impl` has two arms — copy-support and
    /// own-by-transfer — and a struct that owns a `shared` field plus a
    /// `Map`/`Set`-payload `Option` closes BOTH, each for its own correct
    /// reason (the `Map` handle is not duplicable; a shared-owning struct stays
    /// caller-retains per B-2026-08-05-32). Neither refusal is about ownership,
    /// which is what the gate actually wants to know, and the shape leaked its
    /// `Option` payload at every death.
    ///
    /// The two refusals are read here as EVIDENCE rather than as obstacles:
    /// `make_aggregate_param_callee_owned_inst` declines this shape (so no
    /// callee ever entry-copies it or registers a drop) and
    /// `move_declined_copy_struct_arg` returns early for it (so no caller ever
    /// retracts). Callee copies nothing, caller keeps everything — one owner,
    /// which is the question the gate asks, reached one step shorter than
    /// either proxy.
    ///
    /// The final conjunct is the scope, not a detail — see
    /// [`Self::struct_used_as_bare_by_value_param`].
    ///
    /// SPLIT IN TWO ON PURPOSE, and the split is a termination argument rather
    /// than a style choice. The `_core` half holds every conjunct that cannot
    /// re-enter copy-support analysis, and it is what the for-loop
    /// strict-shared arm in [`Self::field_copy_supported`] consults — that arm
    /// runs INSIDE `aggregate_param_copy_supported_struct`, so a predicate that
    /// called back into it would recurse without bound. The full predicate adds
    /// `!aggregate_param_copy_supported_struct` and is what the drop gate uses.
    ///
    /// The two sites still agree, which is the property that matters: when a
    /// struct IS copy-supported, the drop gate admits it on its FIRST arm and
    /// `for_loop_copy_supported` registers it on ITS first disjunct, so the
    /// extra conjunct only decides WHICH arm answers, never whether the two
    /// halves of the pairing disagree.
    pub(super) fn shared_owning_struct_sole_field_owner_core(&self, struct_name: &str) -> bool {
        if !self.type_decls.struct_types.contains_key(struct_name)
            || self.type_decls.shared_types.contains_key(struct_name)
        {
            return false;
        }
        if !self
            .type_decls
            .struct_generic_params
            .get(struct_name)
            .is_none_or(|g| g.is_empty())
        {
            return false;
        }
        self.shared_owning_struct_sole_field_owner_base(struct_name)
            && !self.struct_used_as_bare_by_value_param(struct_name)
    }

    /// [`Self::shared_owning_struct_sole_field_owner_core`] without its
    /// whole-struct by-value scope condition — the type-level half of the
    /// caller-retains story, which holds for the struct regardless of whether
    /// any callee takes it by value. Split out by B-2026-08-08-6 so the scope
    /// question can be asked per FIELD
    /// ([`Self::shared_owning_struct_field_sole_owner`]) instead of once for
    /// the whole type. Not a gate on its own — every caller must add one of the
    /// two scope conditions.
    pub(super) fn shared_owning_struct_sole_field_owner_base(&self, struct_name: &str) -> bool {
        if !self.type_decls.struct_types.contains_key(struct_name)
            || self.type_decls.shared_types.contains_key(struct_name)
        {
            return false;
        }
        if !self
            .type_decls
            .struct_generic_params
            .get(struct_name)
            .is_none_or(|g| g.is_empty())
        {
            return false;
        }
        self.struct_owns_shared_field(struct_name, &mut Vec::new())
            && !self.struct_is_self_referential(struct_name)
    }

    /// The per-FIELD caller-retains gate: is this struct's frame the sole owner
    /// of `field_name`, given that no by-value callee's body can take that
    /// field out of it?
    ///
    /// B-2026-08-08-6. [`Self::shared_owning_struct_sole_field_owner`] answers
    /// the same question for the whole type and has to decline as soon as the
    /// struct is a by-value param ANYWHERE, because one callee that moves one
    /// promoted field out would double-free. That is far more than the evidence
    /// supports: it also declines every OTHER promoted field, including ones no
    /// callee touches, and those keep a leak nothing in the program can free.
    /// Same conjuncts, with the whole-struct scope condition replaced by the
    /// field-granular one.
    pub(super) fn shared_owning_struct_field_sole_owner(
        &self,
        struct_name: &str,
        field_name: &str,
    ) -> bool {
        self.shared_owning_struct_sole_field_owner_base(struct_name)
            && !self.struct_by_value_param_body_takes_field(struct_name, field_name)
            && !self.aggregate_param_copy_supported_struct(struct_name, &mut Vec::new())
    }

    /// [`Self::shared_owning_struct_sole_field_owner_core`] plus the
    /// not-copy-supported conjunct. Used by the drop gate, where the
    /// copy-support arm is a sibling disjunct rather than the caller's frame.
    pub(super) fn shared_owning_struct_sole_field_owner(&self, struct_name: &str) -> bool {
        self.shared_owning_struct_sole_field_owner_core(struct_name)
            && !self.aggregate_param_copy_supported_struct(struct_name, &mut Vec::new())
    }

    /// Does ANY function, method, or `self` receiver in the program take
    /// `struct_name` as a BARE BY-VALUE parameter?
    ///
    /// B-2026-08-07-20 — the scope condition on the caller-retains disjunct in
    /// `emit_struct_drop_synthesis_impl`. A shared-owning struct is
    /// caller-retains at a call boundary (B-2026-08-05-32), so the callee gets a
    /// shallow copy, registers no drop, and any field it moves out is zeroed in
    /// ITS copy — a write the caller's frame never sees. Arming the caller's
    /// drop for a promoted `Option`/`Result` field therefore double-frees
    /// against a callee that moves that field out, in all three spellings
    /// (`match a.m { Some(x) => .. }`, `let mm = a.m`, and the escaping
    /// `Some(x) => x`), measured at 470 valgrind errors / 26 invalid frees at
    /// both opt levels. Closing that needs a callee-BODY predicate — "does this
    /// callee move this promoted field out of this param" — which is its own
    /// slice; until then the disjunct declines for any struct that could reach
    /// a call boundary at all.
    ///
    /// Whole-program and type-keyed ON PURPOSE. The drop fn is synthesized once
    /// per struct TYPE and runs at every death of that type, so its gate needs
    /// an answer true at every site at once — a per-site condition does not fit
    /// the synthesis (the mismatch this row's original text named as the real
    /// work). "Is this type ever a by-value param" is exactly such an answer,
    /// and it is conservative in the safe direction: a struct that is BOTH
    /// let-bound and passed by value keeps today's leak rather than gaining a
    /// double free.
    pub(super) fn struct_used_as_bare_by_value_param(&self, struct_name: &str) -> bool {
        let Some(program) = self.program_snapshot.as_deref() else {
            // No snapshot (REPL cell / partial compile): assume the worst.
            return true;
        };
        let is_bare = |ty: &TypeExpr| -> bool {
            matches!(&ty.kind, TypeKind::Path(p)
                if p.segments.last().map(String::as_str) == Some(struct_name))
        };
        let fn_takes =
            |f: &crate::ast::Function| -> bool { f.params.iter().any(|p| is_bare(&p.ty)) };
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    if fn_takes(f) {
                        return true;
                    }
                }
                Item::ImplBlock(b) => {
                    let target_is_self = is_bare(&b.target_type);
                    for it in &b.items {
                        let ImplItem::Method(m) = it else { continue };
                        if fn_takes(m) {
                            return true;
                        }
                        // A consuming receiver (`fn into_x(self)`) on an `impl`
                        // for this struct is a by-value param under another name.
                        if target_is_self && matches!(m.self_param, Some(SelfParam::Owned)) {
                            return true;
                        }
                    }
                }
                Item::TraitDef(t) => {
                    for it in &t.items {
                        let TraitItem::Method(m) = it else { continue };
                        if m.params.iter().any(|p| is_bare(&p.ty)) {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Does any callee that takes `struct_name` as a bare by-value param have a
    /// BODY that could take ownership of `field_name` out of it?
    ///
    /// B-2026-08-08-6 — the callee-body predicate
    /// [`Self::struct_used_as_bare_by_value_param`] promised and deferred. That
    /// one answers "could this type reach a call boundary at all", which is a
    /// SIGNATURE question, and it declines the caller-retains drop disjunct for
    /// the whole struct on a `yes` — preserving a leak (an `Option[Map]` field
    /// freed by nobody) to avoid a double free on the field a callee moves out.
    /// The two are not the same field. This asks the ownership question per
    /// FIELD, and per callee body, so a field that no by-value callee ever
    /// takes can arm its drop while the moved-out one keeps today's behaviour.
    ///
    /// Whole-program and type-keyed for the same reason its predecessor is: the
    /// drop fn is synthesized once per struct TYPE and runs at every death of
    /// that type, so the answer has to hold at every site at once. What changes
    /// is the granularity — `(type, field)` rather than `type`.
    ///
    /// Conservative in the safe direction at every step. No snapshot means
    /// assume the worst; an owned `self` receiver is a by-value param under
    /// another name; and the body test
    /// ([`crate::deque_head::expr_may_take_struct_field`]) treats any use of the
    /// param that is not a projection to a DIFFERENT field as taking the whole
    /// struct. A false `true` costs the pre-existing leak; a false `false`
    /// would cost a double free.
    pub(super) fn struct_by_value_param_body_takes_field(
        &self,
        struct_name: &str,
        field_name: &str,
    ) -> bool {
        let Some(program) = self.program_snapshot.as_deref() else {
            return true;
        };
        let is_bare = |ty: &TypeExpr| -> bool {
            matches!(&ty.kind, TypeKind::Path(p)
                if p.segments.last().map(String::as_str) == Some(struct_name))
        };
        // A param whose pattern is not a plain binding (a destructure) has no
        // name to track, so it is treated as taking the whole struct.
        let fn_takes_field = |f: &crate::ast::Function| -> bool {
            f.params.iter().any(|p| {
                if !is_bare(&p.ty) {
                    return false;
                }
                let PatternKind::Binding(name) = &p.pattern.kind else {
                    return true;
                };
                body_may_take_field(&f.body, name, field_name)
            })
        };
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    if fn_takes_field(f) {
                        return true;
                    }
                }
                Item::ImplBlock(b) => {
                    let target_is_self = is_bare(&b.target_type);
                    for it in &b.items {
                        let ImplItem::Method(m) = it else { continue };
                        if fn_takes_field(m) {
                            return true;
                        }
                        if target_is_self
                            && matches!(m.self_param, Some(SelfParam::Owned))
                            && body_may_take_field(&m.body, "self", field_name)
                        {
                            return true;
                        }
                    }
                }
                Item::TraitDef(t) => {
                    // A trait method's signature binds no body this struct can
                    // be reasoned about through — any bare param declines.
                    for it in &t.items {
                        let TraitItem::Method(m) = it else { continue };
                        if m.params.iter().any(|p| is_bare(&p.ty)) {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Does an owned by-value struct param of `struct_name` arrive OWN BY
    /// TRANSFER — the callee entry-copies nothing, takes the caller's own
    /// buffers, and registers the drop that frees them?
    ///
    /// This is the caller-side reading of the B-2026-08-05-33 arm in
    /// [`Self::make_aggregate_param_callee_owned_inst`], and it exists so both
    /// sides answer from ONE predicate. The arm's safety argument is a lockstep
    /// — "the caller already retracts its drop for exactly this shape" — and
    /// `move_declined_copy_struct_arg` honours it for an IDENTIFIER argument
    /// only. A fresh struct-LITERAL argument has no binding to retract, and its
    /// caller-temp drop is registered somewhere else entirely
    /// (`track_inline_owned_aggregate_arg_inst`), so the lockstep never reached
    /// it: `ig(S { a: f"..", m: map })` had two owners and double-freed at BOTH
    /// opt levels (B-2026-08-07-15).
    ///
    /// The conditions mirror that arm one for one — a non-shared user struct
    /// that is not copy-supported, does not own a `shared` field
    /// (B-2026-08-05-32: those keep the caller's drop, the callee never decs),
    /// and is not self-referential (B-2026-07-28-3: the callee may store the
    /// alias into an owning container, so it declines and the documented leak
    /// stays). One condition is ADDED rather than mirrored, and it is
    /// `callee_entry_copies_mono`: the arm tries
    /// [`Self::try_make_generic_struct_param_callee_owned`] FIRST, so a struct
    /// that takes the mono rescue is entry-copied and the caller's drop is
    /// right (B-2026-08-06-2 defect (B)).
    ///
    /// THE FLAG IS THE CALLEE'S ANSWER, NOT A CALLER-SIDE LOOK-ALIKE — that
    /// distinction is the whole reason it is a parameter. Only
    /// `compile_generic_call` can evaluate the rescue, because only it has the
    /// callee's substitution installed; it calls
    /// [`Self::mono_entry_copies_aggregate_param`] there and threads the result
    /// down. Every other call path passes `false`, which is correct rather than
    /// merely conservative: the rescue needs an active subst and there is none.
    ///
    /// B-2026-08-07-17 — the first cut of this row excluded EVERY generic
    /// struct instead, reasoning that a caller-side predicate cannot see the
    /// rescue. True, but it gives up more than the mono path: a generic struct
    /// also reaches a CONCRETE param (`fn take(x: Mix[String])`), where there
    /// is no monomorph, no subst, and no rescue — the callee takes the transfer
    /// arm like any other. `Mix[T] { v: T, s: String }` there kept both owners
    /// and stayed at 10 invalid frees per 10 iterations at BOTH opt levels
    /// after this row's fix landed. Erasure is just another way to fail
    /// copy-support (the bare `T` hits `field_copy_supported`'s conservative
    /// `_ => false`), so the shape belongs to this row, not beside it.
    pub(super) fn struct_param_owned_by_transfer(
        &self,
        struct_name: &str,
        callee_entry_copies_mono: bool,
    ) -> bool {
        if !self.type_decls.struct_types.contains_key(struct_name)
            || self.type_decls.shared_types.contains_key(struct_name)
        {
            return false;
        }
        if callee_entry_copies_mono {
            return false;
        }
        !self.aggregate_param_copy_supported_struct(struct_name, &mut Vec::new())
            && !self.struct_owns_shared_field(struct_name, &mut Vec::new())
            && !self.struct_is_self_referential(struct_name)
    }

    fn aggregate_param_copy_supported_struct_mono(&self, struct_name: &str) -> bool {
        if self.type_decls.shared_types.contains_key(struct_name) {
            return false;
        }
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return false;
        };
        // A struct with no heap-typed field after resolution has nothing to copy
        // — treat as unsupported so we don't needlessly flip it to callee-owned
        // (its base path already returned false, i.e. caller-retains no-op).
        let mut any_heap = false;
        for fte in &ftes {
            let resolved = self.subst_monomorph_type_params(fte);
            if !self.field_copy_supported(&resolved, &mut vec![struct_name.to_string()]) {
                return false;
            }
            if self.type_expr_has_drop_heap(&resolved) {
                any_heap = true;
            }
        }
        any_heap
    }

    /// Mono twin of [`deep_copy_struct_heap_fields_in_place`]: GEP at the
    /// CONCRETE mono struct layout (`mono_st`) and classify each field through
    /// its subst-resolved `TypeExpr`, so a generic struct param's bare-`T` heap
    /// fields (`Pair[String]`) are entry-copied at their real `{ptr,len,cap}`
    /// offsets. The base-layout twin GEPs the erased `{i64,…}` and reads bare
    /// `T`, so it copies nothing (B-2026-07-18-32).
    fn deep_copy_struct_heap_fields_in_place_mono(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        struct_name: &str,
        mono_st: StructType<'ctx>,
    ) {
        let Some(ftes) = self
            .type_decls
            .struct_field_type_exprs
            .get(struct_name)
            .cloned()
        else {
            return;
        };
        for (i, fte) in ftes.iter().enumerate() {
            let resolved = self.subst_monomorph_type_params(fte);
            self.deep_copy_one_aggregate_field(base_ptr, mono_st, i as u32, &resolved);
        }
    }

    /// Build the concrete generic instantiation `TypeExpr` (`Pair[String]`) for
    /// `struct_name` from the ACTIVE monomorph subst, so a callee-owned generic
    /// param's scope-exit drop is registered as the per-monomorph
    /// `__karac_drop_struct_Pair$str`. `None` when the struct declares no
    /// generic params or the subst binds none of them (in which case the caller
    /// falls back to the base name-keyed drop).
    fn active_subst_struct_inst(&self, struct_name: &str) -> Option<TypeExpr> {
        use crate::ast::{GenericArg, PathExpr};
        use crate::token::Span;
        let params = self.type_decls.struct_generic_params.get(struct_name)?;
        if params.is_empty() {
            return None;
        }
        let mut args = Vec::with_capacity(params.len());
        for p in params {
            let te = if let Some(full) = self.mono_state.type_subst_type_exprs.get(p) {
                full.clone()
            } else {
                let name = self.mono_state.type_subst_names.get(p)?;
                TypeExpr {
                    kind: TypeKind::Path(PathExpr {
                        segments: vec![name.clone()],
                        generic_args: None,
                        span: Span::default(),
                    }),
                    span: Span::default(),
                }
            };
            args.push(GenericArg::Type(te));
        }
        Some(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![struct_name.to_string()],
                generic_args: Some(args),
                span: Span::default(),
            }),
            span: Span::default(),
        })
    }

    /// Rc-INC the shared box handle stored at `slot` (an 8-byte RC pointer word),
    /// null-guarded. The "copy" of a shared handle is a refcount bump so the copy
    /// co-owns the box, symmetric with the drop's rc-DEC. Used by the
    /// B-2026-07-09-12 clone-on-extract (view-destructure) path.
    pub(super) fn rc_inc_shared_handle_at_slot(
        &self,
        slot: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let handle = self
            .builder
            .build_load(ptr_ty, slot, "viewdup.handle")
            .unwrap()
            .into_pointer_value();
        let is_null = self
            .builder
            .build_is_null(handle, "viewdup.isnull")
            .unwrap();
        let do_bb = self.context.append_basic_block(fn_val, "viewdup.inc.do");
        let cont_bb = self.context.append_basic_block(fn_val, "viewdup.inc.cont");
        self.builder
            .build_conditional_branch(is_null, cont_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        self.emit_refcount_inc_by_type(heap_type, handle);
        self.builder.build_unconditional_branch(cont_bb).unwrap();
        self.builder.position_at_end(cont_bb);
    }

    /// Rc-INC every element of a `Vec[shared]` value at `vec_field_ptr` whose
    /// outer `{ptr,len,cap}` buffer was just deep-copied, so the duplicated Vec
    /// independently co-owns each element box. Mirrors the whole-Vec drop's
    /// per-element rc-DEC (`emit_vec_elem_rc_dec_fn`: load handle, null-check,
    /// rc-dec). VIEW-SCOPED (B-2026-07-09-12 clone-on-extract): deliberately NOT
    /// wired into the shared `deep_copy_*` param-copy machinery, whose earlier
    /// `Vec[shared]` arm double-inc'd against other per-site inc paths (the
    /// reverted param-path leak). Here the only inc is this one and the leaf's own
    /// per-element rc-dec drop balances it.
    pub(super) fn rc_inc_vec_shared_elements(
        &mut self,
        vec_field_ptr: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let (Ok(data_pp), Ok(len_pp)) = (
            self.builder
                .build_struct_gep(vec_ty, vec_field_ptr, 0, "viewvsh.data.pp"),
            self.builder
                .build_struct_gep(vec_ty, vec_field_ptr, 1, "viewvsh.len.pp"),
        ) else {
            return;
        };
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "viewvsh.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_pp, "viewvsh.len")
            .unwrap()
            .into_int_value();
        let loop_bb = self.context.append_basic_block(fn_val, "viewvsh.loop");
        let body_bb = self.context.append_basic_block(fn_val, "viewvsh.body");
        let exit_bb = self.context.append_basic_block(fn_val, "viewvsh.exit");
        let pre_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();
        self.builder.position_at_end(loop_bb);
        let idx_phi = self.builder.build_phi(i64_t, "viewvsh.i").unwrap();
        idx_phi.add_incoming(&[(&i64_t.const_int(0, false), pre_bb)]);
        let i = idx_phi.as_basic_value().into_int_value();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "viewvsh.cmp")
            .unwrap();
        self.builder
            .build_conditional_branch(in_range, body_bb, exit_bb)
            .unwrap();
        self.builder.position_at_end(body_bb);
        // Each element slot is one pointer-width RC handle.
        let slot = unsafe {
            self.builder
                .build_gep(ptr_ty, data, &[i], "viewvsh.slot")
                .unwrap()
        };
        self.rc_inc_shared_handle_at_slot(slot, heap_type);
        let body_end = self.builder.get_insert_block().unwrap();
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "viewvsh.next")
            .unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();
        idx_phi.add_incoming(&[(&next, body_end)]);
        self.builder.position_at_end(exit_bb);
    }

    /// Copy one aggregate field in place per its TypeExpr. String/Vec → outer
    /// buffer copy; nested struct → recurse; tuple → recurse per element;
    /// everything else (primitive, borrow, ignored kinds) → no-op.
    pub(super) fn deep_copy_one_aggregate_field(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        agg_ty: StructType<'ctx>,
        idx: u32,
        fte: &TypeExpr,
    ) {
        let vec_ty = self.vec_struct_type();
        // B-2026-07-10-4 — clone-on-extract mode only (`deep_copy_rc_inc_bare_shared`):
        // a bare `shared` field is a shallow pointer in the copy; rc-INC it so the
        // clone independently co-owns the box, symmetric with the leaf's combined
        // struct-drop rc-DEC. The ENTRY-COPY path leaves the flag false and skips
        // this (its shared handling is unchanged — a bump there leaked).
        if self.drop_rc.deep_copy_rc_inc_bare_shared {
            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(fte) {
                if let Ok(field_ptr) =
                    self.builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.shclone")
                {
                    self.rc_inc_shared_handle_at_slot(field_ptr, heap_ty);
                }
                return;
            }
        }
        // String / Vec field → copy the OUTER buffer in place (`elem_te = None`),
        // mirroring the struct drop's outer-only free (nested Vec elements are a
        // bounded leak on both sides, never corruption).
        let elem_ty: Option<BasicTypeEnum<'ctx>> = if self.is_string_type_expr(fte) {
            Some(self.context.i8_type().into())
        } else {
            self.extract_vec_elem_type(fte)
        };
        if let Some(elem_ty) = elem_ty {
            // B-2026-07-03-28 — copy-depth must equal drop-depth. The struct drop
            // DRAINS a `Vec[elem]` field's `String`/`Map`/`Set`/nested-`Vec`
            // elements (`emit_struct_drop_synthesis`'s VecOrString arm via
            // `elem_te_needs_direct_recursive_drain`), so this entry-copy must
            // element-DEEP copy exactly those shapes — else the callee's copy
            // would share the caller's element buffers and both drains would
            // free them (the test-1 double-free). `emit_vecstr_defensive_copy`'s
            // element-deep mode (`elem_te = Some`) duplicates each element's
            // String / Map / Set / inner-Vec buffer; other element shapes stay
            // outer-only (`None`), matching the drop's outer-only handling for
            // them.
            // B-2026-08-25-10 — resolve the element through the active
            // monomorph substitution BEFORE classifying it. Inside
            // `impl[T] Heap[T]`, a field declared `xs: Vec[T]` yields the bare
            // param `T`, which is neither String nor Vec nor Map/Set by name,
            // so `elem_te_needs_direct_recursive_drain` said "no" and the
            // entry-copy stayed outer-only. The mono's struct drop, however,
            // resolves `T` and IS element-deep, so the copy aliased the
            // caller's element buffers and both drains freed them — the exact
            // copy-depth/drop-depth mismatch the comment above warns about,
            // reached through the one path that erases the element's identity.
            //
            // `subst_monomorph_type_params` consults `type_subst_type_exprs`
            // first, so `T` recovers its FULL concrete type (`Vec[i64]`, not
            // the head-only `Vec` that `type_subst_names` would give) — which
            // the recursive copy needs to size the inner element. Outside a
            // monomorph it is a no-op clone, so every concrete field keeps its
            // existing behaviour.
            let inner_te = crate::codegen::helpers::vec_inner_type_expr(fte)
                .map(|te| self.subst_monomorph_type_params(&te));
            let deep_elem_te = inner_te
                .clone()
                .filter(Self::elem_te_needs_direct_recursive_drain);
            if let Ok(field_ptr) = self
                .builder
                .build_struct_gep(agg_ty, base_ptr, idx, "p14.f")
            {
                if let Ok(val) = self.builder.build_load(vec_ty, field_ptr, "p14.v") {
                    let copied =
                        self.emit_vecstr_defensive_copy(val, elem_ty, deep_elem_te.as_ref());
                    let _ = self.builder.build_store(field_ptr, copied);
                }
                // B-2026-07-04-9(a) — a `Vec[struct]` / `Vec[enum]` / `Vec[Option]`
                // element whose per-element drop
                // (`vec_elem_agg_drop_for_type_expr`) frees inner heap the OUTER
                // `{ptr,len,cap}` copy above cannot reach (`type_expr_has_drop_heap`
                // is FALSE for an all-`Option` struct like `ArgN`, so the
                // `emit_vecstr_defensive_copy` agg branch — and `emit_clone_fn`,
                // whose Option copy is shallow — both miss it). After the outer
                // buffer is duplicated, deep-copy each copied element in place with
                // the SAME machinery the entry-copy uses for a nested struct field
                // (`deep_copy_struct_heap_fields_in_place` / enum / Option), which —
                // unlike `emit_clone_fn` — duplicates `Option[String]` buffers and
                // rc-INCs `Option[shared]` boxes, symmetric with the drop's
                // per-element free / rc-dec. Without this the copied element buffers
                // alias the source and both drains free them (double-free in
                // `__karac_drop_struct_<Outer>`).
                if let Some(elem_te) = inner_te.as_ref() {
                    self.deep_copy_vec_aggregate_elements_in_place(agg_ty, base_ptr, idx, elem_te);
                }
            }
            return;
        }
        // Nested non-shared user struct → recurse into it in place.
        if let TypeKind::Path(p) = &fte.kind {
            if let Some(head) = p.segments.first() {
                if self.type_decls.struct_types.contains_key(head.as_str())
                    && !self.type_decls.shared_types.contains_key(head.as_str())
                {
                    if let Ok(field_ptr) = self
                        .builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.nf")
                    {
                        let name = head.clone();
                        self.deep_copy_struct_heap_fields_in_place(field_ptr, &name);
                    }
                    return;
                }
            }
        }
        // Nested user-ENUM field (#19 FIXED) → deep-copy its live-variant
        // Vec/String payload in place, mirroring the struct drop's per-field enum
        // free (`emit_struct_drop_synthesis`'s `EnumField` arm → `__karac_drop_<E>`).
        // `deep_copy_enum_heap_payload_in_place` duplicates exactly the
        // `VecOrString` payloads `EnumDropKind` frees, so the entry-copy stays
        // symmetric with the drop. Shared enums / Option / Result never reach here
        // — `field_copy_supported` bails on them, so the struct is caller-retains.
        if let TypeKind::Path(p) = &fte.kind {
            if let Some(head) = p.segments.first() {
                if let Some(layout) = self.type_decls.enum_layouts.get(head.as_str()).cloned() {
                    if !layout.is_shared && head != "Option" && head != "Result" {
                        if let Ok(field_ptr) = self
                            .builder
                            .build_struct_gep(agg_ty, base_ptr, idx, "p14.ef")
                        {
                            let name = head.clone();
                            self.deep_copy_enum_heap_payload_in_place(&name, field_ptr, &layout);
                        }
                        return;
                    }
                }
            }
        }
        // B-2026-07-03-28 Facet A — an `Option[String]`/`Option[Vec[..]]` field
        // (inline `{ptr,len,cap}` payload): deep-copy the `Some` buffer in place
        // so a callee-owned param owns it independently, symmetric with the
        // struct drop's `OptionInline` free. `field_copy_supported` already
        // vetted the payload class, so any Option reaching here is copyable.
        if let TypeKind::Path(p) = &fte.kind {
            if p.segments.last().map(|s| s.as_str()) == Some("Option") {
                if let Ok(field_ptr) = self
                    .builder
                    .build_struct_gep(agg_ty, base_ptr, idx, "p14.of")
                {
                    self.deep_copy_option_inline_payload_in_place(field_ptr, fte);
                }
                return;
            }
        }
        // B-2026-07-21-15 — a `Result[T, E]` field in the direct-String/Vec-
        // halves class: deep-copy the LIVE half's `{ptr,len,cap}` overlay in
        // place, symmetric with the struct drop's Result overlay free (the
        // OptionInline classifier's Result extension). `field_copy_supported`
        // vetted the class, so any Result reaching here is copyable.
        if let TypeKind::Path(p) = &fte.kind {
            if p.segments.last().map(|s| s.as_str()) == Some("Result") {
                if let Ok(field_ptr) = self
                    .builder
                    .build_struct_gep(agg_ty, base_ptr, idx, "p14.rf")
                {
                    // B-2026-08-03-3 leg B — the two admitted Result classes are
                    // structurally disjoint (a direct String/Vec heap half vs a
                    // struct/enum heap half), so exactly one helper applies.
                    if self.result_field_struct_enum_payload_ok(fte) {
                        self.deep_copy_result_struct_enum_payload_in_place(field_ptr, fte);
                    } else {
                        self.deep_copy_result_inline_heap_halves_in_place(field_ptr, fte);
                    }
                }
                return;
            }
        }
        // Tuple field → recurse into each element.
        if let TypeKind::Tuple(elems) = &fte.kind {
            if !elems.is_empty() {
                if let (Ok(field_ptr), Some(BasicTypeEnum::StructType(tup_ty))) = (
                    self.builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14.tf"),
                    agg_ty.get_field_type_at_index(idx),
                ) {
                    for (j, ete) in elems.iter().enumerate() {
                        self.deep_copy_one_aggregate_field(field_ptr, tup_ty, j as u32, ete);
                    }
                }
            }
        }
        // Primitive / borrow / ignored kind → nothing to copy.
    }

    /// B-2026-07-04-9(a) — deep-copy each element of an already-outer-copied
    /// `Vec[<aggregate>]` struct FIELD in place. The outer buffer copy above
    /// duplicates the `{ptr,len,cap}` array, but each element is a shallow
    /// bit-copy still aliasing the source's per-element heap; the struct drop
    /// DRAINS those elements (`vec_elem_agg_drop_for_type_expr`), so without a
    /// per-element deep copy the callee's whole-drop and the caller's retained
    /// drop free the SAME element buffers (double-free in
    /// `__karac_drop_struct_<Outer>`). This reuses the SAME field-copy machinery
    /// the entry-copy uses for a nested aggregate field — a struct element via
    /// `deep_copy_struct_heap_fields_in_place`, an enum element via
    /// `deep_copy_enum_heap_payload_in_place`, an `Option` element via
    /// `deep_copy_option_inline_payload_in_place` — which (unlike the
    /// `emit_vecstr_defensive_copy` / `emit_clone_fn` agg path, shallow for
    /// `Option`) duplicates `Option[String]` buffers and rc-INCs `Option[shared]`
    /// boxes, symmetric with the per-element drop's free / rc-dec. Bare `shared`
    /// elements (`Vec[shared]` — an 8-byte RC pointer slot) and no-heap elements
    /// are skipped: the former's drop is a pure rc-dec needing a paired
    /// per-element rc-inc (a distinct residual), the latter needs no copy.
    fn deep_copy_vec_aggregate_elements_in_place(
        &mut self,
        agg_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        elem_te: &TypeExpr,
    ) {
        // B-2026-07-10-4 — clone-on-extract / symmetric-entry-copy mode only: a bare
        // `shared` element (`Vec[TypeExpr]` variant-field list, `Vec[Expr]` arg list)
        // is an 8-byte RC pointer the outer buffer copy aliased without a refcount
        // bump; rc-INC each so the duplicate co-owns the element boxes, symmetric
        // with the struct drop's per-element rc-dec (`vec_elem_agg_drop_for_type_expr`
        // → `__karac_vec_elem_rc_dec_<T>`). Flag off (plain entry-copy) skips this —
        // its Vec[shared] element handling is unchanged.
        if self.drop_rc.deep_copy_rc_inc_bare_shared {
            if let Some(heap_ty) = self.shared_heap_type_for_type_expr(elem_te) {
                if let Ok(field_ptr) =
                    self.builder
                        .build_struct_gep(agg_ty, base_ptr, idx, "p14a.shvec")
                {
                    self.rc_inc_vec_shared_elements(field_ptr, heap_ty);
                }
                return;
            }
        }
        // Classify the element; bail unless it is a value-deep-copyable
        // aggregate whose per-element drop frees inner heap.
        enum ElemCopy {
            Struct(String),
            Enum(String),
            Option,
        }
        let plan = match &elem_te.kind {
            TypeKind::Path(p) => {
                let name = p.segments.first().map(String::as_str).unwrap_or("");
                if name == "Option" {
                    // Only the inline `Some`-payload shapes the drop actually
                    // frees (`vec_elem_agg_drop_for_type_expr`'s Option arm).
                    let frees = Self::option_payload_te(elem_te)
                        .map(|pt| {
                            self.option_payload_inline_recursive_drop_ok(&pt)
                                || self.option_payload_struct_or_enum_drop_ok(&pt)
                        })
                        .unwrap_or(false);
                    frees.then_some(ElemCopy::Option)
                } else if self.shared_heap_type_for_type_expr(elem_te).is_some() {
                    // Bare `shared` element — rc-inc case, handled elsewhere.
                    None
                } else if self.type_decls.struct_types.contains_key(name)
                    && !self.type_decls.shared_types.contains_key(name)
                {
                    Some(ElemCopy::Struct(name.to_string()))
                } else if self
                    .type_decls
                    .enum_layouts
                    .get(name)
                    .map(|l| !l.is_shared)
                    .unwrap_or(false)
                {
                    Some(ElemCopy::Enum(name.to_string()))
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(plan) = plan else {
            return;
        };

        let fn_val = match self.current_fn {
            Some(f) => f,
            None => return,
        };
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let elem_ty = self.llvm_type_for_type_expr(elem_te);

        // Reload the (now outer-copied) Vec field's data ptr + len.
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(agg_ty, base_ptr, idx, "p14a.f")
        else {
            return;
        };
        let (Ok(data_pp), Ok(len_pp)) = (
            self.builder
                .build_struct_gep(vec_ty, field_ptr, 0, "p14a.data.pp"),
            self.builder
                .build_struct_gep(vec_ty, field_ptr, 1, "p14a.len.pp"),
        ) else {
            return;
        };
        let data = self
            .builder
            .build_load(ptr_ty, data_pp, "p14a.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_pp, "p14a.len")
            .unwrap()
            .into_int_value();

        // Per-element loop `0..len` (empty Vec runs zero iterations).
        let loop_bb = self.context.append_basic_block(fn_val, "p14a.loop");
        let body_bb = self.context.append_basic_block(fn_val, "p14a.body");
        let exit_bb = self.context.append_basic_block(fn_val, "p14a.exit");
        let pre_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();

        self.builder.position_at_end(loop_bb);
        let idx_phi = self.builder.build_phi(i64_t, "p14a.i").unwrap();
        idx_phi.add_incoming(&[(&i64_t.const_int(0, false), pre_bb)]);
        let i = idx_phi.as_basic_value().into_int_value();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "p14a.cmp")
            .unwrap();
        self.builder
            .build_conditional_branch(in_range, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let slot = unsafe {
            self.builder
                .build_gep(elem_ty, data, &[i], "p14a.slot")
                .unwrap()
        };
        match &plan {
            ElemCopy::Struct(name) => self.deep_copy_struct_heap_fields_in_place(slot, name),
            ElemCopy::Enum(name) => {
                if let Some(layout) = self.type_decls.enum_layouts.get(name).cloned() {
                    self.deep_copy_enum_heap_payload_in_place(name, slot, &layout);
                }
            }
            ElemCopy::Option => self.deep_copy_option_inline_payload_in_place(slot, elem_te),
        }
        // A sub-copy may have appended blocks and moved the insert point —
        // branch back from wherever we now are.
        let body_end = self.builder.get_insert_block().unwrap();
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "p14a.next")
            .unwrap();
        self.builder.build_unconditional_branch(loop_bb).unwrap();
        idx_phi.add_incoming(&[(&next, body_end)]);

        self.builder.position_at_end(exit_bb);
    }

    /// Deep-copy the live variant's Vec/String payload of
    /// the enum value at `base_ptr`. Emits a tag switch mirroring
    /// `emit_enum_drop_switch`; only variants with a VecOrString payload get a
    /// case. The enum's payload words are stored as raw i64s (data = ptrtoint,
    /// then len, then cap), so the copy reconstructs a `{ptr,len,cap}` value,
    /// runs `emit_vecstr_defensive_copy`, and writes the copied words back.
    ///
    /// ELEMENT-deep since B-2026-08-09-13, and it must stay that way: this is
    /// the copy half of the copy-depth == drop-depth invariant in this module's
    /// doc, and `emit_enum_drop_switch`'s `VecOrString` arm now drains a
    /// `Vec[heap-element]` payload's elements before freeing its buffer. An
    /// outer-only copy against an element-deep drop is a double-free — the two
    /// copies would share element buffers and both drops would free them.
    ///
    /// It was outer-only through B-2026-08-09-9, when the drop was outer-only
    /// too: element copies then had NO owner, so deepening this side alone
    /// leaked 1990 bytes across 300 allocations in
    /// `asan_match_bound_struct_variant_vec_field_reborrow_no_double_free`. The
    /// interim `_with_elements` variant existed for the two match-clone callers
    /// that hand their elements to a consumer; with the drop deepened the
    /// distinction is gone and every caller takes the same depth.
    ///
    /// Depth is bounded by what `emit_vecstr_defensive_copy` can duplicate
    /// (String / Vec / Map / Set / heap-owning aggregate elements) — the same
    /// set `elem_te_needs_direct_recursive_drain` plus
    /// `vec_elem_agg_drop_for_type_expr` drain on the other side.
    pub(super) fn deep_copy_enum_heap_payload_in_place(
        &mut self,
        enum_name: &str,
        base_ptr: PointerValue<'ctx>,
        layout: &EnumLayout<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let enum_ty = layout.llvm_type;
        let fn_val = self.current_fn.unwrap();

        // Per-variant payload element TypeExprs (for buffer-element sizing).
        let variant_tes: HashMap<String, Vec<TypeExpr>> = self
            .enum_variant_field_type_exprs(enum_name)
            .into_iter()
            .map(|(_tag, name, tes)| (name, tes))
            .collect();

        let tag_ptr = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, 0, "p14e.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14e.tag")
            .unwrap()
            .into_int_value();

        let mut tag_entries: Vec<(String, u64)> =
            layout.tags.iter().map(|(n, t)| (n.clone(), *t)).collect();
        tag_entries.sort_by_key(|(_, t)| *t);

        let merge_bb = self.context.append_basic_block(fn_val, "p14e.merge");
        let mut cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut case_bbs: Vec<(String, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        for (name, tag_v) in &tag_entries {
            let has_heap = layout
                .field_drop_kinds
                .get(name)
                .map(|ks| ks.iter().any(|k| k.is_heap_bearing()))
                .unwrap_or(false);
            if !has_heap {
                continue;
            }
            let bb = self
                .context
                .append_basic_block(fn_val, &format!("p14e.{name}"));
            cases.push((i64_t.const_int(*tag_v, false), bb));
            case_bbs.push((name.clone(), bb));
        }

        self.builder.build_switch(tag, merge_bb, &cases).unwrap();

        for (name, bb) in &case_bbs {
            self.builder.position_at_end(*bb);
            if let (Some(kinds), Some(offsets)) = (
                layout.field_drop_kinds.get(name),
                layout.field_word_offsets.get(name),
            ) {
                for (fi, (kind, (start_word, _num_words))) in
                    kinds.iter().zip(offsets.iter()).enumerate()
                {
                    // B-2026-06-13-13: a nested-struct payload is deep-copied by
                    // recursing into the struct's own heap fields in place — the
                    // symmetric peer of the enum drop's `NestedStruct` arm, so the
                    // callee copy and caller temp own independent buffers (no
                    // double-free). The struct's words start at `start_word + 1`.
                    if *kind == EnumDropKind::NestedStruct {
                        let sname =
                            variant_tes
                                .get(name)
                                .and_then(|tes| tes.get(fi))
                                .and_then(|te| match &te.kind {
                                    TypeKind::Path(p) => p.segments.first().cloned(),
                                    _ => None,
                                });
                        if let Some(sname) = sname {
                            if let Ok(field_ptr) = self.builder.build_struct_gep(
                                enum_ty,
                                base_ptr,
                                (*start_word + 1) as u32,
                                "p14e.nstruct.p",
                            ) {
                                self.deep_copy_struct_heap_fields_in_place(field_ptr, &sname);
                            }
                        }
                        continue;
                    }
                    // B-2026-07-23-11: a `Map`/`Set`(-family) payload — deep-clone
                    // the handle in place (src == dst == the payload-word slot) via
                    // the map clone fn, so a callee-owned by-value enum param owns
                    // an INDEPENDENT kv-table. The symmetric peer of the enum
                    // drop's `MapOrSet` arm: without it the callee copy and caller
                    // temp alias one handle and both drops free it (double-free).
                    // `emit_map_clone_fn` reads the old handle, allocates a fresh
                    // deep clone, and writes the new handle back to the slot (it
                    // does NOT free the old one — the caller retains it). The
                    // payload word is one handle at `start_word + 1`.
                    if *kind == EnumDropKind::MapOrSet {
                        let kv = variant_tes
                            .get(name)
                            .and_then(|tes| tes.get(fi))
                            .and_then(|te| {
                                if let Some((k, v)) = super::helpers::map_kv_type_exprs(te) {
                                    Some((k, v))
                                } else {
                                    super::helpers::set_inner_type_expr(te).map(|elem| {
                                        // `Set[T]` clones as `Map[T, ()]` — the unit
                                        // value half (matches the clone-fn Set arm).
                                        let unit_te = TypeExpr {
                                            kind: TypeKind::Tuple(Vec::new()),
                                            span: elem.span,
                                        };
                                        (elem, unit_te)
                                    })
                                }
                            });
                        if let Some((k_te, v_te)) = kv {
                            if let Ok(field_ptr) = self.builder.build_struct_gep(
                                enum_ty,
                                base_ptr,
                                (*start_word + 1) as u32,
                                "p14e.map.p",
                            ) {
                                let clone_fn = self.emit_map_clone_fn(&k_te, &v_te);
                                self.builder
                                    .build_call(clone_fn, &[field_ptr.into(), field_ptr.into()], "")
                                    .unwrap();
                            }
                        }
                        continue;
                    }
                    if *kind != EnumDropKind::VecOrString {
                        continue;
                    }
                    let data_idx = (*start_word + 1) as u32;
                    let len_idx = (*start_word + 2) as u32;
                    let cap_idx = (*start_word + 3) as u32;

                    let data_w = self.load_enum_word(enum_ty, base_ptr, data_idx, "p14e.data");
                    let len_w = self.load_enum_word(enum_ty, base_ptr, len_idx, "p14e.len");
                    let cap_w = self.load_enum_word(enum_ty, base_ptr, cap_idx, "p14e.cap");
                    let data_p = self
                        .builder
                        .build_int_to_ptr(data_w, ptr_ty, "p14e.data.p")
                        .unwrap();

                    // Reconstruct the {ptr,len,cap} value the defensive copy expects.
                    let mut sv = vec_ty.get_undef();
                    sv = self
                        .builder
                        .build_insert_value(sv, data_p, 0, "p14e.sv.d")
                        .unwrap()
                        .into_struct_value();
                    sv = self
                        .builder
                        .build_insert_value(sv, len_w, 1, "p14e.sv.l")
                        .unwrap()
                        .into_struct_value();
                    sv = self
                        .builder
                        .build_insert_value(sv, cap_w, 2, "p14e.sv.c")
                        .unwrap()
                        .into_struct_value();

                    let elem_ty: BasicTypeEnum<'ctx> = variant_tes
                        .get(name)
                        .and_then(|tes| tes.get(fi))
                        .map(|te| {
                            if self.is_string_type_expr(te) {
                                self.context.i8_type().into()
                            } else {
                                self.extract_vec_elem_type(te)
                                    .unwrap_or_else(|| self.context.i8_type().into())
                            }
                        })
                        .unwrap_or_else(|| self.context.i8_type().into());

                    // B-2026-08-09-9/-13 — element depth, unconditional since the
                    // drop side drains too (see this fn's doc). A `String`
                    // payload has no inner (its "elements" are bytes, already
                    // covered by the buffer copy), so it stays `None`.
                    let elem_te = variant_tes
                        .get(name)
                        .and_then(|tes| tes.get(fi))
                        .filter(|te| !self.is_string_type_expr(te))
                        .and_then(super::helpers::vec_inner_type_expr);
                    let copied = self
                        .emit_vecstr_defensive_copy(sv.into(), elem_ty, elem_te.as_ref())
                        .into_struct_value();
                    let cd = self
                        .builder
                        .build_extract_value(copied, 0, "p14e.cd")
                        .unwrap()
                        .into_pointer_value();
                    let cl = self
                        .builder
                        .build_extract_value(copied, 1, "p14e.cl")
                        .unwrap()
                        .into_int_value();
                    let cc = self
                        .builder
                        .build_extract_value(copied, 2, "p14e.cc")
                        .unwrap()
                        .into_int_value();
                    let cd_w = self
                        .builder
                        .build_ptr_to_int(cd, i64_t, "p14e.cd.w")
                        .unwrap();

                    self.store_enum_word(enum_ty, base_ptr, data_idx, cd_w.into());
                    self.store_enum_word(enum_ty, base_ptr, len_idx, cl.into());
                    self.store_enum_word(enum_ty, base_ptr, cap_idx, cc.into());
                }
            }
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);
    }

    /// B-2026-07-03-28 Facet A — deep-copy an `Option[String]` / `Option[Vec[..]]`
    /// FIELD's inline `Some` payload in place, so a callee-owned by-value
    /// aggregate param owns a buffer independent of the caller's retained
    /// original. The type-erased `Option` layout carries no payload drop-kind, so
    /// this is TYPE-AWARE off the field's `TypeExpr` (the copy peer of the
    /// type-aware `emit_option_drop_fn`): tag-switch on `Some`, reconstruct the
    /// inline `{ptr,len,cap}` from words 1..3, run `emit_vecstr_defensive_copy`
    /// (element-DEEP for a `Vec[String]`/collection payload, matching the drop),
    /// and write the fresh `{ptr,len,cap}` words back. `None`-tag runs nothing.
    /// Only the inline-`{ptr,len,cap}` payload class is handled here (the same
    /// class `option_inline_payload_elem` recognises); `field_copy_supported`'s
    /// `Option` arm gates callers to exactly that, keeping copy == drop.
    pub(super) fn deep_copy_option_inline_payload_in_place(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        opt_te: &TypeExpr,
    ) {
        // B-2026-07-03-28 shared leg — an `Option[shared]` payload is a single
        // inline RC box pointer (word 1, ptrtoint), NOT an `{ptr,len,cap}`
        // buffer. The caller-retains entry-copy of it is an rc-INC of the box
        // when Some (so the callee's copy holds an independent ref), the exact
        // peer of `emit_nested_struct_shared_rc_decs_ex`'s `Option[shared]`
        // rc-DEC arm. Handle it before the String/Vec buffer-copy path (which
        // would `return` early on a shared payload).
        if let Some((_, inner_info)) = self.option_inner_shared_type_for_type_expr(opt_te) {
            self.rc_inc_option_inline_shared_payload_in_place(field_ptr, inner_info.heap_type);
            return;
        }
        let Some(payload_te) = Self::option_payload_te(opt_te) else {
            return;
        };
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let fn_val = self.current_fn.unwrap();
        let Some(layout) = self.type_decls.enum_layouts.get("Option").cloned() else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);

        // Element type + (for a Vec[collection] payload) the element-deep TypeExpr,
        // mirroring the drained-Vec entry-copy so a `Vec[String]` payload's char
        // buffers are copied too, matching `emit_option_drop_fn`'s deep free.
        let (elem_ty, deep_elem_te): (BasicTypeEnum<'ctx>, Option<TypeExpr>) =
            if self.is_string_type_expr(&payload_te) {
                (self.context.i8_type().into(), None)
            } else if let Some(et) = self.extract_vec_elem_type(&payload_te) {
                let inner = crate::codegen::helpers::vec_inner_type_expr(&payload_te)
                    .filter(Self::elem_te_needs_direct_recursive_drain);
                (et, inner)
            } else {
                // B-2026-07-04-7 — a non-shared struct/enum payload (BOXED when
                // wider than the 3-word inline area, else inline in words 1..3),
                // not the `{ptr,len,cap}` overlay this fn's buffer-copy path
                // handles. Deep-copy it via the box-aware peer of
                // `emit_option_drop_fn`'s boxed/inline payload free. Pass the OUTER
                // `opt_te` (`Option[Val]`) — the helper re-extracts the payload
                // itself; passing `payload_te` would make its `option_payload_te`
                // return `None` and silently copy nothing (→ shared box → double-free).
                self.deep_copy_option_struct_enum_payload_in_place(field_ptr, opt_te);
                return;
            };

        let tag_ptr = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 0, "p14o.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14o.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "p14o.some",
            )
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "p14o.some");
        let merge_bb = self.context.append_basic_block(fn_val, "p14o.merge");
        self.builder
            .build_conditional_branch(is_some, some_bb, merge_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        // Words: data=idx1, len=idx2, cap=idx3.
        let data_w = self.load_enum_word(option_ty, field_ptr, 1, "p14o.data");
        let len_w = self.load_enum_word(option_ty, field_ptr, 2, "p14o.len");
        let cap_w = self.load_enum_word(option_ty, field_ptr, 3, "p14o.cap");
        let data_p = self
            .builder
            .build_int_to_ptr(data_w, ptr_ty, "p14o.data.p")
            .unwrap();
        let mut sv = vec_ty.get_undef();
        sv = self
            .builder
            .build_insert_value(sv, data_p, 0, "p14o.sv.d")
            .unwrap()
            .into_struct_value();
        sv = self
            .builder
            .build_insert_value(sv, len_w, 1, "p14o.sv.l")
            .unwrap()
            .into_struct_value();
        sv = self
            .builder
            .build_insert_value(sv, cap_w, 2, "p14o.sv.c")
            .unwrap()
            .into_struct_value();
        let copied = self
            .emit_vecstr_defensive_copy(sv.into(), elem_ty, deep_elem_te.as_ref())
            .into_struct_value();
        let cd = self
            .builder
            .build_extract_value(copied, 0, "p14o.cd")
            .unwrap()
            .into_pointer_value();
        let cl = self
            .builder
            .build_extract_value(copied, 1, "p14o.cl")
            .unwrap();
        let cc = self
            .builder
            .build_extract_value(copied, 2, "p14o.cc")
            .unwrap();
        let cd_w = self
            .builder
            .build_ptr_to_int(cd, i64_t, "p14o.cd.w")
            .unwrap();
        self.store_enum_word(option_ty, field_ptr, 1, cd_w.into());
        self.store_enum_word(option_ty, field_ptr, 2, cl);
        self.store_enum_word(option_ty, field_ptr, 3, cc);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
    }

    /// B-2026-07-21-14 — the `Result` sibling of
    /// [`Self::deep_copy_option_inline_payload_in_place`]: deep-copy a
    /// `Result[T, E]` slot's LIVE inline-heap half in place. The `Ok` and
    /// `Err` payloads OVERLAY the same words (`{ptr,len,cap}` at fields
    /// 1..3, matching `FreeInlineResultPayload`'s overlay free), so this is
    /// two tag-guarded copies of the same word dance — each emitted only
    /// when that half's type arg is a DIRECT inline-heap `String`/`Vec`
    /// (`VecDeque` shares the `Vec` overlay). A scalar half copies nothing;
    /// the caller gates out every other half shape (shared, struct wrapper,
    /// nested seeded enum), keeping copy-depth == the registered cleanup's
    /// free-depth.
    pub(super) fn deep_copy_result_inline_heap_halves_in_place(
        &mut self,
        slot: PointerValue<'ctx>,
        result_te: &TypeExpr,
    ) {
        let TypeKind::Path(p) = &result_te.kind else {
            return;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Result") {
            return;
        }
        let Some(args) = p.generic_args.as_ref() else {
            return;
        };
        let ok_te = match args.first() {
            Some(crate::ast::GenericArg::Type(t)) => t.clone(),
            _ => return,
        };
        let err_te = match args.get(1) {
            Some(crate::ast::GenericArg::Type(t)) => t.clone(),
            _ => return,
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Result").cloned() else {
            return;
        };
        let result_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(0);
        let err_tag = layout.tags.get("Err").copied().unwrap_or(1);
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let tag_ptr = self
            .builder
            .build_struct_gep(result_ty, slot, 0, "p14r.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14r.tag")
            .unwrap()
            .into_int_value();
        for (half_tag, half_te, label) in [(ok_tag, ok_te, "ok"), (err_tag, err_te, "err")] {
            // Element type + element-deep TypeExpr, exactly as the Option
            // sibling resolves them; a non-String/Vec half emits no copy arm.
            let (elem_ty, deep_elem_te): (BasicTypeEnum<'ctx>, Option<TypeExpr>) =
                if self.is_string_type_expr(&half_te) {
                    (self.context.i8_type().into(), None)
                } else if let Some(et) = self.extract_vec_elem_type(&half_te) {
                    let inner = crate::codegen::helpers::vec_inner_type_expr(&half_te)
                        .filter(Self::elem_te_needs_direct_recursive_drain);
                    (et, inner)
                } else {
                    continue;
                };
            let copy_bb = self
                .context
                .append_basic_block(fn_val, &format!("p14r.{label}"));
            let next_bb = self
                .context
                .append_basic_block(fn_val, &format!("p14r.{label}.next"));
            let is_half = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    tag,
                    i64_t.const_int(half_tag, false),
                    &format!("p14r.is_{label}"),
                )
                .unwrap();
            self.builder
                .build_conditional_branch(is_half, copy_bb, next_bb)
                .unwrap();
            self.builder.position_at_end(copy_bb);
            self.emit_result_half_overlay_copy(result_ty, slot, elem_ty, deep_elem_te.as_ref());
            self.builder.build_unconditional_branch(next_bb).unwrap();
            self.builder.position_at_end(next_bb);
        }
    }

    /// The `{ptr,len,cap}` overlay copy for ONE already-selected `Result` half:
    /// rebuild a vec-struct value from payload words 1..3, hand it to
    /// `emit_vecstr_defensive_copy`, and store the duplicate back over the same
    /// three words. The caller owns the tag test and the surrounding blocks —
    /// the builder must already be positioned inside the taken-arm block.
    ///
    /// Extracted (B-2026-08-03-11) so the two entry-copy helpers that need it —
    /// [`Self::deep_copy_result_inline_heap_halves_in_place`] for the
    /// all-direct-halves class and
    /// [`Self::deep_copy_result_struct_enum_payload_in_place`] for the class
    /// that mixes a direct half with a struct/enum one — cannot drift apart.
    fn emit_result_half_overlay_copy(
        &mut self,
        result_ty: inkwell::types::StructType<'ctx>,
        slot: PointerValue<'ctx>,
        elem_ty: BasicTypeEnum<'ctx>,
        deep_elem_te: Option<&TypeExpr>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let vec_ty = self.vec_struct_type();
        let data_w = self.load_enum_word(result_ty, slot, 1, "p14r.data");
        let len_w = self.load_enum_word(result_ty, slot, 2, "p14r.len");
        let cap_w = self.load_enum_word(result_ty, slot, 3, "p14r.cap");
        let data_p = self
            .builder
            .build_int_to_ptr(data_w, ptr_ty, "p14r.data.p")
            .unwrap();
        let mut sv = vec_ty.get_undef();
        sv = self
            .builder
            .build_insert_value(sv, data_p, 0, "p14r.sv.d")
            .unwrap()
            .into_struct_value();
        sv = self
            .builder
            .build_insert_value(sv, len_w, 1, "p14r.sv.l")
            .unwrap()
            .into_struct_value();
        sv = self
            .builder
            .build_insert_value(sv, cap_w, 2, "p14r.sv.c")
            .unwrap()
            .into_struct_value();
        let copied = self
            .emit_vecstr_defensive_copy(sv.into(), elem_ty, deep_elem_te)
            .into_struct_value();
        let cd = self
            .builder
            .build_extract_value(copied, 0, "p14r.cd")
            .unwrap()
            .into_pointer_value();
        let cl = self
            .builder
            .build_extract_value(copied, 1, "p14r.cl")
            .unwrap();
        let cc = self
            .builder
            .build_extract_value(copied, 2, "p14r.cc")
            .unwrap();
        let cd_w = self
            .builder
            .build_ptr_to_int(cd, i64_t, "p14r.cd.w")
            .unwrap();
        self.store_enum_word(result_ty, slot, 1, cd_w.into());
        self.store_enum_word(result_ty, slot, 2, cl);
        self.store_enum_word(result_ty, slot, 3, cc);
    }

    /// B-2026-07-04-7 — deep-copy an `Option[<non-shared struct/enum>]` FIELD's
    /// `Some` payload in place, so a callee-owned by-value aggregate param owns
    /// heap independent of the caller's retained original. Unlike the
    /// `{ptr,len,cap}` String/Vec overlay (`deep_copy_option_inline_payload_in_place`)
    /// and the single-RC-pointer shared payload (`rc_inc_..._shared_...`), a
    /// struct/enum payload is either BOXED — when its LLVM word count exceeds the
    /// 3-word inline area, exactly the predicate `coerce_to_payload_words` boxes
    /// on — with word 1 holding the box pointer, or INLINE overlaying words 1..3.
    /// This is the copy peer of `emit_option_drop_fn`'s boxed/inline branch: on
    /// `Some`, if boxed, `malloc` a fresh box, shallow-copy the payload value in,
    /// then deep-copy its heap fields in place (`deep_copy_{struct,enum}_...`) and
    /// store the new box pointer; if inline, deep-copy the payload's heap fields
    /// in place over the Option's payload words. The deep-copy helpers duplicate
    /// exactly the buffers the payload's own `__karac_drop_*` frees (copy ==
    /// drop), so the callee copy and caller original own independent heap. `None`
    /// runs nothing.
    fn deep_copy_option_struct_enum_payload_in_place(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        opt_te: &TypeExpr,
    ) {
        let Some(payload_te) = Self::option_payload_te(opt_te) else {
            return;
        };
        let payload_name = match &payload_te.kind {
            TypeKind::Path(p) => p.segments.first().cloned(),
            _ => None,
        };
        let Some(payload_name) = payload_name else {
            return;
        };
        if self.type_decls.shared_types.contains_key(&payload_name) {
            return;
        }
        let is_struct = self.type_decls.struct_types.contains_key(&payload_name);
        let enum_layout = self
            .type_decls
            .enum_layouts
            .get(&payload_name)
            .filter(|l| !l.is_shared)
            .cloned();
        if !is_struct && enum_layout.is_none() {
            return;
        }

        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let Some(layout) = self.type_decls.enum_layouts.get("Option").cloned() else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);

        let tag_ptr = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 0, "p14oe.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14oe.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "p14oe.some",
            )
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "p14oe.some");
        let merge_bb = self.context.append_basic_block(fn_val, "p14oe.merge");
        self.builder
            .build_conditional_branch(is_some, some_bb, merge_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let payload_llty = self.llvm_type_for_type_expr(&payload_te);
        let payload_words = Self::llvm_type_word_count(payload_llty);
        // Payload area starts at Option field index 1.
        let payload_base = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 1, "p14oe.pl")
            .unwrap();
        if payload_words > 3 {
            // BOXED — word 1 holds the box pointer. Allocate a fresh box, copy
            // the payload value in, deep-copy its heap in place, store the new
            // pointer. Null-guarded (a Some tag with a null box can't occur, but
            // mirror `emit_option_drop_fn`'s box null-guard for symmetry).
            let old_w = self
                .builder
                .build_load(i64_t, payload_base, "p14oe.box.w0")
                .unwrap()
                .into_int_value();
            let old_box = self
                .builder
                .build_int_to_ptr(old_w, ptr_ty, "p14oe.oldbox")
                .unwrap();
            let old_null = self
                .builder
                .build_is_null(old_box, "p14oe.oldbox.null")
                .unwrap();
            let copy_bb = self.context.append_basic_block(fn_val, "p14oe.box.copy");
            self.builder
                .build_conditional_branch(old_null, merge_bb, copy_bb)
                .unwrap();
            self.builder.position_at_end(copy_bb);
            let raw_size = payload_llty.size_of().unwrap();
            let size = if raw_size.get_type().get_bit_width() == 64 {
                raw_size
            } else {
                self.builder
                    .build_int_z_extend(raw_size, i64_t, "p14oe.sz64")
                    .unwrap()
            };
            let new_box = self
                .builder
                .build_call(self.runtime_fns.malloc_fn, &[size.into()], "p14oe.newbox")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();
            let boxval = self
                .builder
                .build_load(payload_llty, old_box, "p14oe.boxval")
                .unwrap();
            self.builder.build_store(new_box, boxval).unwrap();
            if let Some(el) = &enum_layout {
                // B-2026-08-07-12 root B — an `Option`/`Result` payload's layout
                // is the ERASED GENERIC one, and the helper below drives entirely
                // off `field_drop_kinds`. For `Option[String]` that reports the
                // `Some` field as heap-free, so `has_heap` is false, the variant's
                // case block is never emitted, and NOTHING is copied: the fresh box
                // above ends up holding a bitwise duplicate whose `String` pointer
                // still aims at the caller's buffer. Two boxes, one buffer, and
                // both frames' drops free it — a double free at BOTH opt levels,
                // reachable with a callee whose body never touches the field.
                //
                // Route the CONCRETE payload `TypeExpr` back through the
                // inline-payload copier instead, which resolves `String` / `Vec` /
                // `shared` / struct / enum from the type rather than the layout.
                // The two functions are already designed as a pair — this is the
                // return leg of the hand-off documented at
                // `deep_copy_option_inline_payload_in_place`'s struct/enum arm —
                // and the recursion terminates because each hop strips one
                // `Option`/`Result` level until the payload is a buffer, an
                // aggregate, or a scalar that copies nothing.
                match payload_name.as_str() {
                    "Option" => self.deep_copy_option_inline_payload_in_place(new_box, &payload_te),
                    "Result" => {
                        // Same disjoint-class split the struct-FIELD arm makes.
                        if self.result_field_struct_enum_payload_ok(&payload_te) {
                            self.deep_copy_result_struct_enum_payload_in_place(new_box, &payload_te)
                        } else {
                            self.deep_copy_result_inline_heap_halves_in_place(new_box, &payload_te)
                        }
                    }
                    _ => self.deep_copy_enum_heap_payload_in_place(&payload_name, new_box, el),
                }
            } else {
                self.deep_copy_struct_heap_fields_in_place(new_box, &payload_name);
            }
            let new_w = self
                .builder
                .build_ptr_to_int(new_box, i64_t, "p14oe.newbox.w")
                .unwrap();
            self.builder.build_store(payload_base, new_w).unwrap();
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        } else {
            // INLINE — the payload overlays words 1..3 in place; deep-copy its
            // heap fields directly (`payload_base` reinterprets as `payload_llty*`).
            if let Some(el) = &enum_layout {
                self.deep_copy_enum_heap_payload_in_place(&payload_name, payload_base, el);
            } else {
                self.deep_copy_struct_heap_fields_in_place(payload_base, &payload_name);
            }
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);
    }

    /// B-2026-08-03-3 leg B — the `Result` twin of
    /// [`Self::deep_copy_option_struct_enum_payload_in_place`]: deep-copy a
    /// `Result[T, E]` FIELD's live half when that half is a non-shared user
    /// struct/enum, so a callee-owned by-value aggregate param owns heap
    /// independent of the caller's retained original. This is the copy peer of
    /// [`Self::emit_result_drop_fn`]'s per-side boxed/inline branch, and it must
    /// agree with that emitter on the BOXING THRESHOLD — which for `Result` is
    /// **5 words**, not the Option family's 3. The `Result` payload area is
    /// declared 5 words wide (`declarations.rs`'s `result_payload_words`) and
    /// both `coerce_to_payload_words` (the pack site) and `emit_result_drop_fn`
    /// (the free site) box only beyond it, so a 4-word payload — the canonical
    /// `struct Res { id: i64, name: String }` — lives INLINE in a `Result` while
    /// the same struct is BOXED in an `Option`. Copying it with the Option
    /// helper's `> 3` test reads `id` as a box pointer; keep the `> 5` here in
    /// lockstep with the emitter or every shape in this class corrupts.
    fn deep_copy_result_struct_enum_payload_in_place(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        res_te: &TypeExpr,
    ) {
        let TypeKind::Path(p) = &res_te.kind else {
            return;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Result") {
            return;
        }
        let Some(args) = p.generic_args.as_ref() else {
            return;
        };
        let half_te = |i: usize| match args.get(i) {
            Some(GenericArg::Type(t)) => Some(t.clone()),
            _ => None,
        };
        let (Some(ok_te), Some(err_te)) = (half_te(0), half_te(1)) else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get("Result").cloned() else {
            return;
        };
        let result_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(0);
        let err_tag = layout.tags.get("Err").copied().unwrap_or(1);

        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let tag_ptr = self
            .builder
            .build_struct_gep(result_ty, field_ptr, 0, "p14re.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14re.tag")
            .unwrap()
            .into_int_value();

        for (half_tag, half, label) in [(ok_tag, ok_te, "ok"), (err_tag, err_te, "err")] {
            // Per-half plan. A heapless half copies nothing. A heap half is
            // either a non-shared struct/enum (the boxed/inline dance below) or
            // — B-2026-08-03-11, the mixed class — a direct `String`/`Vec`,
            // which reuses the same `{ptr,len,cap}` overlay copy the
            // all-direct-halves helper emits. The caller's gate already rejected
            // every other heap-owning shape, so nothing is silently skipped.
            if !self.te_owns_heap_below_buffer(&half) {
                continue;
            }
            let payload_name = match &half.kind {
                TypeKind::Path(hp) => hp.segments.first().cloned(),
                _ => None,
            };
            let Some(payload_name) = payload_name else {
                continue;
            };
            if self.type_decls.shared_types.contains_key(&payload_name) {
                continue;
            }
            let is_struct = self.type_decls.struct_types.contains_key(&payload_name);
            let enum_layout = self
                .type_decls
                .enum_layouts
                .get(&payload_name)
                .filter(|l| !l.is_shared)
                .cloned();
            // B-2026-09-03-6 — see the copy arm below. `Option`/`Result` are in
            // `enum_layouts` under their bare names, so a nested one reaches the
            // enum path with a layout that has no instantiation of its payload.
            //
            // Gated on `optres_param_entry_copied_te` for the HALF, not just on
            // its shape. That predicate is written as "payload the by-value
            // entry copy can duplicate", and its exclusions are exactly the
            // payloads that already have an owner: a SHARED handle belongs to
            // the rc machinery, and a payload over the boxing-word limit belongs
            // to `boxed_enum_payload_vars` / `boxed_struct_payload_vars`, each
            // with caller-side retraction rules this copy would be a second,
            // unsynchronised answer to. Measured: without this clause the
            // recursion mallocs a fresh box for such a half and three
            // memory_sanitizer fixtures in the boxed-nested family fail
            // (`asan_box_nested_in_result_inline_payload_area_no_leak`,
            // `asan_nested_box_chain_frees_every_envelope`,
            // `asan_nested_box_owned_by_callee_when_no_binding_owns_it`). A half
            // the predicate refuses keeps the by-name path below, which is the
            // behaviour those fixtures were passing with.
            let nested_optres = matches!(payload_name.as_str(), "Option" | "Result")
                && matches!(&half.kind, TypeKind::Path(hp) if hp.generic_args.is_some())
                && self.optres_param_entry_copied_te(&half);
            // Element type for a direct String/Vec half; `None` marks the
            // struct/enum half that takes the boxed-or-inline path.
            let overlay: Option<(BasicTypeEnum<'ctx>, Option<TypeExpr>)> =
                if is_struct || enum_layout.is_some() {
                    None
                } else if self.is_string_type_expr(&half) {
                    Some((self.context.i8_type().into(), None))
                } else if let Some(et) = self.extract_vec_elem_type(&half) {
                    let inner = crate::codegen::helpers::vec_inner_type_expr(&half)
                        .filter(Self::elem_te_needs_direct_recursive_drain);
                    Some((et, inner))
                } else {
                    continue;
                };

            let copy_bb = self
                .context
                .append_basic_block(fn_val, &format!("p14re.{label}"));
            let next_bb = self
                .context
                .append_basic_block(fn_val, &format!("p14re.{label}.next"));
            let is_half = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    tag,
                    i64_t.const_int(half_tag, false),
                    &format!("p14re.is_{label}"),
                )
                .unwrap();
            self.builder
                .build_conditional_branch(is_half, copy_bb, next_bb)
                .unwrap();
            self.builder.position_at_end(copy_bb);

            if let Some((elem_ty, deep_elem_te)) = overlay {
                // Direct String/Vec half of a MIXED Result (B-2026-08-03-11) —
                // the payload's `{ptr,len,cap}` overlays words 1..3 and its free
                // is `emit_result_drop_fn`'s overlay arm, so the copy is the
                // same word dance the all-direct class uses.
                self.emit_result_half_overlay_copy(
                    result_ty,
                    field_ptr,
                    elem_ty,
                    deep_elem_te.as_ref(),
                );
                self.builder.build_unconditional_branch(next_bb).unwrap();
                self.builder.position_at_end(next_bb);
                continue;
            }

            let payload_llty = self.llvm_type_for_type_expr(&half);
            let payload_words = Self::llvm_type_word_count(payload_llty);
            let payload_base = self
                .builder
                .build_struct_gep(result_ty, field_ptr, 1, &format!("p14re.{label}.pl"))
                .unwrap();
            if payload_words > 5 {
                // BOXED — word 1 holds the box pointer. Same dance as the
                // Option twin's boxed branch, null-guarded for symmetry with
                // `emit_result_drop_fn`'s box null-guard.
                let old_w = self
                    .builder
                    .build_load(i64_t, payload_base, &format!("p14re.{label}.box.w0"))
                    .unwrap()
                    .into_int_value();
                let old_box = self
                    .builder
                    .build_int_to_ptr(old_w, ptr_ty, &format!("p14re.{label}.oldbox"))
                    .unwrap();
                let old_null = self
                    .builder
                    .build_is_null(old_box, &format!("p14re.{label}.oldbox.null"))
                    .unwrap();
                let box_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("p14re.{label}.box.copy"));
                self.builder
                    .build_conditional_branch(old_null, next_bb, box_bb)
                    .unwrap();
                self.builder.position_at_end(box_bb);
                let raw_size = payload_llty.size_of().unwrap();
                let size = if raw_size.get_type().get_bit_width() == 64 {
                    raw_size
                } else {
                    self.builder
                        .build_int_z_extend(raw_size, i64_t, &format!("p14re.{label}.sz64"))
                        .unwrap()
                };
                let new_box = self
                    .builder
                    .build_call(
                        self.runtime_fns.malloc_fn,
                        &[size.into()],
                        &format!("p14re.{label}.newbox"),
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let boxval = self
                    .builder
                    .build_load(payload_llty, old_box, &format!("p14re.{label}.boxval"))
                    .unwrap();
                self.builder.build_store(new_box, boxval).unwrap();
                if let Some(el) = &enum_layout {
                    self.deep_copy_enum_heap_payload_in_place(&payload_name, new_box, el);
                } else {
                    self.deep_copy_struct_heap_fields_in_place(new_box, &payload_name);
                }
                let new_w = self
                    .builder
                    .build_ptr_to_int(new_box, i64_t, &format!("p14re.{label}.newbox.w"))
                    .unwrap();
                self.builder.build_store(payload_base, new_w).unwrap();
            } else if nested_optres {
                // B-2026-09-03-6 — a half that is ITSELF an `Option`/`Result`
                // must be copied through the TYPE-EXPR-driven copier, not the
                // by-NAME enum one below.
                //
                // `Option` and `Result` are GENERIC, and
                // `deep_copy_enum_heap_payload_in_place` keys everything on the
                // enum's NAME: it reads `layout.field_drop_kinds["Some"]` for the
                // erased prelude declaration, whose payload is the type parameter
                // `T`. `T` is not heap-bearing, so the `Some` case was not even
                // emitted and the half was copied by NOTHING -- while
                // `emit_result_drop_fn` recurses with the full instantiated
                // TypeExpr (`emit_drop_fn_for_type_expr(Option[String])`) and
                // frees the payload for real. Copy shallower than drop is exactly
                // the invariant this family's doc comments say must hold, and
                // breaking it here meant the callee's entry-copied param and the
                // caller's retained original aliased one buffer:
                // `fn show(x: Result[Option[String], E])` aborted `karac run` with
                // `free(): double free detected in tcache 2` on a single call, and
                // the AOT binary logged an `Invalid free()` per call under
                // valgrind while still printing correctly. With a
                // `Result[Option[Vec[i64]], E]` payload the AOT binary aborts too.
                //
                // `payload_base` is the Result's word 1 -- the very pointer
                // `emit_result_drop_fn` hands the half's drop fn -- so the nested
                // enum's own tag lands where its layout expects it.
                self.deep_copy_optres_param_in_place(payload_base, &half);
            } else {
                // INLINE — the payload overlays words 1..5; deep-copy its heap
                // fields directly (`payload_base` reinterprets as `payload_llty*`).
                if let Some(el) = &enum_layout {
                    self.deep_copy_enum_heap_payload_in_place(&payload_name, payload_base, el);
                } else {
                    self.deep_copy_struct_heap_fields_in_place(payload_base, &payload_name);
                }
            }
            self.builder.build_unconditional_branch(next_bb).unwrap();
            self.builder.position_at_end(next_bb);
        }
    }

    /// B-2026-07-03-28 shared leg — rc-INC an `Option[shared]` FIELD's inline
    /// box pointer (word 1, ptrtoint) when Some, so a callee-owned by-value
    /// aggregate param holds an independent ref to the shared box. The exact
    /// inc peer of `emit_nested_struct_shared_rc_decs_ex`'s `Option[shared]`
    /// rc-dec arm (synth_drop.rs): read the Option tag, and on Some load word 1
    /// as i64, `int_to_ptr`, null-guard, and `emit_refcount_inc_by_type` on the
    /// recovered box. A `None` payload runs nothing. Symmetric copy == drop, so
    /// the callee copy and the caller's retained original both own a ref that
    /// each drop path (Vec-element / destructure-leaf) rc-decs exactly once.
    fn rc_inc_option_inline_shared_payload_in_place(
        &mut self,
        field_ptr: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let Some(layout) = self.type_decls.enum_layouts.get("Option").cloned() else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);

        let tag_ptr = self
            .builder
            .build_struct_gep(option_ty, field_ptr, 0, "p14os.tag.p")
            .unwrap();
        let tag = self
            .builder
            .build_load(i64_t, tag_ptr, "p14os.tag")
            .unwrap()
            .into_int_value();
        let is_some = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                i64_t.const_int(some_tag, false),
                "p14os.some",
            )
            .unwrap();
        let some_bb = self.context.append_basic_block(fn_val, "p14os.some");
        let merge_bb = self.context.append_basic_block(fn_val, "p14os.merge");
        self.builder
            .build_conditional_branch(is_some, some_bb, merge_bb)
            .unwrap();

        self.builder.position_at_end(some_bb);
        let w1 = self.load_enum_word(option_ty, field_ptr, 1, "p14os.w1");
        let inner = self
            .builder
            .build_int_to_ptr(w1, ptr_ty, "p14os.inner")
            .unwrap();
        let inner_null = self
            .builder
            .build_is_null(inner, "p14os.inner.isnull")
            .unwrap();
        let inc_bb = self.context.append_basic_block(fn_val, "p14os.inc.do");
        let skip_bb = self.context.append_basic_block(fn_val, "p14os.inc.skip");
        self.builder
            .build_conditional_branch(inner_null, skip_bb, inc_bb)
            .unwrap();
        self.builder.position_at_end(inc_bb);
        self.emit_refcount_inc_by_type(heap_type, inner);
        self.builder.build_unconditional_branch(skip_bb).unwrap();
        self.builder.position_at_end(skip_bb);
        self.builder.build_unconditional_branch(merge_bb).unwrap();

        self.builder.position_at_end(merge_bb);
    }

    /// The payload `TypeExpr` of an `Option[T]` type expr, else `None`.
    pub(super) fn option_payload_te(opt_te: &TypeExpr) -> Option<TypeExpr> {
        let TypeKind::Path(p) = &opt_te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return None;
        }
        match p.generic_args.as_ref()?.first()? {
            crate::ast::GenericArg::Type(t) => Some(t.clone()),
            _ => None,
        }
    }

    /// B-2026-07-08-9: split a `Result[T, E]` `TypeExpr` into its concrete
    /// `(ok, err)` payload `TypeExpr`s. Sibling of `option_payload_te` for the
    /// Display path. Returns `None` for a non-`Result` type or a `Result`
    /// missing either generic arg.
    pub(super) fn result_payload_tes(res_te: &TypeExpr) -> Option<(TypeExpr, TypeExpr)> {
        let TypeKind::Path(p) = &res_te.kind else {
            return None;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Result") {
            return None;
        }
        let args = p.generic_args.as_ref()?;
        let ok = match args.first()? {
            crate::ast::GenericArg::Type(t) => t.clone(),
            _ => return None,
        };
        let err = match args.get(1)? {
            crate::ast::GenericArg::Type(t) => t.clone(),
            _ => return None,
        };
        Some((ok, err))
    }

    /// #14 — at a struct-literal field init `S { f: obj.field }` whose value is
    /// a heap FIELD moved out of a tracked struct binding `obj` (a callee-owned
    /// by-value aggregate param, or a local), cap-zero that field's buffer in
    /// `obj`'s slot so `obj`'s `StructDrop` skips it — the new struct literal is
    /// now the sole owner. This is the field-access peer of the whole-Identifier
    /// `suppress_source_vec_cleanup_for_arg` (which the literal path already
    /// calls), and the analog of its TupleIndex arm.
    ///
    /// SCOPED to struct-literal field inits, where the value is genuinely MOVED
    /// into the new owner — NOT folded into the general suppression funnel,
    /// which also fires at by-value-arg sites where the callee may not take
    /// ownership (cap-zeroing there would leak). Without this, a builder method
    /// that moves `self`'s fields into a returned literal
    /// (`LogEvent { level: self.level, message: self.message, … }`) double-frees
    /// once `self` is a callee-owned by-value aggregate param — the source field
    /// AND the returned literal both free the same buffer (std.tracing's
    /// `with_field`).
    /// B-2026-07-15-24 — the concrete generic instantiation of a struct field
    /// moved out via `let bound = o.field`, derived from `o`'s recorded
    /// instantiation and the field's declared type. `o: GOuter[Vec[i64]]`,
    /// `field inner: GInner[T]` → `GInner[Vec[i64]]`. Feeds (a) the moved-out
    /// binding's own mono drop (so it GEPs the per-monomorph layout, not the
    /// base erased one) and (b) the source-field mono cap-zeroing in
    /// `suppress_struct_field_move_into_literal`. `None` unless `o` has a
    /// recorded generic instantiation and the field resolves to a generic named
    /// struct — a non-generic field (or an unrecorded object) keeps the
    /// name-shared base-layout behavior, unchanged.
    pub(super) fn field_move_out_struct_inst(&self, value: &Expr) -> Option<TypeExpr> {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return None;
        };
        let obj_name = match &object.kind {
            ExprKind::Identifier(o) => o.as_str(),
            ExprKind::SelfValue => "self",
            _ => return None,
        };
        self.field_move_out_struct_inst_by_name(obj_name, field)
    }

    /// [`Self::field_move_out_struct_inst`] addressed by NAME, for the
    /// destructure spelling of the same move (B-2026-08-28-10), which has no
    /// `FieldAccess` node to resolve the object and field from.
    pub(super) fn field_move_out_struct_inst_by_name(
        &self,
        obj_name: &str,
        field: &str,
    ) -> Option<TypeExpr> {
        let obj_inst = self.type_decls.enum_inst_var_types.get(obj_name)?;
        let TypeKind::Path(op) = &obj_inst.kind else {
            return None;
        };
        let obj_struct = op.segments.last()?.clone();
        let obj_args = op.generic_args.as_ref()?;
        let params = self.type_decls.struct_generic_params.get(&obj_struct)?;
        if params.len() != obj_args.len() {
            return None;
        }
        let mut subst: std::collections::HashMap<String, TypeExpr> =
            std::collections::HashMap::new();
        for (p, a) in params.iter().zip(obj_args.iter()) {
            if let crate::ast::GenericArg::Type(te) = a {
                subst.insert(p.clone(), te.clone());
            }
        }
        if subst.is_empty() {
            return None;
        }
        let field_idx = self
            .type_decls
            .struct_field_names
            .get(&obj_struct)?
            .iter()
            .position(|n| n == field)?;
        let fte = self
            .type_decls
            .struct_field_type_exprs
            .get(&obj_struct)?
            .get(field_idx)?;
        let resolved = crate::codegen::helpers::subst_type_params_in_type_expr(fte, &subst);
        self.is_generic_named_struct_type_expr(&resolved)
            .then_some(resolved)
    }

    pub(super) fn suppress_struct_field_move_into_literal(&self, value: &Expr) {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        // B-2026-08-13-14 — the disarm half of the `UseAfterMove` defensive
        // copy, for the field-bind site. `uam_defensive_copy_field` handed the
        // destination an independent buffer, so the SOURCE still owns (and must
        // still free) its own; cap-zeroing it here would orphan that buffer.
        // Keyed on `uam_copied_sites` — "a copy really happened" — for the same
        // reason `suppress_source_vec_cleanup_for_arg_ex` is: skipping the
        // disarm where no copy was made leaves two owners of one buffer.
        if self
            .span_tables
            .uam_copied_sites
            .contains(&(value.span.offset, value.span.length))
        {
            return;
        }
        // The source root is either a named binding (`obj.field`) or the method
        // receiver (`self.field`) — `self` is bound as an ordinary local named
        // "self" by `compile_function`. The std.tracing builder bodies move
        // `self.fields` / `self.message` out, so SelfValue must resolve here or
        // the move-out suppression never fires (#17 gap 2).
        let s: &str = match &object.kind {
            ExprKind::Identifier(s) => s.as_str(),
            ExprKind::SelfValue => "self",
            _ => return,
        };
        self.suppress_struct_field_move_by_name(s, field);
    }

    /// [`Self::suppress_struct_field_move_into_literal`] addressed by NAME
    /// rather than by a `FieldAccess` expression (B-2026-08-28-10).
    ///
    /// A struct-pattern destructure moves the same field the field-access
    /// spelling does — `let W { r, n } = w` and `let x = w.r` transfer `r`
    /// identically — but has no `FieldAccess` node to hand over, and the
    /// `zero_struct_field_move_cap` it used instead reaches only DIRECT
    /// Vec/String/Map/Option fields. A nested STRUCT field's own heap survived
    /// it, so the source freed a buffer the leaf had just taken: a double free
    /// where the field-access spelling was clean.
    ///
    /// The `uam_copied_sites` check stays with the expression form above, which
    /// is where a span exists to consult. A destructure leaf has no defensive
    /// copy of its own, so the default — suppress — is the right one here.
    pub(super) fn suppress_struct_field_move_by_name(&self, s: &str, field: &str) {
        let Some(slot) = self.variables.get(s).copied() else {
            return;
        };
        let BasicTypeEnum::StructType(agg_ty) = slot.ty else {
            return;
        };
        let vec_ty = self.vec_struct_type();
        if agg_ty == vec_ty {
            return;
        }
        let Some(sname) = self.var_types.var_type_names.get(s).cloned() else {
            return;
        };
        let Some(idx) = self
            .type_decls
            .struct_field_names
            .get(sname.as_str())
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        let field_llvm = agg_ty.get_field_type_at_index(idx as u32);
        let Ok(field_ptr) =
            self.builder
                .build_struct_gep(agg_ty, slot.ptr, idx as u32, "p14.fldmv.p")
        else {
            return;
        };
        // The field's declared type name (`inner: Inner` → "Inner"), used to
        // route named-struct / enum fields to the type-name-driven suppressors
        // that the LLVM-type-only match below can't classify (a Map/Set handle
        // and an enum's payload are both bare-word/ptr layouts indistinguishable
        // from other fields by LLVM type alone).
        let field_type_name: Option<String> = self
            .type_decls
            .struct_field_type_exprs
            .get(sname.as_str())
            .and_then(|ftes| ftes.get(idx))
            .and_then(|fte| match &fte.kind {
                TypeKind::Path(p) => p.segments.first().cloned(),
                _ => None,
            });
        match field_llvm {
            // Direct Vec/String field → zero its cap (drop's `cap > 0` skips).
            Some(BasicTypeEnum::StructType(fst)) if fst == vec_ty => {
                if let Ok(cap_ptr) =
                    self.builder
                        .build_struct_gep(vec_ty, field_ptr, 2, "p14.fldmv.cap")
                {
                    let _ = self
                        .builder
                        .build_store(cap_ptr, self.context.i64_type().const_int(0, false));
                }
            }
            // A nested aggregate field: a named non-shared STRUCT, an ENUM, or a
            // tuple whose drop frees heap leaves the moved-out binding now owns.
            Some(BasicTypeEnum::StructType(fst)) => {
                // Named non-shared struct field (`inner: Inner`) — route through
                // the type-name-driven `zero_struct_move_caps`, which uniformly
                // disarms Vec/String (cap+len), Map/Set (null the handle —
                // B-2026-07-15-23), enum payloads, and nested structs. The
                // LLVM-type-driven `aggregate_has_heap_field` /
                // `zero_aggregate_field_caps` path below sees NEITHER a Map/Set
                // handle (a bare `ptr`, not the vec-struct) NOR an enum leaf (all
                // -i64 words), so a moved-out struct carrying only a Map/Set or an
                // enum field would otherwise leave the SOURCE live and double-free
                // it against `bound`'s drop (df9/dfB/dfC Map-SIGSEGV, dfD enum
                // double-free — all `karac check`-clean). `zero_struct_move_caps`
                // uses the base struct layout; for a generic monomorph whose base
                // erases a bare-`T` heap field the Vec/String path below (mono
                // LLVM type) is the precise one — the two overlap idempotently on
                // Vec, so run both.
                if let Some(name) = field_type_name.as_deref() {
                    if self.type_decls.struct_types.contains_key(name)
                        && !self.type_decls.shared_types.contains_key(name)
                    {
                        // B-2026-07-15-24 — derive the moved-out field's concrete
                        // mono instantiation (`GInner[Vec[i64]]`) so the source
                        // cap-zeroing GEPs the PER-MONOMORPH layout. Without it a
                        // bare-`T` heap field placed before a Map/Vec field in the
                        // nested struct mis-offsets the handle null-store (base
                        // erased layout), leaving the source live → double-free /
                        // SIGSEGV on the nested move-out. A non-generic field
                        // yields `None` → the name-shared base-layout suppression,
                        // unchanged.
                        let nsub = self
                            .field_move_out_struct_inst_by_name(s, field)
                            .map(|inst| self.generic_struct_subst_from_inst(name, &inst))
                            .filter(|s| !s.is_empty());
                        self.zero_struct_move_caps_mono(field_ptr, name, nsub.as_ref());
                    } else if name == "Option" || name == "Result" {
                        // B-2026-08-03-8 — an `Option`/`Result` FIELD moved out
                        // (`let x = h.o`). The enum arm below is a NO-OP for
                        // these two by construction (they carry no static
                        // `EnumDropKind`), so the source's `OptionInline` field
                        // drop stayed armed and freed the payload the moved-out
                        // binding now owns: a use-after-free then a double free,
                        // SEGV, no output. These are the same two neutralizers
                        // `zero_struct_move_caps_mono` applies on a WHOLE-struct
                        // move — the single-field move-out just never got them.
                        if name == "Option" {
                            self.zero_option_field_tag_at(field_ptr);
                        } else if let Some(layout) = self.type_decls.enum_layouts.get("Result") {
                            let result_ty = layout.llvm_type;
                            self.zero_result_payload_area(result_ty, field_ptr, "p14.fldmv.res");
                        }
                    } else if let Some(layout) = self.type_decls.enum_layouts.get(name) {
                        // Enum field (#19) — cap-zero its `VecOrString` payload
                        // words so the owning struct's drop skips the buffer the
                        // moved-out binding now owns (`let tk = t.token`). Shared
                        // enums carry RC (no `VecOrString` kind) and self-skip;
                        // Option/Result have no static kind and `zero_enum_payload_caps`
                        // no-ops for them.
                        if !layout.is_shared {
                            let layout = layout.clone();
                            self.zero_enum_payload_caps(field_ptr, &layout);
                        }
                    }
                }
                // Mono-correct Vec/String cap-zero for a nested aggregate whose
                // heap is a directly-visible (possibly bare-`T`-monomorphized)
                // Vec/String field.
                if self.aggregate_has_heap_field(fst) {
                    self.zero_aggregate_field_caps(field_ptr, fst);
                }
            }
            _ => {}
        }
    }

    /// #27 (B-2026-06-14-8) — `let tk = h.ps.0.tok`: an enum field moved OUT of
    /// a struct that is itself nested in a TUPLE element. The source `value` is a
    /// `FieldAccess` whose OBJECT is a deeper place (`h.ps.0`, a `TupleIndex`),
    /// which [`Self::suppress_struct_field_move_into_literal`] (Identifier/`self`
    /// object only) can't reach. Resolve the field via the place-chain machinery
    /// ([`Self::field_chain_place_ptr`] / [`Self::place_chain_type_name`]) and
    /// cap-zero the enum payload in the owning struct's slot, so its drop skips
    /// the buffer the moved-out `tk` now owns (else double-free). Self-gates to a
    /// non-Identifier/`self` object (the shallow forms keep their dedicated
    /// suppressor), a non-owned-param root, and a non-shared user enum field.
    pub(super) fn suppress_place_field_enum_move_source(&mut self, value: &Expr) {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        // Shallow forms (`obj.field` / `self.field`) are handled by
        // `suppress_struct_field_move_into_literal`; only a DEEPER place here.
        if matches!(object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return;
        }
        match Self::place_root_ident(value) {
            Some(root) if self.borrow_vars.owned_struct_params.contains(root) => return,
            Some(_) => {}
            None => return,
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return;
        };
        let Some(idx) = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        // The moved-out field must be a non-shared user enum (the only case that
        // double-frees through the owning struct's drop; Vec/String/struct fields
        // through a tuple element are a separate follow-on, not yet observed).
        let Some(ename) = self
            .type_decls
            .struct_field_type_names
            .get(obj_ty.as_str())
            .and_then(|tns| tns.get(idx))
            .and_then(|n| n.clone())
        else {
            return;
        };
        let Some(layout) = self.type_decls.enum_layouts.get(ename.as_str()).cloned() else {
            return;
        };
        if layout.is_shared {
            return;
        }
        let Some(st) = self.type_decls.struct_types.get(obj_ty.as_str()).copied() else {
            return;
        };
        let Some(base_ptr) = self.field_chain_place_ptr(object) else {
            return;
        };
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(st, base_ptr, idx as u32, "p27.encap.p")
        else {
            return;
        };
        self.zero_enum_payload_caps(field_ptr, &layout);
    }

    /// B-2026-08-01-31 — `let x = o.h.r` / `let s = o.h.name`: a STRUCT or
    /// Vec/String field moved OUT of a struct reached through a DEEPER place
    /// (a nested field chain), which
    /// [`Self::suppress_struct_field_move_into_literal`] (Identifier/`self`
    /// object only) can't reach. The moved-out binding registers its own
    /// cleanup and now owns the field's heap, but the ROOT's `StructDrop`
    /// walks the chain and freed it again — the exact "Vec/String/struct
    /// fields through a [deeper place] are a separate follow-on, not yet
    /// observed" recorded on the enum sibling above, now observed as a
    /// `karac check`-clean double-free. Resolve the parent via the same
    /// place-chain machinery and cap-zero the moved field in place. Same
    /// self-gates as the enum sibling: deeper-place object only (shallow
    /// forms keep their dedicated suppressor), non-owned-param root. Field
    /// dispatch mirrors `suppress_struct_field_move_into_literal`: a named
    /// non-shared struct routes through `zero_struct_move_caps_mono` — for a
    /// GENERIC monomorph field on a NON-generic parent (B-2026-08-01-34,
    /// `let g = o.h.b` with `b: Boxy[String]`) the declared field TypeExpr
    /// is already the concrete instantiation, so the per-monomorph subst
    /// comes straight from it (the depth-1 site's
    /// `field_move_out_struct_inst` equivalent) and the zeroing GEPs the
    /// mono layout; a generic field on a generic PARENT declines (its field
    /// TE can carry bare params — the B-2026-07-15-24 base-layout caution).
    /// A direct Vec/String field cap-zeroes. Shared / Option / Map / Set
    /// fields keep their existing paths.
    /// Does this place chain bottom out at a Vec INDEX (`ps[0].word`,
    /// `ps[0].inner.word`) rather than at a plain binding / `self`?
    /// B-2026-08-12-27 — such a chain reads out of a CONTAINER ELEMENT, whose
    /// heap the container still owns, so the move-out suppressions must decline
    /// it: the read is cloned instead.
    fn place_chain_is_index_rooted(expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Index { .. } => true,
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                Self::place_chain_is_index_rooted(object)
            }
            _ => false,
        }
    }

    pub(super) fn suppress_place_field_struct_move_source(&mut self, value: &Expr) {
        let ExprKind::FieldAccess { object, field } = &value.kind else {
            return;
        };
        if matches!(object.kind, ExprKind::Identifier(_) | ExprKind::SelfValue) {
            return;
        }
        // B-2026-08-12-27 — an INDEX-rooted chain (`ps[0].word`) is no longer a
        // move. The read is deep-cloned (`clone_vec_elem_heap_field_read`), so
        // the binding owns the clone and the container's element still owns its
        // own buffer; cap-zeroing the element here would orphan that buffer —
        // trading the old double free for a leak.
        //
        // This suppression was the reason the `let` shape looked clean while
        // the other seven owning destinations aborted: it made ONE position a
        // move, silently, against what `karac check` and the interpreter say.
        // Its cost was visible on mutation — `let mut w = ps[0].word;
        // w = w + "X";` then reading `ps[0].word` printed garbage. Struct-rooted
        // chains (`o.h.name`, B-2026-08-01-31) keep the move; they have no
        // container element behind them and nothing clones them.
        if Self::place_chain_is_index_rooted(value) {
            return;
        }
        match Self::place_root_ident(value) {
            Some(root) if self.borrow_vars.owned_struct_params.contains(root) => return,
            Some(_) => {}
            None => return,
        }
        let Some(obj_ty) = self.place_chain_type_name(object) else {
            return;
        };
        let Some(idx) = self
            .type_decls
            .struct_field_names
            .get(obj_ty.as_str())
            .and_then(|names| names.iter().position(|n| n == field))
        else {
            return;
        };
        let Some(fte) = self
            .type_decls
            .struct_field_type_exprs
            .get(obj_ty.as_str())
            .and_then(|tes| tes.get(idx))
            .cloned()
        else {
            return;
        };
        let parent_generic = self
            .type_decls
            .struct_generic_params
            .get(obj_ty.as_str())
            .is_some_and(|ps| !ps.is_empty());
        let mut mono_subst: Option<std::collections::HashMap<String, TypeExpr>> = None;
        let struct_field_name: Option<String> = match &fte.kind {
            TypeKind::Path(p) => p
                .segments
                .last()
                .filter(|n| {
                    if !self.type_decls.struct_types.contains_key(n.as_str())
                        || self.type_decls.shared_types.contains_key(n.as_str())
                    {
                        return false;
                    }
                    let field_generic = self
                        .type_decls
                        .struct_generic_params
                        .get(n.as_str())
                        .is_some_and(|ps| !ps.is_empty());
                    if !field_generic {
                        return true;
                    }
                    // B-2026-08-01-34: a generic-monomorph field on a
                    // NON-generic parent — the declared TE is the concrete
                    // instantiation; derive the mono subst from it so the
                    // zeroing GEPs the per-monomorph layout. Generic
                    // parents decline (bare-param field TEs).
                    if parent_generic {
                        return false;
                    }
                    let subst = self.generic_struct_subst_from_inst(n.as_str(), &fte);
                    if subst.is_empty() {
                        return false;
                    }
                    mono_subst = Some(subst);
                    true
                })
                .cloned(),
            _ => None,
        };
        let vecstr_field =
            self.is_string_type_expr(&fte) || self.extract_vec_elem_type(&fte).is_some();
        if struct_field_name.is_none() && !vecstr_field {
            return;
        }
        let Some(st) = self.type_decls.struct_types.get(obj_ty.as_str()).copied() else {
            return;
        };
        let Some(base_ptr) = self.field_chain_place_ptr(object) else {
            return;
        };
        let Ok(field_ptr) = self
            .builder
            .build_struct_gep(st, base_ptr, idx as u32, "p31.stmv.p")
        else {
            return;
        };
        if let Some(name) = struct_field_name {
            self.zero_struct_move_caps_mono(field_ptr, &name, mono_subst.as_ref());
        } else if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(self.vec_struct_type(), field_ptr, 2, "p31.stmv.cap")
        {
            let _ = self
                .builder
                .build_store(cap_ptr, self.context.i64_type().const_int(0, false));
        }
    }

    fn load_enum_word(
        &self,
        enum_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        name: &str,
    ) -> inkwell::values::IntValue<'ctx> {
        let i64_t = self.context.i64_type();
        let p = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, idx, name)
            .unwrap();
        self.builder
            .build_load(i64_t, p, name)
            .unwrap()
            .into_int_value()
    }

    /// Move-out disarm for a constant-index element read out of an owned
    /// `Array[T, N]` root (B-2026-08-22-18 follow-up): when `a[k]` /`self[k]`
    /// (const `k`) is moved out as a `return` / `let` value and `a` carries the
    /// transfer-owned array element drop (`owned_array_params`), cap-zero element
    /// `k` in the array's source slot so the array's scope-exit
    /// `synthesize_array_drop_fn_te` drop skips it — the returned element now has
    /// the single owner (the destination binding). Without this the returned
    /// buffer is freed twice (the array drop AND the destination). The array
    /// analog of [`Self::suppress_place_field_struct_move_source`]; constant
    /// index only (a dynamic-index move-out is a separate, pre-existing gap).
    pub(super) fn suppress_array_elem_move_source(&mut self, value: &Expr) {
        let ExprKind::Index { object, index } = &value.kind else {
            return;
        };
        let ExprKind::Integer(k, _) = &index.kind else {
            return;
        };
        if *k < 0 {
            return;
        }
        let root = match &object.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return,
        };
        let Some((elem_te, n)) = self.borrow_vars.owned_array_params.get(&root).cloned() else {
            return;
        };
        let k = *k as u32;
        if k >= n {
            return;
        }
        let Some(slot) = self.variables.get(&root).map(|s| s.ptr) else {
            return;
        };
        let elem_ty = self.llvm_type_for_type_expr(&elem_te);
        let arr_ty = elem_ty.array_type(n);
        let i32_t = self.context.i32_type();
        let zero = i32_t.const_zero();
        let idx = i32_t.const_int(k as u64, false);
        let Ok(ep) = (unsafe {
            self.builder
                .build_in_bounds_gep(arr_ty, slot, &[zero, idx], "arr.mv.ep")
        }) else {
            return;
        };
        if self.is_string_type_expr(&elem_te) || self.extract_vec_elem_type(&elem_te).is_some() {
            if let Ok(cap_ptr) =
                self.builder
                    .build_struct_gep(self.vec_struct_type(), ep, 2, "arr.mv.cap")
            {
                let _ = self
                    .builder
                    .build_store(cap_ptr, self.context.i64_type().const_int(0, false));
            }
        } else if let TypeKind::Path(p) = &elem_te.kind {
            if let Some(name) = p.segments.last() {
                if self.type_decls.struct_types.contains_key(name.as_str())
                    && !self.type_decls.shared_types.contains_key(name.as_str())
                {
                    let name = name.clone();
                    self.zero_struct_move_caps_mono(ep, &name, None);
                }
            }
        }
    }

    /// Call-site source suppression for a whole owned `Array[T, N]` passed BY
    /// VALUE into another callee (B-2026-08-22-18 follow-up). The callee takes
    /// ownership (transfer model — [`Self::make_array_param_callee_owned`]), so
    /// the caller's own scope-exit array drop must be retracted, exactly as
    /// moving a Vec/String binding into a call suppresses its buffer free. Keyed
    /// off `owned_array_params`, so a temporary or a non-owning root no-ops.
    /// Without this, `fn g(a: Array[String,2]) { h(a) }` would free the shared
    /// buffers in both `g` and `h` — a double free.
    pub(super) fn suppress_array_binding_move_arg(&mut self, arg: &Expr) {
        let root = match &arg.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return,
        };
        if self.borrow_vars.owned_array_params.remove(&root).is_none() {
            return;
        }
        let Some(slot) = self.variables.get(&root).map(|s| s.ptr) else {
            return;
        };
        // Retract the queued StructDrop for this array's slot (the same
        // compile-time retraction the channel/user-Drop move-out suppressions
        // use at a terminal move site).
        for frame in self.drop_rc.scope_cleanup_actions.iter_mut().rev() {
            frame.retain(|action| {
                !matches!(
                    action,
                    super::state::CleanupAction::StructDrop { struct_alloca, .. }
                        if *struct_alloca == slot
                )
            });
        }
    }

    fn store_enum_word(
        &self,
        enum_ty: StructType<'ctx>,
        base_ptr: PointerValue<'ctx>,
        idx: u32,
        val: inkwell::values::BasicValueEnum<'ctx>,
    ) {
        if let Ok(p) = self
            .builder
            .build_struct_gep(enum_ty, base_ptr, idx, "p14e.store.p")
        {
            let _ = self.builder.build_store(p, val);
        }
    }
}

pub(super) fn is_primitive_type_name(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}
