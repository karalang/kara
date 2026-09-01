//! Control-flow codegen: for, while, loop, if, if-let, match, labeled
//! blocks, break, continue, plus the bounds-check elision plumbing.
//!
//! Houses every per-source-construct compiler that establishes basic
//! blocks for control transfer — the `compile_for_*` family,
//! `compile_if` / `compile_if_let`, `compile_while`, `compile_loop`,
//! `compile_labeled_block`, `compile_break` / `compile_continue`,
//! plus `compile_match` and its supporting machinery
//! (`scrutinee_is_borrow_call`, `compile_pattern_condition`,
//! `extract_enum_tag`, `enum_tag_for_variant`, `enum_type_for_variant`,
//! `pattern_payload_word_count`, `pattern_payload_llvm_type`,
//! `reconstruct_payload_value`). Also houses the BCE-related
//! `collect_asserted_bounds_*` / `walk_guard_conjuncts` /
//! `extract_index_bound_from_binop` / `resolve_len_origin`,
//! `resolve_slice_source` / `load_slice_pattern_element` /
//! `compile_slice_pattern_condition` / `bind_slice_pattern`, and
//! `compile_print`.

use crate::ast::*;

use inkwell::basic_block::BasicBlock;
use inkwell::types::{BasicTypeEnum, IntType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, IntValue, PointerValue};

use super::state::LoopFrame;

/// Saved scrutinee-shape state for `set_scrutinee_shape_flags_for_pattern` /
/// `restore_scrutinee_shape_flags`: `(is_option_result, optres_area,
/// is_shared_enum, is_fresh_owning_temp, is_owned_param, payload_bodies_src)`.
pub(super) type ScrutineeShapeFlags<'ctx> = (
    bool,
    usize,
    bool,
    bool,
    bool,
    Option<(PointerValue<'ctx>, inkwell::values::FunctionValue<'ctx>)>,
    Option<PointerValue<'ctx>>,
);

impl<'ctx> super::Codegen<'ctx> {
    // ── IfLet ────────────────────────────────────────────────────

    pub(super) fn compile_if_let(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Tail-return context: consume it now (the scrutinee `value` below is
        // NOT a tail return), then re-arm it for each branch's final expr so
        // a bare-arg `Option[shared]` leaf gets its per-branch inc.
        let tail = self.fn_ctx.tail_ret_inner.take();
        // Keyed on the scrutinee's span, so compile ORDER is irrelevant.
        let own_value = self.branch_value_is_owned(value);
        let val = self.compile_expr(value)?;
        // B-2026-07-21-8: the if-let route of the ref-chain consuming-read
        // family — `if let Ident(name) = st.tok { <consume name> }` with
        // `st: ref` aliased the caller's payload exactly like the match route
        // (B-2026-07-21-5/-6/-7), but this path never ran the clone legs.
        // Same contract as compile_match: an ESCAPING pattern binding over a
        // `<refparam>.field` chain deep-clones the scrutinee; the enum clone
        // rides the freshtemp drop-tracking (forced below via `did_clone`),
        // the struct clone carries its own StructDrop with the then-arm
        // suppression firing against the clone slot.
        let ref_chain_escapes = matches!(
            value.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) && self.pattern_bindings_escape_in_block(pattern, then_block);
        let (val, did_clone_ref_enum) =
            self.clone_escaping_borrowed_ref_chain_enum(value, val, ref_chain_escapes);
        // B-2026-08-09-11: the LIVE-LOCAL sibling — `if let E.A(v) = e {
        // <consume v> }` where `e` is a plain local read again after the
        // construct. The `match` spelling got this leg with B-2026-08-08-25
        // leg 3; this site kept the transfer path and emptied the source.
        // Same `did_clone` contract, so the clone rides the freshtemp
        // drop-tracking below. Gates are disjoint from the ref-chain leg's
        // (Identifier vs FieldAccess/TupleIndex), so at most one fires.
        let else_end = else_branch.map(|e| e.span.offset + e.span.length);
        let live_local_escapes = self.pattern_bindings_escape_in_block(pattern, then_block);
        let (val, did_clone_live_local_enum) = self.clone_escaping_live_local_enum_block(
            value,
            val,
            pattern,
            live_local_escapes,
            then_block,
            else_end,
        );
        let did_clone_ref_enum = did_clone_ref_enum || did_clone_live_local_enum;
        let (val, refchain_struct_clone) = self.clone_escaping_borrowed_ref_chain_struct(
            value,
            val,
            &[pattern],
            ref_chain_escapes,
        );
        // B-2026-07-21-9: the Option-leaf sibling (`if let Some(s) =
        // <refparam>.opt { <consume s> }`) — consuming then-arm zeroes the
        // clone's tag below; the miss edge leaves its cleanup armed.
        let (val, refchain_option_clone) =
            self.clone_escaping_borrowed_ref_chain_option(value, val, ref_chain_escapes);
        // B-2026-07-21-10: tuple-leaf sibling — see the match site.
        let (val, refchain_tuple_clone) =
            self.clone_escaping_borrowed_ref_chain_tuple(value, val, ref_chain_escapes);
        // B-2026-07-21-14: Result-leaf sibling (`if let Ok(s) =
        // <refparam>.res { <consume s> }`) — consuming then-arm zeroes the
        // clone's payload area below; the miss edge leaves its cleanup armed.
        let (val, refchain_result_clone) =
            self.clone_escaping_borrowed_ref_chain_result(value, val, ref_chain_escapes);
        // B-track (pattern-arm unbound heap-field drop): a fresh-temp enum
        // scrutinee with a heap-bearing payload has no source `EnumDrop`, so an
        // arm that leaves a heap field unbound leaks it (and the miss edge
        // leaks the whole temp). Materialize + `track_enum_var` here so the
        // enum's drop walk frees the unbound fields at the enclosing scope's
        // exit; the suppression after `bind_pattern_values` (then-arm only)
        // zeroes the caps of fields the pattern moved into bindings. No-op for
        // non-fresh / non-enum scrutinees.
        let freshtemp_enum =
            self.materialize_freshtemp_enum_scrutinee(value, pattern, val, did_clone_ref_enum);
        // Oversized-enum-payload §1/§2: free the heap box for a fresh-temp
        // Option[Wide]/Result[Wide,_] scrutinee (box-only — the bound payload
        // owns its inner heap). Registers in the enclosing frame, so the box
        // frees on both the match and miss edges.
        let freshtemp_boxed_slot = if freshtemp_enum.is_none() {
            self.track_freshtemp_boxed_enum_scrutinee(value, &[pattern], val)
        } else {
            None
        };
        // Fresh-temp INLINE-heap `Result` scrutinee (`if let Ok(_) = cell.set(v)`)
        // and fresh-temp `Option[shared]` scrutinee (`if let Some(n) = st.pop()`)
        // — the `match` path (compile_match) registers both, but the if-let path
        // historically stopped at the boxed-enum tracker, so an owned
        // `Option[shared T]` rvalue from an INLINED builtin (`Vec.pop` /
        // `VecDeque.pop_front`, whose result never rides the general
        // owned-temp-drop registration a real `fn`/method return does) leaked
        // its transferred ref: the pattern bind's inc + scope-exit dec cancel,
        // orphaning the +1 the container relinquished (B-2026-07-21-18). Mirror
        // the match chain's mutually-exclusive gating exactly.
        let freshtemp_inline_res = if freshtemp_enum.is_none() {
            self.track_freshtemp_inline_result_scrutinee(value, val)
        } else {
            None
        };
        if freshtemp_enum.is_none() && freshtemp_inline_res.is_none() {
            self.track_freshtemp_shared_option_scrutinee(value, &[pattern], val);
            // B-2026-08-28-74 — the bare `shared` enum sibling; mutually
            // exclusive with the line above by value shape (struct vs RC ptr).
            self.track_freshtemp_shared_enum_scrutinee(value, &[pattern], val);
        }
        let cond = self.compile_pattern_condition(pattern, val)?;
        // Reuse if-else codegen
        let fn_val = self.current_fn.unwrap();
        let then_bb = self.context.append_basic_block(fn_val, "iflet.then");
        let else_bb = self.context.append_basic_block(fn_val, "iflet.else");
        let merge_bb = self.context.append_basic_block(fn_val, "iflet.merge");

        self.builder
            .build_conditional_branch(cond.into_int_value(), then_bb, else_bb)
            .unwrap();
        // B-2026-08-30-2 — dominates both arms and re-executes per pass;
        // see `arm_tail_owner_ctx`.
        let pre_branch_bb = self.builder.get_insert_block().unwrap();
        let arms_all_mint = self.if_arms_all_mint(then_block, else_branch);

        self.builder.position_at_end(then_bb);
        // The cleanup frame is pushed BEFORE the pattern bind (mirroring
        // `compile_while_let`'s body frame and match arms) so a shared
        // pattern binding's scope-exit `RcDec` (`bind_pattern_values`'
        // alias acquire) drains at the END OF THIS ARM — not in the
        // enclosing frame, where a then-block inside a loop would inc once
        // per iteration but dec only once at the enclosing scope's exit.
        self.drop_rc.scope_cleanup_actions.push(Vec::new());
        // Borrowed identifier scrutinee (slice 3q): a `ref` param or a
        // for-loop ELEMENT binding whose container's per-element drop is armed
        // (`scrutinee_is_borrowed_binding`). A payload binding must alias, not
        // register its own free — the owner (caller / container element drop)
        // frees once. Mirrors the `match` path's flag; without it,
        // `for o in v { if let Some(s) = o { … } }` over a
        // `Vec[Option[String]]` double-freed the payload (exit 133).
        let saved_borrow_flag = self.pattern_state.pattern_binding_is_borrow;
        // Slice 3s: `scrutinee_is_borrow_call` was consulted by `match` but
        // NOT here — a read-only `if let Some(x) = m.get(k)` registered an
        // owned track for the aliased payload and double-freed against the
        // map's stored-value drop (exit 133 on plain `Map[i64, String]`).
        // B-2026-08-08-25 (if-let leg): a then-block that only READS the bound
        // payload leaves the source owning it — see
        // `scrutinee_is_readonly_inline_optres_local`.
        let saved_source_retains = self
            .pattern_state
            .pattern_binding_source_retains_inline_payload;
        self.pattern_state
            .pattern_binding_source_retains_inline_payload = {
            // B-2026-08-30-52 — same scoping as the `match` site.
            let saved = self.pattern_state.escape_walk_relaxes_primitive_operators;
            self.pattern_state.escape_walk_relaxes_primitive_operators = true;
            let r =
                self.scrutinee_is_readonly_inline_optres_local_block(value, pattern, then_block);
            self.pattern_state.escape_walk_relaxes_primitive_operators = saved;
            r
        };
        self.pattern_state.pattern_binding_is_borrow = self.pattern_state.pattern_binding_is_borrow
            || self.scrutinee_is_borrowed_binding(value)
            || self.scrutinee_is_borrow_call(value)
            // B-2026-08-08-25 leg 3 (if-let leg) — the USER-ENUM classifier.
            // Held separate from the retains flag above, which is specific to
            // the inline Option/Result payload channel and additionally drives
            // the combinator chain's disarm; a user enum rides
            // `field_drop_kinds` and only needs the borrow classification.
            || self.scrutinee_is_readonly_owned_enum_local_block(value, pattern, then_block)
            // B-2026-08-29-29 (`if let` leg) — the projection-place sibling.
            || self.scrutinee_is_readonly_owned_enum_projection_block(value, pattern, then_block)
            || self.pattern_state.pattern_binding_source_retains_inline_payload;
        // B-2026-07-30-11 (if-let leg): record which of this pattern's
        // binding names sit in a VARIANT payload position so
        // `bind_pattern_values` routes a Drop-declaring payload struct to
        // the UserDrop channel — the exact mirror of the match-arm site
        // (`control_flow_match.rs`); without it `if let Full(r) = b { … }`
        // never ran r's body while the same `match` arm did.
        self.pattern_state.current_variant_payload_bindings.clear();
        {
            let mut vp_names: Vec<String> = Vec::new();
            Self::collect_variant_payload_binding_names(pattern, false, &mut vp_names);
            self.pattern_state
                .current_variant_payload_bindings
                .extend(vp_names);
        }
        let saved_shape_flags =
            self.set_scrutinee_shape_flags_for_pattern(pattern, value, freshtemp_boxed_slot);
        // B-2026-08-28-63 — the `if let` / `while let` copy of the `match`
        // arm's borrowed-only set. Same gate, same reason: a payload binding
        // the block CONSUMES has handed its `Drop` body to whoever took it.
        let saved_arm_borrowed_names =
            std::mem::take(&mut self.pattern_state.pattern_binding_arm_borrowed_only_names);
        {
            let mut names: Vec<String> = Vec::new();
            Self::collect_variant_payload_binding_names(pattern, false, &mut names);
            self.pattern_state.pattern_binding_arm_borrowed_only_names = names
                .into_iter()
                .filter(|n| super::consume_class::binding_only_borrowed_block(n, then_block))
                .collect();
        }
        let bind_res = self.bind_pattern_values(pattern, val);
        self.restore_scrutinee_shape_flags(saved_shape_flags);
        self.pattern_state.pattern_binding_arm_borrowed_only_names = saved_arm_borrowed_names;
        self.pattern_state.current_variant_payload_bindings.clear();
        bind_res?;
        // Slice 3s (B-2026-07-01-12): clone an ESCAPING borrow-mode payload
        // binding — the then-block moving `x` out must own an independent
        // copy (the map retains its stored value).
        if self.pattern_state.pattern_binding_is_borrow {
            self.clone_escaping_borrow_payload_binding(value, pattern, Some(&[]), &[then_block])?;
        }
        let optres_bindings_owned = !self.pattern_state.pattern_binding_is_borrow;
        self.pattern_state.pattern_binding_is_borrow = saved_borrow_flag;
        // B-track: zero the caps of moved-in fields so the source EnumDrop
        // (registered above) frees only the *unbound* heap fields, not the ones
        // the pattern's bindings now own. Then-arm only — the else/miss edge
        // runs no suppression so the drop walk frees the temp wholesale.
        if let Some((alloca, enum_name)) = &freshtemp_enum {
            self.suppress_destructured_enum_payload_cleanup_at(*alloca, enum_name, pattern);
        } else if optres_bindings_owned {
            // B-2026-07-23-13: OWNED-VARIABLE user-enum scrutinee — the missing
            // mirror of the `match` path (control_flow_match.rs, the
            // `suppress_destructured_enum_payload_cleanup(scrutinee, …)` else
            // arm). `let e = E.B(s); if let B(t) = e { … }` destructure-MOVES
            // the String into `t`, but the source `e`'s `__karac_drop_<E>`
            // (queued by `track_enum_var` at its let-site, fires at the outer
            // scope) still read the source's populated payload words and
            // re-freed the same buffer → double-free (the `match` on the same
            // enum was fine — only the if-let leg skipped this). Zero the
            // source's `cap` word(s) for each consumed heap field so the
            // drop-switch's `cap > 0` guard skips. Gated on
            // `optres_bindings_owned` (borrow / ref-param scrutinees fold into
            // `pattern_binding_is_borrow` and no-op here); the helper self-gates
            // to heap-bearing bound fields, so non-heap patterns no-op too. The
            // miss/else edge runs no suppression, so the drop frees `e` whole.
            self.suppress_destructured_enum_payload_cleanup(value, pattern);
            // B-2026-08-29-33 — the PROJECTION-PLACE sibling, which the `match`
            // path has run since #15 and these three legs never did. `if let
            // E.A(r) = s.e { let m = r; … }` therefore left the source struct's
            // enum field fully populated while `m` took the payload, and BOTH
            // freed the buffer: measured as a use-after-free on `karac build`
            // and the JIT alike (valgrind: invalid read of a freed block, no
            // program output at all), against a clean `match` on the same
            // place. It also carries the BODIES half now, so the two move
            // together — the B-2026-08-28-67 lockstep rule.
            self.suppress_destructured_struct_field_enum_cleanup(value, pattern);
            // B-2026-08-31-30 — #16, the plain struct-pattern destructure,
            // which the `match` arm loop has run since it was added and these
            // three legs never did. `if let H { r, .. } = h { … }` therefore
            // left the source struct's field fully populated while the binding
            // owned the same buffer, and BOTH freed it: measured as
            // `free(): double free detected in tcache 2` on `karac build` and
            // on the JIT, against a clean `match` on the same value. Exactly
            // the shape, and exactly the omission, that B-2026-08-29-33 found
            // one level down for an enum-typed FIELD.
            //
            // Then/match edge only, like every suppression around it: the miss
            // edge runs none and the drop frees the source whole.
            self.suppress_destructured_struct_pattern_cleanup(value, pattern);
            // The BODIES half, moving in lockstep with the memory half above —
            // the B-2026-08-28-67 rule. Without it the source's bodies walk
            // still visits the moved-out field and runs its user `Drop` body a
            // second time on the husk the cap-zeroing just left
            // (B-2026-08-31-26).
            self.disarm_arm_destructured_struct_field_bodies(value, pattern);
        }
        // B-2026-06-10-6: a variable `Option[String]`/`Option[Vec]` scrutinee
        // with a `FreeInlineOptionPayload` needs its source `cap` zeroed when
        // this arm binds the payload out, else x's scope-exit free doubles
        // the binding's. No-op for temp / non-inline scrutinees.
        self.suppress_inline_option_payload_cleanup(value, pattern);
        // B-2026-08-05-3: whole-TUPLE payload borrow-only gate — see
        // `arm_only_borrows_result_tuple_payload`. No-op for every other
        // payload shape.
        if !self.block_only_borrows_result_tuple_payload(value, pattern, then_block) {
            self.suppress_inline_result_payload_cleanup(value, pattern);
        }
        self.retract_boxed_tuple_inner_drop_for_block(value, pattern, Some(then_block));
        // B-2026-07-30-11 (Option/Result leg): the payload-BODIES action is
        // retracted alongside the memory suppressions above — same shape
        // gate, interp twin in `pattern_consumes_user_drop_payload`.
        self.suppress_optres_payload_bodies_for_match(value, pattern);
        // B-2026-07-21-16: `if let Some(s) = a.opt { … }` over an OWNED place
        // — zero the source field in the then-arm (the binding owns the
        // payload); the miss edge leaves it for the struct drop.
        self.suppress_consumed_place_optres_field_source(value, pattern, optres_bindings_owned);
        // B-2026-07-22-2: fresh-temp sibling (`if let Some(s) = mk().opt`).
        self.consume_freshtemp_field_scrutinee(value, pattern, optres_bindings_owned);
        self.suppress_inline_option_map_payload_cleanup(value, pattern);
        // B-2026-07-03-31: skip disarming the source payload drop when the
        // then-block ONLY BORROWS the bound payload (not moved out) — the
        // source must free it, else it leaks.
        if !self.block_only_borrows_option_agg_payload(value, pattern, then_block) {
            self.suppress_inline_option_agg_payload_cleanup(value, pattern);
        }
        // B-2026-08-29-2 — the `if let` twin of the match site's leaf-drop
        // retraction: an arm that binds the leaf out already owns it, so the
        // box's interior drop has to stand down or free it twice.
        self.retract_boxed_leaf_drop_for_consuming_pattern(value, pattern);
        // Slice 3t: boxed-payload struct-destructure field suppression — zero
        // the consumed fields inside the box so the binding owns them and the
        // box's inner walk frees only what the pattern left unbound.
        //
        // B-2026-09-01-10 — gated on `optres_bindings_owned`, which is where
        // the `match` spelling has always had it (control_flow_match.rs's
        // `if scrut_ref_ptr.is_none() && !pattern_binding_is_borrow`) and
        // where these three `let`-family paths never did. The comment this
        // replaces claimed the call was "self-gated on `boxed_enum_payload_vars`
        // membership — only a binding OWNED here is registered, so borrow
        // scrutinees no-op", and that is false: `boxed_enum_payload_vars` is a
        // property of the SCRUTINEE VARIABLE, not of the binding mode, so it
        // says nothing about whether the arm took ownership.
        //
        // The population it got wrong is a BORROW-MODE bind over an owned
        // local — `scrutinee_is_readonly_owned_enum_local`, i.e. a body that
        // only READS the bound field. There the bindings alias the box and
        // register no cleanup of their own (`bind_pattern_values`' Vec/String
        // track is gated on the same `!pattern_binding_is_borrow`), so
        // disarming the box left the field with no owner at all: measured
        // 141 B / 3 blocks for `if let Some(Holder { name, id }) = o {
        // println(name.len() + id) }` over three iterations, against a clean
        // `match` on the identical value. Making the body MOVE the field
        // (`slen(name)`) flips the bind to owned and was clean before and
        // after — which is what identifies the binding mode as the axis rather
        // than the construct.
        if optres_bindings_owned {
            self.suppress_boxed_payload_struct_destructure(value, pattern);
            // B-2026-08-04-6 — the FRESH-TEMP twin: same per-field split,
            // against the box staged by `track_freshtemp_boxed_enum_scrutinee`
            // (no named variable exists for the expr-based entry point to
            // find). No-ops when the scrutinee is not a fresh-temp boxed one.
            self.suppress_freshtemp_boxed_payload_struct_destructure(freshtemp_boxed_slot, pattern);
        }
        // B-2026-07-21-8: ref-chain struct clone — fire the per-field
        // cap-zeroing against the CLONE slot (the expr-based suppressors
        // bail on the borrowed root), so the clone's StructDrop frees only
        // the fields this pattern left unbound. Then-arm only, matching the
        // match path.
        if let Some((clone_ptr, clone_name)) = &refchain_struct_clone {
            let (clone_ptr, clone_name) = (*clone_ptr, clone_name.clone());
            self.suppress_destructured_struct_pattern_cleanup_at(clone_ptr, &clone_name, pattern);
        }
        // B-2026-07-21-9: ref-chain Option clone — a consuming Some pattern
        // zeroes the clone's tag (then-arm only; the miss edge keeps the
        // cleanup armed so the clone's payload frees at scope exit).
        if let Some(clone_slot) = refchain_option_clone {
            self.zero_refchain_option_clone_on_consume(clone_slot, pattern);
        }
        // B-2026-07-21-10: ref-chain tuple clone — consumed elements' caps
        // zeroed in the clone slot (then-arm only).
        if let Some((slot, agg_ty, ref elem_tes)) = refchain_tuple_clone {
            let elem_tes = elem_tes.clone();
            self.zero_refchain_tuple_clone_on_consume(slot, agg_ty, &elem_tes, pattern);
        }
        // B-2026-07-21-14: ref-chain Result clone — a consuming Ok/Err
        // pattern zeroes the clone's payload area (then-arm only; the miss
        // edge keeps the cleanup armed so the clone's payload frees at
        // scope exit).
        if let Some(slot) = refchain_result_clone {
            self.suppress_inline_result_payload_cleanup_at(slot, pattern);
        }
        // B-2026-08-08-25 — restore after the suppressors above have consulted
        // it and before the then-block compiles, so a nested match inside the
        // block classifies its own scrutinee from a clean slate.
        self.pattern_state
            .pattern_binding_source_retains_inline_payload = saved_source_retains;
        self.fn_ctx.tail_ret_inner = tail;
        // B-2026-08-28-75 — the `if let` twin of `compile_match`'s arm-tail
        // box-view neutralizer (control_flow_match.rs, B-2026-08-04-2 /
        // B-2026-08-28-66). When the then-block HANDS THE BOUND PAYLOAD OUT as
        // the construct's value (`let k = if let Some(g) = o { g } else { .. }`),
        // that is a move like any other and the source box's interior walk must
        // be retracted, or the box and the destination binding both own the
        // payload's heap.
        //
        // The `match` spelling has had this since B-2026-08-04-2; this path
        // simply never grew it, and the asymmetry stayed invisible because the
        // reassignment eager-free below (stmts.rs) declined for exactly this
        // population — so the un-retracted walk was never reached and the shape
        // presented as a 32-byte envelope LEAK instead. Freeing the envelope
        // without this call turns that leak into a heap-use-after-free: the
        // walk frees the `String` the destination is about to read. Measured on
        // `let k = if let Some(g) = vv { g } else { Val.Nothing }; vv = none();
        // ident_len(k)` — clean with both halves, UAF with only the stmts.rs
        // half, leaking with neither.
        //
        // Same `branch_value_is_owned` gate as the match site, for the same
        // reason it states: neutralizing assumes a destination that registers a
        // drop of its own, and a DISCARDED if-let result has none — without the
        // guard the double free is traded for a leak.
        if own_value {
            if let Some(fe) = then_block.final_expr.as_deref() {
                self.suppress_boxed_payload_view_move(Self::block_tail_expr(fe));
            }
        }
        // B-2026-08-30-52 (b) — the `if let` twin of the match arm's borrow-mode
        // payload registration (control_flow_match.rs, B-2026-07-17-20). Only
        // the `match` site ever recorded these names, so a nested `match` over
        // the binding this construct made could not see that the binding is an
        // ALIAS: the same program read correctly through `match` and double
        // freed through ``if let``, which is the asymmetry B-2026-08-28-67 rules
        // out. Snapshotted and restored around the block for the reason
        // B-2026-08-31-14 records — the set is keyed by binding NAME and would
        // otherwise outlive the construct that made it.
        let saved_borrowed_agg_payload_vars =
            self.borrow_vars.borrowed_agg_payload_struct_vars.clone();
        if !optres_bindings_owned {
            self.register_borrowed_agg_payload_struct_bindings(pattern);
        }
        let mut then_val = self.compile_block(then_block)?;
        self.borrow_vars.borrowed_agg_payload_struct_vars = saved_borrowed_agg_payload_vars;
        // B-2026-08-28-7 — record the then-arm's tail type so an `if let` used
        // as a VALUE receiver (`if let Some(v) = o { Tag { n: v } } else { … }.n`)
        // can be typed, the same way `compile_block_with_frame` records for a
        // plain block and for an `if`'s branches. This arm hand-rolls its frame
        // against a plain `compile_block` (see the B-2026-08-27-34 note below),
        // so it never reaches that recording site and the field read died on
        // codegen's "cannot resolve field" gap while the interpreter answered.
        //
        // Recorded HERE, with the arm's pattern bindings still live, for the
        // same reason the block sibling records before its revert: the tail may
        // name one of them, and the enclosing scope is not the environment that
        // types it.
        if let Some(tail_e) = then_block.final_expr.as_deref() {
            if let Some(tn) = self.type_name_of_expr(tail_e) {
                self.var_types
                    .block_tail_type_names
                    .insert((then_block.span.offset, then_block.span.length), tn);
            }
        }
        let then_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        // B-2026-08-30-2 — hoisted out of the tail-suppression block below so
        // the registration after the drain can still see it.
        let mut then_disarmed = None;
        if !then_terminated {
            // Slice 3s: move-aware then-tail suppression — `if let Some(x) =
            // … { x }` moves the tracked binding into the if-let's value
            // (the caller now owns it), so zero the source's cap / retract
            // its handle cleanup before the frame drains. Mirrors
            // `compile_match`'s arm-tail block verbatim; WITHOUT it the
            // drain freed the escaping buffer and the caller's binding
            // double-freed (exit 133 — pre-existing for OWNED scrutinees
            // like `v.pop()`, not just the Map.get borrow class).
            if let Some(fe) = then_block.final_expr.as_deref() {
                // B-2026-08-29-13 — same ownership gate as `compile_match`'s
                // arm tail: the bare-`shared` transfer inc IS the consumer's
                // ref, so a DISCARDED `if let` (whose scrutinee span
                // `compute_discarded_branch_spans` records) must not emit one —
                // nothing would ever spend it. Only the shared arm is gated;
                // every other suppression this call performs still runs.
                let owns_result = self.branch_value_is_owned(value);
                // B-2026-08-29-5 — and the WHOLE call, not just the shared
                // arm B-2026-08-29-13 gated. Suppression is a HANDOVER: it
                // takes the tail's buffer away from whatever owns it so the
                // branch's consumer can own it instead. A discarded branch has
                // no consumer, so the handover strands the buffer — the same
                // reasoning the note above records for the shared ref and the
                // boxed-payload neutralizer records for its interior walk,
                // applied to every channel this call touches rather than one.
                //
                // Leaving the source armed is the entire fix for the population
                // whose tail NAMES something: a pattern binding, or a local in
                // scope. The population whose tail MINTS its value has no
                // source to leave armed and is handled separately, by giving
                // the value an owner in the arm's own frame.
                if owns_result {
                    self.suppress_source_vec_cleanup_for_arg_ex(fe, owns_result);
                }
                // B-2026-08-30-2 — the if-let THEN arm hand-rolls its frame, so it
                // reaches neither `compile_block_with_frame`'s hook nor
                // `compile_match`'s. Same record, registered after the drain below.
                then_disarmed = self.vecstr_source_disarmed.take();
                // B-2026-08-29-13 — the THIRD leaf hook for the bare-`shared`
                // TRANSFER record, for exactly the reason the `Option[shared T]`
                // note below gives: `compile_block_with_frame` covers plain `if`
                // and an if-let's ELSE branch, but an if-let's THEN branch is
                // compiled here and reaches neither that site nor
                // `compile_match`'s arm tail. Without the raise, the consuming
                // `let` took its own receive-inc on top of the transfer's and
                // the box leaked -- `let keep = if let Bin(l, r) = t { l } else
                // { .. };` at 11 allocs / 10 frees while the `match` spelling of
                // the same program was clean.
                if self.shared_transfer_applied {
                    self.block_tail_shared_transfer = true;
                }
                // B-2026-08-27-34 — the `Option[shared T]` sibling of the
                // suppressor above, and the THIRD leaf hook of the family
                // B-2026-08-26-12 introduced. That fix placed the retain in
                // `compile_block_with_frame` and `compile_match`'s arm tail,
                // and its message claims the first of those covers "if/if let
                // arms". It covers `if` — and an `if let`'s ELSE branch, which
                // routes through `compile_block_with_frame` a few lines below
                // — but NOT an `if let`'s THEN arm, which hand-rolls the frame
                // drain here against a plain `compile_block` and so never
                // reaches that hook.
                //
                // The asymmetry was invisible while the consuming `let` left
                // the binding unregistered: no retain and no dec is still
                // balanced-by-accident for one use. Registering the binding
                // (the B-2026-08-27-34 fix in `control_flow_owned_option_shared`)
                // makes the missing `+1` load-bearing — the new owner's
                // scope-exit `RcDecOption` would free a box the leaf never
                // retained, and the source's own dec would then run through it.
                // Gated on `tail.is_none()` for the same reason the match hook
                // is: when this `if let` IS the function's tail return,
                // `compile_tail_final_expr` already inc'd the leaf.
                if tail.is_none() {
                    self.share_option_shared_ref_for_arg(fe);
                }
                if let ExprKind::Identifier(nm) = &fe.kind {
                    let nm = nm.clone();
                    self.suppress_map_cleanup_for_tail_identifier(&nm);
                }
                // B-2026-08-29-5 — the then-arm sibling of the fresh-tail
                // owner in `compile_block_with_frame`. This arm hand-rolls
                // its frame against a plain `compile_block`, so it reaches
                // neither that hook nor the discarded-`match` statement site,
                // and `if let Some(s) = vv { f(s) };` stranded `f`'s result.
                if !owns_result && self.expr_yields_fresh_owned_temp(fe) {
                    if let Some(v) = then_val {
                        self.materialize_owned_temp(v, (fe.span.offset, fe.span.length));
                    }
                }
            }
            self.drain_top_frame_with_emit();
            // Deep-copy an owned-param then-tail (caller retains the param's
            // buffer) so the if-let value owns an independent buffer — the
            // suppression above only skips a local owner's free, leaving a param
            // tail aliasing the caller's arg. See `compile_if`.
            if let (Some(fe), Some(v)) = (then_block.final_expr.as_deref(), then_val) {
                then_val = Some(self.deepcopy_owned_param_branch_tail(fe, v, own_value)?);
            }
            if let (Some(span), Some(v)) = (self.current_branch_expr_span, then_val) {
                let pending = then_disarmed;
                if own_value && !arms_all_mint {
                    self.register_pending_arm_owner(pending, v, span, Some(pre_branch_bb));
                }
            }
        } else {
            self.drop_rc.scope_cleanup_actions.pop();
        }
        let then_end = self.builder.get_insert_block().unwrap();
        if !then_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(else_bb);
        let else_tail: Option<&Expr>;
        let mut else_pending = None;
        let mut else_val = if let Some(eb) = else_branch {
            self.fn_ctx.tail_ret_inner = tail;
            match &eb.kind {
                ExprKind::Block(blk) => {
                    else_tail = blk.final_expr.as_deref();
                    // B-2026-08-29-5 — an if-let's ELSE arm hands its tail out to
                    // the same (absent) consumer the then-arm does.
                    self.branch_arm_value_discarded = !own_value;
                    if own_value {
                        self.arm_tail_owner_ctx = self
                            .current_branch_expr_span
                            .map(|span| (span, arms_all_mint, pre_branch_bb));
                    }
                    let v = self.compile_block_with_frame(blk)?;
                    else_pending = self.arm_pending_tail_owner.take();
                    v
                }
                _ => {
                    else_tail = Some(eb);
                    Some(self.compile_expr(eb)?)
                }
            }
        } else {
            else_tail = None;
            None
        };
        self.fn_ctx.tail_ret_inner = None;
        let else_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        // Deep-copy an owned-param else-tail — see `compile_if`.
        if !else_terminated {
            if let (Some(fe), Some(v)) = (else_tail, else_val) {
                else_val = Some(self.deepcopy_owned_param_branch_tail(fe, v, own_value)?);
            }
            if let (Some(span), Some(v)) = (self.current_branch_expr_span, else_val) {
                if own_value && !arms_all_mint {
                    self.register_pending_arm_owner(else_pending, v, span, Some(pre_branch_bb));
                }
            }
        }
        let else_end = self.builder.get_insert_block().unwrap();
        if !else_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);
        let placeholder = self.context.i64_type().const_int(0, false).into();
        match (then_terminated, else_terminated) {
            // Both arms diverge — terminate the unreachable merge block (see
            // `compile_if` for the gap-d rationale).
            (true, true) => {
                self.builder.build_unreachable().unwrap();
                Ok(placeholder)
            }
            // Exactly one arm diverges: the `if let` value is the live arm's
            // value (single live predecessor dominates the merge).
            (true, false) => Ok(else_val.unwrap_or(placeholder)),
            (false, true) => Ok(then_val.unwrap_or(placeholder)),
            (false, false) => {
                if let (Some(tv), Some(ev)) = (then_val, else_val) {
                    // Same narrow-int width reconciliation as `compile_if`.
                    let (tv, ev) = self.unify_int_branch_widths(tv, then_end, ev, else_end);
                    // B-2026-08-30-49 — and the same mixed int/float pair:
                    // `let a: f64 = if let Some(v) = o { v } else { 0.0 }` fell
                    // to the placeholder exactly as the plain `if` did.
                    let (tv, ev) = self.unify_int_float_branch_values(
                        tv,
                        self.branch_tail_is_unsigned_int(then_block.final_expr.as_deref()),
                        then_end,
                        ev,
                        self.branch_tail_is_unsigned_int(else_tail),
                        else_end,
                    );
                    if tv.get_type() == ev.get_type() {
                        let phi = self.builder.build_phi(tv.get_type(), "ifletval").unwrap();
                        phi.add_incoming(&[(&tv, then_end), (&ev, else_end)]);
                        return Ok(phi.as_basic_value());
                    }
                }
                Ok(placeholder)
            }
        }
    }

    // ── WhileLet ─────────────────────────────────────────────────

    /// Lower `while let PAT = SCRUT { BODY }` (phase-6-runtime.md line 489).
    /// Structurally a `compile_while` whose condition is a pattern test:
    /// the loop header re-evaluates the scrutinee each iteration, tests it
    /// against the pattern (`compile_pattern_condition`), and on a match
    /// binds the pattern's names (`bind_pattern_values`) before running the
    /// body. A per-iteration scope-cleanup frame (same shape as
    /// `compile_while`) drops the iteration's pattern bindings and any body
    /// temporaries before the next iteration's scrutinee is evaluated.
    pub(super) fn compile_while_let(
        &mut self,
        label: Option<&str>,
        pattern: &Pattern,
        value: &Expr,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "whilelet.cond");
        let body_bb = self.context.append_basic_block(fn_val, "whilelet.body");
        // The miss edge gets its own block (rather than branching straight to
        // exit) so the final non-matching fresh-temp scrutinee can be dropped
        // there — see the loop-exit handling below.
        let miss_bb = self.context.append_basic_block(fn_val, "whilelet.miss");
        let exit_bb = self.context.append_basic_block(fn_val, "whilelet.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.fn_ctx.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: cond_bb,
            break_bb: exit_bb,
            result_slot: None,
            result_ty: None,
            cleanup_depth: self.drop_rc.scope_cleanup_actions.len(),
        });

        // Header: re-evaluate the scrutinee and test the pattern every
        // iteration. `val` is defined in `cond_bb`, which dominates
        // `body_bb`, so the bind below can reuse it (same SSA shape as
        // `compile_if_let`).
        self.builder.position_at_end(cond_bb);
        let val = self.compile_expr(value)?;
        // B-2026-07-21-8 (while-let leg): an ESCAPING `while let V(x) =
        // <refparam>.field` aliases the caller's payload exactly like the
        // match/if-let routes — clone per evaluation. The clone emits here in
        // the HEADER, so a matching iteration's copy rides the per-iteration
        // freshtemp drop (forced below via `did_clone`), and the final
        // non-matching evaluation's copy is freed wholesale on the miss edge
        // (`force` on the miss drop). The struct-leaf leg is deliberately not
        // wired here: a struct pattern always matches, so that while-let
        // shape is a degenerate infinite loop, not a real idiom.
        let ref_chain_escapes = matches!(
            value.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        ) && self.pattern_bindings_escape_in_block(pattern, body);
        let (val, did_clone_ref_enum) =
            self.clone_escaping_borrowed_ref_chain_enum(value, val, ref_chain_escapes);
        // B-2026-08-09-11: the LIVE-LOCAL sibling, per evaluation — same
        // contract as the ref-chain leg just above, whose per-iteration
        // freshtemp drop and miss-edge free the clone already rides. Liveness
        // bounds on the loop BODY, so a read inside the body does not by
        // itself arm the clone while a read after the loop does.
        let live_local_escapes = self.pattern_bindings_escape_in_block(pattern, body);
        let (val, did_clone_live_local_enum) = self.clone_escaping_live_local_enum_block(
            value,
            val,
            pattern,
            live_local_escapes,
            body,
            None,
        );
        let did_clone_ref_enum = did_clone_ref_enum || did_clone_live_local_enum;
        let cond = self.compile_pattern_condition(pattern, val)?;
        self.builder
            .build_conditional_branch(cond.into_int_value(), body_bb, miss_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        // Per-iteration scope frame, same shape as `compile_while` — see its
        // comment for the leak rationale.
        self.drop_rc.scope_cleanup_actions.push(Vec::new());
        // B-track (pattern-arm unbound heap-field drop): a fresh-temp enum
        // scrutinee with a heap payload field the arm leaves unbound leaks per
        // iteration. Unlike if-let/let-else (one enclosing-frame drop), the
        // materialize + `track_enum_var` must register in the *per-iteration*
        // body frame (pushed just above) so the EnumDrop drains at the bottom
        // of each iteration and the entry alloca is overwritten by the next
        // iteration's scrutinee before being read again. The store emits here
        // in `body_bb` (dominated by `cond_bb` where `val` is defined). The
        // heap-bearing *miss* variant at loop exit (the final non-matching
        // scrutinee) is freed wholesale on the `miss_bb` edge below.
        let freshtemp_enum =
            self.materialize_freshtemp_enum_scrutinee(value, pattern, val, did_clone_ref_enum);
        // Oversized-enum-payload §1/§2: free the heap box for a fresh-temp
        // boxed-payload scrutinee, registered in the per-iteration body frame
        // (drains each iteration). An `Option` loop terminates on `None` (no
        // box), so no miss-edge box free is needed; a `Result`-terminating
        // boxed `Err` miss is deferred (spike §1, rare shape).
        let freshtemp_boxed_slot = if freshtemp_enum.is_none() {
            self.track_freshtemp_boxed_enum_scrutinee(value, &[pattern], val)
        } else {
            None
        };
        // Fresh-temp inline-`Result` / `Option[shared]` scrutinee — mirror the
        // match + if-let chain so `while let Some(n) = st.pop()` over a
        // `Vec[shared T]` releases the popped node's transferred ref per
        // iteration instead of leaking it (B-2026-07-21-18). Registered in the
        // current (per-iteration body) frame, same as the boxed tracker above.
        let freshtemp_inline_res = if freshtemp_enum.is_none() {
            self.track_freshtemp_inline_result_scrutinee(value, val)
        } else {
            None
        };
        if freshtemp_enum.is_none() && freshtemp_inline_res.is_none() {
            self.track_freshtemp_shared_option_scrutinee(value, &[pattern], val);
            // B-2026-08-28-74 — the bare `shared` enum sibling; mutually
            // exclusive with the line above by value shape (struct vs RC ptr).
            self.track_freshtemp_shared_enum_scrutinee(value, &[pattern], val);
        }
        // Borrowed identifier scrutinee — see the if-let site (slice 3q).
        // Slice 3s adds the borrow-CALL half (`m.get` scrutinee) + the
        // escaping-payload clone, mirroring if-let.
        let saved_borrow_flag = self.pattern_state.pattern_binding_is_borrow;
        // B-2026-08-08-25 (while-let leg): same caller-retains classifier as
        // the match / if-let sites.
        let saved_source_retains = self
            .pattern_state
            .pattern_binding_source_retains_inline_payload;
        self.pattern_state
            .pattern_binding_source_retains_inline_payload = {
            // B-2026-08-30-52 — same scoping as the `match` site.
            let saved = self.pattern_state.escape_walk_relaxes_primitive_operators;
            self.pattern_state.escape_walk_relaxes_primitive_operators = true;
            let r = self.scrutinee_is_readonly_inline_optres_local_block(value, pattern, body);
            self.pattern_state.escape_walk_relaxes_primitive_operators = saved;
            r
        };
        self.pattern_state.pattern_binding_is_borrow = self.pattern_state.pattern_binding_is_borrow
            || self.scrutinee_is_borrowed_binding(value)
            || self.scrutinee_is_borrow_call(value)
            // B-2026-08-08-25 leg 3 (while-let leg) — see the if-let site.
            || self.scrutinee_is_readonly_owned_enum_local_block(value, pattern, body)
            // B-2026-08-29-29 (`while let` leg) — the projection-place sibling.
            || self.scrutinee_is_readonly_owned_enum_projection_block(value, pattern, body)
            || self.pattern_state.pattern_binding_source_retains_inline_payload;
        // B-2026-07-30-11 (while-let leg): route a Drop-declaring variant
        // payload binding to the UserDrop channel — the match/if-let sites'
        // mirror. The binding lives in the per-iteration body frame, so the
        // body fires once per matched iteration at the binding's NLL end.
        self.pattern_state.current_variant_payload_bindings.clear();
        {
            let mut vp_names: Vec<String> = Vec::new();
            Self::collect_variant_payload_binding_names(pattern, false, &mut vp_names);
            self.pattern_state
                .current_variant_payload_bindings
                .extend(vp_names);
        }
        let saved_shape_flags =
            self.set_scrutinee_shape_flags_for_pattern(pattern, value, freshtemp_boxed_slot);
        // B-2026-08-28-63 — the `if let` / `while let` copy of the `match`
        // arm's borrowed-only set. Same gate, same reason: a payload binding
        // the block CONSUMES has handed its `Drop` body to whoever took it.
        let saved_arm_borrowed_names =
            std::mem::take(&mut self.pattern_state.pattern_binding_arm_borrowed_only_names);
        {
            let mut names: Vec<String> = Vec::new();
            Self::collect_variant_payload_binding_names(pattern, false, &mut names);
            self.pattern_state.pattern_binding_arm_borrowed_only_names = names
                .into_iter()
                .filter(|n| super::consume_class::binding_only_borrowed_block(n, body))
                .collect();
        }
        let bind_res = self.bind_pattern_values(pattern, val);
        self.restore_scrutinee_shape_flags(saved_shape_flags);
        self.pattern_state.pattern_binding_arm_borrowed_only_names = saved_arm_borrowed_names;
        self.pattern_state.current_variant_payload_bindings.clear();
        bind_res?;
        if self.pattern_state.pattern_binding_is_borrow {
            self.clone_escaping_borrow_payload_binding(value, pattern, Some(&[]), &[body])?;
        }
        let optres_bindings_owned = !self.pattern_state.pattern_binding_is_borrow;
        self.pattern_state.pattern_binding_is_borrow = saved_borrow_flag;
        if let Some((alloca, enum_name)) = &freshtemp_enum {
            self.suppress_destructured_enum_payload_cleanup_at(*alloca, enum_name, pattern);
        } else if optres_bindings_owned {
            // B-2026-08-09-14 — the WHILE-LET leg of B-2026-07-23-13, which
            // gave this arm to `if let` and left the loop form on the raw
            // transfer path: a consuming arm over an OWNED enum local moved
            // the payload into the binding but never zeroed the SOURCE's cap,
            // so `e`'s `__karac_drop_<E>` re-freed the buffer the binding had
            // already freed. `match` and `if let` were both correct; only this
            // site was missing.
            //
            // It surfaced only for a source DEAD after the loop because
            // B-2026-08-09-11's live-local clone leg (in the header above)
            // hands a LIVE source's arm its own buffer, so the source's free
            // is not a double one. Dead sources decline that clone by design —
            // the liveness gate is what keeps the common shape off a copy it
            // does not need — and land back here.
            //
            // Emitted in `body_bb`, so it runs per MATCHED iteration and the
            // miss edge stays unsuppressed (the drop frees `e` whole there),
            // matching the if-let contract. Re-zeroing an already-zero cap is
            // a no-op, and an iteration that reassigns the source re-arms it:
            // the assignment's drop of the old value reads the zeroed cap and
            // skips the payload the binding now owns, then the next
            // iteration's store re-populates and this store re-fires.
            self.suppress_destructured_enum_payload_cleanup(value, pattern);
            // B-2026-08-29-33, `while let` leg — see the `if let` note above.
            self.suppress_destructured_struct_field_enum_cleanup(value, pattern);
            // B-2026-08-31-30 — #16, the plain struct-pattern destructure,
            // which the `match` arm loop has run since it was added and these
            // three legs never did. `if let H { r, .. } = h { … }` therefore
            // left the source struct's field fully populated while the binding
            // owned the same buffer, and BOTH freed it: measured as
            // `free(): double free detected in tcache 2` on `karac build` and
            // on the JIT, against a clean `match` on the same value. Exactly
            // the shape, and exactly the omission, that B-2026-08-29-33 found
            // one level down for an enum-typed FIELD.
            //
            // Then/match edge only, like every suppression around it: the miss
            // edge runs none and the drop frees the source whole.
            self.suppress_destructured_struct_pattern_cleanup(value, pattern);
            // The BODIES half, moving in lockstep with the memory half above —
            // the B-2026-08-28-67 rule. Without it the source's bodies walk
            // still visits the moved-out field and runs its user `Drop` body a
            // second time on the husk the cap-zeroing just left
            // (B-2026-08-31-26).
            self.disarm_arm_destructured_struct_field_bodies(value, pattern);
        }
        // B-2026-06-10-6: variable inline-`Option` scrutinee source-cap
        // suppression (see `compile_if_let`). No-op for temp / non-inline.
        self.suppress_inline_option_payload_cleanup(value, pattern);
        // B-2026-08-05-3: whole-TUPLE payload borrow-only gate — see
        // `arm_only_borrows_result_tuple_payload`. No-op for every other
        // payload shape.
        if !self.block_only_borrows_result_tuple_payload(value, pattern, body) {
            self.suppress_inline_result_payload_cleanup(value, pattern);
        }
        self.retract_boxed_tuple_inner_drop_for_block(value, pattern, Some(body));
        // B-2026-07-30-11 (Option/Result leg): the payload-BODIES action is
        // retracted alongside the memory suppressions above — same shape
        // gate, interp twin in `pattern_consumes_user_drop_payload`.
        self.suppress_optres_payload_bodies_for_match(value, pattern);
        self.suppress_inline_option_map_payload_cleanup(value, pattern);
        // B-2026-07-03-31: skip disarming the source payload drop when the
        // loop body ONLY BORROWS the bound payload (not moved out) — the source
        // must free it, else it leaks.
        if !self.block_only_borrows_option_agg_payload(value, pattern, body) {
            self.suppress_inline_option_agg_payload_cleanup(value, pattern);
        }
        // Slice 3t: boxed-payload struct-destructure field suppression — zero
        // the consumed fields inside the box so the binding owns them and the
        // box's inner walk frees only what the pattern left unbound.
        //
        // B-2026-09-01-10 — gated on `optres_bindings_owned`, the gate the
        // `match` spelling has always had and these three `let`-family paths
        // never did. See the `compile_if_let` site for what the ungated call
        // got wrong and how it was measured.
        if optres_bindings_owned {
            self.suppress_boxed_payload_struct_destructure(value, pattern);
            // B-2026-08-04-6 — the FRESH-TEMP twin: same per-field split,
            // against the box staged by `track_freshtemp_boxed_enum_scrutinee`
            // (no named variable exists for the expr-based entry point to
            // find). No-ops when the scrutinee is not a fresh-temp boxed one.
            self.suppress_freshtemp_boxed_payload_struct_destructure(freshtemp_boxed_slot, pattern);
        }
        // B-2026-08-08-25 — restore after the suppressors, before the body
        // compiles (see the if-let site).
        self.pattern_state
            .pattern_binding_source_retains_inline_payload = saved_source_retains;
        // B-2026-08-30-52 (b) — the `while let` twin of the match arm's borrow-mode
        // payload registration (control_flow_match.rs, B-2026-07-17-20). Only
        // the `match` site ever recorded these names, so a nested `match` over
        // the binding this construct made could not see that the binding is an
        // ALIAS: the same program read correctly through `match` and double
        // freed through ``while let``, which is the asymmetry B-2026-08-28-67 rules
        // out. Snapshotted and restored around the block for the reason
        // B-2026-08-31-14 records — the set is keyed by binding NAME and would
        // otherwise outlive the construct that made it.
        let saved_borrowed_agg_payload_vars =
            self.borrow_vars.borrowed_agg_payload_struct_vars.clone();
        if !optres_bindings_owned {
            self.register_borrowed_agg_payload_struct_bindings(pattern);
        }
        self.compile_block(body)?;
        self.borrow_vars.borrowed_agg_payload_struct_vars = saved_borrowed_agg_payload_vars;
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            self.drain_top_frame_with_emit();
            self.builder.build_unconditional_branch(cond_bb).unwrap();
        } else {
            self.drop_rc.scope_cleanup_actions.pop();
        }

        self.fn_ctx.loop_stack.pop();

        // Miss edge (loop exit): the final scrutinee did not match the
        // pattern. If it is a fresh-temp enum carrying heap in its (unmatched)
        // variant, free it wholesale here — it never entered the per-iteration
        // body frame, so this is the only place it can be dropped (B
        // follow-up #2). A miss binds nothing out, so no cap-suppression: the
        // whole value drops. `val` is defined in `cond_bb`, which dominates
        // `miss_bb`. Place / heap-free scrutinees are a no-op (the helper's
        // gate), so a place scrutinee keeps its owner's cleanup untouched.
        self.builder.position_at_end(miss_bb);
        self.drop_freshtemp_enum_scrutinee_on_miss(value, pattern, val, did_clone_ref_enum);
        self.builder.build_unconditional_branch(exit_bb).unwrap();

        self.builder.position_at_end(exit_bb);
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    /// Set the scrutinee-shape flags `bind_pattern_values` consults
    /// (`pattern_binding_scrutinee_is_option_result` / `_optres_area` /
    /// `_is_shared_enum`) from a SINGLE variant pattern — the if-let /
    /// while-let / let-else twin of `compile_match`'s per-arms derivation.
    /// Without these, `bind_pattern_values` classified an `Option`/`Result`
    /// payload binding as a plain user struct: a heap-BOXED payload
    /// (`if let Some(r) = v.pop()` with a >3-word struct) then got its own
    /// owned track on top of the box drop that already owns the interior —
    /// a double-free the match path has excluded since B-2026-06-13-13.
    /// Returns the saved triple for `restore_scrutinee_shape_flags`.
    pub(super) fn set_scrutinee_shape_flags_for_pattern(
        &mut self,
        pattern: &Pattern,
        scrutinee: &Expr,
        freshtemp_boxed_slot: Option<PointerValue<'ctx>>,
    ) -> ScrutineeShapeFlags<'ctx> {
        let saved = (
            self.pattern_state
                .pattern_binding_scrutinee_is_option_result,
            self.pattern_state.pattern_binding_scrutinee_optres_area,
            self.pattern_state.pattern_binding_scrutinee_is_shared_enum,
            self.pattern_state
                .pattern_binding_scrutinee_is_fresh_owning_temp,
            self.pattern_state.pattern_binding_scrutinee_is_owned_param,
            self.pattern_state
                .pattern_binding_scrutinee_payload_bodies_src,
            self.pattern_state.pattern_binding_scrutinee_optres_slot,
        );
        self.pattern_state
            .pattern_binding_scrutinee_is_fresh_owning_temp =
            self.scrutinee_expr_is_owning_fresh_temp(scrutinee);
        // B-2026-08-01-13 — see `compile_match`'s twin derivation.
        self.pattern_state.pattern_binding_scrutinee_is_owned_param =
            self.scrutinee_is_owned_param_binding(scrutinee);
        // B-2026-08-02-25 (match-arm leg) + B-2026-08-04-1 (its fresh-temp
        // twin) — see `compile_match`'s twin derivation. Named source first;
        // a fresh temp has no armed walk to sample, so it falls through to the
        // staged `__freshtemp_boxed_scrut` slot. The two are mutually exclusive
        // by construction (a temp has no name), so the order is documentation
        // rather than a tie-break.
        self.pattern_state
            .pattern_binding_scrutinee_payload_bodies_src =
            match self.scrutinee_armed_payload_bodies_action(scrutinee) {
                Some(found) => Some(found),
                None => self.freshtemp_payload_bodies_action(scrutinee, freshtemp_boxed_slot),
            };
        // B-2026-08-04-2 — see `compile_match`'s twin.
        self.pattern_state.pattern_binding_scrutinee_optres_slot =
            self.scrutinee_optres_slot(scrutinee, freshtemp_boxed_slot);
        let en = self.variant_pattern_enum_name(pattern);
        self.pattern_state
            .pattern_binding_scrutinee_is_option_result =
            matches!(en.as_deref(), Some("Option") | Some("Result"));
        self.pattern_state.pattern_binding_scrutinee_optres_area = match en.as_deref() {
            Some("Option") => 3,
            Some("Result") => 5,
            _ => 0,
        };
        self.pattern_state.pattern_binding_scrutinee_is_shared_enum = en
            .and_then(|n| self.type_decls.shared_types.get(&n).cloned())
            .is_some_and(|i| i.is_enum);
        saved
    }

    /// Restore the sextuple saved by `set_scrutinee_shape_flags_for_pattern`.
    pub(super) fn restore_scrutinee_shape_flags(&mut self, saved: ScrutineeShapeFlags<'ctx>) {
        self.pattern_state
            .pattern_binding_scrutinee_is_option_result = saved.0;
        self.pattern_state.pattern_binding_scrutinee_optres_area = saved.1;
        self.pattern_state.pattern_binding_scrutinee_is_shared_enum = saved.2;
        self.pattern_state
            .pattern_binding_scrutinee_is_fresh_owning_temp = saved.3;
        self.pattern_state.pattern_binding_scrutinee_is_owned_param = saved.4;
        self.pattern_state
            .pattern_binding_scrutinee_payload_bodies_src = saved.5;
        self.pattern_state.pattern_binding_scrutinee_optres_slot = saved.6;
    }

    /// B-2026-08-04-2 — the scrutinee's `Option`/`Result` slot: a named
    /// binding's own slot, else the staged fresh-temp alloca. Unlike
    /// `scrutinee_armed_payload_bodies_action` this does NOT require the
    /// payload to run a user Drop — the double-free it guards is pure memory,
    /// so a payload with a plain `String` field and no `impl Drop` is in scope
    /// too. Restricted to the scrutinee spellings whose slot is a stable
    /// alloca; a borrow-returning call has no owned slot to neutralize and is
    /// filtered at the bind site by `pattern_binding_is_borrow`.
    pub(super) fn scrutinee_optres_slot(
        &self,
        e: &Expr,
        freshtemp_boxed_slot: Option<PointerValue<'ctx>>,
    ) -> Option<PointerValue<'ctx>> {
        let name = match &e.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            _ => return freshtemp_boxed_slot,
        };
        self.variables
            .get(name)
            .map(|s| s.ptr)
            .or(freshtemp_boxed_slot)
    }

    /// B-2026-08-02-25 (match-arm leg) — the armed `__karac_dropelems_opt_*` /
    /// `__karac_dropelems_res_*` payload-bodies action on this scrutinee, as
    /// `(slot, walker)`. A consuming `Some(x)` / `Ok(x)` / `Err(x)` arm
    /// retracts it (`suppress_optres_payload_bodies_for_match`), so `Some`
    /// here says "the source is about to stop owning the payload's Drop
    /// body" — and for a heap-BOXED payload that walk is the body's ONLY fire
    /// path, since the box drop runs the payload's MEMORY-only drop
    /// (B-2026-08-03-10).
    ///
    /// The SLOT travels with the walker deliberately: the re-homed
    /// registration re-runs this same action against this same source slot,
    /// only under the arm binding's name so it fires at the binding's death
    /// instead of the source's. See the field's doc for why the binding's own
    /// reconstructed copy is the wrong subject.
    ///
    /// Must be sampled BEFORE the arm's suppressors run. Both callers do:
    /// `compile_match` derives its scrutinee flags above the arm loop, and
    /// the if-let / while-let / let-else helper above runs before its own
    /// suppressor block. Restricted to the same scrutinee spellings the
    /// suppressor accepts (a bare name / `self`), so the two agree by
    /// construction.
    pub(super) fn scrutinee_armed_payload_bodies_action(
        &self,
        e: &Expr,
    ) -> Option<(PointerValue<'ctx>, inkwell::values::FunctionValue<'ctx>)> {
        let name = match &e.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::SelfValue => "self",
            _ => return None,
        };
        self.armed_container_elem_bodies_action(name)
    }

    /// B-2026-08-04-1 — the FRESH-TEMP counterpart of
    /// `scrutinee_armed_payload_bodies_action`. A temp has no named source and
    /// so no armed walk to sample, but `track_freshtemp_boxed_enum_scrutinee`
    /// does stage the scrutinee aggregate into `__freshtemp_boxed_scrut` and
    /// register the box drop against it — so that slot is the same kind of
    /// subject the named route re-homes: the one holding the box pointer the
    /// scope-exit free will read.
    ///
    /// Why it has to be that slot and not the arm binding's alloca: the binding
    /// holds a reconstructed COPY of the payload's `{ptr,len,cap}`, and a Drop
    /// body that MUTATES a heap field (`self.buf.clear()`) run against the copy
    /// frees the buffer and zeroes the copy's cap while the box keeps the stale
    /// pointer — then the box drop frees it again. Registering the tag-guarded
    /// `__karac_dropelems_opt_*` walk over the staged slot puts the mutation
    /// where the later free looks.
    ///
    /// `None` whenever any ingredient is missing — no staged slot (non-boxed,
    /// non-fresh, or a borrow scrutinee), no resolvable instantiation, or a
    /// payload that runs no user Drop — and the caller then keeps the
    /// pre-existing bodies-only-over-the-copy registration, i.e. today's
    /// behavior.
    pub(super) fn freshtemp_payload_bodies_action(
        &mut self,
        scrutinee: &Expr,
        staged_slot: Option<PointerValue<'ctx>>,
    ) -> Option<(PointerValue<'ctx>, inkwell::values::FunctionValue<'ctx>)> {
        let slot = staged_slot?;
        let te = self.optres_scrutinee_type_expr(scrutinee)?;
        let walker = self.emit_optres_payload_user_drop_bodies_fn(&te)?;
        Some((slot, walker))
    }

    /// B-2026-08-04-3 — the payload STRUCT name of a boxed `Option`/`Result`
    /// scrutinee, for the arms that need it when the sub-pattern carries no
    /// type of its own (a wildcard binds nothing, so `pattern_binding_types`
    /// has no entry to consult).
    ///
    /// Takes the FIRST generic arg for `Option` and, for `Result`, whichever
    /// of `Ok`/`Err` the variant names — the caller passes the variant it is
    /// registering the box drop for, so the two cannot be crossed.
    pub(super) fn optres_scrutinee_payload_struct_name_for(
        &self,
        scrutinee: &Expr,
        variant: &str,
    ) -> Option<String> {
        use crate::ast::{GenericArg, TypeKind};
        let te = self.optres_scrutinee_type_expr(scrutinee)?;
        let TypeKind::Path(p) = &te.kind else {
            return None;
        };
        let args = p.generic_args.as_ref()?;
        let idx = match variant {
            "Err" => 1usize,
            _ => 0usize,
        };
        let GenericArg::Type(pt) = args.get(idx)? else {
            return None;
        };
        let TypeKind::Path(pp) = &pt.kind else {
            return None;
        };
        pp.segments.first().cloned()
    }

    /// The instantiated `Option[T]` / `Result[O, E]` a match/if-let scrutinee
    /// EXPRESSION produces. Same two-step resolution
    /// `track_discarded_optres_payload_bodies` uses for the discarded-temp
    /// case: the span table first (a ctor or typed producer records the
    /// instantiation there), then the fn-return / `pop`-family derivation for
    /// the calls whose span the table misses.
    pub(super) fn optres_scrutinee_type_expr(&self, e: &Expr) -> Option<crate::ast::TypeExpr> {
        self.type_decls
            .enum_inst_type_exprs
            .get(&(e.span.offset, e.span.length))
            .cloned()
            .or_else(|| self.untyped_let_boxed_enum_te(e))
    }

    /// B-2026-08-01-13 — is the scrutinee an Identifier naming an OWNED
    /// (by-value, non-`ref`) parameter of the current function? Payload
    /// bindings destructured from one are views of the callee's entry copy
    /// under the caller-retains convention — their Drop bodies belong to
    /// the caller's fire, so the pattern-binding registration goes
    /// memory-only.
    ///
    /// B-2026-08-03-3 leg B — the root, not just a bare name: `match p.field {
    /// Ok(x) => … }` inside `fn take(p: H)` binds out of the same entry copy a
    /// bare `match p` would, so it carries the same caller-retains body
    /// ownership. Restricting the test to a bare Identifier made the callee's
    /// arm fire the payload's body on top of the caller's — visible only once
    /// the field class in question got a body to fire at all (the Option twin
    /// stayed silent because a 4-word payload is BOXED at Option's 3-word area
    /// and so failed the inline-payload registration, while the same payload is
    /// INLINE in `Result`'s 5-word area). Field / tuple-index chains are the
    /// only widening: an index or call in the chain is not a plain view.
    pub(super) fn scrutinee_is_owned_param_binding(&self, e: &Expr) -> bool {
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::FieldAccess { object, .. } => cur = object,
                ExprKind::TupleIndex { object, .. } => cur = object,
                ExprKind::Identifier(n) => {
                    return (self.fn_ctx.current_fn_param_names.contains(n.as_str())
                        && !self.borrow_vars.ref_params.contains_key(n.as_str()))
                        || self.payload_vars.param_view_locals.contains(n.as_str());
                }
                _ => return false,
            }
        }
    }

    /// Is this scrutinee expression a FRESH OWNING temp — a call, or a
    /// method call that isn't a borrow accessor (`scrutinee_is_borrow_call`)?
    /// The codegen twin of the interpreter's `scrutinee_expr_is_consuming`
    /// fresh-temp half; identifier/`self` places are NOT fresh (their moved
    /// payload's body rides the binding-side channel).
    pub(super) fn scrutinee_expr_is_owning_fresh_temp(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call { .. } => true,
            ExprKind::MethodCall { .. } => !self.scrutinee_is_borrow_call(e),
            _ => false,
        }
    }

    // ── LetElse ──────────────────────────────────────────────────

    /// Lower `let PAT = SCRUT else { ELSE }` (phase-6-runtime.md line 489).
    /// Evaluate the scrutinee, test it against the pattern, and branch: on a
    /// match, bind the pattern's names into the **enclosing** scope (so they
    /// are live for the rest of the block) and fall through to the following
    /// statements; on a miss, run the else block, which the typechecker has
    /// already verified diverges (`return` / `break` / `continue` / panic).
    /// Mirrors `compile_if_let`'s scrutinee+condition machinery, but the
    /// bindings escape the construct and there is no merge block — the match
    /// edge continues straight into the block and the else edge diverges.
    pub(super) fn compile_let_else(
        &mut self,
        pattern: &Pattern,
        value: &Expr,
        else_block: &Block,
    ) -> Result<(), String> {
        let val = self.compile_expr(value)?;
        // B-2026-07-21-8 (let-else leg): same ref-chain clone contract as the
        // if-let site. A `let…else` binding escapes into the enclosing scope
        // by construction, so the escape flag is unconditional for a matching
        // shape (no arm to analyze — mirroring the `escape_exprs = None`
        // convention of the borrow-payload clone below).
        let ref_chain_escapes = matches!(
            value.kind,
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. }
        );
        let (val, did_clone_ref_enum) =
            self.clone_escaping_borrowed_ref_chain_enum(value, val, ref_chain_escapes);
        // B-2026-08-09-11 — the live-local clone leg is deliberately NOT wired
        // at this site, unlike at `if let` / `while let`. It was, briefly, on
        // the strength of a probe that DID reproduce the emptied-source
        // signature here (`hi` then an empty line where `--interp` printed
        // `hi` twice). `karac check` then rejected that probe outright: `let
        // A(v) = e else { … }` MOVES `e`, so the later read of `e` the leg
        // keys on is a `UseAfterMove` and codegen never sees the program in
        // production. The only spelling that survives the ownership checker
        // reassigns `e` first — and then the value read after is FRESH, so an
        // emptied source can never be observed. The leg would still fire there
        // (the reassignment is a mention past the construct) and buy a clone
        // nobody reads. Unreachable-for-the-bug, so it is left out rather than
        // shipped as dead weight.
        let (val, refchain_struct_clone) = self.clone_escaping_borrowed_ref_chain_struct(
            value,
            val,
            &[pattern],
            ref_chain_escapes,
        );
        // B-2026-07-21-9: Option-leaf sibling — see the if-let site.
        let (val, refchain_option_clone) =
            self.clone_escaping_borrowed_ref_chain_option(value, val, ref_chain_escapes);
        // B-2026-07-21-10: tuple-leaf sibling — see the match site.
        let (val, refchain_tuple_clone) =
            self.clone_escaping_borrowed_ref_chain_tuple(value, val, ref_chain_escapes);
        // B-2026-07-21-14: Result-leaf sibling — see the if-let site.
        let (val, refchain_result_clone) =
            self.clone_escaping_borrowed_ref_chain_result(value, val, ref_chain_escapes);
        // B-track (pattern-arm unbound heap-field drop): same fresh-temp enum
        // scrutinee fix as `compile_if_let`. The `EnumDrop` registered here
        // drains at the enclosing scope's exit on the match edge (after the
        // escaped bindings), and at the divergent else edge's
        // `emit_scope_cleanup` walk on the miss edge (wholesale). Suppression
        // on the match edge zeroes the caps of moved-in fields.
        let freshtemp_enum =
            self.materialize_freshtemp_enum_scrutinee(value, pattern, val, did_clone_ref_enum);
        // Oversized-enum-payload §1/§2: free the heap box for a fresh-temp
        // boxed-payload scrutinee (box-only). Registers in the enclosing frame,
        // so it frees after the escaped bindings on the match edge and via the
        // divergent else edge's cleanup walk on the miss edge.
        let freshtemp_boxed_slot = if freshtemp_enum.is_none() {
            self.track_freshtemp_boxed_enum_scrutinee(value, &[pattern], val)
        } else {
            None
        };
        // Fresh-temp inline-`Result` / `Option[shared]` scrutinee — mirror the
        // match + if-let chain so `let Some(n) = st.pop() else { … }` over a
        // `Vec[shared T]` releases the popped node's transferred ref instead of
        // leaking it (B-2026-07-21-18).
        let freshtemp_inline_res = if freshtemp_enum.is_none() {
            self.track_freshtemp_inline_result_scrutinee(value, val)
        } else {
            None
        };
        if freshtemp_enum.is_none() && freshtemp_inline_res.is_none() {
            self.track_freshtemp_shared_option_scrutinee(value, &[pattern], val);
            // B-2026-08-28-74 — the bare `shared` enum sibling; mutually
            // exclusive with the line above by value shape (struct vs RC ptr).
            self.track_freshtemp_shared_enum_scrutinee(value, &[pattern], val);
        }
        let cond = self.compile_pattern_condition(pattern, val)?;

        let fn_val = self.current_fn.unwrap();
        let match_bb = self.context.append_basic_block(fn_val, "letelse.match");
        let else_bb = self.context.append_basic_block(fn_val, "letelse.else");

        self.builder
            .build_conditional_branch(cond.into_int_value(), match_bb, else_bb)
            .unwrap();

        // Else edge: the block diverges (typecheck-enforced). Compile it in
        // its own scope frame; the divergent exit's `emit_scope_cleanup`
        // walks that frame. Guard against a missing terminator defensively —
        // a well-typed program always terminates here.
        self.builder.position_at_end(else_bb);
        self.compile_block_with_frame(else_block)?;
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            self.builder.build_unreachable().unwrap();
        }

        // Match edge: bind into the current (enclosing) scope and fall
        // through. `val` is defined before the branch and dominates here.
        self.builder.position_at_end(match_bb);
        // Borrowed identifier scrutinee — see the if-let site (slice 3q).
        // Slice 3s adds the borrow-CALL half. A `let…else` binding escapes
        // into the enclosing scope by construction, so the payload clone is
        // unconditional (`escape_exprs = None`) — no arm to analyze.
        let saved_borrow_flag = self.pattern_state.pattern_binding_is_borrow;
        self.pattern_state.pattern_binding_is_borrow = self.pattern_state.pattern_binding_is_borrow
            || self.scrutinee_is_borrowed_binding(value)
            || self.scrutinee_is_borrow_call(value);
        // B-2026-07-31-45: record variant-payload binding names so a
        // Drop-declaring payload struct rides the UserDrop channel — the
        // match/if-let sites' mirror. The binding escapes into the
        // ENCLOSING scope, so its body fires at the binding's own NLL end
        // there, after its last use.
        self.pattern_state.current_variant_payload_bindings.clear();
        {
            let mut vp_names: Vec<String> = Vec::new();
            Self::collect_variant_payload_binding_names(pattern, false, &mut vp_names);
            self.pattern_state
                .current_variant_payload_bindings
                .extend(vp_names);
        }
        let saved_shape_flags =
            self.set_scrutinee_shape_flags_for_pattern(pattern, value, freshtemp_boxed_slot);
        let bind_res = self.bind_pattern_values(pattern, val);
        self.restore_scrutinee_shape_flags(saved_shape_flags);
        self.pattern_state.current_variant_payload_bindings.clear();
        bind_res?;
        if self.pattern_state.pattern_binding_is_borrow {
            self.clone_escaping_borrow_payload_binding(value, pattern, None, &[])?;
        }
        let optres_bindings_owned = !self.pattern_state.pattern_binding_is_borrow;
        self.pattern_state.pattern_binding_is_borrow = saved_borrow_flag;
        if let Some((alloca, enum_name)) = &freshtemp_enum {
            self.suppress_destructured_enum_payload_cleanup_at(*alloca, enum_name, pattern);
        } else if optres_bindings_owned {
            // B-2026-07-31-45 — the OWNED-VARIABLE disarm the match
            // (B-2026-07-23-13) and if-let sites already run: `let Full(r2)
            // = w else { … }` destructure-MOVES the payload into r2, but
            // w's own drop walk still read the populated payload words and
            // fired the body/frees a second time (before r2's own slot, and
            // while r2 is live). Zero the consumed fields so the walk skips
            // exactly what r2 now owns; the divergent else edge runs no
            // suppression and drops `w` whole.
            self.suppress_destructured_enum_payload_cleanup(value, pattern);
            // B-2026-08-29-33, `let … else` leg — see the `if let` note above.
            self.suppress_destructured_struct_field_enum_cleanup(value, pattern);
            // B-2026-08-31-30 — #16, the plain struct-pattern destructure,
            // which the `match` arm loop has run since it was added and these
            // three legs never did. `if let H { r, .. } = h { … }` therefore
            // left the source struct's field fully populated while the binding
            // owned the same buffer, and BOTH freed it: measured as
            // `free(): double free detected in tcache 2` on `karac build` and
            // on the JIT, against a clean `match` on the same value. Exactly
            // the shape, and exactly the omission, that B-2026-08-29-33 found
            // one level down for an enum-typed FIELD.
            //
            // Then/match edge only, like every suppression around it: the miss
            // edge runs none and the drop frees the source whole.
            self.suppress_destructured_struct_pattern_cleanup(value, pattern);
            // The BODIES half, moving in lockstep with the memory half above —
            // the B-2026-08-28-67 rule. Without it the source's bodies walk
            // still visits the moved-out field and runs its user `Drop` body a
            // second time on the husk the cap-zeroing just left
            // (B-2026-08-31-26).
            self.disarm_arm_destructured_struct_field_bodies(value, pattern);
        }
        // B-2026-06-10-6: variable inline-`Option` scrutinee — `s` binds into
        // the enclosing scope where x's `FreeInlineOptionPayload` also lives,
        // so zero x's source `cap` to avoid a double-free at that scope's exit.
        self.suppress_inline_option_payload_cleanup(value, pattern);
        self.suppress_inline_result_payload_cleanup(value, pattern);
        // B-2026-07-30-11 (Option/Result leg): the payload-BODIES action is
        // retracted alongside the memory suppressions above — same shape
        // gate, interp twin in `pattern_consumes_user_drop_payload`.
        self.suppress_optres_payload_bodies_for_match(value, pattern);
        // B-2026-08-05-3 (Option leg): a let-else binding escapes into the
        // enclosing scope, so it always takes the boxed tuple's interior.
        self.retract_boxed_tuple_inner_drop_for_block(value, pattern, None);
        // B-2026-07-21-16: `let Some(s) = a.opt else { … }` over an OWNED
        // place — zero the source field on the match edge (the escaped
        // binding owns the payload); the divergent else edge leaves it for
        // the struct drop.
        self.suppress_consumed_place_optres_field_source(value, pattern, optres_bindings_owned);
        // B-2026-07-22-2: fresh-temp sibling (`let Some(s) = mk().opt else`).
        self.consume_freshtemp_field_scrutinee(value, pattern, optres_bindings_owned);
        self.suppress_inline_option_map_payload_cleanup(value, pattern);
        self.suppress_inline_option_agg_payload_cleanup(value, pattern);
        // Slice 3t: boxed-payload struct-destructure field suppression — zero
        // the consumed fields inside the box so the binding owns them and the
        // box's inner walk frees only what the pattern left unbound.
        //
        // B-2026-09-01-10 — gated on `optres_bindings_owned`, the gate the
        // `match` spelling has always had and these three `let`-family paths
        // never did. See the `compile_if_let` site for what the ungated call
        // got wrong and how it was measured.
        if optres_bindings_owned {
            self.suppress_boxed_payload_struct_destructure(value, pattern);
            // B-2026-08-04-6 — the FRESH-TEMP twin: same per-field split,
            // against the box staged by `track_freshtemp_boxed_enum_scrutinee`
            // (no named variable exists for the expr-based entry point to
            // find). No-ops when the scrutinee is not a fresh-temp boxed one.
            self.suppress_freshtemp_boxed_payload_struct_destructure(freshtemp_boxed_slot, pattern);
        }
        // B-2026-07-21-8: ref-chain struct clone — per-field cap-zeroing
        // against the CLONE slot on the match edge (see the if-let site);
        // the else edge diverges with the clone's StructDrop firing
        // wholesale in its cleanup walk.
        if let Some((clone_ptr, clone_name)) = &refchain_struct_clone {
            let (clone_ptr, clone_name) = (*clone_ptr, clone_name.clone());
            self.suppress_destructured_struct_pattern_cleanup_at(clone_ptr, &clone_name, pattern);
        }
        // B-2026-07-21-9: ref-chain Option clone — consuming Some pattern
        // zeroes the clone's tag on the match edge; the divergent else edge
        // frees the clone's payload in its cleanup walk.
        if let Some(clone_slot) = refchain_option_clone {
            self.zero_refchain_option_clone_on_consume(clone_slot, pattern);
        }
        // B-2026-07-21-10: ref-chain tuple clone — match-edge element zero.
        if let Some((slot, agg_ty, ref elem_tes)) = refchain_tuple_clone {
            let elem_tes = elem_tes.clone();
            self.zero_refchain_tuple_clone_on_consume(slot, agg_ty, &elem_tes, pattern);
        }
        // B-2026-07-21-14: ref-chain Result clone — consuming Ok/Err pattern
        // zeroes the clone's payload area on the match edge; the divergent
        // else edge frees the clone's payload in its cleanup walk.
        if let Some(slot) = refchain_result_clone {
            self.suppress_inline_result_payload_cleanup_at(slot, pattern);
        }
        Ok(())
    }

    /// The libc `FILE*` for stdout (`to_stderr == false`) or stderr, as the
    /// `fwrite` stream argument. On glibc / wasi-libc / Apple this loads the
    /// `stdout` / `stderr` (`__stdoutp` / `__stderrp`) data global. The MSVC
    /// UCRT exposes **no such data symbol** — `<stdio.h>`'s `stdout` / `stderr`
    /// are macros over `__acrt_iob_func(n)` (1 = stdout, 2 = stderr) — so a
    /// Windows build emits that call instead. Without it the linked object
    /// carries an undefined `stdout` reference (`lld-link: error: undefined
    /// symbol: stdout`). Host-`cfg`'d, mirroring the `__stdoutp` Apple branch in
    /// `Codegen::new` — karac is built natively per target. Both arms are
    /// syntactically live so `stdout_global` is never "field never read" on
    /// Windows.
    pub(super) fn stdio_stream(&self, to_stderr: bool) -> inkwell::values::BasicValueEnum<'ctx> {
        self.stdio_stream_with(&self.builder, to_stderr)
    }

    /// [`Self::stdio_stream`] against an EXPLICIT builder.
    ///
    /// The stream is materialized by an instruction (a load of `stderr`, or a
    /// call to `__acrt_iob_func` on Windows), so it has to be emitted into the
    /// same function as its use. `emit_panic` outlines its body into a
    /// per-site `__karac_panic_site_<n>` function with its OWN builder, and
    /// emitting the load through `self.builder` there put it in the caller —
    /// "Instruction does not dominate all uses" at module verification
    /// (B-2026-08-23-17).
    pub(super) fn stdio_stream_with(
        &self,
        builder: &inkwell::builder::Builder<'ctx>,
        to_stderr: bool,
    ) -> inkwell::values::BasicValueEnum<'ctx> {
        let ptr_t = self.context.ptr_type(inkwell::AddressSpace::default());
        if cfg!(windows) {
            let i32_t = self.context.i32_type();
            let iob = self
                .module
                .get_function("__acrt_iob_func")
                .unwrap_or_else(|| {
                    self.module.add_function(
                        "__acrt_iob_func",
                        ptr_t.fn_type(&[i32_t.into()], false),
                        Some(inkwell::module::Linkage::External),
                    )
                });
            let idx = i32_t.const_int(if to_stderr { 2 } else { 1 }, false);
            builder
                .build_call(iob, &[idx.into()], "iob")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
        } else {
            let glob = if to_stderr {
                self.stderr_global
            } else {
                self.stdout_global
            };
            builder
                .build_load(ptr_t, glob.as_pointer_value(), "fw.stream")
                .unwrap()
        }
    }

    /// Write exactly `len` bytes of `data` to stdout (or stderr) via `fwrite`,
    /// followed by the newline `nl`. NUL-safe — unlike `printf("%.*s")`, which
    /// stops at the first interior NUL even with a precision set (so a
    /// length-prefixed String carrying `\0` would print truncated, L5).
    /// `fwrite` shares libc's stdio buffer with the `printf` int/bool print
    /// paths, so output ordering across mixed prints is preserved.
    pub(super) fn emit_nul_safe_write(
        &mut self,
        data: inkwell::values::PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        nl: &str,
        to_stderr: bool,
    ) {
        // `fwrite`'s size args are `size_t` — i32 on wasm32, i64 natively
        // (must match the extern declaration EXACTLY; wasm traps a mismatch).
        let size_t = if crate::target::active_target_is_wasm() {
            self.context.i32_type()
        } else {
            self.context.i64_type()
        };
        // Normalize the byte length to size_t (truncate a wider i64 on wasm,
        // widen a narrower count like the char-codepoint path's byte count).
        let len_st = {
            let cur = len.get_type().get_bit_width();
            let want = size_t.get_bit_width();
            if cur == want {
                len
            } else if cur > want {
                self.builder
                    .build_int_truncate(len, size_t, "fw.len.st")
                    .unwrap()
            } else {
                self.builder
                    .build_int_z_extend(len, size_t, "fw.len.st")
                    .unwrap()
            }
        };
        let stream = self.stdio_stream(to_stderr);
        // B-2026-07-30-9 — a trailing newline goes through the LINE wrapper, so
        // payload+newline reach the OS as ONE write. Emitting them as two calls
        // made `println` atomic only for its payload: the serializing lock
        // (glibc's per-`FILE` lock inside `fwrite`) is released in between, so
        // two `spawn`ed tasks printing concurrently produced payload-A,
        // payload-B, newline-A, newline-B — `12\n\n` for a program that says
        // `1\n2\n`. `print` (empty `nl`) keeps the single plain call below;
        // there is nothing to fuse and no behaviour to change.
        if !nl.is_empty() {
            let nl_g = self.builder.build_global_string_ptr(nl, "fw.nl").unwrap();
            let nl_len = size_t.const_int(nl.len() as u64, false);
            self.builder
                .build_call(
                    self.runtime_fns.write_console_line_fn,
                    &[
                        BasicMetadataValueEnum::from(data),
                        BasicMetadataValueEnum::from(len_st),
                        BasicMetadataValueEnum::from(nl_g.as_pointer_value()),
                        BasicMetadataValueEnum::from(nl_len),
                        BasicMetadataValueEnum::from(stream),
                    ],
                    "wcl",
                )
                .unwrap();
            return;
        }
        // Route through the runtime console chokepoint (auto-par ordered-
        // output): at the top level it `fwrite`s `len` bytes to `stream` (the
        // old inline behavior); inside a parallel branch it captures the bytes
        // for ordered replay at the join. `write_console` folds in the `1`
        // element-size, so only (data, len, stream) cross the call boundary.
        self.builder
            .build_call(
                self.runtime_fns.write_console_fn,
                &[
                    BasicMetadataValueEnum::from(data),
                    BasicMetadataValueEnum::from(len_st),
                    BasicMetadataValueEnum::from(stream),
                ],
                "wc",
            )
            .unwrap();
    }

    /// Write the owning String value to stdout (`to_stderr == false`) or
    /// stderr (`true`), append `nl`, then free its heap buffer. Used by the
    /// collection-Display print arms, which render into a throwaway
    /// accumulator and must release it inline (no scope-tracking — avoids
    /// per-call buffer accumulation in loops). The stderr arm backs both the
    /// `main() -> Result` `Err(e)` exit, whose `Error: {e}\n` rendering must
    /// land on stderr per design.md § Entry Point (B-2026-06-12-9), and every
    /// `eprintln` of a collection / Display value (B-2026-08-23-14).
    ///
    /// There is deliberately NO two-argument wrapper defaulting `to_stderr` to
    /// false. One existed, every `compile_print` arm used it, and that is
    /// exactly how `eprintln` came to write to stdout the moment it was given
    /// an intercept — the caller has to name its stream.
    pub(super) fn emit_write_and_free_string(
        &mut self,
        sval: BasicValueEnum<'ctx>,
        nl: &str,
        to_stderr: bool,
    ) {
        let sv = sval.into_struct_value();
        let data = self
            .builder
            .build_extract_value(sv, 0, "ps.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(sv, 1, "ps.len")
            .unwrap()
            .into_int_value();
        self.emit_nul_safe_write(data, len, nl, to_stderr);
        // Free only an OWNING String. The invariant (mirrored by the f-string
        // accumulator's scope-exit cleanup) is `cap == 0 ⇔ non-owning` — a
        // literal-backed String points its `data` at a read-only global and
        // carries `cap == 0`. Built-in display renderers always return an owned
        // (`cap > 0`) String, so this guard is a no-op for them; it is
        // load-bearing for a user `impl Display` whose `to_string` returns a
        // string literal (e.g. `match self { Red => "red", … }`), where an
        // unconditional `free` of the global aborts (SIGABRT). GAP-W4.
        let cap = self
            .builder
            .build_extract_value(sv, 2, "ps.cap")
            .unwrap()
            .into_int_value();
        let fn_val = self.current_fn.unwrap();
        let do_free = self.context.append_basic_block(fn_val, "ps.free");
        let after = self.context.append_basic_block(fn_val, "ps.after");
        let owns = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGT,
                cap,
                self.context.i64_type().const_zero(),
                "ps.owns",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(owns, do_free, after)
            .unwrap();
        self.builder.position_at_end(do_free);
        self.builder
            .build_call(self.runtime_fns.free_fn, &[data.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(after).unwrap();
        self.builder.position_at_end(after);
    }

    /// B-2026-08-14-30 — did this `println` operand PRODUCE the `Vec` it
    /// printed, or merely read one that something else owns?
    ///
    /// The materialize-and-render branch has to drop what it printed when the
    /// value was made for it and must not when it was borrowed, and "not a bare
    /// identifier" — the test it used — answers neither question. A PLACE
    /// expression is not an identifier: `b.xs`, `v[i]`, `t.0` and `*p` all read
    /// storage owned by something with its own cleanup, and the `Vec` they yield
    /// is that container's `{ptr, len, cap}` rather than a copy of it.
    ///
    /// So this enumerates the PRODUCERS instead, which is the short and stable
    /// list: a collection literal, and a call whose return is owned.
    /// `expr_yields_fresh_owned_temp` is the same predicate the `ref`-param
    /// materialization uses to decide this, and it already excludes a
    /// borrow-returning callee — a `ref Vec` return must not be freed either.
    /// Anything not on the list keeps its value un-dropped, which is the
    /// identifier path's behaviour and the safe direction: a missed drop is a
    /// leak the LSan corpus catches, an extra one is a double free in a user's
    /// program.
    ///
    /// B-2026-08-15-6 (Vec-element half) adds the one INDEX shape that produces
    /// rather than reads. The list above rules out `v[i]` as a place expression,
    /// and for a bound `v` that is exactly right — but an index into an inline
    /// `Vec` TEMPORARY (`mkrows()[1]`) is lowered by
    /// `compile_inline_temp_vec_index_ex`, which deep-clones the element (it
    /// drains the temp buffer straight after the read, so a borrowed element
    /// would dangle) and de-registers the synth local. The clone that reaches
    /// this consumer therefore has no container behind it and no cleanup of its
    /// own — the same producer the argument gate has admitted since
    /// B-2026-06-14-32. `expr_is_inline_temp_vec_heap_index` resolves through
    /// `inline_index_recv_vec_te`, which is the dispatch the index lowering
    /// itself uses, so this cannot fire on a shape lowered as a place read.
    pub(super) fn print_vec_operand_is_owned_temp(&self, expr: &Expr) -> bool {
        matches!(
            &expr.kind,
            ExprKind::ArrayLiteral(_)
                | ExprKind::PrefixCollectionLiteral { .. }
                | ExprKind::RepeatLiteral { .. }
        ) || self.expr_yields_fresh_owned_temp(expr)
            || self.expr_is_inline_temp_vec_heap_index(expr)
    }

    pub(super) fn compile_print(
        &mut self,
        name: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let zero = self.context.i64_type().const_int(0, false);
        // B-2026-08-23-14 — the stream is chosen ONCE here and threaded to
        // every write arm below, because the bug this replaced was a missing
        // arm, not a wrong one: `eprintln` reached no intercept at all and
        // fell through to the unknown-callee `i64 0` fallback, so a compiled
        // program's every stderr write vanished while `--interp` printed it.
        // Each arm takes the flag explicitly (there is deliberately no
        // stdout-defaulting convenience wrapper) so a print arm added later
        // has to decide which stream it writes to rather than inheriting
        // stdout by omission.
        let to_stderr = name == "eprintln";
        let nl = if matches!(name, "println" | "eprintln") {
            "\n"
        } else {
            ""
        };
        if args.is_empty() {
            // The zero-argument forms. This branch wrote a hard-coded "\n"
            // whatever the callee was, so a bare `print()` emitted a newline it
            // does not owe: `print("a"); print(); print("b")` compiled to `a\nb`
            // against the interpreter's `ab`. `println()` / `eprintln()` were
            // right only because a newline happens to be what THEY owe.
            //
            // Writing `nl` — already computed above as this form's terminator —
            // is the whole fix: it is "" for `print`, so nothing is emitted.
            // An early `return` on the empty case would be equally correct but
            // is redundant with this, and only one of the two can be the fix.
            // The zero-length call is kept rather than short-circuited so this
            // branch has exactly one exit shape and `nl` stays the single
            // source of truth for "does this form terminate a line".
            //
            // Routed through the console chokepoint (not `printf`) so a bare
            // `println()` inside a parallel branch is captured + ordered too.
            let nl_g = self.builder.build_global_string_ptr(nl, "nl").unwrap();
            self.emit_nul_safe_write(
                nl_g.as_pointer_value(),
                self.context.i64_type().const_int(nl.len() as u64, false),
                "",
                to_stderr,
            );
            return Ok(zero.into());
        }

        // Collection dispatch: when the print arg is a bare identifier that
        // we've registered as a Vec or Map variable, emit a call to the
        // per-type Display fn against the variable's alloca. This is the
        // primary path for `println(v)` on collections; it produces the same
        // formatted output the interpreter prints. Bare Vec/Map values appear
        // as struct/pointer values in the legacy `is_struct_value` /
        // `is_pointer_value` arms below — that path is wrong for collections
        // (Vec gets treated as String; Map gets printed as a raw address) —
        // but those arms are still reachable for non-identifier expressions
        // (function returns, fresh literals) where the source-level type is
        // not in the side-tables, so we leave them in place as fallbacks.
        if let ExprKind::Identifier(var_name) = &args[0].value.kind {
            // Vec[T]: side-table both `vec_elem_types` and `var_elem_type_exprs`
            // are set (the latter is what distinguishes a Vec variable from a
            // String variable, which only sets `vec_elem_types`).
            if self.var_types.vec_elem_types.contains_key(var_name)
                && self.var_types.var_elem_type_exprs.contains_key(var_name)
            {
                let elem_te = self.var_types.var_elem_type_exprs[var_name].clone();
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                let display_fn = self.emit_vec_display_fn_te(&elem_te);
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_write_and_free_string(sval, nl, to_stderr);
                return Ok(zero.into());
            }
            // Map[K, V]: side-tables hold both K and V `TypeExpr`s.
            if self.mapset.map_key_type_exprs.contains_key(var_name)
                && self.var_types.var_elem_type_exprs.contains_key(var_name)
            {
                let k_te = self.mapset.map_key_type_exprs[var_name].clone();
                let v_te = self.var_types.var_elem_type_exprs[var_name].clone();
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                // B-2026-08-14-35 — `SortedMap` populates the same registries;
                // route it to the ascending-order renderer, not `Map`'s.
                let display_fn = if self.mapset.sorted_collection_vars.contains(var_name) {
                    self.emit_sorted_map_display_fn(&k_te, &v_te)?
                } else {
                    self.emit_map_display_fn(&k_te, &v_te)
                };
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_write_and_free_string(sval, nl, to_stderr);
                return Ok(zero.into());
            }
            // Set[T]: side-table holds the element `TypeExpr`.
            if self.mapset.set_elem_type_exprs.contains_key(var_name) {
                let elem_te = self.mapset.set_elem_type_exprs[var_name].clone();
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                let display_fn = if self.mapset.sorted_collection_vars.contains(var_name) {
                    self.emit_sorted_set_display_fn(&elem_te)?
                } else {
                    self.emit_set_display_fn(&elem_te)
                };
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_write_and_free_string(sval, nl, to_stderr);
                return Ok(zero.into());
            }
            // B-2026-07-08-9: Option[T] — synthesize a `Some(<T>)`/`None`
            // renderer from the captured concrete payload type and print it,
            // matching the interpreter (which codegen previously couldn't).
            if let Some(payload_te) = self.var_types.var_option_payload_te.get(var_name).cloned() {
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                let display_fn = self.emit_option_display_te(&payload_te);
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_write_and_free_string(sval, nl, to_stderr);
                return Ok(zero.into());
            }
            // Result[T, E] sibling.
            if let Some((ok_te, err_te)) =
                self.var_types.var_result_payload_te.get(var_name).cloned()
            {
                let slot = self
                    .variables
                    .get(var_name)
                    .copied()
                    .ok_or_else(|| format!("compile_print: '{var_name}' not bound"))?;
                let display_fn = self.emit_result_display_te(&ok_te, &err_te);
                let (_acc, sval) = self.render_via_display_fn(display_fn, slot.ptr);
                self.emit_write_and_free_string(sval, nl, to_stderr);
                return Ok(zero.into());
            }
        }

        // Option/Result *call result* (`println(cache.get(1))`) — the variable
        // case is caught by the identifier arms above; this handles the
        // no-variable-name expr via the span-keyed payload table (spilling the
        // value to an alloca internally). B-2026-07-08-9 (call-result half).
        // Precedes the payload-enum / struct-value error arms, which explicitly
        // exclude the built-in Option/Result enums.
        if let Some((_acc, sval)) = self.try_compile_option_result_display(&args[0].value)? {
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }

        // Whole-`Vector[T, N]` arm — `println(v)` (B-2026-08-29-52). Without
        // it the call reached the value-kind arms below, which have no case
        // for an LLVM `<N x T>` and printed NOTHING AT ALL: the line vanished
        // from the output on both compiled backends while the interpreter
        // printed `Vector(1, 2, 3, 4)`. The f-string spelling of the same
        // thing had the louder failure (an address, or one stray lane); this
        // one was silent. Same renderer for both, so they cannot drift.
        if let Some((_acc, sval)) = self.try_compile_vector_display(&args[0].value)? {
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }

        // Whole-tuple arm — `println(t)` where `t: (i64, i64)` — render via the
        // element-wise tuple Display fn (`(a, b)`, matching the interpreter),
        // then print + free the owning buffer. Precedes the struct-value error
        // arms below, which would otherwise reject the anonymous tuple aggregate
        // (B-2026-07-18-14).
        if let Some((_acc, sval)) = self.try_compile_tuple_display(&args[0].value)? {
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }

        // Vec[T] with no variable name to key on — a fresh literal
        // (`println(vec![1, 2])`), a call result (`println(t.shape())`). Must
        // precede the value-kind arms below: a Vec's `{ptr, len, cap}`
        // aggregate is byte-identical to a String's, so those arms rendered it
        // as a String and printed garbage (B-2026-07-28-12). A materialized
        // temporary registers itself for scope cleanup inside the helper.
        // B-2026-08-31-19 — the array sibling. `println(a)` on an
        // `Array[i64, 3]` printed NOTHING before this: the value-kind arms
        // below have no array case at all, so the call fell through to the
        // unknown-callee return with no write emitted.
        if let Some((_acc, sval)) = self.try_compile_array_display(&args[0].value)? {
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }
        // B-2026-08-31-25 — the slice sibling. `println(s)` on a `Slice[i64]`
        // was a hard BUILD ERROR before this (the array shape misprinted; a
        // slice refused), where the interpreter prints `[1, 2]`.
        if let Some((_acc, sval)) = self.try_compile_slice_display(&args[0].value)? {
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }
        if let Some((_acc, sval)) = self.try_compile_vec_display(&args[0].value)? {
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }
        // B-2026-08-14-31 — the Map/Set sibling, for the same reason one step
        // over: a Map/Set reached through anything but a bound name printed its
        // CONTROL POINTER, because the value-kind arms see one pointer and have
        // nothing to distinguish it from any other.
        if let Some((_acc, sval)) = self.try_compile_map_or_set_display(&args[0].value)? {
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }

        // User `impl Display` (a compiled `<Type>.to_string`) wins over every
        // built-in renderer below — render `println(x)` via the user method,
        // matching `f"{x}"` / `x.to_string()` and the interpreter. The owning
        // String it returns is printed + freed. GAP-W4.
        if self.user_display_impl_type(&args[0].value).is_some() {
            let sval = self.compile_method_call(
                &args[0].value,
                "to_string",
                &[],
                &args[0].value.span,
                &args[0].value.span,
            )?;
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }

        // All-unit enum arm — render the bare variant name (selected on the
        // tag). Precedes the value-kind arms for the same reason as the struct
        // B-2026-07-28-12: a NON-identifier Vec operand — `println([9, 8])`,
        // `println(mk())`, `println(t.shape())`. The identifier arms above key
        // off per-variable side tables, so a fresh literal or a call result had
        // no entry and fell through to the value-kind arms at the bottom, where
        // a Vec's `{ptr,len,cap}` is indistinguishable from a String's and got
        // printed AS one: the element bytes came out as text (`[9, 8]` rendered
        // as 0x09 0x00) and a `\0` truncated it, so the line looked empty. The
        // old comment here called that arm a "fallback" for exactly this case;
        // it is not a fallback, it is a wrong answer.
        //
        // Recover the static type from the span-keyed record the front end
        // fills for every expression, falling back to the declared return type
        // of a call. Then materialize the value into a temp and render it with
        // the same per-element Display fn the identifier path uses, so both
        // spellings print identically and match the interpreter.
        if !matches!(&args[0].value.kind, ExprKind::Identifier(_)) {
            let sp = &args[0].value.span;
            let vec_te = self
                .type_decls
                .enum_inst_type_exprs
                .get(&(sp.offset, sp.length))
                .cloned()
                .or_else(|| self.inline_temp_vec_te(&args[0].value));
            if let Some(elem_te) = vec_te
                .as_ref()
                .and_then(super::helpers::vec_inner_type_expr)
            {
                let v = self.compile_expr(&args[0].value)?;
                if v.is_struct_value() {
                    let tmp = self
                        .builder
                        .build_alloca(v.get_type(), "print.vec.tmp")
                        .unwrap();
                    self.builder.build_store(tmp, v).unwrap();
                    let display_fn = self.emit_vec_display_fn_te(&elem_te);
                    let (_acc, sval) = self.render_via_display_fn(display_fn, tmp);
                    self.emit_write_and_free_string(sval, nl, to_stderr);
                    // Drop the temp only when this expression really PRODUCED
                    // the value — a fresh literal or a call result owns
                    // everything it made, and the identifier path leaves that
                    // to the binding's own cleanup. A DEEP drop, not just the
                    // buffer: a `Vec[String]` / `Vec[Vec[_]]` temp also owns
                    // each element's heap, which a buffer-only free strands
                    // (measured: 24 bytes over two `Vec[String]` prints).
                    //
                    // B-2026-08-14-30 — this used to drop UNCONDITIONALLY, on
                    // the reasoning that "not a bare identifier" means "a fresh
                    // temporary". It does not: a PLACE expression is not an
                    // identifier either, and a `Vec` read out of one is the
                    // container's own `{ptr, len, cap}`, not a copy. So
                    // `println(b.xs)` on a `struct B { xs: Vec[i64] }` freed the
                    // struct's buffer and the struct's own drop freed it again
                    // — a hard double free at runtime, on a shape as ordinary
                    // as printing a field. `Vec[String]` and nested `Vec` went
                    // further and SEGFAULTED, because the deep drop walked
                    // elements it did not own. The interpreter printed all of
                    // them correctly, so this was compiled-only.
                    if self.print_vec_operand_is_owned_temp(&args[0].value) {
                        let drop_fn = self.emit_vec_drop_fn(&elem_te);
                        self.builder.build_call(drop_fn, &[tmp.into()], "").unwrap();
                    }
                    return Ok(zero.into());
                }
            }
        }

        // arm below (an enum lowers to a tagged struct value).
        if let Some(ename) = self.expr_user_enum_name(&args[0].value) {
            let (data, len) = self.compile_unit_enum_display(&args[0].value, &ename)?;
            self.emit_nul_safe_write(data, len, nl, to_stderr);
            return Ok(zero.into());
        }

        // Payload-bearing user enum arm — render via its value-driven Display
        // fn (`Variant` / `Variant(f0, f1)` / `Variant { name: v }`), then
        // print + free the owning buffer.
        if let Some(ename) = self.expr_user_enum_name_any(&args[0].value) {
            let (_acc, sval) = self.render_user_enum_display(&args[0].value, &ename)?;
            self.emit_write_and_free_string(sval, nl, to_stderr);
            return Ok(zero.into());
        }

        // User-struct arm — `#[derive(Display)]` / `impl Display` structs
        // render as `TypeName { field: value, … }` in declaration order
        // (matching the interpreter). Render to an owning String via the
        // synthetic-f-string path, then print it NUL-safely. Must precede
        // the value-kind arms below: a user struct lowers to a struct value
        // that is NOT the 3-field String layout, so without this it would hit
        // the String / raw-pointer arm and ICE / print an address.
        if let Some(sname) = self.expr_user_struct_name(&args[0].value) {
            let s = self
                .compile_struct_display_string(&args[0].value, &sname)?
                .into_struct_value();
            let data = self
                .builder
                .build_extract_value(s, 0, "pd.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(s, 1, "pd.len")
                .unwrap()
                .into_int_value();
            self.emit_nul_safe_write(data, len, nl, to_stderr);
            return Ok(zero.into());
        }

        // Char arm — render as the UTF-8 glyph rather than the integer
        // codepoint. Must precede the generic int path because `char`
        // lowers to `i32` and would otherwise hit the `%lld` branch.
        // The detection covers literals (`println('A')`), char-typed
        // identifiers (`for c in s.chars() { println(c); }`,
        // `let c: char = 'A'; println(c);`), and Vec/Array indexed
        // reads (`println(chars[i])`).
        if self.expr_is_char(&args[0].value) {
            let val = self.compile_expr(&args[0].value)?;
            let (buf_ptr, byte_len) = self.emit_codepoint_to_utf8(val.into_int_value());
            // NUL-safe write: `'\0'` is a single 0x00 byte (byte_len 1) — a
            // `%.*s` print would emit nothing; `fwrite` emits the NUL (L5).
            self.emit_nul_safe_write(buf_ptr, byte_len, nl, to_stderr);
            return Ok(zero.into());
        }

        let val = self.compile_expr(&args[0].value)?;

        if val.is_int_value() {
            let bits = val.into_int_value().get_type().get_bit_width();
            if bits == 1 {
                // Select the literal + its length, then route through the
                // console chokepoint (capture-aware) instead of `printf` — `nl`
                // is appended by `emit_nul_safe_write`, not baked into the text.
                let true_s = self.builder.build_global_string_ptr("true", "ts").unwrap();
                let false_s = self.builder.build_global_string_ptr("false", "fs").unwrap();
                let i64_t = self.context.i64_type();
                let sel_ptr = self
                    .builder
                    .build_select(
                        val.into_int_value(),
                        true_s.as_pointer_value(),
                        false_s.as_pointer_value(),
                        "bstr",
                    )
                    .unwrap()
                    .into_pointer_value();
                let sel_len = self
                    .builder
                    .build_select(
                        val.into_int_value(),
                        i64_t.const_int(4, false),
                        i64_t.const_int(5, false),
                        "blen",
                    )
                    .unwrap()
                    .into_int_value();
                self.emit_nul_safe_write(sel_ptr, sel_len, nl, to_stderr);
            } else {
                // Widen narrower ints to i64 before printf's varargs slot —
                // sign-extend for signed types so a negative `i32` prints as
                // a signed decimal, zero-extend for unsigned types so a
                // large `u32` doesn't get sign-mistreated. Pre-fix this arm
                // passed the raw `i32` to `%lld`, which LLVM zero-padded
                // before the call and printf then read as a 64-bit signed
                // value — giving the unsigned-representation print on
                // negative narrow ints (e.g. `i32 -123` → `4294967173`).
                // Mirrors the per-type display dispatch in
                // [`synth_display::emit_primitive_display`].
                let int_val = val.into_int_value();
                let bits = int_val.get_type().get_bit_width();
                let i64_t = self.context.i64_type();
                let is_unsigned = self.expr_is_unsigned_int(&args[0].value);
                let widened = if bits < 64 {
                    if is_unsigned {
                        self.builder
                            .build_int_z_extend(int_val, i64_t, "print.zext")
                            .unwrap()
                    } else {
                        self.builder
                            .build_int_s_extend(int_val, i64_t, "print.sext")
                            .unwrap()
                    }
                } else {
                    int_val
                };
                // 128-bit goes through the runtime formatter before the
                // snprintf path below can truncate it: `%lld` reads 64 bits, so
                // an i128 loses its top half silently — `2^100` has an all-zero
                // low word and printed `0` (B-2026-08-19-8 stage 4). Same
                // routing the f-string path uses, so `println(x)` and
                // `println(f"{x}")` agree.
                if widened.get_type().get_bit_width() > 64 {
                    let (bp, blen) = self.format_i128_to_stack_buf(widened, is_unsigned);
                    self.emit_nul_safe_write(bp, blen, nl, to_stderr);
                    return Ok(self.context.i64_type().const_zero().into());
                }
                // Render into a stack buffer via `snprintf`, then route the
                // exact bytes through the console chokepoint (capture-aware)
                // instead of `printf` — so an int `println` inside a parallel
                // branch is captured + flushed in order. 32 bytes covers any
                // i64 (≤20 digits + sign + NUL). `nl` is appended by the write.
                let spec = if is_unsigned { "%llu" } else { "%lld" };
                let fmt = self.builder.build_global_string_ptr(spec, "fi").unwrap();
                let ptr_t = self.context.ptr_type(inkwell::AddressSpace::default());
                let size_t = if crate::target::active_target_is_wasm() {
                    self.context.i32_type()
                } else {
                    self.context.i64_type()
                };
                let fn_val = self.current_fn.unwrap();
                let buf = self.create_entry_alloca(
                    fn_val,
                    "ibuf",
                    self.context.i8_type().array_type(32).into(),
                );
                let buf_ptr = self
                    .builder
                    .build_pointer_cast(buf, ptr_t, "ibufp")
                    .unwrap();
                let written = self
                    .builder
                    .build_call(
                        self.runtime_fns.snprintf_fn,
                        &[
                            buf_ptr.into(),
                            size_t.const_int(32, false).into(),
                            fmt.as_pointer_value().into(),
                            widened.into(),
                        ],
                        "iwritten",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                self.emit_nul_safe_write(buf_ptr, written, nl, to_stderr);
            }
        } else if val.is_struct_value() {
            // A user struct that reached here is a `println(StructLiteral{…})`
            // / `println(make())` argument — the declaration-order struct
            // Display arm above only fires for place-expression args
            // (identifier / field access). Emit a clean error rather than
            // mis-reading the struct as the String `{ptr,i64,i64}` layout
            // below (which would extract a non-pointer field and ICE).
            if !self.llvm_ty_is_vec_struct(val.into_struct_value().get_type().into()) {
                return Err(self.deferred_display_error(&args[0].value, true));
            }
            // String struct `{ ptr, i64, i64 }` (data, len, cap). An earlier
            // fix moved this off a bare `%s` (which puts/printf treats as a
            // NUL-terminated C string and walks past a non-terminated heap
            // buffer — an ASAN 1-byte heap-buffer-overflow) onto `%.*s` with
            // the explicit `len`. But `%.*s` ALSO stops at an interior NUL
            // even with a precision, so a String carrying `\0` printed
            // truncated. `emit_nul_safe_write` lowers to `fwrite`, which
            // writes exactly `len` bytes regardless of NULs (L5) and still
            // never reads past the buffer.
            let sv = val.into_struct_value();
            let str_ptr = self
                .builder
                .build_extract_value(sv, 0, "str.ptr")
                .unwrap()
                .into_pointer_value();
            let str_len = self
                .builder
                .build_extract_value(sv, 1, "str.len")
                .unwrap()
                .into_int_value();
            self.emit_nul_safe_write(str_ptr, str_len, nl, to_stderr);
            // #20: a fresh-owned String temp passed directly to `println` /
            // `print` (`println(i.to_string())`, `print(a + b)`) has no
            // consuming binding, so its heap buffer would leak once per call —
            // unbounded in a loop. `free_fresh_owned_str_arg` is Call/MethodCall-
            // only and `cap > 0`-guarded, so a place expression (identifier /
            // field — owned by its binding) or a rodata literal is left
            // untouched (no double-free). The builder is already at the
            // post-write merge block, so every byte read dominates the free.
            // `rhs_stages_fstr_acc` excludes a struct/enum `.to_string()`: it
            // lowers via the synthetic f-string whose accumulator already owns a
            // scope-exit cleanup, so freeing here too would double-free (a
            // scalar/`String` `.to_string()` does NOT stage the acc and is still
            // freed). A direct f-string arg is an `InterpolatedStringLit`, not a
            // Call/MethodCall, so it is excluded upstream by
            // `expr_yields_fresh_owned_temp` regardless.
            if !self.rhs_stages_fstr_acc(&args[0].value) {
                self.free_fresh_owned_str_arg(&args[0].value, val);
            }
        } else if val.is_pointer_value() {
            // Raw pointer treated as a NUL-terminated C string (shared types,
            // etc.): measure with `strlen`, then route the bytes through the
            // console chokepoint (capture-aware) instead of `printf("%s")` —
            // `nl` is appended by the write.
            let strlen_fn = self
                .module
                .get_function("strlen")
                .expect("strlen declared in Codegen::new");
            let slen = self
                .builder
                .build_call(strlen_fn, &[val.into_pointer_value().into()], "p.slen")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.emit_nul_safe_write(val.into_pointer_value(), slen, nl, to_stderr);
        } else if val.is_float_value() {
            // Render with Rust's shortest-round-trip `{}` formatting (via the
            // runtime `karac_runtime_f64_to_str`) so AOT output matches
            // `karac run` exactly — not C `printf`'s `%g` (6 significant
            // figures, lowercase `nan`). `format_f64_to_stack_buf` widens
            // f32→f64 and returns `(buf_ptr, len)`; written NUL-safely with
            // the trailing newline (`nl` is "" for `print`). Float text never
            // carries a NUL, but routing it through the same `fwrite` path
            // keeps the print surface uniform (and buffer-shared with printf).
            let (buf_ptr, len) = self.format_f64_to_stack_buf(val.into_float_value());
            self.emit_nul_safe_write(buf_ptr, len, nl, to_stderr);
        }
        Ok(zero.into())
    }

    // ── Control flow ──────────────────────────────────────────────

    /// B-2026-08-30-2 — does EVERY arm of this `if` mint a fresh owned temp?
    ///
    /// The `If` arm of `branch_tail_mints_fresh_owned_temp`, re-expressed over
    /// the pieces `compile_if` is handed rather than the node it never sees —
    /// including the no-`else` case, which yields a placeholder at the merge
    /// and so mints nothing. True means B-2026-08-29-27's widened consuming
    /// gates will free the merged value at the use site, and an arm-level owner
    /// must therefore stand down.
    fn if_arms_all_mint(&self, then_block: &Block, else_branch: Option<&Expr>) -> bool {
        let (Some(eb), Some(then_tail)) = (else_branch, then_block.final_expr.as_deref()) else {
            return false;
        };
        self.branch_tail_mints_fresh_owned_temp(then_tail)
            && self.branch_tail_mints_fresh_owned_temp(eb)
    }

    pub(super) fn compile_if(
        &mut self,
        condition: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let tail = self.fn_ctx.tail_ret_inner.take();
        // Keyed on the condition's span, so compile ORDER is irrelevant.
        let own_value = self.branch_value_is_owned(condition);
        // B-2026-08-29-25 — does the DISCARD STATEMENT site take ownership of
        // this `if`'s value? If it does, the arm-level owner below must stand
        // down: both would free the same buffer, measured as a double free on
        // B-2026-08-29-5's `[if-fresh-in-both-arms]` fixture. Leaving the flag
        // clear also restores the ordinary tail SUPPRESSION for these arms,
        // which is exactly right — the statement frame is the consumer, and
        // it is how the `match` spelling has always worked.
        //
        // The two gates partition the shape rather than overlapping: the
        // statement site declines a no-`else` branch (the merge yields a
        // placeholder, so it never sees the value) and an arm tail naming an
        // existing binding, which are precisely the two populations
        // B-2026-08-29-5 added the arm-level path for.
        let discard_stmt_owns_value =
            self.discarded_if_parts_qualify(then_block, else_branch) && !own_value;
        let cond_val = self.compile_expr(condition)?.into_int_value();
        // B-2026-08-28-44 — clear any takeover signal left by an unrelated
        // expression compiled before this branch; only what THIS branch's arms
        // hand out may arm the merge-point owner.
        self.branch_arm_clone_taken = None;
        let fn_val = self.current_fn.unwrap();
        let then_bb = self.context.append_basic_block(fn_val, "then");
        let else_bb = self.context.append_basic_block(fn_val, "else");
        let merge_bb = self.context.append_basic_block(fn_val, "ifmerge");

        self.builder
            .build_conditional_branch(cond_val, then_bb, else_bb)
            .unwrap();
        // B-2026-08-30-2 — the block that dominates both arms and re-executes
        // on every pass through this `if`; see `arm_tail_owner_ctx`.
        let pre_branch_bb = self.builder.get_insert_block().unwrap();

        self.builder.position_at_end(then_bb);
        self.fn_ctx.tail_ret_inner = tail;
        // B-2026-08-29-5 — see `branch_arm_value_discarded`: this arm's tail
        // hands its value to nobody when the `if`'s own value is discarded.
        self.branch_arm_value_discarded = !own_value && !discard_stmt_owns_value;
        // B-2026-08-30-2 — the two things an arm cannot work out for itself;
        // see `arm_tail_owner_ctx`. Skipped entirely when the value is
        // discarded, which B-2026-08-29-5 already owns end to end.
        let arms_all_mint = self.if_arms_all_mint(then_block, else_branch);
        if own_value || discard_stmt_owns_value {
            self.arm_tail_owner_ctx = self
                .current_branch_expr_span
                .map(|span| (span, arms_all_mint, pre_branch_bb));
        }
        let mut then_val = self.compile_block_with_frame(then_block)?;
        // B-2026-08-30-2 — what this arm reported; registered a few lines
        // below, once the deep-copy has settled which value escapes.
        let then_pending = self.arm_pending_tail_owner.take();
        let then_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        // Deep-copy an owned-param branch tail (the caller retains the param's
        // buffer) so the `if`-value owns an independent buffer — the branch's
        // move-suppression alone leaves it aliasing the caller's arg and the
        // consumer double-frees. Emit AFTER the frame drained (inside
        // `compile_block_with_frame`) and BEFORE the jump to merge, so the copy
        // lands in this branch's predecessor and its blocks are captured by
        // `then_end_bb` below. No-op for local/non-param tails.
        if !then_terminated {
            if let (Some(fe), Some(v)) = (then_block.final_expr.as_deref(), then_val) {
                then_val = Some(self.deepcopy_owned_param_branch_tail(fe, v, own_value)?);
            }
            if let (Some(span), Some(v)) = (self.current_branch_expr_span, then_val) {
                self.register_pending_arm_owner(then_pending, v, span, Some(pre_branch_bb));
            }
        }
        let then_end_bb = self.builder.get_insert_block().unwrap();
        if !then_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(else_bb);
        // Leaf tail of the else branch, for the owned-param deep-copy below. A
        // nested `else if` recurses through `compile_if` (which deep-copies its
        // own leaves), so it contributes no leaf here.
        let else_tail: Option<&Expr>;
        let mut else_pending = None;
        let mut else_val = if let Some(else_expr) = else_branch {
            self.fn_ctx.tail_ret_inner = tail;
            match &else_expr.kind {
                ExprKind::Block(blk) => {
                    else_tail = blk.final_expr.as_deref();
                    self.branch_arm_value_discarded = !own_value && !discard_stmt_owns_value;
                    if own_value || discard_stmt_owns_value {
                        self.arm_tail_owner_ctx = self
                            .current_branch_expr_span
                            .map(|span| (span, arms_all_mint, pre_branch_bb));
                    }
                    let v = self.compile_block_with_frame(blk)?;
                    else_pending = self.arm_pending_tail_owner.take();
                    v
                }
                ExprKind::If {
                    condition: c,
                    then_block: tb,
                    else_branch: eb,
                } => {
                    else_tail = None;
                    let v = self.compile_if(c, tb, eb.as_deref())?;
                    Some(v)
                }
                _ => {
                    else_tail = Some(else_expr);
                    let v = self.compile_expr(else_expr)?;
                    Some(v)
                }
            }
        } else {
            else_tail = None;
            None
        };
        self.fn_ctx.tail_ret_inner = None;
        let else_terminated = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !else_terminated {
            if let (Some(fe), Some(v)) = (else_tail, else_val) {
                else_val = Some(self.deepcopy_owned_param_branch_tail(fe, v, own_value)?);
            }
            if let (Some(span), Some(v)) = (self.current_branch_expr_span, else_val) {
                self.register_pending_arm_owner(else_pending, v, span, Some(pre_branch_bb));
            }
        }
        let else_end_bb = self.builder.get_insert_block().unwrap();
        if !else_terminated {
            self.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        self.builder.position_at_end(merge_bb);

        let placeholder = self.context.i64_type().const_int(0, false).into();
        match (then_terminated, else_terminated) {
            // Both arms diverge (`return` / `unreachable()` / `todo()` on each
            // side): the merge block has no predecessors. Terminate it with
            // `unreachable` so the enclosing terminator guards (the fn-tail
            // `ret` in `compile_function`, `compile_block` between statements)
            // skip emitting a follow-on instruction — otherwise a
            // value-returning fn whose `if` both-diverges would emit
            // `ret <i64 placeholder>` against its real return type and fail
            // module verification (the gap-d failure class for branchy tails).
            (true, true) => {
                self.builder.build_unreachable().unwrap();
                Ok(placeholder)
            }
            // Exactly one arm diverges: the merge has a single live
            // predecessor, so the `if`-expression's value IS the live arm's
            // value (it dominates the merge — no phi needed). This is what
            // makes `if c { v } else { unreachable() }` evaluate to `v`
            // rather than the const-0 placeholder. `unwrap_or` covers the
            // value-less arm (e.g. a terminated `then` with no `else`).
            (true, false) => Ok(else_val.unwrap_or(placeholder)),
            (false, true) => Ok(then_val.unwrap_or(placeholder)),
            // Neither arm diverges: phi over both when the value types agree;
            // otherwise the `if` is in statement position (unit value) — fall
            // back to the const-0 placeholder.
            (false, false) => {
                if let (Some(tv), Some(ev)) = (then_val, else_val) {
                    let (tv, ev) = self.unify_int_branch_widths(tv, then_end_bb, ev, else_end_bb);
                    let (tv, ev) = self.unify_float_branch_widths(tv, then_end_bb, ev, else_end_bb);
                    // B-2026-08-30-49 — the MIXED pair the two above both pass
                    // through. Runs last: they settle widths within a kind,
                    // this one crosses kinds, so it sees each side already at
                    // the width it will keep.
                    let (tv, ev) = self.unify_int_float_branch_values(
                        tv,
                        self.branch_tail_is_unsigned_int(then_block.final_expr.as_deref()),
                        then_end_bb,
                        ev,
                        self.branch_tail_is_unsigned_int(else_tail),
                        else_end_bb,
                    );
                    if tv.get_type() == ev.get_type() {
                        let phi = self.builder.build_phi(tv.get_type(), "ifval").unwrap();
                        phi.add_incoming(&[(&tv, then_end_bb), (&ev, else_end_bb)]);
                        let merged = phi.as_basic_value();
                        // B-2026-08-28-44 — own what an arm tail handed out.
                        if let Some(elem_ty) = self.branch_arm_clone_taken.take() {
                            self.own_branch_merged_clone(merged, elem_ty);
                        }
                        return Ok(merged);
                    }
                }
                Ok(placeholder)
            }
        }
    }

    /// Reconcile the LLVM int widths of an `if`/`if let`'s two branch values
    /// before the phi. The typechecker has ALREADY unified both branches to one
    /// Kāra type, so any LLVM width mismatch reachable here is a codegen
    /// representation artifact of that single type — never two genuinely
    /// different types. Two such artifacts both surface as a wide branch beside
    /// a narrow one:
    ///
    /// - a suffixless integer literal (`0`) lowers at the default `i64` width
    ///   (`const_int_for_suffix` keys off the suffix only), while the sibling
    ///   carries its real narrower type (`u8`, …) — `{ 0 } else { byte }`;
    /// - a narrow-int arithmetic expr keeps the i64 it is computed at
    ///   (`compile_narrow_int_binop` range-checks to the declared width but
    ///   leaves the value wide for boundary coercion — see its doc), while the
    ///   sibling is the bare narrow value — `if upper { b + 32 } else { b }`,
    ///   the ASCII case-fold surface.
    ///
    /// Either way the wider side's meaningful bits fit the narrower width (same
    /// Kāra type), so truncating it down is value-preserving and makes the phi's
    /// operands agree. The truncate is emitted in the *wider branch's
    /// predecessor* (before its terminating branch to the merge block) so the
    /// phi operand dominates its incoming edge; a const folds with no
    /// instruction, so its position is immaterial. The builder is restored to
    /// the caller's insert block (the merge).
    ///
    /// Without this, the merge falls through to the const-`0` placeholder and
    /// the WHOLE construct evaluates to `0` — originally self-hosting #7 (the
    /// lexer's `fn peek(ref self) -> u8 { if … { 0 } else { … } }` always
    /// returned 0, so every scan loop exited immediately); then the
    /// arithmetic-branch case (`to_lower` / case-fold), which the earlier
    /// `is_const()`-gated version still mis-lowered because it assumed a
    /// non-constant wide branch was typechecker-impossible — narrow-int
    /// arithmetic makes it routine.
    fn unify_int_branch_widths(
        &self,
        a: BasicValueEnum<'ctx>,
        a_pred: BasicBlock<'ctx>,
        b: BasicValueEnum<'ctx>,
        b_pred: BasicBlock<'ctx>,
    ) -> (BasicValueEnum<'ctx>, BasicValueEnum<'ctx>) {
        let (BasicValueEnum::IntValue(av), BasicValueEnum::IntValue(bv)) = (a, b) else {
            return (a, b);
        };
        let (aw, bw) = (av.get_type().get_bit_width(), bv.get_type().get_bit_width());
        if aw > bw {
            (
                self.truncate_branch_value_in_pred(av, bv.get_type(), a_pred),
                b,
            )
        } else if bw > aw {
            (
                a,
                self.truncate_branch_value_in_pred(bv, av.get_type(), b_pred),
            )
        } else {
            (a, b)
        }
    }

    /// Truncate a phi-bound integer branch value down to `target`, emitting the
    /// `trunc` at the END of its predecessor block (before that block's
    /// terminating branch to the merge) so the result dominates the phi's
    /// incoming edge — a `trunc` in the merge block itself would not. A
    /// compile-time constant folds with no instruction emitted, so the
    /// repositioning is a harmless no-op for it. The builder's insert position
    /// is saved and restored, so the caller (positioned at the merge block)
    /// sees no change. Shared by the `if` / `if let` two-arm merge and the
    /// `match` N-arm merge (`unify_int_match_arm_widths`).
    fn truncate_branch_value_in_pred(
        &self,
        v: IntValue<'ctx>,
        target: IntType<'ctx>,
        pred: BasicBlock<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let resume = self.builder.get_insert_block();
        match pred.get_terminator() {
            Some(term) => self.builder.position_before(&term),
            None => self.builder.position_at_end(pred),
        }
        let t = self
            .builder
            .build_int_truncate(v, target, "ifw.trunc")
            .unwrap();
        if let Some(bb) = resume {
            self.builder.position_at_end(bb);
        }
        t.into()
    }

    /// Float sibling of [`unify_int_branch_widths`]. A float literal (`0.0`)
    /// lowers at the default `f64` width; when the sibling branch is `f32` (a
    /// param / typed expr), the phi's operand types disagree and the whole `if`
    /// falls through to the `i64 0` placeholder — an `f32`-returning fn whose
    /// body is `if c { x } else { 0.0 }` then emits `ret i64 0` against `float`
    /// and fails module verification. The typechecker has unified both branches
    /// to one Kāra float type, so the `f64` side is always the literal artifact;
    /// truncate it to the sibling's `f32` (an `f64`-typed fn never hits the
    /// mismatch — its literals are already `f64`). Value-preserving, same
    /// rationale as the integer case.
    fn unify_float_branch_widths(
        &self,
        a: BasicValueEnum<'ctx>,
        a_pred: BasicBlock<'ctx>,
        b: BasicValueEnum<'ctx>,
        b_pred: BasicBlock<'ctx>,
    ) -> (BasicValueEnum<'ctx>, BasicValueEnum<'ctx>) {
        let (BasicValueEnum::FloatValue(av), BasicValueEnum::FloatValue(bv)) = (a, b) else {
            return (a, b);
        };
        let f64_t = self.context.f64_type();
        let f32_t = self.context.f32_type();
        let (a_is64, b_is64) = (av.get_type() == f64_t, bv.get_type() == f64_t);
        if a_is64 && !b_is64 {
            (self.fptrunc_branch_value_in_pred(av, f32_t, a_pred), b)
        } else if b_is64 && !a_is64 {
            (a, self.fptrunc_branch_value_in_pred(bv, f32_t, b_pred))
        } else {
            (a, b)
        }
    }

    /// Float sibling of [`truncate_branch_value_in_pred`]: `fptrunc` a phi-bound
    /// `f64` branch value down to `target` (`f32`) at the end of its predecessor,
    /// so the result dominates the phi's incoming edge.
    fn fptrunc_branch_value_in_pred(
        &self,
        v: inkwell::values::FloatValue<'ctx>,
        target: inkwell::types::FloatType<'ctx>,
        pred: BasicBlock<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let resume = self.builder.get_insert_block();
        match pred.get_terminator() {
            Some(term) => self.builder.position_before(&term),
            None => self.builder.position_at_end(pred),
        }
        let t = self
            .builder
            .build_float_trunc(v, target, "ifw.fptrunc")
            .unwrap();
        if let Some(bb) = resume {
            self.builder.position_at_end(bb);
        }
        t.into()
    }

    /// Harmonize the LLVM int widths of a `match`'s arm values before the phi —
    /// the N-arm analog of [`unify_int_branch_widths`]. Same invariant (the
    /// typechecker unified every arm to one Kāra type) and same artifact
    /// (suffixless literals / narrow-int arithmetic leave some arms i64 beside
    /// narrow siblings). Truncates every arm value wider than the narrowest to
    /// that narrowest width, each in its own predecessor block. Arms that are
    /// not integers, or already share the minimum width, pass through untouched.
    pub(super) fn unify_int_match_arm_widths(
        &self,
        arms: &mut [(BasicValueEnum<'ctx>, BasicBlock<'ctx>)],
    ) {
        let min_width = arms
            .iter()
            .filter_map(|(v, _)| match v {
                BasicValueEnum::IntValue(iv) => Some(iv.get_type().get_bit_width()),
                _ => None,
            })
            .min();
        let Some(min_width) = min_width else {
            return;
        };
        let target = match min_width {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            128 => self.context.i128_type(),
            _ => self.context.i64_type(),
        };
        for (v, bb) in arms.iter_mut() {
            if let BasicValueEnum::IntValue(iv) = v {
                if iv.get_type().get_bit_width() > min_width {
                    *v = self.truncate_branch_value_in_pred(*iv, target, *bb);
                }
            }
        }
    }

    /// Harmonize the LLVM float widths of a `match`'s arm values before the phi —
    /// the float sibling of [`unify_int_match_arm_widths`]. When arms mix float
    /// widths (`match n { I(x) => x as f64, F(f) => f.value }` — an `f64` arm
    /// beside an `f32` from an `F32.value` payload field), the phi's `all-same-
    /// type` check fails and the whole `match` falls to the `i64 0` placeholder
    /// (`ret i64 0` against a `double` return — B-2026-07-23-2). The typechecker
    /// already unified every arm to one Kāra float type, and float widening
    /// (`fpext`) is EXACT — so promote every narrower float arm UP to the widest
    /// present, in its own predecessor block. Widen (not truncate, unlike the
    /// int case): a genuine `f64` arm truncated to `f32` would lose precision,
    /// and any residual mismatch against a *narrower* declared return type is
    /// re-narrowed losslessly by the fn-tail `coerce_to_current_ret_type`
    /// (`f32 -> f64 -> f32` round-trips exact). Arms that are not floats, or
    /// already at the max width, pass through untouched. Uses
    /// `build_float_cast_bf16_safe` so an `f16`/`bf16` arm widens through the
    /// aarch64-selectable integer path, never a bare `fpext bfloat`.
    pub(super) fn unify_float_match_arm_widths(
        &self,
        arms: &mut [(BasicValueEnum<'ctx>, BasicBlock<'ctx>)],
    ) {
        let target = arms
            .iter()
            .filter_map(|(v, _)| match v {
                BasicValueEnum::FloatValue(fv) => Some(fv.get_type()),
                _ => None,
            })
            .max_by_key(|ft| self.float_bits_int_type(*ft).get_bit_width());
        let Some(target) = target else {
            return;
        };
        for (v, bb) in arms.iter_mut() {
            if let BasicValueEnum::FloatValue(fv) = v {
                if fv.get_type() != target {
                    *v = self.floatcast_branch_value_in_pred(*fv, target, *bb);
                }
            }
        }
    }

    /// Cast a phi-bound float branch value to `target` (widening or narrowing) at
    /// the end of its predecessor block — the width-agnostic sibling of
    /// [`fptrunc_branch_value_in_pred`], routed through `build_float_cast_bf16_safe`
    /// so `bf16` legs never emit an unselectable `fpext bfloat`. Positioned in the
    /// predecessor so the result dominates the phi's incoming edge.
    fn floatcast_branch_value_in_pred(
        &self,
        v: inkwell::values::FloatValue<'ctx>,
        target: inkwell::types::FloatType<'ctx>,
        pred: BasicBlock<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let resume = self.builder.get_insert_block();
        match pred.get_terminator() {
            Some(term) => self.builder.position_before(&term),
            None => self.builder.position_at_end(pred),
        }
        let t = self.build_float_cast_bf16_safe(v, target, "mfw.fcast");
        if let Some(bb) = resume {
            self.builder.position_at_end(bb);
        }
        t.into()
    }

    /// Peel a branch / arm tail down to the expression that actually produces
    /// the value, so a signedness probe sees `u` rather than the block wrapping
    /// it — `if c { { u } } else { 0.0 }` and a block-bodied `match` arm both
    /// arrive here as `ExprKind::Block`.
    fn branch_tail_value_expr(e: &Expr) -> &Expr {
        match &e.kind {
            ExprKind::Block(b) => match b.final_expr.as_deref() {
                Some(inner) => Self::branch_tail_value_expr(inner),
                None => e,
            },
            _ => e,
        }
    }

    /// Signedness of a branch / arm tail, for the int→float conversions below:
    /// `uitofp` on a `u64` above `i64::MAX` and `sitofp` on the same bits give
    /// different answers, and only the source expression knows which is meant.
    /// Absent a tail expression the signed reading is the safe default — it is
    /// what every other int→float site in codegen falls back to.
    pub(super) fn branch_tail_is_unsigned_int(&self, e: Option<&Expr>) -> bool {
        e.is_some_and(|e| self.expr_is_unsigned_int(Self::branch_tail_value_expr(e)))
    }

    /// Reconcile a MIXED int/float `if` / `if let` arm pair before the phi —
    /// the cross-KIND sibling of [`unify_int_branch_widths`] and
    /// [`unify_float_branch_widths`], which each reconcile WIDTHS within one
    /// kind and pass a mixed pair straight through. Nothing used to close that
    /// gap, so `let a: f64 = if c { n } else { 0.0 }` with `n: i64` reached the
    /// all-same-type check as `i64` beside `double`, failed it, and fell
    /// through to the const-`i64 0` placeholder: the whole construct evaluated
    /// to `0` on both compiled backends, at every float width, for any value,
    /// with no diagnostic — while `--interp` and the `let d: f64 = n` control
    /// were both correct (B-2026-08-30-49).
    ///
    /// The direction is forced, not chosen. A mixed pair can only arrive here
    /// having ALREADY been unified by the typechecker to one Kāra type, and an
    /// integer arm beside a float arm unifies to the FLOAT — Kāra widens
    /// int→float at an annotation boundary and never narrows float→int
    /// implicitly. So the int side is the one that converts, and converting it
    /// is value-preserving in exactly the sense the sibling helpers rely on.
    ///
    /// Emitted in the int arm's own predecessor (like every helper here) so the
    /// result dominates the phi's incoming edge, and routed through
    /// `build_int_to_float_bf16_safe` so a `bf16` target never emits an
    /// aarch64-unselectable `sitofp … to bfloat`.
    fn unify_int_float_branch_values(
        &self,
        a: BasicValueEnum<'ctx>,
        a_unsigned: bool,
        a_pred: BasicBlock<'ctx>,
        b: BasicValueEnum<'ctx>,
        b_unsigned: bool,
        b_pred: BasicBlock<'ctx>,
    ) -> (BasicValueEnum<'ctx>, BasicValueEnum<'ctx>) {
        match (a, b) {
            (BasicValueEnum::IntValue(av), BasicValueEnum::FloatValue(bv)) => (
                self.int_to_float_branch_value_in_pred(av, bv.get_type(), a_unsigned, a_pred),
                b,
            ),
            (BasicValueEnum::FloatValue(av), BasicValueEnum::IntValue(bv)) => (
                a,
                self.int_to_float_branch_value_in_pred(bv, av.get_type(), b_unsigned, b_pred),
            ),
            _ => (a, b),
        }
    }

    /// N-arm analog of [`unify_int_float_branch_values`] for `match`. Runs
    /// AFTER [`unify_float_match_arm_widths`] has settled the float arms on one
    /// width, so the target here is simply whatever float type the arms now
    /// share; every integer arm converts to it, each in its own predecessor.
    /// A `match` with no float arm at all returns early and is untouched, which
    /// is what keeps an all-integer `match` on its existing path.
    pub(super) fn unify_int_float_match_arm_values(
        &self,
        arms: &mut [(BasicValueEnum<'ctx>, BasicBlock<'ctx>)],
        arm_unsigned: &[bool],
    ) {
        let target = arms.iter().find_map(|(v, _)| match v {
            BasicValueEnum::FloatValue(fv) => Some(fv.get_type()),
            _ => None,
        });
        let Some(target) = target else {
            return;
        };
        for (i, (v, bb)) in arms.iter_mut().enumerate() {
            if let BasicValueEnum::IntValue(iv) = v {
                let unsigned = arm_unsigned.get(i).copied().unwrap_or(false);
                *v = self.int_to_float_branch_value_in_pred(*iv, target, unsigned, *bb);
            }
        }
    }

    /// Int→float sibling of [`truncate_branch_value_in_pred`] /
    /// [`floatcast_branch_value_in_pred`]: convert a phi-bound integer branch
    /// value to `target` at the END of its predecessor block (before that
    /// block's terminating branch to the merge), so the result dominates the
    /// phi's incoming edge — the conversion emitted in the merge block itself
    /// would not. The builder's insert position is saved and restored, so the
    /// caller, positioned at the merge, sees no change.
    fn int_to_float_branch_value_in_pred(
        &self,
        v: IntValue<'ctx>,
        target: inkwell::types::FloatType<'ctx>,
        src_unsigned: bool,
        pred: BasicBlock<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let resume = self.builder.get_insert_block();
        match pred.get_terminator() {
            Some(term) => self.builder.position_before(&term),
            None => self.builder.position_at_end(pred),
        }
        let t = self.build_int_to_float_bf16_safe(v, target, src_unsigned, "bfw.itof");
        if let Some(bb) = resume {
            self.builder.position_at_end(bb);
        }
        t.into()
    }

    pub(super) fn compile_while(
        &mut self,
        label: Option<&str>,
        condition: &Expr,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        // Kernighan popcount idiom (popcount_idiom.rs): `while x != 0 { x = x &
        // (x - 1); c = c + 1 }` becomes one `llvm.ctpop` plus the two traps the
        // source requires. Attempted BEFORE any basic block is created so the
        // decline path leaves no empty blocks behind. Kāra's overflow checks are
        // what stop LLVM recognizing this itself, so it has to be done here.
        if self.try_emit_kernighan_popcount(condition, body)? {
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        let fn_val = self.current_fn.unwrap();
        let cond_bb = self.context.append_basic_block(fn_val, "while.cond");
        let body_bb = self.context.append_basic_block(fn_val, "while.body");
        let exit_bb = self.context.append_basic_block(fn_val, "while.exit");

        // Monotone-variable BCE (control_flow_bce.rs § monotone scan):
        // load each qualifying variable's loop-entry value here in the
        // preheader; the matching `llvm.assume`s are emitted at body
        // entry below.
        let mono_vars = self.collect_monotone_vars(Some(condition), body);
        let mono_inits = self.load_monotone_inits(&mono_vars);

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.fn_ctx.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: cond_bb,
            break_bb: exit_bb,
            result_slot: None,
            result_ty: None,
            cleanup_depth: self.drop_rc.scope_cleanup_actions.len(),
        });

        self.builder.position_at_end(cond_bb);
        // Per-iteration cleanup frame for temporaries created while evaluating
        // the condition — e.g. `.clone()` argument temps for a predicate call
        // (`while has_more(items[i].clone()) { .. }`) or a fresh heap temp the
        // guard consumes. `cond_bb` is re-entered every iteration, so without a
        // frame here those temps would land in the *enclosing* scope's frame
        // (pushed before this loop, drained once after it), leaking one
        // allocation per iteration — an unbounded leak. Mirrors the body frame
        // below; drained after the condition value is materialized (a plain
        // `i1`, never heap) and before the branch, so it runs on both the
        // taken and not-taken edges. Condition temps are never live in the
        // body (guards are `bool`), so freeing here is always safe.
        self.drop_rc.scope_cleanup_actions.push(Vec::new());
        let cond_val = self.compile_expr(condition)?.into_int_value();
        self.drain_top_frame_with_emit();
        self.builder
            .build_conditional_branch(cond_val, body_bb, exit_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        // Bounds-check-elision: the guard is true at body entry, so every
        // signed comparison conjunct that establishes an index bound can be
        // pushed as an asserted fact. `compile_vec_index` consults the stack
        // and drops the matching half of its runtime bounds check.
        let pushed_bounds = self.collect_asserted_bounds_from_guard(condition);
        let mut pushed_count = pushed_bounds.len();
        self.bce.asserted_index_bounds.extend(pushed_bounds);
        // Descending-loop skip (bce_length_pin.rs, B-2026-07-17-1): if this is a
        // recognised inner descending loop, its index is transitively proven
        // `< vec.len()` (length pin + enclosing counter bound). Push the
        // UpperBound facts so `emit_split_bounds_check` drops the upper half;
        // the lower half stays for LLVM to fold from the `k >= LO` guard.
        let cond_key = crate::resolver::SpanKey::from_span(&condition.span);
        if let Some(skip) = self.bce.descending_skips.get(&cond_key).cloned() {
            for vec_var in &skip.vec_vars {
                self.bce
                    .asserted_index_bounds
                    .push(super::state::AssertedIndexBound::UpperBound {
                        idx_var: skip.idx_var.clone(),
                        vec_var: vec_var.clone(),
                    });
                pushed_count += 1;
            }
        }
        // Converging two-pointer skip (bce_length_pin.rs, B-2026-08-04-8): both
        // indices of a recognised `while lo <= hi` are proven `base + idx <
        // vec.len()` (length pin + enclosing counter bound + the guard that
        // bounds `lo` by `hi`'s init). Push the SUM-index facts so
        // `emit_split_bounds_check` drops the upper half on `v[base + lo]` and
        // `v[base + hi]` — and the sign half too when the analysis could also
        // place the row origin and both indices at or above zero.
        if let Some(skip) = self.bce.converging_skips.get(&cond_key).cloned() {
            for vec_var in &skip.vec_vars {
                for idx_var in &skip.idx_vars {
                    self.bce.asserted_index_bounds.push(
                        super::state::AssertedIndexBound::SumIndex {
                            base_var: skip.base_var.clone(),
                            idx_var: idx_var.clone(),
                            vec_var: vec_var.clone(),
                            lower_proven: skip.lower_proven,
                        },
                    );
                    pushed_count += 1;
                }
            }
        }
        // Monotone facts: `x >= / <= its preheader value`, consumed by
        // LLVM's range passes to fold checks the source guard can't
        // express (conditionally-updated write heads / cursors).
        self.emit_monotone_assumes(&mono_inits);
        // Binary-search midpoint facts: a strict `lo < hi` guard lets a
        // `let mid = lo + (hi - lo) / 2` binding in the body assert
        // `lo <= mid < hi`, folding the `nums[mid]` bounds check that
        // interval-based CVP can't (control_flow_bce.rs § midpoint).
        let binsearch_guard = Self::binsearch_guard_pair(condition);
        if let Some(pair) = binsearch_guard.clone() {
            self.bce.binsearch_guard_stack.push(pair);
        }
        // Per-iteration scope frame, same shape as compile_for_range — see
        // its comment for the leak rationale.
        self.drop_rc.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;
        // Pop the bounds we pushed for this loop; restore the surrounding
        // scope's stack untouched. Nested loops therefore see only their
        // own and outer-loop bounds, never inner-loop leftovers.
        for _ in 0..pushed_count {
            self.bce.asserted_index_bounds.pop();
        }
        if binsearch_guard.is_some() {
            self.bce.binsearch_guard_stack.pop();
        }
        // Small constant-trip counted loop → hint LLVM to fully unroll
        // (B-2026-06-17-7): the back-edge branch built below carries
        // `llvm.loop.unroll.full` so a loop like kata:37's `while d <= 9`
        // unrolls the way rustc unrolls its equivalent (worth ~1.34x on
        // that bench). Advisory-only — LLVM ignores it if it can't prove a
        // small constant trip count. Computed while `condition`/`body` are
        // in scope; applied to the back-edge instruction.
        let wants_full_unroll = self.while_loop_wants_full_unroll(condition, body);
        // Partial (fixed-factor) unroll for the runtime-trip scalar-recurrence
        // shape LLVM 18's cost model wrongly declines (kata #70's Fibonacci
        // loop, ~1.38× — B-2026-07-08-24). Full unroll wins the small-constant-
        // trip case, so it takes precedence; memory-bound loops are excluded by
        // the scalar-only body gate so they never get force-unrolled + bloated.
        let wants_partial_unroll =
            !wants_full_unroll && self.while_loop_wants_partial_unroll(condition, body);
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            self.drain_top_frame_with_emit();
            let back_edge = self.builder.build_unconditional_branch(cond_bb).unwrap();
            if wants_full_unroll {
                self.attach_unroll_full_metadata(back_edge);
            } else if wants_partial_unroll {
                self.attach_unroll_count_metadata(back_edge, 4);
            }
        } else {
            self.drop_rc.scope_cleanup_actions.pop();
        }

        self.fn_ctx.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        // Vec-length-pin activation (bce_length_pin.rs): this `while` may be a
        // recognised counted fill whose completion establishes `vec.len() >=
        // bound`. Now that its body has fully emitted, move the pin live so a
        // later `while c < bound` guard resolves `bound` to `vec` and elides the
        // upper-half bounds check on `vec[c]` / `vec[c - k]` (kata #62). The
        // whole-function fail-closed analysis guarantees the fact stays true to
        // end of function, so the pin needs no later invalidation.
        let cond_key = crate::resolver::SpanKey::from_span(&condition.span);
        if let Some(pin) = self.bce.pending_vec_len_pins.remove(&cond_key) {
            self.bce.vec_len_pins.push((pin.bound, pin.vec_var));
        }
        Ok(self.context.i64_type().const_int(0, false).into())
    }

    pub(super) fn compile_loop(
        &mut self,
        label: Option<&str>,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let loop_bb = self.context.append_basic_block(fn_val, "loop.body");
        let exit_bb = self.context.append_basic_block(fn_val, "loop.exit");

        // No slot is pre-allocated: `compile_break` creates one lazily, typed
        // by the first break value it sees (B-2026-08-24-10). A pre-allocated
        // `i64` could only ever hold an integer, which is why `break 2.5`
        // silently produced 0 and `break f"s"` failed module verification.
        self.fn_ctx.loop_stack.push(LoopFrame {
            label: label.map(str::to_string),
            continue_bb: loop_bb,
            break_bb: exit_bb,
            result_slot: None,
            result_ty: None,
            cleanup_depth: self.drop_rc.scope_cleanup_actions.len(),
        });

        self.builder.build_unconditional_branch(loop_bb).unwrap();
        self.builder.position_at_end(loop_bb);
        // Per-iteration scope frame, same shape as compile_for_range — see
        // its comment for the leak rationale (body-local shared-struct
        // lets re-bound on every iteration would otherwise climb refcount
        // N×K and pin the chain). Drained just before the back-edge to
        // `loop_bb`.
        self.drop_rc.scope_cleanup_actions.push(Vec::new());
        self.compile_block(body)?;
        let body_has_terminator = self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_some();
        if !body_has_terminator {
            self.drain_top_frame_with_emit();
            self.builder.build_unconditional_branch(loop_bb).unwrap();
        } else {
            self.drop_rc.scope_cleanup_actions.pop();
        }

        let exited_with = self
            .fn_ctx
            .loop_stack
            .last()
            .and_then(|f| f.result_slot.zip(f.result_ty));
        self.fn_ctx.loop_stack.pop();
        self.builder.position_at_end(exit_bb);

        // A loop nothing ever `break`s out of is DIVERGENT — design.md
        // § `loop` type inference rule 0, "the loop runs forever or diverges
        // via panic/return", type `Never`. Its exit block then has no
        // predecessors and there is no value to produce.
        //
        // This is the same failure class the both-arms-diverge `if` above
        // handles, and it became reachable here the moment a trailing `loop`
        // could be a block's `final_expr` (B-2026-08-24-10): a
        // `-> Vec[String]` function ending in a `loop` that only exits via
        // `return` would otherwise load the i64 `loop.result` slot and emit
        // `ret i64 %loop.val` against a `{ptr, i64, i64}` return type, which
        // fails module verification. Terminating with `unreachable` makes the
        // enclosing `get_terminator().is_none()` guards skip the follow-on
        // `ret`, exactly as they did when the loop was still a statement.
        if exit_bb.get_first_use().is_none() {
            self.builder.build_unreachable().unwrap();
            return Ok(self.context.i64_type().const_int(0, false).into());
        }

        // Load the break value at the type it was stored with. No slot at
        // all means every `break` out of this loop was value-less, so the
        // loop's value is unit — represented here by the same i64 zero the
        // pre-allocated slot used to hold.
        match exited_with {
            Some((slot, ty)) => Ok(self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(ty, slot, "loop.val")
                .unwrap()),
            None => Ok(self.context.i64_type().const_int(0, false).into()),
        }
    }

    /// Compile `label: { body }` (`ExprKind::LabeledBlock`).
    ///
    /// LBC2 / LBC3: allocate an i64 result slot at the entry BB, push a
    /// `LoopFrame` carrying the label and the slot, compile the body,
    /// store the body's tail value (when control falls through normally)
    /// into the slot, branch to a freshly-created `lblock.exit` BB, and
    /// load the slot at the exit. Any `break label expr` inside the body
    /// goes through `compile_break`'s label-aware lookup, stores its
    /// value into the same slot, and branches to the same exit BB.
    ///
    /// Slot LLVM type: i64 today, matching `compile_loop`'s precedent.
    /// The typechecker's LUB constraint already guarantees that for
    /// non-i64-shaped block types, all break sites carry a value of the
    /// same shape — when v1 codegen extends to non-i64 break payloads
    /// (consume `expr_types` lookup), this function and `compile_loop`
    /// flip together. For unit-typed blocks LBC3 specifies the slot is
    /// i64 and `break label` (no value) stores zero.
    ///
    /// `continue_bb` for the frame is a dead `lblock.continue.unreachable`
    /// BB: the resolver rejects `continue label` referring to a labeled
    /// block (`E_CONTINUE_LABEL_BLOCK`), so the BB is never reached at
    /// runtime; pre-allocating it keeps the `LoopFrame` shape uniform.
    pub(super) fn compile_labeled_block(
        &mut self,
        label: &str,
        body: &Block,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let fn_val = self.current_fn.unwrap();
        let i64_t = self.context.i64_type();

        // Slot allocated lazily on the first stored value (tail or
        // `break label expr`), typed by that value — see `compile_loop`.
        // A never-stored slot yields the i64 zero placeholder at the exit,
        // preserving the old zero-init's unit-equivalent semantics.

        let body_bb = self
            .context
            .append_basic_block(fn_val, &format!("lblock.{}.body", label));
        let exit_bb = self
            .context
            .append_basic_block(fn_val, &format!("lblock.{}.exit", label));
        let continue_unreachable_bb = self
            .context
            .append_basic_block(fn_val, &format!("lblock.{}.continue.unreachable", label));

        // Populate the unreachable BB once; it will never branch in.
        // Position back at the previous insert point afterwards.
        let prev_bb = self.builder.get_insert_block();
        self.builder.position_at_end(continue_unreachable_bb);
        self.builder.build_unreachable().unwrap();
        if let Some(bb) = prev_bb {
            self.builder.position_at_end(bb);
        }

        self.builder.build_unconditional_branch(body_bb).unwrap();
        self.builder.position_at_end(body_bb);

        self.fn_ctx.loop_stack.push(LoopFrame {
            label: Some(label.to_string()),
            continue_bb: continue_unreachable_bb,
            break_bb: exit_bb,
            result_slot: None,
            result_ty: None,
            cleanup_depth: self.drop_rc.scope_cleanup_actions.len(),
        });

        // Compile the body. `compile_block` returns the tail expression's
        // value when the block has one; on normal fall-through we store
        // that value into the slot and branch to exit. If the body
        // already terminated (e.g., the tail was an early `break label`,
        // a `return`, or a `panic`), don't add a fall-through branch.
        let tail = self.compile_block(body)?;
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            if let Some(v) = tail {
                self.store_frame_result(v)?;
            }
            self.builder.build_unconditional_branch(exit_bb).unwrap();
        }

        let stored = self
            .fn_ctx
            .loop_stack
            .last()
            .and_then(|f| f.result_slot.zip(f.result_ty));
        self.fn_ctx.loop_stack.pop();
        self.builder.position_at_end(exit_bb);
        match stored {
            Some((slot, ty)) => Ok(self
                .builder
                .build_load::<BasicTypeEnum<'ctx>>(ty, slot, "lblock.val")
                .unwrap()),
            None => Ok(i64_t.const_int(0, false).into()),
        }
    }

    /// Disarm a handle-shaped binding's queued cleanup by ZEROING its slot at
    /// runtime, so the value can be carried out of a loop by `break`. Returns
    /// whether it fired — the caller uses that as permission to store the
    /// handle, since taking ownership and storing it must be one decision.
    ///
    /// Zeroing rather than retracting the queued action is the whole point.
    /// Map/Set cleanup is queue-driven (`FreeMapHandle`) with no in-slot
    /// sentinel like Vec/String's `cap = 0`, so the existing move suppressor
    /// (`suppress_map_cleanup_for_tail_identifier`) disarms by editing the
    /// queue at COMPILE TIME. That is right for a function tail, which runs
    /// once, and wrong here: a `break` is conditional and sits inside a loop
    /// that may iterate many times without taking it, so retracting would
    /// disarm the free on every non-breaking iteration too — a leak in place
    /// of a double free. The runtime store executes only on the path that
    /// actually breaks, and `FreeMapHandle`'s null-guard makes the queued
    /// action a no-op exactly there.
    ///
    /// TWO handle kinds qualify, by OPPOSITE mechanisms — which is the whole
    /// subtlety, and getting it backwards corrupts the heap:
    ///
    ///   * `Map` / `Set` (`FreeMapHandle`) — DISARM. Their cleanup is
    ///     queue-driven with no in-slot sentinel, so this zeroes the slot and
    ///     the (now null-guarded) queued free becomes inert on the breaking
    ///     path only.
    ///
    ///   * `shared` struct / enum (`RcDec`) — DO NOT DISARM. An RC'd value
    ///     moves out by RETAIN, not by suppression: the source's queued dec
    ///     still fires, and `suppress_source_vec_cleanup_for_arg` (called just
    ///     after the store, under `apply_shared_transfer`) emits the balancing
    ///     `+1`, so the value leaves at net +1 for the receiver. That is
    ///     exactly what the function-tail path emits for
    ///     `fn f() -> Node { let n = ...; n }`, verified in its IR.
    ///
    /// Zeroing a `shared` slot was tried and MEASURED to hang a `shared
    /// struct` and bus-error a `shared enum` (B-2026-08-24-21). The cause is
    /// ordering, not RC semantics: the zero-store ran BEFORE the retain, so
    /// `move.rc.load` read null out of the slot and incremented through it.
    /// The trap is easy to fall into because `RcDec` is already null-guarded
    /// — for the unrelated case of a body-local slot whose `let` never ran —
    /// which makes zeroing look like the same one-line move as the Map case.
    ///
    /// Identifier sources only: an rvalue `break Map.new()` or
    /// `break Node { .. }` has no binding to disarm, so it keeps the skip path
    /// and its loud verification error rather than being half-supported.
    fn disarm_moved_handle_by_zeroing(&mut self, expr: &Expr) -> bool {
        let ExprKind::Identifier(name) = &expr.kind else {
            return false;
        };
        let Some(slot) = self.variables.get(name).map(|s| s.ptr) else {
            return false;
        };
        // The cleanup queue is the only witness to what kind of handle this
        // is: the LLVM type is an opaque `ptr` shared by every handle kind, so
        // a Map and a `shared` struct are indistinguishable from the value
        // alone.
        use crate::codegen::state::CleanupAction;
        let is_map = self.drop_rc.scope_cleanup_actions.iter().any(|frame| {
            frame.iter().any(|action| match action {
                CleanupAction::FreeMapHandle { map_alloca, .. } => *map_alloca == slot,
                _ => false,
            })
        });
        if is_map {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            self.builder.build_store(slot, ptr_ty.const_null()).unwrap();
            return true;
        }
        // A `shared` binding: permit the store, but emit NOTHING here. The
        // retain that balances its still-live queued dec is emitted by
        // `suppress_source_vec_cleanup_for_arg` after the store, and it reads
        // the handle back out of this slot — so zeroing would be read as null.
        self.var_types
            .var_type_names
            .get(name)
            .is_some_and(|tn| self.type_decls.shared_types.contains_key(tn.as_str()))
    }

    /// Does this break value produce a handle whose ONLY owner is the
    /// expression itself — a fresh temporary, not a second reference to
    /// something a binding still owns (B-2026-08-25-1)?
    ///
    /// This is the rvalue counterpart to [`Self::disarm_moved_handle_by_zeroing`],
    /// and it exists because every mechanism that one relies on is keyed on a
    /// source BINDING: the disarm needs a slot to null, and the `shared` retain
    /// reads the handle back out of that slot. `break Node { v: 1 }` and
    /// `break make_map(n)` have no binding, so they used to fail the pointer
    /// gate and leave the loop's result slot unwritten — which surfaced as
    /// `Module verification failed: ret i64 0` rather than as a wrong answer.
    ///
    /// The oracle is the RETURN twin. `return Node { v: 1 }` in the same loop
    /// already compiles, and its IR is the whole argument: a `malloc`, a
    /// `store i64 1` into the refcount, and a bare `ret` — no retain, no dec,
    /// nothing queued in any cleanup frame. A `return make_map(n)` is the same
    /// shape one level up (`%call = call ptr @make_map` then `ret ptr %call`).
    /// The temporary is born owned and the drain has nothing to say about it,
    /// so `break` needs no ownership action either — only permission to store.
    ///
    /// Deliberately an ALLOWLIST of forms that manufacture ownership, never a
    /// "not obviously borrowed" test. The forms that must keep failing are the
    /// PLACE reads — `break n.child`, `break v[i]`, `break self.head` — which
    /// hand out a handle the container still owns and would need a retain this
    /// emits none of. Refusing an unfamiliar form costs a compile error;
    /// admitting one costs a use-after-free, so silence is the safe default and
    /// anything added here needs the same standard of proof as the two arms
    /// below.
    fn break_value_is_fresh_owned_handle(&self, expr: &Expr) -> bool {
        match &expr.kind {
            // A `shared` struct literal mallocs its own box and initializes
            // the refcount to 1 — see the `return` IR quoted above. The
            // `shared` test is not redundant even though a plain struct
            // literal never lowers to a pointer: `owned_ptr` is read as a
            // claim about ownership, so it should only be true where that
            // claim has actually been checked.
            ExprKind::StructLiteral { path, .. } => path
                .last()
                .is_some_and(|n| self.type_decls.shared_types.contains_key(n.as_str())),
            // A BRANCHING carrier — `break if c { Node { v: 4 } } else
            // { Node { v: 5 } }` (B-2026-08-25-6). Exactly one tail runs, but
            // the slot is written once for all of them, so EVERY tail has to
            // manufacture ownership: a single place-reading tail would make
            // the store a second owner on that path.
            //
            // ALL-tails, and the quantifier is the whole subtlety. The sibling
            // walker over these same nodes —
            // `init_projects_out_of_container_element` — uses ANY-tail, and
            // the two are not inconsistent: that one NARROWS a registration
            // (being wrong leaves the container owning what it already owned),
            // this one WIDENS a permission (being wrong is a use-after-free).
            // The quantifier flips with the direction of the risk, so a
            // future edit that "aligns" them by copying `any` across would be
            // a soundness bug, not a cleanup.
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => b
                .final_expr
                .as_deref()
                .is_some_and(|t| self.break_value_is_fresh_owned_handle(t)),
            // An `if` with no `else` yields unit and can carry nothing, so it
            // declines rather than admitting a half-covered slot.
            ExprKind::If {
                then_block,
                else_branch,
                ..
            }
            | ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                let Some(otherwise) = else_branch.as_deref() else {
                    return false;
                };
                then_block
                    .final_expr
                    .as_deref()
                    .is_some_and(|t| self.break_value_is_fresh_owned_handle(t))
                    && self.break_value_is_fresh_owned_handle(otherwise)
            }
            // `all` over an EMPTY arm list is vacuously true, which would
            // claim ownership of a value no arm produces — hence the explicit
            // non-empty guard.
            ExprKind::Match { arms, .. } => {
                !arms.is_empty()
                    && arms
                        .iter()
                        .all(|a| self.break_value_is_fresh_owned_handle(&a.body))
            }
            // Calls and method calls — `make_map(n)`, `Map.new()`, and every
            // `shared enum` variant construction, which parses as a call.
            // Kāra's convention is that a call returns an owned value, and
            // this predicate already carves out the one exception (a callee
            // declared to return a borrow), which is why it is reused here
            // rather than rewritten.
            _ => self.expr_yields_fresh_owned_temp(expr),
        }
    }

    /// Store `val` into `frame`'s result slot, creating that slot on first
    /// use at `val`'s OWN type.
    ///
    /// Callers hold a CLONE of the frame (the label-aware lookup in
    /// `compile_break` clones so the borrow ends before `compile_expr` runs),
    /// so the slot has to be recorded on the live stack entry rather than the
    /// copy. Frames are matched by `break_bb`, which uniquely identifies one:
    /// every loop and labeled block appends its own exit block.
    fn store_in_frame(
        &mut self,
        frame: &LoopFrame<'ctx>,
        val: BasicValueEnum<'ctx>,
        owned_ptr: bool,
    ) -> Result<(), String> {
        let key = frame.break_bb;
        if let Some(idx) = self
            .fn_ctx
            .loop_stack
            .iter()
            .position(|f| f.break_bb == key)
        {
            self.store_in_frame_at(idx, val, owned_ptr)?;
        }
        Ok(())
    }

    /// A labeled block's TAIL value targets the innermost frame — its own,
    /// pushed just before the body was compiled.
    fn store_frame_result(&mut self, val: BasicValueEnum<'ctx>) -> Result<(), String> {
        if let Some(idx) = self.fn_ctx.loop_stack.len().checked_sub(1) {
            self.store_in_frame_at(idx, val, false)?;
        }
        Ok(())
    }

    fn store_in_frame_at(
        &mut self,
        idx: usize,
        val: BasicValueEnum<'ctx>,
        owned_ptr: bool,
    ) -> Result<(), String> {
        let ty = val.get_type();
        // What may travel through the result slot (B-2026-08-24-13).
        //
        // Ints were always allowed; floats were added with the lazily-typed
        // slot. STRUCTS — the `{ptr, i64, i64}` header a `String` / `Vec`
        // lowers to, and plain POD structs — are allowed now that
        // `compile_break` suppresses the source binding's scope-exit free
        // before draining the loop's frames. Without that suppression the
        // drain freed the very buffer this slot points at, which measured as
        // "free(): double free detected in tcache 2".
        //
        // Deliberately still excluded, each for its own reason:
        //   * the EMPTY struct — `break outer ()` is a VALUELESS break
        //     (design.md § `break expr`: "Plain `break` (or `break ()`) is
        //     valid in any loop form"), so there is nothing to carry and a
        //     slot would only invent one;
        //   * ARRAYS — `Array[T, N]` drops elementwise via `StructDrop`, so
        //     the whole-binding retraction is a different suppressor.
        // Both keep the previous skip-silently behaviour, which preserves the
        // single owner rather than inventing a second one.
        //
        // A POINTER — every handle kind lowers to one, so this covers `Map`,
        // `Set`, `shared struct` and `shared enum` alike — is storable only
        // against `owned_ptr`, which is a PROOF that this break site owns what
        // it points at, not a hint. Three things can supply that proof, and
        // `compile_break` is where they are established: a `Map`/`Set` binding
        // disarmed by zeroing, a `shared` binding whose still-live dec is
        // balanced by a retain, or a fresh temporary that never had a second
        // owner to begin with. Without such a proof a handle would travel out
        // as a SECOND reference with nothing balancing it — a use-after-free
        // instead of the loud verification error the skip produces
        // (B-2026-08-24-19).
        let storable = ty.is_int_type()
            || ty.is_float_type()
            || (ty.is_struct_type() && ty.into_struct_type().count_fields() > 0)
            || (ty.is_pointer_type() && owned_ptr);
        if !storable {
            return Ok(());
        }
        let slot = match self.fn_ctx.loop_stack[idx].result_slot {
            Some(s) => s,
            None => {
                let fn_val = self.current_fn.unwrap();
                let s = self.create_entry_alloca(fn_val, "brk.result", ty);
                self.fn_ctx.loop_stack[idx].result_slot = Some(s);
                self.fn_ctx.loop_stack[idx].result_ty = Some(ty);
                s
            }
        };
        // Two breaks out of the same construct must agree, which the
        // typechecker now enforces (`check_break_values_agree`). This guard
        // covers only a codegen-side disagreement the source level cannot
        // express — storing at the wrong type would corrupt the slot, so skip
        // rather than emit an ill-typed store.
        if self.fn_ctx.loop_stack[idx].result_ty == Some(ty) {
            self.builder.build_store(slot, val).unwrap();
        }
        Ok(())
    }

    pub(super) fn compile_break(
        &mut self,
        label: Option<&str>,
        value: Option<&Expr>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let zero = self.context.i64_type().const_int(0, false);
        // LBC1: label-aware lookup. With `Some(l)`, walk the frame stack
        // top-down and pick the first frame whose label matches; with
        // `None`, fall back to the innermost frame. This is what makes
        // `break outer;` actually skip past `inner` when `outer` is the
        // labeled loop / labeled block (today's pre-slice behavior would
        // always pick the innermost — silent miscompile under nested
        // labels, no test fixture exercised it before this slice).
        let frame = match label {
            Some(l) => self
                .fn_ctx
                .loop_stack
                .iter()
                .rev()
                .find(|f| f.label.as_deref() == Some(l))
                .cloned(),
            None => self.fn_ctx.loop_stack.last().cloned(),
        };
        // FAIL CLOSED (B-2026-08-24-10). This used to be `if let Some(frame)`
        // with no `else`: a `break` that matched no frame fell straight to
        // the `Ok` below having emitted NO BRANCH, so it became a silent
        // no-op and its loop ran forever. That is how a tail-position `loop`
        // in a non-unit function produced an AOT binary that hangs — no
        // diagnostic at any phase, and the IR still verifies because an
        // infinite loop is perfectly well-formed. A missing frame is a
        // COMPILER bug, not a program error (the resolver already rejects a
        // stray `break`), so the only honest response is to refuse to emit.
        let Some(frame) = frame else {
            return Err(format!(
                "internal: `break{}` found no enclosing loop or labeled-block frame",
                label.map(|l| format!(" {l}")).unwrap_or_default()
            ));
        };
        let val = if let Some(v) = value {
            self.compile_expr(v)?
        } else {
            zero.into()
        };
        // Store the break value into the target frame's slot, creating that
        // slot on first use at the VALUE's type. Previously the slot was a
        // pre-allocated i64 and this store was guarded on `is_int_value()`,
        // so a float or aggregate break silently stored nothing at all.
        // Ownership transfer runs between the LOAD above and the drain below.
        // The handle disarm has to precede the store because it also decides
        // whether the handle may be stored at all.
        // Two independent proofs of ownership, in the order their side
        // effects allow: the disarm runs first because for a `Map` binding it
        // WRITES (nulls the source slot), and only its `false` return means
        // "no binding here to take from". The freshness test then covers the
        // rvalue carriers, which have no binding at all (B-2026-08-25-1).
        let owned_ptr = value.is_some_and(|v| {
            self.disarm_moved_handle_by_zeroing(v) || self.break_value_is_fresh_owned_handle(v)
        });
        self.store_in_frame(&frame, val, owned_ptr)?;
        // MOVE-AWARE SUPPRESSION (B-2026-08-24-13), the break-site twin of
        // what `suppress_cleanup_for_tail_return` does for a function tail.
        //
        // The drain below frees every binding in the frames inside this loop
        // — including the one whose buffer the slot now points at. Zeroing the
        // source's `cap` makes its queued `FreeVecBuffer` no-op, so the value
        // leaving through the slot keeps exactly ONE owner: the binding that
        // receives the loop's value.
        //
        // ORDER IS LOAD-BEARING in both directions. After `store_in_frame`,
        // because zeroing before the value is read corrupts what the receiver
        // gets (B-2026-06-12-6). Before `emit_scope_cleanup_from`, because
        // that is the drain being disarmed.
        //
        // Only the RUNTIME-STORE suppressor is used here, never the
        // compile-time queue-retracting ones (Map/Set, boxed-enum payload). A
        // `break` is conditional and sits inside a loop that may iterate many
        // times without taking it, so a flow-insensitive retraction would
        // disarm the cleanup on the iterations that DON'T break — trading this
        // double free for a leak, the exact bargain the tail-return code warns
        // against. A `Map` / `Set` / boxed-enum break value therefore still
        // takes the skip-silently path above.
        if let Some(v) = value {
            self.suppress_source_vec_cleanup_for_arg(v);
            // The f-string sibling. A `break f"…"` (or `break x.to_string()`)
            // has no source BINDING to disarm — the buffer belongs to the
            // accumulator staged while lowering the interpolation, and that
            // accumulator queued its own `FreeVecBuffer` in a frame the drain
            // below is about to run. `rhs_stages_fstr_acc` is the same
            // predicate the let-binding and tail-return paths use, and it
            // deliberately covers `.to_string()` as well as a literal `f"…"`:
            // a struct `.to_string()` stages its acc through the synthetic
            // f-string in `compile_struct_display_string`, and the narrower
            // `InterpolatedStringLit` match missed exactly that shape once
            // before (B-2026-07-12-17). `take()` yielding `None` — a user
            // `impl Display` that stages no acc — is a harmless no-op.
            if self.rhs_stages_fstr_acc(v) {
                if let Some(acc) = self.last_fstr_acc.take() {
                    self.zero_vec_alloca_cap(acc);
                }
            }
        }
        // Drain the frames INSIDE the loop being exited (per-iteration
        // frame + any nested block / `if let` / match-arm frames between
        // here and the loop boundary) — the back-edge / scope-end drains
        // are on paths this branch skips. Emit-only: the compile-time
        // stack is untouched, the fall-through path keeps its own drains.
        self.emit_scope_cleanup_from(frame.cleanup_depth);
        self.builder
            .build_unconditional_branch(frame.break_bb)
            .unwrap();
        Ok(zero.into())
    }

    pub(super) fn compile_continue(
        &mut self,
        label: Option<&str>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let zero = self.context.i64_type().const_int(0, false);
        // LBC1: same label-aware lookup as `compile_break`. The resolver
        // guarantees `continue label` only resolves to a `Loop`-kind
        // frame, but the codegen-side dispatch is uniform.
        let frame = match label {
            Some(l) => self
                .fn_ctx
                .loop_stack
                .iter()
                .rev()
                .find(|f| f.label.as_deref() == Some(l))
                .cloned(),
            None => self.fn_ctx.loop_stack.last().cloned(),
        };
        // Fail closed for the same reason as `compile_break` above: a
        // `continue` that emits no branch falls through into whatever
        // follows, silently changing the program's control flow.
        let Some(frame) = frame else {
            return Err(format!(
                "internal: `continue{}` found no enclosing loop frame",
                label.map(|l| format!(" {l}")).unwrap_or_default()
            ));
        };
        // Same early-exit drain as `compile_break`: `continue` jumps to
        // the loop header, skipping the body-end back-edge drain.
        self.emit_scope_cleanup_from(frame.cleanup_depth);
        self.builder
            .build_unconditional_branch(frame.continue_bb)
            .unwrap();
        Ok(zero.into())
    }
}
