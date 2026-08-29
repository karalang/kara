//! Match expression evaluation + pattern try/bind helpers.
//!
//! Houses `eval_match` (the entry from `eval_expr_inner` /
//! `eval_stmt_cf`), `try_match_pattern` (read-only pattern probe —
//! does this value match without binding?), `bind_pattern` (the
//! bind half — push pattern bindings into the current scope on a
//! known-match), and the two pattern helpers
//! `value_in_range_pattern` and `literal_to_value`.
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::exec::slice_pattern_view;
use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    // ── Match evaluation ────────────────────────────────────────

    pub(crate) fn eval_match(
        &mut self,
        scrutinee_place: Option<&Expr>,
        scrutinee: &Value,
        arms: &[MatchArm],
        span: &Span,
    ) -> Value {
        // B-2026-07-30-11 (enum leg) — before any arm runs, disarm the
        // scrutinee binding's payload-body walk if ANY arm moves a Drop-bearing
        // payload out. Scanning every arm rather than only the taken one is
        // what keeps this in step with codegen, whose retraction is a
        // compile-time removal and therefore cannot be path-sensitive.
        self.disarm_moved_out_enum_payload(scrutinee_place, scrutinee, arms);
        for arm in arms {
            if self.try_match_pattern(&arm.pattern, scrutinee) {
                // Check guard if present
                if let Some(ref guard) = arm.guard {
                    self.env.push_scope();
                    self.bind_pattern(&arm.pattern, scrutinee.clone());
                    let guard_val = self.eval_expr_inner(guard);
                    self.env.pop_scope();
                    if !self.is_truthy(&guard_val) {
                        continue;
                    }
                }
                self.env.push_scope();
                self.bind_pattern(&arm.pattern, scrutinee.clone());
                // B-2026-08-04-4 — an arm binding holds a FRESH value, so a
                // stale move-out record left by an earlier, unrelated binding
                // of the same NAME must not silence its drops.
                //
                // The move-out sets are keyed by name alone, with no scope
                // component, so they are re-armed at each site that rebinds a
                // name. `let` and assignment already did; a match arm did not,
                // which is the whole defect: `match o { Some(r) => v.push(r) }`
                // records `r` as moved-into-a-container, and a LATER sibling
                // block's `match o2 { Some(r) => .. }` inherited that record
                // and skipped its own `impl Drop` body. Renaming the second
                // binding made the body fire — the name was the only trigger.
                for bound in arm.pattern.binding_names() {
                    self.rearm_container_bodies_for_name(&bound);
                }
                // B-2026-08-29-17 — a payload bound out of an OWNED-PARAM
                // scrutinee is a VIEW of the callee's entry copy, and the
                // view-ness has to PROPAGATE so a rebind of it inherits the
                // ownership story.
                //
                // The bind itself is already correct: `scrutinee_expr_is_consuming`
                // answers false for an owned param (B-2026-08-01-13's
                // caller-retains carve-out), so no Drop slot is registered for
                // the binding. But `let m = r;` inside the arm then reached
                // `let_destructures_owned_param`, whose `src_is_view` test
                // consults exactly this set — and `r` was not in it, so `m`
                // registered a slot of its own and ran the body the CALLER was
                // already going to run. Measured `dR1 dR1 v=1` where one body
                // is due, for `Full(r) => { let m = r; m.id }`.
                //
                // This set IS the view set: B-2026-08-01-15's whole-param
                // rebind propagates by inserting into it, and a payload view
                // belongs there for the same reason. Codegen twin: the
                // `param_view_locals` insert in `pattern_binding.rs`, which
                // carries the identical rule and had the identical hole.
                //
                // Gated on the scrutinee being an owned param, so a LOCAL or
                // fresh-temp scrutinee — where the arm binding really is the
                // only owner — is untouched and keeps registering its slot.
                if scrutinee_place.is_some_and(|sp| {
                    matches!(&sp.kind, ExprKind::Identifier(n)
                        if self.owned_param_names_stack
                            .last()
                            .is_some_and(|params| params.contains(n.as_str())))
                }) {
                    for bound in arm.pattern.binding_names() {
                        if let Some(top) = self.owned_param_names_stack.last_mut() {
                            top.insert(bound);
                        }
                    }
                }
                // B-2026-07-30-11 (match-arm leg): the taken arm's moved-out
                // Drop-bearing payload bindings get REAL Drop slots. Stash
                // them; the arm body's block executor adopts them into its
                // cleanup vec at entry, so NLL placement and every move hook
                // apply exactly as for `let` bindings. Only for a consuming
                // scrutinee — an owned binding/`self` (whose walk the disarm
                // above retracted) or an OWNING fresh temp (`match v.pop()`,
                // `match mk()` — the value is the match's to drop, and
                // codegen already fired these via the same arm-channel
                // routing); a field-access place is a view whose owner still
                // walks, and a borrow accessor's payload aliases the
                // container's element (see `scrutinee_expr_is_consuming`).
                // B-2026-08-28-67 — the stash's half of the read-through
                // gate; see `arm_only_reads_payload_through`. Kept beside the
                // consuming-scrutinee test rather than folded into it because
                // the two ask different questions: that one is about where the
                // SCRUTINEE came from, this one about what the ARM does.
                let arm_reads_through = match scrutinee {
                    Value::EnumVariant { enum_name, .. } => {
                        scrutinee_place.is_some_and(Self::place_walk_is_retractable)
                            && !self.match_disarms_payload_walk(enum_name, arms)
                    }
                    _ => false,
                };
                let consuming_scrutinee = !arm_reads_through
                    && matches!(scrutinee, Value::EnumVariant { .. })
                    && scrutinee_place.is_none_or(|sp| self.scrutinee_expr_is_consuming(sp));
                if consuming_scrutinee {
                    if let Value::EnumVariant { enum_name, .. } = scrutinee {
                        for n in self.arm_moved_user_drop_payload_bindings(enum_name, &arm.pattern)
                        {
                            // B-2026-08-28-63 — a user ENUM payload is a real
                            // Drop slot too. The struct-only bind below meant a
                            // consuming arm that took an enum out
                            // (`match o { Some(e) => .. }` for `Option[E]`) ran
                            // NO body for it, while the same arm binding a
                            // STRUCT payload ran one. Both backends agreed on
                            // zero, so no A/B gate could report it.
                            //
                            // `type_name_runs_user_drop` is the same predicate
                            // the payload-bodies registration uses, so an enum
                            // with no `Drop` of its own but a Drop-bearing
                            // variant payload qualifies here exactly as it does
                            // there. Option/Result are excluded: a nested
                            // built-in payload rides its own walker.
                            let is_drop_binding = match self.env.get(&n) {
                                Some(Value::Struct { name: tn, .. }) => {
                                    self.program.drop_method_keys.contains_key(&tn)
                                }
                                Some(Value::EnumVariant { enum_name: en, .. })
                                    if en != "Option" && en != "Result" =>
                                {
                                    self.type_name_runs_user_drop(&en, &mut Vec::new())
                                }
                                _ => false,
                            };
                            if is_drop_binding {
                                self.pending_arm_drop_bindings.push(n);
                            }
                        }
                    }
                }
                // B-2026-07-28-7: watch the scrutinee's slot across the arm
                // body. If the body ASSIGNS to it (`match cur { Some(n) => {
                // cur = n.next } }` — every linked-structure walk and every
                // state machine), the storage the write-through below would
                // target no longer exists, so writing back the pre-body value
                // silently REVERTS the assignment. That turned the canonical
                // list walk into an infinite loop.
                let watch = scrutinee_place
                    .filter(|p| Self::match_place_is_writable(p))
                    .map(|p| match &p.kind {
                        ExprKind::Identifier(n) => n.clone(),
                        _ => "self".to_string(),
                    });
                if let Some(name) = &watch {
                    self.env.push_watch(name);
                }
                // B-2026-08-28-51 — a bare-identifier arm body never reaches
                // `eval_block_inner`, so the block-tail conditional-move hook
                // does not see it. Mark here instead, immediately before the
                // TAKEN arm evaluates — which is what makes it the runtime bit.
                // No cleanup frame of its own exists for a non-block arm, so
                // nothing can be owned locally; a block-bodied arm falls out on
                // the Identifier test and takes the block path as before.
                self.record_conditional_move_tail(&arm.body, &[]);
                let result = self.eval_expr_inner(&arm.body);
                let reassigned = watch.is_some() && self.env.pop_watch();
                // B-2026-07-23-12: write-through for a mutable-place scrutinee.
                // The interpreter binds a match payload BY VALUE (a clone of the
                // scrutinee), so an in-arm mutation of a bound payload —
                // `match v { Table(m) => m.insert(..) }` where `m` is a `Map`
                // stored by value — updates only the arm-local `m`, never `v`.
                // Codegen writes through correctly; this closes the divergence.
                // After the arm body runs, reconstruct the scrutinee value with
                // each DIRECTLY-bound payload position replaced by its current
                // (possibly mutated) binding, and store it back to the scrutinee
                // place. Done BEFORE `pop_scope` so the arm bindings are still
                // live. Gated to bare-identifier / `self` places (the `mut ref`
                // param and receiver cases the bug reports); the CICO write-back
                // in `eval_call` then propagates a `mut ref` param back to the
                // caller. A non-place scrutinee (`match f() { .. }`) or a pattern
                // with no direct value binding leaves the scrutinee untouched.
                // Skipped entirely when the body reassigned the place
                // (`reassigned`, B-2026-07-28-7): that write is the newer and
                // authoritative one, and it is also what codegen does — there
                // the payload binding is a pointer into the OLD storage, so
                // replacing the place leaves the binding's later mutations
                // invisible to it rather than resurrecting the old value.
                if let Some(place) = scrutinee_place.filter(|_| !reassigned) {
                    if Self::match_place_is_writable(place) {
                        if let Some(patched) = self.patch_arm_bindings(&arm.pattern, scrutinee) {
                            self.write_back_receiver(place, patched);
                        }
                    }
                }
                // B-2026-07-30-11 (match-arm leg): a NON-block arm body never
                // reaches `eval_block_inner`, so the stash above was never
                // adopted — fire the leftovers here, at the arm's end.
                // A body whose only uses of the binding are field / tuple
                // projections (`println(r.id)`) is a read — fire, matching
                // codegen's NLL placement. Any other mention could be a move
                // (`take(r)`, `r.consume()` with an owned receiver) and stays
                // silent, as does a binding some in-body ctor moved out.
                // Block bodies — the common case — drain to empty in the
                // executor and take the precise path instead.
                for n in std::mem::take(&mut self.pending_arm_drop_bindings) {
                    if self.moved_out_user_drop_bindings.contains(&n) {
                        continue;
                    }
                    if crate::deque_head::expr_mentions_name_outside_field_projection(&arm.body, &n)
                    {
                        continue;
                    }
                    if let Some(Value::Struct { name: tn, fields }) = self.env.get(&n) {
                        if self.program.drop_method_keys.contains_key(&tn) {
                            let tn = tn.clone();
                            self.run_user_drop_body_on_value(
                                &tn,
                                Value::Struct {
                                    name: tn.clone(),
                                    fields,
                                },
                            );
                        }
                    // B-2026-08-28-63 — the NON-BLOCK arm body's copy of the
                    // same widening. A block body drains through
                    // `eval_block_inner`'s adopted Drop slots (where
                    // `invoke_user_drop_if_applicable` already answers for an
                    // enum); this leg fires the leftovers by hand and so needs
                    // the enum case spelled out: own body first, then the live
                    // variant's payload bodies — the order every other walk in
                    // this family uses.
                    } else if let Some(v @ Value::EnumVariant { .. }) = self.env.get(&n) {
                        let Value::EnumVariant { enum_name: en, .. } = &v else {
                            unreachable!("matched EnumVariant above")
                        };
                        if en != "Option" && en != "Result" {
                            let en = en.clone();
                            if self.program.drop_method_keys.contains_key(&en) {
                                self.run_user_drop_body_on_value(&en, v.clone());
                            }
                            self.run_enum_payload_user_drops_value(&v);
                        }
                    }
                }
                self.env.pop_scope();
                return result;
            }
        }
        // Defense in depth: the typechecker's exhaustiveness check plus the
        // pattern-scrutinee-mismatch gate (B-2026-07-17-6) should make this
        // path unreachable, but a future front-end gap must degrade to a
        // clean runtime diagnostic rather than panic the whole process (the
        // old `unreachable!` turned an accepted-but-wrong program into a Rust
        // backtrace instead of a Kāra error).
        self.record_runtime_error(
            format!(
                "internal error: non-exhaustive match at {}:{} — no arm matched \
                 the scrutinee value (the typechecker should have rejected this)",
                span.line, span.column
            ),
            span,
        )
    }

    /// B-2026-07-30-11 (enum leg) — record that a `match` over the enum binding
    /// `scrutinee_place` has an arm that moves a Drop-bearing payload out, so
    /// the source's payload-body walk skips it.
    ///
    /// The interpreter twin of codegen's prefix-keyed
    /// `suppress_container_elem_bodies_for_var`, and matched to it on all three
    /// gates: an identifier / `self` place (a fresh temp registered nothing to
    /// disarm), a value enum (a `shared` enum drops through refcounts), and a
    /// payload position whose DECLARED type runs a user body — a Wildcard or
    /// literal sub-pattern claims no ownership, so the source must still fire.
    pub(crate) fn disarm_moved_out_enum_payload(
        &mut self,
        scrutinee_place: Option<&Expr>,
        scrutinee: &Value,
        arms: &[MatchArm],
    ) {
        let Some(place) = scrutinee_place else {
            return;
        };
        // B-2026-08-03-6 — a TUPLE-ELEMENT scrutinee (`match t.0 { Ok(r) => .. }`).
        // The element has no binding of its own, so the name-keyed record below
        // cannot express it; the per-`(binding, index)` set does. Without it the
        // tuple's own element walk still owned the body and ran it at the
        // BINDING's death, while codegen retracts it at the arm — a timing
        // divergence. Codegen's twin is
        // `suppress_tuple_elem_optres_payload_cleanup`, whose memory half is
        // load-bearing there (the arm and the tuple's element drop would
        // otherwise both free the payload).
        if let ExprKind::TupleIndex { object, index } = &place.kind {
            if let ExprKind::Identifier(src) = &object.kind {
                let Value::EnumVariant { enum_name, .. } = scrutinee else {
                    return;
                };
                let enum_name = enum_name.clone();
                let src = src.clone();
                let index = *index as usize;
                if arms
                    .iter()
                    .any(|arm| self.pattern_consumes_user_drop_payload(&enum_name, &arm.pattern))
                {
                    self.moved_out_tuple_elem_bodies.insert((src, index));
                }
            }
            return;
        }
        let name = match &place.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return,
        };
        let Value::EnumVariant { enum_name, .. } = scrutinee else {
            return;
        };
        let enum_name = enum_name.clone();
        if self.match_disarms_payload_walk(&enum_name, arms) {
            self.moved_out_enum_payload_bindings.insert(name);
        }
    }

    /// Single-pattern form of [`Self::disarm_moved_out_enum_payload`], for the
    /// `if let` site (codegen's `control_flow.rs` mirror of the `match` call).
    /// Runs BEFORE the match test, like codegen's compile-time retraction,
    /// which cannot know whether the pattern will match at runtime.
    ///
    /// `scope` is the `then_block` the binding lives in, so the
    /// B-2026-08-28-67 read-through gate can be asked the same question the
    /// `match` arm asks — both spellings must answer it identically, the
    /// spelling-dependent split being exactly the shape B-2026-08-28-63 had to
    /// close once already for this family. `None` means the binding ESCAPES the
    /// construct (`let … else` binds into the enclosing block), where there is
    /// no scope to inspect and the payload is materialized by definition.
    pub(crate) fn disarm_moved_out_enum_payload_one(
        &mut self,
        scrutinee_place: &Expr,
        scrutinee: &Value,
        pattern: &Pattern,
        scope: Option<&Block>,
    ) {
        let name = match &scrutinee_place.kind {
            ExprKind::Identifier(n) => n.clone(),
            ExprKind::SelfValue => "self".to_string(),
            _ => return,
        };
        let Value::EnumVariant { enum_name, .. } = scrutinee else {
            return;
        };
        let enum_name = enum_name.clone();
        if self.pattern_consumes_user_drop_payload(&enum_name, pattern)
            && !scope
                .is_some_and(|b| self.let_form_only_reads_payload_through(&enum_name, pattern, b))
        {
            self.moved_out_enum_payload_bindings.insert(name);
        }
    }

    /// B-2026-07-30-11 (match-arm leg) — the names a taken arm's pattern binds
    /// at payload positions whose value moved out of the scrutinee and carries
    /// a user `impl Drop`. Per-binding sibling of
    /// [`Self::pattern_consumes_user_drop_payload`], with the SAME gates per
    /// position, so a binding is collected exactly when that function would
    /// have disarmed the scrutinee's walk for it — the two must agree or the
    /// payload body fires twice (walk + arm binding) or not at all. DIRECT
    /// payload positions only; a bare `Tuple` pattern is deliberately not a
    /// collector (a tuple scrutinee's element walk stays armed and fires, the
    /// shape-B behavior pinned by `b3011_nonlet1`).
    /// Shared consuming-scrutinee shape gate for the match / if-let /
    /// while-let arm-drop stashes: an identifier place (whose payload walk
    /// the disarm retracted), a `self` place under an OWNED receiver, or an
    /// OWNING fresh temp (a call, or a method call that isn't a borrow
    /// accessor). `get`/`first`/`last` yield an Option whose payload ALIASES
    /// the container's element — the container's own walk runs the body, so
    /// a stash fire would double it; the exclusion set mirrors codegen's
    /// `scrutinee_is_borrow_call`. Projections and unknown shapes stay
    /// non-consuming.
    ///
    /// B-2026-08-01-6: `self` under a `ref self` / `mut ref self` method is
    /// a BORROWED view — its match arms bind aliases the caller's receiver
    /// still owns, and `karac build` fires nothing for them, so the stash
    /// must stay silent (this fired `drop 5 e5` under `karac run` only).
    /// The active receiver mode comes from `self_param_stack`, pushed
    /// around each impl-method body by `try_eval_impl_method`; an empty
    /// stack means no method context, where `self` cannot occur.
    /// B-2026-08-01-13: an Identifier scrutinee naming an OWNED PARAM of
    /// the current function is a view of the callee's entry copy — its
    /// payload's Drop observability belongs to the CALLER (caller-retains:
    /// the caller's NLL / fresh-arg fire reads the original), so the arm
    /// stash must stay silent, mirroring codegen's
    /// `pattern_binding_scrutinee_is_owned_param` memory-only gate. Firing
    /// here doubled the payload body against the caller's fire (identifier
    /// args) and orphaned the ownership story for fresh ctor args.
    pub(super) fn scrutinee_expr_is_consuming(&self, e: &Expr) -> bool {
        // B-2026-08-29-10 — the caller-retains carve-out below is a HAND-OFF,
        // and a METHOD frame's arguments reach no caller-side fire to hand to
        // (the same asymmetry `owned_param_frame_is_method` records for the
        // `let`-destructure gates). So in a method frame an owned-param
        // scrutinee IS consuming and the arm stash must fire: without this a
        // method whose owned enum / `Option` param has its payload bound out
        // and not returned ran ZERO bodies here, against one on every compiled
        // backend for the value-enum spelling and one for both free-function
        // oracles.
        let method_frame_owns = self.owned_param_frame_is_method.last().copied() == Some(true);
        match &e.kind {
            ExprKind::Identifier(n) => {
                method_frame_owns
                    || !self
                        .owned_param_names_stack
                        .last()
                        .is_some_and(|params| params.contains(n.as_str()))
            }
            ExprKind::Call { .. } => true,
            ExprKind::SelfValue => matches!(
                self.self_param_stack.last(),
                Some(crate::ast::SelfParam::Owned)
            ),
            ExprKind::MethodCall { method, .. } => {
                !matches!(method.as_str(), "get" | "first" | "last")
            }
            // B-2026-08-03-6 — a TUPLE-ELEMENT place (`match t.0 { Ok(r) => .. }`),
            // the one projection that IS consuming. Its element walk is
            // retracted by `disarm_moved_out_enum_payload`'s TupleIndex arm, so
            // without the arm stash the body would be lost entirely rather than
            // merely mistimed. Restricted to an identifier root (a deeper
            // projection has no such disarm, so it stays non-consuming and the
            // owner's walk keeps firing) and to a non-owned-param root, matching
            // the Identifier arm's caller-retains carve-out.
            ExprKind::TupleIndex { object, .. } => match &object.kind {
                ExprKind::Identifier(n) => !self
                    .owned_param_names_stack
                    .last()
                    .is_some_and(|params| params.contains(n.as_str())),
                _ => false,
            },
            _ => false,
        }
    }

    /// Does this `match` retract the scrutinee's payload walk — i.e. does ANY
    /// arm move a Drop-bearing payload out?
    ///
    /// THE ONE DECISION BOTH HALVES ASK (B-2026-08-28-67). The disarm and the
    /// arm stash must agree exactly: retract without stashing and the body is
    /// owned by nobody, stash without retracting and it fires twice. Expressing
    /// that as two parallel conditions is how they drift, and it did — the
    /// disarm scans EVERY arm (it is a whole-match retraction, matching
    /// codegen's compile-time one, which cannot be path-sensitive) while a first
    /// cut asked the stash only about the TAKEN arm. A match mixing the two
    /// kinds of arm then retracted the walk for the materializing arm and stood
    /// the stash down for the read-through one:
    ///
    /// ```text
    /// match e { E.A(r) if r.id == 1i64 => { let m = r; .. }
    ///           E.A(r) => { println(f"v{r.id}") }  E.B => {} }
    /// ```
    ///
    /// took the second arm and printed `v5 dE` — `dR5` gone — against the
    /// compiled `v5 dR5 dE`. Routing both through this function makes the
    /// lockstep structural instead of a thing to remember.
    fn match_disarms_payload_walk(&self, enum_name: &str, arms: &[MatchArm]) -> bool {
        arms.iter().any(|arm| {
            self.pattern_consumes_user_drop_payload(enum_name, &arm.pattern)
                && !self.arm_only_reads_payload_through(
                    enum_name,
                    &arm.pattern,
                    &arm.body,
                    arm.guard.as_ref(),
                )
        })
    }

    /// Can [`Self::disarm_moved_out_enum_payload`] retract THIS place's payload
    /// walk? Only an identifier or `self` place has a walk to hand back
    /// (`moved_out_enum_payload_bindings` is keyed by name), so only there can
    /// B-2026-08-28-67's read-through gate stand the arm stash down.
    ///
    /// A FRESH-TEMP scrutinee (`match mk() { E.A(r) => .. }`) is the shape this
    /// exists for. It has a stash but no disarm — nothing named to retract — so
    /// standing the stash down there hands the payload to NOBODY: measured
    /// `v7 dE`, with the payload's `dR7` gone entirely, against `v7 dR7 dE`.
    /// That is the same "the two decisions live in different places" failure the
    /// row records for the first attempt at this fix, reached from the other
    /// side.
    fn place_walk_is_retractable(place: &Expr) -> bool {
        matches!(place.kind, ExprKind::Identifier(_) | ExprKind::SelfValue)
    }

    /// B-2026-08-28-67 — does this arm merely READ THROUGH every Drop-bearing
    /// payload it binds out of `enum_name`, never materializing one as a value?
    ///
    /// When it does, the payload was not moved out at all: the scrutinee still
    /// owns it and its own `Drop` walk runs the body, AFTER the enum's own body
    /// — which is design.md § Part 8's order ("drop each field in order ... after
    /// the user's drop body returns") and what all three compiled backends
    /// already produce. The interpreter's arm stash otherwise fires the payload
    /// body at the ARM's end, i.e. BEFORE the scrutinee's own, so the two
    /// backends printed the same two lines in opposite orders.
    ///
    /// Gated by the CALLERS on the enum having its own `impl Drop`, which is
    /// what makes the move-out illegitimate rather than merely mistimed:
    /// "Partial moves out of a struct field are rejected if the struct has a
    /// `Drop` impl: the drop body assumes all fields are present"
    /// (design.md § Part 8, Interaction with move semantics). Without an own
    /// body there is no destructor to strand and nothing to order against —
    /// measured identical on every backend either way.
    ///
    /// Read-through is decided structurally by `binding_use`, NOT by
    /// `consume_class::binding_only_borrowed`: that predicate models a
    /// free-function argument as entry-copied and therefore non-consuming, but
    /// `keep(r)` materializes `r` and every backend agrees the ARM owns it
    /// there. The two predicates part company on exactly that shape.
    fn arm_only_reads_payload_through(
        &self,
        enum_name: &str,
        pattern: &Pattern,
        body: &Expr,
        guard: Option<&Expr>,
    ) -> bool {
        self.payload_bindings_all(enum_name, pattern, |n| {
            crate::binding_use::binding_only_read_through(n, body)
                && guard.is_none_or(|g| crate::binding_use::binding_only_read_through(n, g))
        })
    }

    /// `if let` / `while let` sibling of [`Self::arm_only_reads_payload_through`],
    /// whose scope is a `Block` rather than an arm expression. Deliberately NOT
    /// offered to `let … else`: that pattern binds into the ENCLOSING block, so
    /// the payload outlives any scope this could inspect and is materialized by
    /// definition.
    pub(super) fn let_form_only_reads_payload_through(
        &self,
        enum_name: &str,
        pattern: &Pattern,
        block: &Block,
    ) -> bool {
        self.payload_bindings_all(enum_name, pattern, |n| {
            crate::binding_use::binding_only_read_through_block(n, block)
        })
    }

    /// Shared core of the two above: the enum owns a `Drop` body, the pattern
    /// really does bind a Drop-bearing payload out, and `f` holds for every
    /// such binding.
    fn payload_bindings_all(
        &self,
        enum_name: &str,
        pattern: &Pattern,
        f: impl Fn(&str) -> bool,
    ) -> bool {
        if !self.program.drop_method_keys.contains_key(enum_name) {
            return false;
        }
        let names = self.arm_moved_user_drop_payload_bindings(enum_name, pattern);
        // Nothing bound out: leave the caller's own gate to decide, unchanged.
        !names.is_empty() && names.iter().all(|n| f(n))
    }

    pub(super) fn arm_moved_user_drop_payload_bindings(
        &self,
        enum_name: &str,
        pattern: &Pattern,
    ) -> Vec<String> {
        let variant = match &pattern.kind {
            PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
                match path.last() {
                    Some(v) => v.clone(),
                    None => return Vec::new(),
                }
            }
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        // Option/Result: shape-only, like the disarm — no EnumDef to consult.
        // The caller's runtime-value filter (a bound `Value::Struct` whose type
        // is in `drop_method_keys`) is what keeps a scalar payload silent.
        if enum_name == "Option" || enum_name == "Result" {
            if matches!(variant.as_str(), "Some" | "Ok" | "Err") {
                if let PatternKind::TupleVariant { patterns, .. } = &pattern.kind {
                    for sub in patterns {
                        if let PatternKind::Binding(n) = &sub.kind {
                            out.push(n.clone());
                        }
                    }
                }
            }
            return out;
        }
        let Some(decls) = self.variant_payload_decls(enum_name, &variant) else {
            return Vec::new();
        };
        match &pattern.kind {
            PatternKind::TupleVariant { patterns, .. } => {
                for (i, sub) in patterns.iter().enumerate() {
                    if let PatternKind::Binding(n) = &sub.kind {
                        if decls
                            .get(i)
                            .map(|(_, te)| self.type_expr_runs_user_drop(te))
                            .unwrap_or(false)
                        {
                            out.push(n.clone());
                        }
                    }
                }
            }
            PatternKind::Struct { fields, .. } => {
                for fp in fields {
                    let bind_name = match &fp.pattern {
                        None => Some(fp.name.clone()),
                        Some(sub) => match &sub.kind {
                            PatternKind::Binding(n) => Some(n.clone()),
                            _ => None,
                        },
                    };
                    if let Some(n) = bind_name {
                        let runs = decls
                            .iter()
                            .find(|(dn, _)| dn.as_deref() == Some(fp.name.as_str()))
                            .map(|(_, te)| self.type_expr_runs_user_drop(te))
                            .unwrap_or(false);
                        if runs {
                            out.push(n);
                        }
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Does `pattern` bind out a payload position of `enum_name` whose declared
    /// type runs a user `impl Drop`? The interpreter's twin of codegen's
    /// `enum_pattern_consumes_user_drop_payload`, down to consulting the
    /// DECLARED payload type rather than the runtime value — an erased generic
    /// payload is invisible to codegen at emit time, so both backends skip it.
    fn pattern_consumes_user_drop_payload(&self, enum_name: &str, pattern: &Pattern) -> bool {
        let variant = match &pattern.kind {
            PatternKind::TupleVariant { path, .. } | PatternKind::Struct { path, .. } => {
                match path.last() {
                    Some(v) => v.clone(),
                    None => return false,
                }
            }
            _ => return false,
        };
        // B-2026-07-30-11 (Option/Result leg): no `EnumDef` to consult and
        // the declared payload is a bare generic param, so the gate here is
        // SHAPE-only — a `Some`/`Ok`/`Err` pattern whose payload position
        // claims ownership. Over-approximate on purpose: the move-out set
        // this feeds is a no-op for a binding whose walk never registered
        // (no te record), and codegen's twin retraction is equally a no-op
        // when no `__karac_dropelems_*` action exists — so both backends
        // agree on every case, disarmed or not.
        if enum_name == "Option" || enum_name == "Result" {
            return match &pattern.kind {
                PatternKind::TupleVariant { patterns, .. } => {
                    matches!(variant.as_str(), "Some" | "Ok" | "Err")
                        && patterns.iter().any(Self::pattern_claims_ownership)
                }
                _ => false,
            };
        }
        let Some(decls) = self.variant_payload_decls(enum_name, &variant) else {
            return false;
        };
        let consumed: Vec<usize> = match &pattern.kind {
            PatternKind::TupleVariant { patterns, .. } => patterns
                .iter()
                .enumerate()
                .filter(|(_, sub)| Self::pattern_claims_ownership(sub))
                .map(|(i, _)| i)
                .collect(),
            PatternKind::Struct { fields, .. } => fields
                .iter()
                .filter(|fp| {
                    fp.pattern
                        .as_ref()
                        .is_none_or(Self::pattern_claims_ownership)
                })
                .filter_map(|fp| {
                    decls
                        .iter()
                        .position(|(n, _)| n.as_deref() == Some(fp.name.as_str()))
                })
                .collect(),
            _ => return false,
        };
        consumed.into_iter().any(|pos| {
            decls
                .get(pos)
                .map(|(_, te)| self.type_expr_runs_user_drop(te))
                .unwrap_or(false)
        })
    }

    /// Does a payload sub-pattern claim ownership of the position it covers? A
    /// `Wildcard` / literal / range / slice does not; anything that binds
    /// (directly or through a nested destructure) does, except a `ref name @ …`
    /// whose whole subtree borrows (design.md § @ Bindings). Arm-for-arm the
    /// same rule as codegen's `pattern_consumes_field` — duplicated rather than
    /// shared because that one lives behind `--features llvm`.
    fn pattern_claims_ownership(sub: &Pattern) -> bool {
        match &sub.kind {
            PatternKind::Wildcard
            | PatternKind::Literal(_)
            | PatternKind::RangePattern { .. }
            | PatternKind::Slice { .. } => false,
            PatternKind::Binding(_) => true,
            PatternKind::AtBinding { by_ref: true, .. } => false,
            PatternKind::AtBinding { pattern, .. } => Self::pattern_claims_ownership(pattern),
            PatternKind::Tuple(pats) => pats.iter().any(Self::pattern_claims_ownership),
            PatternKind::TupleVariant { patterns, .. } => {
                patterns.iter().any(Self::pattern_claims_ownership)
            }
            PatternKind::Struct { fields, .. } => fields.iter().any(|f| {
                f.pattern
                    .as_ref()
                    .map(Self::pattern_claims_ownership)
                    .unwrap_or(true)
            }),
            PatternKind::Or(pats) => pats.iter().any(Self::pattern_claims_ownership),
        }
    }

    /// Does the head type of `te` name a struct that runs a user `impl Drop`
    /// body, directly or through a field?
    pub(crate) fn type_expr_runs_user_drop(&self, te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &te.kind else {
            return false;
        };
        let Some(head) = p.segments.first() else {
            return false;
        };
        self.type_name_runs_user_drop(head, &mut Vec::new())
    }

    pub(crate) fn type_name_runs_user_drop(&self, name: &str, seen: &mut Vec<String>) -> bool {
        if self.program.drop_method_keys.contains_key(name) {
            return true;
        }
        if seen.iter().any(|s| s == name) {
            return false;
        }
        seen.push(name.to_string());
        if self
            .program
            .items
            .iter()
            .find_map(|item| match item {
                Item::StructDef(s) if s.name == name => Some(s),
                _ => None,
            })
            .is_some_and(|s| {
                let fields: Vec<TypeExpr> = s.fields.iter().map(|f| f.ty.clone()).collect();
                fields
                    .iter()
                    .any(|fty| self.field_te_runs_user_drop(fty, seen))
            })
        {
            return true;
        }
        // B-2026-08-28-58 — the ENUM leg, the interpreter twin of the one
        // B-2026-08-28-54 added to codegen's `type_runs_user_drop`. Without
        // it an enum with NO `Drop` of its own but a Drop-bearing variant
        // payload (`enum H { A(R), B }`) classified drop-free, so the
        // `Option[H]` / `Result[H, _]` payload-bodies registration never
        // armed and `dR` never ran. Both backends' type-level gates have to
        // classify identically or the two print different things.
        if name != "Option" && name != "Result" {
            return self
                .program
                .items
                .iter()
                .find_map(|item| match item {
                    Item::EnumDef(e) if e.name == name => Some(e),
                    _ => None,
                })
                .is_some_and(|e| {
                    let tys: Vec<TypeExpr> = e
                        .variants
                        .iter()
                        .flat_map(|v| match &v.kind {
                            crate::ast::VariantKind::Unit => Vec::new(),
                            crate::ast::VariantKind::Tuple(tys) => tys.clone(),
                            crate::ast::VariantKind::Struct(fs) => {
                                fs.iter().map(|f| f.ty.clone()).collect()
                            }
                        })
                        .collect();
                    tys.iter().any(|ty| self.field_te_runs_user_drop(ty, seen))
                });
        }
        false
    }

    /// B-2026-08-02-24 — does a struct FIELD's declared type reach user-Drop
    /// work? The head-name recursion alone read a `Vec[Res]` field as the
    /// head `"Vec"` (no struct def, no Drop) and declined, so a struct
    /// carrying its Drop only through a container field classified as
    /// drop-free — which is what left the interpreter's Map-value bodies
    /// registration unarmed for `Map[i64, Holder]` with `Holder { xs:
    /// Vec[Res] }`. This is the interp twin of codegen's container widening
    /// in `type_runs_user_drop` (B-2026-08-01-22 leg b, extended by
    /// B-2026-08-02-18): ONE container level, covering Vec/VecDeque
    /// elements, Map/SortedMap values, Set/SortedSet elements, and tuple
    /// elements, so both backends' type-level gates classify identically.
    pub(crate) fn field_te_runs_user_drop(&self, fty: &TypeExpr, seen: &mut Vec<String>) -> bool {
        match &fty.kind {
            TypeKind::Path(p) => {
                let Some(head) = p.segments.first() else {
                    return false;
                };
                if self.type_name_runs_user_drop(head, seen) {
                    return true;
                }
                // B-2026-08-03-1 — Option/Result are a container level too:
                // BOTH of Result's arms can be live and either can carry a
                // Drop, so unlike the single-slot collections this checks a
                // RANGE of generic args rather than one index. Codegen twin:
                // `optres_payload_heads` feeding `elem_te_runs_user_drop`.
                let idxs: &[usize] = match head.as_str() {
                    "Vec" | "VecDeque" | "Set" | "SortedSet" => &[0],
                    "Map" | "SortedMap" => &[1],
                    "Option" => &[0],
                    "Result" => &[0, 1],
                    _ => return false,
                };
                let Some(args) = p.generic_args.as_ref() else {
                    return false;
                };
                idxs.iter().any(|&i| match args.get(i) {
                    Some(crate::ast::GenericArg::Type(inner)) => match &inner.kind {
                        TypeKind::Path(ip) => ip
                            .segments
                            .first()
                            .is_some_and(|h| self.type_name_runs_user_drop(h, seen)),
                        _ => false,
                    },
                    _ => false,
                })
            }
            // B-2026-08-03-7 — one further container level INSIDE each element,
            // by recursing through this same predicate rather than reading only
            // the element's head name: `(Option[Res], i64)` classified as
            // body-free, so a struct field or Map value holding such a tuple
            // registered no walk. Codegen twin: `tuple_field_elem_heads`, which
            // gained the same extractors one level down. Nested tuples fall out
            // of the recursion.
            TypeKind::Tuple(elems) => {
                let elems = elems.clone();
                elems.iter().any(|e| self.field_te_runs_user_drop(e, seen))
            }
            _ => false,
        }
    }

    /// B-2026-07-23-12: is `place` a bare-identifier / `self` scrutinee whose
    /// storage a match write-through can update in place? Restricted to these
    /// two forms (not field / index projections) so the write-back never
    /// re-evaluates a projection base with side effects — the `mut ref` param
    /// and receiver cases the divergence reports are both bare identifiers.
    fn match_place_is_writable(place: &Expr) -> bool {
        matches!(&place.kind, ExprKind::Identifier(_) | ExprKind::SelfValue)
    }

    /// B-2026-07-23-12: rebuild `original` (a match scrutinee value) with each
    /// DIRECTLY value-bound payload position replaced by its current binding
    /// value read from the arm scope, so an in-arm mutation writes through to
    /// the scrutinee place. Returns `Some(patched)` only when at least one
    /// direct value binding was patched (an enum-variant tuple/struct payload
    /// or a plain-struct field); `None` for patterns with no direct value
    /// binding (wildcard, literal, nested destructure) so the scrutinee is left
    /// untouched. Only lowercase (snake_case) binding names are patched — the
    /// case-class invariant makes those unambiguous value bindings, never
    /// unit-variant tests, so a `Table(Left)` variant sub-pattern is never
    /// mistaken for a binding.
    fn patch_arm_bindings(&self, pattern: &Pattern, original: &Value) -> Option<Value> {
        match (&pattern.kind, original) {
            (
                PatternKind::TupleVariant { patterns, .. },
                Value::EnumVariant {
                    enum_name,
                    variant,
                    data: EnumData::Tuple(vals),
                },
            ) => {
                let mut new_vals = vals.clone();
                let mut any = false;
                for (i, sub) in patterns.iter().enumerate() {
                    if let PatternKind::Binding(name) = &sub.kind {
                        if !Self::is_patch_binding_name(name) {
                            continue;
                        }
                        if let (Some(cur), Some(slot)) = (self.env.get(name), new_vals.get_mut(i)) {
                            *slot = cur;
                            any = true;
                        }
                    }
                }
                any.then(|| Value::EnumVariant {
                    enum_name: enum_name.clone(),
                    variant: variant.clone(),
                    data: EnumData::Tuple(new_vals),
                })
            }
            (
                PatternKind::Struct { fields, .. },
                Value::EnumVariant {
                    enum_name,
                    variant,
                    data: EnumData::Struct(map),
                },
            ) => {
                let (m, any) = self.patch_struct_fields(fields, map);
                any.then(|| Value::EnumVariant {
                    enum_name: enum_name.clone(),
                    variant: variant.clone(),
                    data: EnumData::Struct(m),
                })
            }
            (PatternKind::Struct { fields, .. }, Value::Struct { name, fields: map }) => {
                let (m, any) = self.patch_struct_fields(fields, map);
                any.then(|| Value::Struct {
                    name: name.clone(),
                    fields: m,
                })
            }
            _ => None,
        }
    }

    /// Patch helper for a struct / struct-variant payload: clone `map` and
    /// overwrite each field whose sub-pattern is a direct value binding
    /// (shorthand `{ f }` or `{ f: bind }`) with that binding's current value.
    /// Returns the (possibly updated) map and whether any field was patched.
    fn patch_struct_fields(
        &self,
        fields: &[FieldPattern],
        map: &std::collections::HashMap<String, Value>,
    ) -> (std::collections::HashMap<String, Value>, bool) {
        let mut m = map.clone();
        let mut any = false;
        for fp in fields {
            let bind_name: Option<&str> = match &fp.pattern {
                None => Some(fp.name.as_str()),
                Some(sub) => match &sub.kind {
                    PatternKind::Binding(n) => Some(n.as_str()),
                    _ => None,
                },
            };
            if let Some(bn) = bind_name {
                if !Self::is_patch_binding_name(bn) {
                    continue;
                }
                if let Some(cur) = self.env.get(bn) {
                    m.insert(fp.name.clone(), cur);
                    any = true;
                }
            }
        }
        (m, any)
    }

    /// A binding name eligible for match write-through patching: a bare
    /// snake_case (lowercase-initial) identifier. The case-class invariant
    /// makes these unambiguous value bindings; a PascalCase or dotted name is a
    /// (possibly unit-variant) type reference and is skipped, so a rare
    /// uppercase binding just retains the pre-fix (non-write-through) behavior
    /// rather than risk mis-patching a variant test.
    fn is_patch_binding_name(name: &str) -> bool {
        !name.contains('.') && name.chars().next().is_some_and(|c| c.is_lowercase())
    }

    // ── Pattern matching ────────────────────────────────────────

    pub(crate) fn try_match_pattern(&self, pattern: &Pattern, value: &Value) -> bool {
        match &pattern.kind {
            PatternKind::Wildcard => true,
            PatternKind::Binding(name) => {
                // A `Binding` node doubles as a unit-variant pattern; see
                // [`Self::binding_is_unit_variant`] for which spellings count
                // and why. When it IS one, compare the last path segment to
                // the scrutinee's tag; otherwise it is a true binding and
                // matches anything.
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                if self.binding_is_unit_variant(name, value) {
                    if let Value::EnumVariant { variant: v2, .. } = value {
                        return variant_name == v2.as_str();
                    }
                    return false;
                }
                true // actual binding — matches anything
            }
            PatternKind::Literal(lit) => {
                let lit_val = self.literal_to_value(lit);
                lit_val == *value
            }
            PatternKind::TupleVariant { path, patterns } => {
                let variant_name = path.last().cloned().unwrap_or_default();
                match value {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } => {
                        variant == &variant_name
                            && patterns.len() == vals.len()
                            && patterns
                                .iter()
                                .zip(vals)
                                .all(|(p, v)| self.try_match_pattern(p, v))
                    }
                    _ => false,
                }
            }
            PatternKind::Struct {
                path,
                fields,
                has_rest: _, // The runtime matcher checks each named field's
                             // sub-pattern. Unlisted fields are unconstrained
                             // whether `..` is present or not — the matcher
                             // never required all fields to be enumerated —
                             // so `has_rest` is a typechecker concern only.
            } => {
                let name = path.last().cloned().unwrap_or_default();
                match value {
                    Value::Struct {
                        name: sn,
                        fields: sfields,
                    } if *sn == name => fields.iter().all(|fp| {
                        if let Some(val) = sfields.get(&fp.name) {
                            if let Some(ref sub) = fp.pattern {
                                self.try_match_pattern(sub, val)
                            } else {
                                true
                            }
                        } else {
                            false
                        }
                    }),
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Struct(sfields),
                        ..
                    } if *variant == name => fields.iter().all(|fp| {
                        if let Some(val) = sfields.get(&fp.name) {
                            if let Some(ref sub) = fp.pattern {
                                self.try_match_pattern(sub, val)
                            } else {
                                true
                            }
                        } else {
                            false
                        }
                    }),
                    _ => false,
                }
            }
            PatternKind::Tuple(patterns) => match value {
                Value::Tuple(vals) => {
                    patterns.len() == vals.len()
                        && patterns
                            .iter()
                            .zip(vals)
                            .all(|(p, v)| self.try_match_pattern(p, v))
                }
                _ => false,
            },
            PatternKind::Or(alternatives) => alternatives
                .iter()
                .any(|p| self.try_match_pattern(p, value)),
            PatternKind::RangePattern {
                start,
                end,
                inclusive,
            } => self.value_in_range_pattern(value, start.as_ref(), end.as_ref(), *inclusive),
            PatternKind::AtBinding { pattern, .. } => self.try_match_pattern(pattern, value),
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let Some((storage, offset, total_len, _)) = slice_pattern_view(value) else {
                    return false;
                };
                let min_len = prefix.len() + suffix.len();
                if rest.is_none() {
                    if total_len != min_len {
                        return false;
                    }
                } else if total_len < min_len {
                    return false;
                }
                let storage_read = storage.read().unwrap();
                for (i, sub) in prefix.iter().enumerate() {
                    if !self.try_match_pattern(sub, &storage_read[offset + i]) {
                        return false;
                    }
                }
                for (i, sub) in suffix.iter().enumerate() {
                    let idx = offset + total_len - suffix.len() + i;
                    if !self.try_match_pattern(sub, &storage_read[idx]) {
                        return false;
                    }
                }
                true
            }
        }
    }

    /// Whether a `PatternKind::Binding` node is really a UNIT-VARIANT pattern
    /// rather than a fresh value binding — the one predicate both
    /// [`Self::try_match_pattern`] and [`Self::bind_pattern`] key on. They held
    /// byte-identical copies of it, and B-2026-08-22-2 was a hole in both.
    ///
    /// Three ways a `Binding` is a variant:
    ///
    /// 1. **Dotted** (`Side.Left`). A real value binding can never contain a
    ///    `.`, so this is unambiguous.
    /// 2. **Bare and in scope** (`Left`, `Less`, `Nfc`). Registered by
    ///    `Interpreter::new` for user enums and the three prelude enums.
    /// 3. **Bare and declared by the SCRUTINEE's enum** (`NotFound` against an
    ///    `IoError`). This is B-2026-08-22-2's fix. Baked-stdlib enum variants
    ///    are registered ONLY under their qualified path, so case 2 misses
    ///    every one of them and the name fell through to "true binding —
    ///    matches anything": the FIRST arm always matched, and
    ///    `match e { NotFound => …, PermissionDenied => … }` on an
    ///    `IoError.PermissionDenied` printed `NotFound` under `--interp` while
    ///    both compiled backends printed the right answer.
    ///
    /// Case 3 asks the SCRUTINEE rather than a global table on purpose. The
    /// alternative — registering every stdlib variant unqualified — would put
    /// ~20 enums' variants in one namespace, where `NotFound` belongs to both
    /// `IoError` and `TlsError` and one of them would have to win. Keyed on the
    /// scrutinee's own enum there is nothing to disambiguate.
    ///
    /// PascalCase is required for the bare cases and is load-bearing: Kāra's
    /// case-class invariant (design.md) makes variant identifiers PascalCase
    /// and value bindings snake_case, so a lowercase name is ALWAYS a fresh
    /// binding. Without that gate an ordinary local holding a unit-variant
    /// value (`let c = Color.Green` shadowing the `c` in
    /// `match m { Info(c) => … }`) turned the constructor's sub-binding into a
    /// variant test and surfaced as a spurious "non-exhaustive match".
    pub(crate) fn binding_is_unit_variant(&self, name: &str, scrutinee: &Value) -> bool {
        if name.contains('.') {
            return true;
        }
        let variant_name = name.rsplit('.').next().unwrap_or(name);
        if !variant_name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_uppercase())
        {
            return false;
        }
        if matches!(
            self.env.get(variant_name),
            Some(Value::EnumVariant {
                data: EnumData::Unit,
                ..
            })
        ) {
            return true;
        }
        match scrutinee {
            Value::EnumVariant { enum_name, .. } => self
                .variant_payload_decls(enum_name, variant_name)
                .is_some_and(|decls| decls.is_empty()),
            _ => false,
        }
    }

    pub(crate) fn bind_pattern(&mut self, pattern: &Pattern, value: Value) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Binding(name) => {
                // Don't create a binding for a unit-variant pattern — the
                // same [`Self::binding_is_unit_variant`] predicate
                // `try_match_pattern` uses, so the two can never drift.
                if self.binding_is_unit_variant(name, &value) {
                    return;
                }
                self.env.define(name.clone(), value);
            }
            PatternKind::Literal(_) => {}
            PatternKind::TupleVariant { patterns, .. } => {
                if let Value::EnumVariant {
                    data: EnumData::Tuple(vals),
                    ..
                } = value
                {
                    for (p, v) in patterns.iter().zip(vals) {
                        self.bind_pattern(p, v);
                    }
                }
            }
            PatternKind::Struct { fields, .. } => {
                let field_vals = match value {
                    Value::Struct { fields: f, .. } => f,
                    Value::EnumVariant {
                        data: EnumData::Struct(f),
                        ..
                    } => f,
                    _ => return,
                };
                for fp in fields {
                    if let Some(val) = field_vals.get(&fp.name) {
                        if let Some(ref sub) = fp.pattern {
                            self.bind_pattern(sub, val.clone());
                        } else {
                            self.env.define(fp.name.clone(), val.clone());
                        }
                    }
                }
            }
            PatternKind::Tuple(patterns) => {
                if let Value::Tuple(vals) = value {
                    for (p, v) in patterns.iter().zip(vals) {
                        self.bind_pattern(p, v);
                    }
                }
            }
            PatternKind::Or(alternatives) => {
                // Bind from first matching alternative
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, value);
                }
            }
            PatternKind::AtBinding { name, pattern, .. } => {
                self.env.define(name.clone(), value.clone());
                self.bind_pattern(pattern, value);
            }
            PatternKind::RangePattern { .. } => {}
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let Some((storage, offset, total_len, source_mutable)) = slice_pattern_view(&value)
                else {
                    return;
                };
                let prefix_vals: Vec<Value>;
                let suffix_vals: Vec<Value>;
                {
                    let storage_read = storage.read().unwrap();
                    prefix_vals = (0..prefix.len())
                        .map(|i| storage_read[offset + i].clone())
                        .collect();
                    suffix_vals = (0..suffix.len())
                        .map(|i| storage_read[offset + total_len - suffix.len() + i].clone())
                        .collect();
                }
                for (sub, val) in prefix.iter().zip(prefix_vals) {
                    self.bind_pattern(sub, val);
                }
                for (sub, val) in suffix.iter().zip(suffix_vals) {
                    self.bind_pattern(sub, val);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    let rest_start = offset + prefix.len();
                    let rest_len = total_len - prefix.len() - suffix.len();
                    let rest_value = Value::Slice {
                        storage,
                        start: rest_start,
                        len: rest_len,
                        mutable: source_mutable,
                    };
                    self.env.define(name.clone(), rest_value);
                }
            }
        }
    }
    /// Match `value` against a range pattern with optional `start` / `end`
    /// bounds. Bounds are integer or char literals (the parser limits
    /// `LiteralPattern` in range position to those two forms). Half-open
    /// forms — `lo..` (`end = None`), `..hi` (`start = None`) — accept
    /// everything past the present bound. Bounded-exclusive (`lo..hi`),
    /// bounded-inclusive (`lo..=hi`), and the half-open inclusive form
    /// (`..=hi`) all share the same comparison.
    fn value_in_range_pattern(
        &self,
        value: &Value,
        start: Option<&RangeBound>,
        end: Option<&RangeBound>,
        inclusive: bool,
    ) -> bool {
        // Project the scrutinee value into a sortable scalar key (i128 to
        // accommodate i64 + char in the same comparison space).
        let key: i128 = match value {
            Value::Int(n) => *n,
            Value::Char(c) => (*c as u32) as i128,
            _ => return false,
        };
        // Resolve a bound to its scalar key. A `Path` bound names a
        // module-level int/char const, bound in `env` at program start;
        // the typechecker already rejected non-const / non-scalar paths,
        // so a `None` here only arises in an already-erroring program.
        let bound_key = |b: &RangeBound| -> Option<i128> {
            match b {
                RangeBound::Literal(LiteralPattern::Integer(n, _)) => Some(*n),
                RangeBound::Literal(LiteralPattern::Char(c)) => Some((*c as u32) as i128),
                RangeBound::Literal(_) => None,
                RangeBound::Path { segments, .. } if segments.len() == 1 => {
                    match self.env.get(&segments[0]) {
                        Some(Value::Int(n)) => Some(n),
                        Some(Value::Char(c)) => Some((c as u32) as i128),
                        _ => None,
                    }
                }
                RangeBound::Path { .. } => None,
            }
        };
        if let Some(lo) = start {
            let Some(lo_key) = bound_key(lo) else {
                return false;
            };
            if key < lo_key {
                return false;
            }
        }
        if let Some(hi) = end {
            let Some(hi_key) = bound_key(hi) else {
                return false;
            };
            if inclusive {
                if key > hi_key {
                    return false;
                }
            } else if key >= hi_key {
                return false;
            }
        }
        true
    }

    fn literal_to_value(&self, lit: &LiteralPattern) -> Value {
        match lit {
            LiteralPattern::Integer(i, _) => Value::Int(*i),
            LiteralPattern::Float(f, _) => Value::Float(*f),
            LiteralPattern::String(s) => Value::String(s.clone()),
            LiteralPattern::Char(c) => Value::Char(*c),
            LiteralPattern::Bool(b) => Value::Bool(*b),
        }
    }
}
